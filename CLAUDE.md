# agy-rs

One Rust binary with batched code-navigation tools for Antigravity and Claude Code. Design: `idea,md`. Behavioural reference (read-only, third-party): `reference/agy-fast/`.

## Commands

Cargo is not on PATH: `export PATH="$HOME/.cargo/bin:$PATH"`.

- `cargo test` — unit + integration tests
- `cargo clippy` — must be clean
- `cargo build --release` — the shipped binary
- `cargo run -- cli --help` — CLI syntax

## Layout

- `src/main.rs` — routes `mcp | hook session-start | hook pre-bash | cli`
- `src/mcp/` — stdio JSON-RPC server, tool schemas
- `src/cli.rs` — flags → the same JSON arguments the MCP tools take; `cli filter` compresses piped output
- `src/core/` — `budget` (8,800-byte cap, folding), `lock` (cross-process file locks), `cwd` (guard)
- `src/tools/` — one file per tool, all dispatched through `tools::call`
- `queries/*.scm` — tree-sitter outline queries
- `plugin/` — Antigravity bundle; `claude/` — Claude Code hooks (`hook pre-bash` rewrites Bash build commands in `src/tools/check.rs`), rules, README

## Rules

- Every tool is `pub fn run(args: serde_json::Value) -> Result<String, String>`; output never exceeds `budget::CAP` bytes.
- In `mcp` mode stdout is protocol only; log to stderr.
- No `deny_unknown_fields` on input structs (the host injects `toolSummary`).
- No tokio; use `rayon` or std threads.
- Don't add dependencies for a few lines of code.
- Don't run `install.sh` or touch `~/.gemini` / `~/.claude` without being asked.
