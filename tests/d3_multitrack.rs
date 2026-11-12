//! Director's Room: stacked video/image/text layers are composited in z order, verified on pixels.

use std::path::{Path, PathBuf};
use std::process::Command;

use cinvar::project::model::{Fit, TrackKind};
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
fn ff(args: &[&str]) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"])
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
fn rgb_at(video: &Path, t: f64, x: usize, y: usize) -> (u8, u8, u8) {
    let out = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-ss",
            &format!("{t}"),
            "-i",
            video.to_str().unwrap(),
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-",
        ])
        .output()
        .unwrap();
    assert_eq!(out.stdout.len(), W * H * 3);
    let i = (y * W + x) * 3;
    (out.stdout[i], out.stdout[i + 1], out.stdout[i + 2])
}
fn near(c: (u8, u8, u8), want: (u8, u8, u8), tol: i32, what: &str) {
    let d = |a: u8, b: u8| (a as i32 - b as i32).abs();
    assert!(
        d(c.0, want.0) <= tol && d(c.1, want.1) <= tol && d(c.2, want.2) <= tol,
        "{what}: got {c:?}, want {want:?}"
    );
}

fn solid(root: &Path, name: &str, color: &str, secs: u32) -> PathBuf {
    let p = root.join(name);
    assert!(ff(&[
        "-f",
        "lavfi",
        "-i",
        &format!("color=c={color}:s=640x360:r=30:d={secs}"),
        "-pix_fmt",
        "yuv420p",
        p.to_str().unwrap()
    ]));
    p
}

#[test]
fn four_layers_composite_in_z_order_with_timing() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let red = solid(&root, "red.mp4", "red", 6); // base, full frame
    let green = solid(&root, "green.mp4", "green", 3); // PiP, starts at 1 s
    let blue_png = root.join("blue.png");
    assert!(ff(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=blue:s=100x100",
        "-frames:v",
        "1",
        blue_png.to_str().unwrap()
    ]));

    let projects = root.join("p");
    std::fs::create_dir_all(&projects).unwrap();
    let (store, mut project) = ProjectStore::create(&projects, "mt").unwrap();
    let a_red = store.import_asset(&mut project, &red, "t").unwrap();
    let a_green = store.import_asset(&mut project, &green, "t").unwrap();
    let a_blue = store.import_asset(&mut project, &blue_png, "t").unwrap();
    ops::create_timeline(
        &mut project,
        "Main",
        W as u32,
        H as u32,
        parse_fps("30").unwrap(),
    )
    .unwrap();

    let base = ops::append_clip(&mut project, None, None, &a_red.id).unwrap();
    let _ = base;
    // Second and third video tracks above the base.
    let pip_track = ops::add_track(&mut project, None, TrackKind::Video, "PiP").unwrap();
    let logo_track = ops::add_track(&mut project, None, TrackKind::Video, "Logo").unwrap();
    let pip = ops::insert_clip(
        &mut project,
        None,
        Some(&pip_track),
        ops::ClipRequest {
            asset_id: &a_green.id,
            source_in: None,
            source_out: None,
            at: Some(Time::from_secs_f64(1.0)),
            name: None,
        },
    )
    .unwrap();
    let logo = ops::insert_clip(
        &mut project,
        None,
        Some(&logo_track),
        ops::ClipRequest {
            asset_id: &a_blue.id,
            source_in: None,
            source_out: Some(Time::from_secs_f64(4.0)),
            at: Some(Time::from_secs_f64(2.0)),
            name: None,
        },
    )
    .unwrap();

    // PiP: quarter size, top-right. Logo: small, bottom-left (offsets are from the canvas centre).
    let mut t = project.clip(&pip).unwrap().transform.clone();
    t.fit = Fit::Contain;
    t.scale = 0.25;
    t.x = 200;
    t.y = -120;
    ops::set_transform(&mut project, &pip, t).unwrap();
    let mut t = project.clip(&logo).unwrap().transform.clone();
    t.fit = Fit::Contain;
    t.scale = 0.2;
    t.x = -260;
    t.y = 140;
    ops::set_transform(&mut project, &logo, t).unwrap();
    // Track order is z order: later tracks sit on top.
    store.save(&mut project).unwrap();

    let out = root.join("mt.mp4");
    let mut req = RenderRequest::for_output(&out);
    req.width = Some(W as u32);
    req.height = Some(H as u32);
    req.crf = Some(14);
    let outcome = render::render(&project, &store, &req, &|_, _| {}, &|| false).expect("render");
    // Ignore the expected notice from hosts that list a hardware encoder they cannot use (CI runners).
    let unexpected: Vec<_> = outcome
        .warnings
        .iter()
        .filter(|w| {
            !w.contains("probe encode failed")
                && !w.contains("available memory could not be determined")
        })
        .collect();
    assert!(unexpected.is_empty(), "{:?}", unexpected);

    // t=0.5: only the base is visible.
    near(
        rgb_at(&out, 0.5, 520, 60),
        (255, 0, 0),
        12,
        "base alone at 0.5 s",
    );
    // t=1.5: PiP (green) is in the top right; base elsewhere.
    near(
        rgb_at(&out, 1.5, 520, 60),
        (0, 128, 0),
        14,
        "pip visible at 1.5 s",
    );
    near(
        rgb_at(&out, 1.5, 100, 300),
        (255, 0, 0),
        12,
        "base still red at the bottom left",
    );
    // t=2.5: logo (blue) bottom-left appears while PiP continues.
    near(
        rgb_at(&out, 2.5, 60, 300),
        (0, 0, 255),
        14,
        "logo visible at 2.5 s",
    );
    near(
        rgb_at(&out, 2.5, 520, 60),
        (0, 128, 0),
        14,
        "pip still visible",
    );
    // t=4.5: PiP ended (3 s clip from 1 s), logo still up.
    near(
        rgb_at(&out, 4.5, 520, 60),
        (255, 0, 0),
        12,
        "pip gone after it ends",
    );
    near(rgb_at(&out, 4.5, 60, 300), (0, 0, 255), 14, "logo persists");
    // t=5.5: logo ended (2..6 s of a 4 s clip => ends at 6 s, so still up)... base ends at 6 s.
}

#[test]
fn animated_pip_moves_across_the_base_without_blanking_it() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let red = solid(&root, "red.mp4", "red", 4);
    let green = solid(&root, "green.mp4", "green", 4);
    let projects = root.join("p");
    std::fs::create_dir_all(&projects).unwrap();
    let (store, mut project) = ProjectStore::create(&projects, "pip").unwrap();
    let a_red = store.import_asset(&mut project, &red, "t").unwrap();
    let a_green = store.import_asset(&mut project, &green, "t").unwrap();
    ops::create_timeline(
        &mut project,
        "Main",
        W as u32,
        H as u32,
        parse_fps("30").unwrap(),
    )
    .unwrap();
    ops::append_clip(&mut project, None, None, &a_red.id).unwrap();
    let pip_track = ops::add_track(&mut project, None, TrackKind::Video, "PiP").unwrap();
    let pip = ops::insert_clip(
        &mut project,
        None,
        Some(&pip_track),
        ops::ClipRequest {
            asset_id: &a_green.id,
            source_in: None,
            source_out: None,
            at: Some(Time::ZERO),
            name: None,
        },
    )
    .unwrap();
    let mut t = project.clip(&pip).unwrap().transform.clone();
    t.fit = Fit::Contain;
    t.scale = 0.25;
    ops::set_transform(&mut project, &pip, t).unwrap();
    // Slide from the left edge to the right edge over the whole clip.
    ops::add_keyframe(&mut project, &pip, "x", Time::ZERO, -240.0, "linear").unwrap();
    ops::add_keyframe(
        &mut project,
        &pip,
        "x",
        Time::from_secs_f64(4.0),
        240.0,
        "linear",
    )
    .unwrap();
    let out = root.join("pip.mp4");
    let mut req = RenderRequest::for_output(&out);
    req.width = Some(W as u32);
    req.height = Some(H as u32);
    req.crf = Some(14);
    let outcome = render::render(&project, &store, &req, &|_, _| {}, &|| false).expect("render");
    // Ignore the expected notice from hosts that list a hardware encoder they cannot use (CI runners).
    let unexpected: Vec<_> = outcome
        .warnings
        .iter()
        .filter(|w| {
            !w.contains("probe encode failed")
                && !w.contains("available memory could not be determined")
        })
        .collect();
    assert!(unexpected.is_empty(), "{:?}", unexpected);
    // t=0.2 the PiP is near x=80 (centre 320-240+...); the right side is untouched base.
    near(
        rgb_at(&out, 0.2, 560, 180),
        (255, 0, 0),
        12,
        "base red on the right while PiP is on the left",
    );
    near(
        rgb_at(&out, 0.2, 90, 180),
        (0, 128, 0),
        14,
        "pip on the left at the start",
    );
    near(
        rgb_at(&out, 3.8, 90, 180),
        (255, 0, 0),
        12,
        "base red on the left once PiP has moved right",
    );
    near(
        rgb_at(&out, 3.8, 550, 180),
        (0, 128, 0),
        14,
        "pip on the right at the end",
    );
}

/// Every frame of a render, downscaled to 16x9 gray, so a single black frame can be found.
fn all_frames_gray(video: &Path) -> Vec<Vec<u8>> {
    let out = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-i",
            video.to_str().unwrap(),
            "-vf",
            "scale=16:9",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "-",
        ])
        .output()
        .unwrap();
    out.stdout.chunks(16 * 9).map(|c| c.to_vec()).collect()
}

#[test]
fn no_black_flash_at_clip_ends_across_fps_conversion_static_or_animated() {
    if !have_ffmpeg() {
        return;
    }
    for animated in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let a = root.join("a.mp4");
        let b = root.join("b.mp4");
        // 30 fps sources with durations that do not land on 24 fps frame boundaries.
        for (path, color, d) in [(&a, "red", "5.54"), (&b, "green", "3.37")] {
            assert!(ff(&[
                "-f",
                "lavfi",
                "-i",
                &format!("color=c={color}:s=640x360:r=30:d={d}"),
                "-pix_fmt",
                "yuv420p",
                path.to_str().unwrap()
            ]));
        }
        let projects = root.join("p");
        std::fs::create_dir_all(&projects).unwrap();
        let (store, mut project) = ProjectStore::create(&projects, "blk").unwrap();
        let aa = store.import_asset(&mut project, &a, "t").unwrap();
        let ab = store.import_asset(&mut project, &b, "t").unwrap();
        ops::create_timeline(
            &mut project,
            "Main",
            W as u32,
            H as u32,
            parse_fps("24").unwrap(),
        )
        .unwrap();
        let c1 = ops::append_clip(&mut project, None, None, &aa.id).unwrap();
        // Like the real trailer: a seeked/trimmed source and a fade-in on the first clip.
        ops::trim_clip(
            &mut project,
            &c1,
            Some(Time::from_secs_f64(0.5)),
            Some(Time::from_secs_f64(5.5)),
        )
        .unwrap();
        ops::set_transition(
            &mut project,
            &c1,
            cinvar::project::model::TransitionSide::In,
            "fadeblack",
            Time::from_secs_f64(1.0),
        )
        .unwrap();
        ops::append_clip(&mut project, None, None, &ab.id).unwrap();
        if animated {
            ops::apply_animation_preset(&mut project, &c1, "push_in", 1.0, None).unwrap();
        }
        let out = root.join("blk.mp4");
        let mut req = RenderRequest::for_output(&out);
        req.width = Some(W as u32);
        req.height = Some(H as u32);
        render::render(&project, &store, &req, &|_, _| {}, &|| false).expect("render");
        let frames = all_frames_gray(&out);
        assert!(frames.len() > 200, "{} frames", frames.len());
        for (i, f) in frames.iter().enumerate() {
            if i < 20 {
                continue; // the fade-from-black at the head is intended
            }
            assert!(
                f.iter().any(|v| *v > 20),
                "animated={animated}: frame {i} (t={:.3}s) is black",
                i as f64 / 24.0
            );
        }
    }
}
