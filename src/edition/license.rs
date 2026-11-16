//! Signed, offline-verifiable license files (Ed25519).
//!
//! A license file is JSON: `{"key_id": "...", "payload": "<base64>", "signature": "<base64>"}`.
//! `payload` is the base64 of a UTF-8 JSON document (see [`Payload`]); `signature` is an Ed25519
//! signature over those exact payload bytes. Verification needs only the public key, so the app
//! never contacts a server and never needs an account.
//!
//! The payload carries an opaque `order_ref` (for support lookup), never a name or email.
//! Perpetual licenses have no expiry field: a $149 license stays valid for every version unless
//! a future policy adds a `min_version`/`max_version` pair (unknown payload fields are ignored,
//! so older binaries keep accepting newer licenses).
//!
//! No production public key is compiled in yet: [`production_keys`] is empty until the owner
//! generates the signing keypair (private key stays outside the repository). Until then no real
//! license verifies, which is safe because enforcement defaults to observe-only.

use std::path::{Path, PathBuf};

use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};

pub const LICENSE_FILE: &str = "license.json";
const MAX_LICENSE_BYTES: u64 = 16 * 1024;
const PAYLOAD_VERSION: u32 = 1;

/// A trusted verification key and the id licenses use to name it (allows key rotation).
#[derive(Debug, Clone)]
pub struct PublicKey {
    pub id: String,
    pub bytes: [u8; 32],
}

/// Production verification keys. The matching private key is held by the operator outside this
/// repository (see docs/LICENSE_OPERATIONS.md). To rotate: add a `k2` entry here, ship it, and
/// start signing with the new key; keep `k1` for as long as licenses signed with it must verify.
pub fn production_keys() -> &'static [PublicKey] {
    static KEYS: std::sync::OnceLock<Vec<PublicKey>> = std::sync::OnceLock::new();
    KEYS.get_or_init(|| {
        vec![PublicKey {
            id: "k1".into(),
            bytes: [
                0xff, 0x2c, 0x19, 0xa6, 0x8e, 0xb2, 0xd8, 0xa2, 0xa1, 0xaa, 0x9e, 0xc0, 0xb0, 0x21,
                0xff, 0xed, 0x92, 0x75, 0x30, 0x85, 0x53, 0x2f, 0xfd, 0x46, 0x2c, 0xc2, 0x13, 0xed,
                0x60, 0x17, 0x2d, 0xa5,
            ],
        }]
    })
}

pub fn default_path() -> PathBuf {
    crate::util::paths::config_dir().join(LICENSE_FILE)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub key_id: String,
    pub payload: String,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Payload {
    pub v: u32,
    /// Unique license id.
    pub id: String,
    /// Must be `"pro"`; the only paid edition.
    pub edition: String,
    /// Opaque order/receipt reference for support (no personal data).
    #[serde(default)]
    pub order_ref: String,
    /// RFC 3339 date the license was issued.
    #[serde(default)]
    pub issued_at: String,
    /// True for licenses issued to customers who bought before the Free/Pro split.
    #[serde(default)]
    pub grandfathered: bool,
}

/// A license that passed signature and content checks.
#[derive(Debug, Clone)]
pub struct VerifiedLicense {
    pub id: String,
    pub grandfathered: bool,
    pub order_ref: String,
}

fn bad(reason: impl Into<String>) -> Error {
    Error::new(ErrorCode::InvalidInput, reason.into())
}

/// Verifies `envelope_json` against `keys`. Errors never include key material.
pub fn verify(envelope_json: &str, keys: &[PublicKey]) -> Result<VerifiedLicense> {
    let env: Envelope =
        serde_json::from_str(envelope_json).map_err(|_| bad("license file is not valid JSON"))?;
    let key = keys.iter().find(|k| k.id == env.key_id).ok_or_else(|| {
        bad(format!(
            "license was signed with unknown key '{}'",
            env.key_id
        ))
    })?;
    let b64 = base64::engine::general_purpose::STANDARD;
    let payload_bytes = b64
        .decode(env.payload.as_bytes())
        .map_err(|_| bad("license payload is not valid base64"))?;
    let sig_bytes = b64
        .decode(env.signature.as_bytes())
        .map_err(|_| bad("license signature is not valid base64"))?;
    let sig =
        Signature::from_slice(&sig_bytes).map_err(|_| bad("license signature is malformed"))?;
    let vk =
        VerifyingKey::from_bytes(&key.bytes).map_err(|_| bad("trusted license key is invalid"))?;
    vk.verify(&payload_bytes, &sig)
        .map_err(|_| bad("license signature does not match"))?;
    let payload: Payload =
        serde_json::from_slice(&payload_bytes).map_err(|_| bad("license payload is not valid"))?;
    if payload.v != PAYLOAD_VERSION {
        return Err(bad(format!("unsupported license version {}", payload.v)));
    }
    if payload.edition != "pro" {
        return Err(bad(format!(
            "unknown license edition '{}'",
            payload.edition
        )));
    }
    if payload.id.trim().is_empty() {
        return Err(bad("license has no id"));
    }
    Ok(VerifiedLicense {
        id: payload.id,
        grandfathered: payload.grandfathered,
        order_ref: payload.order_ref,
    })
}

/// `Ok(None)` when there is no license file; `Err` when one exists but does not verify.
pub fn load(path: &Path, keys: &[PublicKey]) -> Result<Option<VerifiedLicense>> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(Error::new(
                ErrorCode::Io,
                format!("cannot read license: {e}"),
            ))
        }
    };
    if !meta.is_file() || meta.len() > MAX_LICENSE_BYTES {
        return Err(bad("license file is not a regular file or is too large"));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::new(ErrorCode::Io, format!("cannot read license: {e}")))?;
    verify(&text, keys).map(Some)
}

/// Verifies `src` and, only if it verifies, copies it to `dest` (0600). Returns the license.
/// A bad file never replaces a good installed one.
pub fn install(src: &Path, dest: &Path, keys: &[PublicKey]) -> Result<VerifiedLicense> {
    let lic = load(src, keys)?.ok_or_else(|| Error::not_found("license file not found"))?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = dest.with_extension("json.tmp");
    std::fs::copy(src, &tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, dest)?;
    Ok(lic)
}

/// Installs a license pasted as text (web UI). Same verification and atomic replace as [`install`].
pub fn install_text(text: &str, dest: &Path, keys: &[PublicKey]) -> Result<VerifiedLicense> {
    if text.len() as u64 > MAX_LICENSE_BYTES {
        return Err(bad("license is too large"));
    }
    let lic = verify(text, keys)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = dest.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, dest)?;
    Ok(lic)
}

/// Removes the installed license (back to Free). Not an error if there is none.
pub fn remove(dest: &Path) -> Result<bool> {
    match std::fs::remove_file(dest) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Signing side. Compiled only for tests and the separate `issuer` feature, never into the
/// distributed `cinvar` binaries.
#[cfg(any(test, feature = "issuer"))]
pub mod signing {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    pub fn seed_to_key(seed: &[u8; 32]) -> SigningKey {
        SigningKey::from_bytes(seed)
    }

    pub fn public_bytes(seed: &[u8; 32]) -> [u8; 32] {
        seed_to_key(seed).verifying_key().to_bytes()
    }

    /// Signs an exact payload byte string into a license envelope JSON.
    pub fn sign_raw(seed: &[u8; 32], key_id: &str, payload: &[u8]) -> String {
        let b64 = base64::engine::general_purpose::STANDARD;
        let sig = seed_to_key(seed).sign(payload);
        serde_json::to_string_pretty(&Envelope {
            key_id: key_id.into(),
            payload: b64.encode(payload),
            signature: b64.encode(sig.to_bytes()),
        })
        .expect("serialize envelope")
    }

    pub fn payload_json(
        id: &str,
        order_ref: &str,
        issued_at: &str,
        grandfathered: bool,
    ) -> Vec<u8> {
        serde_json::to_vec(&Payload {
            v: PAYLOAD_VERSION,
            id: id.into(),
            edition: "pro".into(),
            order_ref: order_ref.into(),
            issued_at: issued_at.into(),
            grandfathered,
        })
        .expect("serialize payload")
    }
}

#[cfg(test)]
pub mod testing {
    //! Test-only signing helpers. The test key is public and worthless: it is never part of
    //! [`production_keys`].
    use super::*;

    pub const TEST_SECRET: [u8; 32] = [7u8; 32];

    pub fn test_key() -> PublicKey {
        PublicKey {
            id: "test".into(),
            bytes: signing::public_bytes(&TEST_SECRET),
        }
    }

    pub fn mint(id: &str, edition: &str, grandfathered: bool) -> String {
        let payload = serde_json::to_vec(&serde_json::json!({
            "v": 1, "id": id, "edition": edition, "order_ref": "ord_test",
            "issued_at": "2026-11-14", "grandfathered": grandfathered,
            "future_field": "ignored",
        }))
        .unwrap();
        mint_raw(&payload, "test")
    }

    pub fn mint_raw(payload: &[u8], key_id: &str) -> String {
        signing::sign_raw(&TEST_SECRET, key_id, payload)
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    #[test]
    fn valid_license_verifies_and_ignores_unknown_fields() {
        let lic = verify(&mint("L-1", "pro", true), &[test_key()]).unwrap();
        assert_eq!(lic.id, "L-1");
        assert!(lic.grandfathered);
    }

    #[test]
    fn tampered_payload_is_rejected() {
        let good = mint("L-1", "pro", false);
        let mut env: Envelope = serde_json::from_str(&good).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut bytes = b64.decode(&env.payload).unwrap();
        let text = String::from_utf8(bytes.clone())
            .unwrap()
            .replace("false", "true ");
        bytes = text.into_bytes();
        env.payload = b64.encode(bytes);
        let err = verify(&serde_json::to_string(&env).unwrap(), &[test_key()]).unwrap_err();
        assert!(err.message.contains("signature"), "{}", err.message);
    }

    #[test]
    fn unknown_key_wrong_edition_and_garbage_are_rejected() {
        assert!(verify(&mint("L-1", "pro", false), &[]).is_err());
        assert!(verify(&mint("L-1", "enterprise", false), &[test_key()]).is_err());
        assert!(verify("not json", &[test_key()]).is_err());
        assert!(verify(
            &mint_raw(b"{\"v\":2,\"id\":\"x\",\"edition\":\"pro\"}", "test"),
            &[test_key()]
        )
        .is_err());
        assert!(verify(
            &mint_raw(b"{\"v\":1,\"id\":\" \",\"edition\":\"pro\"}", "test"),
            &[test_key()]
        )
        .is_err());
    }

    #[test]
    fn production_key_is_provisioned_valid_and_is_not_the_test_key() {
        let keys = production_keys();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].id, "k1");
        assert!(
            ed25519_dalek::VerifyingKey::from_bytes(&keys[0].bytes).is_ok(),
            "valid curve point"
        );
        assert_ne!(
            keys[0].bytes,
            test_key().bytes,
            "the public test key must never ship"
        );
        // a license signed with the (public, worthless) test key must not verify in production
        assert!(verify(&mint("L-1", "pro", false), keys).is_err());
    }

    #[test]
    fn install_only_accepts_verified_licenses_and_never_clobbers() {
        let dir = tempfile::tempdir().unwrap();
        let (good, bad, dest) = (
            dir.path().join("g.json"),
            dir.path().join("b.json"),
            dir.path().join("cfg/license.json"),
        );
        std::fs::write(&good, mint("L-1", "pro", true)).unwrap();
        std::fs::write(&bad, "junk").unwrap();
        assert!(install(&bad, &dest, &[test_key()]).is_err());
        assert!(!dest.exists());
        assert_eq!(install(&good, &dest, &[test_key()]).unwrap().id, "L-1");
        assert!(install(&bad, &dest, &[test_key()]).is_err());
        assert_eq!(
            load(&dest, &[test_key()]).unwrap().unwrap().id,
            "L-1",
            "good license survives a bad install"
        );
        assert!(
            install(&good, &dest, &[]).is_err(),
            "no trusted keys => nothing installs"
        );
        assert!(remove(&dest).unwrap());
        assert!(!remove(&dest).unwrap());
    }

    #[test]
    fn missing_file_is_none_and_bad_file_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(LICENSE_FILE);
        assert!(load(&p, &[test_key()]).unwrap().is_none());
        std::fs::write(&p, "junk").unwrap();
        assert!(load(&p, &[test_key()]).is_err());
        std::fs::write(&p, mint("L-9", "pro", false)).unwrap();
        assert_eq!(load(&p, &[test_key()]).unwrap().unwrap().id, "L-9");
    }
}
