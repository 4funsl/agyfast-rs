#!/usr/bin/env bash
# Build agy-rs and install the Antigravity plugin and the CLI. Safe to re-run.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
plugin_dir="$HOME/.gemini/config/plugins/agy-rs"
cli_dir="$HOME/.local/bin"

cd "$root"
cargo build --release
bin="$root/target/release/agy-rs"

mkdir -p "$plugin_dir/bin" "$cli_dir"
for f in plugin.json mcp_config.json hooks.json; do
  install -m 0644 "plugin/$f" "$plugin_dir/$f"
done
mkdir -p "$plugin_dir/rules"
install -m 0644 plugin/rules/AGENTS.md "$plugin_dir/rules/AGENTS.md"
# Write to a temp name then rename, so a running MCP server is never overwritten in place.
install -m 0755 "$bin" "$plugin_dir/bin/agy-rs.new" && mv -f "$plugin_dir/bin/agy-rs.new" "$plugin_dir/bin/agy-rs"
install -m 0755 "$bin" "$cli_dir/agy-rs.new" && mv -f "$cli_dir/agy-rs.new" "$cli_dir/agy-rs"

echo "Installed plugin: $plugin_dir"
echo "Installed CLI:    $cli_dir/agy-rs (make sure $cli_dir is on PATH)"
echo
echo "Claude Code (not edited automatically):"
echo "  1. Merge the hooks (SessionStart, PreToolUse) from $root/claude/settings.json into ~/.claude/settings.json"
echo "  2. Append $root/claude/CLAUDE.md to ~/.claude/CLAUDE.md or your project's CLAUDE.md"
echo "  3. Allow the output filter: \"Bash($cli_dir/agy-rs cli filter:*)\" (see $root/claude/README.md)"
