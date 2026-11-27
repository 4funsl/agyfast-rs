# Selective batch replay

A client has saved a batch and wants to rerun a subset using the original item numbers. One agent must complete the shared Rust API migration, implement the selection language across four request shapes, and make command failures produce a selector the client can reuse. A separate public client regression exercises the content search renderer.

The design targets **25–40 turns** for one headless agent at medium effort and about **5–8 minutes** on a warm workspace. These are planning estimates. The agent run, elapsed time and any plugin effect remain unmeasured. In the earlier solo experiment, the agent applied 18 related hunks in one batch and passed its first test run. Here, compilation, order, side effects and rendering have independently observable failure cases. [4test](../../docs/BENCHMARKS.md) · [7test, Addendum 2](../../docs/BENCHMARKS.md)

## Run one trial

Run setup from the repository root. It uses Git, Bash, Python 3.9+, the Rust toolchain with Clippy, and the locked dependency set cached locally.

```sh
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_NET_OFFLINE=true
unset CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR
hard="$PWD/bench/hard"
dest=$(mktemp -d)
"$hard/setup.sh" "$dest"
```

Give one headless agent the exact contents of `$hard/prompt.txt` and set its workspace to `$dest`. Start the agent timer after setup. End it when the agent replies. Score the saved workspace from outside it.

```sh
"$hard/verify.sh" "$dest"
```

Use a fresh destination per trial. The setup script also accepts a nonexistent directory. Repeating setup on a clean fixture at `start` succeeds. A changed fixture or unrelated nonempty directory is preserved and setup exits with an error.

## Where the extra cycles come from

1. **Compile the migration.** Four callers still supply scalar byte limits to the two fair-join functions. Their new signatures require a shared batch plan. Search and checks use deferred per-section rendering. Outline and mixed reads pass materialized sections.
2. **Make selection observable.** Selectors are ordered sets. A leading exclusion seeds the set with all items, and re-adding an item moves it to the end. Rendered sections have dense indices, while headers, check names and validation errors refer to original slots. Mixed reads assemble their slots from three input arrays. The original batch limits still apply.
3. **Preserve execution semantics and output budgets.** Skipped commands can have side effects or invalid working directories. All selected commands need validation before any run. A selected check report must reserve space for a complete, sorted retry expression after compact or full output. A request's advertised schema and its actual CLI and protocol behavior are scored separately from the parser.
4. **Diagnose the independent regression.** A public case has two search hits. The first file's context cannot fit, but the second file's hit can. The renderer in `src/tools/search.rs` exits its round-robin scan early. This case runs without a selector, so implementing the new feature alone leaves it red.

Partial-reference runs confirmed the distinction. With the consumer changes applied, the starter selector stub and the original renderer produced a successful all-target build and three failing public contract tests. Completing the parser changed that to two passes and one failure. Repairing the renderer passed all three. An agent can combine fixes in a single edit, so the benchmark does not require a particular number of commands or turns.

## Fixture and scoring

Setup archives only the manifests, ignore file, queries, Rust source and tests from `base.tar.gz`, a snapshot of this project at an earlier revision (when the binary was still named `agy-rs`). It warms the destination's own target before applying `starter.patch`. The starter adds the partial migration, offline Cargo config and three visible client cases. It also removes timing-dependent assertions and deliberate timeout cases from the pinned check tests. The new acceptance cases use fixed input, file contents and command exit statuses.

The fixture has a single root commit, tagged `start`. Its measured SHA-1 is `de4e79aef7f5fa3a0be2a15b3e3fefac3f18e784`. A solved public `cargo test --offline --locked --all-targets` ran 67 tests on the development host. It took **18.69 s cold** with an empty Cargo target and **2.57 s warm** on the following run. A cold run still uses the local registry cache.

The verifier awards 15 equally weighted points.

- Four for the all-target build, Clippy with warnings denied, the public test suite and execution of both requested regression tests.
- Ten black-box cases cover selector ordering, rejected input, original cardinality limits, mixed reads, command isolation, replayable footers, UTF-8 byte budgets, schema and CLI behavior, rejected writes and search fairness.
- One compiled Rust client checks the supplied plan and fair-join APIs directly.

The hidden Python and Rust sources are copied into temporary directories in the fixture and removed on exit. The runtime is rebuilt before black-box checks so a stale warm executable cannot earn points. The verifier exits zero for a scored run, including a failing one. Read the final `score=` line. Use `HARD_VERBOSE=1` to include failure diagnostics on stderr. Diff statistics use a temporary Git index so new agent files count and the agent's staging area is preserved. A clean tree has the explicit summary `0 files changed`.

## Reference replay

Use a separate empty destination for the reference.

```sh
ref=$(mktemp -d)
"$hard/setup.sh" "$ref"
"$hard/verify.sh" "$ref"                          # score=0/15
git -C "$ref" apply "$hard/solution.patch"
"$hard/verify.sh" "$ref"                          # score=15/15
```

The reference patch produces this `git diff --stat start` output.

```text
 src/cli.rs           |  2 ++
 src/core/batch.rs    | 69 +++++++++++++++++++++++++++++++++++++++++++++++-----
 src/core/budget.rs   |  5 +++-
 src/mcp/schema.rs    |  4 +++
 src/tools/check.rs   | 48 ++++++++++++++++++++++++++++++------
 src/tools/edit.rs    |  3 +++
 src/tools/outline.rs | 11 ++++++---
 src/tools/search.rs  | 17 +++++++------
 src/tools/slices.rs  |  8 +++---
 9 files changed, 139 insertions(+), 28 deletions(-)
```
