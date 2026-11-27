//! Cross-process advisory file locks for parallel `/swarm` edits.
//! Lock files live in the system temp dir so `git status` stays clean.

use crate::core::cwd::key;
use std::fs::File;
use std::io;
use std::path::Path;

#[cfg(unix)]
fn lock_dir() -> io::Result<std::path::PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    // SAFETY: geteuid takes no arguments and has no preconditions.
    let uid = unsafe { libc::geteuid() };
    // Fixed base, not temp_dir(): processes with different TMPDIR must lock the same file.
    let dir = std::path::PathBuf::from(format!("/tmp/agyfast-rs-locks-{uid}"));
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        result => result?,
    }
    let meta = dir.symlink_metadata()?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "lock directory must be private and owned by the current user"));
    }
    Ok(dir)
}

#[cfg(not(unix))]
fn lock_dir() -> io::Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("agyfast-rs-locks");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Take an exclusive lock for `path`, blocking until it is free.
/// `path` must already be canonical (`cwd::canonical`). The lock is released when
/// the returned handle is dropped (or the process dies).
pub fn acquire(path: &Path) -> io::Result<File> {
    // FNV-1a: stable across builds, unlike std's hasher; a collision only over-locks.
    let hash = key(path)
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    let dir = lock_dir()?;
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(format!("{hash:016x}.lock")))?;
    file.lock()?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_path_contends_and_new_files_work() {
        let target = std::env::temp_dir().join("agyfast-rs-lock-test-does-not-exist.txt");
        let held = acquire(&target).unwrap();
        let t = target.clone();
        let waiter = std::thread::spawn(move || acquire(&t).map(|_| std::time::Instant::now()));
        std::thread::sleep(std::time::Duration::from_millis(150));
        let released = std::time::Instant::now();
        drop(held);
        assert!(waiter.join().unwrap().unwrap() >= released);
    }
}
