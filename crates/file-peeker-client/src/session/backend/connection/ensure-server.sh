#!/bin/sh

server_version=$1
force_install=$2
server_root="$HOME/.file-peeker/servers/$server_version"
server_executable="$server_root/bin/file-peeker-server"

install_server() {
    set -- \
        --locked \
        --root "$server_root" \
        --bin file-peeker-server
    if [ "$force_install" = true ]; then
        set -- --force "$@"
    fi

    cargo install \
        "$@" \
        --git https://github.com/tanchihpin0517/file-peeker.git \
        --version "$server_version" \
        file-peeker-server
}

if [ "$force_install" != true ] && [ -x "$server_executable" ]; then
    printf 'FILE_PEEKER_SERVER_READY=%s\n' "$server_executable"
elif command -v cargo >/dev/null 2>&1 &&
    install_server &&
    [ -x "$server_executable" ]; then
    printf 'FILE_PEEKER_SERVER_READY=%s\n' "$server_executable"
else
    printf 'FILE_PEEKER_SERVER_ERROR=%s\n' 'unable to install file-peeker-server'
fi
