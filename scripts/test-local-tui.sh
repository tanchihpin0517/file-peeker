#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

cargo build \
    --manifest-path "$repo_root/Cargo.toml" \
    -p file-peeker-server \
    --bin file-peeker-server

cargo run \
    --manifest-path "$repo_root/Cargo.toml" \
    -p file-peeker-tui \
    --bin file-peeker \
    -- \
    --smoke \
    "$repo_root"
