//! `agyfast-rs cli <tool> ...`: the same five tools without an MCP server, for
//! swarm workers and plain shells. Flags are translated to the MCP argument
//! JSON and go through the same dispatch.

use crate::{core, tools};
use serde_json::{Map, Value, json};
use std::io::Read;

const HELP: &str = r#"agyfast-rs cli <tool> [--cwd DIR] [--max-chars N] [--json '{...}' | --stdin] ...
  search   [modifiers] --text|--regex|--glob|--fuzzy|--git PATTERN  (repeatable; modifiers apply to the next query)
           modifiers: --path P --glob-filter G --file-type T --output-mode content|files_with_matches|count
                      --sort-by path|mtime --context N --max-results N --ignore-case --multiline --definition
  outline  FILE[#NAME[,NAME]]...
  slices   [--outline FILE]... FILE:START-END[,START-END]...
  edit     [--best-effort] [--dry-run] [--verify CMD] [--verify-timeout S] [--max-verify-chars N] --stdin   ({"edits":[...]} on stdin)
  check    [--compact|--full] [--concurrency N] [--timeout S] "CMD"...
  filter   "CMD"   compress CMD's output read from stdin (runs nothing); first line [agyfast-rs PASS|FAIL],
                   exit 1 when the output shows failure (used by `agyfast-rs hook pre-bash`)
  snapshot
  worker-brief   print the /swarm worker brief with this binary's path filled in; set --cwd <WORKSPACE> before pasting
--json / --stdin take the tool's full MCP argument object; flags given after them override it."#;

/// The `/swarm` worker brief; `<AGY>` becomes the running binary's quoted path (a call expression on Windows).
const WORKER_BRIEF: &str = r#"Navigate with ranged read_file (<=200 lines per call, parallel) plus the agyfast-rs CLI; reads are the largest cost, so add --definition to queries whose hits you will read or edit: each hit comes back with its whole definition, and you edit from those lines. Put per-query modifiers before their query:
  <AGY> cli search --cwd <WORKSPACE> --path src --definition --text "foo" --git "status --short"
  <AGY> cli outline --cwd <WORKSPACE> a.rs b.ts#Name,other   (file#Name prints those definitions' source)
  <AGY> cli slices --cwd <WORKSPACE> b.rs:10-60,90-120
  <AGY> cli edit --cwd <WORKSPACE> --best-effort --verify "cargo test <module>::tests::<test_name>" --stdin   (stdin: {"edits":[...]})
  <AGY> cli check --cwd <WORKSPACE> --compact "cargo test <module>::tests::<test_name>"
Run every build/test/lint command through cli check. Discipline: smallest working diff; reuse existing abstractions; grep all callers before a bug fix; one small test for non-trivial logic.
Rust in parallel workers: use the shared target/ and never set CARGO_TARGET_DIR (a cold build costs far more than waiting on Cargo's lock); run only your own test by name; leave cargo check, clippy and the full test run to the coordinator.
The built-in edit_file replaces every match of its target, so make each target unique (include a neighbouring line only your block has) and never edit lines another worker owns."#;

/// How a shell is told to run `exe`: POSIX single quotes, or PowerShell's call operator.
fn invoke(exe: &str, win: bool) -> String {
    if win { format!("& '{}'", exe.replace('\'', "''")) } else { tools::check::sh_quote(exe) }
}

fn worker_brief() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("Error: cannot locate this executable: {e}"))?;
    Ok(WORKER_BRIEF.replace("<AGY>", &invoke(&exe.display().to_string(), cfg!(windows))))
}

fn object(text: &str) -> Result<Map<String, Value>, String> {
    let text = text.trim_start_matches('\u{feff}');
    match serde_json::from_str(text) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err("Error: expected a JSON object of tool arguments".into()),
        Err(e) => Err(format!("Error: invalid JSON arguments: {e}")),
    }
}

fn build(tool: &str, args: &[String]) -> Result<Value, String> {
    let mut top = Map::new();
    let mut query = Map::new(); // modifiers waiting for the next search query
    let mut items: Vec<Value> = Vec::new();
    let mut outlines: Vec<Value> = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("Error: {arg} needs a value"));
        let number = |s: String| s.parse::<u64>().map_err(|_| format!("Error: {arg} needs a number, got `{s}`"));
        match arg.as_str() {
            "--json" => top.extend(object(&value()?)?),
            "--stdin" => {
                let mut text = String::new();
                std::io::stdin().read_to_string(&mut text).map_err(|e| format!("Error: reading stdin: {e}"))?;
                top.extend(object(&text)?);
            }
            "--cwd" => drop(top.insert("cwd".into(), value()?.into())),
            "--max-chars" => drop(top.insert("maxChars".into(), number(value()?)?.into())),
            "--concurrency" => drop(top.insert("concurrency".into(), number(value()?)?.into())),
            "--timeout" => drop(top.insert("timeoutSec".into(), number(value()?)?.into())),
            "--compact" | "--full" => drop(top.insert("outputMode".into(), arg[2..].into())),
            "--best-effort" => drop(top.insert("bestEffort".into(), true.into())),
            "--dry-run" => drop(top.insert("dryRun".into(), true.into())),
            "--verify" => drop(top.insert("verify".into(), value()?.into())),
            "--verify-timeout" => drop(top.insert("verifyTimeoutSec".into(), number(value()?)?.into())),
            "--max-verify-chars" => drop(top.insert("maxVerifyChars".into(), number(value()?)?.into())),
            "--outline" => outlines.push(value()?.into()),
            "--type" => drop(query.insert("type".into(), value()?.into())),
            "--path" => drop(query.insert("path".into(), value()?.into())),
            "--glob-filter" => drop(query.insert("glob".into(), value()?.into())),
            "--file-type" => drop(query.insert("fileType".into(), value()?.into())),
            "--output-mode" => drop(query.insert("outputMode".into(), value()?.into())),
            "--sort-by" => drop(query.insert("sortBy".into(), value()?.into())),
            "--context" => drop(query.insert("contextLines".into(), number(value()?)?.into())),
            "--max-results" => drop(query.insert("maxResults".into(), number(value()?)?.into())),
            "--ignore-case" => drop(query.insert("caseSensitive".into(), false.into())),
            "--multiline" => drop(query.insert("multiline".into(), true.into())),
            "--definition" => drop(query.insert("definition".into(), true.into())),
            "--text" | "--regex" | "--glob" | "--fuzzy" | "--git" | "--pattern" => {
                let mut q = std::mem::take(&mut query);
                if arg != "--pattern" {
                    q.insert("type".into(), arg[2..].into());
                }
                q.entry("type").or_insert("text".into());
                q.insert("pattern".into(), value()?.into());
                items.push(q.into());
            }
            flag if flag.starts_with("--") => return Err(format!("Error: unknown flag {flag}\n{HELP}")),
            positional if tool == "batch_search" => {
                let mut q = std::mem::take(&mut query);
                q.entry("type").or_insert("text".into());
                q.insert("pattern".into(), positional.into());
                items.push(q.into());
            }
            positional => items.push(positional.into()),
        }
    }
    if !query.is_empty() {
        return Err("Error: search modifiers (--path, --definition, etc.) must appear before a search query, not after all queries or on another tool".into());
    }
    let mut set = |key: &str, list: Vec<Value>| {
        if !list.is_empty() {
            top.insert(key.into(), list.into());
        }
    };
    match tool {
        "batch_search" => set("queries", items),
        "quick_outline" => set("paths", items),
        "read_slices" => {
            set("lineRanges", items);
            set("outlines", outlines);
        }
        "run_checks" => set("commands", items.into_iter().map(|c| json!({"command": c})).collect()),
        _ => {}
    }
    Ok(top.into())
}

const FILTER_HEAD: usize = 1 << 20;
const FILTER_TAIL: usize = 15 << 20;

/// Read `r` to EOF, keeping the first 1 MiB and last 15 MiB when total > 16 MiB.
fn read_bounded(mut r: impl Read) -> std::io::Result<Vec<u8>> {
    let (mut head, mut tail, mut tail_pos, mut total) = (Vec::new(), Vec::new(), 0usize, 0usize);
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n;
        let mut s = &buf[..n];
        if head.len() < FILTER_HEAD {
            let take = (FILTER_HEAD - head.len()).min(s.len());
            head.extend_from_slice(&s[..take]);
            s = &s[take..];
        }
        if !s.is_empty() {
            if s.len() >= FILTER_TAIL {
                tail.clear();
                tail.extend_from_slice(&s[s.len() - FILTER_TAIL..]);
                tail_pos = 0;
            } else if tail.len() < FILTER_TAIL {
                let take = (FILTER_TAIL - tail.len()).min(s.len());
                tail.extend_from_slice(&s[..take]);
                let rem = &s[take..];
                if !rem.is_empty() {
                    let first = FILTER_TAIL.min(rem.len());
                    tail[..first].copy_from_slice(&rem[..first]);
                    tail_pos = first % FILTER_TAIL;
                }
            } else {
                let first = (FILTER_TAIL - tail_pos).min(s.len());
                tail[tail_pos..tail_pos + first].copy_from_slice(&s[..first]);
                let rem = &s[first..];
                if !rem.is_empty() {
                    tail[..rem.len()].copy_from_slice(rem);
                    tail_pos = rem.len();
                } else {
                    tail_pos = (tail_pos + first) % FILTER_TAIL;
                }
            }
        }
    }
    if total <= FILTER_HEAD + FILTER_TAIL {
        head.extend_from_slice(&tail);
        return Ok(head);
    }
    let omitted = total - (FILTER_HEAD + FILTER_TAIL);
    head.extend_from_slice(format!("\n... [{omitted} bytes of output omitted] ...\n").as_bytes());
    head.extend_from_slice(&tail[tail_pos..]);
    head.extend_from_slice(&tail[..tail_pos]);
    Ok(head)
}

pub fn run(args: &[String]) -> i32 {
    let tool = match args.first().map(String::as_str) {
        Some("search") => "batch_search",
        Some("outline") => "quick_outline",
        Some("slices") => "read_slices",
        Some("edit") => "apply_edits",
        Some("check") => "run_checks",
        Some("snapshot") => {
            return match core::cwd::resolve(args.iter().skip_while(|a| *a != "--cwd").nth(1).map(String::as_str)) {
                Ok(dir) => {
                    println!("{}", tools::snapshot::snapshot(&dir));
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            };
        }
        Some("filter") => {
            let [_, command] = args else {
                eprintln!("{HELP}");
                return 2;
            };
            let raw = match read_bounded(std::io::stdin().lock()) {
                Ok(raw) => raw,
                Err(e) => {
                    eprintln!("Error: reading stdin: {e}");
                    return 1;
                }
            };
            let (text, failed) = tools::check::filter(command, &String::from_utf8_lossy(&raw));
            println!("{text}");
            return i32::from(failed);
        }
        Some("worker-brief") => {
            return match worker_brief() {
                Ok(brief) => {
                    println!("{brief}");
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            };
        }
        Some("--help" | "-h") => {
            println!("{HELP}");
            return 0;
        }
        _ => {
            eprintln!("{HELP}");
            return 2;
        }
    };
    match build(tool, &args[1..]).and_then(|a| tools::call(tool, a)) {
        Ok(text) => {
            println!("{text}");
            0
        }
        Err(text) => {
            println!("{text}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn search_modifiers_bind_to_the_next_query_only() {
        let v = build("batch_search", &args(&["--cwd", "/w", "--path", "src", "--ignore-case", "--definition", "--text", "foo", "--git", "status --short"])).unwrap();
        assert_eq!(
            v,
            json!({"cwd": "/w", "queries": [
                {"type": "text", "pattern": "foo", "path": "src", "caseSensitive": false, "definition": true},
                {"type": "git", "pattern": "status --short"}
            ]})
        );
    }

    #[test]
    fn other_tools_map_positionals_and_flags() {
        assert_eq!(build("quick_outline", &args(&["a.rs", "b.ts"])).unwrap(), json!({"paths": ["a.rs", "b.ts"]}));
        assert_eq!(
            build("read_slices", &args(&["--outline", "a.rs", "b.rs:1-20,40-50"])).unwrap(),
            json!({"outlines": ["a.rs"], "lineRanges": ["b.rs:1-20,40-50"]})
        );
        assert_eq!(
            build("run_checks", &args(&["--compact", "--timeout", "30", "cargo check", "cargo test"])).unwrap(),
            json!({"outputMode": "compact", "timeoutSec": 30, "commands": [{"command": "cargo check"}, {"command": "cargo test"}]})
        );
        assert_eq!(
            build("apply_edits", &args(&["--json", r#"{"edits":[]}"#, "--best-effort"])).unwrap(),
            json!({"edits": [], "bestEffort": true})
        );
        assert_eq!(object("\u{feff}{\"edits\":[]}").unwrap().get("edits"), Some(&json!([])));
        assert_eq!(object("\u{feff}\u{feff}{\"edits\":[]}").unwrap().get("edits"), Some(&json!([])));
        assert_eq!(build("apply_edits", &args(&["--json", "\u{feff}{\"edits\":[]}"])).unwrap(), json!({"edits": []}));
        assert!(build("run_checks", &args(&["--bogus"])).is_err());
        assert!(build("run_checks", &args(&["--timeout", "soon"])).is_err());
    }

    #[test]
    fn worker_brief_names_this_executable() {
        let brief = worker_brief().unwrap();
        let exe = std::env::current_exe().unwrap();
        assert!(exe.is_absolute() && brief.contains(&format!("{} cli check --cwd <WORKSPACE>", invoke(&exe.display().to_string(), cfg!(windows)))), "{brief}");
        assert!(!brief.contains("<AGY>") && brief.contains("--definition"));
        assert!(!brief.contains("CARGO_TARGET_DIR=") && brief.contains("--compact \"cargo test <module>::tests::<test_name>\""));
    }

    #[test]
    fn invoke_quotes_per_shell() {
        assert_eq!(invoke(r"C:\Users\o'k\a.exe", true), r"& 'C:\Users\o''k\a.exe'");
        assert_eq!(invoke("/a b/x", false), "'/a b/x'");
    }

    #[test]
    fn positional_search_inherits_modifiers_and_trailing_modifiers_error() {
        assert_eq!(
            build("batch_search", &args(&["--path", "src", "--definition", "foo"])).unwrap(),
            json!({"queries": [{"type": "text", "pattern": "foo", "path": "src", "definition": true}]})
        );
        assert!(build("batch_search", &args(&["--text", "foo", "--path", "src"])).is_err());
        assert!(build("quick_outline", &args(&["--path", "src", "a.rs"])).is_err());
    }

    #[test]
    fn read_bounded_caps_over_16_mib() {
        assert_eq!(read_bounded(std::io::Cursor::new(b"hello")).unwrap(), b"hello");
        let mut input = vec![b'a'; FILTER_HEAD];
        input.extend_from_slice(&[b'm'; 100]);
        input.extend_from_slice(&vec![b'z'; FILTER_TAIL]);
        let out = read_bounded(std::io::Cursor::new(input)).unwrap();
        let marker = b"\n... [100 bytes of output omitted] ...\n";
        assert_eq!(out.len(), FILTER_HEAD + marker.len() + FILTER_TAIL);
        assert!(out.starts_with(&[b'a'; 16]));
        assert_eq!(&out[FILTER_HEAD..FILTER_HEAD + marker.len()], marker);
        assert!(out.ends_with(&[b'z'; 16]));
    }
}
