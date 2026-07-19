#!/bin/sh

set -eu

runtime=$1
run_root=$2
version=$3

if [ -L "$run_root" ] || { [ -e "$run_root" ] && [ ! -d "$run_root" ]; }; then
    printf '%s\n' 'unsafe remote runtime directory' >&2
    exit 1
fi

umask 077
mkdir -p "$run_root"
chmod 700 "$run_root"
mkdir "$runtime"
chmod 700 "$runtime"

server="$HOME/.file-peeker/servers/$version/bin/file-peeker-server"
"$server" serve --socket "$runtime/server.sock" --remove-parent-on-exit &
server_pid=$!

(cat >/dev/null; kill "$server_pid" 2>/dev/null || :) &
monitor_pid=$!

cleanup() {
    kill "$server_pid" "$monitor_pid" 2>/dev/null || :
    rm -rf "$runtime"
}
trap cleanup EXIT HUP INT TERM

set +e
wait "$server_pid"
status=$?
kill "$monitor_pid" 2>/dev/null || :
wait "$monitor_pid" 2>/dev/null
exit "$status"
