## agyfast-rs (code navigation helpers, via Bash)

- Keep the built-in Read, Grep, Glob and Edit tools; agyfast-rs does not replace them.
- Before reading an unknown large file (>300 lines), run `agyfast-rs cli outline <files...>` (up to 40 files) to get symbols with line ranges, then Read only the ranges you need.
- For targeted edits, search with `agyfast-rs cli search --context 2 --text ...` on the hits you will edit and edit straight from those lines (reads are the largest cost); if they don't show enough, Read only the hit's `Lstart-end` definition range, not a 100-line window or the file head.
- Build/test/lint output (cargo, pytest, go test, jest/vitest, tsc, eslint) is compressed automatically: run the plain command, without `| tail` or `| grep`; the first line `[agyfast-rs PASS|FAIL] <cmd>` is the verdict, then errors and failures only.
- The SessionStart snapshot (git state, stacks, tree) is already in context; do not repeat `ls` or `git status` for it.
