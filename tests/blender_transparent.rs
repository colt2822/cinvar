//! Real transparent-Blender evidence (NOT mocked). Ignored by default; run explicitly:
//!   CINVAR_BLENDER=/usr/bin/blender cargo test --release --test blender_transparent -- --ignored --nocapture
//! The tests fail (never skip) when Blender or ffmpeg is missing.

use std::path::{Path, PathBuf};
use std::process::Command;

use cinvar::blender::alpha::{self, OverlaySource, OverlaySpec};
use cinvar::blender::{self, AnimationSpec};
use cinvar::util::time::Time;

fn blender_path() -> PathBuf {
    blender::available().expect("Blender not found: set CINVAR_BLENDER")
}

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn transparent_spec() -> AnimationSpec {
    AnimationSpec {
        template: "logo_reveal".into(),
        text: "CINVAR".into(),
        duration: Time::from_secs_f64(0.75),
        width: 480,
        height: 270,
        fps: 24,
        color: "#4f8cff".into(),
        transparent: true,
    }
}

/// Raw pixel read of one frame as 8-bit grey, for comparing two files honestly.
fn grey_frame(input: &Path, at: f64) -> Vec<u8> {
    let out = input.with_extension("probe.gray");
    let status = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-ss", &format!("{at:.3}"), "-i"])
        .arg(input)
        .args(["-frames:v", "1", "-pix_fmt", "gray", "-f", "rawvideo"])
        .arg(&out)
        .status()
        .expect("ffmpeg runs");
    assert!(
        status.success(),
        "ffmpeg could not read {}",
        input.display()
    );
    let bytes = std::fs::read(&out).expect("raw frame");
    let _ = std::fs::remove_file(&out);
    bytes
}

/// Pixels that differ by more than 12 grey levels inside a rectangle: (changed, total).
fn changed_counts(
    a: &[u8],
    b: &[u8],
    width: usize,
    rect: (usize, usize, usize, usize),
) -> (usize, usize) {
    let (x0, y0, x1, y1) = rect;
    let mut changed = 0usize;
    let mut total = 0usize;
    for y in y0..y1 {
        for x in x0..x1 {
            let index = y * width + x;
            total += 1;
            if (a[index] as i32 - b[index] as i32).abs() > 12 {
                changed += 1;
            }
        }
    }
    (changed, total)
}

fn share(counts: (usize, usize)) -> f64 {
    counts.0 as f64 / counts.1.max(1) as f64
}

fn ffprobe(input: &Path, entries: &str) -> String {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            entries,
            "-of",
            "default=nw=1",
        ])
        .arg(input)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The verification has to discriminate: an ordinary opaque encode must be rejected.
#[test]
fn opaque_video_is_reported_as_having_no_alpha() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("ffmpeg not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("opaque.mp4");
    let status = Command::new("ffmpeg")
        .args([
            "-y", "-v", "error",
            "-f", "lavfi", "-i", "testsrc2=size=64x36:rate=12:duration=1",
            "-c:v", "libx264", "-pix_fmt", "yuv420p",
        ])
        .arg(&base)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success());
    let check = alpha::verify_alpha(&base).expect("probe runs");
    eprintln!(
        "opaque mp4: codec={} pix_fmt={} has_alpha={} verified={}",
        check.codec, check.pix_fmt, check.has_alpha_channel, check.verified
    );
    assert_eq!(check.pix_fmt, "yuv420p");
    assert!(!check.has_alpha_channel);
    assert!(!check.verified);
    assert_eq!(check.frames_sampled, 0, "no alpha plane means nothing to decode");
}

#[test]
#[ignore = "requires a real Blender install (set CINVAR_BLENDER)"]
fn real_transparent_render_delivers_verified_alpha() {
    blender_path();
    assert!(have("ffmpeg"), "ffmpeg is required");
    let dir = tempfile::tempdir().unwrap();
    let spec = transparent_spec();
    let render = blender::render_animation_job(&spec, dir.path(), &blender::RenderControl::none())
        .expect("transparent render");
    eprintln!("primary clip: {}", render.clip.display());
    eprintln!(
        "encoders: prores_4444={:?} vp9_alpha={:?}",
        render.encoders.map(|e| e.prores_4444),
        render.encoders.map(|e| e.vp9_alpha)
    );
    assert!(render.transparent);
    assert!(
        !render.alpha.is_empty(),
        "a transparent render must verify its delivered alpha"
    );
    for check in &render.alpha {
        eprintln!(
            "{}: codec={} pix_fmt={} frames={} min={} max={} mean={:.2} coverage={:.4} verified={}",
            check.path.display(),
            check.codec,
            check.pix_fmt,
            check.frames_sampled,
            check.alpha_min,
            check.alpha_max,
            check.alpha_mean,
            check.coverage,
            check.verified
        );
        // Delivered alpha: the encoded pixel format itself must carry the channel.
        assert!(
            alpha::pix_fmt_has_alpha(&check.pix_fmt),
            "delivered pix_fmt has no alpha: {}",
            check.pix_fmt
        );
        assert!(check.has_alpha_channel);
        assert!(
            check.alpha_max > 0,
            "alpha is all zero (nothing rendered) in {}",
            check.path.display()
        );
        assert!(
            !check.fully_opaque,
            "alpha is uniformly opaque in {}: the alpha was flattened",
            check.path.display()
        );
        assert!(
            check.verified,
            "alpha not verified for {}",
            check.path.display()
        );
        assert!(
            check.coverage > 0.001,
            "alpha coverage {:.4} is trivial",
            check.coverage
        );
    }
    assert!(render.alpha_verified());
    // WebM is only delivered when the probe verified this ffmpeg can keep VP9 alpha.
    match render.encoders {
        Some(support) if support.vp9_alpha => {
            assert!(render.webm.is_some(), "verified VP9 alpha but no WebM was written");
            let check = render
                .alpha
                .iter()
                .find(|c| c.path == *render.webm.as_ref().unwrap())
                .expect("the WebM was verified");
            assert!(check.verified);
        }
        Some(_) => assert!(
            render.webm.is_none(),
            "no verified VP9 alpha, yet a WebM was delivered"
        ),
        None => panic!("a transparent render must report its verified encoders"),
    }

    // Independent probe, not through our own helper: what does ffprobe say about the file?
    let pix = ffprobe(
        &render.clip,
        "stream=codec_name,pix_fmt,width,height:stream_tags=encoder",
    );
    eprintln!("ffprobe {}:\n{}", render.clip.display(), pix);
    assert!(
        pix.contains("yuva"),
        "ffprobe reports no alpha pixel format for {}:\n{pix}",
        render.clip.display()
    );

    // The PNG sequence stays on disk for direct timeline compositing.
    let sequence = blender::frames_pattern(&render.job_dir);
    assert!(sequence.to_string_lossy().ends_with("frame_%04d.png"));
    let frames = std::fs::read_dir(blender::frames_dir(&render.job_dir))
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .map(|e| e.path().extension().and_then(|x| x.to_str()) == Some("png"))
                .unwrap_or(false)
        })
        .count();
    eprintln!("npng frames: {frames}");
    assert_eq!(frames, render.frames);
    let first_png = blender::frames_dir(&render.job_dir).join("frame_0001.png");
    let (min, max, mean, coverage) = alpha::alpha_plane_stats(&first_png, None).expect("png alpha");
    eprintln!("frame_0001.png alpha min={min} max={max} mean={mean:.2} coverage={coverage:.4}");
    assert!(max > 0 && min < max, "RGBA PNG carries no varying alpha");
}

#[test]
#[ignore = "requires a real Blender install (set CINVAR_BLENDER)"]
fn real_transparent_clip_composites_over_existing_video() {
    blender_path();
    assert!(have("ffmpeg") && have("ffprobe"));
    let dir = tempfile::tempdir().unwrap();
    let spec = transparent_spec();
    let render = blender::render_animation_job(&spec, dir.path(), &blender::RenderControl::none())
        .expect("transparent render");

    let base = dir.path().join("base.mp4");
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=24:duration=3",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=330:duration=3",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(&base)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success());

    // Clip input: the alpha-preserving encode.
    let clip_out = dir.path().join("composited_clip.mp4");
    let overlay = OverlaySpec::default()
        .at(Time::from_secs_f64(1.0))
        .anchor(alpha::Anchor::BottomRight, 16)
        .width(200);
    alpha::composite_over_video(
        &base,
        &OverlaySource::file(render.clip.clone()),
        &overlay,
        &clip_out,
    )
    .expect("composite the transparent clip over the base video");
    eprintln!("composited clip: {}", clip_out.display());
    assert!(std::fs::metadata(&clip_out).unwrap().len() > 0);
    let probe = ffprobe(&clip_out, "stream=codec_type,width,height:format=duration");
    eprintln!("ffprobe composited:\n{probe}");
    assert!(probe.contains("width=640") && probe.contains("height=360"));

    // Placement and timestamps, measured: the bottom-right 200x~112 box changes inside the
    // overlay window, the rest of the frame does not, and nothing changes before it starts.
    const W: usize = 640;
    const H: usize = 360;
    let rect = (W - 200 - 16, H - 112 - 16, W - 16, H - 16);
    let base_frame = grey_frame(&base, 1.4);
    let out_frame = grey_frame(&clip_out, 1.4);
    assert_eq!(base_frame.len(), W * H);
    let panel = changed_counts(&base_frame, &out_frame, W, rect);
    let all = changed_counts(&base_frame, &out_frame, W, (0, 0, W, H));
    let inside = share(panel);
    let outside = share((all.0 - panel.0, all.1 - panel.1));
    eprintln!(
        "t=1.4s: {:.2}% of the overlay box changed, {:.3}% of the rest of the frame",
        inside * 100.0,
        outside * 100.0
    );
    assert!(
        inside > 0.01,
        "the overlay box barely changed ({:.3}%): the overlay is missing",
        inside * 100.0
    );
    assert!(
        outside < 0.01,
        "the composite changed {:.2}% of the frame outside the overlay box: wrong placement",
        outside * 100.0
    );
    let early_base = grey_frame(&base, 0.5);
    let early_out = grey_frame(&clip_out, 0.5);
    let early = share(changed_counts(&early_base, &early_out, W, (0, 0, W, H)));
    eprintln!(
        "t=0.5s (before the overlay starts): {:.3}% changed",
        early * 100.0
    );
    assert!(
        early < 0.01,
        "the overlay is visible at t=0.5s but starts at 1.0s ({:.2}% changed)",
        early * 100.0
    );

    // PNG sequence path: the same frames composite straight onto the timeline.
    let sequence_out = dir.path().join("composited_sequence.mp4");
    alpha::composite_over_video(
        &base,
        &blender::png_sequence_source(&render.job_dir, &spec),
        &overlay,
        &sequence_out,
    )
    .expect("composite the PNG sequence over the base video");
    let seq_frame = grey_frame(&sequence_out, 1.4);
    let seq_inside = share(changed_counts(&base_frame, &seq_frame, W, rect));
    eprintln!(
        "sequence composite changed {:.2}% of the overlay box",
        seq_inside * 100.0
    );
    assert!(
        seq_inside > 0.01,
        "the PNG-sequence composite did not draw the overlay ({:.3}% changed)",
        seq_inside * 100.0
    );

    // Audio from the base survives.
    let audio = ffprobe(&clip_out, "stream=codec_type");
    assert!(audio.contains("audio"), "base audio was dropped:\n{audio}");
}
