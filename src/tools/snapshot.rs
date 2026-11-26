use crate::core::budget::fold;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

const MAX_BYTES: usize = 1500;
const MAX_STDIN: usize = 256 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_millis(800);
const SKIP_DIRS: &[&str] = &["node_modules", "target", "dist", "build", "out", "venv", "__pycache__", "vendor"];

fn git_child(cwd: &Path, args: &[&str]) -> Option<std::process::Child> {
    Command::new("git")
        .args(["--no-optional-locks", "-c", "core.quotePath=false", "--no-pager"])
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

fn git_finish(child: Option<std::process::Child>) -> Option<String> {
    let mut child = child?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.by_ref().take(64 * 1024).read_to_string(&mut s);
        s
    });
    match child.wait_timeout(GIT_TIMEOUT) {
        Ok(Some(st)) if st.success() => reader.join().ok(),
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

/// Branch, dirty/untracked counts and the last commit subjects; `None` outside a repo.
fn git_section(cwd: &Path) -> Option<String> {
    cwd.ancestors().find(|d| d.join(".git").exists())?;
    // Both git processes run concurrently.
    let status = git_child(cwd, &["status", "--porcelain=v1", "--branch"]);
    let log = git_child(cwd, &["log", "-n", "3", "--format=%h %s"]);
    let status = git_finish(status);
    let log = git_finish(log);
    let mut text = String::from("Git: ");
    match status {
        Some(s) => {
            let mut lines = s.lines();
            let head = lines.next().unwrap_or("").trim_start_matches("## ");
            let branch = head.split("...").next().unwrap_or(head);
            let branch = branch.strip_prefix("No commits yet on ").unwrap_or(branch);
            let (mut dirty, mut untracked) = (0, 0);
            for l in lines {
                if l.starts_with("??") { untracked += 1 } else if !l.trim().is_empty() { dirty += 1 }
            }
            text += &format!("branch `{branch}`, {dirty} dirty, {untracked} untracked");
        }
        None => text += "status unavailable",
    }
    for l in log.iter().flat_map(|l| l.lines()) {
        text += &format!("\n  - {}", fold(l, 72));
    }
    Some(text)
}

fn read(cwd: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(cwd.join(name)).ok()
}

fn stacks(cwd: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(t) = read(cwd, "Cargo.toml").and_then(|t| t.parse::<toml::Table>().ok()) {
        let name = t.get("package").and_then(|p| p.get("name")).and_then(|n| n.as_str());
        let mut s = format!("Cargo.toml ({})", name.unwrap_or("workspace"));
        if let Some(m) = t.get("workspace").and_then(|w| w.get("members")).and_then(|m| m.as_array()) {
            let list: Vec<_> = m.iter().filter_map(|v| v.as_str()).take(6).collect();
            s += &format!(" members: {}{}", list.join(", "), if m.len() > 6 { ", ..." } else { "" });
        }
        out.push(s);
    }
    if let Some(Value::Object(p)) = read(cwd, "package.json").and_then(|t| serde_json::from_str(&t).ok()) {
        let mut s = format!("package.json ({})", p.get("name").and_then(Value::as_str).unwrap_or("?"));
        if let Some(Value::Object(sc)) = p.get("scripts") {
            let names: Vec<_> = sc.keys().take(8).map(String::as_str).collect();
            s += &format!(" scripts: {}", names.join(", "));
        }
        out.push(s);
    }
    if let Some(t) = read(cwd, "pyproject.toml").and_then(|t| t.parse::<toml::Table>().ok()) {
        let name = t.get("project").and_then(|p| p.get("name")).or_else(|| t.get("tool")?.get("poetry")?.get("name"));
        out.push(format!("pyproject.toml ({})", name.and_then(|n| n.as_str()).unwrap_or("?")));
    }
    if let Some(m) = read(cwd, "go.mod").and_then(|t| t.lines().find_map(|l| l.strip_prefix("module ").map(|m| m.trim().to_string()))) {
        out.push(format!("go.mod ({m})"));
    }
    out
}

/// Two-level tree honouring .gitignore: `name/ (n): child, child, ...`.
fn tree(cwd: &Path) -> String {
    let walk = ignore::WalkBuilder::new(cwd)
        .max_depth(Some(2))
        .require_git(false)
        .filter_entry(|e| !(e.file_type().is_some_and(|t| t.is_dir()) && SKIP_DIRS.contains(&e.file_name().to_string_lossy().as_ref())))
        .build();
    let mut top: BTreeMap<String, (bool, Vec<String>)> = BTreeMap::new();
    for e in walk.flatten().filter(|e| e.depth() > 0) {
        let Ok(rel) = e.path().strip_prefix(cwd) else { continue };
        let mut parts = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned());
        let (Some(first), second) = (parts.next(), parts.next()) else { continue };
        let entry = top.entry(first).or_insert((false, Vec::new()));
        match second {
            Some(child) => {
                entry.0 = true;
                entry.1.push(child);
            }
            None => entry.0 |= e.file_type().is_some_and(|t| t.is_dir()),
        }
    }
    let mut lines = Vec::new();
    for (i, (name, (dir, kids))) in top.iter_mut().enumerate() {
        kids.sort();
        if i == 30 {
            lines.push(format!("... +{} more", top.len() - 30));
            break;
        }
        lines.push(match dir {
            true => {
                let shown: Vec<_> = kids.iter().take(5).map(String::as_str).collect();
                let more = if kids.len() > 5 { ", ..." } else { "" };
                format!("{name}/ ({}){}{}", kids.len(), if shown.is_empty() { "" } else { ": " }, shown.join(", ") + more)
            }
            false => name.clone(),
        });
    }
    lines.join("\n")
}

/// Workspace snapshot text (git state, manifests, 2-level tree), at most 1500 bytes.
pub fn snapshot(cwd: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.extend(git_section(cwd));
    parts.extend(stacks(cwd));
    let t = tree(cwd);
    if !t.is_empty() {
        parts.push(format!("Tree:\n{t}"));
    }
    fold(&parts.join("\n"), MAX_BYTES)
}

fn strip_bom(mut bytes: &[u8]) -> &[u8] {
    while let Some(rest) = bytes.strip_prefix(b"\xef\xbb\xbf") {
        bytes = rest;
    }
    bytes
}

fn read_payload() -> Option<Value> {
    if std::io::stdin().is_terminal() {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    // The host may keep stdin open: answer as soon as a complete JSON object arrived.
    std::thread::spawn(move || {
        let (mut buf, mut chunk) = (Vec::new(), [0u8; 8192]);
        let mut stdin = std::io::stdin().lock();
        while let Ok(n) = stdin.read(&mut chunk) {
            buf.extend_from_slice(&chunk[..n]);
            if n == 0 || buf.len() >= MAX_STDIN || serde_json::from_slice::<Value>(strip_bom(&buf)).is_ok() {
                break;
            }
        }
        let _ = tx.send(serde_json::from_slice::<Value>(strip_bom(&buf)).ok());
    });
    rx.recv_timeout(Duration::from_millis(1500)).ok().flatten()
}

const PATH_LISTS: [&str; 4] = ["workspacePaths", "workspace_paths", "workspaceRoots", "workspace_roots"];

fn workspace_dir(p: &Value) -> Option<PathBuf> {
    let first = |k: &str| p.get(k)?.as_array()?.iter().filter_map(Value::as_str).map(str::trim).find(|s| !s.is_empty());
    let dir = p.get("cwd").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
        .or_else(|| PATH_LISTS.iter().find_map(|k| first(k)))?;
    Some(PathBuf::from(dir)).filter(|d| d.is_dir())
}

/// Claude Code sends `hook_event_name` and no workspace lists; Antigravity sends workspace paths.
fn wants_claude_shape(p: &Value) -> bool {
    p.get("hook_event_name").is_some_and(Value::is_string) && !PATH_LISTS.iter().any(|k| p.get(k).is_some())
}

fn response(payload: &Value, text: &str) -> Value {
    if wants_claude_shape(payload) {
        json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": text}})
    } else {
        json!({"injectSteps": [{"systemMessage": {"systemMessage": text}}]})
    }
}

/// `agyfast-rs hook session-start`: read the host payload on stdin, print the hook response.
pub fn hook() {
    let out = read_payload()
        .and_then(|p| {
            let text = snapshot(&workspace_dir(&p)?.canonicalize().ok()?);
            (!text.is_empty()).then(|| response(&p, &text))
        })
        .unwrap_or_else(|| json!({}));
    println!("{out}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn size_cap_holds_with_multibyte_names() {
        let d = dir();
        for i in 0..80 {
            let sub = d.path().join(format!("каталог-{i}-日本語"));
            std::fs::create_dir(&sub).unwrap();
            for j in 0..10 {
                std::fs::write(sub.join(format!("файл-{j}-🌍.txt")), "x").unwrap();
            }
        }
        let s = snapshot(d.path());
        assert!(s.len() <= MAX_BYTES && s.contains("каталог"));
    }

    #[test]
    fn empty_and_non_git_dirs_work() {
        let d = dir();
        assert_eq!(snapshot(d.path()), "");
        std::fs::write(d.path().join("a.txt"), "x").unwrap();
        let s = snapshot(d.path());
        assert!(!s.contains("Git:") && s.contains("a.txt"));
    }

    #[test]
    fn manifests_are_detected_and_build_dirs_skipped() {
        let d = dir();
        let p = d.path();
        std::fs::write(p.join("Cargo.toml"), "[package]\nname = \"demo\"\n[workspace]\nmembers = [\"a\", \"b\"]\n").unwrap();
        std::fs::write(p.join("package.json"), r#"{"name":"web","scripts":{"build":"x","test":"y"}}"#).unwrap();
        std::fs::write(p.join("pyproject.toml"), "[project]\nname = \"py\"\n").unwrap();
        std::fs::write(p.join("go.mod"), "module example.com/g\n\ngo 1.22\n").unwrap();
        std::fs::create_dir_all(p.join("node_modules/x")).unwrap();
        std::fs::create_dir_all(p.join("src")).unwrap();
        std::fs::write(p.join("src/main.rs"), "").unwrap();
        let s = snapshot(p);
        for want in ["Cargo.toml (demo) members: a, b", "package.json (web) scripts: build, test", "pyproject.toml (py)", "go.mod (example.com/g)", "src/ (1): main.rs"] {
            assert!(s.contains(want), "missing {want} in {s}");
        }
        assert!(!s.contains("node_modules"));
    }

    #[test]
    fn git_state_is_reported() {
        let d = dir();
        let p = d.path();
        let git = |a: &[&str]| assert!(Command::new("git").args(a).current_dir(p).output().unwrap().status.success());
        git(&["init", "-q"]);
        std::fs::write(p.join("a.txt"), "x").unwrap();
        git(&["add", "."]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "first commit"]);
        std::fs::write(p.join("b.txt"), "y").unwrap();
        let s = snapshot(p);
        assert!(s.contains("0 dirty, 1 untracked") && s.contains("first commit"), "{s}");
    }

    #[test]
    fn hook_output_shape_follows_the_payload() {
        let d = dir();
        let cwd = d.path().to_str().unwrap();
        let claude = json!({"hook_event_name": "SessionStart", "cwd": cwd});
        assert_eq!(workspace_dir(&claude).unwrap(), d.path());
        assert_eq!(response(&claude, "t")["hookSpecificOutput"]["additionalContext"], "t");
        let agy = json!({"hookEventName": "SessionStart", "workspacePaths": [cwd]});
        assert_eq!(workspace_dir(&agy).unwrap(), d.path());
        assert_eq!(response(&agy, "t")["injectSteps"][0]["systemMessage"]["systemMessage"], "t");
        assert!(workspace_dir(&json!({"cwd": "/definitely/not/here"})).is_none());
        assert!(serde_json::from_slice::<Value>(strip_bom(b"\xef\xbb\xbf\xef\xbb\xbf{\"cwd\":\"/x\"}")).is_ok());
        assert_eq!(strip_bom(b"{\"a\":1}"), b"{\"a\":1}");
    }
}
