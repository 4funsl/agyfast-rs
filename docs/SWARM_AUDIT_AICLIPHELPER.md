# Swarm & Large-Repo Audit Report (`agyfast-rs` on `aicliphelper`)

**Date:** 2026-11-26  
**Target repo:** `D:\05Code\aicliphelper` (Windows 10/11 x64 Rust crate, 15 modules in `src/*.rs`, ~36,000 LOC, largest files: `src/main.rs` 7,308 LOC, `src/win32.rs` 5,019 LOC, `src/layout_win32.rs` 4,222 LOC)  
**Tested `agyfast-rs` binary:** `0.1.3` local build (`size=9,347,584`, `mtime=2026-11-26 13:13:28`, `sha256=e663b96f4c5edb96`)  
**Saved git branches (in `D:\05Code\aicliphelper-audit`):**
- `audit-baseline` (`cfea373`) — shared starting snapshot
- `audit-swarm-with-plugin` (`9c45cdf`) — Run 1 (with `agyfast-rs cli` `worker-brief`)
- `audit-swarm-without-plugin` (`15f8014`) — Run 2 (without `agyfast-rs`, built-in tools only)

---

## 1. Single-Session MCP Benchmarks (`agy -p`, `gemini-4-hydrogen-medium`, read-only)

### Task 1 — Standard orientation + 4 struct lookups + `terminal_ctrl_c_fallback` trace + `cargo check --all-targets`

| Metric | With `agyfast-rs` MCP | Without `agyfast-rs` | Delta |
| --- | --- | --- | --- |
| Wall time | **24.35 s** | 38.04 s | **−36.0% (−13.68 s)** |
| LLM turns | **2** | 4 | **−50%** |
| Tool calls (tool time) | **3** (`7.36 s`) | 16 (`35.95 s`) | **−81.3% calls / −79.5% time** |
| Tool errors | 0 | 0 | — |
| `input` / `cache_read` / `output` tokens | `30,889` / `23,445` / `3,943` | `32,008` / `72,407` / `5,369` | `cache_read` **−67.6%**, `output` **−26.6%** |
| Accuracy (4/4) | 100% | 100% | Parity |

### Task 2 — Large `.rs` file quality stress test (`src/main.rs` 7,308 LOC + `src/layout_win32.rs` 4,222 LOC, 213-line `run_manual_convert` truncated mid-body by `quick_outline`)

| Metric | With `agyfast-rs` MCP | Without `agyfast-rs` | Delta |
| --- | --- | --- | --- |
| Wall time | **28.76 s** | 34.56 s | **−16.8% (−5.81 s)** |
| LLM turns | **3** | 4 | **−25%** |
| Tool calls (tool time) | **4** (`4.67 s`) | 7 (`11.68 s`) | **−42.9% calls / −60.0% time** |
| `input` / `cache_read` / `output` tokens | `30,022` / `50,814` / `4,856` | `32,295` / `60,523` / `4,783` | `input` **−7.0%**, `cache_read` **−16.0%** |
| Quality rubric (10/10 verified ground-truth points) | **10 / 10** | **10 / 10** | **No degradation** |

**Large-file verdict:** `quick_outline` truncated the middle of `run_manual_convert` (`L2625–2747`) with an explicit `... [truncated: 5970 bytes omitted] ...` marker; the model immediately followed up with two targeted `read_file` calls (`0.17 s` + `0.19 s`) and scored 10/10 with zero hallucinations.

---

## 2. Native `/swarm` A/B Benchmark (7 Real Audit Fixes + Tests)

**Task:** Implement 7 Ponytail audit fixes (#1 `ModifierTracker` stuck-modifier recovery, #2 fail-closed `SendMessageTimeoutW` + bounded UIA password probe, #3 RAII `JobSettleGuard` for worker panics, #4 `config.json.tmp` cleanup + Windows `rename` retry, #5 `TargetExclusionCache` per-HWND, #6 `OsLayoutCatalog::refresh_force` clearing removed HKLs, #7 remove unused `arboard` dependency) + focused unit tests.  
**Roster (identical models in both runs):**
- `win32-layout` / `win32-layout-np` — `Gemini 4 Neon (XHigh)` (`src/layout_win32.rs`)
- `runtime-config` / `runtime-config-np` — `Gemini 4 Hydrogen (XHigh)` (`src/main.rs`, `src/jobs.rs`, `src/config.rs`, `Cargo.toml`)
- `verifier` / `verifier-np` — `Gemini 4 Neon (High)` (read-only diff review + `cargo check --all-targets && cargo clippy --all-targets -- -D warnings && cargo test`)

### Results Table (Measured from Board Timestamps, Git Diffs & Worker Transcripts)

| Metric | Run 1 — With `agyfast-rs cli` (`9c45cdf`) | Run 2 — Without `agyfast-rs` (`15f8014`) | Delta (R1 vs R2) |
| --- | --- | --- | --- |
| `runtime-config` wall | 4m 49s (+18s fix) | 4m 09s (+27s fix) | +31s |
| `win32-layout` wall | 12m 03s (+20s fix) | 9m 19s (+20s fix) | +2m 44s |
| `verifier` wall | 5m 28s *(cold `target/`)* | 3m 42s *(warm `target/`)* | +1m 46s *(cold build)* |
| Total wall | 18m 01s | 13m 20s | +4m 41s |
| **Total `input_tokens`** | **493,314** | 570,669 | **−77,355 (−13.6%)** |
| **Total `cache_read_tokens`** | **6,908,153** | 8,489,078 | **−1,580,925 (−18.6%)** |
| Total `output_tokens` | 147,719 | 135,295 | +12,424 (+9.2%) |
| **Total (`input + output`)** | **641,033** | 705,964 | **−64,931 (−9.2%)** |
| Total transcript bytes | **955 KB** | 1,221 KB | **−21.8%** |
| Total LLM turns (all 3 workers) | 69 (`20 + 31 + 18`) | 64 (`20 + 31 + 13`) | +5 *(cold build waits)* |
| Verifier retries | 2 (HRTB + `nonminimal_bool`) | 2 (same 2!) | Parity |
| Tests (`cargo test`) | **355 passed, 0 failed, 3 ignored** | **355 passed, 0 failed, 3 ignored** | Parity |
| `clippy --all-targets -- -D warnings` | PASS (0 warnings) | PASS (0 warnings) | Parity |
| Diff size (6 files) | **`+1024 / -177`** (`+571` prod, `+453` test) | `+1131 / -184` (`+572` prod, `+559` test) | **−107 lines leaner** |

### Per-Worker Token Breakdown

| Worker | Turns | `input_tokens` | `cache_read_tokens` | `output_tokens` | `read_file` | `run_command` | `edit_file` | CLI errors |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `R1_win32-layout` | 20 | **200,510** | **2,181,149** | 82,493 | 25 | 15 | 16 | 2 |
| `R2_win32-layout-np` | 20 | 239,714 | 3,091,096 | 76,595 | 14 | 9 | 17 | 1 |
| `R1_runtime-config` | 31 | **180,897** | **3,461,962** | 50,424 | 29 | 18 | 13 | **6** |
| `R2_runtime-config-np` | 31 | 217,876 | 4,463,952 | 46,424 | 29 | 13 | 14 | 0 |
| `R1_verifier` | 18 | 111,907 | 1,265,042 | 14,802 | 12 | 13 | 0 | 0 |
| `R2_verifier-np` | 13 | 113,079 | 934,030 | 12,276 | 11 | 7 | 0 | 0 |

---

## 3. Root-Cause Analysis: Why `/swarm` Was Slower Despite Using 13.6% Fewer Input Tokens & 18.6% Fewer Cache-Read Tokens

Inspecting the 6 worker transcripts revealed **5 exact friction points** where Run 1 wasted turns and wall time:

1. **`cli edit --stdin` schema is undocumented in `worker-brief` and `cli edit --help` (5 wasted turns in `R1_runtime-config`):**
   - `R1_runtime-config` ran `cli edit --help`, then probed with `--json '{"edits":[{"foo":1}]}'`, then guessed `"file"/"old"/"new"`, then `"path"/"old"/"new"`, then `"hunks":[{"old":...}]"` before giving up and falling back to built-in `edit_file` (13 calls).
   - `R1_win32-layout` also ran `cli edit --help`, found no schema, and abandoned `cli edit` for `edit_file` (16 calls).
2. **`cli slices` + 8.8 KB CLI cap caused duplicate reads (`R1_win32-layout`: 25 `read_file` vs 14 in R2):**
   - `cli slices` is capped at the same `8.8 KB` MCP budget (`budget::CAP`) and pays ~1.5 s PowerShell spawn overhead, whereas built-in `read_file` returns 46 KB/call in ~7 ms locally.
   - `R1_win32-layout` called `cli outline` on two 4k–5k line files (truncated) + `cli slices` (truncated), then had to re-read the same ranges with `read_file`.
3. **Silent modifier-order trap in `cli search`:**
   - `R1_runtime-config` ran `cli search --text "arboard" --glob-filter "!Cargo.lock"` — trailing `--glob-filter` after `--text` was silently ignored, forcing a retry with `--glob-filter` first.
4. **Cross-subcommand flag confusion (`cli outline ... --git "status --short"`):**
   - `R1_win32-layout` combined `--git` with `cli outline` (which only exists on `cli search`), wasting a call.
5. **Cold `target/` build in Run 1 `verifier`:**
   - `R1_verifier` waited on the first cold compile of `D:\05Code\aicliphelper-audit\target\` (~2 min extra wall time and 5 extra polling turns vs warm `R2_verifier-np`).

---

## 4. Concrete Roadmap to Make `agyfast-rs` Faster & Cheaper in `/swarm`

Ordered by impact per line of code:

1. **Fix `worker-brief` (`src/cli.rs`) — biggest immediate win (saves 4–6 turns & ~20–30 s per worker):**
   - **Remove `cli slices`** from `worker-brief` (built-in `read_file` is 5× larger and 200× faster).
   - **Remove `cli edit`** from `worker-brief` for parallel authors (built-in `edit_file` is ~7 ms local with zero PowerShell heredoc/JSON escaping). Tell workers: *use `cli search --definition` and `cli outline file#Name` for discovery, built-in `read_file`/`edit_file` for reading/editing, and `cli check --compact` for builds/tests*.
   - Still print the exact 1-line JSON schema `{"edits":[{"path":"...","hunks":[{"target":"...","replacement":"..."}]}]}` inside `cli edit --help` for anyone who explicitly calls `cli edit`.
2. **Raise default `--max-chars` for CLI mode (`src/cli.rs` / `src/core/budget.rs`):**
   - Keep `8_800` for MCP (remote tool cap), but raise CLI default to `20_000–24_000` (overridable via `--max-chars`). This stops `cli outline` and `cli search --definition` from truncating large 4k–7k line Rust files and forcing follow-up `read_file` rounds.
3. **Reject dangling trailing modifiers in `cli search` (`src/cli.rs`):**
   - If `query` map is non-empty after the argument loop (e.g., `--text "foo" --glob-filter "*.rs"`), return a clear error (`modifiers must come before --text/--regex/--glob`) instead of silently dropping them.
4. **Shorten the CLI invocation in `worker-brief` (`src/cli.rs`):**
   - When `agyfast-rs` is on `PATH` (`~/.local/bin`), emit `agyfast-rs cli ...` instead of `& 'C:\Users\root\.gemini\config\plugins\agyfast-rs\bin\agyfast-rs.exe' cli ...` — saves ~65–80 output characters on every shell call and avoids PowerShell quoting mistakes.
5. **Keep `cli check --compact` for verifiers (already a clear win):**
   - `R1_verifier` compressed `cargo check`, `cargo clippy`, and 355-test `cargo test` output to single-line summaries with 0 errors.
