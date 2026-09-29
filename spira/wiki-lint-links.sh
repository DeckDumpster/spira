#!/usr/bin/env bash
# wiki-lint-links.sh <wiki-dir> [file]
#
# Fails if a [[wikilink]] added to <file> (default index.md) by the current git index
# resolves to no path the same index tracks. "Added" means present in the staged content
# but not in HEAD's, so a link that was already broken before this commit does not newly
# fail it — only a commit that introduces the dangling link does.
#
# Compares against `git ls-files` (the index, not the working tree), so a page staged in
# the same commit as the link that names it counts as resolved.
set -uo pipefail
wiki="${1:?wiki-lint-links.sh: wiki dir required}"
file="${2:-index.md}"

extract_links() {
    grep -oE '\[\[[^][]+\]\]' | sed -E 's/^\[\[ *//; s/ *\]\]$//; s/ *\|.*$//' | sort -u
}

old="$(git -C "$wiki" show "HEAD:$file" 2>/dev/null)" || old=""
new="$(git -C "$wiki" show ":$file" 2>/dev/null)" || new=""

added="$(comm -13 <(printf '%s' "$old" | extract_links) <(printf '%s' "$new" | extract_links))"
[ -n "$added" ] || exit 0

tracked_basenames="$(git -C "$wiki" ls-files -- '*.md' | xargs -r -n1 basename | sed -E 's/\.md$//' | sort -u)"

missing=""
while IFS= read -r link; do
    [ -n "$link" ] || continue
    if ! grep -qFx "$link" <<< "$tracked_basenames"; then
        missing="${missing:+$missing$'\n'}$link"
    fi
done <<< "$added"

[ -z "$missing" ] || {
    printf 'wiki-lint-links.sh: %s adds a wikilink to a page this commit does not track:\n' "$file" >&2
    printf '  [[%s]]\n' "$missing" >&2
    exit 1
}
exit 0
