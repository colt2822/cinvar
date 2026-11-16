//! Pro preview: a Free edition may run a Pro-gated operation for real, but only for the first
//! 10 seconds and with the CINVAR PRO PREVIEW notice burned into the file.
//!
//! Everything here produces and inspects real files: ffprobe for size/codec/duration, and
//! signalstats on the bottom band of a black source for the overlay (black in, so any light pixel
//! there was drawn by drawtext). A licensed run must be untouched by the flag.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cinvar::edition::{Enforcement, Entitlements};
use cinvar::error::ErrorCode;
use cinvar::project::model::{HwMode, TrackKind};
use cinvar::project::ops::{self, ClipRequest};
use cinvar::project::store::ProjectStore;
use cinvar::render::{self, RenderRequest};
use cinvar::util::proc;
use cinvar::util::time::{Rational, Time};

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn ffmpeg_ok() -> bool {
    proc::which("ffmpeg").is_some() && proc::which("ffprobe").is_some()
}

/// A config dir that enforces the edition limits. Test builds default to observe-only (the
/// edition-observe feature), so every refusal must be requested explicitly.
fn enforcing_config() -> (std::sync::MutexGuard<'static, ()>, tempfile::TempDir) {
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "[edition]\nenforce = true\n").unwrap();
    std::env::set_var("CINVAR_CONFIG_DIR", dir.path());
    (guard, dir)
}

/// A flat black source: the only light pixels in a rendered frame come from burned-in text.
fn black_source(dir: &Path, secs: f64) -> PathBuf {
    let src = dir.join("black.mp4");
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-y", "-v", "error", "-f", "lavfi", "-i",
            &format!("color=c=black:s=640x360:r=24:d={secs}"),
            "-c:v", "libx264", "-pix_fmt", "yuv420p",
        ])
        .arg(&src)
        .status()
        .unwrap();
    assert!(status.success());
    src
}

fn build_project(
    dir: &Path,
    canvas: (u32, u32),
    secs: f64,
) -> (ProjectStore, cinvar::project::model::Project) {
    let src = black_source(dir, secs);
    let (store, mut project) = ProjectStore::create(dir, "preview").unwrap();
    let asset = store.import_asset(&mut project, &src, "preview").unwrap();
    let tl = ops::create_timeline(&mut project, "Main", canvas.0, canvas.1, Rational::new(24, 1))
        .unwrap();
    let track = project.timeline_by_id(&tl).unwrap().tracks_of(TrackKind::Video)[0]
        .id
        .clone();
    ops::insert_clip(
        &mut project,
        Some(&tl),
        Some(&track),
        ClipRequest {
            asset_id: &asset.id,
            source_in: Some(Time::ZERO),
            source_out: Some(Time::from_secs_f64(secs)),
            at: Some(Time::ZERO),
            name: None,
        },
    )
    .unwrap();
    store.save(&mut project).unwrap();
    (store, project)
}

fn request(store: &ProjectStore, name: &str, ent: Entitlements) -> RenderRequest {
    let mut req = RenderRequest::for_output(store.renders_dir().join(name));
    req.hardware = Some(HwMode::Off);
    req.entitlements = Some(Arc::new(ent));
    req
}

fn free() -> Entitlements {
    Entitlements::free(Enforcement::Enforce)
}

fn pro() -> Entitlements {
    Entitlements::pro(Enforcement::Enforce, "L-preview-test", false)
}

struct Media {
    width: u32,
    height: u32,
    codec: String,
    duration: f64,
}

fn probe(path: &Path) -> Media {
    let out = std::process::Command::new("ffprobe")
        .args([
            "-v", "error", "-select_streams", "v:0",
            "-show_entries", "stream=width,height,codec_name",
            "-show_entries", "format=duration", "-of", "json",
        ])
        .arg(path)
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    Media {
        width: v["streams"][0]["width"].as_u64().unwrap() as u32,
        height: v["streams"][0]["height"].as_u64().unwrap() as u32,
        codec: v["streams"][0]["codec_name"].as_str().unwrap_or("").to_string(),
        duration: v["format"]["duration"].as_str().unwrap_or("0").parse().unwrap(),
    }
}

/// Mean luma of the bottom 12 percent of the first frame, where the preview notice is drawn. A
/// black source without the notice measures 0.0; with it the band carries glyph pixels.
fn bottom_band_luma(path: &Path) -> f64 {
    let out = std::process::Command::new("ffmpeg")
        .args([
            "-v", "error", "-ss", "0", "-i", &path.to_string_lossy(),
            "-vf", "crop=iw:ih*0.12:0:ih*0.84,format=gray,signalstats,metadata=print:file=-",
            "-frames:v", "1", "-f", "null", "-",
        ])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    text.lines()
        .find_map(|l| l.trim().strip_prefix("lavfi.signalstats.YAVG="))
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or_else(|| panic!("no signalstats output for {}", path.display()))
}

fn warnings_of(report: &render::RenderOutcome) -> String {
    report.warnings.join(" | ")
}

#[test]
fn free_is_refused_and_pro_preview_renders_4k_short_and_watermarked() {
    if !ffmpeg_ok() {
        eprintln!("SKIP: ffmpeg/ffprobe missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (store, project) = build_project(dir.path(), (1920, 1080), 12.0);

    // 1. Free, no flag: the existing refusal, and no file left behind.
    let mut req = request(&store, "free4k.mp4", free());
    req.width = Some(3840);
    req.height = Some(2160);
    let err = render::plan_render(&project, &store, &req).unwrap_err();
    assert_eq!(err.code, ErrorCode::PermissionDenied);
    assert!(err.message.contains("2048x1080"), "{}", err.message);
    let res = render::render(&project, &store, &req, &|_, _| {}, &|| false);
    assert_eq!(res.unwrap_err().code, ErrorCode::PermissionDenied);
    assert!(!store.renders_dir().join("free4k.mp4").exists());

    // 2. Same Free edition with the flag: a real 4K file, cut to the first 10 s, watermarked.
    let mut req = request(&store, "preview4k.mp4", free());
    req.width = Some(3840);
    req.height = Some(2160);
    req.pro_preview = true;
    let plan = render::plan_render(&project, &store, &req).expect("preview may plan 4K");
    assert_eq!((plan.width, plan.height), (3840, 2160));
    assert!(
        plan.warnings.iter().any(|w| w.contains("PRO PREVIEW")),
        "{:?}",
        plan.warnings
    );
    let outcome =
        render::render(&project, &store, &req, &|_, _| {}, &|| false).expect("preview render");
    let media = probe(&outcome.output);
    assert_eq!((media.width, media.height), (3840, 2160), "the real 4K size");
    assert!(
        media.duration <= 10.5 && media.duration >= 9.0,
        "preview is cut to 10 s: {}",
        media.duration
    );
    let luma = bottom_band_luma(&outcome.output);
    assert!(luma > 1.0, "the preview notice must be burned in (band luma {luma})");
    assert!(warnings_of(&outcome).contains("PRO PREVIEW"), "{}", warnings_of(&outcome));
    assert!(outcome.bytes > 0 && outcome.frames > 0);
}

#[test]
fn a_licensed_run_ignores_the_flag_entirely() {
    if !ffmpeg_ok() {
        eprintln!("SKIP: ffmpeg/ffprobe missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (store, project) = build_project(dir.path(), (1920, 1080), 12.0);
    let mut req = request(&store, "pro4k_flag.mp4", pro());
    req.width = Some(3840);
    req.height = Some(2160);
    req.pro_preview = true;
    let plan = render::plan_render(&project, &store, &req).unwrap();
    assert_eq!((plan.width, plan.height), (3840, 2160));
    assert!(
        plan.warnings.iter().all(|w| !w.contains("PRO PREVIEW")),
        "{:?}",
        plan.warnings
    );
    let outcome = render::render(&project, &store, &req, &|_, _| {}, &|| false).unwrap();
    let media = probe(&outcome.output);
    assert_eq!((media.width, media.height), (3840, 2160));
    assert!(
        media.duration > 11.0,
        "a licensed run is the full 12 s timeline, not a preview: {}",
        media.duration
    );
    assert_eq!(bottom_band_luma(&outcome.output), 0.0, "no watermark for a licensed run");
}

#[test]
fn prores_codec_is_pro_only_and_previews_as_real_prores() {
    if !ffmpeg_ok() {
        eprintln!("SKIP: ffmpeg/ffprobe missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (store, project) = build_project(dir.path(), (640, 360), 12.0);

    // Free cannot choose ProRes (or DNxHR / AV1) at all.
    let mut req = request(&store, "free_prores.mov", free());
    req.codec = Some("prores".into());
    let err = render::plan_render(&project, &store, &req).unwrap_err();
    assert_eq!(err.code, ErrorCode::PermissionDenied);
    assert!(err.message.contains("export.pro_codecs"), "{}", err.message);
    for codec in ["dnxhr", "av1"] {
        let mut req = request(&store, "free_other.mp4", free());
        req.codec = Some(codec.into());
        assert_eq!(
            render::plan_render(&project, &store, &req).unwrap_err().code,
            ErrorCode::PermissionDenied,
            "{codec}"
        );
    }

    // With the flag it encodes real ProRes, shortened and watermarked.
    let mut req = request(&store, "preview_prores.mov", free());
    req.codec = Some("prores".into());
    req.pro_preview = true;
    let outcome = render::render(&project, &store, &req, &|_, _| {}, &|| false)
        .unwrap_or_else(|e| panic!("preview prores: {}", e.message));
    let media = probe(&outcome.output);
    assert_eq!(media.codec, "prores", "the encoder really ran");
    assert_eq!((media.width, media.height), (640, 360));
    assert!(media.duration <= 10.5, "{}", media.duration);
    assert!(bottom_band_luma(&outcome.output) > 1.0);

    // Pro encodes the same codec at full length without a watermark.
    let mut req = request(&store, "pro_prores.mov", pro());
    req.codec = Some("prores".into());
    let outcome = render::render(&project, &store, &req, &|_, _| {}, &|| false).expect("pro prores");
    let media = probe(&outcome.output);
    assert_eq!(media.codec, "prores");
    assert!(media.duration > 11.0, "{}", media.duration);
    assert_eq!(bottom_band_luma(&outcome.output), 0.0);
}

fn cli(cfg: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_cinvar"))
        .env("CINVAR_CONFIG_DIR", cfg)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn the_command_line_applies_the_flag_globally() {
    if !ffmpeg_ok() {
        eprintln!("SKIP: ffmpeg/ffprobe missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (store, _project) = build_project(dir.path(), (1920, 1080), 12.0);
    let cfg = tempfile::tempdir().unwrap();
    std::fs::write(cfg.path().join("config.toml"), "[edition]\nenforce = true\n").unwrap();
    let proj = dir.path().join("preview");

    // Without the flag: the existing refusal, exit code 3 (PermissionDenied).
    let big = store.renders_dir().join("cli4k.mp4");
    let out = cli(
        cfg.path(),
        &[
            "--project", &proj.to_string_lossy(),
            "render", "--output", &big.to_string_lossy(),
            "--width", "3840", "--height", "2160", "--wait",
        ],
    );
    assert_eq!(out.status.code(), Some(3), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("2048x1080"));
    assert!(!big.exists());

    // With the global flag: a real 4K file, at most 10 s, watermarked.
    let pv = store.renders_dir().join("cli4k_preview.mp4");
    let out = cli(
        cfg.path(),
        &[
            "--pro-preview",
            "--project", &proj.to_string_lossy(),
            "render", "--output", &pv.to_string_lossy(),
            "--width", "3840", "--height", "2160", "--wait", "--json",
        ],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let media = probe(&pv);
    assert_eq!((media.width, media.height), (3840, 2160));
    assert!(media.duration <= 10.5, "{}", media.duration);
    assert!(bottom_band_luma(&pv) > 1.0);
}

/// A stand-in for the Real-ESRGAN helper: it scales the extracted frames with ffmpeg and prints
/// the JSON the real helper prints. Real-ESRGAN itself is not installed on this machine, so this
/// proves the preview plumbing (gate order, the 10 s cut, the burn-in), not model quality.
const FAKE_ESRGAN: &str = r#"#!/usr/bin/env python3
import json, os, subprocess, sys
args = sys.argv[1:]
if args and args[0] == "-I":
    args = args[1:]
_helper, frames_dir, out_dir, scale, gpuid, tile = args[:6]
os.makedirs(out_dir, exist_ok=True)
frames = sorted(f for f in os.listdir(frames_dir) if f.endswith(".png"))
subprocess.run(["ffmpeg", "-v", "error", "-y", "-framerate", "24",
                "-i", os.path.join(frames_dir, "%09d.png"),
                "-vf", "scale=iw*%s:ih*%s" % (scale, scale),
                "-start_number", "1", os.path.join(out_dir, "%09d.png")], check=True)
print(json.dumps({"ok": True, "count": len(frames), "gpuid": int(gpuid),
                  "scale": int(scale), "seconds": 1.0}))
"#;

fn fake_esrgan(dir: &Path) -> PathBuf {
    let path = dir.join("esrgan_stub.py");
    std::fs::write(&path, FAKE_ESRGAN).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

#[test]
fn enhance_is_gated_and_previews_the_real_upscale_path() {
    if !ffmpeg_ok() {
        eprintln!("SKIP: ffmpeg/ffprobe missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = black_source(dir.path(), 12.0);
    let cfg = tempfile::tempdir().unwrap();
    std::fs::write(cfg.path().join("config.toml"), "[edition]\nenforce = true\n").unwrap();
    let stub = fake_esrgan(dir.path());

    let run = |flag: bool, out: &Path| {
        let mut args: Vec<String> = Vec::new();
        if flag {
            args.push("--pro-preview".into());
        }
        args.extend([
            "enhance".to_string(),
            src.to_string_lossy().to_string(),
            "--upscale".to_string(),
            "2".to_string(),
            out.to_string_lossy().to_string(),
        ]);
        let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        std::process::Command::new(env!("CARGO_BIN_EXE_cinvar"))
            .env("CINVAR_CONFIG_DIR", cfg.path())
            .env("CINVAR_ESRGAN_PYTHON", &stub)
            .args(&argv)
            .output()
            .unwrap()
    };

    let refused = run(false, &dir.path().join("plain.mp4"));
    assert_eq!(
        refused.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    let stderr = String::from_utf8_lossy(&refused.stderr).to_string();
    assert!(stderr.contains("enhance.upscale"), "{stderr}");
    assert!(!dir.path().join("plain.mp4").exists());

    // The edition decides before the environment does: without any upscaler configured the
    // refusal is still the gate, not a missing-dependency error.
    let bare = std::process::Command::new(env!("CARGO_BIN_EXE_cinvar"))
        .env("CINVAR_CONFIG_DIR", cfg.path())
        .env_remove("CINVAR_ESRGAN_PYTHON")
        .args([
            "enhance",
            &src.to_string_lossy(),
            "--upscale",
            "2",
            &dir.path().join("bare.mp4").to_string_lossy(),
        ])
        .output()
        .unwrap();
    assert_eq!(bare.status.code(), Some(3), "{}", String::from_utf8_lossy(&bare.stderr));
    assert!(String::from_utf8_lossy(&bare.stderr).contains("enhance.upscale"));

    let out_path = dir.path().join("preview_2x.mp4");
    let ok = run(true, &out_path);
    assert!(ok.status.success(), "{}", String::from_utf8_lossy(&ok.stderr));
    let media = probe(&out_path);
    assert_eq!((media.width, media.height), (1280, 720), "real 2x upscale");
    assert!(
        media.duration <= 10.5,
        "the preview only upscales the first 10 s: {}",
        media.duration
    );
    assert!(bottom_band_luma(&out_path) > 1.0, "the upscaled preview carries the notice");
}

#[test]
fn gated_mcp_tools_are_refused_through_the_tool_context() {
    let _cfg = enforcing_config();
    let dir = tempfile::tempdir().unwrap();
    let (store, mut project) = ProjectStore::create(dir.path(), "tools").unwrap();
    let registry = cinvar::tools::build_default();
    let jobs = cinvar::jobs::Jobs::new();
    let mut history = cinvar::project::history::History::default();
    let mut config = cinvar::config::Config::default();
    config.edition.enforce = true;
    let mut ctx = cinvar::tools::ToolCtx {
        project: &mut project,
        store: &store,
        config: &config,
        jobs: &jobs,
        history: &mut history,
        dry_run: false,
    };

    // Advanced colour grading is Pro; read-only inspection is not.
    let err = registry
        .call(
            "cinvar.color.apply_transform",
            &mut ctx,
            serde_json::json!({"clip_id": "x", "source": "sRGB", "destination": "Rec.709"}),
        )
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::PermissionDenied);
    assert!(err.message.contains("color.advanced"), "{}", err.message);

    // OTIO export is Pro, and says how to unlock it.
    let err = registry
        .call(
            "cinvar.otio.export",
            &mut ctx,
            serde_json::json!({"output": "exports/cut.otio"}),
        )
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::PermissionDenied);
    assert!(err.message.contains("export.otio"), "{}", err.message);

    // Semantic indexing: refused before the index runs, naming the Free allowance.
    let footage = dir.path().join("footage");
    std::fs::create_dir_all(&footage).unwrap();
    for i in 0..(cinvar::edition::SEMANTIC_FREE_CLIP_LIMIT + 1) {
        std::fs::write(footage.join(format!("clip{i:02}.mp4")), b"not really a video").unwrap();
    }
    assert_eq!(
        cinvar::tools::count_indexable_clips(&footage),
        cinvar::edition::SEMANTIC_FREE_CLIP_LIMIT + 1
    );
    let err = registry
        .call(
            "cinvar.scenes.index",
            &mut ctx,
            serde_json::json!({"media_root": footage.to_string_lossy()}),
        )
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::PermissionDenied);
    assert!(err.message.contains("10 clips"), "{}", err.message);
    assert!(err.to_string().contains("cinvar activate"), "{}", err);

    // The boundary itself: 10 is allowed, 11 is not.
    let ent = cinvar::edition::resolve(&config);
    assert!(ent
        .check_semantic_clips(cinvar::edition::SEMANTIC_FREE_CLIP_LIMIT)
        .is_ok());
    assert_eq!(
        ent.check_semantic_clips(cinvar::edition::SEMANTIC_FREE_CLIP_LIMIT + 1)
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    // Pro indexes without a cap.
    assert!(Entitlements::pro(Enforcement::Enforce, "L", false)
        .check_semantic_clips(10_000)
        .is_ok());

    // A free tool still works in the same context.
    registry
        .call("cinvar.project.status", &mut ctx, serde_json::json!({}))
        .expect("ungated tool works");
}
