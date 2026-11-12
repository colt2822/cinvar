//! Director's Room: keyframe animation is measured on rendered pixels, not on filter strings.

use std::path::{Path, PathBuf};
use std::process::Command;

use cinvar::project::model::Fit;
use cinvar::project::ops;
use cinvar::project::store::ProjectStore;
use cinvar::render::{self, RenderRequest};
use cinvar::util::time::{parse_fps, Time};

const W: usize = 640;
const H: usize = 360;

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn ffmpeg(args: &[&str]) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"])
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A white box (160x90) centred on black, 4 s, no audio.
fn box_clip(path: &Path) -> bool {
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=640x360:r=30:d=4,drawbox=x=240:y=135:w=160:h=90:color=white:t=fill",
        "-pix_fmt",
        "yuv420p",
        path.to_str().unwrap(),
    ])
}

fn gray_frame(video: &Path, t: f64) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-ss",
            &format!("{t}"),
            "-i",
            video.to_str().unwrap(),
        ])
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "gray", "-"])
        .output()
        .expect("ffmpeg runs");
    assert_eq!(out.stdout.len(), W * H, "one gray frame at t={t}");
    out.stdout
}

/// (x0, x1, y0, y1) of pixels brighter than the threshold.
fn bbox(frame: &[u8], threshold: u8) -> Option<(usize, usize, usize, usize)> {
    let (mut x0, mut x1, mut y0, mut y1) = (usize::MAX, 0, usize::MAX, 0);
    let mut any = false;
    for y in 0..H {
        for x in 0..W {
            if frame[y * W + x] > threshold {
                any = true;
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
        }
    }
    any.then_some((x0, x1, y0, y1))
}

struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    store: ProjectStore,
    project: cinvar::project::model::Project,
    clip: String,
}

fn rig(fit: Fit) -> Option<Rig> {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let src = root.join("box.mp4");
    if !box_clip(&src) {
        return None;
    }
    let projects = root.join("projects");
    std::fs::create_dir_all(&projects).unwrap();
    let (store, mut project) = ProjectStore::create(&projects, "kf").unwrap();
    let asset = store.import_asset(&mut project, &src, "test").unwrap();
    ops::create_timeline(
        &mut project,
        "Main",
        W as u32,
        H as u32,
        parse_fps("30").unwrap(),
    )
    .unwrap();
    let clip = ops::append_clip(&mut project, None, None, &asset.id).unwrap();
    let mut transform = project.clip(&clip).unwrap().transform.clone();
    transform.fit = fit;
    ops::set_transform(&mut project, &clip, transform).unwrap();
    Some(Rig {
        _dir: dir,
        root,
        store,
        project,
        clip,
    })
}

fn render_rig(r: &Rig, name: &str) -> PathBuf {
    let out = r.root.join(name);
    let mut req = RenderRequest::for_output(&out);
    req.width = Some(W as u32);
    req.height = Some(H as u32);
    req.crf = Some(14);
    let outcome =
        render::render(&r.project, &r.store, &req, &|_, _| {}, &|| false).expect("render succeeds");
    // A host whose FFmpeg lists a hardware encoder it cannot actually use (CI runners) is told so
    // by a warning; that environment notice is expected, anything else is not.
    let unexpected: Vec<_> = outcome
        .warnings
        .iter()
        .filter(|w| {
            !w.contains("probe encode failed")
                && !w.contains("available memory could not be determined")
        })
        .collect();
    assert!(
        unexpected.is_empty(),
        "no silent fallbacks: {:?}",
        unexpected
    );
    out
}

fn kf(r: &mut Rig, prop: &str, at: f64, value: f64, easing: &str) {
    let start = r.project.clip(&r.clip).unwrap().timeline_start;
    ops::add_keyframe(
        &mut r.project,
        &r.clip,
        prop,
        start + Time::from_secs_f64(at),
        value,
        easing,
    )
    .unwrap();
}

fn close(actual: f64, expected: f64, tol: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tol,
        "{what}: got {actual}, expected {expected} +/- {tol}"
    );
}

#[test]
fn scale_and_position_animate_on_real_pixels() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg missing");
        return;
    }
    let Some(mut r) = rig(Fit::Contain) else {
        return;
    };
    kf(&mut r, "scale", 0.0, 1.0, "linear");
    kf(&mut r, "scale", 4.0, 2.0, "linear");
    kf(&mut r, "x", 0.0, 0.0, "linear");
    kf(&mut r, "x", 4.0, 100.0, "linear");
    kf(&mut r, "y", 0.0, 0.0, "linear");
    kf(&mut r, "y", 4.0, -40.0, "linear");
    let out = render_rig(&r, "anim.mp4");
    for t in [0.0_f64, 1.0, 2.0, 3.5] {
        let (x0, x1, y0, y1) = bbox(&gray_frame(&out, t), 128).expect("box visible");
        let f = t / 4.0;
        close(
            (x1 - x0 + 1) as f64,
            160.0 * (1.0 + f),
            3.0,
            &format!("width at {t}s"),
        );
        close(
            (y1 - y0 + 1) as f64,
            90.0 * (1.0 + f),
            3.0,
            &format!("height at {t}s"),
        );
        close(
            (x0 + x1) as f64 / 2.0,
            320.0 + 100.0 * f,
            2.5,
            &format!("centre x at {t}s"),
        );
        close(
            (y0 + y1) as f64 / 2.0,
            180.0 - 40.0 * f,
            2.5,
            &format!("centre y at {t}s"),
        );
    }
}

#[test]
fn eased_zoom_is_slower_at_the_start_than_linear() {
    if !have_ffmpeg() {
        return;
    }
    let Some(mut r) = rig(Fit::Contain) else {
        return;
    };
    kf(&mut r, "scale", 0.0, 1.0, "ease_in");
    kf(&mut r, "scale", 4.0, 2.0, "linear");
    let out = render_rig(&r, "ease.mp4");
    let (x0, x1, _, _) = bbox(&gray_frame(&out, 1.0), 128).unwrap();
    // ease_in at p=0.25 is 0.0625, so scale 1.0625 and width 170, not the linear 200.
    close((x1 - x0 + 1) as f64, 170.0, 3.0, "eased width at 1s");
}

#[test]
fn opacity_animates_and_composites_over_the_layer_below() {
    if !have_ffmpeg() {
        return;
    }
    let Some(mut r) = rig(Fit::Cover) else { return };
    // Full-canvas zoom so the white box fills the frame, fading 0 -> 1 over 4 s.
    kf(&mut r, "scale", 0.0, 6.0, "linear");
    kf(&mut r, "opacity", 0.0, 0.0, "linear");
    kf(&mut r, "opacity", 4.0, 1.0, "linear");
    let out = render_rig(&r, "fade.mp4");
    let mean = |t: f64| {
        let f = gray_frame(&out, t);
        f.iter().map(|v| *v as f64).sum::<f64>() / f.len() as f64
    };
    let (a, b, c) = (mean(0.2), mean(2.0), mean(3.8));
    assert!(a < b && b < c, "brightness rises with opacity: {a} {b} {c}");
    close(
        b,
        0.5 * 235.0,
        25.0,
        "half opacity is about half brightness",
    );
}

#[test]
fn rotation_animates() {
    if !have_ffmpeg() {
        return;
    }
    let Some(mut r) = rig(Fit::Contain) else {
        return;
    };
    kf(&mut r, "rotation", 0.0, 0.0, "linear");
    kf(&mut r, "rotation", 4.0, 90.0, "linear");
    let out = render_rig(&r, "rot.mp4");
    let (x0, x1, y0, y1) = bbox(&gray_frame(&out, 0.0), 128).unwrap();
    assert!(x1 - x0 > y1 - y0, "starts wider than tall");
    let (x0, x1, y0, y1) = bbox(&gray_frame(&out, 3.95), 128).unwrap();
    assert!(
        y1 - y0 > x1 - x0,
        "ends about 90 degrees later, taller than wide"
    );
}

#[test]
fn unsupported_property_or_easing_is_refused_not_ignored() {
    let Some(mut r) = rig(Fit::Contain) else {
        return;
    };
    let start = r.project.clip(&r.clip).unwrap().timeline_start;
    let err = ops::add_keyframe(&mut r.project, &r.clip, "blur", start, 1.0, "linear").unwrap_err();
    assert!(err.to_string().contains("animatable"), "{err}");
    assert!(ops::add_keyframe(&mut r.project, &r.clip, "x", start, 1.0, "bounce").is_err());
}

#[test]
fn animation_follows_clip_time_under_speed_change() {
    if !have_ffmpeg() {
        return;
    }
    let Some(mut r) = rig(Fit::Contain) else {
        return;
    };
    ops::set_speed(&mut r.project, &r.clip, 2.0).unwrap(); // 4 s source -> 2 s on the timeline
    kf(&mut r, "scale", 0.0, 1.0, "linear");
    kf(&mut r, "scale", 2.0, 2.0, "linear");
    let out = render_rig(&r, "speed.mp4");
    let (x0, x1, _, _) = bbox(&gray_frame(&out, 1.0), 128).unwrap();
    close(
        (x1 - x0 + 1) as f64,
        240.0,
        4.0,
        "halfway through the 2 s clip the zoom is at 1.5x (box 160*1.5)",
    );
    let (x0, x1, _, _) = bbox(&gray_frame(&out, 1.9), 128).unwrap();
    assert!((x1 - x0 + 1) as f64 > 290.0, "near the end it is near 2x");
}

#[test]
fn range_render_keeps_animation_aligned_to_the_clip() {
    if !have_ffmpeg() {
        return;
    }
    let Some(mut r) = rig(Fit::Contain) else {
        return;
    };
    kf(&mut r, "scale", 0.0, 1.0, "linear");
    kf(&mut r, "scale", 4.0, 2.0, "linear");
    let out = r.root.join("range.mp4");
    let mut req = RenderRequest::for_output(&out);
    req.width = Some(W as u32);
    req.height = Some(H as u32);
    req.range = Some((Time::from_secs_f64(2.0), Time::from_secs_f64(4.0)));
    render::render(&r.project, &r.store, &req, &|_, _| {}, &|| false).expect("range render");
    // The range starts at 2 s of the clip, where the zoom is 1.5x.
    let (x0, x1, _, _) = bbox(&gray_frame(&out, 0.0), 128).unwrap();
    close(
        (x1 - x0 + 1) as f64,
        240.0,
        4.0,
        "range start already at 1.5x",
    );
}

fn text_rig() -> Option<Rig> {
    let mut r = rig(Fit::Contain)?;
    // Remove the box clip so only text remains over black.
    ops::remove_clip(&mut r.project, &r.clip.clone()).ok()?;
    Some(r)
}

fn add_title(r: &mut Rig, text: &str, anim_in: Option<&str>, anim_out: Option<&str>) -> String {
    use cinvar::project::model::{TextOverlay, TextPosition};
    let overlay = TextOverlay {
        size: 40,
        position: TextPosition::Center,
        outline_width: 0,
        animate_in: anim_in.map(String::from),
        animate_out: anim_out.map(String::from),
        ..TextOverlay::default()
    };
    ops::add_text(
        &mut r.project,
        None,
        text,
        Time::ZERO,
        Time::from_secs_f64(4.0),
        Some(overlay),
    )
    .unwrap()
}

#[test]
fn long_titles_wrap_inside_the_frame() {
    if !have_ffmpeg() {
        return;
    }
    let Some(mut r) = text_rig() else { return };
    add_title(
        &mut r,
        "A very long headline that cannot possibly fit on one line of a small frame",
        None,
        None,
    );
    let out = render_rig(&r, "wrap.mp4");
    let (x0, x1, y0, y1) = bbox(&gray_frame(&out, 1.0), 128).expect("text drawn");
    assert!(
        x0 >= 20 && x1 <= W - 20,
        "text inside side margins: {x0}..{x1}"
    );
    assert!(
        y1 - y0 > 60,
        "wrapped onto several lines, height {}",
        y1 - y0
    );
    // The font must not have been shrunk to illegibility: line height stays near 40 px.
    assert!(y1 - y0 < 6 * 60, "not an absurd number of lines");
}

#[test]
fn text_fade_in_and_out_are_real() {
    if !have_ffmpeg() {
        return;
    }
    let Some(mut r) = text_rig() else { return };
    add_title(&mut r, "FADE", Some("fade"), Some("fade"));
    let out = render_rig(&r, "tfade.mp4");
    let peak = |t: f64| *gray_frame(&out, t).iter().max().unwrap() as f64;
    assert!(
        peak(0.0) < 30.0,
        "invisible at the start, peak {}",
        peak(0.0)
    );
    assert!(peak(2.0) > 200.0, "fully visible mid-clip");
    assert!(peak(3.9) < 90.0, "faded out at the end, peak {}", peak(3.9));
}

#[test]
fn text_slide_up_moves_into_place() {
    if !have_ffmpeg() {
        return;
    }
    let Some(mut r) = text_rig() else { return };
    add_title(&mut r, "SLIDE", Some("slide_up"), None);
    let out = render_rig(&r, "tslide.mp4");
    let mid_y = |t: f64| {
        let (_, _, y0, y1) = bbox(&gray_frame(&out, t), 40).unwrap_or((0, 0, 0, 0));
        (y0 + y1) as f64 / 2.0
    };
    assert!(
        mid_y(0.25) > mid_y(2.0) + 4.0,
        "starts lower than where it settles: {} vs {}",
        mid_y(0.25),
        mid_y(2.0)
    );
}

#[test]
fn text_rejects_scale_keyframes_instead_of_ignoring_them() {
    let Some(mut r) = text_rig() else { return };
    let id = add_title(&mut r, "NOPE", None, None);
    kf_clip(&mut r, &id, "scale", 0.0, 1.0);
    kf_clip(&mut r, &id, "scale", 2.0, 2.0);
    if !have_ffmpeg() {
        return;
    }
    let out = r.root.join("tbad.mp4");
    let mut req = RenderRequest::for_output(&out);
    req.width = Some(W as u32);
    req.height = Some(H as u32);
    let err = render::render(&r.project, &r.store, &req, &|_, _| {}, &|| false).unwrap_err();
    assert!(
        err.to_string().contains("text clips cannot animate"),
        "{err}"
    );
}

fn kf_clip(r: &mut Rig, clip: &str, prop: &str, at: f64, value: f64) {
    ops::add_keyframe(
        &mut r.project,
        clip,
        prop,
        Time::from_secs_f64(at),
        value,
        "linear",
    )
    .unwrap();
}

#[test]
fn presets_expand_to_real_animation() {
    if !have_ffmpeg() {
        return;
    }
    let Some(mut r) = rig(Fit::Contain) else {
        return;
    };
    let written =
        ops::apply_animation_preset(&mut r.project, &r.clip, "push_in", 1.0, None).unwrap();
    assert!(written.len() >= 2);
    let out = render_rig(&r, "push.mp4");
    let width = |t: f64| {
        let (x0, x1, _, _) = bbox(&gray_frame(&out, t), 128).unwrap();
        x1 - x0 + 1
    };
    assert!(
        width(3.9) > width(0.1) + 8,
        "push_in grows the box: {} -> {}",
        width(0.1),
        width(3.9)
    );
    assert!(ops::apply_animation_preset(&mut r.project, &r.clip, "wobble", 1.0, None).is_err());
}

#[test]
fn preview_and_export_text_have_the_same_proportions() {
    if !have_ffmpeg() {
        return;
    }
    let Some(mut r) = text_rig() else { return };
    add_title(&mut r, "WYSIWYG", None, None);
    let size_at = |w: u32, h: u32, name: &str| {
        let out = r.root.join(name);
        let mut req = RenderRequest::for_output(&out);
        req.width = Some(w);
        req.height = Some(h);
        render::render(&r.project, &r.store, &req, &|_, _| {}, &|| false).unwrap();
        let raw = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-ss",
                "1",
                "-i",
                out.to_str().unwrap(),
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "gray",
                "-",
            ])
            .output()
            .unwrap()
            .stdout;
        let (w, h) = (w as usize, h as usize);
        let (mut x0, mut x1) = (w, 0);
        for y in 0..h {
            for x in 0..w {
                if raw[y * w + x] > 128 {
                    x0 = x0.min(x);
                    x1 = x1.max(x);
                }
            }
        }
        (x1 - x0 + 1) as f64 / w as f64
    };
    let full = size_at(640, 360, "full.mp4");
    let preview = size_at(320, 180, "half.mp4");
    close(
        preview,
        full,
        0.02,
        "text occupies the same fraction of the frame in a half-size preview",
    );
}

#[test]
fn bold_capital_titles_stay_inside_the_frame_at_large_sizes() {
    if !have_ffmpeg() {
        return;
    }
    let Some(mut r) = text_rig() else { return };
    use cinvar::project::model::{TextOverlay, TextPosition};
    let overlay = TextOverlay {
        size: 80, // timeline is 640 wide: very large for a 20-character bold title
        position: TextPosition::Center,
        outline_width: 0,
        font: Some("DejaVu Sans Bold".into()),
        ..TextOverlay::default()
    };
    ops::add_text(
        &mut r.project,
        None,
        "WHERE LIGHT BEGINS",
        Time::ZERO,
        Time::from_secs_f64(3.0),
        Some(overlay),
    )
    .unwrap();
    let out = render_rig(&r, "bold.mp4");
    let (x0, x1, _, _) = bbox(&gray_frame(&out, 1.0), 128).expect("title drawn");
    assert!(
        x0 >= 16 && x1 <= W - 16,
        "bold caps inside side margins: {x0}..{x1}"
    );
}
