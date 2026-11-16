use std::collections::BTreeMap;
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

/// Where a recorded helper path came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperOrigin {
    /// The adapter's environment variable: user authority, and it wins.
    Environment,
    /// The path `cinvar setup --pro-ai` recorded in `Config.helpers`.
    Config,
}

impl HelperOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            HelperOrigin::Environment => "environment",
            HelperOrigin::Config => "config",
        }
    }
}

/// Resolves a helper path with the precedence every adapter applies: the explicit environment
/// variable first (user authority), then the path recorded in the config, then nothing — which
/// leaves the caller with whatever it does on `PATH`.
///
/// Pure so the precedence is testable against a temp config without touching process globals.
pub fn resolve_helper(
    env_value: Option<&str>,
    helpers: &BTreeMap<String, String>,
    key: &str,
) -> Option<(PathBuf, HelperOrigin)> {
    let present = |v: &str| {
        let trimmed = v.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(expand_tilde(trimmed))
        }
    };
    if let Some(path) = env_value.and_then(present) {
        return Some((path, HelperOrigin::Environment));
    }
    helpers
        .get(key)
        .and_then(|v| present(v))
        .map(|path| (path, HelperOrigin::Config))
}

/// `resolve_helper` against this process: the environment, then the config file at
/// `config_dir()/config.toml` (`CINVAR_CONFIG_DIR` overrides the directory).
pub fn helper_path(key: &str) -> Option<(PathBuf, HelperOrigin)> {
    let env = std::env::var(key).ok();
    let helpers = crate::config::Config::load()
        .map(|cfg| cfg.helpers)
        .unwrap_or_default();
    resolve_helper(env.as_deref(), &helpers, key)
}

/// The value a child process should see for `key`: the environment first, then the helper path
/// the config recorded under the same name. `None` leaves the variable unset for the child.
pub fn helper_value(key: &str) -> Option<String> {
    helper_path(key).map(|(path, _)| path.to_string_lossy().to_string())
}

/// Directory of stand-ins for paths an ffmpeg filtergraph cannot name.
pub fn filterpath_dir() -> PathBuf {
    data_dir().join("filterpaths")
}

/// True when ffmpeg's filtergraph option syntax cannot carry this path.
///
/// Measured on ffmpeg 6.1.1: a filtergraph option value is parsed twice (graph, then filter) and
/// neither layer can escape a `'` inside `'...'`, so a font path holding an apostrophe is
/// silently dropped — the frame renders in a different face — and a subtitle path fails to open
/// altogether. Control characters cannot be carried either. A backslash is a separator on
/// Windows but a literal name character elsewhere, so rewriting it changes the path unless the
/// platform meant it that way.
pub fn filtergraph_inexpressible(path: &Path) -> bool {
    let text = path.to_string_lossy();
    if text.chars().any(|c| c == '\'' || c.is_control()) {
        return true;
    }
    !cfg!(windows) && text.contains('\\') && path.is_file()
}

/// Copies `src` to a quote-free name under `root` and returns that path.
///
/// The file name is the sha256 of the source path plus its extension, so distinct sources never
/// collide and one source is copied once. A copy whose length no longer matches the source is
/// refreshed, so a cache can never serve a stale file.
pub fn stage_in(root: &Path, src: &Path) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(src.to_string_lossy().as_bytes());
    let digest: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .map(|e| format!(".{}", e.to_ascii_lowercase()))
        .unwrap_or_default();
    let staged = root.join(format!("{digest}{ext}"));
    let current = match (std::fs::metadata(src), std::fs::metadata(&staged)) {
        (Ok(source), Ok(copy)) => copy.is_file() && copy.len() == source.len(),
        _ => false,
    };
    if !current {
        ensure_dir(root)?;
        std::fs::copy(src, &staged)?;
    }
    Ok(staged)
}

/// stage_in under the data directory cache (filterpaths), falling back to the temp directory when
/// that cache cannot be written or its own path holds a character the filtergraph cannot carry
/// (a home directory named o'brien makes the cache useless).
pub fn stage_for_filtergraph(src: &Path) -> Result<PathBuf> {
    let mut last_error = None;
    for root in [
        filterpath_dir(),
        std::env::temp_dir().join("cinvar-filterpaths"),
    ] {
        match stage_in(&root, src) {
            Ok(staged) if !filtergraph_inexpressible(&staged) => return Ok(staged),
            // This root works but cannot itself be named in a filtergraph: try the next one.
            Ok(_) => {}
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        Error::new(
            ErrorCode::Render,
            format!(
                "no directory without a quote is available to stage {}",
                src.display()
            ),
        )
        .with_hint("move the data directory or TMPDIR to a path without an apostrophe")
    }))
}

pub fn ensure_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    Ok(())
}

/// Atomic replace of a whole file. The previous implementation created a predictable temp name
/// with `File::create` (which follows a symlink) and then renamed: a plugin that could plants a
/// link at that name redirected the write outside the project. The work is done by
/// `fshandle::write_atomic`, which creates the temp file exclusively in the destination
/// directory and publishes by rename.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    crate::util::fshandle::write_atomic(path, bytes)
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

    /// The precedence a customer relies on: export wins, then what `setup --pro-ai` recorded,
    /// then nothing (the caller keeps its PATH behaviour).
    #[test]
    fn helper_precedence_is_environment_then_config_then_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let mut cfg = crate::config::Config::default();
        cfg.set_helper("CINVAR_DEMUCS_PYTHON", "/recorded/venv/bin/python");
        cfg.save_to(&config_path).unwrap();
        let cfg = crate::config::Config::load_from(&config_path).unwrap();
        assert_eq!(
            resolve_helper(None, &cfg.helpers, "CINVAR_DEMUCS_PYTHON"),
            Some((
                PathBuf::from("/recorded/venv/bin/python"),
                HelperOrigin::Config
            ))
        );
        assert_eq!(
            resolve_helper(
                Some("/exported/venv/bin/python"),
                &cfg.helpers,
                "CINVAR_DEMUCS_PYTHON"
            ),
            Some((
                PathBuf::from("/exported/venv/bin/python"),
                HelperOrigin::Environment
            )),
            "an exported variable is user authority and beats the config"
        );
        assert_eq!(
            resolve_helper(Some("   "), &cfg.helpers, "CINVAR_DEMUCS_PYTHON"),
            Some((
                PathBuf::from("/recorded/venv/bin/python"),
                HelperOrigin::Config
            )),
            "a blank export is not a value"
        );
        assert_eq!(
            resolve_helper(None, &cfg.helpers, "CINVAR_ESRGAN_PYTHON"),
            None,
            "an unrecorded key leaves the adapter on its PATH behaviour"
        );
        // A tilde is expanded rather than handed to a child as a literal.
        assert_eq!(
            resolve_helper(Some("~/venv/bin/python"), &cfg.helpers, "X"),
            Some((expand_tilde("~/venv/bin/python"), HelperOrigin::Environment))
        );
    }

    /// Only what ffmpeg genuinely cannot carry is staged. Everything else keeps its real path, so
    /// a staged copy is never created for a path that already works.
    #[test]
    fn only_unexpressible_paths_need_staging() {
        assert!(filtergraph_inexpressible(Path::new("/tmp/it's/face.ttf")));
        let newline = PathBuf::from(format!("/tmp/line{}break.ttf", '\n'));
        assert!(filtergraph_inexpressible(&newline));
        // Verified against ffmpeg 6.1.1: these are carried by the filtergraph and render the
        // intended face, so they must not be staged.
        for ok in [
            "/tmp/Fonts With Spaces/DejaVu Sans.ttf",
            "/tmp/a,b/face.ttf",
            "/tmp/hellö/face.ttf",
            "/tmp/视频/face.ttf",
            "C:/Windows/Fonts/arial.ttf",
        ] {
            assert!(
                !filtergraph_inexpressible(Path::new(ok)),
                "{ok} is expressible"
            );
        }
        // A backslash is a separator on Windows and a literal character here, where rewriting it
        // silently points at a path that does not exist.
        if !cfg!(windows) {
            let dir = tempfile::tempdir().unwrap();
            let weird = dir.path().join("back\\slash.ttf");
            std::fs::write(&weird, b"x").unwrap();
            assert!(filtergraph_inexpressible(&weird));
        }
    }

    #[test]
    fn staging_copies_to_a_quote_free_name_and_refreshes_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("filterpaths");
        let source_dir = dir.path().join("it's a font dir");
        std::fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join("DejaVu Serif.ttf");
        std::fs::write(&source, vec![7u8; 64]).unwrap();

        let staged = stage_in(&root, &source).unwrap();
        assert!(staged.is_file());
        assert!(
            !staged.to_string_lossy().contains('\''),
            "the staged path is quote-free: {}",
            staged.display()
        );
        assert_eq!(staged.extension().and_then(|e| e.to_str()), Some("ttf"));
        let name = staged.file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(name.len(), 64 + 4, "sha256 hex plus extension, got {name}");
        assert_eq!(std::fs::read(&staged).unwrap(), vec![7u8; 64]);
        // The same source maps to the same stand-in.
        assert_eq!(stage_in(&root, &source).unwrap(), staged);
        // ...and a changed source is copied again instead of leaving a stale stand-in.
        std::fs::write(&source, vec![9u8; 128]).unwrap();
        let refreshed = stage_in(&root, &source).unwrap();
        assert_eq!(refreshed, staged);
        assert_eq!(std::fs::read(&refreshed).unwrap(), vec![9u8; 128]);
    }
}
