#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
resource_dir="$repo_root/swift/Resources"

cargo build \
    --manifest-path "$repo_root/Cargo.toml" \
    -p file-peeker-server \
    --release \
    --bin file-peeker-server

mkdir -p "$resource_dir"
cp "$repo_root/target/release/file-peeker-server" "$resource_dir/file-peeker-server"
