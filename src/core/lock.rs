//! Cross-process advisory file locks for parallel `/swarm` edits.
//! Lock files live in the system temp dir so `git status` stays clean.

use crate::core::cwd::{key, simplify};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// Canonical form of a path that may not exist yet (new files).
fn canonical(path: &Path) -> PathBuf {
    if let Ok(p) = path.canonicalize() {
        return simplify(p);
    }
    match (path.parent().and_then(|p| p.canonicalize().ok()), path.file_name()) {
        (Some(dir), Some(name)) => simplify(dir).join(name),
        _ => std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()),
    }
}

/// Take an exclusive lock for `path`, blocking until it is free.
/// The lock is released when the returned handle is dropped (or the process dies).
pub fn acquire(path: &Path) -> io::Result<File> {
    // FNV-1a: stable across builds, unlike std's hasher; a collision only over-locks.
    let hash = key(&canonical(path))
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    let dir = std::env::temp_dir().join("agyfast-rs-locks");
    std::fs::create_dir_all(&dir)?;
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
