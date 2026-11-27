#!/usr/bin/env bash
# Build agyfast-rs (or use the prebuilt binary of a release archive) and install the Antigravity plugin and the CLI. Safe to re-run.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
plugin_dir="$HOME/.gemini/config/plugins/agyfast-rs"
cli_dir="$HOME/.local/bin"

cd "$root"
if [ -f "$root/agyfast-rs" ] && [ -x "$root/agyfast-rs" ]; then
  bin="$root/agyfast-rs" # release archive: the prebuilt binary sits next to this script
else
  cargo build --release
  bin="$root/target/release/agyfast-rs"
fi

mkdir -p "$plugin_dir/bin" "$cli_dir"
for f in plugin.json mcp_config.json hooks.json; do
  install -m 0644 "plugin/$f" "$plugin_dir/$f"
done
mkdir -p "$plugin_dir/rules"
install -m 0644 plugin/rules/AGENTS.md "$plugin_dir/rules/AGENTS.md"
# Write to a temp name then rename, so a running MCP server is never overwritten in place.
install -m 0755 "$bin" "$plugin_dir/bin/agyfast-rs.new"
mv -f "$plugin_dir/bin/agyfast-rs.new" "$plugin_dir/bin/agyfast-rs"
install -m 0755 "$bin" "$cli_dir/agyfast-rs.new"
mv -f "$cli_dir/agyfast-rs.new" "$cli_dir/agyfast-rs"

echo "Installed plugin: $plugin_dir"
echo "Installed CLI:    $cli_dir/agyfast-rs (make sure $cli_dir is on PATH)"
echo
echo "Claude Code (not edited automatically):"
echo "  1. Merge the hooks (SessionStart, PreToolUse) from $root/claude/settings.json into ~/.claude/settings.json"
echo "  2. Append $root/claude/CLAUDE.md to ~/.claude/CLAUDE.md or your project's CLAUDE.md"
echo "  3. Allow the output filter: \"Bash($cli_dir/agyfast-rs cli filter:*)\" (see $root/claude/README.md)"
