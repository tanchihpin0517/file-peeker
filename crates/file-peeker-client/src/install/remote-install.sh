#!/bin/sh

set -eu

version=$1
cargo_hint=$2
protocol_version=$3
source=$4
source_root=$5
policy=$6
install_root="$HOME/.file-peeker/servers/$version"
installed_bin="$install_root/bin/file-peeker-server"

verify_server() {
    test -x "$installed_bin" || return 1
    actual=$("$installed_bin" version --format json 2>/dev/null) || return 1
    expected=$(printf '{"server_version":"%s","protocol_versions":[%s]}' "$version" "$protocol_version")
    test "$actual" = "$expected"
}

case "$policy" in
    reuse|overwrite) ;;
    *)
        printf '%s\n' "unknown installation policy: $policy" >&2
        exit 2
        ;;
esac

if test "$policy" = reuse && verify_server; then
    printf '%s\n' 'FILE_PEEKER_INSTALL_OUTCOME=already_installed'
    "$installed_bin" version --format json
    exit 0
fi

if test "$source" = check; then
    printf '%s\n' 'FILE_PEEKER_INSTALL_REQUIRED'
    exit 0
fi

if test -n "$cargo_hint"; then
    cargo_bin=$cargo_hint
else
    cargo_bin=$(command -v cargo) || {
        printf '%s\n' 'cargo was not found on the remote server' >&2
        exit 127
    }
fi

case "$source" in
    crates_io)
        "$cargo_bin" install \
            --locked \
            --force \
            --root "$install_root" \
            --version "$version" \
            --bin file-peeker-server \
            file-peeker-server
        ;;
    workspace)
        protocol_package="$source_root/file-peeker-protocol-$version"
        server_package="$source_root/file-peeker-server-$version"
        tar -xzf "$source_root/protocol.crate" -C "$source_root"
        tar -xzf "$source_root/server.crate" -C "$source_root"
        "$cargo_bin" install \
            --locked \
            --force \
            --root "$install_root" \
            --path "$server_package" \
            --bin file-peeker-server \
            --config "patch.crates-io.file-peeker-protocol.path='$protocol_package'"
        ;;
    *)
        printf '%s\n' "unknown installation source: $source" >&2
        exit 2
        ;;
esac

if ! verify_server; then
    printf '%s\n' 'installed server failed version verification' >&2
    exit 1
fi

printf '%s\n' 'FILE_PEEKER_INSTALL_OUTCOME=installed'
"$installed_bin" version --format json
