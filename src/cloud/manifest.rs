//! Portable render manifest: everything a remote worker needs, with no local paths or secrets.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Error, ErrorCode, Result};
use crate::project::model::Project;
use crate::project::store::{sha256_file, ProjectStore};
use crate::render::RenderRequest;

pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetRef {
    pub sha256: String,
    /// Project-relative path the asset must be restored to (validated on the worker).
    pub path: String,
    pub bytes: u64,
}

/// The render parameters that may travel. `output` is always chosen by the worker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RenderSpec {
    pub size_preset: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub codec: Option<String>,
    pub crf: Option<u8>,
    pub bit_depth: Option<u8>,
    pub format: Option<String>,
    pub burn_captions: bool,
    pub timeline_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobManifest {
    pub schema: u32,
    pub project: Value,
    pub assets: Vec<AssetRef>,
    pub render: RenderSpec,
    pub output_name: String,
}

impl JobManifest {
    /// Deterministic: same project revision + settings => same hash.
    pub fn hash(&self) -> String {
        let text = serde_json::to_string(self).unwrap_or_default();
        Sha256::digest(text.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    pub fn total_asset_bytes(&self) -> u64 {
        self.assets.iter().map(|a| a.bytes).sum()
    }
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// A relative, normalised, non-escaping project path.
pub fn safe_relative(path: &str) -> bool {
    let p = Path::new(path);
    !path.is_empty()
        && p.is_relative()
        && p.components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

pub fn validate(manifest: &JobManifest) -> Result<()> {
    if manifest.schema != SCHEMA {
        return Err(Error::unsupported(format!(
            "manifest schema {} is not supported (expected {SCHEMA})",
            manifest.schema
        )));
    }
    for a in &manifest.assets {
        if !is_hex64(&a.sha256) {
            return Err(Error::invalid("asset sha256 must be 64 hex characters"));
        }
        if !safe_relative(&a.path) {
            return Err(Error::new(
                ErrorCode::PermissionDenied,
                format!("asset path '{}' is not a safe relative path", a.path),
            ));
        }
    }
    if manifest.output_name.is_empty()
        || manifest.output_name.contains('/')
        || manifest.output_name.contains('\\')
        || manifest.output_name.starts_with('.')
    {
        return Err(Error::invalid("output_name must be a plain file name"));
    }
    Ok(())
}

/// Builds a manifest for `project` and returns the local files to upload (sha256, path).
/// Assets are hashed from disk (not trusted from metadata) so the manifest is verifiable.
pub fn build(
    project: &Project,
    store: &ProjectStore,
    request: &RenderRequest,
) -> Result<(JobManifest, Vec<(String, PathBuf)>)> {
    let timeline = match request.timeline_id.as_deref() {
        Some(id) => project.timeline_by_id(id),
        None => project.timeline(),
    }
    .ok_or_else(|| Error::invalid("no timeline to render"))?;
    let mut used: Vec<String> = timeline
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .filter_map(|c| c.asset.clone())
        .collect();
    used.sort();
    used.dedup();
    let mut assets = Vec::new();
    let mut files = Vec::new();
    for id in used {
        let asset = project
            .asset(&id)
            .ok_or_else(|| Error::not_found(format!("asset {id} is not in this project")))?;
        let local = store.asset_path(asset)?;
        if !local.is_file() {
            return Err(
                Error::not_found(format!("asset file missing: {}", asset.path))
                    .with_hint("relink or re-import the asset before a cloud render"),
            );
        }
        let sha = sha256_file(&local)?;
        let bytes = std::fs::metadata(&local)?.len();
        assets.push(AssetRef {
            sha256: sha.clone(),
            path: asset.path.clone(),
            bytes,
        });
        files.push((sha, local));
    }
    let ext = request
        .format
        .clone()
        .or_else(|| {
            request
                .output
                .extension()
                .and_then(|e| e.to_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "mp4".into());
    let manifest = JobManifest {
        schema: SCHEMA,
        project: serde_json::to_value(project)?,
        assets,
        render: RenderSpec {
            size_preset: request.size_preset.clone(),
            width: request.width,
            height: request.height,
            codec: request.codec.clone(),
            crf: request.crf,
            bit_depth: request.bit_depth,
            format: request.format.clone(),
            burn_captions: request.burn_captions,
            timeline_id: request.timeline_id.clone(),
        },
        output_name: format!("render.{ext}"),
    };
    validate(&manifest)?;
    Ok((manifest, files))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> JobManifest {
        JobManifest {
            schema: SCHEMA,
            project: serde_json::json!({}),
            assets: vec![],
            render: RenderSpec {
                size_preset: None,
                width: None,
                height: None,
                codec: None,
                crf: None,
                bit_depth: None,
                format: None,
                burn_captions: true,
                timeline_id: None,
            },
            output_name: "render.mp4".into(),
        }
    }

    #[test]
    fn path_safety() {
        assert!(safe_relative("assets/a.mp4"));
        assert!(!safe_relative("../x"));
        assert!(!safe_relative("/etc/passwd"));
        assert!(!safe_relative("a/../../b"));
        assert!(!safe_relative(""));
    }

    #[test]
    fn hostile_manifests_are_rejected() {
        let mut m = base();
        m.assets.push(AssetRef {
            sha256: "zz".into(),
            path: "assets/a".into(),
            bytes: 1,
        });
        assert!(validate(&m).is_err());
        let mut m = base();
        m.assets.push(AssetRef {
            sha256: "a".repeat(64),
            path: "../../etc/passwd".into(),
            bytes: 1,
        });
        assert_eq!(validate(&m).unwrap_err().code, ErrorCode::PermissionDenied);
        let mut m = base();
        m.output_name = "../x.mp4".into();
        assert!(validate(&m).is_err());
        assert!(validate(&base()).is_ok());
    }

    #[test]
    fn hash_is_deterministic_and_sensitive() {
        let a = base();
        let mut b = base();
        assert_eq!(a.hash(), b.hash());
        b.render.crf = Some(18);
        assert_ne!(a.hash(), b.hash());
    }
}
