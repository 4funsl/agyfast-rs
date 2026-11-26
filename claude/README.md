# agy-rs for Claude Code

1. Put `agy-rs` on PATH (`install.sh` copies it to `~/.local/bin`).
2. Merge the `hooks` from `settings.json` here into `~/.claude/settings.json` (all projects) or `<project>/.claude/settings.json` (one project). `SessionStart` adds the workspace snapshot; `PreToolUse` on `Bash` rewrites build/test/lint commands, e.g. `cargo test` becomes `cargo test 2>&1 | /home/me/.local/bin/agy-rs cli filter 'cargo test'`.
3. Add `CLAUDE.md` here to `~/.claude/CLAUDE.md` or the project's `CLAUDE.md`.
4. Allow the filter once, with the path the hook prints (check it with `echo '{"tool_name":"Bash","tool_input":{"command":"cargo test"}}' | agy-rs hook pre-bash`):
   `"permissions": { "allow": ["Bash(/home/me/.local/bin/agy-rs cli filter:*)"] }`
   The path is written unquoted when it has only `[A-Za-z0-9/._+-]`; a path with other characters is single-quoted, and the rule must then start with the same quoted text.

The hook only rewrites; it never returns `permissionDecision`. Claude Code checks permissions against the rewritten command, and the original command stays at the head of the pipeline, so your existing rules (`Bash(cargo test:*)`) still decide whether it runs. The filter only reads stdin and runs nothing, so allowing it grants nothing new. Commands with pipes, other redirections, `;`, subshells, `run_in_background`, `cargo run`, `--nocapture` or explicit output formats are left untouched.
