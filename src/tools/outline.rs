//! quick_outline: one tree-sitter query engine for every language, plus a line
//! scanner for Markdown, unsupported extensions and parse/query failures.

use crate::core::{budget, cwd};
use crate::tools::slices::slice_text;
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

const MAX_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SIG: usize = 160;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Args {
    cwd: Option<String>,
    #[serde(default)]
    paths: Vec<String>,
    max_chars: Option<usize>,
}

pub fn run(args: Value) -> Result<String, String> {
    let args: Args = serde_json::from_value(args).map_err(|e| format!("Error: invalid arguments: {e}"))?;
    if args.paths.is_empty() || args.paths.len() > 40 {
        return Err(format!("Error: `paths` needs 1..40 entries, got {}", args.paths.len()));
    }
    let dir = cwd::resolve(args.cwd.as_deref())?;
    // `FILE#NAME[,NAME]` reads definitions (no names: outline); one section per file.
    let mut jobs: Vec<(String, Vec<String>)> = Vec::new();
    for p in &args.paths {
        match p.rsplit_once('#').filter(|_| !dir.join(p).exists()) {
            Some((file, names)) => {
                let names = names.split(',').map(str::trim).filter(|n| !n.is_empty()).map(String::from);
                match jobs.iter_mut().find(|j| j.0 == file && !j.1.is_empty()) {
                    Some(j) => j.1.extend(names),
                    None => jobs.push((file.to_string(), names.collect())),
                }
            }
            None => jobs.push((p.clone(), Vec::new())),
        }
    }
    let parts: Vec<String> = jobs.par_iter().map(|(p, names)| if names.is_empty() { outline_file(&dir.join(p), p) } else { definitions(&dir.join(p), p, names) }).collect();
    Ok(budget::join_fair(&parts, budget::limit(args.max_chars, 1000)))
}

/// File text for outlining or slicing; `Err` is a one-line reason to show inline.
pub fn read_text(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a regular file".into());
    }
    if meta.len() > MAX_BYTES {
        return Err(format!("skipped: {:.1} MB is over the 2 MB limit", meta.len() as f64 / 1048576.0));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return Err("skipped: binary file".into());
    }
    let text = String::from_utf8_lossy(&bytes);
    Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string())
}

/// One `## path (N lines)` section; `shown` is the path as the caller wrote it.
pub fn outline_file(path: &Path, shown: &str) -> String {
    let src = match read_text(path) {
        Ok(s) => s,
        Err(e) if e.starts_with("skipped") => return format!("## {shown} ({e})"),
        Err(e) => return format!("## {shown}\nERROR: {e}"),
    };
    outline_text(path, shown, &src)
}

fn outline_text(path: &Path, shown: &str, src: &str) -> String {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let rows = outline_source(&ext, src);
    let body = if rows.is_empty() { "(no symbols detected)".to_string() } else { rows.join("\n") };
    format!("## {shown} ({} lines)\n{body}", src.lines().count())
}

/// Numbered source of every definition named in `names` (`Outer::inner` or `Outer.inner` for nesting),
/// each extended up over its attribute/doc/decorator lines; otherwise a one-line reason plus the outline.
fn definitions(path: &Path, shown: &str, names: &[String]) -> String {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let Ok(src) = read_text(path) else { return outline_file(path, shown) };
    let Some((lang, query)) = spec(&ext) else {
        return format!("No grammar for `.{ext}` files, so names can't be looked up; outline follows:\n{}", outline_text(path, shown, &src));
    };
    let Some(entries) = tree_entries(lang, query, &src) else {
        return format!("Could not parse {shown}; outline follows:\n{}", outline_text(path, shown, &src));
    };
    let lines: Vec<&str> = src.lines().collect();
    let mut ranges = Vec::new();
    let mut missing = Vec::new();
    for name in names {
        let segs: Vec<&str> = name.split("::").flat_map(|s| s.split('.')).collect();
        let (last, outer) = segs.split_last().unwrap_or((&"", &[]));
        let mut found = false;
        for (i, e) in entries.iter().enumerate().filter(|(_, e)| e.name == *last && e.collapse != Some(false)) {
            // Enclosing items, innermost first; `outer` must appear among them in order.
            let mut anc: Vec<&Entry> = entries.iter().enumerate().filter(|&(j, a)| j != i && a.collapse.is_none() && a.start <= e.start && a.end >= e.end).map(|(_, a)| a).collect();
            anc.sort_by_key(|a| (std::cmp::Reverse(a.start), a.end));
            let mut want = outer.iter().rev().peekable();
            for a in anc {
                if want.peek().is_some_and(|w| **w == a.name) {
                    want.next();
                }
            }
            if want.peek().is_some() {
                continue;
            }
            let mut s = e.lines.0;
            while s > 1 && ["#", "//", "@"].iter().any(|p| lines[s - 2].trim_start().starts_with(p)) {
                s -= 1;
            }
            ranges.push(Ok((s as i64, e.lines.1 as i64)));
            found = true;
        }
        if !found {
            missing.push(format!("`{name}`"));
        }
    }
    let note = || format!("No definition named {} in {shown}; outline follows:\n{}", missing.join(", "), outline_text(path, shown, &src));
    match (ranges.is_empty(), missing.is_empty()) {
        (true, _) => note(),
        (false, true) => slice_text(shown, &src, &ranges),
        (false, false) => format!("{}\n{}", slice_text(shown, &src, &ranges), note()),
    }
}

fn outline_source(ext: &str, src: &str) -> Vec<String> {
    if let Some((lang, query)) = spec(ext)
        && let Some(rows) = tree_outline(lang, query, src)
    {
        return rows;
    }
    if matches!(ext, "md" | "markdown" | "mdx") { markdown_outline(src) } else { scan_outline(src) }
}

/// The `LangSpec` table: grammar plus compiled query, cached per language.
fn spec(ext: &str) -> Option<&'static (tree_sitter::Language, tree_sitter::Query)> {
    #[allow(unused_macros)]
    macro_rules! cache {
        ($lang:expr, $query:expr) => {{
            static CELL: std::sync::OnceLock<Option<(tree_sitter::Language, tree_sitter::Query)>> = std::sync::OnceLock::new();
            CELL.get_or_init(|| {
                let lang: tree_sitter::Language = $lang.into();
                let query = tree_sitter::Query::new(&lang, $query).ok()?;
                Some((lang, query))
            }).as_ref()
        }};
    }
    match ext {
        #[cfg(feature = "lang-rust")]
        "rs" => cache!(tree_sitter_rust::LANGUAGE, include_str!("../../queries/rust.scm")),
        #[cfg(feature = "lang-ts")]
        "ts" | "mts" | "cts" => cache!(tree_sitter_typescript::LANGUAGE_TYPESCRIPT, include_str!("../../queries/typescript.scm")),
        #[cfg(feature = "lang-ts")]
        "tsx" => cache!(tree_sitter_typescript::LANGUAGE_TSX, include_str!("../../queries/typescript.scm")),
        #[cfg(feature = "lang-ts")]
        "js" | "jsx" | "mjs" | "cjs" => cache!(tree_sitter_javascript::LANGUAGE, include_str!("../../queries/javascript.scm")),
        #[cfg(feature = "lang-python")]
        "py" | "pyi" => cache!(tree_sitter_python::LANGUAGE, include_str!("../../queries/python.scm")),
        #[cfg(feature = "lang-go")]
        "go" => cache!(tree_sitter_go::LANGUAGE, include_str!("../../queries/go.scm")),
        #[cfg(feature = "lang-config")]
        "json" | "jsonc" => cache!(tree_sitter_json::LANGUAGE, include_str!("../../queries/json.scm")),
        #[cfg(feature = "lang-config")]
        "toml" => cache!(tree_sitter_toml_ng::LANGUAGE, include_str!("../../queries/toml.scm")),
        #[cfg(feature = "lang-config")]
        "yaml" | "yml" => cache!(tree_sitter_yaml::LANGUAGE, include_str!("../../queries/yaml.scm")),
        _ => None,
    }
}

/// 1-based inclusive line range; a node ending at column 0 does not own that line.
fn lines_of(n: tree_sitter::Node) -> (usize, usize) {
    let (s, e) = (n.start_position(), n.end_position());
    let end = if e.column == 0 && e.row > s.row { e.row - 1 } else { e.row };
    (s.row + 1, end + 1)
}

pub fn span(s: usize, e: usize) -> String {
    if e > s { format!("L{s}-{e}") } else { format!("L{s}") }
}

/// Whitespace collapsed to single spaces, capped at `MAX_SIG` chars.
fn one_line(s: &str) -> String {
    let mut out = s.split_whitespace().collect::<Vec<_>>().join(" ").replace("( ", "(").replace(", )", ")").replace(" )", ")");
    if let Some((i, _)) = out.char_indices().nth(MAX_SIG) {
        out.truncate(i);
        out.push('…');
    }
    out
}

struct Entry {
    start: usize,
    end: usize,
    lines: (usize, usize),
    text: String,
    /// The item's `name`, `type` (impl blocks, generics stripped) or `key` field; empty for imports.
    name: String,
    /// None: an item. Some(false): an import. Some(true): a test module.
    collapse: Option<bool>,
}

fn tree_outline(lang: &tree_sitter::Language, query: &tree_sitter::Query, src: &str) -> Option<Vec<String>> {
    tree_entries(lang, query, src).map(render)
}

/// Definitions as (first line, last line, one-line signature), sorted by start,
/// for files a grammar supports; empty for other files or on any failure.
pub fn symbols(path: &Path) -> Vec<(usize, usize, String)> {
    let Ok(src) = read_text(path) else { return Vec::new() };
    symbols_in(path, &src)
}

pub(crate) fn symbols_in(path: &Path, src: &str) -> Vec<(usize, usize, String)> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let Some((lang, query)) = spec(&ext) else { return Vec::new() };
    let entries = tree_entries(lang, query, src).unwrap_or_default();
    entries.into_iter().filter(|e| e.collapse.is_none()).map(|e| (e.lines.0, e.lines.1, e.text)).collect()
}

fn tree_entries(lang: &tree_sitter::Language, query: &tree_sitter::Query, src: &str) -> Option<Vec<Entry>> {
    use tree_sitter::StreamingIterator;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(lang).ok()?;
    let tree = parser.parse(src, None)?;
    let idx = |name| query.capture_index_for_name(name);
    let (item_i, body_i, coll_i) = (idx("item"), idx("body"), idx("collapse"));

    // Several patterns may hit the same item; any match that found a body wins.
    let mut items: HashMap<usize, (tree_sitter::Node, Option<usize>)> = HashMap::new();
    let mut entries = Vec::new();
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), src.as_bytes());
    while let Some(m) = matches.next() {
        let of = |i| m.captures().iter().filter(move |c| Some(c.index) == i).map(|c| c.node);
        let coll: Vec<_> = of(coll_i).collect();
        if let (Some(first), Some(last)) = (coll.iter().min_by_key(|n| n.start_byte()), coll.iter().max_by_key(|n| n.end_byte())) {
            let test = coll.iter().any(|n| n.kind() == "mod_item");
            entries.push(Entry {
                start: first.start_byte(),
                end: last.end_byte(),
                lines: (lines_of(*first).0, lines_of(*last).1),
                text: if test { src[first.start_byte()..last.end_byte()].to_string() } else { String::new() },
                name: coll.iter().find(|n| n.kind() == "mod_item").map(|n| name_of(*n, src, 2)).unwrap_or_default(),
                collapse: Some(test),
            });
        } else if let Some(item) = of(item_i).next() {
            let body = of(body_i).next().map(|b| b.start_byte());
            let slot = items.entry(item.id()).or_insert((item, None));
            slot.1 = slot.1.or(body);
        }
    }
    for (node, body) in items.into_values() {
        let text = &src[node.start_byte()..body.unwrap_or(node.end_byte())];
        let text = if body.is_some() { text } else { text.lines().next().unwrap_or("") };
        entries.push(Entry { start: node.start_byte(), end: node.end_byte(), lines: lines_of(node), text: one_line(text), name: name_of(node, src, 2), collapse: None });
    }
    entries.sort_by_key(|e| (e.start, std::cmp::Reverse(e.end), e.collapse.is_none()));
    Some(entries)
}

/// The definition's name from the syntax tree; wrappers (export, decorators, `const x = ...`) are
/// looked through up to `depth` levels.
fn name_of(n: tree_sitter::Node, src: &str, depth: usize) -> String {
    if let Some(c) = ["name", "type", "key"].iter().find_map(|f| n.child_by_field_name(f)) {
        let t = &src[c.byte_range()];
        let t = t.split('<').next().unwrap_or(t).trim_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace());
        return t.rsplit("::").next().unwrap_or(t).to_string();
    }
    match ["declaration", "definition"].iter().find_map(|f| n.child_by_field_name(f)).or_else(|| n.named_child(0)) {
        Some(c) if depth > 0 => name_of(c, src, depth - 1),
        _ if n.named_child_count() == 0 && depth < 2 => src[n.byte_range()].to_string(),
        _ => String::new(),
    }
}

fn render(entries: Vec<Entry>) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    let mut parents: Vec<usize> = Vec::new();
    let mut hidden_until = 0;
    let mut run: Option<(usize, usize, usize)> = None; // open import run: (count, first, last line)
    let flush = |run: &mut Option<(usize, usize, usize)>, rows: &mut Vec<String>| {
        if let Some((n, s, e)) = run.take() {
            rows.push(format!("{} ({n} import{})", span(s, e), if n == 1 { "" } else { "s" }));
        }
    };
    for e in entries {
        if e.start < hidden_until {
            continue;
        }
        while parents.last().is_some_and(|&end| end <= e.start) {
            parents.pop();
        }
        let (s, l) = e.lines;
        match e.collapse {
            Some(_) if !parents.is_empty() => {}
            Some(false) => match &mut run {
                Some((n, _, last)) => (*n, *last) = (*n + 1, (*last).max(l)),
                None => run = Some((1, s, l)),
            },
            Some(true) => {
                flush(&mut run, &mut rows);
                hidden_until = e.end;
                let tests = e.text.lines().map(str::trim).filter(|t| t.starts_with("#[") && t.contains("test") && !t.contains("cfg(test")).count();
                let head = e.text.lines().map(str::trim).find(|t| t.contains("mod ")).unwrap_or("mod");
                rows.push(format!("{} #[cfg(test)] {} ({tests} test{})", span(s, l), head.trim_end_matches('{').trim(), if tests == 1 { "" } else { "s" }));
            }
            None => {
                flush(&mut run, &mut rows);
                rows.push(format!("{}{} {}", "  ".repeat(parents.len()), span(s, l), e.text));
                parents.push(e.end);
            }
        }
    }
    flush(&mut run, &mut rows);
    rows
}

/// Headings outside fenced code; each spans until the next heading of the same or higher level.
fn markdown_outline(src: &str) -> Vec<String> {
    let lines: Vec<&str> = src.lines().collect();
    let mut heads = Vec::new();
    let mut fence: Option<&str> = None;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if let Some(f) = ["```", "~~~"].into_iter().find(|f| t.starts_with(f)) {
            fence = if fence == Some(f) { None } else { fence.or(Some(f)) };
            continue;
        }
        let level = t.bytes().take_while(|&b| b == b'#').count();
        if fence.is_none() && (1..=6).contains(&level) && t[level..].starts_with([' ', '\t']) {
            heads.push((i, level, one_line(t)));
        }
    }
    heads.iter().enumerate().map(|(k, (i, level, text))| {
        let end = heads[k + 1..].iter().find(|h| h.1 <= *level).map_or(lines.len(), |h| h.0);
        let end = (*i + 1..end).rev().find(|&j| !lines[j].trim().is_empty()).unwrap_or(*i);
        format!("{} {text}", span(i + 1, end + 1))
    }).collect()
}

const DEF_WORDS: &[&str] = &[
    "fn", "func", "fun", "function", "def", "class", "struct", "enum", "interface", "trait", "impl",
    "module", "namespace", "object", "record", "protocol", "extension", "union", "sub", "proc", "macro",
];
const MODIFIERS: &[&str] = &[
    "export", "pub", "public", "private", "protected", "internal", "static", "async", "abstract", "final",
    "override", "virtual", "inline", "extern", "default", "open", "sealed", "data", "suspend", "unsafe", "const",
];
const CONTROL: &[&str] = &["if", "for", "while", "switch", "return", "else", "catch", "new", "throw", "case", "do", "await", "yield", "delete", "typeof"];

/// Conservative guess at a definition line in an unsupported language.
fn looks_like_def(line: &str) -> bool {
    let t = line.trim();
    if t.ends_with(';') || t.ends_with(',') || t.starts_with(['/', '*', '#', '-', '<', '@', '"', '\'']) {
        return false;
    }
    let mut words = t.split_whitespace().skip_while(|w| MODIFIERS.contains(w) || w.starts_with("pub("));
    let Some(first) = words.next() else { return false };
    let first = first.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_');
    if DEF_WORDS.contains(&first) {
        return words.next().is_some_and(|w| w.starts_with(|c: char| c.is_alphabetic() || c == '_'));
    }
    // C/Java-style `Type name(args) {`: at least two words before `(` and no `=` there.
    let Some(open) = t.find('(') else { return false };
    let head = &t[..open];
    let ws: Vec<&str> = head.split_whitespace().collect();
    ws.len() >= 2
        && !head.contains(['=', '.', '!', '|', '&', '+'])
        && !CONTROL.iter().any(|c| ws.contains(c))
        && ws.last().is_some_and(|w| w.trim_start_matches('*').starts_with(|c: char| c.is_alphabetic() || c == '_'))
        && (t.ends_with('{') || t.ends_with(')'))
}

/// Definition-looking lines; each spans until the next one at the same or lower indent.
fn scan_outline(src: &str) -> Vec<String> {
    let lines: Vec<&str> = src.lines().collect();
    let indent = |l: &str| l.len() - l.trim_start().len();
    let defs: Vec<(usize, usize)> = lines.iter().enumerate().filter(|(_, l)| looks_like_def(l)).map(|(i, l)| (i, indent(l))).collect();
    let mut stack: Vec<usize> = Vec::new();
    defs.iter().enumerate().map(|(k, &(i, ind))| {
        while stack.last().is_some_and(|&p| p >= ind) {
            stack.pop();
        }
        let depth = stack.len();
        stack.push(ind);
        let next = defs[k + 1..].iter().find(|d| d.1 <= ind).map_or(lines.len(), |d| d.0);
        let end = (i + 1..next).find(|&j| !lines[j].trim().is_empty() && indent(lines[j]) < ind).unwrap_or(next);
        let end = (i + 1..end).rev().find(|&j| !lines[j].trim().is_empty()).unwrap_or(i);
        format!("{}{} {}", "  ".repeat(depth), span(i + 1, end + 1), one_line(lines[i]))
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outline(ext: &str, src: &str) -> String {
        outline_source(ext, src).join("\n")
    }

    /// Every grammar loads, every query compiles, and one known symbol lands on its lines.
    #[test]
    fn every_language_parses_and_queries_compile() {
        let cases: &[(&str, &str, &str)] = &[
            #[cfg(feature = "lang-rust")]
            ("rs", "use a::b;\nuse c::d;\n\npub struct Cache {\n  size: usize,\n}\n\nimpl Cache {\n  pub fn new(size: usize) -> Self {\n    Self { size }\n  }\n}\n", "  L9-11 pub fn new(size: usize) -> Self"),
            #[cfg(feature = "lang-ts")]
            ("ts", "import a from 'a';\nexport class Server {\n  start() {\n    return 1;\n  }\n}\nexport const createApp = (cfg: Config) => {\n  return 1;\n};\n", "L7-9 export const createApp = (cfg: Config) =>"),
            #[cfg(feature = "lang-ts")]
            ("tsx", "export function App(): JSX.Element {\n  return <div/>;\n}\n", "L1-3 export function App(): JSX.Element"),
            #[cfg(feature = "lang-ts")]
            ("js", "const x = 1;\nclass A {\n  run(a) {\n    return <b>{a}</b>;\n  }\n}\n", "  L3-5 run(a)"),
            #[cfg(feature = "lang-ts")]
            ("js", "var A = class {\n  run(a) {\n    return <b>{a}</b>;\n  }\n};\n", "L1-5 var A = class"),
            #[cfg(feature = "lang-python")]
            ("py", "import os\nclass Worker:\n    @property\n    def run(self):\n        return 42\n\nasync def helper():\n    return True\n", "  L3-5 @property def run(self):"),
            #[cfg(feature = "lang-go")]
            ("go", "package main\n\nimport (\n  \"fmt\"\n  \"os\"\n)\n\nfunc (e *Engine) Start() {\n  fmt.Println(os.Args)\n}\n", "L8-10 func (e *Engine) Start()"),
            #[cfg(feature = "lang-config")]
            ("json", "{\n    \"name\": \"demo\",\n    \"scripts\": {\n        \"test\": \"node --test\"\n    }\n}\n", "  L4 \"test\": \"node --test\""),
            #[cfg(feature = "lang-config")]
            ("toml", "[package]\nname = \"x\"\n\n[dependencies]\nserde = \"1\"\n", "  L5 serde = \"1\""),
            #[cfg(feature = "lang-config")]
            ("yaml", "on:\n  push:\n    branches: [main]\njobs:\n  build: {}\n", "  L5 build: {}"),
        ];
        for (ext, src, want) in cases {
            assert!(spec(ext).is_some(), "{ext} grammar/query failed");
            let got = outline(ext, src);
            assert!(got.lines().any(|l| l == *want), "{ext}: want {want:?} in\n{got}");
        }
    }

    #[cfg(feature = "lang-rust")]
    #[test]
    fn rust_collapses_imports_and_test_modules() {
        let src = "use a;\n// c\nuse b::{c, d};\n\nfn main() {}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n    #[test]\n    fn one() {}\n    #[test]\n    fn two() {}\n}\n";
        assert_eq!(outline("rs", src), "L1-3 (2 imports)\nL5 fn main()\nL7-14 #[cfg(test)] mod tests (2 tests)");
    }

    #[cfg(feature = "lang-go")]
    #[test]
    fn go_counts_each_import_spec() {
        assert!(outline("go", "package m\nimport \"a\"\nimport (\n\"b\"\n\"c\"\n)\n").starts_with("L2-6 (3 imports)"));
    }

    #[cfg(feature = "lang-rust")]
    #[test]
    fn multibyte_signatures_keep_char_boundaries() {
        let src = format!("pub fn привіт_{}() {{}}\n", "я".repeat(300));
        let got = outline("rs", &src);
        assert!(got.starts_with("L1 pub fn привіт_") && got.ends_with('…'), "{got}");
    }

    #[test]
    fn markdown_skips_fences_and_spans_sections() {
        let src = "# Title\nIntro.\n```bash\n# not a heading\n```\n## Usage\nDetails.\n";
        assert_eq!(outline("md", src), "L1-7 # Title\nL6-7 ## Usage");
    }

    #[test]
    fn scanner_finds_c_and_java_definitions() {
        let c = "int main(int argc, char **argv) {\n  if (x) {\n  return 0;\n}\nstatic void helper(void) {\n}\n";
        assert_eq!(outline("c", c), "L1-4 int main(int argc, char **argv) {\nL5-6 static void helper(void) {");
        let java = "public class Service {\n  public int add(int a, int b) {\n    return a + b;\n  }\n}\n";
        assert_eq!(outline("java", java), "L1-5 public class Service {\n  L2-4 public int add(int a, int b) {");
    }

    #[cfg(any(feature = "lang-rust", all(feature = "lang-ts", feature = "lang-python")))]
    fn defs(files: &[(&str, &str)], paths: &[&str]) -> String {
        let dir = tempfile::tempdir().unwrap();
        for (name, src) in files {
            std::fs::write(dir.path().join(name), src).unwrap();
        }
        run(serde_json::json!({"cwd": dir.path(), "paths": paths})).unwrap()
    }

    #[cfg(feature = "lang-rust")]
    #[test]
    fn names_return_numbered_definitions() {
        let src = "use a;\n\n/// Doc.\n#[derive(Debug)]\npub struct Args {\n    x: u8,\n}\n\nfn run() {}\n\nstruct Collect<T>(T);\nimpl<T> Sink for Collect<T> {\n    fn inner(&self) {}\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn one() {}\n}\n";
        let files = [("a.rs", src)];
        // Two names in one entry plus the same file again: one section, doc comment and attribute included.
        let out = defs(&files, &["a.rs#Args,run", "a.rs#Collect::inner"]);
        assert_eq!(out, "## Slice: a.rs (20 lines)\n--- L3-7 ---\n3: /// Doc.\n4: #[derive(Debug)]\n5: pub struct Args {\n6:     x: u8,\n7: }\n--- L9-9 ---\n9: fn run() {}\n--- L13-13 ---\n13:     fn inner(&self) {}");
        assert!(defs(&files, &["a.rs#Collect"]).contains("--- L11-14 ---"));
        assert!(defs(&files, &["a.rs#Args::inner"]).starts_with("No definition named `Args::inner` in a.rs; outline follows:\n## a.rs (20 lines)\nL1 (1 import)"));
        assert!(defs(&files, &["a.rs#tests"]).contains("--- L16-20 ---\n16: #[cfg(test)]"));
        assert!(defs(&files, &["a.rs#one"]).contains("--- L18-19 ---\n18:     #[test]\n19:     fn one() {}"));
        // Found and unknown names together, mixed with a plain path that still outlines unchanged.
        let out = defs(&files, &["a.rs#run,nope", "a.rs"]);
        let plain = format!("## a.rs (20 lines)\n{}", outline("rs", src));
        assert_eq!(out, format!("## Slice: a.rs (20 lines)\n--- L9-9 ---\n9: fn run() {{}}\nNo definition named `nope` in a.rs; outline follows:\n{plain}\n\n{plain}"));
        assert_eq!(defs(&files, &["a.rs"]), plain);
    }

    #[cfg(feature = "lang-rust")]
    #[test]
    fn long_definitions_fold_within_cap() {
        let body: String = (0..2000).map(|i| format!("    let x{i} = {i};\n")).collect();
        let out = defs(&[("big.rs", &format!("fn big() {{\n{body}}}\n"))], &["big.rs#big"]);
        assert!(out.len() <= budget::CAP && out.contains("[truncated:") && out.starts_with("## Slice: big.rs"), "{}", out.len());
    }

    #[cfg(all(feature = "lang-ts", feature = "lang-python"))]
    #[test]
    fn names_in_typescript_and_python() {
        let ts = "// Starts it.\nexport const createApp = (cfg: Config) => {\n  return 1;\n};\nexport class Server {\n  @get()\n  start() {\n    return 1;\n  }\n}\n";
        let py = "class Worker:\n    # Runs.\n    @property\n    def run(self):\n        return 42\n";
        let files = [("a.ts", ts), ("b.py", py)];
        assert!(defs(&files, &["a.ts#createApp"]).contains("--- L1-4 ---\n1: // Starts it."));
        assert!(defs(&files, &["a.ts#Server.start"]).contains("--- L6-9 ---\n6:   @get()"));
        assert!(defs(&files, &["b.py#Worker.run"]).contains("--- L2-5 ---\n2:     # Runs.\n3:     @property"));
        assert!(defs(&[("n.md", "# T\n")], &["n.md#T"]).starts_with("No grammar for `.md` files"));
    }

    #[test]
    fn binary_and_missing_files_are_one_line_notes() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("x.bin");
        std::fs::write(&bin, [0u8, 1, 2]).unwrap();
        assert_eq!(outline_file(&bin, "x.bin"), "## x.bin (skipped: binary file)");
        assert!(outline_file(&dir.path().join("nope.rs"), "nope.rs").starts_with("## nope.rs\nERROR:"));
    }
}
