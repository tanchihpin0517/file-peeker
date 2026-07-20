#!/bin/sh
set -eu

repo_root=$(cd "$(dirname "$0")/.." && pwd)

exec cargo run \
    --manifest-path "$repo_root/Cargo.toml" \
    -p file-peeker-tui \
    --bin file-peeker \
    -- \
    "$@"
