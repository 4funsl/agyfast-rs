#![cfg(windows)]

use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_agyfast-rs");

fn run_with_stdin(args: &[&str], cwd: &Path, stdin_bytes: &[u8], envs: &[(&str, &std::ffi::OsStr)]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin_bytes).unwrap();
    child.wait_with_output().unwrap()
}

fn bom_json(v: Value) -> Vec<u8> {
    let mut bytes = b"\xef\xbb\xbf\xef\xbb\xbf".to_vec();
    bytes.extend_from_slice(v.to_string().as_bytes());
    bytes
}

#[allow(clippy::permissions_set_readonly_false)]
fn set_readonly(path: &Path, ro: bool) {
    let mut p = fs::metadata(path).unwrap().permissions();
    p.set_readonly(ro);
    fs::set_permissions(path, p).unwrap();
}

#[test]
fn cli_and_hooks_accept_double_utf8_bom_and_enforce_plugin_root() {
    let dir = tempfile::tempdir().unwrap();
    let ro = dir.path().join("ro.txt");
    fs::write(&ro, "old\n").unwrap();
    fs::write(dir.path().join("ro.txt:metadata"), "kept").unwrap();
    set_readonly(&ro, true);

    let edit_payload = bom_json(json!({
        "cwd": dir.path(),
        "edits": [{"path": "ro.txt", "hunks": [{"target": "old", "replacement": "new"}]}]
    }));
    let out = run_with_stdin(&["cli", "edit", "--stdin"], dir.path(), &edit_payload, &[]);
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(fs::read_to_string(&ro).unwrap(), "new\n");
    assert!(fs::metadata(&ro).unwrap().permissions().readonly(), "ReadOnly attribute was lost");
    assert_eq!(fs::read(dir.path().join("ro.txt:metadata")).unwrap(), b"kept");
    set_readonly(&ro, false);

    let hook_ss = bom_json(json!({"hookEventName": "SessionStart", "workspacePaths": [dir.path()]}));
    let ss_out = run_with_stdin(&["hook", "session-start"], dir.path(), &hook_ss, &[]);
    assert!(ss_out.status.success());
    let ss_text = String::from_utf8_lossy(&ss_out.stdout);
    assert!(ss_text.contains("injectSteps"), "{ss_text}");

    let hook_pb = bom_json(json!({"tool_name": "Bash", "tool_input": {"command": "cargo check"}}));
    let pb_out = run_with_stdin(&["hook", "pre-bash"], dir.path(), &hook_pb, &[]);
    assert!(pb_out.status.success());
    let pb_text = String::from_utf8_lossy(&pb_out.stdout);
    assert!(pb_text.contains("PreToolUse"), "{pb_text}");

    let fake_plugin = dir.path().join("fake_plugin");
    fs::create_dir_all(&fake_plugin).unwrap();
    let guard_out = run_with_stdin(
        &["cli", "search", "--text", "foo"],
        &fake_plugin,
        b"",
        &[("PLUGIN_ROOT", fake_plugin.as_os_str())],
    );
    assert!(!guard_out.status.success());
    let combined = format!("{}{}", String::from_utf8_lossy(&guard_out.stdout), String::from_utf8_lossy(&guard_out.stderr));
    assert!(combined.contains("inside the plugin directory"), "{combined}");
}

#[test]
fn edit_preserves_readonly_overwrite_crlf_and_ntfs_case_dedup() {
    let dir = tempfile::tempdir().unwrap();

    let ro_over = dir.path().join("ro_over.txt");
    fs::write(&ro_over, "v1\n").unwrap();
    fs::write(dir.path().join("ro_over.txt:metadata"), "kept").unwrap();
    set_readonly(&ro_over, true);
    let over_out = run_with_stdin(
        &["cli", "edit", "--cwd", dir.path().to_str().unwrap(), "--stdin"],
        dir.path(),
        json!({"edits": [{"path": "ro_over.txt", "create": "v2\n", "overwrite": true}]}).to_string().as_bytes(),
        &[],
    );
    assert!(over_out.status.success(), "{}", String::from_utf8_lossy(&over_out.stdout));
    assert_eq!(fs::read_to_string(&ro_over).unwrap(), "v2\n");
    assert!(fs::metadata(&ro_over).unwrap().permissions().readonly());
    assert_eq!(fs::read(dir.path().join("ro_over.txt:metadata")).unwrap(), b"kept");
    set_readonly(&ro_over, false);

    let crlf = dir.path().join("crlf.txt");
    fs::write(&crlf, b"a\r\nb\r\nc\r\n").unwrap();
    let crlf_out = run_with_stdin(
        &["cli", "edit", "--cwd", dir.path().to_str().unwrap(), "--stdin"],
        dir.path(),
        json!({"edits": [{"path": "crlf.txt", "hunks": [{"target": "b", "replacement": "B1\nB2"}]}]}).to_string().as_bytes(),
        &[],
    );
    assert!(crlf_out.status.success());
    assert_eq!(fs::read(&crlf).unwrap(), b"a\r\nB1\r\nB2\r\nc\r\n");

    let case_file = dir.path().join("CaseTest.txt");
    fs::write(&case_file, "one\ntwo\n").unwrap();
    let merge_out = run_with_stdin(
        &["cli", "edit", "--cwd", dir.path().to_str().unwrap(), "--stdin"],
        dir.path(),
        json!({"edits": [
            {"path": "CaseTest.txt", "hunks": [{"target": "one", "replacement": "ONE"}]},
            {"path": "casetest.txt", "hunks": [{"target": "two", "replacement": "TWO"}]}
        ]})
        .to_string()
        .as_bytes(),
        &[],
    );
    assert!(merge_out.status.success(), "{}", String::from_utf8_lossy(&merge_out.stdout));
    assert_eq!(fs::read_to_string(&case_file).unwrap(), "ONE\nTWO\n");

    let dup_out = run_with_stdin(
        &["cli", "edit", "--cwd", dir.path().to_str().unwrap(), "--stdin"],
        dir.path(),
        json!({"edits": [
            {"path": "DupNew.txt", "create": "a\n"},
            {"path": "dupnew.txt", "create": "b\n"}
        ]})
        .to_string()
        .as_bytes(),
        &[],
    );
    assert!(!dup_out.status.success());
    let dup_text = String::from_utf8_lossy(&dup_out.stdout);
    assert!(dup_text.contains("listed 2 times"), "{dup_text}");
}

#[test]
fn checks_preserve_non_ascii_backslashes_ntstatus_and_posix_sh() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    let sub = dir.path().join("папка");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("marker.txt"), "marker_ok\n").unwrap();

    let non_ascii = Command::new(BIN)
        .args(["cli", "check", "--cwd", cwd, "--full", r"cmd /c type .\папка\marker.txt"])
        .output()
        .unwrap();
    assert!(non_ascii.status.success());
    let na_out = String::from_utf8_lossy(&non_ascii.stdout);
    assert!(na_out.contains("[PASS") && na_out.contains("marker_ok"), "{na_out}");

    let escaped = "echo one ^&& echo two";
    let native = Command::new("cmd").args(["/D", "/C", escaped]).output().unwrap();
    let wrapped = Command::new(BIN).args(["cli", "check", "--cwd", cwd, "--full", escaped]).output().unwrap();
    let report = String::from_utf8_lossy(&wrapped.stdout);
    assert!(native.status.success() && report.contains("[PASS"), "{report}");
    for line in String::from_utf8_lossy(&native.stdout).lines() {
        assert!(report.lines().any(|l| l.trim() == line.trim()), "{report}");
    }

    let nt = Command::new(BIN)
        .args(["cli", "check", "--cwd", cwd, "exit -1073741819", "exit -1073741502"])
        .output()
        .unwrap();
    assert!(nt.status.success());
    let nt_out = String::from_utf8_lossy(&nt.stdout);
    assert!(nt_out.contains("exit 0xC0000005 - access violation"), "{nt_out}");
    assert!(nt_out.contains("exit 0xC0000142 - DLL init failed"), "{nt_out}");

    let has_sh = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).map(|d| d.join("sh.exe")).collect::<Vec<_>>())
        .chain(std::env::var_os("ProgramFiles").into_iter().flat_map(|p| {
            let g = PathBuf::from(p).join("Git");
            [g.join(r"bin\sh.exe"), g.join(r"usr\bin\sh.exe")]
        }))
        .any(|p| p.is_file());
    if has_sh {
        let cur = std::env::var_os("PATH").unwrap_or_default();
        let filtered: Vec<_> = std::env::split_paths(&cur)
            .filter(|p| !p.to_string_lossy().to_ascii_lowercase().contains(r"git\usr\bin"))
            .collect();
        let joined = std::env::join_paths(filtered).unwrap();
        let sh_out = Command::new(BIN)
            .args(["cli", "check", "--cwd", cwd, "--full", "sleep 0", "echo 'hello world'"])
            .env("PATH", joined)
            .output()
            .unwrap();
        assert!(sh_out.status.success());
        let sh_text = String::from_utf8_lossy(&sh_out.stdout);
        assert!(sh_text.contains("Checks: 2/2 passed") && sh_text.contains("hello world"), "{sh_text}");
    }
}

#[test]
fn search_reports_exclusive_lock_alongside_readable_hits() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let locked = dir.path().join("busy.txt");
    fs::write(&locked, "needle\n").unwrap();
    fs::write(dir.path().join("match.txt"), "needle\n").unwrap();
    fs::write(dir.path().join("binary"), b"needle\0").unwrap();
    let _handle = fs::OpenOptions::new().read(true).share_mode(0).open(locked).unwrap();
    for mode in ["content", "files_with_matches", "count"] {
        let result = run_with_stdin(&["cli", "search", "--output-mode", mode, "--text", "needle"], dir.path(), b"", &[]);
        let text = String::from_utf8_lossy(&result.stdout);
        assert!(result.status.success() && text.contains("Incomplete search: 1 unreadable file") && text.contains("match.txt") && !text.contains("binary"), "{text}");
    }
}

#[test]
fn bash_hook_chain_follows_filter_verdict() {
    let bash = std::env::var_os("ProgramFiles").map(PathBuf::from).map(|d| d.join(r"Git\bin\bash.exe"));
    let Some(bash) = bash.filter(|p| p.is_file()) else { return };
    let dir = tempfile::tempdir().unwrap();
    let input = json!({"tool_name": "Bash", "tool_input": {"command": "pytest && touch continued"}});
    let hook = run_with_stdin(&["hook", "pre-bash"], dir.path(), input.to_string().as_bytes(), &[]);
    let response: Value = serde_json::from_slice(&hook.stdout).unwrap();
    let command = response["hookSpecificOutput"]["updatedInput"]["command"].as_str().unwrap();
    // The filter judges the output: the pipeline's status is its verdict, not the child's exit code.
    for (body, code, verdict) in [("printf '1 failed'; return 5", 1, "FAIL"), ("printf '3 passed'; return 0", 0, "PASS")] {
        let output = Command::new(&bash).args(["-c", &format!("pytest() {{ {body}; }}; {command}")]).current_dir(dir.path()).output().unwrap();
        assert_eq!(output.status.code(), Some(code), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(String::from_utf8_lossy(&output.stdout).contains(&format!("[agyfast-rs {verdict}]")));
        assert_eq!(dir.path().join("continued").exists(), code == 0);
    }
}

#[test]
fn mcp_stdio_handles_windows_paths_and_hidden_globs() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(r".hidden\sub")).unwrap();
    fs::write(dir.path().join(r".hidden\sub\note.md"), "# note\n").unwrap();
    fs::create_dir_all(dir.path().join("sub")).unwrap();
    fs::write(dir.path().join(r"sub\mod.rs"), "pub fn greet() -> i32 {\n    42\n}\n").unwrap();
    fs::write(dir.path().join("edit.txt"), "v1\n").unwrap();

    let mut child = Command::new(BIN)
        .arg("mcp")
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let frames = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "w", "version": "0"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "batch_search", "arguments": {
            "cwd": dir.path(),
            "toolSummary": "s",
            "queries": [
                {"type": "glob", "pattern": r".hidden\**\*.md"},
                {"type": "glob", "pattern": ".hidden/**/*.md"},
                {"type": "fuzzy", "pattern": r"sub\mod"}
            ]
        }}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "quick_outline", "arguments": {
            "cwd": dir.path(),
            "toolSummary": "o",
            "paths": [r"sub\mod.rs#greet"]
        }}}),
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "apply_edits", "arguments": {
            "cwd": dir.path(),
            "toolSummary": "e",
            "verify": "echo ok",
            "edits": [{"path": "edit.txt", "hunks": [{"target": "v1", "replacement": "v2"}]}]
        }}}),
    ];
    let mut stdin = child.stdin.take().unwrap();
    for f in &frames {
        writeln!(stdin, "{f}").unwrap();
    }
    drop(stdin);

    let replies: Vec<Value> = BufReader::new(child.stdout.take().unwrap())
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
        .collect();
    assert!(child.wait().unwrap().success());
    assert_eq!(replies.len(), 4);

    let search_txt = replies[1]["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(search_txt.matches("note.md").count(), 2, "{search_txt}");
    assert!(search_txt.contains("sub/mod.rs"), "{search_txt}");

    let outline_txt = replies[2]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(outline_txt.contains("pub fn greet()"), "{outline_txt}");

    let edit_txt = replies[3]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(edit_txt.contains("Applied edits to 1 file") && edit_txt.contains("Verify [PASS"), "{edit_txt}");
    assert_eq!(fs::read_to_string(dir.path().join("edit.txt")).unwrap(), "v2\n");
}
