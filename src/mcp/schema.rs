//! Tool definitions for `tools/list`. Schemas stay open (no
//! `additionalProperties: false`) because Antigravity injects `toolSummary`.

use serde_json::{Value, json};

fn tool(name: &str, description: &str, required: &[&str], mut properties: Value) -> Value {
    properties["cwd"] = json!({"type": "string", "description": "Absolute workspace root."});
    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type": "object", "properties": properties, "required": required},
    })
}

pub fn tools() -> Value {
    let max_chars = json!({"type": "integer", "description": "Output byte cap, max 8800."});
    json!([
        tool(
            "batch_search",
            "1-20 searches concurrently in one call; honors .gitignore. Content hits carry `Lstart-end` enclosing-symbol ranges. For hits you will read or edit set `definition: true` instead of following up with `read_file`.",
            &["queries"],
            json!({
                "queries": {"type": "array", "minItems": 1, "maxItems": 20, "items": {
                    "type": "object", "required": ["type", "pattern"], "properties": {
                        "type": {"type": "string", "enum": ["text", "regex", "glob", "fuzzy", "git"]},
                        "pattern": {"type": "string", "description": "For git: read-only arguments, e.g. `status --short`."},
                        "path": {"type": "string"},
                        "glob": {"type": "string", "description": "File filter."},
                        "fileType": {"type": "string", "description": "e.g. rs, ts, py."},
                        "multiline": {"type": "boolean"},
                        "outputMode": {"type": "string", "enum": ["content", "files_with_matches", "count"]},
                        "sortBy": {"type": "string", "enum": ["path", "mtime"]},
                        "caseSensitive": {"type": "boolean"},
                        "contextLines": {"type": "integer", "minimum": 0, "maximum": 5},
                        "definition": {"type": "boolean", "description": "Show each hit's whole enclosing definition (<=60 lines, else ±10) instead of contextLines."},
                        "maxResults": {"type": "integer", "minimum": 1, "maximum": 200}
                    }
                }},
                "maxChars": max_chars
            }),
        ),
        tool(
            "quick_outline",
            "Symbols with line ranges for up to 40 files (Rust, TS/JS, Python, Go, JSON/TOML/YAML, Markdown headings). `file#Name[,Name]` returns those definitions' numbered source instead.",
            &["paths"],
            json!({
                "paths": {"type": "array", "minItems": 1, "maxItems": 40, "items": {"type": "string"}},
                "maxChars": max_chars
            }),
        ),
        tool(
            "apply_edits",
            "Exact-match hunks or file creates for 1-50 files, atomic and locked, then an optional verify command.",
            &["edits"],
            json!({
                "edits": {"type": "array", "minItems": 1, "maxItems": 50, "items": {
                    "type": "object", "required": ["path"], "properties": {
                        "path": {"type": "string"},
                        "hunks": {"type": "array", "items": {"type": "object", "required": ["target", "replacement"], "properties": {
                            "target": {"type": "string", "description": "Exact text; must match once unless replaceAll."},
                            "replacement": {"type": "string"},
                            "startLine": {"type": "integer", "description": "1-based hint narrowing the search."},
                            "endLine": {"type": "integer"},
                            "replaceAll": {"type": "boolean"}
                        }}},
                        "create": {"type": "string", "description": "New file content; not with hunks."},
                        "overwrite": {"type": "boolean", "description": "Let create replace a file."}
                    }
                }},
                "dryRun": {"type": "boolean"},
                "bestEffort": {"type": "boolean", "description": "Write the files that match even if others fail."},
                "verify": {"type": "string", "description": "Run after writing; `&&` steps run in order, each compressed, e.g. `cargo check --all-targets && cargo test`. Pass every check here; don't rerun them."},
                "verifyTimeoutSec": {"type": "integer"},
                "maxVerifyChars": {"type": "integer", "minimum": 300, "maximum": 7500}
            }),
        ),
        tool(
            "run_checks",
            "Run 1-12 non-interactive commands concurrently with timeouts; cargo, test-runner and linter output is compressed to errors and a summary. Use it for any build/test/lint command, even one; `&&` steps run in order.",
            &["commands"],
            json!({
                "commands": {"type": "array", "minItems": 1, "maxItems": 12, "items": {
                    "type": "object", "required": ["command"], "properties": {
                        "name": {"type": "string"},
                        "command": {"type": "string"},
                        "cwd": {"type": "string"},
                        "timeoutSec": {"type": "integer"}
                    }
                }},
                "concurrency": {"type": "integer", "minimum": 1, "maximum": 8},
                "timeoutSec": {"type": "integer"},
                "outputMode": {"type": "string", "enum": ["compact", "full"], "description": "full: show passing output too."},
                "maxChars": max_chars
            }),
        ),
    ])
}
