//! `run_checks`: run shell commands concurrently in their own process groups
//! with timeouts, and compress their output per stack (cargo JSON, test
//! runners, TS linters, generic head-tail). Plain `a && b` chains run step by
//! step so each step gets its own compressor.

use crate::core::{budget, cwd};
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

const DEFAULT_TIMEOUT: u64 = 60;
const MAX_TIMEOUT: u64 = 170; // the reference's per-call MCP budget
const DEFAULT_CONCURRENCY: usize = 4;
const READ_CAP: u64 = 16 << 20;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Args {
    cwd: Option<String>,
    commands: Vec<Spec>,
    concurrency: Option<usize>,
    timeout_sec: Option<u64>,
    output_mode: Option<String>,
    max_chars: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Spec {
    name: Option<String>,
    command: String,
    cwd: Option<String>,
    timeout_sec: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
enum Status {
    Pass,
    Fail(String),
    Timeout,
    Error(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Cargo,
    Tests,
    Lint,
    Generic,
}

struct Run {
    status: Status,
    ms: u128,
    out: String, // ANSI- and CR-cleaned combined stdout+stderr
    kind: Kind,
    injected: bool,
    timeout: u64,
    steps: Vec<(String, Option<Run>)>, // chain steps; `None` = not run after a failure
}

pub fn run(args: Value) -> Result<String, String> {
    let a: Args = serde_json::from_value(args).map_err(|e| format!("Error: invalid run_checks arguments: {e}"))?;
    if !(1..=12).contains(&a.commands.len()) {
        return Err(format!("Error: commands must hold 1-12 entries, got {}", a.commands.len()));
    }
    let full = match a.output_mode.as_deref() {
        None | Some("compact") => false,
        Some("full") => true,
        Some(o) => return Err(format!("Error: outputMode must be \"compact\" or \"full\", got `{o}`")),
    };
    let base = cwd::resolve(a.cwd.as_deref())?;
    let jobs = a
        .commands
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let command = c.command.trim();
            if command.is_empty() {
                return Err(format!("Error: commands[{i}].command is empty"));
            }
            let dir = match c.cwd.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                Some(d) => base.join(d),
                None => base.clone(),
            };
            if !dir.is_dir() {
                return Err(format!("Error: commands[{i}].cwd does not exist: {}", dir.display()));
            }
            let name = match c.name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                Some(n) => n.to_string(),
                None if command.len() <= 80 && !command.contains('\n') => command.to_string(),
                None => format!("check-{}", i + 1),
            };
            let timeout = c.timeout_sec.or(a.timeout_sec).unwrap_or(DEFAULT_TIMEOUT).clamp(1, MAX_TIMEOUT);
            Ok((name, command, dir, timeout))
        })
        .collect::<Result<Vec<_>, String>>()?;

    let threads = a.concurrency.unwrap_or(DEFAULT_CONCURRENCY).clamp(1, 8);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|e| format!("Error: cannot start worker threads: {e}"))?;
    let start = Instant::now();
    // with_max_len(1): every command is its own stealable job, so a long one never queues others behind it.
    let runs: Vec<Run> =
        pool.install(|| jobs.par_iter().with_max_len(1).map(|(_, cmd, dir, t)| execute(cmd, dir, *t)).collect());

    let passed = runs.iter().filter(|r| r.status == Status::Pass).count();
    let mut head = format!("Checks: {passed}/{} passed", runs.len());
    if passed < runs.len() {
        head += &format!(", {} failed", runs.len() - passed);
    }
    head += &format!(" (wall: {}ms)", start.elapsed().as_millis());
    // Part 0 is the header; each command's part is rendered for the share it is given.
    let part = |i: usize, share: usize| {
        let Some(((name, command, _, _), r)) = i.checked_sub(1).map(|j| (&jobs[j], &runs[j])) else {
            return head.clone();
        };
        let cmd_line = if name.as_str() == *command { String::new() } else { format!("\n$ {command}") };
        if r.status == Status::Pass && !full {
            let cmd = if name.as_str() == *command { String::new() } else { format!(" (`{command}`)") };
            format!("- [PASS {}ms] {name}{cmd}{}", r.ms, pass_tail(r))
        } else {
            let top = format!("## [{} {}ms] {name}{cmd_line}\n", badge(&r.status), r.ms);
            let body = body(r, full, share.saturating_sub(top.len()));
            top + &body
        }
    };
    let lens: Vec<usize> = (0..=runs.len()).map(|i| part(i, usize::MAX).len()).collect();
    Ok(budget::join_fair_by(&lens, budget::limit(a.max_chars, 500), |i, share| budget::fold(&part(i, share), share)))
}

/// Run one verify command for `apply_edits` and return its compressed report,
/// starting with `Verify [PASS|FAIL|TIMEOUT|ERROR ...]`, at most `max_chars` bytes.
pub fn verify(command: &str, cwd: &Path, timeout_sec: u64, max_chars: usize) -> String {
    let timeout = if timeout_sec == 0 { DEFAULT_TIMEOUT } else { timeout_sec.min(MAX_TIMEOUT) };
    let r = execute(command.trim(), cwd, timeout);
    // A chain's body is its step lines, passing or not.
    let top = format!("Verify [{} in {}ms] `{command}`:\n", badge(&r.status), r.ms);
    let body = if r.status == Status::Pass && r.steps.is_empty() {
        summary(&compress(r.kind, &r.out, budget::CAP))
    } else {
        body(&r, false, max_chars.saturating_sub(top.len()))
    };
    budget::fold(&(top + &body), max_chars)
}

/// `: summary` for a passing command; a chain gets one indented line per step.
fn pass_tail(r: &Run) -> String {
    if r.steps.is_empty() { format!(": {}", summary(&compress(r.kind, &r.out, budget::CAP))) } else { format!(":\n{}", chain_body(r, false, budget::CAP)) }
}

/// One line per step; failing (or, in full mode, every) step output under its line.
fn chain_body(r: &Run, full: bool, max: usize) -> String {
    let (mut out, mut skipped) = (Vec::new(), Vec::new());
    for (cmd, s) in &r.steps {
        match s {
            None => skipped.push(format!("`{cmd}`")),
            Some(s) if s.status == Status::Pass && !full => {
                out.push(format!("  [PASS {}ms] {cmd}: {}", s.ms, summary(&compress(s.kind, &s.out, budget::CAP))))
            }
            Some(s) => out.push(format!("  [{} {}ms] {cmd}:\n{}", badge(&s.status), s.ms, body(s, full, max))),
        }
    }
    if !skipped.is_empty() {
        out.push(format!("Not run: {}", skipped.join(", ")));
    }
    out.join("\n")
}

fn badge(s: &Status) -> String {
    match s {
        Status::Pass => "PASS".into(),
        Status::Fail(label) => format!("FAIL ({label})"),
        Status::Timeout => "TIMEOUT".into(),
        Status::Error(_) => "ERROR".into(),
    }
}

/// Body under a `##` header: compressed output, or the cleaned raw output of
/// a passing command in full mode (cargo JSON is never shown raw).
fn body(r: &Run, full: bool, max: usize) -> String {
    if !r.steps.is_empty() {
        return chain_body(r, full, max);
    }
    let text = match &r.status {
        Status::Error(e) => return format!("spawn error: {e}"),
        Status::Pass if full && !r.injected => r.out.clone(),
        _ => compress(r.kind, &r.out, max),
    };
    let text = if text.trim().is_empty() { "(no output)".to_string() } else { text };
    match r.status {
        Status::Timeout => format!("TIMED OUT after {}s, process group killed\n{text}", r.timeout),
        _ => text,
    }
}

fn execute(command: &str, dir: &Path, timeout: u64) -> Run {
    match split_chain(command) {
        Some(steps) => execute_chain(&steps, dir, timeout),
        None => execute_one(command, dir, timeout, Duration::from_secs(timeout)),
    }
}

/// `timeout` is the reported limit, `wait` what is left of it.
fn execute_one(command: &str, dir: &Path, timeout: u64, wait: Duration) -> Run {
    let json = inject_json(command);
    let start = Instant::now();
    let (status, out) = match spawn_and_wait(json.as_deref().unwrap_or(command), dir, wait) {
        Ok(r) => r,
        Err(e) => (Status::Error(e.to_string()), String::new()),
    };
    let (ms, out, kind) = (start.elapsed().as_millis(), clean(&out), kind(command));
    Run { status, ms, out, kind, injected: json.is_some(), timeout, steps: Vec::new() }
}

#[derive(Debug, PartialEq)]
enum Step<'a> {
    Cd(&'a str),
    Run(&'a str),
}

/// Steps of a plain `a && b && ...` chain, or `None` to run the command as one
/// shell: anything but top-level `&&` between simple commands (see `and_parts`),
/// or a step that changes shell state other than a plain `cd DIR`.
fn split_chain(command: &str) -> Option<Vec<Step<'_>>> {
    chain_steps(and_parts(command, false).filter(|p| p.len() >= 2)?)
}

fn chain_steps(parts: Vec<&str>) -> Option<Vec<Step<'_>>> {
    const STATEFUL: &[&str] = &[
        "export", "source", ".", "set", "unset", "alias", "unalias", "shopt", "umask", "pushd", "popd", "eval", "exec",
        "trap", "ulimit", "readonly", "declare", "typeset", "local", "hash", "shift",
    ];
    parts
        .into_iter()
        .map(|p| {
            let w = words(p);
            let i = program(&w);
            match w.get(i)?.1 {
                "cd" if i == 0 && w.len() == 2 && !w[1].1.starts_with(['-', '~']) && !w[1].1.contains('$') => {
                    Some(Step::Cd(w[1].1.trim_matches(['\'', '"'])))
                }
                "cd" => None,
                prog if STATEFUL.contains(&prog) => None,
                _ => Some(Step::Run(p)),
            }
        })
        .collect()
}

/// The trimmed parts of a command made only of simple commands joined by
/// top-level `&&` (one part when there is no `&&`), or `None` for pipes, `;`,
/// `||`, `&`, redirections, subshells, braces, backslashes, comments or an open quote.
/// With `allow_merge`, a ` 2>&1` ending a part is allowed (and kept in it).
fn and_parts(command: &str, allow_merge: bool) -> Option<Vec<&str>> {
    let b = command.as_bytes();
    let (mut parts, mut start, mut quote, mut i) = (Vec::new(), 0, None, 0);
    let mut merged = false; // set once the current part has had its trailing `2>&1`
    while i < b.len() {
        let c = b[i];
        if c == b'\\' {
            return None;
        }
        if merged && quote.is_none() && !c.is_ascii_whitespace() && !command[i..].starts_with("&&") {
            return None; // something after the `2>&1` other than `&&`
        }
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                b'\'' | b'"' => quote = Some(c),
                b'2' if allow_merge
                    && i > 0
                    && b[i - 1].is_ascii_whitespace()
                    && command[i..].starts_with("2>&1")
                    && b.get(i + 4).is_none_or(|n| n.is_ascii_whitespace() || *n == b'&') =>
                {
                    merged = true;
                    i += 4;
                    continue;
                }
                b'&' if b.get(i + 1) == Some(&b'&') => {
                    merged = false;
                    parts.push(command[start..i].trim());
                    i += 2;
                    start = i;
                    continue;
                }
                b'|' | b'&' | b';' | b'<' | b'>' | b'(' | b')' | b'`' | b'{' | b'}' | b'#' | b'\n' => return None,
                _ => {}
            },
        }
        i += 1;
    }
    parts.push(command[start..].trim());
    quote.is_none().then_some(parts)
}

/// Single-quote `s` for a POSIX shell.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A step the PreToolUse hook pipes through `cli filter`: a recognised
/// build/test/lint command whose flags don't ask for raw or machine output.
fn filterable(step: &str) -> bool {
    const RAW: &[&str] = &[
        "--message-format", "--nocapture", "--show-output", "--format", "--json", "-json", "--list", "--watch", "--help",
        "-h", "--version", "-s", "--capture",
    ];
    kind(step) != Kind::Generic
        && !words(step).iter().any(|(_, w)| RAW.iter().any(|f| w == f || w.strip_prefix(f).is_some_and(|r| r.starts_with('='))))
}

/// `command` with each filterable step of a plain command or `&&` chain turned
/// into `STEP 2>&1 | EXE cli filter 'STEP'` (cargo builds get JSON diagnostics);
/// other steps stay as written. A trailing `2>&1` on a step is dropped (the
/// rewrite adds its own); any other redirection or pipe leaves the command alone.
/// `None` when nothing would change.
fn rewrite(command: &str, exe: &str) -> Option<String> {
    let command = command.trim();
    let parts = and_parts(command, true).filter(|_| !command.contains("agyfast-rs"))?;
    if parts.len() > 1 {
        chain_steps(parts.clone())?;
    }
    // Unquoted when it needs no quotes, so the allow rule is the same text however the host tokenises it.
    let exe = if exe.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-+".contains(&b)) { exe.to_string() } else { sh_quote(exe) };
    let steps: Vec<String> = parts
        .iter()
        .map(|&p| match filterable(p) {
            true => {
                let p = p.strip_suffix("2>&1").map_or(p, str::trim_end);
                format!("{} 2>&1 | {exe} cli filter {}", inject_json(p).as_deref().unwrap_or(p), sh_quote(p))
            }
            false => p.to_string(),
        })
        .collect();
    parts.iter().any(|p| filterable(p)).then(|| steps.join(" && "))
}

/// Claude Code `PreToolUse` response rewriting a Bash call, or `None` to leave
/// it alone (any unexpected shape included). Never sets `permissionDecision`.
fn pre_bash(payload: &str, exe: &str) -> Option<Value> {
    let p: Value = serde_json::from_str(payload).ok()?;
    if p["tool_name"] != "Bash" {
        return None;
    }
    let mut input = p["tool_input"].as_object()?.clone();
    if !matches!(input.get("run_in_background"), None | Some(Value::Null | Value::Bool(false))) {
        return None;
    }
    let command = rewrite(input.get("command")?.as_str()?, exe)?;
    input.insert("command".into(), command.into());
    Some(serde_json::json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "updatedInput": input}}))
}

/// `agyfast-rs hook pre-bash`: read the hook payload on stdin, print the rewrite or nothing.
pub fn pre_bash_hook() {
    let mut payload = String::new();
    let exe = std::env::current_exe().ok();
    if std::io::stdin().read_to_string(&mut payload).is_ok()
        && let Some(out) = exe.as_deref().and_then(Path::to_str).and_then(|exe| pre_bash(&payload, exe))
    {
        println!("{out}");
    }
}

/// `agyfast-rs cli filter`: compress `raw`, the captured output of `command`, under a
/// status line; `true` when the output shows failure. The exit code is lost in
/// the pipe, so failure is read from the content.
pub fn filter(command: &str, raw: &str) -> (String, bool) {
    let (k, out) = (kind(command), clean(raw));
    let failed = failed(k, &out);
    let text = match failed {
        None => format!("[agyfast-rs] {command} (status not judged)\n{}", generic(&out)),
        Some(false) => format!("[agyfast-rs PASS] {command}: {}", summary(&compress(k, &out, budget::CAP))),
        Some(true) => {
            let top = format!("[agyfast-rs FAIL] {command}\n");
            let body = compress(k, &out, budget::CAP - top.len());
            top + if body.trim().is_empty() { "(no output)" } else { &body }
        }
    };
    (budget::fold(&text, budget::CAP), failed == Some(true))
}

/// Whether output of this kind shows failure; `None` when the kind gives no verdict.
fn failed(kind: Kind, out: &str) -> Option<bool> {
    let error_line = |l: &str| l.starts_with("error:") || l.starts_with("error[");
    // `5 failed`, `1 error`, `2 errors`: a non-zero count of failures.
    let counted = |l: &str| {
        let w: Vec<&str> = l.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
        w.windows(2).any(|p| p[0].parse::<u64>().is_ok_and(|n| n > 0) && matches!(p[1], "failed" | "failures" | "error" | "errors"))
    };
    Some(match kind {
        Kind::Generic => return None,
        // cargo's JSON stream ends with `build-finished`; without it (manifest errors) cargo's own `error:` lines decide.
        Kind::Cargo => match out.lines().filter(|l| l.contains("\"build-finished\"")).filter_map(|l| serde_json::from_str::<Value>(l).ok()).next_back() {
            Some(v) => v["success"] == false,
            None => out.lines().any(error_line),
        },
        Kind::Tests | Kind::Lint => out.lines().map(str::trim).any(|t| {
            error_line(t)
                || t == "FAIL"
                || ["test result: FAILED", "FAILED ", "FAIL ", "FAIL\t", "--- FAIL"].iter().any(|p| t.starts_with(p))
                || t.contains("error TS")
                || counted(t)
        }),
    })
}

/// Run chain steps in order until one fails, all within one `timeout`.
fn execute_chain(steps: &[Step], dir: &Path, timeout: u64) -> Run {
    let start = Instant::now();
    let deadline = start + Duration::from_secs(timeout);
    let (mut dir, mut status, mut done) = (dir.to_path_buf(), Status::Pass, Vec::new());
    for step in steps {
        match step {
            Step::Run(c) if status != Status::Pass => done.push((c.to_string(), None)),
            Step::Run(c) => {
                let r = execute_one(c, &dir, timeout, deadline.saturating_duration_since(Instant::now()));
                status = r.status.clone();
                done.push((c.to_string(), Some(r)));
            }
            Step::Cd(_) if status != Status::Pass => {}
            Step::Cd(d) => {
                dir = dir.join(d);
                if !dir.is_dir() {
                    status = Status::Fail(format!("cd: no such directory: {d}"));
                }
            }
        }
    }
    let ms = start.elapsed().as_millis();
    Run { status, ms, out: String::new(), kind: Kind::Generic, injected: false, timeout, steps: done }
}

fn spawn_and_wait(command: &str, dir: &Path, timeout: Duration) -> std::io::Result<(Status, String)> {
    // One temp file for stdout and stderr: interleaved, and no pipe can fill up and deadlock.
    let mut file = tempfile::tempfile()?;
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(file.try_clone()?)
        .stderr(file.try_clone()?);
    for (k, v) in [("GIT_TERMINAL_PROMPT", "0"), ("GCM_INTERACTIVE", "never")] {
        if std::env::var_os(k).is_none() {
            cmd.env(k, v);
        }
    }
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd.spawn()?;
    drop(cmd);
    let status = match child.wait_timeout(timeout)? {
        Some(s) => exit_status(s),
        None => {
            kill_group(&mut child);
            child.wait()?;
            Status::Timeout
        }
    };
    Ok((status, read_capped(&mut file)?))
}

#[cfg(unix)]
fn kill_group(child: &mut Child) {
    // SAFETY: plain syscall; the child leads its own process group (process_group(0)).
    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
}

#[cfg(not(unix))]
fn kill_group(child: &mut Child) {
    let _ = child.kill();
}

fn exit_status(s: ExitStatus) -> Status {
    #[cfg(unix)]
    if let Some(sig) = std::os::unix::process::ExitStatusExt::signal(&s) {
        return Status::Fail(match sig {
            libc::SIGKILL => "SIGKILL - possible OOM".into(),
            libc::SIGSEGV => "SIGSEGV - segmentation fault".into(),
            libc::SIGABRT => "SIGABRT - process aborted".into(),
            libc::SIGTERM => "SIGTERM".into(),
            n => format!("signal {n}"),
        });
    }
    match s.code() {
        Some(0) => Status::Pass,
        Some(137) => Status::Fail("exit 137 / SIGKILL - possible OOM".into()),
        Some(139) => Status::Fail("exit 139 / SIGSEGV - segmentation fault".into()),
        Some(134) => Status::Fail("exit 134 / SIGABRT - process aborted".into()),
        Some(c) => Status::Fail(format!("exit {c}")),
        None => Status::Fail("exit ?".into()),
    }
}

fn read_capped(f: &mut File) -> std::io::Result<String> {
    let len = f.seek(SeekFrom::End(0))?;
    f.seek(SeekFrom::Start(0))?;
    let mut buf = Vec::new();
    if len <= READ_CAP {
        f.read_to_end(&mut buf)?;
    } else {
        // shortcut: runaway output keeps its first 1 MiB and last 15 MiB only; enough for any summary.
        f.by_ref().take(1 << 20).read_to_end(&mut buf)?;
        buf.extend_from_slice(format!("\n... [{} bytes of output omitted] ...\n", len - READ_CAP).as_bytes());
        f.seek(SeekFrom::Start(len - (READ_CAP - (1 << 20))))?;
        f.read_to_end(&mut buf)?;
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Strip ANSI escapes, keep only the last frame of `\r` progress redraws,
/// trim line ends and collapse blank-line runs.
fn clean(s: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in s.split('\n') {
        let frame = line.rsplit('\r').map(strip_ansi).find(|f| !f.trim().is_empty()).unwrap_or_default();
        let frame = frame.trim_end().to_string();
        if !(frame.is_empty() && lines.last().is_none_or(|l| l.is_empty())) {
            lines.push(frame);
        }
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('[') => while it.next().is_some_and(|c| !('@'..='~').contains(&c)) {},
            Some(']') => {
                while let Some(c) = it.next() {
                    if c == '\x07' || (c == '\x1b' && it.next().is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Shell-ish words with their byte offsets; quotes group but are kept.
fn words(s: &str) -> Vec<(usize, &str)> {
    let (mut out, mut start, mut quote) = (Vec::new(), None, None);
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c.is_whitespace() => {
                if let Some(st) = start.take() {
                    out.push((st, &s[st..i]));
                }
            }
            None => {
                if c == '\'' || c == '"' {
                    quote = Some(c);
                }
                start.get_or_insert(i);
            }
        }
    }
    if let Some(st) = start {
        out.push((st, &s[st..]));
    }
    out
}

/// Index of the real program word, past `VAR=value`, `env` and runner wrappers.
fn program(w: &[(usize, &str)]) -> usize {
    let word = |i: usize| w.get(i).map_or("", |x| x.1);
    let mut i = 0;
    loop {
        let cur = word(i);
        let assign = cur.split_once('=').is_some_and(|(k, _)| {
            !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
        i += match (cur, word(i + 1)) {
            _ if assign => 1,
            ("env" | "npx" | "bunx" | "pnpx" | "yarn", _) => 1,
            ("pnpm" | "bun", "exec" | "dlx" | "x") | ("uv" | "poetry" | "pipenv" | "hatch", "run") => 2,
            (p, "-m") if p.starts_with("python") => 2,
            (f, _) if f.starts_with('-') && i > 0 => 1, // wrapper flags such as `npx --yes`
            _ => return i,
        };
    }
}

fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// Index of the cargo subcommand word when `w[i]` is `cargo`.
fn cargo_sub(w: &[(usize, &str)], i: usize) -> Option<usize> {
    (basename(w.get(i)?.1) == "cargo").then_some(())?;
    (i + 1..w.len()).find(|&j| !w[j].1.starts_with(['+', '-']))
}

/// Compressor for a command, judged by its last `&&` step before any pipe.
fn kind(command: &str) -> Kind {
    let step = command.rsplit("&&").next().unwrap_or("").split('|').next().unwrap_or("");
    let w = words(step);
    let i = program(&w);
    let word = |j: usize| w.get(j).map_or("", |x| x.1);
    if let Some(j) = cargo_sub(&w, i) {
        return match word(j) {
            "check" | "clippy" | "build" | "c" | "b" => Kind::Cargo,
            "test" | "t" | "nextest" => Kind::Tests,
            _ => Kind::Generic,
        };
    }
    match (basename(word(i)), word(i + 1)) {
        ("pytest" | "py.test" | "vitest" | "jest", _) | ("go", "test") => Kind::Tests,
        ("tsc" | "vue-tsc" | "eslint" | "biome", _) => Kind::Lint,
        _ => Kind::Generic,
    }
}

/// `cargo check|clippy|build ...` with `--message-format=json` right after the
/// subcommand (so before any `--`), only for a single plain invocation.
fn inject_json(command: &str) -> Option<String> {
    if command.contains(|c| "|&;<>`$(){}\n\\".contains(c)) || command.contains("--message-format") {
        return None;
    }
    let w = words(command);
    let (at, sub) = w[cargo_sub(&w, program(&w))?];
    if !matches!(sub, "check" | "clippy" | "build" | "c" | "b") {
        return None;
    }
    let end = at + sub.len();
    Some(format!("{} --message-format=json{}", &command[..end], &command[end..]))
}

/// `max` is the space the result should fit; only cargo diagnostics adapt to it.
fn compress(kind: Kind, out: &str, max: usize) -> String {
    match kind {
        Kind::Cargo => compress_cargo(out, max),
        Kind::Tests => compress_tests(out),
        Kind::Lint => compress_lint(out),
        Kind::Generic => generic(out),
    }
}

/// Keep `head` and `tail` lines around an omission marker; blank runs collapse.
fn fold_lines(mut lines: Vec<&str>, head: usize, tail: usize) -> String {
    lines.dedup_by(|a, b| a.trim().is_empty() && b.trim().is_empty());
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    if lines.len() <= head + tail + 1 {
        return lines.join("\n");
    }
    let omitted = lines.len() - head - tail;
    format!("{}\n... [{omitted} lines omitted] ...\n{}", lines[..head].join("\n"), lines[lines.len() - tail..].join("\n"))
}

/// Head-tail fold favouring the tail, where summaries and final errors live.
fn generic(out: &str) -> String {
    fold_lines(out.lines().collect(), 15, 60)
}

fn cargo_noise(line: &str) -> bool {
    const STATUS: &[&str] = &[
        "Compiling ", "Checking ", "Finished ", "Fresh ", "Blocking ", "Downloading ", "Downloaded ", "Updating ",
        "Locking ", "Adding ", "Building ", "Documenting ", "Doc-tests ", "Waiting ", "Packaging ", "Running ",
    ];
    let t = line.trim_start();
    STATUS.iter().any(|p| t.starts_with(p))
        || t.starts_with("error: could not compile")
        || t.starts_with("warning: build failed")
        || (t.starts_with("warning: `") && t.contains(" generated "))
}

/// Cargo with `--message-format=json`: one line per diagnostic, errors first,
/// deduplicated, plus counts. Without JSON diagnostics, falls back to the text.
/// Each distinct location gets its source line underneath while the result fits `max`.
fn compress_cargo(out: &str, max: usize) -> String {
    let (mut errors, mut warnings, mut text) = (Vec::new(), Vec::new(), Vec::new());
    for line in out.lines() {
        if let Some(v) = line.starts_with('{').then(|| serde_json::from_str::<Value>(line).ok()).flatten() {
            if v["reason"] == "compiler-message" {
                let m = &v["message"];
                let level = m["level"].as_str().unwrap_or("");
                let bucket = if level.starts_with("error") {
                    &mut errors
                } else if level == "warning" {
                    &mut warnings
                } else {
                    continue;
                };
                let msg = m["message"].as_str().unwrap_or("");
                if !msg.starts_with("aborting due to") && !msg.ends_with(" emitted") {
                    bucket.push(diagnostic(m));
                }
            }
        } else if !cargo_noise(line) {
            text.push(line);
        }
    }
    let mut seen = HashSet::new();
    errors.retain(|d| seen.insert(d.0.clone()));
    warnings.retain(|d| seen.insert(d.0.clone()));
    let mut at = HashSet::new(); // one source line per file:line, errors claim it first
    for d in errors.iter_mut().chain(warnings.iter_mut()) {
        if d.1.as_ref().is_some_and(|(key, _)| !at.insert(key.clone())) {
            d.1 = None;
        }
    }
    let (e, w) = (errors.len(), warnings.len());
    // Without compiler errors, other text (build-script or manifest failures) explains the outcome.
    let rest = if e == 0 { generic(&text.join("\n")) } else { String::new() };
    let render = |error_src: bool, warning_src: bool| {
        let mut lines = Vec::new();
        for (diags, src) in [(&errors, error_src), (&warnings, warning_src)] {
            for (line, source) in diags {
                lines.push(line.clone());
                if let Some((_, code)) = source.as_ref().filter(|_| src) {
                    lines.push(format!("  > {code}"));
                }
            }
        }
        if !rest.trim().is_empty() {
            lines.push(rest.clone());
        }
        if e + w > 0 || lines.is_empty() {
            let s = |n: usize| if n == 1 { "" } else { "s" };
            lines.push(format!("{e} error{}, {w} warning{}", s(e), s(w)));
        }
        lines.join("\n")
    };
    // Source text goes first when space is short (warnings' before errors'), never a location.
    [(true, true), (true, false)].into_iter().map(|(a, b)| render(a, b)).find(|s| s.len() <= max).unwrap_or_else(|| render(false, false))
}

/// `file:line:col [code] message: label | suggested_fix`, plus the primary
/// span's `file:line` and trimmed source line (none for a warning whose
/// machine-applicable fix already says what to change).
fn diagnostic(m: &Value) -> (String, Option<(String, String)>) {
    let one_line = |s: &str| s.replace('\n', "\\n");
    let span = m["spans"].as_array().and_then(|s| s.iter().find(|s| s["is_primary"] == true));
    let mut line = span.map_or(String::new(), |s| {
        format!("{}:{}:{} ", s["file_name"].as_str().unwrap_or("?"), s["line_start"], s["column_start"])
    });
    if let Some(code) = m["code"]["code"].as_str() {
        line += &format!("[{code}] ");
    }
    line += &one_line(m["message"].as_str().unwrap_or(""));
    if let Some(label) = span.and_then(|s| s["label"].as_str()) {
        line += &format!(": {}", one_line(label));
    }
    let fix = m["children"].as_array().into_iter().flatten().find_map(|c| {
        let repl: Vec<&str> = c["spans"]
            .as_array()?
            .iter()
            .filter(|s| s["suggestion_applicability"] == "MachineApplicable")
            .filter_map(|s| s["suggested_replacement"].as_str())
            .collect();
        if repl.is_empty() {
            return None;
        }
        let shown: Vec<String> = repl.iter().filter(|r| !r.is_empty()).map(|r| format!("`{}`", one_line(r))).collect();
        let msg = one_line(c["message"].as_str().unwrap_or("fix"));
        Some(if shown.is_empty() { msg } else { format!("{msg}: {}", shown.join(", ")) })
    });
    let source = span.filter(|_| fix.is_none() || m["level"] != "warning").and_then(|s| {
        Some((format!("{}:{}", s["file_name"].as_str()?, s["line_start"]), source_line(&s["text"][0])?))
    });
    if let Some(fix) = fix {
        line += &format!(" | {fix}");
    }
    (line, source)
}

/// A span's source line without indentation, cut to 100 chars around the highlight.
fn source_line(t: &Value) -> Option<String> {
    const MAX: usize = 100;
    let raw = t["text"].as_str()?;
    let lead = raw.chars().take_while(|c| c.is_whitespace()).count();
    let chars: Vec<char> = raw.trim().chars().collect();
    if chars.is_empty() {
        return None;
    }
    if chars.len() <= MAX {
        return Some(chars.into_iter().collect());
    }
    // highlight_start is a 1-based char column of the untrimmed line.
    let hl = (t["highlight_start"].as_u64().unwrap_or(1) as usize).saturating_sub(1 + lead);
    let start = hl.saturating_sub(MAX / 4).min(chars.len() - MAX);
    let cut: String = chars[start..start + MAX].iter().collect();
    Some(format!("{}{cut}{}", if start > 0 { "…" } else { "" }, if start + MAX < chars.len() { "…" } else { "" }))
}

/// cargo test / pytest / go test / vitest / jest: drop passing tests and
/// progress, keep failures, traces and the summary.
fn compress_tests(out: &str) -> String {
    let noise = |line: &str| {
        let t = line.trim();
        let frame = t.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start();
        let labelled_caret = frame.contains('^') && !frame.rsplit('^').next().unwrap_or("").trim().is_empty();
        cargo_noise(line)
            || (t.starts_with("test ") && (t.ends_with(" ... ok") || t.ends_with(" ... ignored")))
            || (t.starts_with("running ") && (t.ends_with(" tests") || t.ends_with(" test")))
            || ["=== RUN", "=== PAUSE", "=== CONT", "--- PASS", "PASS ", "✓", "✔", "√", "= note:"].iter().any(|p| t.starts_with(p))
            // rustc code frames go, except caret lines carrying a label; the `-->` location stays
            || (frame.starts_with('|') && !labelled_caret)
            || t.starts_with("For more information about this error")
            || t.ends_with(" PASSED")
            || t.ends_with("[no test files]")
            || (t.ends_with("%]") && !t.contains("FAIL") && !t.contains("ERROR"))
            || (t.starts_with("at ") && (t.contains("node_modules") || t.contains("node:internal")))
    };
    fold_lines(out.lines().filter(|l| !noise(l)).collect(), 30, 50)
}

/// tsc / eslint / biome: keep diagnostic lines, file headers and summaries.
fn compress_lint(out: &str) -> String {
    let keep = |line: &str| {
        let t = line.trim();
        let lower = t.to_lowercase();
        let first = t.split_whitespace().next().unwrap_or("");
        let located = first.contains('.')
            && first.split([':', '(']).nth(1).is_some_and(|n| n.starts_with(|c: char| c.is_ascii_digit()));
        let file_header = !line.starts_with(char::is_whitespace) && !t.contains(' ') && t.contains('/') && t.contains('.');
        !t.contains('│')
            && (located
                || file_header
                || ["error", "warning", "problem"].iter().any(|w| lower.contains(w))
                || t.starts_with(['×', '✖'])
                || t.starts_with("Found ")
                || t.starts_with("Checked "))
    };
    let kept: Vec<&str> = out.lines().filter(|l| keep(l)).collect();
    if kept.is_empty() { generic(out) } else { fold_lines(kept, 40, 40) }
}

/// One or two summary lines for a passing command.
fn summary(body: &str) -> String {
    const WORDS: &[&str] = &[
        "passed", "test", "tests", "spec", "specs", "suite", "suites", "ok", "warning", "warnings", "error", "errors",
        "compiled", "finished",
    ];
    let is_summary = |l: &str| {
        let lower = l.to_lowercase();
        lower.contains("built in") || lower.contains("done in") || lower.split(|c: char| !c.is_alphanumeric()).any(|w| WORDS.contains(&w))
    };
    let lines: Vec<&str> = body.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("... [")).collect();
    let results: Vec<&str> = lines.iter().copied().filter(|l| l.starts_with("test result:")).collect();
    if results.len() > 1 {
        return test_totals(&results);
    }
    let hits: Vec<&str> = lines.iter().copied().filter(|l| is_summary(l)).collect();
    let picked = match (hits.len(), lines.len()) {
        (_, 0) => return "OK".into(),
        (h, _) if h > 0 => &hits[h.saturating_sub(2)..],
        (_, n) if n <= 2 => &lines[..],
        (_, n) => &lines[n - 1..],
    };
    budget::fold(&picked.join(" | "), 300)
}

/// One line summing cargo's per-binary `test result:` lines.
fn test_totals(results: &[&str]) -> String {
    const LABELS: [&str; 4] = ["passed", "failed", "ignored", "filtered out"];
    let mut n = [0u64; 4];
    for seg in results.iter().flat_map(|l| l.split([';', '.'])) {
        if let Some((num, label)) = seg.trim().split_once(' ')
            && let (Ok(num), Some(k)) = (num.parse::<u64>(), LABELS.iter().position(|l| *l == label))
        {
            n[k] += num;
        }
    }
    let ok = if results.iter().all(|l| l.starts_with("test result: ok")) { "ok" } else { "FAILED" };
    let filtered = if n[3] > 0 { format!("; {} filtered out", n[3]) } else { String::new() };
    format!("test result: {ok}. {} passed; {} failed; {} ignored{filtered} ({} suites)", n[0], n[1], n[2], results.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Real `cargo check --message-format=json 2>&1` on a crate with a type error and an unused
    // import (`rendered`, `explanation` and artifact details trimmed for size).
    const CARGO_CHECK: &str = r##"    Checking demo v0.1.0 (/work/demo)
{"reason":"compiler-message","message":{"$message_type":"diagnostic","children":[{"children":[],"code":null,"level":"note","message":"`#[warn(unused_imports)]` (part of `#[warn(unused)]`) on by default","spans":[]},{"children":[],"code":null,"level":"help","message":"remove the whole `use` item","spans":[{"byte_end":31,"byte_start":0,"column_end":1,"column_start":1,"expansion":null,"file_name":"src/main.rs","is_primary":true,"label":null,"line_end":2,"line_start":1,"suggested_replacement":"","suggestion_applicability":"MachineApplicable","text":[{"highlight_end":31,"highlight_start":1,"text":"use std::collections::HashMap;"},{"highlight_end":1,"highlight_start":1,"text":""}]}]}],"level":"warning","message":"unused import: `std::collections::HashMap`","spans":[{"byte_end":29,"byte_start":4,"column_end":30,"column_start":5,"expansion":null,"file_name":"src/main.rs","is_primary":true,"label":null,"line_end":1,"line_start":1,"suggested_replacement":null,"suggestion_applicability":null,"text":[{"highlight_end":30,"highlight_start":5,"text":"use std::collections::HashMap;"}]}],"code":{"code":"unused_imports"}}}
{"reason":"compiler-message","message":{"$message_type":"diagnostic","children":[],"level":"error","message":"mismatched types","spans":[{"byte_end":108,"byte_start":102,"column_end":24,"column_start":18,"expansion":null,"file_name":"src/main.rs","is_primary":true,"label":"expected `i32`, found `&str`","line_end":6,"line_start":6,"suggested_replacement":null,"suggestion_applicability":null,"text":[{"highlight_end":24,"highlight_start":18,"text":"    let x: i32 = \"five\";"}]},{"byte_end":99,"byte_start":96,"column_end":15,"column_start":12,"expansion":null,"file_name":"src/main.rs","is_primary":false,"label":"expected due to this","line_end":6,"line_start":6,"suggested_replacement":null,"suggestion_applicability":null,"text":[{"highlight_end":15,"highlight_start":12,"text":"    let x: i32 = \"five\";"}]}],"code":{"code":"E0308"}}}
{"reason":"compiler-message","message":{"$message_type":"diagnostic","children":[],"level":"failure-note","message":"For more information about this error, try `rustc --explain E0308`.","spans":[],"code":null}}
error: could not compile `demo` (bin "demo") due to 1 previous error; 1 warning emitted
{"reason":"build-finished","success":false}"##;
    // Real `cargo check --message-format=json 2>&1` on a type error in a multibyte line
    // (`rendered`, `explanation`, package and target details trimmed for size).
    const CARGO_CHECK_UTF8: &str = r##"    Checking demo v0.1.0 (/work/demo)
{"reason":"compiler-message","message":{"$message_type":"diagnostic","children":[],"level":"error","message":"mismatched types","spans":[{"byte_end":55,"byte_start":36,"column_end":35,"column_start":25,"expansion":null,"file_name":"src/main.rs","is_primary":true,"label":"expected `i32`, found `&str`","line_end":2,"line_start":2,"suggested_replacement":null,"suggestion_applicability":null,"text":[{"highlight_end":35,"highlight_start":25,"text":"    let greeting: i32 = \"привіт 🌍\";"}]},{"byte_end":33,"byte_start":30,"column_end":22,"column_start":19,"expansion":null,"file_name":"src/main.rs","is_primary":false,"label":"expected due to this","line_end":2,"line_start":2,"suggested_replacement":null,"suggestion_applicability":null,"text":[{"highlight_end":22,"highlight_start":19,"text":"    let greeting: i32 = \"привіт 🌍\";"}]}],"code":{"code":"E0308"}}}
{"reason":"compiler-message","message":{"$message_type":"diagnostic","children":[],"level":"failure-note","message":"For more information about this error, try `rustc --explain E0308`.","spans":[],"code":null}}
error: could not compile `demo` (bin "demo") due to 1 previous error
{"reason":"build-finished","success":false}"##;
    // Real `cargo clippy --message-format=json 2>&1` (passing, two warnings with fixes).
    const CARGO_CLIPPY: &str = r##"    Checking demo v0.1.0 (/work/demo)
{"reason":"compiler-message","message":{"$message_type":"diagnostic","children":[{"children":[],"code":null,"level":"note","message":"`#[warn(unused_variables)]` (part of `#[warn(unused)]`) on by default","spans":[]},{"children":[],"code":null,"level":"help","message":"if this is intentional, prefix it with an underscore","spans":[{"byte_end":75,"byte_start":69,"column_end":15,"column_start":9,"expansion":null,"file_name":"src/main.rs","is_primary":true,"label":null,"line_end":4,"line_start":4,"suggested_replacement":"_unused","suggestion_applicability":"MachineApplicable","text":[{"highlight_end":15,"highlight_start":9,"text":"    let unused = 3;"}]}]}],"level":"warning","message":"unused variable: `unused`","spans":[{"byte_end":75,"byte_start":69,"column_end":15,"column_start":9,"expansion":null,"file_name":"src/main.rs","is_primary":true,"label":null,"line_end":4,"line_start":4,"suggested_replacement":null,"suggestion_applicability":null,"text":[{"highlight_end":15,"highlight_start":9,"text":"    let unused = 3;"}]}],"code":{"code":"unused_variables"}}}
{"reason":"compiler-message","message":{"$message_type":"diagnostic","children":[{"children":[],"code":null,"level":"help","message":"for further information visit https://rust-lang.github.io/rust-clippy/rust-1.100.0/index.html#needless_return","spans":[]},{"children":[],"code":null,"level":"note","message":"`#[warn(clippy::needless_return)]` on by default","spans":[]},{"children":[],"code":null,"level":"help","message":"remove `return`","spans":[{"byte_end":44,"byte_start":32,"column_end":45,"column_start":33,"expansion":null,"file_name":"src/main.rs","is_primary":true,"label":null,"line_end":1,"line_start":1,"suggested_replacement":"a + b","suggestion_applicability":"MachineApplicable","text":[{"highlight_end":45,"highlight_start":33,"text":"fn add(a: i32, b: i32) -> i32 { return a + b; }"}]},{"byte_end":46,"byte_start":44,"column_end":47,"column_start":45,"expansion":null,"file_name":"src/main.rs","is_primary":true,"label":null,"line_end":1,"line_start":1,"suggested_replacement":"","suggestion_applicability":"MachineApplicable","text":[{"highlight_end":47,"highlight_start":45,"text":"fn add(a: i32, b: i32) -> i32 { return a + b; }"}]}]}],"level":"warning","message":"unneeded `return` statement","spans":[{"byte_end":44,"byte_start":32,"column_end":45,"column_start":33,"expansion":null,"file_name":"src/main.rs","is_primary":true,"label":null,"line_end":1,"line_start":1,"suggested_replacement":null,"suggestion_applicability":null,"text":[{"highlight_end":45,"highlight_start":33,"text":"fn add(a: i32, b: i32) -> i32 { return a + b; }"}]}],"code":{"code":"clippy::needless_return"}}}
{"reason":"compiler-artifact"}
{"reason":"build-finished","success":true}
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.05s"##;
    // Real `cargo test 2>&1` with one failing test.
    const CARGO_TEST: &str = r##"   Compiling demo v0.1.0 (/work/demo)
warning: unused variable: `unused`
 --> src/main.rs:4:9
  |
4 |     let unused = 3;
  |         ^^^^^^ help: if this is intentional, prefix it with an underscore: `_unused`
  |
  = note: `#[warn(unused_variables)]` (part of `#[warn(unused)]`) on by default

warning: `demo` (bin "demo" test) generated 1 warning (run `cargo fix --bin "demo" -p demo --tests` to apply 1 suggestion)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.13s
     Running unittests src/main.rs (target/debug/build/demo/51a431faf31de935/out/demo-51a431faf31de935)

running 2 tests
test tests::ok_one ... ok
test tests::bad_one ... FAILED

failures:

---- tests::bad_one stdout ----

thread 'tests::bad_one' (15164704) panicked at src/main.rs:13:20:
assertion `left == right` failed: math is broken
  left: 4
 right: 5
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    tests::bad_one

test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

error: test failed, to rerun pass `--bin demo`"##;
    // Real `go test -v ./...` with one failing test.
    const GO_TEST: &str = "=== RUN   TestOk\n--- PASS: TestOk (0.00s)\n=== RUN   TestBad\n    m_test.go:9: add: got 4, want 5\n--- FAIL: TestBad (0.00s)\n=== RUN   TestSub\n=== RUN   TestSub/inner_ok\n--- PASS: TestSub (0.00s)\n    --- PASS: TestSub/inner_ok (0.00s)\nFAIL\nFAIL\texample.com/gt\t0.317s\nFAIL";

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn injects_message_format_only_into_plain_cargo_invocations() {
        let j = |c: &str| inject_json(c);
        assert_eq!(j("cargo check").as_deref(), Some("cargo check --message-format=json"));
        assert_eq!(
            j("cargo +nightly clippy --all-targets -- -D warnings").as_deref(),
            Some("cargo +nightly clippy --message-format=json --all-targets -- -D warnings")
        );
        assert_eq!(
            j("RUSTFLAGS=\"-D warnings\" CARGO_TARGET_DIR=/tmp/t cargo build --release").as_deref(),
            Some("RUSTFLAGS=\"-D warnings\" CARGO_TARGET_DIR=/tmp/t cargo build --message-format=json --release")
        );
        assert_eq!(j("cargo --locked c -p x").as_deref(), Some("cargo --locked c --message-format=json -p x"));
        for c in [
            "cargo test",
            "cargo check --message-format short",
            "cargo check 2>&1 | head",
            "cargo check && cargo test",
            "cd sub; cargo check",
            "echo cargo check",
            "cargo run -- check",
            "cargo check $FLAGS",
        ] {
            assert_eq!(j(c), None, "{c}");
        }
    }

    #[test]
    fn dispatches_through_assignments_and_wrappers() {
        for (c, k) in [
            ("cargo check", Kind::Cargo),
            ("cd a && cargo clippy 2>&1 | tail", Kind::Cargo),
            ("FOO=1 cargo test -p x", Kind::Tests),
            ("uv run pytest -q", Kind::Tests),
            ("python3 -m pytest tests", Kind::Tests),
            ("pnpm exec vitest run", Kind::Tests),
            ("npx --yes jest", Kind::Tests),
            ("go test ./...", Kind::Tests),
            ("npx tsc --noEmit", Kind::Lint),
            ("./node_modules/.bin/eslint src", Kind::Lint),
            ("biome check .", Kind::Lint),
            ("go build ./...", Kind::Generic),
            ("ls -la", Kind::Generic),
        ] {
            assert_eq!(kind(c), k, "{c}");
        }
    }

    #[test]
    fn cargo_json_becomes_one_line_per_diagnostic() {
        // The error gets its source line; the warning's fix already says what to change.
        let out = compress_cargo(CARGO_CHECK, budget::CAP);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "src/main.rs:6:18 [E0308] mismatched types: expected `i32`, found `&str`");
        assert_eq!(lines[1], "  > let x: i32 = \"five\";");
        assert_eq!(
            lines[2],
            "src/main.rs:1:5 [unused_imports] unused import: `std::collections::HashMap` | remove the whole `use` item"
        );
        assert_eq!(lines[3], "1 error, 1 warning");
        assert_eq!(lines.len(), 4, "{out}");
        // Too tight for source text: every location and the count line stay.
        let tight = compress_cargo(CARGO_CHECK, out.len() - 1);
        assert_eq!(tight, [lines[0], lines[2], lines[3]].join("\n"));

        let out = compress_cargo(CARGO_CLIPPY, budget::CAP);
        assert!(out.contains("[clippy::needless_return] unneeded `return` statement | remove `return`: `a + b`"), "{out}");
        assert!(out.contains("| if this is intentional, prefix it with an underscore: `_unused`"), "{out}");
        assert!(out.ends_with("0 errors, 2 warnings") && !out.contains("Finished"), "{out}");
        assert_eq!(summary(&out), "0 errors, 2 warnings");

        // Real manifest error: no JSON diagnostics, so the text is kept.
        let manifest = "error: invalid type: integer `1`, expected SemVer version\n --> Cargo.toml:3:11\n  |\n3 | version = 1\n  |           ^";
        assert_eq!(compress_cargo(manifest, budget::CAP), manifest);
        assert_eq!(compress_cargo("    Finished `dev` profile [unoptimized] target(s) in 0.05s", budget::CAP), "0 errors, 0 warnings");
    }

    #[test]
    fn source_lines_are_multibyte_safe_and_go_first_under_a_tight_budget() {
        let out = compress_cargo(CARGO_CHECK_UTF8, budget::CAP);
        assert_eq!(
            out,
            "src/main.rs:2:25 [E0308] mismatched types: expected `i32`, found `&str`\n  > let greeting: i32 = \"привіт 🌍\";\n1 error, 0 warnings"
        );
        assert_eq!(compress_cargo(CARGO_CHECK_UTF8, 60).lines().count(), 2);

        // A warning without a fix keeps its source unless space is short; the error's goes last.
        let warn = r#"{"reason":"compiler-message","message":{"children":[],"level":"warning","message":"function `helper` is never used","spans":[{"column_start":4,"file_name":"src/main.rs","is_primary":true,"label":null,"line_start":5,"text":[{"highlight_end":10,"highlight_start":4,"text":"fn helper() {}"}]}],"code":{"code":"dead_code"}}}"#;
        let both = format!("{CARGO_CHECK_UTF8}\n{warn}");
        let full = compress_cargo(&both, budget::CAP);
        assert!(full.contains("\n  > fn helper() {}\n1 error, 1 warning"), "{full}");
        let errors_only = compress_cargo(&both, full.len() - 1);
        assert!(errors_only.contains("  > let greeting") && !errors_only.contains("  > fn helper"), "{errors_only}");
        assert_eq!(compress_cargo(&both, 10).lines().count(), 3);

        // Long lines are cut around the highlight on char boundaries.
        let long = format!("    {}let bad: i32 = \"€\";{}", "é".repeat(150), "ü".repeat(150));
        let cut = source_line(&json!({"text": long, "highlight_start": 155})).unwrap();
        assert!(cut.starts_with('…') && cut.ends_with('…') && cut.contains("let bad") && cut.chars().count() == 102, "{cut}");
    }

    #[test]
    fn test_runners_keep_failures_and_summary() {
        let out = compress_tests(CARGO_TEST);
        assert!(out.contains("|         ^^^^^^ help: if this is intentional, prefix it with an underscore: `_unused`"), "{out}");
        for want in ["test tests::bad_one ... FAILED", "math is broken", "left: 4", "test result: FAILED. 1 passed; 1 failed", "error: test failed"] {
            assert!(out.contains(want), "missing {want}: {out}");
        }
        for gone in ["tests::ok_one ... ok", "Compiling", "Running unittests", "running 2 tests", "4 |     let unused"] {
            assert!(!out.contains(gone), "kept {gone}: {out}");
        }

        let out = compress_tests(GO_TEST);
        assert!(out.contains("m_test.go:9: add: got 4, want 5") && out.contains("--- FAIL: TestBad") && out.contains("FAIL\texample.com/gt"));
        assert!(!out.contains("=== RUN") && !out.contains("--- PASS"), "{out}");

        // Hand-written (pytest not installed here).
        let pytest = "============ test session starts ============\nplatform darwin -- Python 3.12.1, pytest-8.3.2\ncollected 3 items\n\ntests/test_a.py ..F                                   [100%]\n\n================= FAILURES =================\n___________________ test_bad ___________________\n\n    def test_bad():\n>       assert add(2, 2) == 5\nE       assert 4 == 5\n\ntests/test_a.py:9: AssertionError\n========= short test summary info =========\nFAILED tests/test_a.py::test_bad - assert 4 == 5\n========= 1 failed, 2 passed in 0.03s =========";
        let out = compress_tests(pytest);
        assert!(out.contains("E       assert 4 == 5") && out.contains("FAILED tests/test_a.py::test_bad") && out.ends_with("1 failed, 2 passed in 0.03s ========="));
        assert!(!out.contains("..F"), "{out}");

        // Hand-written (vitest / jest not installed here).
        let vitest = " ✓ src/math.test.ts (2 tests) 3ms\n ❯ src/api.test.ts (2 tests | 1 failed) 8ms\n   ✓ api > lists\n   × api > creates 5ms\n     → expected 201 to be 200\n\n FAIL  src/api.test.ts > api > creates\nAssertionError: expected 201 to be 200\n ❯ src/api.test.ts:14:22\n\n Test Files  1 failed | 1 passed (2)\n      Tests  1 failed | 3 passed (4)";
        let out = compress_tests(vitest);
        assert!(out.contains("× api > creates") && out.contains("src/api.test.ts:14:22") && out.contains("Tests  1 failed | 3 passed (4)"));
        assert!(!out.contains("✓"), "{out}");
        let jest = "PASS src/a.test.js\nFAIL src/b.test.js\n  ● b › adds\n\n    expect(received).toBe(expected)\n\n    Expected: 5\n    Received: 4\n\n      at Object.<anonymous> (src/b.test.js:3:17)\n      at Promise.then.completed (node_modules/jest-circus/build/utils.js:298:28)\n\nTests:       1 failed, 3 passed, 4 total";
        let out = compress_tests(jest);
        assert!(out.contains("● b › adds") && out.contains("src/b.test.js:3:17") && out.ends_with("Tests:       1 failed, 3 passed, 4 total"));
        assert!(!out.contains("PASS src/a") && !out.contains("jest-circus"), "{out}");
    }

    #[test]
    fn linters_keep_diagnostics_and_summaries() {
        // Hand-written (tsc / eslint / biome not installed here).
        let tsc = "src/a.ts(12,5): error TS2322: Type 'string' is not assignable to type 'number'.\nsrc/b.ts:3:1 - error TS2304: Cannot find name 'foo'.\n\n3 foo();\n  ~~~\n\nFound 2 errors in 2 files.";
        assert_eq!(
            compress_lint(tsc),
            "src/a.ts(12,5): error TS2322: Type 'string' is not assignable to type 'number'.\nsrc/b.ts:3:1 - error TS2304: Cannot find name 'foo'.\nFound 2 errors in 2 files."
        );
        let eslint = "\n/repo/src/a.ts\n  3:7   error    'x' is assigned a value but never used  no-unused-vars\n  9:1   warning  Unexpected console statement            no-console\n\n✖ 2 problems (1 error, 1 warning)\n";
        assert_eq!(
            compress_lint(eslint),
            "/repo/src/a.ts\n  3:7   error    'x' is assigned a value but never used  no-unused-vars\n  9:1   warning  Unexpected console statement            no-console\n✖ 2 problems (1 error, 1 warning)"
        );
        let biome = "src/a.ts:1:1 lint/style/useConst  FIXABLE  ━━━━━━━━━━\n\n  × This let declares a variable that is only assigned once.\n\n  > 1 │ let a = 1;\n      │ ^^^\n\n  i Safe fix: Use const instead.\n\nChecked 4 files in 3ms. No fixes applied.\nFound 1 error.";
        let out = compress_lint(biome);
        assert!(out.starts_with("src/a.ts:1:1 lint/style/useConst") && out.contains("× This let") && out.ends_with("Found 1 error."));
        assert!(!out.contains('│') && !out.contains("Safe fix"), "{out}");
    }

    #[test]
    fn clean_strips_ansi_and_progress_redraws() {
        let raw = "\x1b[31mError: bad\x1b[0m\r\nprogress 10%\rprogress 100%\n\x1b]8;;http://x\x07link\x1b]8;;\x07\n\n\n\x1b[2Kdone   \n\n";
        assert_eq!(clean(raw), "Error: bad\nprogress 100%\nlink\n\ndone");
    }

    #[test]
    fn generic_folds_favouring_the_tail() {
        let text: Vec<String> = (1..=200).map(|i| format!("line {i}")).collect();
        let out = generic(&text.join("\n"));
        assert!(out.starts_with("line 1\n") && out.ends_with("line 200") && out.contains("[125 lines omitted]"));
        assert!(out.contains("line 141") && !out.contains("line 140\n"));
    }

    #[test]
    fn timeout_kills_the_whole_process_group() {
        let dir = tmp();
        let r = execute("sleep 30 & echo $! > pid; echo started; wait", dir.path(), 1);
        assert_eq!(r.status, Status::Timeout);
        assert!(r.ms < 5000, "took {}ms", r.ms);
        assert!(body(&r, false, budget::CAP).contains("TIMED OUT after 1s") && r.out.contains("started"));
        let pid: i32 = std::fs::read_to_string(dir.path().join("pid")).unwrap().trim().parse().unwrap();
        let gone = (0..100).any(|_| {
            std::thread::sleep(Duration::from_millis(20));
            // SAFETY: signal 0 only probes whether the pid exists.
            unsafe { libc::kill(pid, 0) != 0 }
        });
        assert!(gone, "grandchild {pid} survived the timeout");
    }

    #[test]
    fn large_output_does_not_deadlock() {
        let r = execute("seq 1 300000; seq 1 300000 >&2", tmp().path(), 30);
        assert_eq!(r.status, Status::Pass);
        assert!(r.out.len() > 3_000_000);
        assert!(generic(&r.out).ends_with("299999\n300000"));
    }

    #[test]
    fn run_reports_in_order_within_a_multibyte_budget() {
        let dir = tmp();
        let cwd = dir.path().to_str().unwrap();
        let out = run(json!({
            "cwd": cwd, "maxChars": 600, "concurrency": 3, "toolSummary": "x",
            "commands": [
                {"command": "sleep 0.5; echo 'All 3 tests passed'"},
                {"name": "utf8", "command": "sleep 0.5; for i in $(seq 1 300); do echo 'привіт 🌍 помилка'; done; exit 3"},
                {"command": "sleep 0.5; exit 137"},
            ]
        }))
        .unwrap();
        assert!(out.len() <= 600, "{} bytes", out.len());
        assert!(out.starts_with("Checks: 1/3 passed, 2 failed (wall: "), "{out}");
        let wall: u64 = out.split("wall: ").nth(1).unwrap().split("ms").next().unwrap().parse().unwrap();
        assert!(wall < 1400, "not concurrent: {wall}ms");
        let pass = out.find("- [PASS ").unwrap();
        let utf8 = out.find("## [FAIL (exit 3) ").unwrap();
        assert!(pass < utf8 && out[utf8..].contains("] utf8\n$ sleep 0.5;"), "{out}");
        assert!(out.contains("All 3 tests passed") && out.contains("FAIL (exit 137 / SIGKILL - possible OOM)"), "{out}");

        let full = run(json!({"cwd": cwd, "outputMode": "full", "commands": [{"command": "printf 'alpha\\nbeta\\n'"}]})).unwrap();
        assert!(full.contains("## [PASS ") && full.ends_with("printf 'alpha\\nbeta\\n'\nalpha\nbeta"), "{full}");
    }

    #[test]
    fn bad_input_is_an_error() {
        let cwd = tmp();
        let cwd = cwd.path().to_str().unwrap();
        for args in [
            json!({"cwd": cwd, "commands": []}),
            json!({"cwd": cwd, "commands": [{"command": "  "}]}),
            json!({"cwd": cwd, "commands": [{"command": "true", "cwd": "missing"}]}),
            json!({"cwd": cwd, "commands": [{"command": "true"}], "outputMode": "loud"}),
            json!({"cwd": cwd, "commands": "true"}),
        ] {
            assert!(run(args.clone()).unwrap_err().starts_with("Error: "), "{args}");
        }
    }

    #[test]
    fn splits_only_plain_and_chains() {
        use Step::{Cd, Run};
        let split = split_chain;
        assert_eq!(split("cargo check --all-targets && cargo test"), Some(vec![Run("cargo check --all-targets"), Run("cargo test")]));
        assert_eq!(split("cd crates/x && cargo test"), Some(vec![Cd("crates/x"), Run("cargo test")]));
        assert_eq!(split("cd 'a b'&&RUSTFLAGS=-Dwarnings cargo check"), Some(vec![Cd("a b"), Run("RUSTFLAGS=-Dwarnings cargo check")]));
        assert_eq!(split("echo 'x && y; z' && echo \"$(date) | ok\""), Some(vec![Run("echo 'x && y; z'"), Run("echo \"$(date) | ok\"")]));
        for c in [
            "cargo check",
            "cargo test -- \"a && b\"",
            "cargo check 2>&1 | tail && cargo test",
            "cargo check; cargo test && true",
            "cargo check || true && cargo test",
            "(cd x && cargo test)",
            "cargo test && cat log > out",
            "sleep 1 & cargo check && cargo test",
            "cargo check && $(echo cargo) test",
            "true && { cargo test; }",
            "export RUST_LOG=1 && cargo test",
            "source env.sh && cargo test",
            ". env.sh && cargo test",
            "set -e && cargo test",
            "X=1 && cargo test",
            "FOO=1 cd x && cargo test",
            "cd && cargo test",
            "cd - && cargo test",
            "cd $HOME && cargo test",
            "cd a b && cargo test",
            "cargo check &&",
            "cargo check && 'unterminated",
            "cargo check && cargo test \\\n --all",
        ] {
            assert_eq!(split(c), None, "{c}");
        }
    }

    #[test]
    fn chain_stops_at_first_failure_and_names_the_rest() {
        let dir = tmp();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/marker"), "").unwrap();
        let r = execute("cd sub && test -f marker && echo 'All 3 tests passed'", dir.path(), 10);
        assert_eq!(r.status, Status::Pass);
        assert_eq!(chain_body(&r, false, budget::CAP).lines().count(), 2, "{}", chain_body(&r, false, budget::CAP));

        let r = execute("true && sh -c 'echo boom; exit 4' && touch ran && cd nowhere && echo never", dir.path(), 10);
        assert_eq!(r.status, Status::Fail("exit 4".into()));
        let b = body(&r, false, budget::CAP);
        assert!(b.contains("  [PASS ") && b.contains("  [FAIL (exit 4) ") && b.contains("\nboom\n"), "{b}");
        assert!(b.ends_with("Not run: `touch ran`, `echo never`"), "{b}");
        assert!(!dir.path().join("ran").exists());

        let r = execute("cd missing && true", dir.path(), 10);
        assert_eq!(r.status, Status::Fail("cd: no such directory: missing".into()));
        assert!(body(&r, false, budget::CAP).ends_with("Not run: `true`"));

        // The timeout covers the whole chain, not each step.
        let r = execute("sleep 0.7 && sleep 0.7 && sleep 0.7", dir.path(), 1);
        assert_eq!(r.status, Status::Timeout);
        assert!(r.ms < 1800, "took {}ms", r.ms);
        assert!(body(&r, false, budget::CAP).contains("TIMED OUT after 1s") && body(&r, false, budget::CAP).ends_with("Not run: `sleep 0.7`"));
    }

    #[test]
    fn several_test_results_aggregate_into_one_line() {
        let body = "warning: unused import\ntest result: ok. 3 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s\ntest result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.20s\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s";
        assert_eq!(summary(body), "test result: ok. 15 passed; 0 failed; 1 ignored; 2 filtered out (3 suites)");
        let one = "test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s";
        assert_eq!(summary(one), one);
    }

    #[test]
    fn real_cargo_chain_passes_then_fails_on_a_type_error() {
        let dir = tmp();
        let p = dir.path();
        std::fs::create_dir_all(p.join("src")).unwrap();
        std::fs::create_dir_all(p.join("tests")).unwrap();
        std::fs::write(p.join("Cargo.toml"), "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        std::fs::write(p.join("src/lib.rs"), "pub fn add(a: i32, b: i32) -> i32 { a + b }\n#[test]\nfn unit() { assert_eq!(add(1, 2), 3); }\n").unwrap();
        std::fs::write(p.join("tests/it.rs"), "#[test]\nfn it() { assert_eq!(demo::add(2, 2), 4); }\n").unwrap();
        let chain = "cargo check --all-targets && cargo test";
        let v = verify(chain, p, 120, 1000);
        assert!(v.starts_with("Verify [PASS in "), "{v}");
        assert!(v.contains("] cargo check --all-targets: 0 errors, 0 warnings\n"), "{v}");
        assert!(v.contains("] cargo test: test result: ok. 2 passed; 0 failed; 0 ignored (3 suites)"), "{v}");
        assert_eq!(v.lines().count(), 3, "{v}");

        std::fs::write(p.join("src/lib.rs"), "pub fn add(a: i32, b: i32) -> i32 { let x: i32 = \"five\"; a + b + x }\n").unwrap();
        let v = verify(chain, p, 120, 1000);
        assert!(v.starts_with("Verify [FAIL (exit 101) in "), "{v}");
        assert!(v.contains("src/lib.rs:1:50 [E0308] mismatched types") && v.contains("1 error, 0 warnings"), "{v}");
        assert!(v.ends_with("Not run: `cargo test`"), "{v}");
    }

    #[test]
    fn verify_chain_stays_within_budget() {
        let dir = tmp();
        std::fs::write(dir.path().join("big.sh"), "seq 1 5000; exit 2").unwrap();
        let v = verify("true && sh big.sh && echo never", dir.path(), 10, 400);
        assert!(v.starts_with("Verify [FAIL (exit 2) in ") && v.len() <= 400 && v.ends_with("Not run: `echo never`"), "{v}");
    }

    const EXE: &str = "/opt/agy/agyfast-rs";

    fn rewritten(command: &str) -> Option<String> {
        let payload = json!({"tool_name": "Bash", "tool_input": {"command": command}}).to_string();
        pre_bash(&payload, EXE).map(|v| v["hookSpecificOutput"]["updatedInput"]["command"].as_str().unwrap().to_string())
    }

    #[test]
    fn hook_rewrites_a_cargo_check_and_copies_the_whole_input() {
        let payload = json!({
            "hook_event_name": "PreToolUse", "tool_name": "Bash", "cwd": "/w",
            "tool_input": {"command": "cargo check --all-targets", "description": "check", "timeout": 120000,
                           "run_in_background": false, "future_field": {"x": [1]}}
        });
        let out = pre_bash(&payload.to_string(), EXE).unwrap();
        assert_eq!(
            out,
            json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "updatedInput": {
                "command": "cargo check --message-format=json --all-targets 2>&1 | /opt/agy/agyfast-rs cli filter 'cargo check --all-targets'",
                "description": "check", "timeout": 120000, "run_in_background": false, "future_field": {"x": [1]}}}})
        );
        assert!(out["hookSpecificOutput"].get("permissionDecision").is_none());
        // A path that needs quoting gets single quotes.
        let odd = pre_bash(&payload.to_string(), "/My Tools/agyfast-rs").unwrap();
        assert!(odd.to_string().contains("| '/My Tools/agyfast-rs' cli filter"), "{odd}");
    }

    #[test]
    fn hook_rewrites_each_check_step_of_a_chain() {
        assert_eq!(
            rewritten("cd crates/x && cargo check && cargo test foo").as_deref(),
            Some("cd crates/x && cargo check --message-format=json 2>&1 | /opt/agy/agyfast-rs cli filter 'cargo check' && cargo test foo 2>&1 | /opt/agy/agyfast-rs cli filter 'cargo test foo'")
        );
        // A trailing 2>&1 is the model's habit, not a wish for raw output; other steps stay as written.
        assert_eq!(
            rewritten("cargo test 2>&1").as_deref(),
            Some("cargo test 2>&1 | /opt/agy/agyfast-rs cli filter 'cargo test'")
        );
        assert_eq!(
            rewritten("cargo check --all-targets 2>&1 && echo done 2>&1").as_deref(),
            Some("cargo check --message-format=json --all-targets 2>&1 | /opt/agy/agyfast-rs cli filter 'cargo check --all-targets' && echo done 2>&1")
        );
        assert_eq!(
            rewritten("cargo run && cargo clippy").as_deref(),
            Some("cargo run && cargo clippy --message-format=json 2>&1 | /opt/agy/agyfast-rs cli filter 'cargo clippy'")
        );
    }

    #[test]
    fn hook_quotes_the_original_command_for_the_shell() {
        let out = rewritten(r#"cargo test "it's" -- --exact"#).unwrap();
        assert_eq!(out, r#"cargo test "it's" -- --exact 2>&1 | /opt/agy/agyfast-rs cli filter 'cargo test "it'\''s" -- --exact'"#);
        // The shell hands the filter the original command back byte for byte.
        let echoed = Command::new("sh").arg("-c").arg(format!("printf %s {}", sh_quote(r#"cargo test "it's" $HOME `x`"#))).output().unwrap();
        assert_eq!(String::from_utf8(echoed.stdout).unwrap(), r#"cargo test "it's" $HOME `x`"#);
    }

    #[test]
    fn hook_leaves_raw_output_and_odd_shapes_alone() {
        for c in [
            "cargo check 2>&1 | tail -30",
            "cargo test | grep FAILED",
            "cargo test > log 2>&1",
            "cargo test 2>&1 foo",
            "cargo test 2>&1 &",
            "cargo check; cargo test",
            "(cd x && cargo test)",
            "export X=1 && cargo test",
            "cargo run",
            "cargo test -- --nocapture",
            "cargo test -- --show-output",
            "cargo check --message-format=short",
            "cargo install ripgrep",
            "cargo add serde",
            "cargo fmt",
            "cargo publish",
            "agyfast-rs cli check 'cargo test'",
            "cargo test 2>&1 | /x/agyfast-rs cli filter 'cargo test'",
            "cd x && ls",
            "go build ./...",
            "",
        ] {
            assert_eq!(rewritten(c), None, "{c}");
        }
        let bash = |input: Value| json!({"tool_name": "Bash", "tool_input": input}).to_string();
        for payload in [
            bash(json!({"command": "cargo test", "run_in_background": true})),
            bash(json!({"command": "cargo test", "run_in_background": "yes"})),
            bash(json!({"command": 7})),
            bash(json!({})),
            json!({"tool_name": "Read", "tool_input": {"command": "cargo test"}}).to_string(),
            json!({"tool_name": "Bash", "tool_input": "cargo test"}).to_string(),
            "{not json".to_string(),
            String::new(),
        ] {
            assert_eq!(pre_bash(&payload, EXE), None, "{payload}");
        }
    }

    #[test]
    fn filter_reads_failure_from_the_content() {
        let (out, failed) = filter("cargo check", CARGO_CHECK);
        assert!(failed && out.starts_with("[agyfast-rs FAIL] cargo check\nsrc/main.rs:6:18 [E0308]") && out.ends_with("1 error, 1 warning"), "{out}");
        let (out, failed) = filter("cargo clippy", CARGO_CLIPPY);
        assert!(!failed && out == "[agyfast-rs PASS] cargo clippy: 0 errors, 2 warnings", "{out}");
        let (out, failed) = filter("cargo test", CARGO_TEST);
        assert!(failed && out.contains("test result: FAILED. 1 passed; 1 failed") && !out.contains("ok_one"), "{out}");
        assert!(filter("go test ./...", GO_TEST).1);
        assert!(filter("npx tsc --noEmit", "src/a.ts(12,5): error TS2322: Type 'string' is not assignable.").1);
        assert!(!filter("npx eslint src", "/r/a.ts\n  9:1  warning  x  no-console\n\n✖ 1 problem (0 errors, 1 warning)").1);
        assert!(!filter("pytest", "===== 3 passed in 0.10s =====").1);
        assert!(filter("pytest", "FAILED t.py::bad - assert 1\n===== 1 failed, 2 passed in 0.10s =====").1);
        // Without cargo's JSON (a manifest error), cargo's own error line decides.
        assert!(filter("cargo check", "error: failed to parse manifest at `/w/Cargo.toml`").1);
        let lines: Vec<String> = (1..=500).map(|i| format!("line {i}")).collect();
        let (out, failed) = filter("make all", &lines.join("\n"));
        assert!(!failed && out.starts_with("[agyfast-rs] make all (status not judged)\nline 1\n") && out.ends_with("line 500"), "{out}");
        assert!(filter("cargo test", &"x".repeat(50_000)).0.len() <= budget::CAP);
    }

    #[test]
    fn filter_judges_real_cargo_output() {
        let dir = tmp();
        let p = dir.path();
        std::fs::create_dir_all(p.join("src")).unwrap();
        std::fs::write(p.join("Cargo.toml"), "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        let sh = |c: &str| String::from_utf8_lossy(&Command::new("sh").arg("-c").arg(c).current_dir(p).output().unwrap().stdout).into_owned();
        let lib = "pub fn add(a: i32, b: i32) -> i32 { a + b }\n#[test]\nfn good() { assert_eq!(add(1, 2), 3); }\n";
        std::fs::write(p.join("src/lib.rs"), lib).unwrap();
        let (out, failed) = filter("cargo check", &sh("cargo check --message-format=json 2>&1"));
        assert_eq!((out.as_str(), failed), ("[agyfast-rs PASS] cargo check: 0 errors, 0 warnings", false));

        std::fs::write(p.join("src/lib.rs"), format!("{lib}#[test]\nfn bad() {{ assert_eq!(add(2, 2), 5, \"math is broken\"); }}\n")).unwrap();
        let (out, failed) = filter("cargo test", &sh("cargo test 2>&1"));
        assert!(failed && out.starts_with("[agyfast-rs FAIL] cargo test\n") && out.contains("math is broken") && out.contains("test result: FAILED. 1 passed; 1 failed"), "{out}");

        std::fs::write(p.join("src/lib.rs"), "pub fn add(a: i32) -> i32 { let x: i32 = \"five\"; a + x }\n").unwrap();
        let (out, failed) = filter("cargo check", &sh("cargo check --message-format=json 2>&1"));
        assert!(failed && out.contains("src/lib.rs:1:42 [E0308] mismatched types") && out.ends_with("1 error, 0 warnings"), "{out}");
    }

    #[test]
    fn verify_reports_status_within_budget() {
        let dir = tmp();
        let v = verify("echo '1 passed'", dir.path(), 0, 300);
        assert!(v.starts_with("Verify [PASS in ") && v.ends_with("`echo '1 passed'`:\n1 passed"), "{v}");
        let v = verify("seq 1 5000; exit 2", dir.path(), 5, 400);
        assert!(v.starts_with("Verify [FAIL (exit 2) in ") && v.len() <= 400 && v.ends_with("5000"), "{v}");
        assert!(verify("sleep 5", dir.path(), 1, 300).starts_with("Verify [TIMEOUT in "));
    }
}
