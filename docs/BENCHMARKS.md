# Benchmarks

How `agyfast-rs` is measured, and the current results. Every number here comes from a headless
Antigravity CLI run that we scored and counted ourselves; nothing is estimated.

## Method

**One task, one fresh fixture, one prompt.** A setup script creates a new git repository in a
known state. The agent gets a single prompt that names no tools and no plugin. The only thing that
changes between the columns of a table is which plugin is enabled (and, where stated, solo or
swarm).

```sh
agy -p "$(cat prompt.txt)" --model gemini-4-hydrogen-low \
    --settings '{"permissions":{"allow":["mcp(*)","write_file(<fixture>)","command(cargo)", ...]}}' \
    --output-format stream-json --print-timeout 600s
```

**Quality first.** A scorer runs the build, the linter, the test suite and hidden acceptance tests
against the result and prints a score. Costs are only compared between runs with the same score.
In every table below all columns have the same score.

**What is counted.**

| Measure | Source |
| :--- | :--- |
| Wall time | Start to exit of the `agy` process |
| LLM turns | Model responses in the session (for a swarm: coordinator plus every worker) |
| Tool calls | Tool invocations in the stream (swarm: coordinator plus workers) |
| Tokens | `input_tokens`, `cache_read_tokens`, `output_tokens` per turn, from the stream for the main session and from each worker's transcript for swarms. `output_tokens` already includes thinking |

**Cost-weighted tokens** = `output × 5 + input + cache_read × 0.1`. This is the same 1 : 0.1 : 5
weighting other Antigravity tooling uses to approximate quota.

**Noise.** Repeating one identical configuration changed cost by 11% and the turn count by 3; a
second repeat (0.1.4, below) changed cost by 9% and tool calls by 14 at the same turn count. Each
cell below is a single run, so treat differences under roughly 15% as "not shown". Differences in
turns and wall time have been more stable than differences in cost and tool calls.

## Tasks

| Task | What the agent must do | Checks |
| :--- | :--- | ---: |
| **Hard** | Finish a half-done internal migration that leaves the build broken, add a selector feature across four tools, their schemas and the CLI, and write two named regression tests. One existing test fails for a reason the prompt does not mention. | 15 |
| **Long** | Repair three existing report importers (unified diff, LCOV, TAP). Each has four defects that are not described; they must be found from saved input files, recorded expectations and existing tests. Then publish one operation through the CLI and the MCP schema. The three importers do not depend on each other. | 36 |

Both tasks are changes to this project's own Rust code. One hidden check of the hard task (a
byte-budget edge case in the starting code) was failed by every run in every configuration, so its
effective maximum is 14.

## Results

### Solo agent, hard task, Gemini 4 Hydrogen Low

| | No plugin | agy-fast 0.3.1 | agyfast-rs |
| :--- | ---: | ---: | ---: |
| Score | 14/15 | 14/15 | 14/15 |
| Wall time | 184 s | 151 s | 146 s |
| LLM turns | 25 | 20 | 16 |
| Tool calls | 51 | 28 | 29 |
| `read_file` calls | 16 | 15 | 10 |
| Uncached input tokens | 101,832 | 80,524 | 67,492 |
| Cached input tokens | 824,845 | 703,444 | 549,072 |
| Output tokens | 17,772 | 17,444 | 16,854 |
| First-turn input (fixed prefix) | 12,045 | 16,808 | 14,751 |
| **Cost-weighted** | **273,176** | **238,088** | **206,669** |
| vs no plugin | — | −13% | −24% |

The saving is in input: fewer turns re-reading the conversation. Output is the same in all three,
which is what you would expect when the same code gets written.

### Repeat of the hard task on agyfast-rs 0.1.4

Same task, model and settings one day later, after the 0.1.4 fixes: one more run without a plugin
and two identical runs with the 0.1.4 build.

| | No plugin (repeat) | agyfast-rs 0.1.4, run 1 | agyfast-rs 0.1.4, run 2 |
| :--- | ---: | ---: | ---: |
| Score | 14/15 | 14/15 | 14/15 |
| Wall time | 179 s | 141 s | 141 s |
| LLM turns | 26 | 19 | 19 |
| Tool calls | 51 | 38 | 24 |
| `read_file` calls | 16 | 11 | 12 |
| Uncached input tokens | 88,633 | 76,669 | 71,853 |
| Cached input tokens | 874,912 | 694,859 | 627,924 |
| Output tokens | 17,681 | 18,719 | 16,941 |
| First-turn input (fixed prefix) | 12,216 | 14,938 | 14,945 |
| **Cost-weighted** | **264,529** | **239,750** | **219,350** |
| vs this no-plugin run | — | −9% | −17% |

- **The plain host did not change:** the two no-plugin runs differ by 3% in cost and one turn.
- **The two 0.1.4 runs differ by 9% in cost and 14 tool calls** with the same turn count, score and
  wall time. Run 1 made all 21 edits with the built-in `edit_file`; run 2 used the batched
  `apply_edits` five times and `edit_file` twice. Nothing in the plugin differed between them.
- **Across all five Low runs:** without a plugin 25–26 turns, 179–184 s and 265–273k; with
  `agyfast-rs` 16–19 turns, 141–146 s and 207–240k. The cost saving ranges from 9% to 24% depending
  on which pair of runs is compared; the turn and time savings were the same in every pair.

- **Shorter tool names.** 0.1.4 as released publishes the tools as `search`, `outline`, `edit` and
  `check` on a server named `code` (earlier `batch_search`, `quick_outline`, `apply_edits`,
  `run_checks` on `rs`). Two further runs with these names gave 219,458 (18 turns, 149 s) and
  197,005 (15 turns, 131 s), both 14/15: inside the range above, so no effect of the names is
  shown. In half of all 0.1.4 runs, with either naming, the model made every edit with the
  built-in `edit_file`; those runs cost about 20k more.

### Solo agent, hard task, Gemini 4 Hydrogen Medium

| | No plugin | agyfast-rs (earlier build) |
| :--- | ---: | ---: |
| Score | 14/15 | 14/15 |
| Wall time | 210 s | 193 s |
| LLM turns | 17 | 12 |
| Tool calls | 44 | 23 |
| **Cost-weighted** | **364,354** | **311,334** |
| vs no plugin | — | −15% |

Low reached the same score as Medium for 25–30% less cost on this task.

### Solo and swarm, long task, Gemini 4 Hydrogen Low

| | Solo, agyfast-rs | 3-worker swarm, agyfast-rs | 3-worker swarm, no plugin |
| :--- | ---: | ---: | ---: |
| Score | 36/36 | 36/36 | 36/36 |
| Wall time | 259 s | 212 s | 280 s |
| LLM turns, all sessions | 27 | 48 | 55 |
| Tool calls, all sessions | 80 | 116 | 140 |
| **Cost-weighted** | **417,220** | **568,987** | **626,135** |
| Time vs solo | — | −18% | +8% |
| Cost vs solo | — | +36% | +50% |

In the swarm with `agyfast-rs`, workers were active for 115 s of 212 s. They ran all 13 of their build
and test commands through `agyfast-rs cli check`; without the plugin they ran `cargo` directly.

### Swarm, hard task, Gemini 4 Hydrogen Medium

| | Solo, agyfast-rs | 3-worker swarm, agyfast-rs | 3-worker swarm, agy-fast | 3-worker swarm, no plugin |
| :--- | ---: | ---: | ---: | ---: |
| Score | 14/15 | 14/15 | 14/15 | 14/15 |
| Wall time | 193 s | 217 s | 215 s | 216 s |
| **Cost-weighted** | **311,334** | **623,416** | **659,420** | **700,896** |

All parts of this task depend on one shared contract. Workers were active for about 80 s of each
run; the rest was the coordinator reading before the split and integrating afterwards.

## What the results say

1. **Solo:** `agyfast-rs` reduced turns by 5–9 and cost by 9–24% against the plain host at equal
   score, on two effort levels and two builds.
2. **Against agy-fast:** 4 fewer turns and 13% less cost in the one head-to-head run; that gap is
   about the size of the noise. The clearer differences are structural: less fixed context and
   build output that always goes through the compressor.
3. **Swarm, independent parts:** with `agyfast-rs` the swarm was faster than one agent (−18% time) for
   more tokens (+36%). Without it the swarm was slower than one agent.
4. **Swarm, coupled parts:** about twice the cost of one agent and no faster, with any plugin.

## What did not work

Published because it is part of the measurement:

- **Swarm guidance in rules.** Four different wordings of coordinator and worker instructions
  changed swarm cost by +19%, −7%, +5.5% and +14%: no effect distinguishable from noise. One of
  them made the coordinator 23% cheaper and the workers 43% more expensive.
- **Claude Code.** With the optional hooks and rules, Claude Sonnet solved the hard task with the
  same score, turns and tools as without them; the hook never triggered.
- **A greenfield "write three parsers from a spec" task** was solved by one agent in 157 s and
  13 turns. Volume of new code from a clear specification is cheap for these models; tasks get
  long through reading and fix-and-retest loops.

## Limits

- One run per configuration, except the hard task on Low: two runs without a plugin and three
  with `agyfast-rs` (one on an earlier build, two on 0.1.4).
- Two tasks, both on this repository's own Rust code, both under five minutes.
- One model family (Gemini 4 Hydrogen); Antigravity CLI 1.5.17–1.5.18.
- Quality is whatever the scorers check; diffs were not otherwise reviewed.

## Measuring your own sessions

Antigravity keeps a transcript per conversation at
`~/.gemini/antigravity-cli/brain/<conversation-id>/.system_generated/logs/transcript_full.jsonl`.
Each `PLANNER_RESPONSE` line is one LLM turn with its token fields and tool calls, so the cost
formula above can be applied to any session, including each `/swarm` worker.
