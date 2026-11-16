//! Regression: ffmpeg parses a filtergraph option value twice and neither layer can escape a
//! quote inside single quotes, so a font or subtitle file under a path holding an apostrophe used
//! to be either silently dropped (drawtext then rendered a different face with no error at all) or
//! refused outright ("Unable to open .../its/sub.srt"). These tests run real ffmpeg and compare
//! measured frames, not strings.

use std::path::{Path, PathBuf};
use std::process::Command;

use cinvar::project::model::{TextOverlay, TextPosition};
use cinvar::project::ops;
use cinvar::project::store::ProjectStore;
use cinvar::render::{self, RenderRequest};
use cinvar::util::paths;
use cinvar::util::time::{parse_fps, Time};

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-filters"])
        .output()
        .map(|o| {
            let listing = String::from_utf8_lossy(&o.stdout);
            o.status.success()
                && listing.contains(" drawtext ")
                && listing.contains(" subtitles ")
        })
        .unwrap_or(false)
}

fn run_ffmpeg(args: &[String]) -> bool {
    Command::new("ffmpeg")
        .args(args)
        .output()
        .map(|o| {
            if !o.status.success() {
                eprintln!("ffmpeg failed: {}", String::from_utf8_lossy(&o.stderr));
            }
            o.status.success()
        })
        .unwrap_or(false)
}

/// A serif face renders visibly differently from drawtext's own default, which is what makes
/// "the intended face was used" measurable. Any installed face is accepted.
fn a_font_file() -> Option<PathBuf> {
    [
        "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSerif-Regular.ttf",
        "/usr/share/fonts/truetype/freefont/FreeSerif.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
}

/// One decoded frame at "at" seconds as raw grey pixels. Comparing pixels rather than encoded
/// bytes is deliberate: the renderer's H.264 encoder is not byte-stable under load, which is the
/// same reason the distributed-render work compares PSNR instead of hashes.
fn frame(video: &Path, at: f64) -> Option<Vec<u8>> {
    let out = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-ss",
            &format!("{at:.3}"),
            "-i",
            &video.to_string_lossy(),
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "-",
        ])
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    Some(out.stdout)
}

/// Mean absolute difference per sample, 0..255. Two renders of the same face land well under 1;
/// a different typeface lands far above it.
fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return f64::INFINITY;
    }
    let total: u64 = (0..n)
        .map(|i| (a[i] as i32 - b[i] as i32).unsigned_abs() as u64)
        .sum();
    total as f64 / n as f64
}

/// Fraction of samples differing by more than the given number of grey levels. A thin caption
/// glyph is high contrast but covers few pixels, so counting visibly changed samples says "the
/// text is there" far better than a mean does.
fn changed_fraction(a: &[u8], b: &[u8], threshold: i32) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let changed = (0..n)
        .filter(|&i| (a[i] as i32 - b[i] as i32).abs() > threshold)
        .count();
    changed as f64 / n as f64
}

fn synth_clip(path: &Path) -> bool {
    run_ffmpeg(&[
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-y".to_string(),
        "-f".to_string(),
        "lavfi".to_string(),
        "-i".to_string(),
        "testsrc2=size=320x180:rate=30:duration=1".to_string(),
        "-c:v".to_string(),
        "libx264".to_string(),
        "-pix_fmt".to_string(),
        "yuv420p".to_string(),
        path.to_string_lossy().to_string(),
    ])
}

/// Every shape of path the brief names: an apostrophe, spaces, non-ASCII, a comma, and a colon
/// (which is what a Windows drive letter looks like once the path reaches the filtergraph).
fn tricky_names() -> Vec<String> {
    vec![
        "owner's cut".to_string(),
        "Fonts With Spaces".to_string(),
        "hellö 视频".to_string(),
        "a,b".to_string(),
        "drive C: style".to_string(),
    ]
}

#[test]
fn a_filtergraph_path_is_staged_when_and_only_when_it_has_to_be() {
    let dir = tempfile::tempdir().unwrap();
    for name in tricky_names() {
        let source_dir = dir.path().join(&name);
        std::fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join("sub.srt");
        std::fs::write(&source, b"1\n00:00:00,000 --> 00:00:01,000\nhello\n").unwrap();
        let staged = paths::stage_for_filtergraph(&source)
            .unwrap_or_else(|e| panic!("a stand-in is produced for {name}: {}", e.message));
        assert!(staged.is_file(), "{name} staged to {}", staged.display());
        assert!(
            !paths::filtergraph_inexpressible(&staged),
            "the stand-in is expressible: {}",
            staged.display()
        );
        assert_eq!(
            std::fs::read(&staged).unwrap(),
            std::fs::read(&source).unwrap()
        );
        assert_eq!(
            staged.extension().and_then(|e| e.to_str()),
            Some("srt"),
            "the extension survives staging, because drawtext and libass sniff it"
        );
        // Only the apostrophe needs this at all.
        assert_eq!(
            paths::filtergraph_inexpressible(&source),
            name.contains('\''),
            "{name}: staging is for what ffmpeg cannot carry"
        );
    }
}

/// The burn-in used to fail outright: ffmpeg stripped the escaped quote and then could not open
/// the subtitle file. Every shape of path now burns, and all of them render the same picture.
#[test]
fn subtitle_burn_in_works_for_every_shape_of_path() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg with drawtext and subtitles is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let source_video = dir.path().join("source.mp4");
    if !synth_clip(&source_video) {
        eprintln!("skipping: could not synthesise a clip");
        return;
    }
    let srt_body = "1\n00:00:00,050 --> 00:00:00,900\nburned in\n";
    let mut reference: Option<Vec<u8>> = None;
    for (index, name) in tricky_names().into_iter().enumerate() {
        let subtitle_dir = dir.path().join(&name);
        std::fs::create_dir_all(&subtitle_dir).unwrap();
        let subtitle = subtitle_dir.join("captions.srt");
        std::fs::write(&subtitle, srt_body).unwrap();
        let burned = dir.path().join(format!("burned-{index}.mp4"));
        cinvar::captions::ass::burn_subtitle_file(&source_video, &subtitle, &burned)
            .unwrap_or_else(|e| panic!("burn-in from {name} succeeds: {}", e.message));
        let picture =
            frame(&burned, 0.4).unwrap_or_else(|| panic!("the burn-in from {name} decodes"));
        let untouched = frame(&source_video, 0.4).expect("the source decodes");
        let burned_fraction = changed_fraction(&picture, &untouched, 24);
        eprintln!(
            "{name}: {:.3}% of samples changed by the burn-in, mean {:.3}",
            burned_fraction * 100.0,
            mean_abs_diff(&picture, &untouched)
        );
        // The caption covers few pixels of a 320x180 frame (0.34% measured on ffmpeg 6.1.1), so
        // the floor is "some high-contrast text" rather than a large area: no caption at all
        // scores 0.0%, and two renders of the same picture also score 0.0%.
        assert!(
            burned_fraction > 0.001,
            "{name}: the caption really was burned into the picture ({:.4}% changed, expected above 0.1%)",
            burned_fraction * 100.0
        );
        match &reference {
            None => reference = Some(picture),
            Some(first) => {
                let drift = changed_fraction(first, &picture, 24);
                eprintln!(
                    "{name}: differs from the first burn-in by {:.4}%",
                    drift * 100.0
                );
                assert!(
                    drift < 0.001,
                    "{name}: the same script burns to the same picture ({:.4}% drifted)",
                    drift * 100.0
                );
            }
        }
    }
}

/// Why staging exists rather than better escaping: an apostrophe cannot be escaped into the
/// filtergraph's single quotes. Asserted against ffmpeg, not assumed.
#[test]
fn escaping_an_apostrophe_into_the_filtergraph_cannot_work() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg with subtitles is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let source_video = dir.path().join("source.mp4");
    if !synth_clip(&source_video) {
        return;
    }
    let subtitle_dir = dir.path().join("owner's cut");
    std::fs::create_dir_all(&subtitle_dir).unwrap();
    let subtitle = subtitle_dir.join("captions.srt");
    std::fs::write(&subtitle, "1\n00:00:00,050 --> 00:00:00,900\nburned in\n").unwrap();

    let escaped = subtitle
        .to_string_lossy()
        .replace(':', "\\:")
        .replace('\'', "\\'");
    let naive = dir.path().join("naive.mp4");
    let naive_worked = run_ffmpeg(&[
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-y".to_string(),
        "-i".to_string(),
        source_video.to_string_lossy().to_string(),
        "-vf".to_string(),
        format!("subtitles=filename='{escaped}'"),
        "-c:v".to_string(),
        "libx264".to_string(),
        "-pix_fmt".to_string(),
        "yuv420p".to_string(),
        naive.to_string_lossy().to_string(),
    ]);
    if naive_worked {
        // If a future ffmpeg learns to escape it, the render must at least show the caption.
        let burned = frame(&naive, 0.4).expect("decodes");
        let untouched = frame(&source_video, 0.4).expect("decodes");
        assert!(
            mean_abs_diff(&burned, &untouched) > 2.0,
            "an escaped apostrophe never silently drops the subtitle"
        );
    }
}

/// The end-to-end claim: a title whose font lives under a path with an apostrophe renders the
/// requested face, and that face is measurably not the fallback the dropped option produced.
#[test]
fn a_rendered_title_uses_the_font_named_under_an_apostrophe_path() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg/ffprobe are not installed");
        return;
    }
    let Some(font_source) = a_font_file() else {
        eprintln!("skipping: no system font to copy");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let source_clip = dir.path().join("source.mp4");
    if !synth_clip(&source_clip) {
        eprintln!("skipping: could not synthesise a clip");
        return;
    }
    let apostrophe_dir = dir.path().join("owner's fonts");
    std::fs::create_dir_all(&apostrophe_dir).unwrap();
    let apostrophe_font = apostrophe_dir.join("Requested Serif.ttf");
    std::fs::copy(&font_source, &apostrophe_font).unwrap();
    let clean_font = dir.path().join("requested-serif.ttf");
    std::fs::copy(&font_source, &clean_font).unwrap();

    let render_with = |font: Option<&Path>, label: &str| -> Option<PathBuf> {
        let projects = dir.path().join(format!("projects-{label}"));
        std::fs::create_dir_all(&projects).unwrap();
        let (store, mut project) = ProjectStore::create(&projects, label).ok()?;
        let asset = store
            .import_asset(&mut project, &source_clip, "test:fonts")
            .ok()?;
        ops::create_timeline(&mut project, "Main", 320, 180, parse_fps("30").ok()?).ok()?;
        ops::append_clip(&mut project, None, None, &asset.id).ok()?;
        let overlay = TextOverlay {
            font: font.map(|p| p.to_string_lossy().to_string()),
            size: 40,
            position: TextPosition::Center,
            ..TextOverlay::default()
        };
        ops::add_text(
            &mut project,
            None,
            "Sphinx",
            Time::from_secs_f64(0.0),
            Time::from_secs_f64(1.0),
            Some(overlay),
        )
        .ok()?;
        store.save(&mut project).ok()?;
        let output = dir.path().join(format!("render-{label}.mp4"));
        let mut request = RenderRequest::for_output(&output);
        request.width = Some(320);
        request.height = Some(180);
        request.crf = Some(18);
        // The plan must name a font file, never the raw apostrophe path.
        let plan = render::plan_render(&project, &store, &request).ok()?;
        if font.is_some() {
            assert!(
                plan.filtergraph.contains(":fontfile="),
                "{label}: a font file reaches drawtext: {}",
                plan.filtergraph
            );
            assert!(
                !plan.filtergraph.contains("owner's"),
                "{label}: the raw apostrophe path is not handed to ffmpeg: {}",
                plan.filtergraph
            );
        }
        let progress = |_: f32, _: &str| {};
        let cancelled = || false;
        render::render(&project, &store, &request, &progress, &cancelled).ok()?;
        Some(output)
    };

    let (Some(apostrophe_out), Some(clean_out), Some(fallback_out)) = (
        render_with(Some(&apostrophe_font), "apostrophe"),
        render_with(Some(&clean_font), "clean"),
        render_with(None, "fallback"),
    ) else {
        eprintln!("skipping: the fixture could not be rendered");
        return;
    };

    let staged = frame(&apostrophe_out, 0.4).expect("decodes");
    let intended = frame(&clean_out, 0.4).expect("decodes");
    let fallback = frame(&fallback_out, 0.4).expect("decodes");

    let to_intended = mean_abs_diff(&staged, &intended);
    let to_fallback = mean_abs_diff(&staged, &fallback);
    let intended_to_fallback = mean_abs_diff(&intended, &fallback);
    eprintln!("apostrophe vs clean face   = {to_intended:.3} (must be near 0: the same face)");
    eprintln!("apostrophe vs default face = {to_fallback:.3}");
    eprintln!("clean vs default face      = {intended_to_fallback:.3}");

    let staged_changed = changed_fraction(&staged, &intended, 24);
    eprintln!(
        "apostrophe vs clean face: {:.4}% of samples differ",
        staged_changed * 100.0
    );
    assert!(
        to_intended < 1.0,
        "the font named under an apostrophe path renders the same face as the same font at a clean path (diff {to_intended:.3})"
    );
    if intended_to_fallback < 1.0 {
        eprintln!("skipping the fallback comparison: the default face is the requested face here");
    } else {
        assert!(
            to_fallback > 2.0,
            "the requested face must not degrade to the default one (diff {to_fallback:.3})"
        );
    }
}
