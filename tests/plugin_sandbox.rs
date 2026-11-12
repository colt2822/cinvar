#![cfg(target_os = "linux")]
//! Sandbox details that need their own process (they set CINVAR_CONFIG_DIR): the credentials
//! directory is invisible to plugin code even when it lives outside $HOME, and plugin health
//! checks run inside the same sandbox.
use cinvar::plugins::registry::{self, Health, PluginState};
use serde_json::json;

#[test]
fn credentials_outside_home_and_health_checks_are_sandboxed() {
    if cinvar::util::proc::which("bwrap").is_none() {
        eprintln!("bubblewrap not installed; skipping");
        return;
    }
    // /var/tmp is neither $HOME nor a tmpfs the sandbox would mask anyway.
    let cfg = tempfile::tempdir_in("/var/tmp").unwrap();
    std::env::set_var("CINVAR_CONFIG_DIR", cfg.path());
    std::fs::write(
        cfg.path().join("credentials.toml"),
        "runpod = \"rpa_SECRET\"",
    )
    .unwrap();

    // sandbox_command directly
    let project = tempfile::tempdir().unwrap();
    let (prog, args, sandboxed) = cinvar::plugins::sandbox_command(
        std::path::Path::new("/bin/sh"),
        &[
            "-c".into(),
            format!(
                "cat '{}/credentials.toml' 2>&1; echo END",
                cfg.path().display()
            ),
        ],
        None,
        &Default::default(),
        Some(project.path()),
    );
    assert!(sandboxed);
    let out = std::process::Command::new(prog)
        .args(args)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !text.contains("rpa_SECRET"),
        "credentials visible inside the sandbox: {text}"
    );
    assert!(text.contains("END"));

    // health check: runs sandboxed -> cannot write into its own (read-only) plugin dir
    let plugins = tempfile::tempdir().unwrap();
    let root = plugins.path().join("probe");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("health.sh"), "#!/bin/sh\ntouch marker\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            root.join("health.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    std::fs::write(
        root.join("plugin.json"),
        json!({"name": "probe", "version": "1.0.0", "description": "t", "cinvar_min": "0.1.0",
               "health": ["health.sh"],
               "tools": [{"name": "t", "description": "d", "input": {"type": "object", "properties": {}}, "command": ["true"]}]})
        .to_string(),
    )
    .unwrap();
    // Health checks never run for a plugin that is not enabled.
    let off = registry::inventory(plugins.path(), &PluginState::default(), true);
    let p = off.iter().find(|p| p.name == "probe").unwrap();
    assert_eq!(p.health, Health::NotChecked, "{:?}", p.health_detail);
    assert!(!root.join("marker").exists());
    let mut state = PluginState::default();
    state.enabled.insert("probe".into(), true);
    let inv = registry::inventory(plugins.path(), &state, true);
    let p = inv.iter().find(|p| p.name == "probe").unwrap();
    assert_eq!(
        p.health,
        Health::Unavailable,
        "the sandboxed health check must fail to write: {:?}",
        p.health_detail
    );
    assert!(
        !root.join("marker").exists(),
        "health check wrote into the plugin directory"
    );
}

#[test]
fn host_control_sockets_under_run_are_not_reachable() {
    if cinvar::util::proc::which("bwrap").is_none() {
        eprintln!("bubblewrap not installed; skipping");
        return;
    }
    let project = tempfile::tempdir().unwrap();
    let list = |network: bool| {
        let perms = cinvar::plugins::PluginPermissions {
            network,
            ..Default::default()
        };
        let (prog, args, sandboxed) = cinvar::plugins::sandbox_command(
            std::path::Path::new("/bin/sh"),
            &[
                "-c".into(),
                "find /run/user /run/dbus /run/docker.sock -type s 2>/dev/null | wc -l; ls /run | wc -l".into(),
            ],
            None,
            &perms,
            Some(project.path()),
        );
        assert!(sandboxed);
        let out = std::process::Command::new(prog)
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    // No network: /run is an empty tmpfs.
    let t = list(false);
    let lines: Vec<&str> = t.lines().map(str::trim).collect();
    assert_eq!(lines, ["0", "0"], "{t}");
    // Network allowed: the rest of /run (DNS) stays, but no control sockets are reachable.
    let t = list(true);
    assert_eq!(
        t.lines().next().unwrap().trim(),
        "0",
        "sockets visible: {t}"
    );
}
