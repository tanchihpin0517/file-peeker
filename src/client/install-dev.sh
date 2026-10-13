#!/bin/sh
# Builds the dev source a client uploaded to ~/.file-peeker-dev/src, and
# installs it as ~/.file-peeker-dev/bin/fpk with the source's hash recorded
# beside it, so a later probe can tell whether the host has caught up with the
# client's source. A dev install lives apart from a release one in everything,
# so the two never share a binary or a server.
#
# Debug, because it builds far faster and a dev host is for trying things. The
# target directory is kept between installs, so a rebuild is incremental.
#
# Run on the host over SSH: sh -s -- <source hash>
set -eu

hash=$1
root="$HOME/.file-peeker-dev"

# A non-interactive SSH session reads no shell profile, so rustup's own PATH
# setup is the usual way to find cargo.
if [ -f "$HOME/.cargo/env" ]; then
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
fi
if ! command -v cargo >/dev/null 2>&1; then
    echo "fpk: this host has no Rust toolchain; a dev build installs by building its source here" >&2
    exit 2
fi

cargo install --debug --locked --force \
    --path "$root/src" \
    --root "$root/cargo" \
    --target-dir "$root/build" \
    --bin fpk

mkdir -p "$root/bin"
chmod 700 "$root"
cp "$root/cargo/bin/fpk" "$root/bin/fpk.partial"
mv -f "$root/bin/fpk.partial" "$root/bin/fpk"
printf '%s\n' "$hash" >"$root/bin/fpk.source"
echo "installed dev fpk, source $(printf '%s' "$hash" | cut -c1-8), at $root/bin/fpk"
