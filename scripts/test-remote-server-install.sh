#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/test-remote-server-install.sh SSH_DESTINATION

Packages the unpublished File Peeker server and protocol crates, transfers both
packages to an SSH target, and installs the server from the packaged source.

Environment:
  SSH_BIN       SSH executable (default: ssh)
  SSH_ARGS      Extra SSH arguments, shell-split by this script
  REMOTE_CARGO  Remote Cargo executable (default: cargo)
EOF
}

if [[ $# -ne 1 ]]; then
    usage >&2
    exit 2
fi

ssh_destination=$1
ssh_bin=${SSH_BIN:-ssh}
remote_cargo=${REMOTE_CARGO:-cargo}

ssh_args=()
if [[ -n ${SSH_ARGS:-} ]]; then
    # SSH_ARGS is intended for controlled test environments.
    read -r -a ssh_args <<<"$SSH_ARGS"
    run_ssh() {
        "$ssh_bin" "${ssh_args[@]}" "$@"
    }
else
    run_ssh() {
        "$ssh_bin" "$@"
    }
fi

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
version=$(sed -nE 's/^version = "([^"]+)"/\1/p' "$repo_root/Cargo.toml" | head -1)
if [[ -z $version ]]; then
    echo "Could not read workspace package version" >&2
    exit 1
fi

protocol_crate="$repo_root/target/package/file-peeker-protocol-$version.crate"
server_crate="$repo_root/target/package/file-peeker-server-$version.crate"
remote_root="/tmp/file-peeker-install-test-${USER:-user}-$$"

cleanup() {
    result=$?
    trap - EXIT INT TERM

    echo "Cleaning remote fixture: $ssh_destination:$remote_root"
    if run_ssh "$ssh_destination" "rm -rf '$remote_root'"; then
        echo "Remote fixture removed"
    else
        echo "WARNING: failed to remove remote fixture" >&2
        if [[ $result -eq 0 ]]; then
            result=1
        fi
    fi

    if [[ $result -eq 0 ]]; then
        echo "PASS remote server installation test"
    else
        echo "FAIL remote server installation test (exit $result)" >&2
    fi

    exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

echo "Packaging file-peeker-protocol $version"
cargo package \
    --manifest-path "$repo_root/crates/file-peeker-protocol/Cargo.toml" \
    --allow-dirty \
    --no-verify

echo "Packaging file-peeker-server $version"
cargo package \
    --manifest-path "$repo_root/crates/file-peeker-server/Cargo.toml" \
    --allow-dirty \
    --no-verify \
    --config "patch.crates-io.file-peeker-protocol.path='$repo_root/crates/file-peeker-protocol'"

for crate_archive in "$protocol_crate" "$server_crate"; do
    if [[ ! -f $crate_archive ]]; then
        echo "Expected package was not created: $crate_archive" >&2
        exit 1
    fi
done

echo "Transferring packages to $ssh_destination"
run_ssh "$ssh_destination" "mkdir -p '$remote_root'"
run_ssh "$ssh_destination" \
    "cat > '$remote_root/file-peeker-protocol-$version.crate'" \
    <"$protocol_crate"
run_ssh "$ssh_destination" \
    "cat > '$remote_root/file-peeker-server-$version.crate'" \
    <"$server_crate"

echo "Installing file-peeker-server $version on $ssh_destination"
run_ssh "$ssh_destination" sh -s -- \
    "$remote_root" \
    "$version" \
    "$remote_cargo" <<'REMOTE_SCRIPT'
set -eu

remote_root=$1
version=$2
cargo_bin=$3

protocol_package="$remote_root/file-peeker-protocol-$version"
server_package="$remote_root/file-peeker-server-$version"
install_root="$remote_root/install"

tar -xzf "$remote_root/file-peeker-protocol-$version.crate" -C "$remote_root"
tar -xzf "$remote_root/file-peeker-server-$version.crate" -C "$remote_root"

"$cargo_bin" install \
    --locked \
    --root "$install_root" \
    --path "$server_package" \
    --bin file-peeker-server \
    --config "patch.crates-io.file-peeker-protocol.path='$protocol_package'"

installed_bin="$install_root/bin/file-peeker-server"
test -x "$installed_bin"

install_list=$("$cargo_bin" install --root "$install_root" --list)
case "$install_list" in
    *"file-peeker-server v$version "*"file-peeker-server"*) ;;
    *)
        echo "Cargo did not report the expected installed server:" >&2
        echo "$install_list" >&2
        exit 1
        ;;
esac

echo "Remote installation verified: $installed_bin"
REMOTE_SCRIPT
