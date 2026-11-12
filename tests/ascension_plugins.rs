//! Plugin registry: discovery, compatibility, dependency detection, enable/disable, pinning,
//! invalid-plugin reporting, permission enforcement. All against real files on disk.

use cinvar::plugins::registry::{self, Health, Kind, PluginState};
use serde_json::json;

fn write_plugin(dir: &std::path::Path, folder: &str, manifest: serde_json::Value) {
    let root = dir.join(folder);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("plugin.json"), manifest.to_string()).unwrap();
}

fn base(name: &str, version: &str) -> serde_json::Value {
    json!({"name": name, "version": version, "description": "t", "cinvar_min": "0.1.0",
           "tools": [{"name": "echo", "description": "e", "input": {"type": "object", "properties": {}}, "command": ["true"]}]})
}

#[test]
fn builtins_are_listed_with_disclosures_and_work_without_them() {
    let dir = tempfile::tempdir().unwrap();
    let inv = registry::inventory(dir.path(), &PluginState::default(), false);
    let get = |id: &str| {
        inv.iter()
            .find(|p| p.id == id)
            .unwrap_or_else(|| panic!("missing {id}"))
    };
    for id in [
        "cinvar.blender",
        "cinvar.runpod",
        "cinvar.comfyui",
        "cinvar.otio",
        "cinvar.transcribe",
        "cinvar.scenedetect",
        "cinvar.cloud-storage",
    ] {
        assert_eq!(get(id).kind, Kind::Builtin);
    }
    let runpod = get("cinvar.runpod");
    assert!(runpod.costs_money && runpod.uses_cloud && runpod.sends_files_off_device);
    assert!(!get("cinvar.blender").costs_money);
    assert_eq!(
        get("cinvar.cloud-storage").health,
        Health::ContractOnly,
        "unimplemented adapters must say so"
    );
    assert_eq!(get("cinvar.otio").health, Health::Ok);
}

#[test]
fn external_plugins_start_disabled_and_report_problems_without_failing_the_scan() {
    let dir = tempfile::tempdir().unwrap();
    write_plugin(dir.path(), "good", base("good", "1.0.0"));
    let mut future = base("future", "1.0.0");
    future["cinvar_min"] = json!("99.0.0");
    write_plugin(dir.path(), "future", future);
    let mut capped = base("capped", "1.0.0");
    capped["cinvar_max"] = json!("0.0.1");
    write_plugin(dir.path(), "capped", capped);
    let mut wrong_os = base("wrongos", "1.0.0");
    wrong_os["platforms"] = json!(["plan9"]);
    write_plugin(dir.path(), "wrongos", wrong_os);
    let mut dep = base("needsdep", "1.0.0");
    dep["requires"] = json!(["definitely-not-a-real-binary-xyz"]);
    write_plugin(dir.path(), "needsdep", dep);
    let mut collide = base("collide", "1.0.0");
    collide["id"] = json!("cinvar.blender");
    write_plugin(dir.path(), "collide", collide);
    std::fs::create_dir_all(dir.path().join("broken")).unwrap();
    std::fs::write(dir.path().join("broken/plugin.json"), "{ not json").unwrap();

    let inv = registry::inventory(dir.path(), &PluginState::default(), false);
    let ext = |n: &str| {
        inv.iter()
            .find(|p| p.id == n || p.name == n)
            .unwrap_or_else(|| panic!("missing {n}"))
            .clone()
    };
    assert!(
        ext("good").compatible && !ext("good").enabled,
        "external plugins must be opt-in"
    );
    assert!(!ext("future").compatible && ext("future").problems[0].contains("99.0.0"));
    assert!(!ext("capped").compatible);
    assert!(!ext("wrongos").compatible);
    assert_eq!(
        ext("needsdep").missing_dependencies,
        vec!["definitely-not-a-real-binary-xyz"]
    );
    assert!(inv
        .iter()
        .any(|p| p.kind == Kind::External && p.problems.iter().any(|m| m.contains("collides"))));
    let broken = ext("broken");
    assert_eq!(broken.health, Health::Invalid);
    assert!(broken.health_detail.contains("invalid plugin.json"));
}

#[test]
fn enable_disable_and_pin_persist_and_gate_registration() {
    let dir = tempfile::tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    let plugins = dir.path().join("plugins");
    write_plugin(&plugins, "good", base("good", "1.0.0"));

    let state = PluginState::load(&state_path);
    let (run, skipped) = registry::runnable(&plugins, &state);
    assert!(run.is_empty());
    assert_eq!(skipped[0].1, "disabled");

    registry::set_enabled(&state_path, "good", true).unwrap();
    let state = PluginState::load(&state_path);
    let (run, _) = registry::runnable(&plugins, &state);
    assert_eq!(run.len(), 1);

    // Pinning to another version refuses it, unpinning restores it.
    registry::set_pin(&state_path, "good", Some("2.0.0")).unwrap();
    let (run, skipped) = registry::runnable(&plugins, &PluginState::load(&state_path));
    assert!(run.is_empty());
    assert!(skipped[0].1.contains("pinned to 2.0.0"), "{skipped:?}");
    registry::set_pin(&state_path, "good", None).unwrap();
    let (run, _) = registry::runnable(&plugins, &PluginState::load(&state_path));
    assert_eq!(run.len(), 1);

    // Safe disabling.
    registry::set_enabled(&state_path, "good", false).unwrap();
    let (run, _) = registry::runnable(&plugins, &PluginState::load(&state_path));
    assert!(run.is_empty());
    assert!(registry::require_known(&plugins, "nope").is_err());
}

#[test]
fn subprocess_permission_is_enforced_at_registration() {
    let dir = tempfile::tempdir().unwrap();
    let mut m = base("spawner", "1.0.0");
    m["permissions"] = json!({"spawn_subprocess": true});
    write_plugin(dir.path(), "spawner", m);
    let manifests = cinvar::plugins::discover(dir.path()).unwrap();
    let mut reg = cinvar::tools::Registry::new();
    let err = cinvar::plugins::register(&mut reg, &manifests, false).unwrap_err();
    assert_eq!(err.code, cinvar::error::ErrorCode::PermissionDenied);
    assert_eq!(
        reg.len(),
        0,
        "a refused plugin must leave the registry untouched"
    );
    assert_eq!(
        cinvar::plugins::register(&mut reg, &manifests, true).unwrap(),
        1
    );
}

#[test]
fn health_checks_only_run_when_subprocess_plugins_are_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let mut m = base("healthy", "1.0.0");
    m["health"] = json!(["true"]);
    write_plugin(dir.path(), "healthy", m);
    let skipped = registry::inventory(dir.path(), &PluginState::default(), false);
    assert_eq!(
        skipped.iter().find(|p| p.name == "healthy").unwrap().health,
        Health::NotChecked
    );
    // Not enabled: health checks (plugin code) must not run even when subprocesses are allowed.
    let not_enabled = registry::inventory(dir.path(), &PluginState::default(), true);
    assert_eq!(
        not_enabled
            .iter()
            .find(|p| p.name == "healthy")
            .unwrap()
            .health,
        Health::NotChecked
    );
    let mut enabled = PluginState::default();
    enabled.enabled.insert("healthy".into(), true);
    let ran = registry::inventory(dir.path(), &enabled, true);
    assert_eq!(
        ran.iter().find(|p| p.name == "healthy").unwrap().health,
        Health::Ok
    );
    let mut esc = base("escaper", "1.0.0");
    esc["health"] = json!(["../../bin/sh"]);
    write_plugin(dir.path(), "escaper", esc);
    enabled.enabled.insert("escaper".into(), true);
    let ran = registry::inventory(dir.path(), &enabled, true);
    assert_eq!(
        ran.iter().find(|p| p.name == "escaper").unwrap().health,
        Health::Invalid
    );
}
