# agyfast-rs Tool Routing Rules

`agyfast-rs_code` MCP tools batch work behind one remote check (~1.8s) and compress output; built-in `read_file` stays for reads (local, ~7ms, 46 KB/call vs MCP's 8.8 KB cap).

| Task | Tool |
| :--- | :--- |
| Unknown file >300 lines (10-40 files per call) | `outline`, then ranged `read_file` |
| A definition you can name (fn, struct, impl, `tests` module), in any file >150 lines | `outline` with `file#Name[,Name]` (several files per call): its numbered source, not the whole file and not outline then `read_file` |
| Small files, known line ranges | built-in `read_file`, parallel, <=200 lines each |
| >=2 searches or read-only `git` queries | one `search`, per-query `path` |
| >=2 edited blocks or files, or edit + check | `edit` (`bestEffort: true` for >=3 files) |
| Any build/test/lint command (cargo, pytest, go test, tsc, eslint), even one | `check`, not native `run_command` |

## Rules

1. Call `mcp_agyfast-rs_code_*` directly (eager); don't read `mcp/agyfast-rs_code/*.json`. Always pass absolute `cwd` and a short `toolSummary`.
2. After edits put the whole check chain in `edit.verify`, e.g. `cargo check --all-targets && cargo test` (`&&` steps run in order, each compressed), and don't rerun those commands. Plain commands like `git checkout` stay on `run_command`.
3. One call per agyfast-rs tool per turn, never two in parallel. If `edit` reports corrupted nested arguments (empty `edits`, booleans in `edits`), don't retry; use built-in `edit_file`/`write_file`.
4. Read the `SessionStart` snapshot before `ls`/`git status`; `git diff --stat` before full diffs; never blind-read `1-800`.
5. Reads are the largest cost: give `search` queries whose hits you will read or edit `definition: true` (each hit returns its whole definition) and edit straight from those lines; `read_file` only what that does not show.
6. Keep checks under ~45s; defer >60s builds to phase end. For <=10s `run_command` set `WaitMsBeforeAsync` 5000-10000 instead of backgrounding.
7. `/swarm` workers get no MCP, rules or `SessionStart`: run `"$HOME/.gemini/config/plugins/agyfast-rs/bin/agyfast-rs" cli worker-brief` and paste its output into each worker prompt, filling in `--cwd <WORKSPACE>`. Give every file exactly one owner: a file several workers must change goes to one worker, or to the coordinator after the workers finish.
