#!/usr/bin/env bash
#
# Documentation gate. Two things a reader notices immediately and a writer never
# does: a link that 404s, and an anchor that silently scrolls nowhere.
#
# A rule with no enforcer is a wish, so this runs in CI beside the cargo gate.

set -uo pipefail

cd "$(dirname "$0")/.." || exit 1

bad=0

check_links() {
    local file dir links link target anchor resolved slugs
    for file in $(find . -name '*.md' -not -path './target/*' -not -path './.git/*'); do
        dir=$(dirname "$file")
        links=$(grep -oE '\]\([^)#]*\.md(#[A-Za-z0-9-]+)?\)' "$file" | sed 's/^](//; s/)$//')

        for link in $links; do
            target="${link%%#*}"
            anchor="${link#*#}"
            resolved="$dir/$target"

            if [ ! -f "$resolved" ]; then
                echo "broken link    $file -> $link"
                bad=$((bad + 1))
                continue
            fi

            # No anchor to check when the link had no '#'.
            [ "$anchor" = "$link" ] && continue

            # GitHub's slug: lowercase, punctuation dropped, spaces to dashes.
            slugs=$(grep -E '^#{1,6} ' "$resolved" \
                | sed 's/^#* //' \
                | tr '[:upper:]' '[:lower:]' \
                | sed 's/[^a-z0-9 -]//g; s/ /-/g')

            if ! echo "$slugs" | grep -qx "$anchor"; then
                echo "broken anchor  $file -> $link"
                bad=$((bad + 1))
            fi
        done
    done
}

# House style: no em dashes anywhere, in prose or in code comments.
#
# The character is built from its escape rather than written literally, so this
# file does not match its own check.
check_em_dashes() {
    local hits em_dash
    em_dash=$(printf '\u2014')
    hits=$(grep -rn "$em_dash" --include='*.md' --include='*.rs' --include='*.sh' \
        --include='*.ps1' --include='*.toml' . 2>/dev/null \
        | grep -v '^./target/' || true)

    if [ -n "$hits" ]; then
        echo "$hits" | while IFS= read -r line; do
            echo "em dash        $line"
        done
        bad=$((bad + $(echo "$hits" | wc -l)))
    fi
}

check_links
check_em_dashes

if [ "$bad" -ne 0 ]; then
    echo
    echo "$bad documentation problem(s)"
    exit 1
fi

echo "documentation: links and anchors resolve, no em dashes"
