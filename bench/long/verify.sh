#!/usr/bin/env bash
set -euo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_NET_OFFLINE=true CARGO_TERM_COLOR=never
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_OPTIONAL_LOCKS=0
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR
exec python3 -IB "$(dirname -- "$0")/score.py" "$@"
