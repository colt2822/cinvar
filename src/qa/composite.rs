//! Composite visibility QA: proves an upper-track clip neither hides the footage beneath it
//! (the "black background" failure) nor vanishes (the "invisible overlay" failure).
//! Everything is measured from real frames: the render, the base asset and the overlay asset
//! are each decoded at the same instants and compared at low resolution. No model is involved.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::project::model::{Clip, Fit, Project, Timeline, TrackKind};
use crate::project::store::ProjectStore;

const W: usize = 48;
const BLACK_LUMA: u8 = 24;
/// Output may be this much blacker than the base footage before we call it occlusion.
const OCCLUSION_LIMIT: f64 = 0.35;
/// Fraction of the overlay's opaque pixels that must visibly change the frame.
const VISIBLE_RATIO: f64 = 0.25;
const MIN_COVERAGE: f64 = 0.01;
const DIFF_LUMA: i32 = 16;

pub struct Verdict {
    pub passed: bool,
    pub detail: String,
}

fn height_for(cw: u32, ch: u32) -> usize {
    let h = (W as f64 * ch as f64 / cw.max(1) as f64).round() as usize;
    h.max(2)
}

fn frame(bin: &Path, input: &Path, at: f64, vf: &str, h: usize) -> Option<Vec<u8>> {
    let out = Command::new(bin)
        .args(["-v", "error", "-nostdin", "-ss", &format!("{at:.4}"), "-i"])
        .arg(input)
        .args(["-frames:v", "1", "-vf", vf, "-pix_fmt", "gray", "-f", "rawvideo", "-"])
        .output()
        .ok()?;
    (out.status.success() && out.stdout.len() == W * h).then_some(out.stdout)
}

fn fit_filter(fit: Fit, h: usize, transparent_pad: bool) -> String {
    let pad = if transparent_pad { "black@0" } else { "black" };
    match fit {
        Fit::Cover => format!("scale={W}:{h}:force_original_aspect_ratio=increase,crop={W}:{h}"),
        Fit::Stretch => format!("scale={W}:{h}"),
        Fit::Contain => format!(
            "scale={W}:{h}:force_original_aspect_ratio=decrease,pad={W}:{h}:(ow-iw)/2:(oh-ih)/2:color={pad}"
        ),
    }
}

fn source_time(clip: &Clip, t: f64) -> f64 {
    let speed = if clip.speed <= 0.0 { 1.0 } else { clip.speed };
    clip.source_in.as_secs_f64() + (t - clip.timeline_start.as_secs_f64()) * speed
}

fn black_fraction(px: &[u8]) -> f64 {
    px.iter().filter(|p| **p < BLACK_LUMA).count() as f64 / px.len() as f64
}

fn asset_file(project: &Project, store: &ProjectStore, clip: &Clip) -> Option<PathBuf> {
    let asset = project.asset(clip.asset.as_ref()?)?;
    let path = store.asset_path(asset).ok()?;
    path.is_file().then_some(path)
}

/// `None` when there is nothing to check (no overlay clip sits over another video clip).
pub fn check(
    project: &Project,
    store: &ProjectStore,
    timeline: &Timeline,
    rendered: &Path,
    canvas: (u32, u32),
) -> Option<Verdict> {
    let bin = super::probe::ffmpeg().ok()?;
    let h = height_for(canvas.0, canvas.1);
    let video: Vec<_> = timeline
        .tracks
        .iter()
        .filter(|t| matches!(t.kind, TrackKind::Video | TrackKind::Overlay) && !t.hidden)
        .collect();
    let mut checked = 0usize;
    let mut problems: Vec<String> = Vec::new();

    for (ui, upper) in video.iter().enumerate().skip(1) {
        for oc in upper.clips.iter().filter(|c| c.text.is_none() && c.asset.is_some()) {
            if oc.transform.opacity < 0.2 {
                continue;
            }
            for lower in video.iter().take(ui) {
                for bc in lower.clips.iter().filter(|c| c.text.is_none() && c.asset.is_some()) {
                    let start = oc.timeline_start.max(bc.timeline_start).as_secs_f64();
                    let end = oc.timeline_end().min(bc.timeline_end()).as_secs_f64();
                    if end - start < 0.2 {
                        continue;
                    }
                    let (Some(ofile), Some(bfile)) =
                        (asset_file(project, store, oc), asset_file(project, store, bc))
                    else {
                        continue;
                    };
                    for k in [0.25, 0.5, 0.75] {
                        let t = start + (end - start) * k;
                        let base = frame(&bin, &bfile, source_time(bc, t), &fit_filter(bc.transform.fit, h, false), h);
                        let out = frame(&bin, rendered, t, &format!("scale={W}:{h}"), h);
                        let cov = frame(
                            &bin,
                            &ofile,
                            source_time(oc, t),
                            &format!("format=rgba,{},format=yuva444p,alphaextract", fit_filter(oc.transform.fit, h, true)),
                            h,
                        );
                        let (Some(base), Some(out), Some(cov)) = (base, out, cov) else {
                            continue;
                        };
                        checked += 1;
                        let at = format!("{:.2}s", t);
                        let occluded = black_fraction(&out) - black_fraction(&base);
                        if occluded > OCCLUSION_LIMIT {
                            problems.push(format!(
                                "at {at} '{}' blacks out {:.0}% of the frame over footage '{}' (overlay hides the base)",
                                oc.name,
                                occluded * 100.0,
                                bc.name
                            ));
                            continue;
                        }
                        let opaque: Vec<usize> =
                            (0..cov.len()).filter(|i| cov[*i] > 40).collect();
                        let coverage = opaque.len() as f64 / cov.len() as f64;
                        if coverage < MIN_COVERAGE {
                            continue;
                        }
                        let changed = opaque
                            .iter()
                            .filter(|i| (out[**i] as i32 - base[**i] as i32).abs() > DIFF_LUMA)
                            .count() as f64;
                        if changed / (opaque.len() as f64) < VISIBLE_RATIO {
                            problems.push(format!(
                                "at {at} '{}' covers {:.1}% of the frame but only {:.0}% of it differs from the footage (overlay invisible)",
                                oc.name,
                                coverage * 100.0,
                                changed / opaque.len() as f64 * 100.0
                            ));
                        }
                    }
                }
            }
        }
    }
    if checked == 0 {
        return None;
    }
    Some(if problems.is_empty() {
        Verdict {
            passed: true,
            detail: format!("measured {checked} sample(s): overlays are visible and base footage is not blacked out"),
        }
    } else {
        problems.truncate(4);
        Verdict {
            passed: false,
            detail: problems.join("; "),
        }
    })
}
