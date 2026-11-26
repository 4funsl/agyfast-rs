//! Many concurrent `agy-rs cli edit` processes against one file: no edit may be
//! lost and no line corrupted. Fails without the per-file lock or atomic write.

use std::io::Write;
use std::process::{Command, Stdio};

const WORKERS: usize = 16;
const FILLER: usize = 40_000; // big file: widens the read -> write window so races show up

fn marker(i: usize, state: &str) -> String {
    format!("marker_{i:02} = {state};")
}

fn file_text(done: &[bool]) -> String {
    let mut s = String::new();
    for (i, &d) in done.iter().enumerate() {
        s.push_str(&marker(i, if d { "edited" } else { "pending" }));
        s.push('\n');
        for j in 0..FILLER / WORKERS {
            s.push_str(&format!("filler line {i}-{j} lorem ipsum dolor sit amet\n"));
        }
    }
    s
}

#[test]
fn concurrent_cli_edits_lose_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("shared.txt");
    std::fs::write(&file, file_text(&[false; WORKERS])).unwrap();

    // Spawn everyone first; they block on stdin, so feeding it afterwards starts them together.
    let mut children: Vec<_> = (0..WORKERS)
        .map(|_| {
            Command::new(env!("CARGO_BIN_EXE_agy-rs"))
                .args(["cli", "edit", "--cwd", dir.path().to_str().unwrap(), "--stdin"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let payloads: Vec<String> = (0..WORKERS)
        .map(|i| {
            serde_json::json!({"edits": [{"path": "shared.txt", "hunks": [
                {"target": marker(i, "pending"), "replacement": marker(i, "edited")}
            ]}]})
            .to_string()
        })
        .collect();
    // An unlocked observer must only ever see whole files (atomic rename, never truncate-in-place).
    // Each edit shortens the file by one byte, so any size outside that range is a torn write.
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observer = {
        let (file, done) = (file.clone(), done.clone());
        let full = file_text(&[false; WORKERS]).len() as u64;
        std::thread::spawn(move || {
            let mut torn = 0;
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                let len = std::fs::metadata(&file).map_or(0, |m| m.len());
                torn += usize::from(!(full - WORKERS as u64..=full).contains(&len));
            }
            torn
        })
    };
    for (child, payload) in children.iter_mut().zip(&payloads) {
        child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    }
    for (i, child) in children.into_iter().enumerate() {
        let out = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "worker {i} failed: {stdout}{}", String::from_utf8_lossy(&out.stderr));
        assert!(stdout.contains("shared.txt: updated (1 hunk), lines +0"), "worker {i}: {stdout}");
    }

    done.store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(observer.join().unwrap(), 0, "observer saw partially written files");
    let after = std::fs::read_to_string(&file).unwrap();
    let lost: Vec<usize> = (0..WORKERS).filter(|&i| !after.contains(&marker(i, "edited"))).collect();
    assert!(lost.is_empty(), "lost edits from workers {lost:?}");
    assert!(after == file_text(&[true; WORKERS]), "file content corrupted");
    let leftovers: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(leftovers.len(), 1, "temp files left behind: {leftovers:?}");
}
