#!/usr/bin/env zsh
setopt errexit nounset pipefail

repo_root=${0:A:h:h}
app_name="FilePeeker"
bundle_id="dev.filepeeker.FilePeeker"
app_path="$repo_root/swift/DerivedData/Build/Products/Debug/FilePeeker.app"

wait_for_app_exit() {
    local attempt
    for attempt in {1..50}; do
        if ! pgrep -x "$app_name" >/dev/null; then
            return 0
        fi
        sleep 0.1
    done
    return 1
}

if pgrep -x "$app_name" >/dev/null; then
    osascript -e "tell application id \"$bundle_id\" to quit" || true

    if ! wait_for_app_exit; then
        pkill -TERM -x "$app_name"
    fi

    if ! wait_for_app_exit; then
        echo "File Peeker did not terminate" >&2
        exit 1
    fi
fi

make -C "$repo_root" xcode-build

if [[ ! -d "$app_path" ]]; then
    echo "File Peeker app was not produced at $app_path" >&2
    exit 1
fi

open "$app_path"
