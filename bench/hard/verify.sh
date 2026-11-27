#!/usr/bin/env bash
set -euo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
export LC_ALL=C CARGO_NET_OFFLINE=true CARGO_TERM_COLOR=never
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR
hard=$(cd -- "$(dirname -- "$0")" && pwd -P)
[[ $# == 1 ]] || { echo "Usage: verify.sh <dest-dir>" >&2; exit 1; }
cd -- "$1"
dest=$(pwd -P)
[[ -d .git && -f .git/hard-fixture && $(git rev-parse --show-toplevel) == "$dest" &&
   $(git rev-parse 'start^{commit}') == "$(cat .git/hard-fixture)" ]] || {
    echo "Expected the repository created by setup.sh." >&2
    exit 1
}
tmp=$(mktemp -d)
hidden=
api=
cleanup() {
    if [[ -n $hidden ]]; then rm -rf -- "$hidden"; fi
    if [[ -n $api ]]; then rm -rf -- "$api"; fi
    rm -rf -- "$tmp"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM HUP

# Snapshot new, staged, unstaged and deleted candidate files without changing the
# real index. Do this before injecting acceptance cases or starting Cargo.
(
    if [[ -f .git/index ]]; then cp .git/index "$tmp/index"; fi
    export GIT_INDEX_FILE="$tmp/index"
    git ls-files --others --exclude-standard -z > "$tmp/untracked"
    if [[ -s $tmp/untracked ]]; then
        git --literal-pathspecs add -N --pathspec-from-file="$tmp/untracked" --pathspec-file-nul
    fi
    git diff --no-ext-diff --stat start > "$tmp/diffstat"
)
summary=$(tail -n 1 "$tmp/diffstat")
summary=${summary:-" 0 files changed"}
read -r files _ <<< "$summary"

passed=0 total=0
record() {
    echo "$1=$2"
    total=$((total + 1))
    if [[ $2 == pass ]]; then passed=$((passed + 1)); fi
}
check() {
    local name=$1
    shift
    if "$@" > "$tmp/$name.log" 2>&1; then
        record "$name" pass
    else
        record "$name" fail
        if [[ ${HARD_VERBOSE:-0} == 1 ]]; then tail -n 35 "$tmp/$name.log" >&2; fi
    fi
}

opts=(--locked --offline --all-targets --target-dir "$dest/target")
check build cargo build "${opts[@]}"
check clippy cargo clippy "${opts[@]}" -- -D warnings
check tests cargo test "${opts[@]}" -- --format pretty --color never
has_tests() {
    local name
    for name in replay_readd_order replay_failed_ranges; do
        grep -Eq "^test (.*::)?${name} \.\.\. ok$" "$tmp/tests.log" || return 1
    done
}
check authored_tests has_tests

# The baseline build is deliberately broken. A kept pre-migration executable
# must never earn credit after a failed build. Build just the runtime separately
# so a candidate with a broken test target still gets its behavioral score.
hidden=$(mktemp -d "$dest/.hard-acceptance.XXXXXXXX")
cp "$hard/hidden/replay.py" "$hidden/replay.py"
export HARD_BINARY="$dest/target/debug/agy-rs"
if cargo build --locked --offline --bin agy-rs --target-dir "$dest/target" > "$tmp/runtime.log" 2>&1; then
    runtime=1
else
    runtime=0
fi
for name in selector_order invalid_selections original_limits mixed_reads command_isolation \
            footer_roundtrip byte_budgets schema_cli writes search_fairness; do
    if [[ $runtime == 1 ]]; then
        check "$name" python3 -IB "$hidden/replay.py" "Replay.test_$name"
        if [[ $name == byte_budgets ]]; then # unscored detail: which part of the check failed
            for part in footer selected framing; do
                python3 -IB "$hidden/replay.py" "Replay.test_byte_budgets_$part" > /dev/null 2>&1 &&
                    echo "byte_budgets.$part=pass" || echo "byte_budgets.$part=fail"
            done
        fi
    else
        record "$name" fail
    fi
done
# Cargo discovers tests/<target>/main.rs. Use a new directory so a candidate's
# own integration tests cannot collide with the probe or be removed by cleanup.
typed_plan() {
    [[ -d tests && ! -L tests ]] || return 1
    api=$(mktemp -d "$dest/tests/hard_contract_XXXXXXXX") || return 1
    cp "$hard/hidden/typed_plan.rs" "$api/main.rs" || return 1
    cargo test --locked --offline --target-dir "$dest/target" --test "${api##*/}" -- \
        typed_plan_contract --exact --format pretty --color never > "$tmp/probe.log" 2>&1 &&
        grep -Fxq 'test typed_plan_contract ... ok' "$tmp/probe.log" && return 0
    cat "$tmp/probe.log"
    return 1
}
check typed_plan typed_plan
echo "files_changed=$files"
echo "$summary"
echo "score=$passed/$total"
