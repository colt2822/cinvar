//! Operator workflow: keygen -> issue -> verify, against the real issuer binary.
#![cfg(feature = "issuer")]

use std::process::Command;

fn issuer(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cinvar-license-issuer"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn keygen_issue_verify_roundtrip_and_safety_rails() {
    let dir = tempfile::tempdir().unwrap();
    let kd = dir.path().join("keys");
    let out = issuer(&["keygen", "--out", kd.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let pubhex = std::fs::read_to_string(kd.join("license-signing.pub")).unwrap();
    assert!(text.contains(pubhex.trim()));
    let secret = std::fs::read_to_string(kd.join("license-signing.key")).unwrap();
    assert!(
        !text.contains(secret.trim()),
        "private key must never be printed"
    );
    // refuses to overwrite an existing key directory
    assert!(!issuer(&["keygen", "--out", kd.to_str().unwrap()])
        .status
        .success());

    let lic = dir.path().join("a.json");
    let ledger = dir.path().join("ledger.jsonl");
    let key = kd.join("license-signing.key");
    let out = issuer(&[
        "issue",
        "--key",
        key.to_str().unwrap(),
        "--order-ref",
        "ord_123",
        "--grandfathered",
        "--out",
        lic.to_str().unwrap(),
        "--ledger",
        ledger.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ok = issuer(&[
        "verify",
        "--file",
        lic.to_str().unwrap(),
        "--pub",
        pubhex.trim(),
    ]);
    assert!(ok.status.success());
    assert!(String::from_utf8_lossy(&ok.stdout).contains("grandfathered=true"));
    // a different key does not verify it, and neither does production
    let other = "00".repeat(32);
    assert!(
        !issuer(&["verify", "--file", lic.to_str().unwrap(), "--pub", &other])
            .status
            .success()
    );
    assert!(!issuer(&["verify", "--file", lic.to_str().unwrap()])
        .status
        .success());
    // the ledger records ids and order refs, never keys
    let l = std::fs::read_to_string(&ledger).unwrap();
    assert!(l.contains("ord_123") && !l.contains(secret.trim()));

    // batch
    let csv = dir.path().join("o.csv");
    std::fs::write(&csv, "order_ref,grandfathered\nord_a,true\nord_b,false\n").unwrap();
    let od = dir.path().join("out");
    let out = issuer(&[
        "issue-batch",
        "--key",
        key.to_str().unwrap(),
        "--csv",
        csv.to_str().unwrap(),
        "--outdir",
        od.to_str().unwrap(),
        "--ledger",
        ledger.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(od.join("ord_a.json").exists() && od.join("ord_b.json").exists());
    // hostile order refs are refused (no path traversal)
    std::fs::write(&csv, "../evil,true\n").unwrap();
    assert!(!issuer(&[
        "issue-batch",
        "--key",
        key.to_str().unwrap(),
        "--csv",
        csv.to_str().unwrap(),
        "--outdir",
        od.to_str().unwrap()
    ])
    .status
    .success());

    // group/world-readable keys are refused
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!issuer(&[
            "issue",
            "--key",
            key.to_str().unwrap(),
            "--order-ref",
            "x",
            "--out",
            dir.path().join("x.json").to_str().unwrap()
        ])
        .status
        .success());
    }
}
