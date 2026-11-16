//! Operator tool: generate the license signing key and issue/verify CINVAR Pro licenses.
//! Built only with `--features issuer`; never packaged. The private key lives OUTSIDE any
//! repository (default ~/.cinvar-issuer, mode 0700/0600) and is never printed.
use std::io::Write;
use std::path::{Path, PathBuf};

use cinvar::edition::license::{self, signing};
use sha2::{Digest, Sha256};

fn die(msg: &str) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(2);
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    let s = s.trim();
    if s.len() % 2 != 0 || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        die("not valid hex");
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
}

fn default_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| die("HOME not set")))
        .join(".cinvar-issuer")
}

/// True if `p` is inside a git work tree AND git would not ignore it (so `git add -A` could
/// capture a key placed there).
fn exposed_to_git(p: &Path) -> bool {
    let probe = if p.exists() {
        p.to_path_buf()
    } else {
        p.parent().map(|q| q.to_path_buf()).unwrap_or_default()
    };
    if !inside_git_repo(&probe) {
        return false;
    }
    let ignored = std::process::Command::new("git")
        .arg("-C")
        .arg(&probe)
        .args(["check-ignore", "-q"])
        .arg(p)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    !ignored
}

fn inside_git_repo(p: &Path) -> bool {
    let mut cur = std::fs::canonicalize(p)
        .ok()
        .or_else(|| p.parent().and_then(|q| std::fs::canonicalize(q).ok()));
    while let Some(dir) = cur {
        if dir.join(".git").exists() {
            return true;
        }
        cur = dir.parent().map(|d| d.to_path_buf());
    }
    false
}

fn read_seed(path: &Path) -> [u8; 32] {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .unwrap_or_else(|e| die(&format!("cannot read key: {e}")))
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            die("signing key must not be accessible by group/others (chmod 600)");
        }
    }
    let raw = unhex(
        &std::fs::read_to_string(path).unwrap_or_else(|e| die(&format!("cannot read key: {e}"))),
    );
    raw.try_into()
        .unwrap_or_else(|_| die("signing key must be 32 bytes"))
}

fn write_secret(path: &Path, data: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap_or_else(|e| die(&format!("cannot create {}: {e}", path.display())));
        f.write_all(data.as_bytes()).unwrap();
    }
    #[cfg(not(unix))]
    std::fs::write(path, data).unwrap();
}

fn today() -> String {
    cinvar::util::clock::now_rfc3339()
        .chars()
        .take(10)
        .collect()
}

fn issue_one(
    seed: &[u8; 32],
    key_id: &str,
    order_ref: &str,
    grandfathered: bool,
    ledger: Option<&Path>,
    out: &Path,
) {
    let mut rnd = [0u8; 6];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut rnd);
    let id = format!("L-{}", hex(&rnd));
    let payload = signing::payload_json(&id, order_ref, &today(), grandfathered);
    let envelope = signing::sign_raw(seed, key_id, &payload);
    // Never issue something the shipped verifier would reject.
    let key = license::PublicKey {
        id: key_id.into(),
        bytes: signing::public_bytes(seed),
    };
    if let Err(e) = license::verify(&envelope, &[key]) {
        die(&format!("self-check failed: {}", e.message));
    }
    std::fs::write(out, envelope)
        .unwrap_or_else(|e| die(&format!("cannot write {}: {e}", out.display())));
    if let Some(l) = ledger {
        let line = serde_json::json!({
            "license_id": id, "order_ref": order_ref, "issued_at": today(),
            "grandfathered": grandfathered, "key_id": key_id,
            "payload_sha256": hex(&Sha256::digest(&payload)),
        });
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(l)
            .unwrap_or_else(|e| die(&format!("ledger: {e}")));
        writeln!(f, "{line}").unwrap();
    }
    println!("issued {id} for order {order_ref} -> {}", out.display());
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("keygen") => {
            let dir = flag(&args, "--out")
                .map(PathBuf::from)
                .unwrap_or_else(default_dir);
            if exposed_to_git(&dir) {
                die("refusing to create the signing key where git could track it (inside a work tree and not git-ignored)");
            }
            if dir.exists()
                && std::fs::read_dir(&dir)
                    .map(|mut d| d.next().is_some())
                    .unwrap_or(true)
            {
                die("key directory exists and is not empty; refusing to overwrite a signing key");
            }
            std::fs::create_dir_all(&dir).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let mut seed = [0u8; 32];
            rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut seed);
            write_secret(&dir.join("license-signing.key"), &hex(&seed));
            let public = signing::public_bytes(&seed);
            std::fs::write(dir.join("license-signing.pub"), hex(&public)).unwrap();
            println!(
                "private key: {} (mode 0600, not printed; back it up offline)",
                dir.join("license-signing.key").display()
            );
            println!("public key (hex): {}", hex(&public));
        }
        Some("issue") => {
            let key = flag(&args, "--key")
                .map(PathBuf::from)
                .unwrap_or_else(|| default_dir().join("license-signing.key"));
            let order = flag(&args, "--order-ref")
                .unwrap_or_else(|| die("--order-ref required (opaque order id, no personal data)"));
            let out = flag(&args, "--out").unwrap_or_else(|| die("--out required"));
            let ledger = flag(&args, "--ledger")
                .map(PathBuf::from)
                .unwrap_or_else(|| default_dir().join("ledger.jsonl"));
            issue_one(
                &read_seed(&key),
                flag(&args, "--key-id").unwrap_or("k1"),
                order,
                args.iter().any(|a| a == "--grandfathered"),
                Some(&ledger),
                Path::new(out),
            );
        }
        Some("issue-batch") => {
            // CSV: order_ref[,grandfathered]; header optional. Output: <outdir>/<order_ref>.json
            let key = flag(&args, "--key")
                .map(PathBuf::from)
                .unwrap_or_else(|| default_dir().join("license-signing.key"));
            let csv = flag(&args, "--csv").unwrap_or_else(|| die("--csv required"));
            let outdir =
                PathBuf::from(flag(&args, "--outdir").unwrap_or_else(|| die("--outdir required")));
            std::fs::create_dir_all(&outdir).unwrap();
            let ledger = flag(&args, "--ledger")
                .map(PathBuf::from)
                .unwrap_or_else(|| default_dir().join("ledger.jsonl"));
            let seed = read_seed(&key);
            let text = std::fs::read_to_string(csv).unwrap_or_else(|e| die(&format!("csv: {e}")));
            let mut n = 0;
            for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
                let mut cols = line.split(',').map(str::trim);
                let order = cols.next().unwrap();
                if order.eq_ignore_ascii_case("order_ref") {
                    continue;
                }
                if !order
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-.".contains(&c))
                {
                    die(&format!(
                        "order_ref '{order}' has characters outside [A-Za-z0-9_.-]"
                    ));
                }
                let gf = cols
                    .next()
                    .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
                    .unwrap_or(true);
                issue_one(
                    &seed,
                    "k1",
                    order,
                    gf,
                    Some(&ledger),
                    &outdir.join(format!("{order}.json")),
                );
                n += 1;
            }
            println!("{n} license(s) issued");
        }
        Some("verify") => {
            let file = flag(&args, "--file").unwrap_or_else(|| die("--file required"));
            let keys: Vec<license::PublicKey> = match flag(&args, "--pub") {
                Some(h) => vec![license::PublicKey {
                    id: flag(&args, "--key-id").unwrap_or("k1").into(),
                    bytes: unhex(h)
                        .try_into()
                        .unwrap_or_else(|_| die("public key must be 32 bytes")),
                }],
                None => license::production_keys().to_vec(),
            };
            let text = std::fs::read_to_string(file).unwrap_or_else(|e| die(&format!("{e}")));
            match license::verify(&text, &keys) {
                Ok(l) => println!(
                    "valid: {} grandfathered={} order={}",
                    l.id, l.grandfathered, l.order_ref
                ),
                Err(e) => {
                    eprintln!("INVALID: {}", e.message);
                    std::process::exit(1)
                }
            }
        }
        _ => {
            eprintln!("usage: cinvar-license-issuer keygen [--out DIR] | issue --order-ref ID [--grandfathered] --out FILE [--key FILE] | issue-batch --csv FILE --outdir DIR | verify --file FILE [--pub HEX]");
            std::process::exit(2);
        }
    }
}
