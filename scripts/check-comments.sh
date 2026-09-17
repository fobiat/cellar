#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
ceiling=10
failed=0

check_group() {
    local name=$1 mode=$2
    shift 2
    local files=()
    mapfile -d '' files < <(git ls-files -z -- "$@")
    local counts
    counts=$(awk -v mode="$mode" '
        BEGIN { comments = 0; code = 0; block = 0 }
        {
            text = $0
            sub(/^[[:space:]]*/, "", text)
            if (text == "") next
            comment = 0
            if (block) {
                comment = 1
                close_pos = mode == "script" ? index(text, "#>") : index(text, "*/")
                if (close_pos) {
                    rest = substr(text, close_pos + 2)
                    sub(/^[[:space:]]*/, "", rest)
                    if (rest != "") comment = 0
                    block = 0
                }
            } else if (mode == "script") {
                if (text ~ /^<#/) {
                    comment = 1
                    if (!index(substr(text, 3), "#>")) block = 1
                } else if (text ~ /^#/ && text !~ /^#!/) {
                    comment = 1
                }
            } else if (text ~ /^\/\*/) {
                comment = 1
                if (!index(substr(text, 3), "*/")) block = 1
            } else if (text ~ /^\/\//) {
                comment = 1
            }
            if (comment) comments++; else code++
        }
        END { print comments, code }
    ' "${files[@]}")
    local comments code total
    read -r comments code <<< "$counts"
    total=$((comments + code))
    printf '%-8s %5d comments / %5d nonblank lines\n' "$name" "$comments" "$total"
    if ((comments * 100 > total * ceiling)); then
        echo "$name exceeds the ${ceiling}% whole-line comment ceiling" >&2
        failed=1
    fi
}

check_group Rust source '*.rs'
check_group JS source '*.js' '*.mjs'
check_group scripts script '*.sh' '*.ps1'
check_group CSS source '*.css'

exit "$failed"
