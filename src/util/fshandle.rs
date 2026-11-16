//! Descriptor-relative file access.
//!
//! A path is a name, and a name can be re-pointed between the check and the use: canonicalise,
//! then open by name again, and a hostile writer can swap a component for a symlink or a
//! directory in between (TOCTOU). Everything in this module performs the authoritative check
//! and the open as one kernel operation, so the file that was validated is the file that is
//! read or written:
//!
//! * Linux: `openat2(2)` with `RESOLVE_BENEATH` relative to an already-open directory
//!   descriptor. Any resolution that would leave the root - through `..`, an absolute path or a
//!   symlink planted inside it - fails with `EXDEV`/`ELOOP` instead of opening something else.
//! * Other Unix: a component-by-component `openat(2)` walk with `O_NOFOLLOW`, which cannot be
//!   redirected either. It is stricter than the Linux path: every symlink is refused.
//! * Windows: no equivalent syscall exists here; the fallback is the previous canonicalise +
//!   open. That residual is recorded in `docs/SECURITY_REVIEW_PACKAGE.md` (O-03 / C-02) and is
//!   not claimed to be race-free.
//!
//! Copying follows the same rule: the source is read through a descriptor that was opened once,
//! so a swap of its name mid-import cannot feed different bytes into the hash than into the
//! project, and the destination is created with `O_CREAT|O_EXCL|O_NOFOLLOW` and published with
//! `renameat(2)` inside its own directory.

use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{Error, ErrorCode, Result};

/// Buffer used for the single copy-and-hash pass.
const COPY_BUF: usize = 256 * 1024;

/// A directory that has been opened once and is thereafter addressed by descriptor.
#[derive(Debug)]
pub struct Dir {
    file: File,
    pub path: PathBuf,
    /// The root with its own symlinks resolved once, so containment can be judged against the
    /// same prefix the kernel will use for `RESOLVE_BENEATH`.
    canonical: PathBuf,
}

impl Dir {
    /// Opens `path` as a directory. This is the one place a name is resolved by the caller's
    /// choice; every later operation is relative to the descriptor.
    pub fn open(path: &Path) -> Result<Dir> {
        let file = File::open(path).map_err(|e| {
            Error::new(
                ErrorCode::InvalidInput,
                format!("cannot open directory {}: {e}", path.display()),
            )
        })?;
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        Ok(Dir {
            file,
            path: path.to_path_buf(),
            canonical,
        })
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    /// Opens `candidate` (absolute inside this root, or relative to it) without ever resolving a
    /// name outside the root and without following a symlink that leaves it. Returns the display
    /// path and the open handle: the handle is the authority from here on.
    pub fn open_file(&self, candidate: &Path) -> Result<(PathBuf, File)> {
        let rel = self.relative(candidate)?;
        let attempt = open_beneath(self.file(), &rel);
        // `RESOLVE_BENEATH` refuses absolute symlinks even when they stay inside the root, and an
        // absolute link is ordinary in a media folder. Resolve it exactly once and re-open the
        // result through the descriptor: containment is still decided by the kernel, so the only
        // thing a swap can change is which file *inside* the root is opened - never an escape.
        let attempt = match attempt {
            Err(e) if e.raw_os_error() == Some(libc_exdev()) => {
                match self.reopen_resolved(candidate) {
                    Some(ok) => Ok(ok),
                    None => Err(e),
                }
            }
            other => other,
        };
        let file = attempt.map_err(|e| {
            Error::new(
                ErrorCode::PermissionDenied,
                format!(
                    "{} cannot be opened inside {}: {e}",
                    candidate.display(),
                    self.path.display()
                ),
            )
            .with_hint("only files that really live inside the project (or a configured media root) can be touched; a symlink that points elsewhere is refused")
        })?;
        Ok((self.path.join(&rel), file))
    }

    /// Resolves an absolute in-root symlink and re-opens the real path beneath the descriptor.
    /// Returns `None` when the link leaves the root, which is the case the caller must refuse.
    fn reopen_resolved(&self, candidate: &Path) -> Option<File> {
        let abs = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.path.join(candidate)
        };
        let real = abs.canonicalize().ok()?;
        let rel = real.strip_prefix(&self.canonical).ok()?;
        if rel.as_os_str().is_empty() {
            return None;
        }
        open_beneath(self.file(), rel).ok()
    }

    /// Splits the boundary check from the open in name only: the components handed to the kernel
    /// below are the same ones that were validated, relative to the already-open root.
    fn relative(&self, candidate: &Path) -> Result<PathBuf> {
        let root = crate::util::paths::normalize(&self.path);
        let abs = if candidate.is_absolute() {
            crate::util::paths::normalize(candidate)
        } else {
            crate::util::paths::normalize(&root.join(candidate))
        };
        let rel = abs.strip_prefix(&root).map_err(|_| {
            Error::new(
                ErrorCode::PermissionDenied,
                format!(
                    "path escapes the boundary {}: {}",
                    root.display(),
                    abs.display()
                ),
            )
            .with_hint(
                "only files inside the project can be touched; import media into the project first",
            )
        })?;
        if rel.as_os_str().is_empty() {
            return Err(Error::invalid("a directory is not a file"));
        }
        Ok(rel.to_path_buf())
    }
}

/// Opens one user-named file exactly once and returns the handle plus its display path. Used by
/// the CLI import path, where the human named the file and following their own symlink is not an
/// attack - but reading the name twice still is.
pub fn open_once(path: &Path) -> Result<(PathBuf, File)> {
    let file = File::open(path).map_err(|e| {
        Error::new(
            ErrorCode::NotFound,
            format!("cannot open {}: {e}", path.display()),
        )
    })?;
    Ok((path.to_path_buf(), file))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// A copy that has been written but not yet published: the caller decides the final name from
/// the hash, and an abandoned staging copy removes itself.
pub struct Staged {
    dir: File,
    #[cfg(not(unix))]
    dir_path: PathBuf,
    tmp: String,
    file: File,
    pub sha256: String,
    pub bytes: u64,
    published: bool,
}

impl Staged {
    pub fn file(&self) -> &File {
        &self.file
    }

    /// The name the staged bytes currently live under inside the directory (for content probes
    /// that need a path rather than a descriptor).
    pub fn tmp_name(&self) -> &str {
        &self.tmp
    }

    /// Publishes the staged bytes under `name` inside the same directory. Any directory entry
    /// already sitting there - including a planted symlink - is removed through the descriptor
    /// first, so the write can never be redirected outside the directory.
    pub fn publish(mut self, dir: &Dir, name: &str) -> Result<()> {
        check_name(name)?;
        #[cfg(unix)]
        {
            remove_if_present(dir.file(), name);
            rename_within(dir.file(), &self.tmp, name)?;
        }
        #[cfg(not(unix))]
        {
            let from = dir.path.join(&self.tmp);
            crate::util::paths::unlink_if_symlink(&dir.path.join(name));
            std::fs::rename(&from, dir.path.join(name))?;
        }
        self.published = true;
        Ok(())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        #[cfg(unix)]
        {
            let _ = &self.dir;
            remove_if_present(&self.dir, &self.tmp);
        }
        #[cfg(not(unix))]
        {
            let _ = std::fs::remove_file(self.dir_path.join(&self.tmp));
        }
    }
}

fn check_name(name: &str) -> Result<()> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        return Err(Error::invalid(format!(
            "refusing unsafe asset name: {name}"
        )));
    }
    Ok(())
}

/// Copies `src` into `dir` without publishing it and reports the hash of exactly those bytes.
pub fn stage_into(dir: &Dir, src: &mut File) -> Result<Staged> {
    let tmp = format!(
        ".incoming-{}-{}",
        std::process::id(),
        crate::util::ids::new_id("t").replace(['.', '/'], "-")
    );
    #[cfg(unix)]
    {
        let raw = create_exclusive(dir.file(), &tmp)?;
        let mut out = File::from(raw);
        let (sha256, bytes) = copy_and_hash(src, &mut out)?;
        out.sync_all()?;
        let file = out.try_clone()?;
        drop(out);
        Ok(Staged {
            dir: dir.file().try_clone()?,
            tmp,
            file,
            sha256,
            bytes,
            published: false,
        })
    }
    #[cfg(not(unix))]
    {
        let path = dir.path.join(&tmp);
        let mut out = File::create(&path)?;
        let (sha256, bytes) = copy_and_hash(src, &mut out)?;
        out.sync_all()?;
        let file = out.try_clone()?;
        drop(out);
        Ok(Staged {
            dir: dir.file().try_clone()?,
            dir_path: dir.path.clone(),
            tmp,
            file,
            sha256,
            bytes,
            published: false,
        })
    }
}

/// Copies `src` (an already-open descriptor) into `dir`, hashing the same bytes in the same
/// pass, and publishes it atomically under `name`. Returns `(sha256, bytes_copied)`.
///
/// The temporary file is created inside the destination directory with `O_EXCL` so a planted
/// symlink cannot redirect the write, and `renameat` inside that directory is the only
/// publication step: the destination is either absent or complete.
pub fn copy_hash_into(dir: &Dir, name: &str, src: &mut File) -> Result<(String, u64)> {
    let staged = stage_into(dir, src)?;
    let sha = staged.sha256.clone();
    let bytes = staged.bytes;
    staged.publish(dir, name)?;
    Ok((sha, bytes))
}

/// `EXDEV` is what `RESOLVE_BENEATH` returns for a resolution it refused.
#[cfg(unix)]
fn libc_exdev() -> i32 {
    libc::EXDEV
}

#[cfg(not(unix))]
fn libc_exdev() -> i32 {
    18
}

/// Reads `src` from its current offset to the end, writing every byte to `out` and hashing the
/// identical bytes. One pass, one descriptor: the recorded hash cannot describe different bytes
/// than the ones stored.
pub fn copy_and_hash(src: &mut File, out: &mut File) -> Result<(String, u64)> {
    use std::io::{Read, Seek, Write};
    src.seek(io::SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; COPY_BUF];
    let mut total: u64 = 0;
    loop {
        let read = src.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
        out.write_all(&buf[..read])?;
        total += read as u64;
    }
    Ok((format!("{:x}", hasher.finalize()), total))
}

/// Size and kind of an open descriptor. Using `fstat` on the handle means the size that is
/// recorded belongs to the bytes that are copied.
pub fn handle_len(file: &File) -> Result<u64> {
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(Error::new(ErrorCode::InvalidInput, "not a regular file"));
    }
    Ok(meta.len())
}

pub fn sha256_of_handle(file: &mut File) -> Result<String> {
    use std::io::{Read, Seek};
    file.seek(io::SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; COPY_BUF];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn sha256_of_bytes(bytes: &[u8]) -> String {
    sha256_hex(bytes)
}

// ---------------------------------------------------------------------------------------------
// Platform layer: the only place that talks to the kernel about directory-relative opens.
// ---------------------------------------------------------------------------------------------

#[cfg(all(target_os = "linux", unix))]
fn open_beneath(dir: &File, rel: &Path) -> io::Result<File> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::{AsRawFd, FromRawFd};

    // open_how / RESOLVE_* are from linux/openat2.h; spelled out so the build does not depend on
    // a libc version having exported them.
    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }
    const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
    const RESOLVE_BENEATH: u64 = 0x08;

    let c = CString::new(rel.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    let how = OpenHow {
        flags: (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOCTTY) as u64,
        mode: 0,
        resolve: RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS,
    };
    // SAFETY: fd, path and struct are valid for the call; the struct is repr(C) with the exact
    // layout of struct open_how and the size passed matches it.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dir.as_raw_fd() as libc::c_long,
            c.as_ptr() as *const libc::c_char,
            &how as *const OpenHow,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if rc >= 0 {
        // SAFETY: openat2 returned a fresh descriptor we own.
        return Ok(unsafe { File::from_raw_fd(rc as libc::c_int) });
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        // Kernel without openat2 (pre-5.6) or a seccomp filter that hides it: the component walk
        // is equally unforgeable, just stricter.
        Some(libc::ENOSYS) | Some(libc::EPERM) => walk_no_symlinks(dir, rel),
        // EXDEV: RESOLVE_BENEATH refused an escape. ELOOP: a symlink loop or magic link.
        _ => Err(err),
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn open_beneath(dir: &File, rel: &Path) -> io::Result<File> {
    walk_no_symlinks(dir, rel)
}

#[cfg(not(unix))]
fn open_beneath(dir: &File, _rel: &Path) -> io::Result<File> {
    let _ = dir;
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "descriptor-relative opens are not available on this platform",
    ))
}

/// Component-by-component `openat` with `O_NOFOLLOW`: no component can be a symlink, so no name
/// can be redirected to a different file between the check and the use.
#[cfg(unix)]
fn walk_no_symlinks(dir: &File, rel: &Path) -> io::Result<File> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::{AsRawFd, FromRawFd};

    let mut parts: Vec<CString> = Vec::new();
    for comp in rel.components() {
        match comp {
            Component::Normal(name) => {
                if name == "." || name == ".." {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "relative traversal is not allowed",
                    ));
                }
                parts.push(
                    CString::new(name.as_bytes())
                        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL byte"))?,
                );
            }
            Component::CurDir => {}
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "absolute or parent components are not allowed",
                ))
            }
        }
    }
    if parts.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty path"));
    }
    let mut cur = dir.try_clone()?;
    for (i, part) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        let mut flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
        if !last {
            flags |= libc::O_DIRECTORY;
        }
        // SAFETY: cur is owned, part is a valid C string.
        let fd = unsafe { libc::openat(cur.as_raw_fd(), part.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a fresh descriptor we own.
        cur = unsafe { File::from_raw_fd(fd) };
    }
    Ok(cur)
}

#[cfg(unix)]
fn create_exclusive(dir: &File, name: &str) -> io::Result<std::os::unix::io::OwnedFd> {
    use std::ffi::CString;
    use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};

    let c =
        CString::new(name).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL byte"))?;
    // SAFETY: dir is open, c is a valid C string, mode applies with O_CREAT.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            c.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fresh descriptor, sole owner.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(unix)]
fn remove_if_present(dir: &File, name: &str) {
    use std::ffi::CString;
    use std::os::unix::io::AsRawFd;

    if let Ok(c) = CString::new(name) {
        // SAFETY: dir is open, c is a valid C string. Failure is ignored: the caller reports the
        // rename error if the name really could not be cleared.
        unsafe {
            libc::unlinkat(dir.as_raw_fd(), c.as_ptr(), 0);
        }
    }
}

#[cfg(unix)]
fn rename_within(dir: &File, from: &str, to: &str) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::io::AsRawFd;

    let a =
        CString::new(from).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL byte"))?;
    let b =
        CString::new(to).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL byte"))?;
    // SAFETY: both names are bare file names relative to the same open directory.
    let rc = unsafe { libc::renameat(dir.as_raw_fd(), a.as_ptr(), dir.as_raw_fd(), b.as_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Writes `bytes` to `path` atomically. The temporary file is created with `O_CREAT|O_EXCL`
/// in the destination directory and the rename replaces the directory entry - so a link planted
/// at either name is replaced, never written through, and a reader sees old-or-new, never half.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        crate::util::paths::ensure_dir(parent)?;
    }
    let parent = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| Error::invalid(format!("not a file path: {}", path.display())))?
        .to_string_lossy()
        .to_string();
    let dir = Dir::open(parent)?;
    let tmp = format!(
        ".incoming-{}-{}",
        std::process::id(),
        crate::util::ids::new_id("w").replace(['.', '/'], "-")
    );
    #[cfg(unix)]
    {
        let raw = create_exclusive(dir.file(), &tmp)?;
        let mut file = File::from(raw);
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        if let Err(e) = rename_within(dir.file(), &tmp, &name) {
            remove_if_present(dir.file(), &tmp);
            return Err(e.into());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let tmp_path = parent.join(&tmp);
        let mut file = File::create(&tmp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }
}

/// Removes a directory entry, but only if it is a symbolic link. Used where a link planted in a
/// project folder must be replaced rather than followed, without a check-then-unlink window.
#[cfg(unix)]
pub fn unlink_symlink_at(dir: &File, name: &str) -> bool {
    use std::ffi::CString;
    use std::os::unix::io::AsRawFd;

    let Ok(c) = CString::new(name) else {
        return false;
    };
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: fstatat fills st; AT_SYMLINK_NOFOLLOW makes the call describe the link itself.
    let rc = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            c.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 || (st.st_mode & libc::S_IFMT) != libc::S_IFLNK {
        return false;
    }
    // SAFETY: dir is open, c is a valid C string.
    unsafe { libc::unlinkat(dir.as_raw_fd(), c.as_ptr(), 0) == 0 }
}

#[cfg(not(unix))]
pub fn unlink_symlink_at(_dir: &File, _name: &str) -> bool {
    false
}

/// Same identity as the descriptor: used to confirm that the file just published under a name is
/// still the file that was created (a publish step must not be silently redirected).
pub fn same_file_as(file: &File, path: &Path) -> bool {
    let Ok(a) = file.metadata() else {
        return false;
    };
    let Ok(b) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn a_candidate_outside_the_root_is_refused_before_the_syscall() {
        let dir = scratch();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let d = Dir::open(&root).unwrap();
        assert!(d.open_file(&dir.path().join("outside.txt")).is_err());
        assert!(d.open_file(Path::new("../outside.txt")).is_err());
        assert!(d.open_file(&root).is_err());
    }

    #[test]
    fn unsafe_publish_names_are_refused() {
        for bad in ["", "..", ".", "a/b", "a\\b"] {
            assert!(check_name(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(check_name("asset-123.mp4").is_ok());
    }

    #[test]
    fn an_abandoned_staging_copy_is_removed() {
        let dir = scratch();
        let assets = dir.path().join("assets");
        std::fs::create_dir_all(&assets).unwrap();
        let payload = dir.path().join("p.bin");
        std::fs::write(&payload, b"payload").unwrap();
        let mut src = File::open(&payload).unwrap();
        let d = Dir::open(&assets).unwrap();
        let staged = stage_into(&d, &mut src).unwrap();
        assert_eq!(staged.bytes, 7);
        assert_eq!(std::fs::read_dir(&assets).unwrap().count(), 1);
        drop(staged);
        assert_eq!(
            std::fs::read_dir(&assets).unwrap().count(),
            0,
            "an unpublished staging file must not survive"
        );
    }

    #[test]
    fn a_published_copy_matches_its_hash() {
        let dir = scratch();
        let assets = dir.path().join("assets");
        std::fs::create_dir_all(&assets).unwrap();
        let payload = dir.path().join("p.bin");
        std::fs::write(&payload, vec![0x11u8; 4096]).unwrap();
        let mut src = File::open(&payload).unwrap();
        let d = Dir::open(&assets).unwrap();
        let (sha, bytes) = copy_hash_into(&d, "out.bin", &mut src).unwrap();
        assert_eq!(bytes, 4096);
        assert_eq!(
            std::fs::read(assets.join("out.bin")).unwrap(),
            vec![0x11u8; 4096]
        );
        assert_eq!(sha, crate::project::store::sha256_file(&payload).unwrap());
    }
}
