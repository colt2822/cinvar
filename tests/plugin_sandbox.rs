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

/// An abstract-namespace Unix socket listener held by the test (host) process.
struct AbstractListener {
    fd: libc::c_int,
}

impl AbstractListener {
    /// Binds and listens on the abstract name in the host network namespace. None when the kernel
    /// refuses, which only happens on a machine that already blocks AF_UNIX sockets.
    fn bind(name: &str) -> Option<Self> {
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        if fd < 0 {
            return None;
        }
        let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let bytes = name.as_bytes();
        // sun_path[0] == 0 marks the abstract namespace; the name follows that NUL.
        if bytes.len() + 1 >= addr.sun_path.len() {
            unsafe { libc::close(fd) };
            return None;
        }
        for (i, b) in bytes.iter().enumerate() {
            addr.sun_path[i + 1] = *b as libc::c_char;
        }
        let len = (std::mem::size_of::<libc::sa_family_t>() + 1 + bytes.len()) as libc::socklen_t;
        let bound = unsafe { libc::bind(fd, &addr as *const _ as *const libc::sockaddr, len) };
        if bound != 0 || unsafe { libc::listen(fd, 8) } != 0 {
            unsafe { libc::close(fd) };
            return None;
        }
        Some(Self { fd })
    }
}

impl Drop for AbstractListener {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

/// A manifest may declare network: true; that buys network egress, never control of host services.
/// Host abstract Unix sockets (@dbus-*, @systemd, docker) are reachable by name from a shared
/// network namespace even when every socket file under /run is masked, so the sandbox must refuse
/// to create AF_UNIX sockets.
#[test]
fn host_abstract_unix_sockets_are_not_reachable_from_a_network_plugin() {
    if cinvar::util::proc::which("bwrap").is_none() {
        eprintln!("bubblewrap not installed; skipping");
        return;
    }
    let Some(python) = cinvar::util::proc::which("python3") else {
        eprintln!("python3 not installed; skipping");
        return;
    };
    let name = format!("cinvar-sbx-probe-{}", std::process::id());
    let Some(_listener) = AbstractListener::bind(&name) else {
        eprintln!("could not bind an abstract Unix socket; skipping");
        return;
    };
    // A host TCP listener stands in for legitimate egress: the network permission must keep working
    // after the Unix-socket escape is closed.
    let tcp = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = tcp.local_addr().unwrap().port();
    let script = format!(
        r#"
import socket
try:
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(3)
    s.connect(chr(0) + "{name}")
    print("ABSTRACT_OK")
except OSError as e:
    print("ABSTRACT_BLOCKED", e.errno)
try:
    t = socket.create_connection(("127.0.0.1", {port}), timeout=3)
    print("INET_OK")
except OSError as e:
    print("INET_BLOCKED", e.strerror)
# The filter is an address-family allowlist, not an AF_UNIX blacklist: UDP, IPv6 and internal
# socketpairs (what a plugin uses to talk to its own threads) must survive.
try:
    socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    print("UDP_OK")
except OSError as e:
    print("UDP_BLOCKED", e.errno)
try:
    socket.socket(socket.AF_INET6, socket.SOCK_STREAM)
    print("INET6_OK")
except OSError as e:
    print("INET6_BLOCKED", e.errno)
try:
    socket.socketpair()
    print("SOCKETPAIR_OK")
except OSError as e:
    print("SOCKETPAIR_BLOCKED", e.errno)
"#
    );
    let perms = cinvar::plugins::PluginPermissions {
        network: true,
        ..Default::default()
    };
    let project = tempfile::tempdir().unwrap();
    let (prog, args, sandboxed) = cinvar::plugins::sandbox_command(
        &python,
        &["-c".into(), script],
        None,
        &perms,
        Some(project.path()),
    );
    assert!(sandboxed);
    let out = std::process::Command::new(prog)
        .args(args)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("ABSTRACT_BLOCKED"),
        "a plugin with network:true reached a host abstract Unix socket: {text}"
    );
    for kept in ["INET_OK", "UDP_OK", "INET6_OK", "SOCKETPAIR_OK"] {
        assert!(
            text.contains(kept),
            "closing the Unix-socket escape broke legitimate plugin networking ({kept}): {text}"
        );
    }
}

/// /proc inside the sandbox is a fresh procfs in the plugin's own PID namespace: host PIDs must be
/// neither listed nor readable, or a plugin could inspect or signal host processes.
#[test]
fn sandboxed_proc_shows_no_host_processes() {
    if cinvar::util::proc::which("bwrap").is_none() {
        eprintln!("bubblewrap not installed; skipping");
        return;
    }
    let host_pid = std::process::id();
    let project = tempfile::tempdir().unwrap();
    let script = format!(
        "echo -n 'CMDLINE: '; cat /proc/{host_pid}/cmdline >/dev/null 2>&1 && echo READABLE || echo HIDDEN; echo -n 'COUNT: '; ls /proc | grep -c '^[0-9][0-9]*$'"
    );
    let (prog, args, sandboxed) = cinvar::plugins::sandbox_command(
        std::path::Path::new("/bin/sh"),
        &["-c".into(), script],
        None,
        &Default::default(),
        Some(project.path()),
    );
    assert!(sandboxed);
    let out = std::process::Command::new(prog)
        .args(args)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("CMDLINE: HIDDEN"),
        "host process {host_pid} is readable inside the sandbox: {text}"
    );
    let visible: usize = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("COUNT: "))
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(usize::MAX);
    assert!(
        visible < 16,
        "the sandbox sees {visible} /proc entries; it must only show its own namespace: {text}"
    );
}

/// The argv *is* the sandbox: check both permission cases and that only canonicalised, pre-existing
/// directories are bind-mounted (a manifest cannot add a host bind of its own).
#[test]
fn sandbox_argv_binds_only_canonical_dirs_and_keeps_host_netns_guarded() {
    if cinvar::util::proc::which("bwrap").is_none() {
        eprintln!("bubblewrap not installed; skipping");
        return;
    }
    let project = tempfile::tempdir().unwrap();
    let build = |network: bool, root: Option<&std::path::Path>| {
        let perms = cinvar::plugins::PluginPermissions {
            network,
            ..Default::default()
        };
        let (prog, args, sandboxed) = cinvar::plugins::sandbox_command(
            std::path::Path::new("/bin/true"),
            &[],
            root,
            &perms,
            Some(project.path()),
        );
        assert!(sandboxed);
        eprintln!("network={network} program={} argv={args:?}", prog.display());
        (prog, args)
    };
    let has = |args: &[String], needle: &str| args.iter().any(|a| a == needle);

    // No network: an isolated network namespace, plain bubblewrap, no filter needed.
    let (prog, off) = build(false, None);
    assert_eq!(prog.file_name().unwrap(), "bwrap", "{}", prog.display());
    assert!(has(&off, "--unshare-net"), "{off:?}");
    assert!(!has(&off, "--seccomp"), "{off:?}");

    // Network: the host namespace stays (egress works) but AF_UNIX sockets are denied by a seccomp
    // filter handed to bubblewrap on a descriptor.
    let (prog, on) = build(true, None);
    assert!(
        !has(&on, "--unshare-net"),
        "the network permission must not silently become no network: {on:?}"
    );
    assert!(has(&on, "--seccomp"), "{on:?}");
    assert_eq!(prog.file_name().unwrap(), "sh", "{}", prog.display());

    // plugin_root is canonicalised: a symlinked plugin directory cannot redirect the read-only bind
    // somewhere else on the host.
    let real = project.path().join("real-plugin");
    std::fs::create_dir_all(&real).unwrap();
    let link = project.path().join("linked-plugin");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let (_, via_link) = build(false, Some(&link));
    let canonical = std::fs::canonicalize(&real).unwrap();
    // The last --ro-bind pair is the plugin directory; the first is the read-only root.
    let idx = via_link.iter().rposition(|a| a == "--ro-bind").unwrap();
    assert_eq!(
        via_link[idx + 1],
        canonical.to_string_lossy(),
        "a symlinked plugin_root must be bound at its canonical path: {via_link:?}"
    );
    assert_eq!(via_link[idx + 2], via_link[idx + 1]);
    assert!(
        !via_link.iter().any(|a| *a == *link.to_string_lossy()),
        "the manifest-side path leaked into the argv: {via_link:?}"
    );
}
