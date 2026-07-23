#!/usr/bin/env bash

set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
status=0

check_file_links() {
    local file="$1"
    local destination
    while IFS= read -r destination; do
        case "$destination" in
            http://* | https://* | mailto:* | \#*)
                continue
                ;;
        esac

        destination="${destination%%#*}"
        if [[ "$destination" == \<*\> ]]; then
            destination="${destination#<}"
            destination="${destination%>}"
        fi
        if [[ -z "$destination" ]]; then
            continue
        fi

        local target
        if [[ "$destination" == /* ]]; then
            target="$repository_root$destination"
        else
            target="$(dirname "$file")/$destination"
        fi
        if [[ ! -e "$target" ]]; then
            echo "${file#"$repository_root/"}: broken local link: $destination" >&2
            status=1
        fi
    done < <(
        perl -ne 'while (/\]\(([^)]+)\)/g) { print "$1\n" }' "$file"
    )
}

check_file_links "$repository_root/README.md"
while IFS= read -r file; do
    check_file_links "$file"
done < <(find "$repository_root/docs" -type f -name '*.md' -print | sort)

if rg -n '[[:blank:]]+$' "$repository_root/README.md" "$repository_root/docs"; then
    echo "documentation contains trailing whitespace" >&2
    status=1
fi

exit "$status"

