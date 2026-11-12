use std::path::{Component, Path, PathBuf};

use crate::error::{Error, ErrorCode, Result};

pub fn home_dir() -> PathBuf {
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    if let Ok(h) = std::env::var("USERPROFILE") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    PathBuf::from(".")
}

pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CINVAR_CONFIG_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("cinvar");
        }
    }
    home_dir().join(".config").join("cinvar")
}

pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CINVAR_DATA_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    if let Ok(dir) = std::env::var("XDG_DATA_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("cinvar");
        }
    }
    home_dir().join(".local").join("share").join("cinvar")
}

pub fn expand_tilde(input: &str) -> PathBuf {
    if let Some(rest) = input.strip_prefix("~/") {
        home_dir().join(rest)
    } else if input == "~" {
        home_dir()
    } else {
        PathBuf::from(input)
    }
}

pub fn absolutize(path: &Path, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize(path)
    } else {
        normalize(&cwd.join(path))
    }
}

pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub fn ensure_within(root: &Path, candidate: &Path) -> Result<PathBuf> {
    let root_norm = normalize(root);
    let abs = if candidate.is_absolute() {
        normalize(candidate)
    } else {
        normalize(&root_norm.join(candidate))
    };
    if !abs.starts_with(&root_norm) {
        return Err(Error::new(
            ErrorCode::PermissionDenied,
            format!("path escapes the project boundary: {}", abs.display()),
        )
        .with_hint("only files inside the project directory can be touched; import media into the project first"));
    }
    // Lexical checks miss symlinks: resolve the deepest existing ancestor and check that too.
    if let Ok(real_root) = root.canonicalize() {
        let mut probe = abs.as_path();
        loop {
            if let Ok(real) = probe.canonicalize() {
                if !real.starts_with(&real_root) {
                    return Err(Error::new(
                        ErrorCode::PermissionDenied,
                        format!(
                            "path resolves outside the project through a symlink: {}",
                            abs.display()
                        ),
                    )
                    .with_hint("only files inside the project directory can be touched"));
                }
                break;
            }
            match probe.parent() {
                Some(parent) => probe = parent,
                None => break,
            }
        }
    }
    Ok(abs)
}

pub fn ensure_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    Ok(())
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Unique per writer (pid + counter): two writers to one target must never share a temp file.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "{}.{}.{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("dat"),
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        // Flush to disk before the rename so a crash never leaves a truncated project file.
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Removes `path` if (and only if) it is a symbolic link, so a following write creates a fresh
/// regular file instead of following a link planted in a hostile project folder or by a plugin.
pub fn unlink_if_symlink(path: &Path) {
    if std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_escape() {
        let root = PathBuf::from("/tmp/proj");
        assert!(ensure_within(&root, Path::new("media/a.mp4")).is_ok());
        assert!(ensure_within(&root, Path::new("../../etc/passwd")).is_err());
        assert!(ensure_within(&root, Path::new("/etc/passwd")).is_err());
    }

    #[test]
    fn normalizes() {
        assert_eq!(normalize(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
    }
}
