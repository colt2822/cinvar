//! cinvar.render.export must honor or reject every argument, never silently drop one.

use cinvar::project::store::ProjectStore;
use serde_json::json;

fn call(args: serde_json::Value) -> cinvar::error::Result<serde_json::Value> {
    let dir = tempfile::tempdir().unwrap();
    let (store, mut project) = ProjectStore::create(dir.path(), "strict").unwrap();
    let registry = cinvar::tools::build_default();
    let jobs = cinvar::jobs::Jobs::new();
    let mut history = cinvar::project::history::History::default();
    let config = cinvar::config::Config::default();
    let mut ctx = cinvar::tools::ToolCtx {
        project: &mut project,
        store: &store,
        config: &config,
        jobs: &jobs,
        history: &mut history,
        dry_run: false,
    };
    registry.call("cinvar.render.export", &mut ctx, args)
}

#[test]
fn unknown_argument_is_rejected() {
    let err = call(json!({"hardwre": "off"})).unwrap_err().to_string();
    assert!(err.contains("unknown argument"), "{err}");
}

#[test]
fn invalid_hardware_and_bit_depth_are_rejected() {
    assert!(call(json!({"hardware": "maybe"})).unwrap_err().to_string().contains("hardware"));
    assert!(call(json!({"bit_depth": 12})).unwrap_err().to_string().contains("bit_depth"));
}

#[test]
fn declared_arguments_are_in_the_schema() {
    let registry = cinvar::tools::build_default();
    let schemas = registry.to_schemas();
    let t = schemas.iter().find(|s| s.name == "cinvar.render.export").unwrap();
    for k in ["hardware", "size_preset", "bit_depth", "video_bitrate"] {
        assert!(t.input_schema["properties"].get(k).is_some(), "{k} missing from schema");
    }
}
