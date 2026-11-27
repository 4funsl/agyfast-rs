# agyfast-rs for Claude Code

1. Put `agyfast-rs` on PATH (`install.sh` copies it to `~/.local/bin`).
2. Merge the `hooks` from `settings.json` here into `~/.claude/settings.json` (all projects) or `<project>/.claude/settings.json` (one project). `SessionStart` adds the workspace snapshot. `PreToolUse` on `Bash` passes build, test, and lint output and exit status to `cli filter`. The filtered step returns the original exit code, including in `&&` chains.
3. Add `CLAUDE.md` here to `~/.claude/CLAUDE.md` or the project's `CLAUDE.md`.
4. Allow the filter once, with the path the hook prints (check it with `echo '{"tool_name":"Bash","tool_input":{"command":"cargo test"}}' | agyfast-rs hook pre-bash`):
   `"permissions": { "allow": ["Bash(/home/me/.local/bin/agyfast-rs cli filter:*)"] }`
   The path is written unquoted when it has only `[A-Za-z0-9/._+-]`; a path with other characters is single-quoted, and the rule must then start with the same quoted text.

The hook leaves `permissionDecision` unset. Claude Code checks permissions against the rewritten command, which includes the original command inside a shell group and a separate filter invocation. The filter reads stdin and reports the captured output and status. Commands with pipes, other redirections, `;`, subshells, `run_in_background`, `cargo run`, `--nocapture` or explicit output formats are left untouched.
