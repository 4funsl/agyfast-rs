//! `apply_edits`: exact-match hunks and file creation across 1-50 files,
//! all-or-nothing by default, cross-process locked, atomically written.

use crate::core::{budget, cwd, lock};
use serde::Deserialize;
use serde_json::Value;
use std::fs::{self, Permissions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Args {
    cwd: Option<String>,
    edits: Vec<Edit>,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    best_effort: bool,
    verify: Option<String>,
    verify_timeout_sec: Option<u64>,
    max_verify_chars: Option<usize>,
}

#[derive(Deserialize)]
struct Edit {
    path: String,
    hunks: Option<Vec<Hunk>>,
    create: Option<String>,
    #[serde(default)]
    overwrite: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Hunk {
    target: String,
    replacement: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
    #[serde(default)]
    replace_all: bool,
}

/// All edit entries for one real file (duplicate paths are merged in order).
struct Job {
    display: String,
    path: PathBuf,
    entries: Vec<Edit>,
}

struct Plan {
    display: String,
    path: PathBuf,
    new: Vec<u8>,
    /// Previous content and permissions, `None` for a new file; used for rollback.
    old: Option<(Vec<u8>, Permissions)>,
    summary: String,
}

fn line_count(s: &str) -> i64 {
    (s.matches('\n').count() + usize::from(!s.is_empty() && !s.ends_with('\n'))) as i64
}

fn line_of(starts: &[usize], offset: usize) -> usize {
    starts.partition_point(|&s| s <= offset)
}

fn line_range(starts: &[usize], offset: usize, len: usize) -> String {
    let (a, b) = (line_of(starts, offset), line_of(starts, offset + len.max(1) - 1));
    if a == b { format!("L{a}") } else { format!("L{a}-L{b}") }
}

fn snippet(lines: &[&str], from: usize, count: usize) -> String {
    lines[from..(from + count.clamp(1, 6)).min(lines.len())]
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let cut: String = l.chars().take(120).collect();
            format!("\n    {}: {cut}{}", from + i + 1, if cut.len() < l.len() { "..." } else { "" })
        })
        .collect()
}

/// Where the agent probably meant: whitespace-insensitive match first, then the
/// most distinctive target line found verbatim (trimmed) in the file.
fn closest(content: &str, target: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let tl: Vec<&str> = target.lines().collect();
    let want: String = target.split_whitespace().collect();
    if !want.is_empty() {
        let mut squashed = String::with_capacity(content.len());
        let mut sq_starts = Vec::with_capacity(lines.len());
        for l in &lines {
            sq_starts.push(squashed.len());
            for w in l.split_whitespace() {
                squashed.push_str(w);
            }
        }
        if let Some(pos) = squashed.find(&want) {
            let i = sq_starts.partition_point(|&s| s <= pos).saturating_sub(1);
            return format!(" Whitespace-insensitive match at line {} (fix indentation/spacing):{}", i + 1, snippet(&lines, i, tl.len()));
        }
    }
    let mut index: std::collections::HashMap<&str, (usize, usize)> = std::collections::HashMap::with_capacity(lines.len());
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        if t.len() >= 3 {
            index.entry(t).and_modify(|e| e.0 += 1).or_insert((1, i));
        }
    }
    let best = tl
        .iter()
        .enumerate()
        .map(|(idx, t)| (idx, t.trim()))
        .filter(|(_, t)| t.len() >= 3)
        .filter_map(|(idx, t)| index.get(t).map(|&(count, first)| (count, usize::MAX - t.len(), first.saturating_sub(idx))))
        .min();
    match best {
        Some((_, _, at)) => format!(" Closest match near line {}:{}", at + 1, snippet(&lines, at, tl.len())),
        None => " No similar line found; re-read the file.".into(),
    }
}

/// Byte window of lines `start..=end` (1-based); the target must lie fully inside it.
fn window(starts: &[usize], len: usize, s: Option<usize>, e: Option<usize>) -> Result<(usize, usize), String> {
    let total = if len > 0 && starts.last() == Some(&len) { starts.len() - 1 } else { starts.len() };
    let (s0, e0) = (s.unwrap_or(1), e.unwrap_or(total).min(total));
    if s0 == 0 || e == Some(0) {
        return Err("Invalid startLine/endLine: lines are 1-based".into());
    }
    if s0 > total {
        return Err(format!("startLine {s0} is past the end of the file ({total} lines)"));
    }
    if e0 < s0 {
        return Err(format!("Invalid line range: startLine {s0} > endLine {e0}"));
    }
    Ok((starts[s0 - 1], starts.get(e0).copied().unwrap_or(len)))
}

fn plan_hunks(display: &str, content: &str, hunks: &[&Hunk]) -> Result<(String, String), String> {
    let crs: Vec<_> = content.match_indices("\r\n").enumerate().map(|(i, (o, _))| o - i).collect();
    let normalized = if crs.is_empty() { std::borrow::Cow::Borrowed(content) } else { std::borrow::Cow::Owned(content.replace("\r\n", "\n")) };
    // CRs before a logical offset are the extra bytes at its on-disk position.
    let original = |o| o + crs.partition_point(|&cr| cr < o);
    let starts: Vec<_> = std::iter::once(0).chain(normalized.match_indices('\n').map(|(i, _)| i + 1)).collect();
    let mut spans = Vec::new();
    let mut errors = Vec::new();
    for (i, h) in hunks.iter().enumerate() {
        let (t, r) = (h.target.replace("\r\n", "\n"), h.replacement.replace("\r\n", "\n"));
        let err = |m: String| format!("{display} hunk #{}: {m}", i + 1);
        if t.is_empty() {
            errors.push(err("empty target".into()));
            continue;
        }
        if t == r {
            errors.push(err("replacement equals target (no-op)".into()));
            continue;
        }
        let (ws, we) = match window(&starts, normalized.len(), h.start_line, h.end_line) {
            Ok(w) => w,
            Err(m) => {
                errors.push(err(m));
                continue;
            }
        };
        let region = &normalized[ws..we];
        let found: Vec<_> = region.match_indices(&t).map(|(o, _)| ws + o).collect();
        if !h.replace_all && let [first] = found.as_slice() && region.rfind(&t) != Some(first - ws) {
            errors.push(err("target has overlapping matches. Add more context or use replaceAll: true.".into()));
            continue;
        }
        let ranged = h.start_line.is_some() || h.end_line.is_some();
        match found.len() {
            0 => {
                let outside: Vec<String> =
                    normalized.match_indices(&t).take(3).map(|(o, _)| line_range(&starts, o, t.len())).collect();
                let hint = if outside.is_empty() {
                    closest(&normalized, &t)
                } else {
                    format!(" Exact match found outside range at {} (adjust or omit startLine/endLine).", outside.join(", "))
                };
                let within = if ranged { format!(" within lines {}-{}", line_of(&starts, ws), line_of(&starts, we.max(1) - 1)) } else { String::new() };
                errors.push(err(format!("target not found{within}.{hint}")));
            }
            n if n > 1 && !h.replace_all => {
                let at: Vec<String> = found.iter().take(10).map(|&o| line_of(&starts, o).to_string()).collect();
                errors.push(err(format!(
                    "target matched {n} times (lines: {}). Add startLine/endLine, more context, or replaceAll: true.",
                    at.join(", ")
                )));
            }
            _ => spans.extend(found.into_iter().map(|o| {
                let (s, e) = (original(o), original(o + t.len()));
                let line = line_of(&starts, o);
                let line_end = starts.get(line).or_else(|| starts.get(line - 1));
                let crlf = line_end.is_some_and(|&end| end > 0 && crs.binary_search(&(end - 1)).is_ok());
                (s, e, if crlf { r.replace('\n', "\r\n") } else { r.clone() }, i, o)
            })),
        }
    }
    spans.sort_by_key(|s| (s.0, s.1));
    for w in spans.windows(2) {
        if w[1].0 < w[0].1 {
            errors.push(format!("{display}: hunk #{} and hunk #{} overlap near line {}", w[0].3 + 1, w[1].3 + 1, line_of(&starts, w[1].4)));
        }
    }
    if !errors.is_empty() {
        return Err(errors.join("\n- "));
    }
    let mut out = String::with_capacity(content.len());
    let mut cursor = 0;
    for (s, e, r, _, _) in &spans {
        out.push_str(&content[cursor..*s]);
        out.push_str(r);
        cursor = *e;
    }
    out.push_str(&content[cursor..]);
    let n = hunks.len();
    let reps = if spans.len() != n { format!(", {} replacements", spans.len()) } else { String::new() };
    let summary = format!(
        "- {display}: updated ({n} hunk{}{reps}), lines {:+}",
        if n == 1 { "" } else { "s" },
        line_count(&out) - line_count(content)
    );
    Ok((out, summary))
}

fn plan(job: &Job) -> Result<Plan, String> {
    let d = &job.display;
    let creates = job.entries.iter().filter(|e| e.create.is_some()).count();
    if creates > 0 && job.entries.len() > 1 {
        return Err(format!("{d}: listed {} times including `create`; give a created file once", job.entries.len()));
    }
    if job.entries.iter().any(|e| e.create.is_some() == e.hunks.is_some()) {
        return Err(format!("{d}: give exactly one of `hunks` or `create`"));
    }
    if job.entries.iter().any(|e| e.hunks.as_ref().is_some_and(Vec::is_empty)) {
        return Err(format!("{d}: `hunks` is empty"));
    }
    let existing = match fs::symlink_metadata(&job.path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{d}: cannot stat: {e}")),
        Ok(_) => match fs::metadata(&job.path) {
            Ok(m) if m.is_file() => Some(m.permissions()),
            Ok(_) => return Err(format!("{d}: not a regular file")),
            Err(_) => return Err(format!("{d}: dangling symlink, refusing to write through it")),
        },
    };
    let read = |perms: Permissions| fs::read(&job.path).map(|b| (b, perms)).map_err(|e| format!("{d}: cannot read: {e}"));
    if let Some(text) = &job.entries[0].create {
        return match existing {
            Some(_) if !job.entries[0].overwrite => Err(format!("{d}: already exists; set overwrite: true to replace it")),
            Some(perms) => {
                let old = read(perms)?;
                let delta = line_count(text) - line_count(&String::from_utf8_lossy(&old.0));
                Ok(Plan { display: d.clone(), path: job.path.clone(), new: text.clone().into_bytes(), old: Some(old), summary: format!("- {d}: overwritten, lines {delta:+}") })
            }
            None => Ok(Plan {
                display: d.clone(),
                path: job.path.clone(),
                new: text.clone().into_bytes(),
                old: None,
                summary: format!("- {d}: created, lines +{}", line_count(text)),
            }),
        };
    }
    let Some(perms) = existing else {
        return Err(format!("{d}: file not found (use `create` for new files)"));
    };
    let old = read(perms)?;
    let content = std::str::from_utf8(&old.0).map_err(|_| format!("{d}: not UTF-8 text, refusing to edit"))?;
    let hunks: Vec<&Hunk> = job.entries.iter().flat_map(|e| e.hunks.iter().flatten()).collect();
    let (new, summary) = plan_hunks(d, content, &hunks)?;
    Ok(Plan { display: d.clone(), path: job.path.clone(), new: new.into_bytes(), old: Some(old), summary })
}

/// Temp file in the target's directory, then rename over it: readers never see a partial file.
fn write_atomic(path: &Path, bytes: &[u8], perms: Option<&Permissions>, #[cfg_attr(not(windows), allow(unused_variables))] replaced: &mut bool) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::other("path has no parent directory"))?;
    fs::create_dir_all(dir)?;
    #[cfg_attr(not(unix), allow(unused_mut))] // only Unix sets permissions below
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    if perms.is_none() {
        builder.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o666)); // new files honour umask
    }
    let mut tmp = builder.tempfile_in(dir)?;
    #[cfg(windows)]
    if perms.is_some() {
        // CopyFileEx preserves named streams. Rewrite only the unnamed stream.
        fs::copy(path, tmp.path())?;
        if let Some(p) = perms.filter(|p| p.readonly()) {
            let mut writable = p.clone();
            #[allow(clippy::permissions_set_readonly_false)]
            writable.set_readonly(false);
            tmp.as_file().set_permissions(writable)?;
        }
        tmp.as_file().set_len(0)?;
    }
    tmp.write_all(bytes)?;
    #[cfg(not(windows))]
    if let Some(p) = perms {
        tmp.as_file().set_permissions(p.clone())?;
    }
    tmp.as_file().sync_all()?;
    #[cfg(windows)]
    return persist_windows(tmp, path, perms, replaced);
    #[cfg(not(windows))]
    {
        tmp.persist(path).map_err(|e| e.error)?;
        Ok(())
    }
}

/// Windows refuses to replace a read-only file, and Defender/indexers briefly lock fresh files
/// (ERROR_ACCESS_DENIED 5 / ERROR_SHARING_VIOLATION 32), so clear read-only and retry.
/// `tempfile`'s Windows `persist` resets attributes to `FILE_ATTRIBUTE_NORMAL`, so re-apply read-only after.
#[cfg(windows)]
fn persist_windows(mut tmp: tempfile::NamedTempFile, path: &Path, perms: Option<&Permissions>, replaced: &mut bool) -> io::Result<()> {
    let is_busy = |e: &io::Error| e.kind() == io::ErrorKind::PermissionDenied || matches!(e.raw_os_error(), Some(5 | 32));
    let was_readonly = fs::metadata(path).ok().filter(|m| m.permissions().readonly());
    if let Some(m) = &was_readonly {
        let mut p = m.permissions();
        #[allow(clippy::permissions_set_readonly_false)] // Windows only: clears the attribute, not a mode bit
        p.set_readonly(false);
        fs::set_permissions(path, p)?;
    }
    for attempt in 1..=8 {
        match tmp.persist(path) {
            Ok(_) => {
                *replaced = true;
                if let Some(p) = perms.filter(|p| p.readonly()) {
                    for perm_attempt in 1..=8 {
                        match fs::set_permissions(path, p.clone()) {
                            Ok(()) => break,
                            Err(e) if is_busy(&e) && perm_attempt < 8 => std::thread::sleep(std::time::Duration::from_millis(20)),
                            Err(e) => return Err(e),
                        }
                    }
                }
                return Ok(());
            }
            Err(e) => {
                if !is_busy(&e.error) || attempt == 8 {
                    if let Some(m) = was_readonly {
                        let _ = fs::set_permissions(path, m.permissions()); // target stays as it was
                    }
                    return Err(e.error);
                }
                tmp = e.file;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }
    unreachable!()
}

fn bullets(header: &str, items: &[String], max: usize) -> String {
    let body = budget::fair(&items.iter().map(|e| format!("- {e}")).collect::<Vec<_>>(), max.saturating_sub(header.len() + items.len() + 1));
    format!("{header}\n{}", body.join("\n"))
}

fn rollback(w: &Plan) -> Result<(), String> {
    let res = match &w.old {
        Some((bytes, perms)) => write_atomic(&w.path, bytes, Some(perms), &mut false),
        None => fs::remove_file(&w.path),
    };
    res.map_err(|e| format!("{}: {e}", w.display))
}

fn commit<'a>(plans: &'a [Plan], best_effort: bool, failed: &mut Vec<String>) -> Result<(Vec<&'a Plan>, Vec<String>), String> {
    let mut written: Vec<&Plan> = Vec::new();
    let mut rb_err: Vec<String> = Vec::new();
    for p in plans {
        let mut replaced = false;
        match write_atomic(&p.path, &p.new, p.old.as_ref().map(|o| &o.1), &mut replaced) {
            Ok(()) => written.push(p),
            Err(e) if best_effort => {
                let mut msg = format!("{}: write failed: {e}", p.display);
                if replaced && let Err(re) = rollback(p) {
                    msg.push_str(&format!("; rollback also failed: {re}"));
                    rb_err.push(re);
                }
                failed.push(msg);
            }
            Err(e) => {
                if replaced {
                    written.push(p);
                }
                for w in &written {
                    if let Err(re) = rollback(w) {
                        rb_err.push(re);
                    }
                }
                if rb_err.is_empty() {
                    return Err(format!("Error: write failed for {}: {e}; rolled back {} file(s) (0 files modified)", p.path.display(), written.len()));
                }
                return Err(format!(
                    "Error: write failed for {}: {e}; rollback failed for {} of {} file(s) (disk may be partially modified): {}",
                    p.path.display(),
                    rb_err.len(),
                    written.len(),
                    rb_err.join("; ")
                ));
            }
        }
    }
    Ok((written, rb_err))
}

pub fn run(args: Value) -> Result<String, String> {
    let a: Args = serde_json::from_value(args).map_err(|e| format!("Error: invalid apply_edits arguments (0 files modified): {e}"))?;
    if !(1..=50).contains(&a.edits.len()) {
        return Err(format!("Error: `edits` must list 1-50 files, got {} (0 files modified)", a.edits.len()));
    }
    let cwd = cwd::resolve(a.cwd.as_deref())?;
    let mut jobs: Vec<Job> = Vec::new();
    for e in a.edits {
        let path = cwd::canonical(&cwd.join(&e.path));
        match jobs.iter_mut().find(|j| cwd::key(&j.path) == cwd::key(&path)) {
            Some(j) => j.entries.push(e),
            None => jobs.push(Job { display: e.path.clone(), path, entries: vec![e] }),
        }
    }
    // Locks are taken in sorted order (no deadlock between batches) and held across read -> write.
    let mut order: Vec<&Path> = jobs.iter().map(|j| j.path.as_path()).collect();
    order.sort_by_key(|p| cwd::key(p)); // same order as the lock keys, or case-differing new files could deadlock
    let mut locks = Vec::new();
    for p in order {
        locks.push(lock::acquire(p).map_err(|e| format!("Error: cannot lock {}: {e} (0 files modified)", p.display()))?);
    }
    let mut plans = Vec::new();
    let mut failed = Vec::new();
    for r in jobs.iter().map(plan) {
        match r {
            Ok(p) => plans.push(p),
            Err(e) => failed.push(e),
        }
    }
    if !failed.is_empty() && (!a.best_effort || plans.is_empty() || a.dry_run) {
        return Err(bullets(&format!("Error: validation failed for {} of {} file(s) (0 files modified):", failed.len(), jobs.len()), &failed, budget::CAP));
    }
    let summaries: Vec<&str> = plans.iter().map(|p| p.summary.as_str()).collect();
    if a.dry_run {
        return Ok(format!("Dry run: {} file(s) valid (0 files modified):\n{}", plans.len(), summaries.join("\n")));
    }
    let (written, rb_err) = commit(&plans, a.best_effort, &mut failed)?;
    drop(locks);
    let lines: Vec<&str> = written.iter().map(|p| p.summary.as_str()).collect();
    if written.is_empty() {
        let hdr = if rb_err.is_empty() {
            "Error: every write failed (0 files modified):"
        } else {
            "Error: every write failed (rollback also failed; disk may be partially modified):"
        };
        return Err(bullets(hdr, &failed, budget::CAP));
    }
    if !failed.is_empty() {
        let head = if rb_err.is_empty() {
            format!(
                "Partial edit applied ({}/{} file(s) written; {} not written, left unchanged).\nWritten:\n{}",
                written.len(),
                jobs.len(),
                failed.len(),
                lines.join("\n")
            )
        } else {
            format!(
                "Partial edit applied ({}/{} file(s) written; {} failed, rollback failed for {} — disk may be partially modified).\nWritten:\n{}",
                written.len(),
                jobs.len(),
                failed.len(),
                rb_err.len(),
                lines.join("\n")
            )
        };
        let tail = if a.verify.is_some() { "\nVerify skipped: not every file was applied." } else { "" };
        let rest = bullets("Not written:", &failed, budget::CAP.saturating_sub(head.len() + tail.len() + 1));
        return Ok(format!("{head}\n{rest}{tail}"));
    }
    let report = format!("Applied edits to {} file(s):\n{}", written.len(), lines.join("\n"));
    let Some(cmd) = a.verify.as_deref().filter(|c| !c.trim().is_empty()) else {
        return Ok(report);
    };
    let max = budget::limit(a.max_verify_chars, 300).min(7500);
    let v = crate::tools::check::verify(cmd, &cwd, a.verify_timeout_sec.unwrap_or(60), max);
    Ok(format!("{}\n{v}", budget::fold(&report, budget::CAP.saturating_sub(v.len() + 1))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            fs::write(dir.path().join(name), text).unwrap();
        }
        dir
    }

    fn edit(dir: &tempfile::TempDir, extra: Value) -> Result<String, String> {
        let mut args = json!({"cwd": dir.path().to_str().unwrap(), "toolSummary": "x"});
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        run(args)
    }

    fn read(dir: &tempfile::TempDir, name: &str) -> String {
        fs::read_to_string(dir.path().join(name)).unwrap()
    }

    const DUP: &str = "fn one() {\n  return value;\n}\nfn two() {\n  return value;\n}\n";

    #[test]
    fn overlapping_matches_and_mixed_line_endings() {
        let d = setup(&[("a", "aaa"), ("mixed", "a\r\nb\r\na\nb\nlast")]);
        let h = |path, target, replacement, all| json!({"edits": [{"path": path, "hunks": [
            {"target": target, "replacement": replacement, "replaceAll": all}
        ]}]});
        assert!(edit(&d, h("a", "aa", "X", false)).unwrap_err().contains("overlapping matches"));
        assert_eq!(read(&d, "a"), "aaa");
        edit(&d, h("a", "aa", "X", true)).unwrap();
        assert_eq!(read(&d, "a"), "Xa");
        assert!(edit(&d, h("mixed", "a\nb", "X\nY", false)).unwrap_err().contains("matched 2 times"));
        edit(&d, h("mixed", "a\nb", "X\nY", true)).unwrap();
        edit(&d, h("mixed", "last", "last\nline", false)).unwrap();
        assert_eq!(read(&d, "mixed"), "X\r\nY\r\nX\nY\nlast\nline");
    }

    #[test]
    fn unique_ambiguous_missing_and_window() {
        let d = setup(&[("a.rs", DUP)]);
        let h = |t: &str, extra: Value| {
            let mut hunk = json!({"target": t, "replacement": "return 2;"});
            hunk.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            json!({"edits": [{"path": "a.rs", "hunks": [hunk]}]})
        };
        let amb = edit(&d, h("return value;", json!({}))).unwrap_err();
        assert!(amb.contains("matched 2 times (lines: 2, 5)"), "{amb}");
        let miss = edit(&d, h("return valeu;", json!({}))).unwrap_err();
        assert!(miss.contains("target not found") && miss.contains("0 files modified"), "{miss}");
        let ok = edit(&d, h("return value;", json!({"startLine": 4, "endLine": 6}))).unwrap();
        assert!(ok.contains("- a.rs: updated (1 hunk), lines +0"), "{ok}");
        assert_eq!(read(&d, "a.rs"), "fn one() {\n  return value;\n}\nfn two() {\n  return 2;\n}\n");
        let all = edit(&d, json!({"edits": [{"path": "a.rs", "hunks": [{"target": "fn", "replacement": "pub fn", "replaceAll": true}]}]})).unwrap();
        assert!(all.contains("1 hunk, 2 replacements"), "{all}");
        let bad = edit(&d, h("return value;", json!({"startLine": 0}))).unwrap_err();
        assert!(bad.contains("1-based"), "{bad}");
        assert!(edit(&d, h("", json!({}))).unwrap_err().contains("empty target"));
        assert!(edit(&d, h("return 2;", json!({}))).unwrap_err().contains("no-op"));
    }

    #[test]
    fn hints_drift_whitespace_and_closest_line() {
        let mut text: String = (1..=85).map(|i| format!("const f{i} = {i};\n")).collect();
        text.push_str("function target() {\n  const answer = 42;\n  return answer;\n}\n");
        let d = setup(&[("d.ts", &text)]);
        let one = |hunk: Value| edit(&d, json!({"edits": [{"path": "d.ts", "hunks": [hunk]}]})).unwrap_err();
        let drift = one(json!({"target": "function target() {\n  const answer = 42;", "replacement": "x", "startLine": 1, "endLine": 40}));
        assert!(drift.contains("within lines 1-40") && drift.contains("outside range at L86-L87"), "{drift}");
        let ws = one(json!({"target": "function target() {\n    const answer = 42;", "replacement": "x"}));
        assert!(ws.contains("Whitespace-insensitive match at line 86") && ws.contains("87:   const answer = 42;"), "{ws}");
        let near = one(json!({"target": "function target() {\n  const answer = 41;\n  return answer;", "replacement": "x"}));
        assert!(near.contains("Closest match near line 86:") && near.contains("86: function target() {"), "{near}");
        assert!(near.len() < 600, "{near}");
    }

    #[test]
    fn crlf_and_multibyte_preserved() {
        let d = setup(&[("w.txt", "line1\r\nline2\r\nline3\r\n"), ("u.txt", "привіт 🌍\ncase \"ERP\" -> \"实付\";\n")]);
        edit(&d, json!({"edits": [
            {"path": "w.txt", "hunks": [{"target": "line1\nline2", "replacement": "L1\nL2\nL2b"}]},
            {"path": "u.txt", "hunks": [{"target": "🌍", "replacement": "світ"}]}
        ]}))
        .unwrap();
        assert_eq!(read(&d, "w.txt"), "L1\r\nL2\r\nL2b\r\nline3\r\n");
        assert_eq!(read(&d, "u.txt"), "привіт світ\ncase \"ERP\" -> \"实付\";\n");
        let e = edit(&d, json!({"edits": [{"path": "u.txt", "hunks": [{"target": "case \"ERP\" -> \"实付\";\nnope", "replacement": "x"}]}]})).unwrap_err();
        assert!(e.contains("Closest match near line 2") && e.contains("实付"), "{e}");
    }

    #[test]
    fn all_or_nothing_vs_best_effort_and_dry_run() {
        let d = setup(&[("good.txt", "good\n"), ("bad.txt", "bad\n")]);
        let edits = json!([
            {"path": "good.txt", "hunks": [{"target": "good", "replacement": "GOOD"}]},
            {"path": "bad.txt", "hunks": [{"target": "missing", "replacement": "x"}]}
        ]);
        assert!(edit(&d, json!({"edits": edits})).unwrap_err().contains("0 files modified"));
        assert_eq!(read(&d, "good.txt"), "good\n");
        assert!(edit(&d, json!({"edits": [edits[0]], "dryRun": true})).unwrap().starts_with("Dry run"));
        assert_eq!(read(&d, "good.txt"), "good\n");
        let partial = edit(&d, json!({"edits": edits, "bestEffort": true, "verify": "true"})).unwrap();
        assert!(partial.contains("1/2 file(s) written") && partial.contains("- good.txt: updated"), "{partial}");
        assert!(partial.contains("Not written:\n- bad.txt hunk #1") && partial.contains("Verify skipped"), "{partial}");
        assert_eq!((read(&d, "good.txt").as_str(), read(&d, "bad.txt").as_str()), ("GOOD\n", "bad\n"));
    }

    #[test]
    fn duplicate_paths_merge_and_overlaps_rejected() {
        let d = setup(&[("m.txt", "alpha beta alpha gamma\n")]);
        let o = edit(&d, json!({"edits": [{"path": "m.txt", "hunks": [
            {"target": "alpha beta", "replacement": "X"}, {"target": "beta alpha", "replacement": "Y"}]}]}))
        .unwrap_err();
        assert!(o.contains("overlap"), "{o}");
        edit(&d, json!({"edits": [
            {"path": "m.txt", "hunks": [{"target": "alpha", "replacement": "ALPHA", "replaceAll": true}]},
            {"path": "./m.txt", "hunks": [{"target": "gamma", "replacement": "GAMMA"}]}
        ]}))
        .unwrap();
        assert_eq!(read(&d, "m.txt"), "ALPHA beta ALPHA GAMMA\n");
    }

    #[test]
    fn create_overwrite_symlink_and_permissions() {
        let d = setup(&[("keep.txt", "keep\n"), ("tool.sh", "echo v1\n")]);
        assert!(edit(&d, json!({"edits": [{"path": "keep.txt", "create": "new\n"}]})).unwrap_err().contains("overwrite: true"));
        let ok = edit(&d, json!({"edits": [
            {"path": "keep.txt", "create": "a\nb\n", "overwrite": true},
            {"path": "nested/deep/new.txt", "create": "only one line\n"}
        ]}))
        .unwrap();
        assert!(ok.contains("keep.txt: overwritten, lines +1") && ok.contains("new.txt: created, lines +1"), "{ok}");
        assert_eq!(read(&d, "nested/deep/new.txt"), "only one line\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            let p = d.path().join("tool.sh");
            fs::set_permissions(&p, Permissions::from_mode(0o751)).unwrap();
            symlink(&p, d.path().join("link.sh")).unwrap();
            edit(&d, json!({"edits": [{"path": "link.sh", "hunks": [{"target": "v1", "replacement": "v2"}]}]})).unwrap();
            assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o751);
            assert!(fs::symlink_metadata(d.path().join("link.sh")).unwrap().file_type().is_symlink());
            assert_eq!(read(&d, "tool.sh"), "echo v2\n");
            symlink(d.path().join("nowhere"), d.path().join("dangling")).unwrap();
            assert!(edit(&d, json!({"edits": [{"path": "dangling", "create": "x"}]})).unwrap_err().contains("dangling"));
        }
        #[cfg(windows)]
        {
            let p = d.path().join("ro.txt");
            fs::write(&p, "old\n").unwrap();
            let mut ro = fs::metadata(&p).unwrap().permissions();
            ro.set_readonly(true);
            fs::set_permissions(&p, ro).unwrap();
            edit(&d, json!({"edits": [{"path": "ro.txt", "hunks": [{"target": "old", "replacement": "new"}]}]})).unwrap();
            assert_eq!(read(&d, "ro.txt"), "new\n");
            let mut pms = fs::metadata(&p).unwrap().permissions();
            assert!(pms.readonly(), "ReadOnly attribute was lost after edit");
            #[allow(clippy::permissions_set_readonly_false)]
            pms.set_readonly(false);
            fs::set_permissions(&p, pms).unwrap();
        }
    }

    #[test]
    fn mixed_crlf_lf_hunk_edits_lf_block() {
        let d = setup(&[("mix.txt", "head\r\nline1\nline2\ntail\r\n")]);
        edit(&d, json!({"edits": [{"path": "mix.txt", "hunks": [{"target": "line1\nline2", "replacement": "L1\nL2\nL3"}]}]})).unwrap();
        assert_eq!(read(&d, "mix.txt"), "head\r\nL1\nL2\nL3\ntail\r\n");
    }

    #[test]
    fn mid_batch_write_failure_rolls_back_and_reports_rollback_errors() {
        let d = setup(&[("a.txt", "old\n")]);
        let a_path = d.path().join("a.txt");
        #[cfg(windows)]
        fs::write(d.path().join("a.txt:rollback"), "kept").unwrap();
        let a_perms = fs::metadata(&a_path).unwrap().permissions();
        let created = d.path().join("new.txt");
        let blocker = d.path().join("blocker");
        fs::create_dir(&blocker).unwrap();
        let plans = vec![
            Plan { display: "a.txt".into(), path: a_path.clone(), new: b"new\n".to_vec(), old: Some((b"old\n".to_vec(), a_perms)), summary: String::new() },
            Plan { display: "new.txt".into(), path: created.clone(), new: b"hi\n".to_vec(), old: None, summary: String::new() },
            Plan { display: "blocker".into(), path: blocker.clone(), new: b"x".to_vec(), old: None, summary: String::new() },
        ];
        let err = commit(&plans, false, &mut Vec::new()).err().unwrap();
        assert!(err.contains("rolled back 2 file(s) (0 files modified)"), "{err}");
        assert_eq!(read(&d, "a.txt"), "old\n");
        assert!(!created.exists());
        #[cfg(windows)]
        assert_eq!(read(&d, "a.txt:rollback"), "kept");

        // Duplicate created path forces second rollback's remove_file to fail.
        let dup = d.path().join("dup.txt");
        let rb_plans = vec![
            Plan { display: "dup.txt".into(), path: dup.clone(), new: b"1".to_vec(), old: None, summary: String::new() },
            Plan { display: "dup.txt".into(), path: dup, new: b"2".to_vec(), old: None, summary: String::new() },
            Plan { display: "blocker".into(), path: blocker, new: b"x".to_vec(), old: None, summary: String::new() },
        ];
        let rb_err = commit(&rb_plans, false, &mut Vec::new()).err().unwrap();
        assert!(rb_err.contains("rollback failed") && rb_err.contains("dup.txt"), "{rb_err}");
        assert!(!rb_err.contains("0 files modified") && !rb_err.contains("left unchanged"), "{rb_err}");
    }

    #[test]
    fn post_edit_verify_reports_pass_and_fail() {
        let d = setup(&[("v.txt", "one\n")]);
        let pass = edit(&d, json!({"edits": [{"path": "v.txt", "hunks": [{"target": "one", "replacement": "two"}]}], "verify": "echo verify-ok"})).unwrap();
        assert!(pass.contains("v.txt: updated") && pass.contains("PASS"), "{pass}");
        let fail = edit(&d, json!({"edits": [{"path": "v.txt", "hunks": [{"target": "two", "replacement": "three"}]}], "verify": "exit 1"})).unwrap();
        assert!(fail.contains("v.txt: updated") && fail.contains("FAIL"), "{fail}");
        assert_eq!(read(&d, "v.txt"), "three\n");
    }
}
