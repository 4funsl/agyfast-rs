# Report importer repair benchmark

Repair three working importers against saved customer reports, then publish one existing operation. The trial prompt gives the goals and acceptance commands. The fixture supplies the evidence: Rust implementations, existing unit tests, raw reports, captured client-visible values or error locations, and a CLI/MCP client transcript. Give the agent **only [prompt.txt](prompt.txt)** and the fixture checkout. This README, the scorer, and the patches belong to the operator.

## Why this revision

Hydrogen Low solved the previous three-parser task solo in **157 seconds, 13 turns, 36/36**. It could turn each part of the specification into a complete replacement file in one turn. The earlier 35–65 turn estimate was too high. This revision measures diagnosis and repair across file boundaries. Each adapter already handles ordinary input. A fix can satisfy one archived incident while leaving the other three red. A passing legacy archive and existing unit tests in each area exercise behavior that must survive the repairs. [Measured calibration](../../docs/BENCHMARKS.md)

| Workstream | Owned fixture paths | Four independent incident families |
|---|---|---|
| A · patch viewer | `src/tools/report/diff/`, `tests/report_diff.rs`, `fixtures/reports/diff/` | Mail series and physical locations; path transfers; suppressed blank context; hunk boundary ordering. |
| B · coverage dashboard | `src/tools/report/lcov/`, `tests/report_lcov.rs`, `fixtures/reports/lcov/` | Platform path identities; optional function extents; grouped function aliases; retried producer snapshots. |
| C · test inspector | `src/tools/report/tap/`, `tests/report_tap.rs`, `fixtures/reports/tap/` | Multiplexed lanes; escaped display names; diagnostic indentation; buffered subtests. |
| Integration | `src/cli.rs`, `src/mcp/schema.rs`, list expectations in `src/mcp/mod.rs` and `tests/mcp_stdio.rs` | Publish the inspector through the flags, help and schema used by the saved client. |

All three use the fixture's prewired report entry point. Each worker can reproduce a failure, follow the parser into its format-specific helpers, edit its own Rust files, and run its unit and archive tests immediately. The corpus and recorded expectations stay fixed. The integration owner needs only the client transcript. The three partial patches demonstrate that publishing the operation and repairing the other adapters are unnecessary for a workstream to earn its nine points.

**Calibration target:** a fully correct solo run in **360–480 s**, and a fully correct three-worker run in **under 600 s**, both subject to the same **600 s wall-clock cutoff**. These are targets for the next timed trials. The 157-second measurement describes the retired fixture. Neither elapsed time nor a minimum turn count can be inferred from passing the verifier. The three parser paths can be repaired concurrently, and the agent can batch fixes if it identifies them together.

## Setup and timed trials

Requires Bash, Git, Python 3.9+, Rust with Clippy, and Cargo.lock's dependency versions available locally. Setup unpacks `base.tar.gz` (a snapshot of this project at an earlier revision, when the binary was still named `agy-rs`), applies the starter, creates a fresh fixture repository with one `start` tag, and compiles all targets offline before the timer starts. A fresh empty destination outside this repository is required. The fixture excludes operator materials and agent history.

```sh
export PATH="$HOME/.cargo/bin:$PATH"
long="$PWD/bench/long"
trial=$(mktemp -d)
"$long/setup.sh" "$trial"
```

Run the existing headless command from that workspace with the usual prompt delivery and per-arm settings. Wrap the **whole run**, including coordinator dispatch, wait, and integration, in the supplied deadline wrapper. For a shell array containing the experiment driver's foreground command and arguments:

```sh
(cd "$trial" && "$long/limit.py" "${trial_command[@]}")
"$long/verify.sh" "$trial"
```

The wrapper passes through stdio and normal exit codes. At 600 seconds it kills the local trial process group and exits 124. Use the same deadline in the host if your runner detaches workers outside that group. Score the saved workspace even on timeout, and record the timeout alongside its score. Setup and verification are outside the trial clock. For a paired comparison, hold model, effort, host version, warm-build conditions, and worker count fixed. Record the longest worker interval and time spent before dispatch and after return. The first solo and swarm trials establish whether this revision meets the timing target.

## Reproducing and scoring

Inside a fixture, a single incident can be replayed without running unrelated tests:

```sh
cargo test --offline --locked --test report_diff diff_mailbox -- --nocapture
cargo test --offline --locked --test report_lcov
cargo test --offline --locked --test report_tap
```

Each format directory in `fixtures/reports` pairs native inputs with partial JSON captures. The matching public tests invoke the actual executable. Successful captures assert the fields the client used; failed captures identify the physical input line. The three legacy cases pass at start. Each hidden behavioral case changes the input and checks one client observation. Its last field in [the hidden case table](hidden/reports.py) identifies the public evidence file stem for that expectation. Publication requirements come from `fixtures/reports/clients.md`, authored-test names from the archive README, and the build commands from the prompt.

The scorer prints 36 named decisions, five group subtotals, and a total. It copies tracked and nonignored untracked source into an external scratch workspace, copies hidden tests there, and runs Cargo against that copy. It gates all black-box points on a successful all-target build and uses the executable reported by that build. Six hygiene points cover build, Clippy, the full public suite, and one pair of executed, passing authored regressions per workstream. The starter builds and passes Clippy. Its public incident failures deliberately cost the full-suite point until all three repairs land.

```sh
# Optional dependency cache outside both repositories, reusable across candidates.
export LONG_VERIFY_TARGET="$(mktemp -d)"
LONG_VERBOSE=1 "$long/verify.sh" "$trial"

# For a reference replay, choose exactly one patch and a fresh destination.
ref=$(mktemp -d)
"$long/setup.sh" "$ref"
git -C "$ref" apply "$long/solution.patch"
"$long/verify.sh" "$ref"
```

Verification has a 55-second command budget, leaving time for cleanup and score output. Unfinished checks fail. A scored submission exits zero regardless of score; read the final score line.

## Measured patch matrix

| Candidate | Diff /9 | LCOV /9 | TAP /9 | Integration /3 | Hygiene /6 | Total /36 |
|---|---:|---:|---:|---:|---:|---:|
| Untouched | 0 | 0 | 0 | 0 | 2 | **2** |
| [A alone](solution-a.patch) | 9 | 0 | 0 | 0 | 3 | **12** |
| [B alone](solution-b.patch) | 0 | 9 | 0 | 0 | 3 | **12** |
| [C alone](solution-c.patch) | 0 | 0 | 9 | 0 | 3 | **12** |
| [Full solution](solution.patch) | 9 | 9 | 9 | 3 | 6 | **36** |

The five scoring runs took **9.01–9.70 seconds** each with a warm external Cargo target. Repeating verification on a partially staged full solution reproduced the score and preserved its working diff, status, and index checksum. After introducing a compilation error in that same workspace, the warm-cache score fell to **0/36**.

Reference size against the fixture start, including the six authored tests:

| Change | Existing Rust files edited | Lines added | Lines removed |
|---|---:|---:|---:|
| A · diff | 3 | 83 | 10 |
| B · LCOV | 3 | 141 | 14 |
| C · TAP | 3 | 110 | 4 |
| Integration | 4 | 16 | 3 |
| Full | 13 | 350 | 31 |
