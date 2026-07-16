#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

cargo build \
    --manifest-path "$repo_root/Cargo.toml" \
    -p file-peeker-server \
    --bin file-peeker-server

FILE_PEEKER_TEST_SERVER="$repo_root/target/debug/file-peeker-server" \
    cargo test \
    --manifest-path "$repo_root/Cargo.toml" \
    -p file-peeker-client \
    --test local_server \
    -- --ignored --nocapture
