#!/bin/sh
# Installs a published fpk release into ~/.file-peeker/bin/fpk, as herdr installs
# its own: the asset for this host's system and architecture, from the release
# of exactly the version the client asked for, checked against SHA256SUMS.
#
# Run on the host over SSH: sh -s -- <version> <repository URL>
set -eu

version=$1
repository=$2

case "$(uname -s)" in
    Linux) os=linux ;;
    Darwin) os=macos ;;
    *)
        echo "fpk: no release is built for $(uname -s)" >&2
        exit 2
        ;;
esac
case "$(uname -m)" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *)
        echo "fpk: no release is built for $(uname -m)" >&2
        exit 2
        ;;
esac

asset="fpk-$os-$arch"
base="$repository/releases/download/v$version"
root="$HOME/.file-peeker"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then
        wget -q "$1" -O "$2"
    else
        echo "fpk: this host has neither curl nor wget to download a release with" >&2
        exit 2
    fi
}

checksum() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{ print $1 }'
    else
        shasum -a 256 "$1" | awk '{ print $1 }'
    fi
}

echo "downloading $asset from release v$version"
if ! fetch "$base/$asset" "$work/$asset"; then
    echo "fpk: release v$version has no $asset at $base" >&2
    exit 3
fi
fetch "$base/SHA256SUMS" "$work/SHA256SUMS"
expected=$(awk -v asset="$asset" '$2 == asset || $2 == "*" asset { print $1 }' "$work/SHA256SUMS")
actual=$(checksum "$work/$asset")
if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
    echo "fpk: $asset does not match its checksum in release v$version" >&2
    exit 4
fi

mkdir -p "$root/bin"
chmod 700 "$root"
chmod 755 "$work/$asset"
cp "$work/$asset" "$root/bin/fpk.partial"
mv -f "$root/bin/fpk.partial" "$root/bin/fpk"
rm -f "$root/bin/fpk.source"
echo "installed fpk v$version at $root/bin/fpk"
