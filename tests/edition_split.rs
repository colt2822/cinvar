//! Free/Pro edition split, Phase 1: the Free export cap is enforced at every render boundary and
//! Pro can reach supported higher resolutions. Real FFmpeg renders, verified with ffprobe.

use std::path::Path;
use std::sync::Arc;

use cinvar::edition::{Enforcement, Entitlements};
use cinvar::error::ErrorCode;
use cinvar::project::model::{HwMode, TrackKind};
use cinvar::project::ops::{self, ClipRequest};
use cinvar::project::store::ProjectStore;
use cinvar::render::{self, RenderRequest};
use cinvar::util::proc;
use cinvar::util::time::{Rational, Time};

fn ffmpeg_ok() -> bool {
    proc::which("ffmpeg").is_some() && proc::which("ffprobe").is_some()
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Env vars are process-global, so every test that depends on CINVAR_CONFIG_DIR holds this lock.
/// With `Some(dir)` the config dir is `dir`; with `None` it is a fresh empty directory (no
/// license, no config), so a developer's real license can never make a test pass or fail.
fn config_guard(
    dir: Option<&Path>,
) -> (
    std::sync::MutexGuard<'static, ()>,
    Option<tempfile::TempDir>,
) {
    let g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = if dir.is_none() {
        Some(tempfile::tempdir().unwrap())
    } else {
        None
    };
    std::env::set_var(
        "CINVAR_CONFIG_DIR",
        dir.unwrap_or_else(|| tmp.as_ref().unwrap().path()),
    );
    (g, tmp)
}

fn build_project(dir: &Path, w: u32, h: u32) -> (ProjectStore, cinvar::project::model::Project) {
    build_project_src(dir, w, h, "1280x720")
}

fn build_project_src(
    dir: &Path,
    w: u32,
    h: u32,
    src_size: &str,
) -> (ProjectStore, cinvar::project::model::Project) {
    let src = dir.join("src.mp4");
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc2=size={src_size}:rate=24:duration=2"),
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&src)
        .status()
        .unwrap();
    assert!(status.success());
    let (store, mut project) = ProjectStore::create(dir, "edition").unwrap();
    let asset = store.import_asset(&mut project, &src, "edition").unwrap();
    let tl = ops::create_timeline(&mut project, "Main", w, h, Rational::new(24, 1)).unwrap();
    let track = project
        .timeline_by_id(&tl)
        .unwrap()
        .tracks_of(TrackKind::Video)[0]
        .id
        .clone();
    ops::insert_clip(
        &mut project,
        Some(&tl),
        Some(&track),
        ClipRequest {
            asset_id: &asset.id,
            source_in: Some(Time::ZERO),
            source_out: Some(Time::from_secs_f64(0.5)),
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

fn probe_size(path: &Path) -> (u32, u32) {
    let out = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let mut it = text.trim().split(',');
    (
        it.next().unwrap().parse().unwrap(),
        it.next().unwrap().parse().unwrap(),
    )
}

fn free() -> Entitlements {
    Entitlements::free(Enforcement::Enforce)
}

#[test]
fn free_cannot_plan_or_render_above_2k_by_any_route() {
    if !ffmpeg_ok() {
        eprintln!("SKIP: ffmpeg/ffprobe missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (store, project) = build_project(dir.path(), 1920, 1080);

    // named presets
    for preset in ["4k", "8k"] {
        let mut req = request(&store, "p.mp4", free());
        req.size_preset = Some(preset.into());
        let err = render::plan_render(&project, &store, &req).unwrap_err();
        assert_eq!(err.code, ErrorCode::PermissionDenied, "{preset}");
        assert!(err.message.contains("Free"), "{}", err.message);
    }
    // explicit size, either axis, either orientation, just over the line
    for (w, h) in [
        (3840, 2160),
        (2560, 1440),
        (2050, 1080),
        (1080, 2050),
        (1440, 2560),
        (3840, 1080),
    ] {
        let mut req = request(&store, "w.mp4", free());
        req.width = Some(w);
        req.height = Some(h);
        let err = render::plan_render(&project, &store, &req).unwrap_err();
        assert_eq!(err.code, ErrorCode::PermissionDenied, "{w}x{h}");
    }
    // width alone: height is derived from the timeline aspect ratio and still counted
    let mut req = request(&store, "wo.mp4", free());
    req.width = Some(3840);
    assert!(render::plan_render(&project, &store, &req).is_err());

    // a 4K timeline canvas is not a way around the cap: with no explicit size it is fitted
    // down with a warning, and asking for the canvas size explicitly is refused
    let dir4k = tempfile::tempdir().unwrap();
    let (store4k, project4k) = build_project(dir4k.path(), 3840, 2160);
    let req = request(&store4k, "canvas.mp4", free());
    let plan = render::plan_render(&project4k, &store4k, &req).unwrap();
    assert_eq!((plan.width, plan.height), (1920, 1080));
    assert!(plan.warnings.iter().any(|m| m.contains("exceeds")));
    let mut req = request(&store4k, "canvas2.mp4", free());
    req.width = Some(3840);
    assert_eq!(
        render::plan_render(&project4k, &store4k, &req)
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );

    // the full render() entry point refuses too, and leaves no file behind
    let mut req = request(&store, "full.mp4", free());
    req.size_preset = Some("4k".into());
    let res = render::render(&project, &store, &req, &|_, _| {}, &|| false);
    assert_eq!(res.unwrap_err().code, ErrorCode::PermissionDenied);
    assert!(!store.renders_dir().join("full.mp4").exists());
}

#[test]
fn free_can_export_up_to_the_cap() {
    if !ffmpeg_ok() {
        eprintln!("SKIP: ffmpeg/ffprobe missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (store, project) = build_project(dir.path(), 1920, 1080);
    for (w, h) in [(1280, 720), (1920, 1080), (2048, 1080), (1080, 2048)] {
        let name = format!("ok_{w}x{h}.mp4");
        let mut req = request(&store, &name, free());
        req.width = Some(w);
        req.height = Some(h);
        let outcome = render::render(&project, &store, &req, &|_, _| {}, &|| false)
            .unwrap_or_else(|e| panic!("{w}x{h}: {}", e.message));
        assert_eq!(probe_size(&outcome.output), (w, h));
    }
}

#[test]
fn pro_exports_4k_for_real() {
    if !ffmpeg_ok() {
        eprintln!("SKIP: ffmpeg/ffprobe missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (store, project) = build_project(dir.path(), 1920, 1080);
    let mut req = request(
        &store,
        "pro4k.mp4",
        Entitlements::pro(Enforcement::Enforce, "L-test", false),
    );
    req.size_preset = Some("4k".into());
    let outcome = render::render(&project, &store, &req, &|_, _| {}, &|| false).expect("pro 4k");
    assert_eq!(probe_size(&outcome.output), (3840, 2160));
}

#[test]
fn pro_plans_8k_and_still_hits_the_engine_limit() {
    if !ffmpeg_ok() {
        eprintln!("SKIP: ffmpeg/ffprobe missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (store, project) = build_project(dir.path(), 1920, 1080);
    let pro = || Entitlements::pro(Enforcement::Enforce, "L-test", false);
    let mut req = request(&store, "pro8k.mp4", pro());
    req.size_preset = Some("8k".into());
    render::plan_render(&project, &store, &req).expect("pro may plan 8k");
    // Pro is not "no limits": the engine's own bound still refuses, with its own error.
    let mut req = request(&store, "huge.mp4", pro());
    req.width = Some(20000);
    req.height = Some(2000);
    let err = render::plan_render(&project, &store, &req).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
}

#[test]
fn observe_mode_exists_only_in_test_builds_and_keeps_legacy_behaviour() {
    if !ffmpeg_ok() {
        eprintln!("SKIP: ffmpeg/ffprobe missing");
        return;
    }
    let _cfg = config_guard(None);
    assert!(
        !cinvar::config::Config::default().edition.enforce,
        "default must stay observe-only"
    );
    let dir = tempfile::tempdir().unwrap();
    let (store, project) = build_project(dir.path(), 1920, 1080);
    // No explicit entitlements: resolved from (empty) disk config => Free + observe.
    let mut req = RenderRequest::for_output(store.renders_dir().join("legacy.mp4"));
    req.size_preset = Some("8k".into());
    req.hardware = Some(HwMode::Off);
    render::plan_render(&project, &store, &req).expect("observe mode must not refuse 8k");
    // Explicit Free+Observe likewise.
    let mut req = request(&store, "o.mp4", Entitlements::free(Enforcement::Observe));
    req.size_preset = Some("4k".into());
    render::plan_render(&project, &store, &req).expect("observe never refuses");
}

#[test]
fn request_without_entitlements_is_still_gated_when_enforcement_is_on() {
    if !ffmpeg_ok() {
        eprintln!("SKIP: ffmpeg/ffprobe missing");
        return;
    }
    // A caller that forgets to attach entitlements falls back to this machine's config+license.
    let cfg_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        cfg_dir.path().join("config.toml"),
        "[edition]\nenforce = true\n",
    )
    .unwrap();
    let _cfg = config_guard(Some(cfg_dir.path()));
    let dir = tempfile::tempdir().unwrap();
    let (store, project) = build_project(dir.path(), 1920, 1080);
    let mut req = RenderRequest::for_output(store.renders_dir().join("fallback.mp4"));
    req.size_preset = Some("4k".into());
    let res = render::plan_render(&project, &store, &req);
    assert_eq!(res.unwrap_err().code, ErrorCode::PermissionDenied);
}

#[test]
fn entitlements_cannot_be_smuggled_in_a_serialized_request() {
    let json = serde_json::json!({
        "output": "/tmp/x.mp4", "timeline_id": null, "width": null, "height": null, "fps": null,
        "crf": null, "codec": null, "format": null, "hardware": null, "range": null,
        "preview": false, "burn_captions": true,
        "entitlements": {"edition": "pro", "enforcement": "observe"}
    });
    let req: RenderRequest = serde_json::from_value(json).unwrap();
    assert!(req.entitlements.is_none());
    let back = serde_json::to_value(&req).unwrap();
    assert!(back.get("entitlements").is_none());
}

fn cli(cfg: &Path, project: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_cinvar"))
        .env("CINVAR_CONFIG_DIR", cfg)
        .arg("--project")
        .arg(project)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn command_line_render_respects_the_ceiling_when_enforced() {
    if !ffmpeg_ok() {
        eprintln!("SKIP: ffmpeg/ffprobe missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (store, _project) = build_project(dir.path(), 1920, 1080);
    let proj_dir = dir.path().join("edition");
    let cfg = tempfile::tempdir().unwrap();
    std::fs::write(
        cfg.path().join("config.toml"),
        "[edition]\nenforce = true\n",
    )
    .unwrap();

    let big = store.renders_dir().join("cli4k.mp4");
    let out = cli(
        cfg.path(),
        &proj_dir,
        &[
            "render",
            "--output",
            big.to_str().unwrap(),
            "--width",
            "3840",
            "--height",
            "2160",
            "--wait",
        ],
    );
    assert!(!out.status.success(), "4k must be refused");
    let text =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Free"), "{text}");
    assert!(!big.exists());

    let ok = store.renders_dir().join("cli1080.mp4");
    let out = cli(
        cfg.path(),
        &proj_dir,
        &[
            "render",
            "--output",
            ok.to_str().unwrap(),
            "--width",
            "1920",
            "--height",
            "1080",
            "--wait",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(probe_size(&ok), (1920, 1080));

    // same command, enforcement off (the default): legacy behaviour
    let cfg_off = tempfile::tempdir().unwrap();
    let legacy = store.renders_dir().join("cli_legacy4k.mp4");
    let out = cli(
        cfg_off.path(),
        &proj_dir,
        &[
            "render",
            "--output",
            legacy.to_str().unwrap(),
            "--width",
            "3840",
            "--height",
            "2160",
            "--wait",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn edition_status_reports_free_and_observe_by_default() {
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    let out = cli(cfg.path(), proj.path(), &["--json", "edition", "status"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["edition"], "free");
    assert_eq!(v["enforcement"], "observe");
    assert_eq!(v["export_limit"], "2048x1080");
    assert_eq!(v["features"], serde_json::json!(["export.2k"]));
    // a license file that does not verify never upgrades the edition, and says why
    std::fs::write(
        cfg.path().join("license.json"),
        "{\"key_id\":\"k1\",\"payload\":\"e30=\",\"signature\":\"AA==\"}",
    )
    .unwrap();
    let out = cli(cfg.path(), proj.path(), &["--json", "edition", "status"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["edition"], "free");
    assert_eq!(v["source"]["kind"], "rejected");
}

#[test]
fn cloud_render_tool_is_pro_only_when_enforced_and_other_tools_are_untouched() {
    let _cfg = config_guard(None);
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
    let err = registry
        .call("cinvar.render.cloud", &mut ctx, serde_json::json!({}))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::PermissionDenied);
    assert!(err.message.contains("render.cloud"), "{}", err.message);
    registry
        .call("cinvar.project.status", &mut ctx, serde_json::json!({}))
        .expect("ungated tool works");
    // blender: a 4K canvas is refused before Blender is even looked for
    let err = registry
        .call(
            "cinvar.blender.animation",
            &mut ctx,
            serde_json::json!({"width": 3840, "height": 2160}),
        )
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::PermissionDenied, "{}", err.message);
}

#[test]
fn free_edits_and_exports_a_4k_source_on_a_2k_timeline() {
    if !ffmpeg_ok() {
        eprintln!("SKIP: ffmpeg/ffprobe missing");
        return;
    }
    // "Project import with higher-resolution media": the restriction is on OUTPUT only.
    let dir = tempfile::tempdir().unwrap();
    let (store, project) = build_project_src(dir.path(), 1920, 1080, "3840x2160");
    let asset = &project.assets[0];
    let info = asset.info.as_ref().expect("probed");
    assert_eq!(
        (info.width, info.height),
        (3840, 2160),
        "4K source is imported intact"
    );
    let outcome = render::render(
        &project,
        &store,
        &request(&store, "from4k.mp4", free()),
        &|_, _| {},
        &|| false,
    )
    .expect("Free exports 1080p from a 4K source");
    assert_eq!(probe_size(&outcome.output), (1920, 1080));
}

#[test]
fn pro_4k_project_opened_in_free_exports_fitted_with_a_warning_but_explicit_4k_is_refused() {
    if !ffmpeg_ok() {
        eprintln!("SKIP: ffmpeg/ffprobe missing");
        return;
    }
    // Projects transfer between editions: a 4K-canvas timeline built under Pro, exported from Free
    // with no explicit size (what the web UI button does), is fitted to the Free tier and says so.
    let dir = tempfile::tempdir().unwrap();
    let (store, project) = build_project(dir.path(), 3840, 2160);
    let outcome = render::render(
        &project,
        &store,
        &request(&store, "fit.mp4", free()),
        &|_, _| {},
        &|| false,
    )
    .expect("implicit export is fitted");
    let (w, h) = probe_size(&outcome.output);
    assert_eq!(
        (w, h),
        (1920, 1080),
        "16:9 aspect preserved, inside the Free tier"
    );
    assert!(
        outcome
            .warnings
            .iter()
            .any(|m| m.contains("exceeds") && m.contains("1920x1080")),
        "{:?}",
        outcome.warnings
    );
    // 9:16 4K canvas fits to 1080x1920 (portrait), not a landscape box
    let d2 = tempfile::tempdir().unwrap();
    let (s2, p2) = build_project(d2.path(), 2160, 3840);
    let plan = render::plan_render(&p2, &s2, &request(&s2, "v.mp4", free())).unwrap();
    assert_eq!((plan.width, plan.height), (1080, 1920));
    // explicit above-limit requests are still refused, never fitted
    let mut req = request(&store, "explicit.mp4", free());
    req.width = Some(3840);
    req.height = Some(2160);
    assert_eq!(
        render::plan_render(&project, &store, &req)
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    // Pro is untouched: native 4K, no warning
    let pro = request(
        &store,
        "pro.mp4",
        Entitlements::pro(Enforcement::Enforce, "L", false),
    );
    let plan = render::plan_render(&project, &store, &pro).unwrap();
    assert_eq!((plan.width, plan.height), (3840, 2160));
    assert!(plan.warnings.iter().all(|m| !m.contains("exceeds")));
}
#[test]
fn pro_codecs_are_gated_at_the_render_boundary() {
    if !ffmpeg_ok() {
        eprintln!("SKIP: ffmpeg/ffprobe missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (store, project) = build_project(dir.path(), 1280, 720);
    // Every Pro codec is refused for Free unless it is a preview run.
    for codec in ["prores", "dnxhr", "av1"] {
        let mut req = request(&store, "c.mp4", free());
        req.codec = Some(codec.into());
        let err = render::plan_render(&project, &store, &req).unwrap_err();
        assert_eq!(err.code, ErrorCode::PermissionDenied, "{codec}");
        assert!(err.message.contains("export.pro_codecs"), "{codec}: {}", err.message);
        // The refusal points at the preview escape hatch rather than leaving the user stuck.
        assert!(err.to_string().contains("--pro-preview"), "{codec}: {err}");
    }
    // Pro plans the real encoders (no render: the gate is what this test is about).
    let pro_ent = || Entitlements::pro(Enforcement::Enforce, "L-test", false);
    for (codec, encoder) in [("prores", "prores_ks"), ("dnxhr", "dnxhd"), ("av1", "libsvtav1")] {
        let mut req = request(&store, "p.mov", pro_ent());
        req.codec = Some(codec.into());
        let plan = render::plan_render(&project, &store, &req).expect("pro codec");
        assert_eq!(plan.encoder, encoder, "{codec}");
    }
    // The free codecs stay free.
    for codec in ["h264", "hevc", "vp9"] {
        let mut req = request(&store, "f.webm", free());
        req.codec = Some(codec.into());
        render::plan_render(&project, &store, &req).unwrap_or_else(|e| panic!("{codec}: {}", e.message));
    }
}

#[test]
fn tool_table_gates_every_advanced_entry_point_and_leaves_inspection_free() {
    use cinvar::edition::{tool_feature, Feature};
    for (tool, feature) in [
        ("cinvar.color.apply_transform", Feature::ColorAdvanced),
        ("cinvar.color.import_lut", Feature::ColorAdvanced),
        ("cinvar.otio.export", Feature::OtioExport),
        ("cinvar.render.cloud", Feature::CloudRender),
        ("cinvar.blender.animation", Feature::BlenderProduction),
        ("cinvar.blender.mockup_screen", Feature::BlenderMockup),
        ("cinvar.captions.word_level", Feature::CaptionsWordLevel),
        ("cinvar.enhance.upscale", Feature::EnhanceUpscale),
        ("cinvar.audio.stems", Feature::AudioStems),
        ("cinvar.batch.folders", Feature::BatchMultiFolder),
    ] {
        assert_eq!(tool_feature(tool), Some(feature), "{tool}");
    }
    // Read-only inspection, timeline editing and every free delivery path stay ungated.
    for tool in [
        "cinvar.color.inspect",
        "cinvar.project.status",
        "cinvar.timeline.trim",
        "cinvar.timeline.split",
        "cinvar.captions.export",
        "cinvar.captions.import",
        "cinvar.qa.render_check",
        "cinvar.render.export",
        "cinvar.render.preview",
        "cinvar.scenes.search",
    ] {
        assert_eq!(tool_feature(tool), None, "{tool} must stay free");
    }
}

#[test]
fn the_free_semantic_cap_is_enforced_at_the_index_entry_point() {
    use cinvar::edition::SEMANTIC_FREE_CLIP_LIMIT;
    let cfg = tempfile::tempdir().unwrap();
    std::fs::write(
        cfg.path().join("config.toml"),
        "[edition]\nenforce = true\n",
    )
    .unwrap();
    let work = tempfile::tempdir().unwrap();
    let footage = work.path().join("footage");
    std::fs::create_dir_all(&footage).unwrap();
    for i in 0..(SEMANTIC_FREE_CLIP_LIMIT + 2) {
        std::fs::write(footage.join(format!("clip{i:02}.mp4")), b"placeholder").unwrap();
    }
    assert_eq!(
        cinvar::tools::count_indexable_clips(&footage),
        SEMANTIC_FREE_CLIP_LIMIT + 2
    );
    // The CLI refuses before the index is built (so no partial index is ever written).
    let out = cli(
        cfg.path(),
        work.path(),
        &["analyze", "--media-root", &footage.to_string_lossy()],
    );
    assert_eq!(out.status.code(), Some(3), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(text.contains("10 clips"), "{text}");
    assert!(text.contains("cinvar activate"), "{text}");
}
