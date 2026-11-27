//! read_slices: outlines plus exact 1-based inclusive line ranges in one call.

use crate::core::{budget, cwd};
use crate::tools::outline::{outline_file, read_text};
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Args {
    cwd: Option<String>,
    #[serde(default)]
    outlines: Vec<String>,
    #[serde(default)]
    line_ranges: Vec<String>,
    max_chars: Option<usize>,
}

enum Job {
    Outline(String),
    /// Each range, or why it could not be parsed.
    Slice(String, Vec<Result<(i64, i64), String>>),
}

pub fn run(args: Value) -> Result<String, String> {
    let args: Args = serde_json::from_value(args).map_err(|e| format!("Error: invalid arguments: {e}"))?;
    if args.outlines.is_empty() && args.line_ranges.is_empty() {
        return Err("Error: provide at least one of `outlines` or `lineRanges`".into());
    }
    if args.outlines.len() > 40 {
        return Err(format!("Error: at most 40 outlines, got {}", args.outlines.len()));
    }
    let dir = cwd::resolve(args.cwd.as_deref())?;
    let jobs: Vec<Job> = args.outlines.into_iter().map(Job::Outline)
        .chain(args.line_ranges.iter().map(|s| parse_line_ranges(s)))
        .collect();
    let parts: Vec<String> = jobs.par_iter().map(|job| match job {
        Job::Outline(p) => outline_file(&dir.join(p), p),
        Job::Slice(p, ranges) => slice_file(&dir.join(p), p, ranges),
    }).collect();
    Ok(budget::join_fair(&parts, budget::limit(args.max_chars, 1000)))
}

/// `"path:start-end,start2-end2"`; a bare `N` means line N. The last `:` splits, so Windows drives work.
fn parse_line_ranges(spec: &str) -> Job {
    let Some((path, list)) = spec.rsplit_once(':') else {
        return Job::Slice(spec.into(), vec![Err("expected \"path:start-end[,start-end]\"".into())]);
    };
    let ranges = list.split(',').map(|r| {
        let (a, b) = r.split_once('-').unwrap_or((r, r));
        match (a.trim().parse(), b.trim().parse()) {
            (Ok(a), Ok(b)) => Ok((a, b)),
            _ => Err(format!("bad range {r:?} in {spec:?}")),
        }
    }).collect();
    Job::Slice(path.into(), ranges)
}

pub fn slice_file(path: &Path, shown: &str, ranges: &[Result<(i64, i64), String>]) -> String {
    let text = match read_text(path) {
        Ok(t) => t,
        Err(e) => return format!("## Slice: {shown}\nERROR: {e}"),
    };
    slice_text(shown, &text, ranges)
}

pub(crate) fn slice_text(shown: &str, text: &str, ranges: &[Result<(i64, i64), String>]) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let n = lines.len() as i64;
    let mut notes = Vec::new();
    let mut ok: Vec<(i64, i64)> = Vec::new();
    for r in ranges {
        let &(a, b) = match r {
            Ok(r) => r,
            Err(e) => {
                notes.push(format!("ERROR: {e}"));
                continue;
            }
        };
        if a > b {
            notes.push(format!("--- L{a}-{b}: start is after end ---"));
        } else if n == 0 || a > n || b < 1 {
            notes.push(format!("--- L{a}-{b}: out of bounds; file has {n} lines ---"));
        } else {
            ok.push((a.max(1), b.min(n)));
        }
    }
    ok.sort_unstable();
    let mut merged: Vec<(i64, i64)> = Vec::new();
    for (a, b) in ok {
        match merged.last_mut() {
            Some(last) if a <= last.1 + 1 => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    let mut out = vec![format!("## Slice: {shown} ({n} lines)")];
    for (a, b) in merged {
        out.push(format!("--- L{a}-{b} ---"));
        let width = b.to_string().len();
        out.extend((a..=b).map(|i| format!("{i:>width$}: {}", lines[i as usize - 1])));
    }
    out.extend(notes);
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn clamps_merges_and_reports_bad_ranges_inline() {
        let dir = tempfile::tempdir().unwrap();
        let body: String = (1..=12).map(|i| format!("рядок {i} 🌍\n")).collect();
        std::fs::write(dir.path().join("a.txt"), body).unwrap();
        let out = run(json!({
            "cwd": dir.path(),
            "lineRanges": ["a.txt:3-4,4-6,11-50,99-120,5-2", "a.txt:1-2,7", "missing.txt:1-5", "a.txt:x-y"],
            "toolSummary": "ignored"
        })).unwrap();
        assert!(out.contains("## Slice: a.txt (12 lines)\n--- L3-6 ---\n3: рядок 3 🌍\n4: рядок 4 🌍"), "{out}");
        assert!(out.contains("--- L11-12 ---\n11: рядок 11 🌍\n12: рядок 12 🌍"));
        assert!(out.contains("--- L99-120: out of bounds; file has 12 lines ---"));
        assert!(out.contains("--- L5-2: start is after end ---"));
        assert!(out.contains("--- L1-2 ---\n1: рядок 1 🌍\n2: рядок 2 🌍\n--- L7-7 ---\n7: рядок 7 🌍"));
        assert!(out.contains("## Slice: missing.txt\nERROR:"));
        assert!(out.contains("ERROR: bad range \"x-y\""));
    }

    #[test]
    fn empty_file_range_reports_out_of_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty.txt");
        std::fs::write(&empty, "").unwrap();
        let out = slice_file(&empty, "empty.txt", &[Ok((0, 5)), Ok((1, 5))]);
        assert!(out.contains("## Slice: empty.txt (0 lines)"), "{out}");
        assert!(out.contains("--- L0-5: out of bounds; file has 0 lines ---"), "{out}");
        assert!(out.contains("--- L1-5: out of bounds; file has 0 lines ---"), "{out}");
        assert!(!out.contains("--- L1-0 ---"), "{out}");
    }

    #[test]
    fn outlines_and_empty_requests() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("r.md"), "# A\ntext\n").unwrap();
        let out = run(json!({"cwd": dir.path(), "outlines": ["r.md"]})).unwrap();
        assert_eq!(out, "## r.md (2 lines)\nL1-2 # A");
        assert!(run(json!({"cwd": dir.path()})).unwrap_err().starts_with("Error: "));
    }
}
