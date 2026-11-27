//! batch_search: 1-20 text/regex/glob/fuzzy/read-only-git queries run
//! concurrently; each result is printed under its own header.

use crate::core::{budget, cwd};
use crate::tools::{outline, snapshot};
use globset::{GlobBuilder, GlobMatcher};
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::WalkBuilder;
use ignore::types::{Types, TypesBuilder};
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::Value;
use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Args {
    cwd: Option<String>,
    queries: Vec<Query>,
    max_chars: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Query {
    #[serde(rename = "type")]
    kind: String,
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    file_type: Option<String>,
    #[serde(default)]
    multiline: bool,
    output_mode: Option<String>,
    sort_by: Option<String>,
    case_sensitive: Option<bool>,
    context_lines: Option<usize>,
    max_results: Option<usize>,
    #[serde(default)]
    definition: bool,
}

/// Build/cache directories skipped even when not gitignored.
const SKIP_DIRS: &[&str] = &[
    ".git", ".hg", ".svn", "node_modules", "target", "dist", "build", "out", ".next", ".nuxt",
    ".svelte-kit", ".cache", ".turbo", "coverage", "__pycache__", ".venv", "venv", ".tox",
    ".pytest_cache", ".mypy_cache", ".ruff_cache", ".gradle", ".idea",
];
const MAX_LINE: usize = 240;
const TIP: &str = "Tip: \"definition\": true on a query returns each hit's whole definition; quick_outline \"file#Name\" reads one by name.";
/// Longest definition shown whole by `definition: true`; longer ones are cut to ±CLIP lines around the hit.
const WHOLE: usize = 60;
const CLIP: usize = 10;

pub fn run(args: Value) -> Result<String, String> {
    let args: Args = serde_json::from_value(args).map_err(|e| format!("Error: invalid batch_search arguments: {e}"))?;
    if !(1..=20).contains(&args.queries.len()) {
        return Err(format!("Error: batch_search takes 1-20 queries, got {}", args.queries.len()));
    }
    let base = cwd::resolve(args.cwd.as_deref())?;
    let parts: Vec<(String, Body)> = args
        .queries
        .par_iter()
        .enumerate()
        .map(|(i, q)| {
            let mut header = format!(
                "## Query {} [{}] {}{}",
                i + 1,
                q.kind,
                q.pattern,
                q.path.as_deref().map(|p| format!(" (in {p})")).unwrap_or_default()
            );
            let mut unreadable = 0;
            let body = run_query(q, &base, &mut unreadable).unwrap_or_else(|e| Body::Text(format!("Error: {e}")));
            if unreadable > 0 {
                header.push_str(&format!("\nIncomplete search: {unreadable} unreadable file(s)."));
            }
            (header, body)
        })
        .collect();
    let lens: Vec<usize> = parts.iter().map(|(h, b)| h.len() + 1 + b.full_len()).collect();
    let hit_defs: Vec<bool> = args.queries.iter().zip(&parts).filter(|(_, (_, b))| matches!(b, Body::Hits(_))).map(|(q, _)| q.definition).collect();
    let limit = budget::limit(args.max_chars, 500);
    let tip = !hit_defs.iter().all(|d| *d) && limit >= TIP.len() + 1 + 300;
    let out = budget::join_fair_by(&lens, if tip { limit - TIP.len() - 1 } else { limit }, |i, share| {
        let (h, b) = &parts[i];
        budget::fold(&format!("{h}\n{}", b.fit(share.saturating_sub(h.len() + 1))), share)
    });
    Ok(if tip { format!("{out}\n{TIP}") } else { out })
}

/// A query result, rendered to fit whatever share of the budget it gets.
enum Body {
    /// Free text (git output, errors): head-tail folded when over budget.
    Text(String),
    /// Lines already capped by count, out of `total` `what`; cut from the end.
    List(Vec<String>, usize, &'static str),
    Hits(Hits),
}

impl Body {
    /// Full size, or an estimate of it (symbol markers are not counted).
    fn full_len(&self) -> usize {
        match self {
            Body::Text(s) => s.len(),
            Body::List(items, ..) => items.iter().map(|s| s.len() + 1).sum::<usize>() + 64,
            Body::Hits(h) => h.files.iter().map(|f| f.path.len() + 1 + f.lines.iter().map(|l| l.text.len() + 9).sum::<usize>()).sum(),
        }
    }

    fn fit(&self, room: usize) -> String {
        match self {
            Body::Text(s) => budget::fold(s, room),
            Body::List(items, total, what) => fit_list(items, *total, what, room),
            Body::Hits(h) => h.fit(room),
        }
    }
}

/// Whole lines from the start that fit `room`, with a notice when any are left out.
fn fit_list(items: &[String], total: usize, what: &str, room: usize) -> String {
    if total == 0 {
        return format!("(no {what} matched)");
    }
    let notice = |k: usize| if k < total { format!("... [truncated: showing {k} of {total} {what}]") } else { String::new() };
    let (mut k, mut len) = (0, 0);
    while k < items.len() && len + items[k].len() + 1 + notice(k + 1).len() <= room {
        len += items[k].len() + 1;
        k += 1;
    }
    let tail = notice(k);
    let mut out: Vec<&str> = items[..k].iter().map(String::as_str).collect();
    if !tail.is_empty() {
        out.push(&tail);
    }
    out.join("\n")
}

fn run_query(q: &Query, base: &Path, unreadable: &mut usize) -> Result<Body, String> {
    // Whitespace is a real text/regex pattern (a tab, two spaces); elsewhere it means nothing.
    if q.pattern.is_empty() || (q.pattern.trim().is_empty() && !matches!(q.kind.as_str(), "text" | "regex")) {
        return Err("pattern must not be empty".into());
    }
    let root = base.join(q.path.as_deref().unwrap_or(""));
    if !root.exists() {
        return Err(format!("path does not exist: {}", root.display()));
    }
    match q.kind.as_str() {
        "git" => run_git(&q.pattern, if root.is_file() { root.parent().unwrap_or(base) } else { &root }).map(Body::Text),
        "glob" => {
            let m = path_glob(&q.pattern)?;
            let hidden = !(q.pattern.starts_with('.') || slashes(&q.pattern).contains("/."));
            let files: Vec<_> = walk(&root, q, hidden, unreadable)?.into_iter().filter(|f| m(&root, f)).collect();
            Ok(list(&files, base, q.max_results.unwrap_or(80)))
        }
        "fuzzy" => {
            let mut scored: Vec<(i64, String)> = walk(&root, q, true, unreadable)?
                .iter()
                .map(|f| rel(base, f))
                .filter_map(|p| Some((fuzzy_score(&q.pattern, &p)?, p)))
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.len().cmp(&b.1.len())).then(a.1.cmp(&b.1)));
            let paths: Vec<_> = scored.into_iter().map(|(_, p)| PathBuf::from(p)).collect();
            Ok(list(&paths, base, q.max_results.unwrap_or(30)))
        }
        "text" | "regex" => search(q, &root, base, unreadable),
        other => Err(format!("unknown query type `{other}` (text, regex, glob, fuzzy, git)")),
    }
}

/// Windows: `\` -> `/`, so paths and globs use one separator (fuzzy_score, basename lookup).
/// Unix keeps `\`: it is a glob escape there.
fn slashes(s: &str) -> String {
    if cfg!(windows) { s.replace('\\', "/") } else { s.to_owned() }
}

/// Display path relative to `base`; callers only print it or score it (file access uses the absolute path).
fn rel(base: &Path, p: &Path) -> String {
    slashes(&p.strip_prefix(base).unwrap_or(p).display().to_string())
}

/// Up to `max` paths, out of all of them.
fn list(paths: &[PathBuf], base: &Path, max: usize) -> Body {
    Body::List(paths.iter().take(max.clamp(1, 200)).map(|p| rel(base, p)).collect(), paths.len(), "files")
}

/// Glob matcher over a path relative to the search root; patterns without `/`
/// match the file name only.
fn path_glob(pattern: &str) -> Result<impl Fn(&Path, &Path) -> bool, String> {
    let pattern = &slashes(pattern);
    let m: GlobMatcher = GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map_err(|e| format!("invalid glob `{pattern}`: {e}"))?
        .compile_matcher();
    let base_only = !pattern.contains('/');
    Ok(move |root: &Path, f: &Path| {
        if base_only {
            f.file_name().is_some_and(|n| m.is_match(n))
        } else {
            m.is_match(f.strip_prefix(root).ok().filter(|r| !r.as_os_str().is_empty()).unwrap_or(f))
        }
    })
}

fn file_types(ft: &str) -> Result<Types, String> {
    let key = ft.trim().trim_start_matches('.').to_lowercase();
    let key = match key.as_str() {
        "rs" => "rust",
        "javascript" => "js",
        k => k,
    };
    let mut b = TypesBuilder::new();
    b.add_defaults();
    if !b.definitions().iter().any(|d| d.name() == key) {
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(format!("unsupported fileType `{ft}`"));
        }
        b.add(key, &format!("*.{key}")).map_err(|e| e.to_string())?;
    }
    b.select(key);
    b.build().map_err(|e| e.to_string())
}

/// Files under `root` honouring .gitignore, the skip list, `glob` and `fileType`,
/// sorted by path (or newest first with `sortBy: "mtime"`).
/// Directories that cannot be read are added to `unreadable`.
fn walk(root: &Path, q: &Query, hidden: bool, unreadable: &mut usize) -> Result<Vec<PathBuf>, String> {
    let glob = q.glob.as_deref().map(path_glob).transpose()?;
    let types = q.file_type.as_deref().map(file_types).transpose()?;
    let mut files: Vec<PathBuf> = WalkBuilder::new(root)
        .hidden(hidden)
        .require_git(false)
        .filter_entry(|e| {
            e.depth() == 0
                || !(e.file_type().is_some_and(|t| t.is_dir())
                    && e.file_name().to_str().is_some_and(|n| SKIP_DIRS.contains(&n)))
        })
        .build()
        .filter_map(|e| e.inspect_err(|_| *unreadable += 1).ok())
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(|e| e.into_path())
        .filter(|f| glob.as_ref().is_none_or(|g| g(root, f)))
        .filter(|f| types.as_ref().is_none_or(|t| t.matched(f, false).is_whitelist()))
        .collect();
    files.sort();
    if q.sort_by.as_deref() == Some("mtime") {
        files.sort_by_cached_key(|f| std::cmp::Reverse(f.metadata().and_then(|m| m.modified()).ok()));
    }
    Ok(files)
}

#[derive(Clone)]
struct Line {
    n: u64,
    is_match: bool,
    text: String,
}

/// Collects up to `cap` matching lines (plus context) and counts all of them.
struct Collect<'a> {
    matcher: &'a RegexMatcher,
    cap: usize,
    lines: Vec<Line>,
    stored: usize,
    count: usize,
    closed: bool,
    binary: bool,
}

impl Collect<'_> {
    fn push(&mut self, n: u64, is_match: bool, text: &str) {
        self.lines.push(Line { n, is_match, text: snippet(self.matcher, text, is_match) });
    }
}

impl Sink for Collect<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _: &Searcher, m: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        let first = m.line_number().unwrap_or(0);
        for (i, line) in String::from_utf8_lossy(m.bytes()).lines().enumerate() {
            self.count += 1;
            if self.stored < self.cap {
                self.stored += 1;
                self.push(first + i as u64, true, line);
            } else {
                self.closed = true;
            }
        }
        Ok(true)
    }

    fn context(&mut self, _: &Searcher, c: &SinkContext<'_>) -> Result<bool, Self::Error> {
        if !self.closed {
            let text = String::from_utf8_lossy(c.bytes());
            self.push(c.line_number().unwrap_or(0), false, text.trim_end_matches(['\n', '\r']));
        }
        Ok(true)
    }

    fn binary_data(&mut self, _: &Searcher, _: u64) -> Result<bool, Self::Error> {
        self.binary = true;
        Ok(false)
    }
}

/// Trim a long line to a window around its first match.
fn snippet(matcher: &RegexMatcher, line: &str, is_match: bool) -> String {
    if line.len() <= MAX_LINE {
        return line.to_string();
    }
    let at = if is_match { matcher.find(line.as_bytes()).ok().flatten().map_or(0, |m| m.start()) } else { 0 };
    let start = if at + 40 < MAX_LINE { 0 } else { line.floor_char_boundary(at.saturating_sub(80)) };
    let end = line.floor_char_boundary(start + MAX_LINE);
    format!("{}{}{}", if start > 0 { "..." } else { "" }, &line[start..end], if end < line.len() { "..." } else { "" })
}

fn search(q: &Query, root: &Path, base: &Path, unreadable: &mut usize) -> Result<Body, String> {
    let mode = q.output_mode.as_deref().unwrap_or("content");
    if !["content", "files_with_matches", "count"].contains(&mode) {
        return Err(format!("invalid outputMode `{mode}`"));
    }
    // Content is cut by the byte budget; maxResults, when given, also caps the count.
    let max = q.max_results.unwrap_or(if mode == "content" { 200 } else { 40 }).clamp(1, 200);
    let def = q.definition && mode == "content";
    let ctx = if mode == "content" && !def { q.context_lines.unwrap_or(0).min(5) } else { 0 };
    let mut mb = RegexMatcherBuilder::new();
    mb.case_insensitive(q.case_sensitive == Some(false))
        .fixed_strings(q.kind == "text")
        .multi_line(true)
        // CRLF mode bans `\n` in the pattern, which multi-line search needs.
        .crlf(!q.multiline)
        .dot_matches_new_line(q.multiline);
    if !q.multiline {
        mb.line_terminator(Some(b'\n'));
    }
    let matcher = mb.build(&q.pattern).map_err(|e| format!("invalid {} pattern: {e}", q.kind))?;
    let files = walk(root, q, true, unreadable)?;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let errors = AtomicUsize::new(0);
    let hits: Vec<(PathBuf, Collect)> = files
        .into_par_iter()
        .filter_map(|f| {
            // List modes print counts only, so they store no lines.
            let mut sink =
                Collect { matcher: &matcher, cap: if mode == "content" { max } else { 0 }, lines: Vec::new(), stored: 0, count: 0, closed: false, binary: false };
            if SearcherBuilder::new()
                .binary_detection(BinaryDetection::quit(0))
                .multi_line(q.multiline)
                .before_context(ctx)
                .after_context(ctx)
                .build()
                .search_path(&matcher, &f, &mut sink)
                .is_err() {
                errors.fetch_add(1, Ordering::Relaxed);
                return None;
            }
            (sink.count > 0 && !sink.binary).then_some((f, sink))
        })
        .collect();
    *unreadable += errors.into_inner();
    if hits.is_empty() {
        return Ok(Body::Text("(no matches)".into()));
    }
    if mode != "content" {
        let items = hits
            .iter()
            .take(max)
            .map(|(f, s)| if mode == "count" { format!("{}: {} matching lines", rel(base, f), s.count) } else { rel(base, f) })
            .collect();
        return Ok(Body::List(items, hits.len(), "files"));
    }
    let mut files: Vec<FileHits> = hits
        .into_iter()
        .map(|(f, s)| FileHits { path: rel(base, &f), lines: s.lines, abs: f, count: s.count })
        .collect();
    // Source before documentation, so a capped result is not spent on Markdown; the sort is stable.
    files.sort_by_key(|f| is_doc(&f.path));
    let split = files.partition_point(|f| !is_doc(&f.path));
    Ok(Body::Hits(Hits { files, split, ctx, cap: max, matcher: def.then_some(matcher) }))
}

/// Lines `(first, last)` shown for a hit at line `n`: its innermost enclosing definition, clipped
/// around the hit when long, else a few lines either side.
fn window(syms: &[(usize, usize, String)], n: usize, last: usize) -> (usize, usize) {
    // Sorted by start, so the last definition containing the line is the innermost.
    match syms.iter().rposition(|s| s.0 <= n && n <= s.1) {
        Some(k) if syms[k].1 - syms[k].0 < WHOLE => (syms[k].0, syms[k].1),
        Some(k) => (syms[k].0.max(n.saturating_sub(CLIP)), syms[k].1.min(n + CLIP)),
        None => (n.saturating_sub(3).max(1), (n + 3).min(last)),
    }
}

/// Replaces matching lines (searched without context) by the union of their windows.
fn widen(lines: &[Line], path: &Path, syms: &OnceCell<Vec<(usize, usize, String)>>, matcher: &RegexMatcher) -> Vec<Line> {
    let (text, syms) = match outline::read_text(path) {
        Ok(t) => {
            let s = syms.get_or_init(|| outline::symbols_in(path, &t));
            (t, s)
        }
        // Over 2 MB or binary-looking: no symbols, but still widen to a line window as before the cache.
        Err(_) => {
            let Ok(bytes) = fs::read(path) else {
                syms.get_or_init(Vec::new);
                return lines.to_vec();
            };
            (String::from_utf8_lossy(&bytes).into_owned(), syms.get_or_init(Vec::new))
        }
    };
    let src: Vec<&str> = text.lines().collect();
    let mut keep: BTreeMap<usize, Line> = BTreeMap::new();
    for l in lines {
        let (a, b) = window(syms, l.n as usize, src.len());
        for n in a..=b {
            keep.entry(n).or_insert_with(|| Line { n: n as u64, is_match: false, text: snippet(matcher, src.get(n - 1).unwrap_or(&""), false) });
        }
    }
    for l in lines {
        keep.insert(l.n as usize, l.clone());
    }
    keep.into_values().collect()
}

struct FileHits {
    path: String,
    abs: PathBuf,
    /// Up to `cap` matching lines with their context.
    lines: Vec<Line>,
    /// All matching lines in the file.
    count: usize,
}

/// Content-mode hits, grouped per file:
/// ```text
/// src/a.rs
///  L10-24 fn enclosing(x: u8)
///  12: matching line
///  13- context line
/// ```
struct Hits {
    /// Source files first, then documentation from index `split`.
    files: Vec<FileHits>,
    split: usize,
    ctx: usize,
    cap: usize,
    /// `Some` when `definition: true`: widens visited files lazily.
    matcher: Option<RegexMatcher>,
}

const MAX_SIG: usize = 80;
/// Consecutive hits that do not fit after which a fill round stops.
const MAX_MISSES: usize = 16;

/// Notice inputs kept up to date while hits are added: all hits, shown hits, files with
/// hits not shown and the summed length of their notice items.
#[derive(Clone, Copy)]
struct Omitted {
    total: usize,
    done: usize,
    files: usize,
    items_len: usize,
}

fn is_doc(path: &str) -> bool {
    let ext = path.rsplit_once('.').map_or("", |(_, e)| e);
    ["md", "mdx", "markdown", "txt", "rst", "adoc"].iter().any(|d| ext.eq_ignore_ascii_case(d))
}

impl Hits {
    /// Round-robin over the source files, then over the documentation files (every
    /// file's first hit before any file's second) until the count cap or `room`
    /// bytes are reached, then one notice.
    fn fit(&self, room: usize) -> String {
        let n = self.files.len();
        let syms: Vec<OnceCell<Vec<(usize, usize, String)>>> = (0..n).map(|_| OnceCell::new()).collect();
        let widened: Vec<OnceCell<Vec<Line>>> = (0..n).map(|_| OnceCell::new()).collect();
        let stored: Vec<usize> = self.files.iter().map(|f| f.lines.iter().filter(|l| l.is_match).count()).collect();
        let (mut shown, mut blocks) = (vec![0; n], vec![String::new(); n]);
        let (mut used, mut total) = (0, 0); // `used`: block bytes, one newline each
        let mut om = self.omitted(&shown);
        let mut tier = 0..self.split;
        let render = |i: usize, k: usize| {
            let f = &self.files[i];
            let lines = match &self.matcher {
                Some(m) => widened[i].get_or_init(|| widen(&f.lines, &f.abs, &syms[i], m)).as_slice(),
                None => f.lines.as_slice(),
            };
            self.block(i, k, lines, syms[i].get_or_init(|| outline::symbols(&f.abs)))
        };
        'fill: loop {
            let (before, mut misses) = (total, 0);
            for i in tier.clone() {
                if shown[i] == stored[i] {
                    continue;
                }
                if total == self.cap {
                    break 'fill;
                }
                let block = render(i, shown[i] + 1);
                let next = used + block.len() + 1 - if blocks[i].is_empty() { 0 } else { blocks[i].len() + 1 };
                let om_next = self.step(om, i, shown[i]);
                shown[i] += 1;
                if next + self.notice(&shown, om_next, room / 4).len() > room {
                    shown[i] -= 1;
                    // A smaller later file may still fit, but once the budget is full, stop looking.
                    misses += 1;
                    if misses == MAX_MISSES {
                        break;
                    }
                    continue;
                }
                (used, blocks[i], total, om, misses) = (next, block, total + 1, om_next, 0);
            }
            if total == before {
                if tier.end == n {
                    break;
                }
                tier = self.split..n; // sources exhausted: now the documentation files
            }
        }
        let notice = if total == 0
            && let Some(i) = (0..n).find(|&i| stored[i] > 0)
        {
            shown[i] = 1;
            let notice = self.notice(&shown, self.omitted(&shown), room / 4);
            let avail = room.saturating_sub(if notice.is_empty() { 0 } else { notice.len() + 1 });
            if avail > 0 {
                blocks[i] = budget::fold(&render(i, 1), avail);
                notice
            } else {
                shown[i] = 0;
                self.notice(&shown, self.omitted(&shown), room)
            }
        } else {
            self.notice(&shown, om, room.saturating_sub(used))
        };
        let mut out: Vec<String> = blocks.into_iter().filter(|b| !b.is_empty()).collect();
        if !notice.is_empty() {
            out.push(notice);
        }
        out.join("\n")
    }

    /// File `i` with its first `n` hits, their context and enclosing-definition markers.
    fn block(&self, i: usize, n: usize, lines: &[Line], syms: &[(usize, usize, String)]) -> String {
        let f = &self.files[i];
        let def = self.matcher.is_some();
        let mut out: Vec<(u64, String)> = vec![(0, f.path.clone())];
        let (mut seen, mut after, mut last_hit, mut cur, mut hi) = (0, 0, 1, None, 0);
        for l in lines {
            if l.is_match {
                if seen == n {
                    // Later hits inside the last shown window are part of the definition.
                    if def && l.n as usize <= hi {
                        out.push((l.n, format!(" {}- {}", l.n, l.text)));
                        continue;
                    }
                    break;
                }
                hi = window(syms, l.n as usize, usize::MAX).1;
                (seen, after) = (seen + 1, 0);
                // Sorted by start, so the last definition containing the line is the innermost.
                let sym = syms.iter().rposition(|s| s.0 as u64 <= l.n && l.n <= s.1 as u64);
                if sym != cur {
                    // A hit on the definition's first line already shows its signature.
                    if let Some((a, b, sig)) = sym.map(|k| &syms[k]).filter(|s| (s.0, s.1) != (l.n as usize, l.n as usize)) {
                        let sig = match sig.char_indices().nth(MAX_SIG) {
                            _ if *a == l.n as usize => String::new(),
                            Some((cut, _)) => format!(" {}…", &sig[..cut]),
                            None => format!(" {sig}"),
                        };
                        // Before the context lines that are already inside the definition.
                        let at = (last_hit..out.len()).find(|&j| out[j].0 >= *a as u64).unwrap_or(out.len());
                        out.insert(at, (0, format!(" {}{sig}", outline::span(*a, *b))));
                    }
                    cur = sym;
                }
                out.push((l.n, format!(" {}: {}", l.n, l.text)));
                last_hit = out.len();
            } else {
                if seen == n {
                    if def {
                        if l.n as usize > hi {
                            break;
                        }
                    } else if after == self.ctx {
                        break;
                    }
                    after += 1;
                }
                out.push((l.n, format!(" {}- {}", l.n, l.text)));
            }
        }
        out.into_iter().map(|(_, s)| s.trim_end().to_string()).collect::<Vec<_>>().join("\n")
    }

    /// Length of file `i`'s " path count" item in the notice when `s` of its hits are shown.
    fn item_len(&self, i: usize, s: usize) -> usize {
        let f = &self.files[i];
        if f.count > s { 2 + f.path.len() + (f.count - s).ilog10() as usize + 1 } else { 0 }
    }

    /// What `notice` needs for `shown`, from scratch.
    fn omitted(&self, shown: &[usize]) -> Omitted {
        let mut om = Omitted { total: self.files.iter().map(|f| f.count).sum(), done: shown.iter().sum(), files: 0, items_len: 0 };
        for (i, &s) in shown.iter().enumerate() {
            om.files += usize::from(self.files[i].count > s);
            om.items_len += self.item_len(i, s);
        }
        om
    }

    /// `om` after file `i` goes from `s` to `s + 1` shown hits.
    fn step(&self, mut om: Omitted, i: usize, s: usize) -> Omitted {
        om.done += 1;
        om.items_len = om.items_len - self.item_len(i, s) + self.item_len(i, s + 1);
        om.files -= usize::from(self.files[i].count == s + 1);
        om
    }

    /// Totals plus every file with hits not shown and how many, within `room`
    /// where possible; files that do not fit are summed up as one count.
    fn notice(&self, shown: &[usize], om: Omitted, room: usize) -> String {
        let (total, done, omitted_count) = (om.total, om.done, om.files);
        if done == total {
            return String::new();
        }
        let items_len = om.items_len + omitted_count.saturating_sub(1); // plus the separating commas
        let mut out = format!("... [truncated: showing {done} of {total} matching lines in {} files; not shown:", self.files.len());
        let all_fit = out.len() + items_len < room;
        let (mut listed, mut rest) = (0, total - done);
        for (f, &s) in self.files.iter().zip(shown) {
            if f.count <= s {
                continue;
            }
            let c = f.count - s;
            let item_len = usize::from(listed > 0) + 2 + f.path.len() + c.ilog10() as usize + 1;
            if !all_fit && out.len() + item_len + 32 > room {
                break; // keep room for the summary
            }
            let _ = write!(out, "{} {} {c}", if listed > 0 { "," } else { "" }, f.path);
            (listed, rest) = (listed + 1, rest - c);
        }
        if listed < omitted_count {
            let _ = write!(out, "{} +{} files ({rest} lines)", if listed > 0 { "," } else { "" }, omitted_count - listed);
        }
        out.push(']');
        out
    }
}

/// Ranks paths by a case-insensitive subsequence match; higher is better.
fn fuzzy_score(pattern: &str, path: &str) -> Option<i64> {
    fn cost(q: &str, h: &str) -> Option<i64> {
        let (q, h) = (q.to_lowercase(), h.to_lowercase());
        if let Some(at) = h.find(&q) {
            return Some(if at == 0 { -100 } else { -60 + at.min(25) as i64 });
        }
        let h: Vec<char> = h.chars().collect();
        let (mut cur, mut first, mut last) = (0, None, 0);
        for c in q.chars() {
            while cur < h.len() && h[cur] != c {
                cur += 1;
            }
            if cur == h.len() {
                return None;
            }
            first.get_or_insert(cur);
            last = cur;
            cur += 1;
        }
        let first = first?;
        Some((last + 1 - first - q.chars().count()) as i64 - if first == 0 { 20 } else { 0 })
    }
    let pattern = pattern.replace('\\', "/");
    let name = path.rsplit('/').next().unwrap_or(path);
    let mut total = 0;
    for tok in pattern.split_whitespace() {
        let full = 1000 - cost(tok, path)?;
        let base = if tok.contains('/') { None } else { cost(tok, name).map(|c| 1000 - c + 45) };
        total += base.map_or(full, |b| b.max(full)) + if name.eq_ignore_ascii_case(tok) { 60 } else { 0 };
    }
    Some(total)
}

/// Shell-like split: whitespace separates, '…' and "…" group, `\` escapes
/// (except inside single quotes).
fn split_args(s: &str) -> Result<Vec<String>, String> {
    let (mut out, mut cur, mut quote, mut esc, mut started) = (Vec::new(), String::new(), None, false, false);
    for ch in s.chars() {
        if esc {
            cur.push(ch);
            esc = false;
        } else if ch == '\\' && quote != Some('\'') {
            esc = true;
            started = true;
        } else if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                cur.push(ch);
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
            started = true;
        } else if ch.is_whitespace() {
            if started {
                out.push(std::mem::take(&mut cur));
                started = false;
            }
        } else {
            cur.push(ch);
            started = true;
        }
    }
    if quote.is_some() {
        return Err("unterminated quote in git query".into());
    }
    if started {
        out.push(cur);
    }
    Ok(out)
}

const GIT_SUBCMDS: &[&str] = &[
    "status", "log", "diff", "show", "blame", "ls-files", "rev-parse", "grep", "describe", "shortlog", "branch",
    "tag", "remote", "stash",
];
/// Long options that write files, run programs or redirect the repository.
/// Abbreviations are rejected too because git accepts unique prefixes.
const GIT_FORBIDDEN: &[&str] = &[
    "--output", "--ext-diff", "--textconv", "--exec-path", "--exec", "--config", "--config-env", "--git-dir",
    "--work-tree", "--upload-pack", "--open-files-in-pager", "--namespace", "--super-prefix",
];

/// `branch`/`tag` are allowed only in their listing forms.
struct ListRules {
    bools: &'static [&'static str],
    eq_opt: &'static [&'static str],
    commit: &'static [&'static str],
    value: &'static [&'static str],
    shorts: &'static str,
}

const BRANCH: ListRules = ListRules {
    bools: &[
        "--all", "--remotes", "--verbose", "--list", "--show-current", "--no-abbrev", "--no-color", "--ignore-case",
        "--omit-empty", "--no-column", "--quiet",
    ],
    eq_opt: &["--abbrev", "--color", "--column"],
    commit: &["--contains", "--no-contains", "--points-at", "--merged", "--no-merged"],
    value: &["--sort", "--format"],
    shorts: "arvlqi",
};
const TAG: ListRules = ListRules {
    bools: &["--list", "--no-color", "--ignore-case", "--omit-empty", "--no-column"],
    eq_opt: &["--color", "--column"],
    commit: &["--contains", "--no-contains", "--points-at", "--merged", "--no-merged"],
    value: &["--sort", "--format"],
    shorts: "li",
};

fn check_list_only(sub: &str, rest: &[String], r: &ListRules) -> Result<(), String> {
    let bad = |a: &str| Err(format!("`{a}` is not allowed in read-only git {sub} (listing forms only)"));
    let (mut listing, mut it) = (false, rest.iter());
    while let Some(a) = it.next() {
        if a == "--" {
            return if listing { Ok(()) } else { bad(a) };
        }
        if let Some((name, _)) = a.split_once('=').filter(|_| a.starts_with("--")) {
            if !(r.value.contains(&name) || r.eq_opt.contains(&name) || r.commit.contains(&name)) {
                return bad(a);
            }
            listing |= r.commit.contains(&name);
        } else if a.starts_with("--") {
            if r.commit.contains(&a.as_str()) || r.value.contains(&a.as_str()) {
                listing |= r.commit.contains(&a.as_str());
                if it.next().is_none_or(|v| v.starts_with('-')) {
                    return bad(a);
                }
            } else if r.bools.contains(&a.as_str()) || r.eq_opt.contains(&a.as_str()) {
                listing |= a == "--list";
            } else {
                return bad(a);
            }
        } else if a.starts_with('-') && a.len() > 1 {
            if sub == "tag" && a.strip_prefix("-n").is_some_and(|d| d.chars().all(|c| c.is_ascii_digit())) {
                listing = true;
            } else if a[1..].chars().all(|c| r.shorts.contains(c)) {
                listing |= a.contains('l');
            } else {
                return bad(a);
            }
        } else if !listing {
            return bad(a);
        }
    }
    Ok(())
}

/// Validate a git query and return the subcommand and its arguments (with
/// hardening flags injected). Everything not known to be read-only is rejected.
fn git_args(pattern: &str) -> Result<Vec<String>, String> {
    let mut tokens = split_args(pattern)?;
    if tokens.first().is_some_and(|t| t == "git") {
        tokens.remove(0);
    }
    let Some(sub) = tokens.first().cloned() else {
        return Err("empty git query; e.g. `status --short` or `log -n 5 --oneline`".into());
    };
    if !GIT_SUBCMDS.contains(&sub.as_str()) {
        return Err(format!("git `{sub}` is not allowed; read-only subcommands only: {}", GIT_SUBCMDS.join(", ")));
    }
    let rest = &tokens[1..];
    for a in rest {
        if a.chars().any(char::is_control)
            || ["&&", "||", ";", "|", "&"].contains(&a.as_str())
            || a.starts_with(['>', '<'])
            || a.starts_with("2>")
            || a.contains('`')
            || a.contains("$(")
        {
            return Err(format!("`{a}`: shell syntax is not supported (git runs without a shell); use separate queries"));
        }
        let name = a.split_once('=').map_or(a.as_str(), |(n, _)| n);
        let forbidden = a.starts_with("--")
            && name != "--text"
            && (name == "--ext"
                || name.starts_with("--ext-")
                || GIT_FORBIDDEN.iter().any(|f| name.starts_with(f) || (name.len() >= 3 && f.starts_with(name))));
        let short_c = a.starts_with("-c");
        let grep_pager = sub == "grep" && a.starts_with('-') && !a.starts_with("--") && a.contains('O');
        if forbidden || short_c || grep_pager {
            return Err(format!("git option `{a}` is not allowed (it can write files, run programs or change config)"));
        }
    }
    let flags = |a: &&String| a.starts_with('-') && !a.starts_with("--") && a.len() > 1;
    let mut args = vec![sub.clone()];
    match sub.as_str() {
        "status" => {
            let verbose = rest.iter().take_while(|a| *a != "--").any(|a| {
                a.starts_with("--v") || (flags(&a) && a[1..].chars().take_while(|c| !"uM".contains(*c)).any(|c| c == 'v'))
            });
            if verbose {
                return Err("verbose status runs textconv; use a `diff` query instead".into());
            }
        }
        "branch" => check_list_only("branch", rest, &BRANCH)?,
        "tag" => check_list_only("tag", rest, &TAG)?,
        "remote" => match rest.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
            [] | ["-v" | "--verbose"] => {}
            ["get-url", opts @ .., name] if opts.iter().all(|o| ["--push", "--all"].contains(o)) => remote_name(name)?,
            ["show", name] => {
                remote_name(name)?;
                args.push("show".into());
                args.push("-n".into()); // -n: no network
                args.push(name.to_string());
                return Ok(args);
            }
            _ => return Err("only `remote`, `remote -v`, `remote get-url NAME` and `remote show NAME` are allowed".into()),
        },
        "stash" => {
            if !rest.first().is_some_and(|a| a == "list" || a == "show") {
                return Err("only `stash list` and `stash show` are allowed".into());
            }
            args.extend([rest[0].as_str(), "--no-ext-diff", "--no-textconv"].map(String::from));
            args.extend(rest[1..].iter().cloned());
            return Ok(args);
        }
        "diff" | "log" | "show" => args.extend(["--no-ext-diff", "--no-textconv"].map(String::from)),
        "blame" => args.push("--no-textconv".into()),
        _ => {}
    }
    args.extend(rest.iter().cloned());
    // shortlog without a revision reads stdin; default to HEAD.
    if sub == "shortlog" && rest.iter().all(|a| a.starts_with('-')) {
        args.push("HEAD".into());
    }
    Ok(args)
}

fn remote_name(name: &str) -> Result<(), String> {
    if name.starts_with('-') || name.contains([':', '/', '\\']) {
        return Err(format!("`{name}` must be a configured remote name, not a URL, path or option"));
    }
    Ok(())
}

const GIT_OUT_CAP: u64 = 512 * 1024;

fn run_git(pattern: &str, dir: &Path) -> Result<String, String> {
    let args = git_args(pattern)?;
    let filters = snapshot::git_filters(dir, Instant::now() + Duration::from_secs(10)).ok_or("cannot read git filter config")?;
    let mut child = snapshot::git_command(dir, &filters)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run git: {e}"))?;
    let reader = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(r) = r {
                let _ = r.take(GIT_OUT_CAP).read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = reader(child.stdout.take().map(|r| Box::new(r) as _));
    let err = reader(child.stderr.take().map(|r| Box::new(r) as _));
    let Some(status) = child.wait_timeout(Duration::from_secs(10)).map_err(|e| e.to_string())? else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("git timed out after 10s".into());
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    // A full buffer means we stopped reading and git died of SIGPIPE: still a success.
    if !status.success() && (out.len() as u64) < GIT_OUT_CAP {
        let msg = String::from_utf8_lossy(if err.is_empty() { &out } else { &err }).trim().to_string();
        return Err(if msg.is_empty() { format!("git exited with {status}") } else { msg });
    }
    let text = String::from_utf8_lossy(&out).trim_end().to_string();
    Ok(if text.is_empty() { "(empty output)".into() } else { text })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    #[test]
    fn splits_shell_like_args() {
        assert_eq!(split_args(r#"log -n 5 --format="%h %s""#).unwrap(), ["log", "-n", "5", "--format=%h %s"]);
        assert_eq!(split_args(r#"grep 'a b' "" x\ y"#).unwrap(), ["grep", "a b", "", "x y"]);
        assert!(split_args(r#"log "unterminated"#).is_err());
    }

    #[test]
    fn git_allowlist_accepts_read_only() {
        for ok in [
            "status --short", "git log -n 5 --oneline", "diff --stat HEAD~1", "show HEAD:Cargo.toml", "blame -L 1,5 x.rs",
            "ls-files", "rev-parse HEAD", "grep -n foo", "grep -C 2 foo", "describe --tags", "shortlog -sn",
            "branch --list", "branch -av", "branch --show-current", "branch --color --list main*",
            "branch --contains HEAD", "tag -l v*", "tag -n", "tag -n5", "tag --column=never", "remote -v",
            "remote get-url origin", "remote show origin", "stash list", "stash list -p", "stash show -p", "diff --text",
        ] {
            assert!(git_args(ok).is_ok(), "should accept `{ok}`: {:?}", git_args(ok));
        }
        assert_eq!(git_args("log -1").unwrap(), ["log", "--no-ext-diff", "--no-textconv", "-1"]);
        assert_eq!(git_args("stash list -p").unwrap(), ["stash", "list", "--no-ext-diff", "--no-textconv", "-p"]);
        assert_eq!(git_args("stash show -p").unwrap(), ["stash", "show", "--no-ext-diff", "--no-textconv", "-p"]);
        assert_eq!(git_args("remote show origin").unwrap(), ["remote", "show", "-n", "origin"]);
        assert_eq!(git_args("shortlog -sn").unwrap(), ["shortlog", "-sn", "HEAD"]);
    }

    #[test]
    fn git_allowlist_rejects_writes_exec_and_redirects() {
        for bad in [
            "commit -m evil", "push origin main", "reset --hard", "checkout main", "clean -fd", "config core.pager sh",
            "-c core.pager=sh log", "--git-dir=/x log", "-C /tmp status", "branch -D feature", "branch -Dmain",
            "branch new-branch", "branch --color new-branch", "branch -l --no-list evil", "branch --color -- x",
            "branch --sort -D", "tag v1.0.0", "tag -d v1", "tag -l -am msg v1", "tag -я", "tag --color=always evil",
            "remote add origin https://x", "remote show https://x/y", "remote set-url origin x", "stash",
            "stash pop", "stash drop", "diff --output=/tmp/leak", "diff --out=/tmp/leak", "diff -c core.pager=sh",
            "diff -ccore.pager=sh", "log --ext-diff", "log --ext", "show --textconv", "show --textc",
            "grep -O foo", "grep --open-files-in-pager=vi foo", "grep --open foo", "status -v", "status --verbose",
            "log --exec-path=/x", "log --work-tree=/x", "log ; rm -rf /", "log | sh", "log > out", "log && ls",
            "log $(id)", "log `id`", "log \"a\nb\"", "log \"unterminated", "", "git",
        ] {
            assert!(git_args(bad).is_err(), "should reject `{bad}`: {:?}", git_args(bad));
        }
    }

    #[test]
    fn slashes_only_on_windows() {
        assert_eq!(slashes(r"src\**\*.rs"), if cfg!(windows) { "src/**/*.rs" } else { r"src\**\*.rs" });
        assert_eq!(rel(Path::new("base"), &Path::new("base").join("src").join("a.rs")), "src/a.rs");
    }

    #[test]
    fn fuzzy_prefers_contiguous_basename() {
        let mut v = ["mcp-server.ts", "mcpserv.ts", "src/x.rs"].map(|p| (fuzzy_score("MCPSERV", p), p));
        v.sort_by_key(|x| std::cmp::Reverse(x.0));
        assert_eq!(v[0].1, "mcpserv.ts");
        assert_eq!(v[1].1, "mcp-server.ts");
        assert_eq!(v[2].0, None);
    }

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        fs::create_dir_all(p.join("src")).unwrap();
        fs::create_dir_all(p.join("node_modules/pkg")).unwrap();
        fs::write(p.join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(p.join("src/a.rs"), "fn main() {\n    let привіт = \"світ\";\n    needle();\n}\n").unwrap();
        fs::write(p.join("src/b.py"), "def needle():\n    pass\n").unwrap();
        fs::write(p.join("ignored.txt"), "needle\n").unwrap();
        fs::write(p.join("node_modules/pkg/i.js"), "needle\n").unwrap();
        fs::write(p.join("blob.bin"), b"needle\x00\xff").unwrap();
        dir
    }

    fn untip(out: String) -> String {
        out.strip_suffix(&format!("\n{TIP}")).map_or(out.clone(), String::from)
    }

    fn call(dir: &Path, queries: Value) -> String {
        untip(run(json!({"cwd": dir.to_str().unwrap(), "queries": queries, "toolSummary": "x"})).unwrap())
    }

    #[test]
    fn text_and_regex_search_respect_ignores_and_multibyte() {
        let dir = fixture();
        let out = call(
            dir.path(),
            json!([
                {"type": "text", "pattern": "needle"},
                {"type": "regex", "pattern": "привіт\\s*=", "contextLines": 1},
                {"type": "text", "pattern": "NEEDLE", "caseSensitive": false, "fileType": "py"},
                {"type": "regex", "pattern": "([", "path": "src"},
                {"type": "text", "pattern": "x", "path": "missing"},
                {"type": "glob", "pattern": "src/*.{rs,py}"},
                {"type": "text", "pattern": "needle", "outputMode": "count"},
            ]),
        );
        let s: Vec<&str> = out.split("\n\n").collect();
        let (rs, py) = (if cfg!(feature = "lang-rust") { " L1-4 fn main()\n" } else { "" }, if cfg!(feature = "lang-python") { " L1-2\n" } else { "" });
        assert_eq!(s[0], format!("## Query 1 [text] needle\nsrc/a.rs\n{rs} 3:     needle();\nsrc/b.py\n{py} 1: def needle():"));
        assert!(s[1].ends_with(&format!("src/a.rs\n{rs} 1- fn main() {{\n 2:     let привіт = \"світ\";\n 3-     needle();")), "{}", s[1]);
        assert!(s[2].ends_with(" 1: def needle():"));
        assert!(s[3].contains("Error: invalid regex pattern"));
        assert!(s[4].contains("Error: path does not exist"));
        assert!(s[5].ends_with("src/a.rs\nsrc/b.py"));
        assert!(s[6].ends_with("src/a.rs: 1 matching lines\nsrc/b.py: 1 matching lines"));
        assert!(!out.contains("ignored.txt") && !out.contains("node_modules") && !out.contains("blob.bin"));
    }

    #[test]
    fn caps_results_and_reports_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..3 {
            fs::write(dir.path().join(format!("f{i}.txt")), "pre\nhit one\nmid\nhit two\npost\n").unwrap();
        }
        // An explicit maxResults caps the count, spread one hit per file.
        let out = call(dir.path(), json!([{"type": "text", "pattern": "hit", "maxResults": 3, "contextLines": 1}]));
        assert!(out.ends_with(
            "f2.txt\n 1- pre\n 2: hit one\n 3- mid\n... [truncated: showing 3 of 6 matching lines in 3 files; not shown: f0.txt 1, f1.txt 1, f2.txt 1]"
        ), "{out}");
        assert_eq!(out.matches(": hit").count(), 3);
        let long = "x".repeat(1000) + "hit";
        fs::write(dir.path().join("f0.txt"), &long).unwrap();
        let out = call(dir.path(), json!([{"type": "text", "pattern": "hit", "path": "f0.txt"}]));
        assert!(out.contains("...") && out.ends_with("xhit") && out.len() < 400, "{out}");
    }

    #[test]
    fn budget_cut_spreads_across_files_and_names_every_omitted_one() {
        let dir = tempfile::tempdir().unwrap();
        let body: String = (1..=20).map(|i| format!("привіт hit 🌍 {i}\n")).collect();
        for i in 0..12 {
            fs::write(dir.path().join(format!("f{i:02}.txt")), &body).unwrap();
        }
        // Without maxResults the budget, not a line count, decides: all 60 short hits fit.
        let out = call(dir.path(), json!([{"type": "text", "pattern": "hit", "glob": "f0[0-2].txt"}]));
        assert_eq!(out.matches(": привіт").count(), 60, "{out}");
        assert!(!out.contains("truncated"));

        let out = untip(run(json!({"cwd": dir.path(), "queries": [{"type": "text", "pattern": "hit"}], "maxChars": 1500})).unwrap());
        assert!(out.len() <= 1500, "{}", out.len());
        let notice = out.lines().last().unwrap();
        assert!(notice.starts_with("... [truncated: showing ") && notice.contains(" of 240 matching lines in 12 files"), "{notice}");
        let mut shown = std::collections::HashMap::new();
        let mut file = "";
        for l in out.lines().skip(1) {
            if l.starts_with('f') {
                file = l;
            } else if l.starts_with(' ') {
                *shown.entry(file).or_insert(0) += 1;
            }
        }
        let counts: Vec<usize> = shown.values().copied().collect();
        assert_eq!(shown.len(), 12, "every file shows its first hits\n{out}");
        assert!(counts.iter().max().unwrap() - counts.iter().min().unwrap() <= 1, "{counts:?}");
        for (f, n) in &shown {
            assert!(notice.contains(&format!(" {f} {}", 20 - n)), "{f} missing from {notice}");
        }
        // A list too long for the notice is summed up as a count.
        let out = untip(run(json!({"cwd": dir.path(), "queries": [{"type": "text", "pattern": "hit"}], "maxChars": 500})).unwrap());
        assert!(out.len() <= 500 && out.contains(" files (") && out.ends_with(" lines)]"), "{out}");
    }

    #[test]
    fn source_hits_come_before_documentation() {
        let dir = tempfile::tempdir().unwrap();
        let md: String = (1..=200).map(|i| format!("hit in docs {i}\n")).collect();
        fs::write(dir.path().join("a.md"), &md).unwrap();
        fs::write(dir.path().join("b.rs"), "// hit one\n// hit two\n").unwrap();
        fs::write(dir.path().join("c.rs"), "// hit three\n").unwrap();
        let out = untip(run(json!({"cwd": dir.path(), "queries": [{"type": "text", "pattern": "hit"}], "maxChars": 500})).unwrap());
        assert!(out.len() <= 500, "{out}");
        assert_eq!(out.matches("// hit").count(), 3, "{out}");
        let notice = out.lines().last().unwrap();
        assert!(notice.contains("truncated") && notice.contains(" a.md ") && !notice.contains(".rs "), "{out}");
        let out = call(dir.path(), json!([{"type": "text", "pattern": "hit"}]));
        let at = |s: &str| out.find(s).unwrap();
        assert!(at("b.rs") < at("c.rs") && at("c.rs") < at("a.md"), "{out}");
    }

    #[test]
    fn marks_the_enclosing_definition_once() {
        let dir = tempfile::tempdir().unwrap();
        let src = "// hit at top\nstruct S;\nimpl S {\n    fn go(&self) {\n        hit();\n        hit();\n    }\n}\n";
        fs::write(dir.path().join("a.rs"), src).unwrap();
        fs::write(dir.path().join("a.txt"), src).unwrap();
        let out = call(dir.path(), json!([{"type": "text", "pattern": "hit"}]));
        let rs = if cfg!(feature = "lang-rust") { " L4-7 fn go(&self)\n" } else { "" };
        assert!(out.contains(&format!("a.rs\n 1: // hit at top\n{rs} 5:         hit();\n 6:         hit();\na.txt\n 1:")), "{out}");
        assert!(!out.split("a.txt").nth(1).unwrap().contains(" L"), "{out}");
    }

    #[test]
    fn definition_returns_the_whole_enclosing_definition() {
        let dir = tempfile::tempdir().unwrap();
        let src = "fn first() {\n    let a = 1;\n    hit(a);\n    hit(a);\n}\n\nfn second() {\n    other();\n}\n";
        fs::write(dir.path().join("a.rs"), src).unwrap();
        let out = call(dir.path(), json!([{"type": "text", "pattern": "hit", "definition": true, "contextLines": 3}]));
        if cfg!(feature = "lang-rust") {
            assert_eq!(out, "## Query 1 [text] hit\na.rs\n L1-5 fn first()\n 1- fn first() {\n 2-     let a = 1;\n 3:     hit(a);\n 4:     hit(a);\n 5- }", "{out}");
            assert!(!out.contains("second") && !out.contains("other"), "{out}");
        }
        // The tip shows for plain content queries only, never for glob-only or all-definition batches.
        let run_tip = |q: Value| run(json!({"cwd": dir.path(), "queries": q})).unwrap().ends_with(TIP);
        assert!(run_tip(json!([{"type": "text", "pattern": "hit"}])));
        assert!(run_tip(json!([{"type": "text", "pattern": "hit"}, {"type": "text", "pattern": "hit", "definition": true}])));
        assert!(!run_tip(json!([{"type": "text", "pattern": "hit", "definition": true}])));
        assert!(!run_tip(json!([{"type": "glob", "pattern": "*.rs"}])));
    }

    #[test]
    fn definition_clips_long_definitions_and_plain_text() {
        let dir = tempfile::tempdir().unwrap();
        let body: String = (2..=99).map(|i| format!("    line{i};\n")).collect();
        fs::write(dir.path().join("a.rs"), format!("fn long() {{\n{body}}}\n")).unwrap(); // 100 lines
        fs::write(dir.path().join("a.txt"), (1..=20).map(|i| format!("t{i}\n")).collect::<String>()).unwrap();
        let out = call(dir.path(), json!([{"type": "text", "pattern": "line50;", "definition": true}]));
        if cfg!(feature = "lang-rust") {
            assert!(out.contains(" 40-     line40;") && out.contains(" 51-     line51;") && out.contains(" 60-     line60;"), "{out}");
            assert!(!out.contains(" 39-") && !out.contains(" 61-"), "{out}");
            assert!(out.contains(" L1-100 fn long()"), "{out}");
        }
        let out = call(dir.path(), json!([{"type": "text", "pattern": "t10", "definition": true}]));
        assert_eq!(out.lines().skip(2).collect::<Vec<_>>(), [" 7- t7", " 8- t8", " 9- t9", " 10: t10", " 11- t11", " 12- t12", " 13- t13"]);
    }

    #[test]
    fn definition_stays_within_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let src: String = (0..6).map(|i| format!("fn f{i}() {{\n    hit {};\n}}\n", "x".repeat(200))).collect();
        fs::write(dir.path().join("a.rs"), src).unwrap();
        let out = run(json!({"cwd": dir.path(), "queries": [{"type": "text", "pattern": "hit", "definition": true}], "maxChars": 500})).unwrap();
        assert!(out.len() <= 500, "{}", out.len());

        if !cfg!(feature = "lang-rust") {
            return;
        }
        // An oversized first definition still returns >= 1 hit folded to budget, and skips to smaller files when any fit.
        let big: String = (0..40).map(|i| format!("    let x{i} = \"{}\";\n", "y".repeat(30))).collect();
        fs::write(dir.path().join("a.rs"), format!("fn huge0() {{\n{big}    hit();\n}}\nfn huge1() {{\n{big}    hit();\n}}\n")).unwrap();
        let out = run(json!({"cwd": dir.path(), "queries": [{"type": "text", "pattern": "hit", "definition": true}], "maxChars": 500})).unwrap();
        assert!(out.len() <= 500 && out.contains("a.rs") && out.contains("showing 1 of 2"), "{out}");

        fs::write(dir.path().join("b.rs"), "fn small() {\n    hit();\n}\n").unwrap();
        let out = run(json!({"cwd": dir.path(), "queries": [{"type": "text", "pattern": "hit", "definition": true}], "maxChars": 500})).unwrap();
        assert!(out.len() <= 500 && out.contains("b.rs") && out.contains("fn small()") && out.contains("showing 1 of 3"), "{out}");
    }

    #[test]
    fn definition_widens_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let big = format!("fn big_target() {{\n    let needle_here = 1;\n}}\n{}", "// xxx\n".repeat(300_000));
        fs::write(dir.path().join("big.rs"), big).unwrap();
        let out = call(dir.path(), json!([{"type": "text", "pattern": "needle_here", "definition": true}]));
        assert!(out.contains("fn big_target() {") && out.contains("needle_here"), "{out}");
    }

    #[test]
    fn multiline_matches_within_and_across_lines() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "foo\nbar\n").unwrap();
        let out = call(dir.path(), json!([{"type": "regex", "pattern": "fo+", "multiline": true}, {"type": "regex", "pattern": "foo\\nbar", "multiline": true}]));
        let s: Vec<&str> = out.split("\n\n").collect();
        assert!(s[0].ends_with("a.txt\n 1: foo") && s[1].ends_with("a.txt\n 1: foo\n 2: bar"), "{out}");
    }

    #[test]
    fn notice_counts_stay_exact_when_many_files_do_not_fit() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..300 {
            fs::write(dir.path().join(format!("f{i:03}.txt")), "hit\n".repeat(1 + i % 3)).unwrap();
        }
        let out = untip(run(json!({"cwd": dir.path(), "queries": [{"type": "text", "pattern": "hit"}], "maxChars": 2000})).unwrap());
        assert!(out.len() <= 2000, "{}", out.len());
        let notice = out.lines().last().unwrap();
        let shown = out.matches(": hit").count();
        assert!(notice.starts_with(&format!("... [truncated: showing {shown} of 600 matching lines in 300 files; not shown:")), "{notice}");
        // Listed per-file counts plus the summed-up rest cover exactly the hidden lines.
        let list = notice.split("not shown:").nth(1).unwrap().trim_end_matches(']');
        let (mut lines, mut files) = (0, 0);
        for item in list.split(',') {
            let w: Vec<&str> = item.split_whitespace().collect();
            if let Some(rest) = item.trim().strip_prefix('+') {
                files += rest.split(' ').next().unwrap().parse::<usize>().unwrap();
                lines += w[2].trim_start_matches('(').parse::<usize>().unwrap();
            } else {
                files += 1;
                lines += w[1].parse::<usize>().unwrap();
            }
        }
        assert_eq!(lines, 600 - shown, "{notice}");
        let mut shown_in = std::collections::HashMap::new();
        let mut file = "";
        for l in out.lines() {
            if l.starts_with('f') {
                file = l;
            } else if l.contains(": hit") {
                *shown_in.entry(file).or_insert(0) += 1;
            }
        }
        let full = (0..300).filter(|i| shown_in.get(format!("f{i:03}.txt").as_str()) == Some(&(1 + i % 3))).count();
        assert_eq!(files, 300 - full, "{notice}");
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_directories_are_reported() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("h.txt"), "needle\n").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let readable = fs::read_dir(&locked).is_ok(); // root ignores the mode
        let out = call(dir.path(), json!([{"type": "text", "pattern": "needle"}]));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        if !readable {
            assert_eq!(out, "## Query 1 [text] needle\nIncomplete search: 1 unreadable file(s).\n(no matches)");
        }
    }

    #[test]
    fn whitespace_patterns_work_for_text_and_regex_only() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "a\tb\n").unwrap();
        let out = call(dir.path(), json!([
            {"type": "text", "pattern": "\t"}, {"type": "regex", "pattern": "\t"}, {"type": "text", "pattern": ""},
            {"type": "fuzzy", "pattern": " "}, {"type": "glob", "pattern": "\t"},
        ]));
        let s: Vec<&str> = out.split("\n\n").collect();
        assert!(s[0].ends_with("a.txt\n 1: a\tb") && s[1].ends_with("a.txt\n 1: a\tb"), "{out}");
        assert!(s[2..].iter().all(|q| q.ends_with("Error: pattern must not be empty")), "{out}");
    }
}

