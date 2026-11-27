#!/usr/bin/env bash
set -euo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
export LC_ALL=C CARGO_NET_OFFLINE=true GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR
hard=$(cd -- "$(dirname -- "$0")" && pwd -P)
repo=$(git -C "$hard" rev-parse --show-toplevel)
die() { echo "$*" >&2; exit 1; }
[[ $# == 1 && -n $1 ]] || die "Usage: setup.sh <dest-dir>"

# Resolve symlinks and .. before creating anything in the requested location.
dest=$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$1")
case "$dest/" in "$repo/"*) die "Choose a fixture directory outside the source repository.";; esac
mkdir -p -- "$dest"
cd -- "$dest"
shopt -s dotglob nullglob
entries=(./*)
if (( ${#entries[@]} )); then
    if [[ -d .git && -f .git/hard-fixture ]] &&
       [[ $(cat .git/hard-fixture) == "$(git rev-parse HEAD)" ]] &&
       [[ $(git rev-parse HEAD) == "$(git rev-parse 'start^{commit}')" ]] &&
       [[ -z $(git status --porcelain --untracked-files=all) ]]; then
        echo "Fixture already at start: $dest"
        exit 0
    fi
    die "Destination contains other files or a changed fixture. Use a fresh directory."
fi

# base.tar.gz: .gitignore, Cargo.toml, Cargo.lock, queries, src and tests of this project at an earlier revision.
tar -xzf "$hard/base.tar.gz"
# Prime dependency and test artifacts from compilable source before the migration.
cargo test --offline --locked --no-run --target-dir "$dest/target" --quiet
git init -q --template= --object-format=sha1 -b main
git apply --whitespace=error "$hard/starter.patch"
git -c core.autocrlf=false add -f -- .cargo/config.toml .gitignore Cargo.toml Cargo.lock queries src tests
export GIT_AUTHOR_NAME=Benchmark GIT_COMMITTER_NAME=Benchmark
export GIT_AUTHOR_EMAIL=benchmark@localhost GIT_COMMITTER_EMAIL=benchmark@localhost
export GIT_AUTHOR_DATE=2000-01-01T00:00:00Z GIT_COMMITTER_DATE=2000-01-01T00:00:00Z
git -c core.hooksPath=/dev/null -c commit.gpgsign=false commit -qm 'Start selective batch replay benchmark'
git -c tag.gpgSign=false tag start
git rev-parse HEAD > .git/hard-fixture
echo "Fixture ready: $dest"
