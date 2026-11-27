#!/usr/bin/env bash
set -euo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_NET_OFFLINE=true GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR
here=$(cd -- "$(dirname -- "$0")" && pwd -P)
repo=$(git -C "$here" rev-parse --show-toplevel)
[[ $# == 1 && -n $1 ]] || { echo 'usage: setup.sh <empty destination>' >&2; exit 2; }
dest=$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$1")
case "$dest/" in "$repo/"*) echo 'choose a destination outside the repository' >&2; exit 2;; esac
mkdir -p -- "$dest"
cd -- "$dest"
shopt -s dotglob nullglob
entries=(*)
(( ${#entries[@]} == 0 )) || { echo 'destination must be empty' >&2; exit 2; }
# base.tar.gz: .gitignore, Cargo.toml, Cargo.lock, queries, src and tests of this project at an earlier revision.
tar -xzf "$here/base.tar.gz"
git init -q --template= -b main
git apply --whitespace=error "$here/starter.patch"
git add -- .gitignore Cargo.toml Cargo.lock queries src tests fixtures
export GIT_AUTHOR_NAME=Benchmark GIT_COMMITTER_NAME=Benchmark
export GIT_AUTHOR_EMAIL=benchmark@localhost GIT_COMMITTER_EMAIL=benchmark@localhost
export GIT_AUTHOR_DATE=2000-01-01T00:00:00Z GIT_COMMITTER_DATE=2000-01-01T00:00:00Z
git -c core.hooksPath=/dev/null -c commit.gpgsign=false commit -qm 'Start report archive repair benchmark'
git -c tag.gpgSign=false tag start
git rev-parse HEAD > .git/long-repair-fixture
cargo build --quiet --offline --locked --all-targets
echo "Fixture ready: $dest"
