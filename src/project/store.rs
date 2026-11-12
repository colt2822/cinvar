use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::project::model::*;
use crate::util::{paths, text};

pub const PROJECT_FILE: &str = "project.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevisionInfo {
    pub revision: u64,
    pub file: String,
    pub label: String,
    pub at: String,
    pub bytes: u64,
}

#[derive(Debug, Clone)]
pub struct ProjectStore {
    pub root: PathBuf,
}

impl ProjectStore {
    pub fn create(parent: &Path, name: &str) -> Result<(ProjectStore, Project)> {
        paths::ensure_dir(parent)?;
        let slug = text::slugify(name);
        let mut root = parent.join(&slug);
        let mut attempt = 2;
        while root.exists() {
            root = parent.join(format!("{slug}-{attempt}"));
            attempt += 1;
        }
        let store = ProjectStore { root };
        for sub in ["assets", "cache", "renders", "revisions", "sessions", "tmp"] {
            paths::ensure_dir(&store.root.join(sub))?;
        }
        let mut project = Project::new(name);
        store.save(&mut project)?;
        Ok((store, project))
    }

    pub fn open(dir: &Path) -> Result<ProjectStore> {
        let root = if dir.file_name().map(|n| n == PROJECT_FILE).unwrap_or(false) {
            dir.parent().unwrap_or(Path::new(".")).to_path_buf()
        } else {
            dir.to_path_buf()
        };
        let store = ProjectStore {
            root: paths::normalize(&root),
        };
        if !store.project_file().exists() {
            return Err(
                Error::not_found(format!("no CINVAR project at {}", store.root.display()))
                    .with_hint("create one with: cinvar project create --name MyProject"),
            );
        }
        for sub in ["assets", "cache", "renders", "revisions", "sessions", "tmp"] {
            paths::ensure_dir(&store.root.join(sub))?;
        }
        Ok(store)
    }

    pub fn project_file(&self) -> PathBuf {
        self.root.join(PROJECT_FILE)
    }
    pub fn assets_dir(&self) -> PathBuf {
        self.root.join("assets")
    }
    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }
    pub fn renders_dir(&self) -> PathBuf {
        self.root.join("renders")
    }
    pub fn revisions_dir(&self) -> PathBuf {
        self.root.join("revisions")
    }
    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }
    pub fn jobs_dir(&self) -> PathBuf {
        self.root.join("jobs")
    }
    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    pub fn load(&self) -> Result<Project> {
        let text = std::fs::read_to_string(self.project_file())?;
        let project: Project = serde_json::from_str(&text)?;
        if project.schema > SCHEMA_VERSION {
            return Err(Error::unsupported(format!(
                "project schema {} is newer than this build supports ({SCHEMA_VERSION})",
                project.schema
            ))
            .with_hint("upgrade CINVAR to open this project"));
        }
        Ok(project)
    }

    /// Take the cross-process lock for a load -> edit -> save cycle.
    pub fn lock(&self, wait: std::time::Duration) -> Result<crate::project::lock::ProjectLock> {
        crate::project::lock::ProjectLock::acquire(&self.root, wait)
    }

    pub fn save(&self, project: &mut Project) -> Result<()> {
        // Optimistic concurrency: refuse to overwrite a newer revision written by someone else.
        if let Ok(text) = std::fs::read_to_string(self.project_file()) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                let on_disk = value.get("revision").and_then(|r| r.as_u64()).unwrap_or(0);
                if on_disk > project.revision {
                    return Err(Error::new(
                        crate::error::ErrorCode::Io,
                        format!(
                            "the project changed on disk (revision {on_disk}) since it was loaded (revision {}); your edit was not saved",
                            project.revision
                        ),
                    )
                    .with_hint("reload the project (cinvar.project.status) and repeat the edit"));
                }
            }
        }
        project.updated_at = crate::util::clock::now_rfc3339();
        project.revision = project.revision.saturating_add(1);
        let body = serde_json::to_string_pretty(project)?;
        paths::write_atomic(&self.project_file(), body.as_bytes())?;
        let snapshot = self.revisions_dir().join(format!(
            "{:05}-{}.json",
            project.revision,
            text::slugify(&project.name)
        ));
        let _ = paths::write_atomic(&snapshot, body.as_bytes());
        self.prune_revisions(project.settings.snapshot_limit)?;
        Ok(())
    }

    pub fn snapshot(&self, project: &Project, label: &str) -> Result<PathBuf> {
        let name = format!(
            "{:05}-{}-{}.json",
            project.revision,
            text::slugify(&project.name),
            text::slugify(label)
        );
        let path = self.revisions_dir().join(name);
        let body = serde_json::to_string_pretty(project)?;
        paths::write_atomic(&path, body.as_bytes())?;
        Ok(path)
    }

    pub fn revisions(&self) -> Result<Vec<RevisionInfo>> {
        let mut out = Vec::new();
        let dir = self.revisions_dir();
        if !dir.exists() {
            return Ok(out);
        }
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let file = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let revision = file
                .split('-')
                .next()
                .and_then(|r| r.parse::<u64>().ok())
                .unwrap_or(0);
            let label = file
                .trim_end_matches(".json")
                .splitn(3, '-')
                .nth(2)
                .unwrap_or("")
                .to_string();
            let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
            let at = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| crate::util::clock::format_rfc3339(d.as_secs() as i64))
                .unwrap_or_default();
            out.push(RevisionInfo {
                revision,
                file,
                label,
                at,
                bytes,
            });
        }
        out.sort_by(|a, b| b.revision.cmp(&a.revision).then(b.file.cmp(&a.file)));
        Ok(out)
    }

    fn prune_revisions(&self, limit: usize) -> Result<()> {
        if limit == 0 {
            return Ok(());
        }
        let revisions = self.revisions()?;
        for info in revisions.into_iter().skip(limit) {
            let _ = std::fs::remove_file(self.revisions_dir().join(&info.file));
        }
        Ok(())
    }

    pub fn rollback(&self, project: &mut Project, revision: u64) -> Result<()> {
        let revisions = self.revisions()?;
        let target = revisions
            .iter()
            .find(|r| r.revision == revision)
            .ok_or_else(|| Error::not_found(format!("revision {revision} does not exist")))?;
        let text = std::fs::read_to_string(self.revisions_dir().join(&target.file))?;
        let mut restored: Project = serde_json::from_str(&text)?;
        let keep_revisions = project.revision;
        restored.revision = keep_revisions.max(restored.revision);
        *project = restored;
        self.save(project)
    }

    pub fn import_asset(
        &self,
        project: &mut Project,
        source: &Path,
        origin: &str,
    ) -> Result<Asset> {
        if !source.exists() {
            return Err(Error::not_found(format!(
                "media file not found: {}",
                source.display()
            )));
        }
        let bytes_total = std::fs::metadata(source)?.len();
        let file_name = source
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let ext = source
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let kind = match ext.as_str() {
            "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" | "mts" | "ts" => AssetKind::Video,
            "wav" | "mp3" | "m4a" | "aac" | "flac" | "ogg" | "opus" => AssetKind::Audio,
            "png" | "jpg" | "jpeg" | "webp" | "bmp" | "tif" | "tiff" | "gif" => AssetKind::Image,
            "ttf" | "otf" => AssetKind::Font,
            _ => AssetKind::Other,
        };

        let sha256 = sha256_file(source)?;
        let existing = project
            .assets
            .iter()
            .find(|a| a.sha256 == sha256 && a.bytes == bytes_total);
        if let Some(asset) = existing {
            return Ok(asset.clone());
        }

        let unique = format!("{}-{}", &sha256[..8.min(sha256.len())], file_name);
        let dest = self.assets_dir().join(&unique);
        paths::unlink_if_symlink(&dest);
        if !dest.exists() {
            std::fs::copy(source, &dest)?;
        }

        let info = match kind {
            AssetKind::Video | AssetKind::Audio | AssetKind::Image => {
                crate::media::probe(&dest).ok()
            }
            _ => None,
        };

        let asset = Asset {
            id: crate::util::ids::new_id("asset"),
            path: format!("assets/{unique}"),
            kind,
            bytes: bytes_total,
            imported_at: crate::util::clock::now_rfc3339(),
            sha256,
            info,
            missing: false,
            provenance: Some(origin.to_string()),
        };
        project.add_asset(asset.clone());
        Ok(asset)
    }

    pub fn asset_path(&self, asset: &Asset) -> Result<PathBuf> {
        paths::ensure_within(&self.root, Path::new(&asset.path))
    }

    pub fn relink(&self, project: &mut Project, asset_id: &str, new_source: &Path) -> Result<()> {
        if !new_source.exists() {
            return Err(Error::not_found(format!(
                "{} does not exist",
                new_source.display()
            )));
        }
        // Content-validated like an import: a relinked asset is later rendered straight from this path.
        let info = Some(crate::media::probe_untrusted(new_source)?);
        let asset = project
            .assets
            .iter_mut()
            .find(|a| a.id == asset_id)
            .ok_or_else(|| Error::not_found(format!("asset {asset_id} not found")))?;
        asset.path = new_source.to_string_lossy().to_string();
        asset.info = info;
        asset.missing = false;
        Ok(())
    }

    pub fn refresh_missing(&self, project: &mut Project) -> usize {
        let mut count = 0;
        for asset in project.assets.iter_mut() {
            let path = self.root.join(&asset.path);
            let missing = !path.exists();
            if missing != asset.missing {
                asset.missing = missing;
                count += 1;
            }
        }
        count
    }
}

pub fn sha256_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_and_roundtrips_project() {
        let dir = tempfile::tempdir().unwrap();
        let (store, mut project) = ProjectStore::create(dir.path(), "Test Project").unwrap();
        assert!(store.project_file().exists());
        project.notes.push("hello".into());
        store.save(&mut project).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.name, "Test Project");
        assert_eq!(loaded.notes, vec!["hello".to_string()]);
        assert!(store.revisions().unwrap().len() >= 2);
    }

    #[test]
    fn rollback_restores_previous_state() {
        let dir = tempfile::tempdir().unwrap();
        let (store, mut project) = ProjectStore::create(dir.path(), "Rollback").unwrap();
        project.notes.push("first".into());
        store.save(&mut project).unwrap();
        let first_rev = project.revision;
        project.notes.push("second".into());
        store.save(&mut project).unwrap();
        store.rollback(&mut project, first_rev).unwrap();
        assert_eq!(project.notes, vec!["first".to_string()]);
    }

    #[test]
    fn rejects_paths_outside_project() {
        let store = ProjectStore {
            root: PathBuf::from("/tmp/cinvar-test"),
        };
        let asset = Asset {
            id: "a".into(),
            path: "../../etc/passwd".into(),
            kind: AssetKind::Video,
            bytes: 0,
            imported_at: String::new(),
            sha256: String::new(),
            info: None,
            missing: false,
            provenance: None,
        };
        assert!(store.asset_path(&asset).is_err());
    }
}
