#![cfg(unix)]
//! Adversarial tests for the agent filesystem boundary.
//!
//! The property under test is not "the parser rejects this string" but "the file that was
//! validated is the file that is read or written". Every test below attacks the window between a
//! boundary check and the later open of the same name: a name swapped for a symlink, a parent
//! directory swapped for a symlink, a payload changed in place between the hash and the copy.

use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cinvar::project::store::{sha256_file, ProjectStore};
use cinvar::util::fshandle;

fn project(root: &Path) -> (ProjectStore, cinvar::project::model::Project) {
    ProjectStore::create(root, "race").expect("create project")
}

/// The importer records a hash and a size for the asset it stores. Those two numbers are what
/// every later render, dedupe and provenance check trusts, so they must describe the very bytes
/// that ended up in the project - even when another process rewrites the source name while the
/// import is running.
#[test]
fn the_recorded_hash_and_size_always_describe_the_stored_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let (store, mut proj) = project(dir.path());
    let src_root = dir.path().join("src");
    std::fs::create_dir_all(&src_root).unwrap();

    // Two distinct payloads, different content and different length.
    let payload_a = vec![0xA5u8; 300 * 1024];
    let payload_b = vec![0x5Au8; 700 * 1024];
    let a = src_root.join("a.bin");
    let b = src_root.join("b.bin");
    std::fs::write(&a, &payload_a).unwrap();
    std::fs::write(&b, &payload_b).unwrap();
    let src = src_root.join("clip.mp4");
    std::fs::hard_link(&a, &src).unwrap();

    let done = Arc::new(AtomicBool::new(false));
    let attacker = {
        let done = done.clone();
        let (src, a, b) = (src.clone(), a.clone(), b.clone());
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                let _ = std::fs::remove_file(&src);
                let _ = std::fs::hard_link(&a, &src);
                let _ = std::fs::remove_file(&src);
                let _ = std::fs::hard_link(&b, &src);
            }
        })
    };

    let mut checked = 0usize;
    let mut mismatches = Vec::new();
    for _ in 0..30 {
        if let Ok(asset) = store.import_asset(&mut proj, &src, "race-test") {
            let stored = store.root.join(&asset.path);
            let got = sha256_file(&stored).unwrap();
            let len = std::fs::metadata(&stored).unwrap().len();
            if got != asset.sha256 || len != asset.bytes {
                mismatches.push(format!(
                    "recorded sha {} / {} bytes, stored sha {} / {} bytes",
                    &asset.sha256[..12],
                    asset.bytes,
                    &got[..12],
                    len
                ));
            }
            checked += 1;
        }
    }
    done.store(true, Ordering::Relaxed);
    attacker.join().unwrap();

    assert!(checked > 0, "no import completed, the test proved nothing");
    assert!(
        mismatches.is_empty(),
        "the stored bytes are not the bytes that were hashed ({} of {} imports): {:?}",
        mismatches.len(),
        checked,
        &mismatches[..mismatches.len().min(3)]
    );
}

/// A directory inside the project replaced by a symlink that points elsewhere must not let an
/// open reach the outside. This is the primitive the media tools use.
#[test]
fn a_parent_swapped_for_a_symlink_cannot_redirect_an_open() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir_all(root.join("tmp")).unwrap();
    std::fs::write(root.join("tmp/clip.mp4"), b"inside-the-project").unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("clip.mp4"), b"OUTSIDE-SECRET").unwrap();

    let d = fshandle::Dir::open(&root).unwrap();
    let (display, mut f) = d.open_file(&root.join("tmp/clip.mp4")).unwrap();
    assert!(display.starts_with(&root));
    let mut text = String::new();
    f.read_to_string(&mut text).unwrap();
    assert_eq!(text, "inside-the-project");

    // Swap the parent directory for a symlink to the outside and try the same name again.
    std::fs::remove_dir_all(root.join("tmp")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("tmp")).unwrap();
    let err = d.open_file(&root.join("tmp/clip.mp4"));
    assert!(
        err.is_err(),
        "a symlinked parent redirected the open outside the root"
    );

    // A name that is lexically outside the root is refused before any syscall.
    let err = d.open_file(&outside.join("clip.mp4"));
    assert!(err.is_err(), "a path outside the root was accepted");
}

/// Publication must not follow a link planted under the destination name: the write happens in a
/// temporary file created with O_EXCL and is renamed into place inside the same directory.
#[test]
fn a_link_planted_under_the_asset_name_is_not_written_through() {
    let dir = tempfile::tempdir().unwrap();
    let assets = dir.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    let victim = dir.path().join("victim.txt");
    std::fs::write(&victim, b"do-not-touch").unwrap();
    std::os::unix::fs::symlink(&victim, assets.join("planted.mp4")).unwrap();

    let payload = dir.path().join("payload.bin");
    std::fs::write(&payload, vec![7u8; 64 * 1024]).unwrap();
    let mut src = std::fs::File::open(&payload).unwrap();

    let d = fshandle::Dir::open(&assets).unwrap();
    let (sha, bytes) = fshandle::copy_hash_into(&d, "planted.mp4", &mut src).unwrap();
    assert_eq!(bytes, 64 * 1024);
    assert_eq!(sha, sha256_file(&payload).unwrap());

    // The victim is untouched and the published name is now a real file with the payload.
    let victim_now = std::fs::read(&victim).unwrap();
    assert_eq!(
        victim_now, b"do-not-touch",
        "a planted link was written through"
    );
    let meta = std::fs::symlink_metadata(assets.join("planted.mp4")).unwrap();
    assert!(!meta.file_type().is_symlink(), "the link was not replaced");
    assert_eq!(
        std::fs::metadata(assets.join("planted.mp4")).unwrap().len(),
        64 * 1024
    );
}

/// Legitimate workflows keep working: importing a real media file twice dedupes to one asset, and
/// a symlink that stays inside the allowed root is still usable.
#[test]
fn legitimate_imports_and_dedupe_still_work() {
    let dir = tempfile::tempdir().unwrap();
    let (store, mut proj) = project(dir.path());
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).unwrap();
    let real = media.join("real.mp4");
    std::fs::write(&real, b"not really a movie but importable by extension").unwrap();

    let first = store.import_asset(&mut proj, &real, "test").unwrap();
    let second = store.import_asset(&mut proj, &real, "test").unwrap();
    assert_eq!(
        first.id, second.id,
        "identical bytes must dedupe to one asset"
    );
    assert_eq!(first.sha256, sha256_file(&real).unwrap());
    let stored = store.root.join(&first.path);
    assert!(stored.exists());
    assert_eq!(std::fs::metadata(&stored).unwrap().len(), first.bytes);

    // A symlink to a file that is already inside the project is resolved and accepted.
    let link = store.root.join("tmp").join("alias.mp4");
    std::os::unix::fs::symlink(&stored, &link).unwrap();
    let d = fshandle::Dir::open(&store.root).unwrap();
    let (_, mut f) = d.open_file(&link).unwrap();
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).unwrap();
    assert_eq!(buf, b"not really a movie but importable by extension");
}

/// The media policy is applied to the copied bytes inside the project, and a rejected import must
/// leave nothing behind: the staged copy removes itself.
#[test]
fn a_rejected_import_leaves_no_staged_bytes_in_the_project() {
    use cinvar::project::store::ImportPolicy;
    let dir = tempfile::tempdir().unwrap();
    let (store, mut proj) = project(dir.path());
    let junk = dir.path().join("disguised.mp4");
    std::fs::write(&junk, b"password=hunter2").unwrap();
    let handle = std::fs::File::open(&junk).unwrap();
    let err = store
        .import_handle(
            &mut proj,
            handle,
            "disguised.mp4",
            "test",
            ImportPolicy::MediaOnly,
        )
        .expect_err("junk must not import as media");
    assert!(
        err.message.contains("not importable as media"),
        "unexpected message: {}",
        err.message
    );
    assert!(
        !err.message.contains(".incoming-"),
        "the failure message leaked an internal staging path: {}",
        err.message
    );
    let left: Vec<_> = std::fs::read_dir(store.assets_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        left.is_empty(),
        "rejected import left files behind: {left:?}"
    );
}

/// A symlink planted at the destination of an atomic write must be replaced, not written through,
/// and a symlink at a name that looks like an internal temp must not be usable either.
#[test]
fn an_atomic_write_never_follows_a_planted_link() {
    let dir = tempfile::tempdir().unwrap();
    let victim = dir.path().join("victim.txt");
    std::fs::write(&victim, b"precious").unwrap();
    let target = dir.path().join("project.json");
    std::os::unix::fs::symlink(&victim, &target).unwrap();

    cinvar::util::paths::write_atomic(&target, b"{\"ok\":true}").unwrap();
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
    assert!(!std::fs::symlink_metadata(&target)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(std::fs::read(&target).unwrap(), b"{\"ok\":true}");

    // The previous implementation derived a predictable temp name from the target
    // ("<name>.<ext>.<pid>.<counter>.tmp") and created it with File::create, which follows a
    // symlink. Plant links at every name that counter could produce: a writer that reuses one of
    // them writes straight into the victim, which is what must no longer be possible.
    let pid = std::process::id();
    let mut planted_any = false;
    for n in 0..16 {
        let planted = dir.path().join(format!("project.json.{}.{}.tmp", pid, n));
        if std::os::unix::fs::symlink(&victim, &planted).is_ok() {
            planted_any = true;
        }
    }
    assert!(planted_any, "could not plant the probed temp names");
    cinvar::util::paths::write_atomic(&target, b"{\"ok\":2}").unwrap();
    assert_eq!(
        std::fs::read(&victim).unwrap(),
        b"precious",
        "a write went through a planted temp-name link"
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"{\"ok\":2}");
}
