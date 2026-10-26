#!/usr/bin/env bash
#
# docs-links.sh <root> — prints every broken relative link or anchor in README.md and docs/,
# every README anchor reference in the tree that dangles, and a README over its line cap;
# exits 1 when it printed anything.
set -uo pipefail
root="$1"
export LC_ALL=C.UTF-8

tracked() {   # tracked -> every file of the tree, relative; a tree without git is walked
    if git -C "$root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        git -C "$root" ls-files
    else
        (cd "$root" && find . \( -name .git -o -name target -o -name .runtime -o -name .beads -o -name .dolt \) -prune \
            -o -type f -print | sed "s#^\./##")
    fi
}

unfenced() {   # unfenced <file> -> its lines outside ``` fences
    awk '/^```/ { fence = !fence; next } !fence' "$1"
}

anchors() {   # anchors <file.md> -> the GitHub-style slug of each heading outside a fence
    unfenced "$1" | grep -E '^#{1,6} ' | sed -E 's/^#+//; s/^[[:space:]]+//; s/[[:space:]]+$//' |
        tr '[:upper:]' '[:lower:]' | sed -E 's/[`*_]//g; s/[^[:alnum:] -]//g; s/ /-/g'
}

declare -A ANCH
has_anchor() {   # has_anchor <file.md> <frag>
    [ -n "${ANCH[$1]+x}" ] || ANCH[$1]=$'\n'"$(anchors "$1")"$'\n'
    [[ "${ANCH[$1]}" == *$'\n'"$2"$'\n'* ]]
}

bad=(); seen=0
while IFS=$'\t' read -r src target; do
    seen=$((seen + 1))
    [[ "$target" =~ ^[a-z]+: ]] && continue
    path="${target%%#*}"; frag=""
    [[ "$target" == *"#"* ]] && frag="${target#*#}"
    if [ -n "$path" ]; then
        dest="$(realpath -m -s "$(dirname "$root/$src")/$path")"
    else
        dest="$root/$src"
    fi
    if [ ! -e "$dest" ]; then bad+=("$src: $target: no such file"); continue; fi
    if [ -n "$frag" ] && [[ "$dest" == *.md ]] && ! has_anchor "$dest" "$frag"; then
        bad+=("$src: $target: no such anchor")
    fi
done < <(tracked | grep -E '^(README\.md|docs/.*\.md)$' | grep -vE -- '-evidence/|/sources/' |
    while IFS= read -r f; do
        unfenced "$root/$f" | grep -oE '!?\[[^]]*\]\([^)[:space:]]+\)' | grep -v '^!' |
            sed -E "s|.*\]\(([^)]*)\)$|$f\t\1|"
    done)

while IFS= read -r r; do
    has_anchor "$root/README.md" "${r#*#}" || bad+=("$r: dangling README anchor")
done < <(tracked | grep -vx spira/test-docs-links.sh | (cd "$root" && xargs -d '\n' grep -shoI 'README\.md#[A-Za-z0-9_-]*') | sort -u)

[ "$seen" -gt 0 ] || bad+=("no link found in README.md or docs/: the extractor read nothing")
n="$(wc -l < "$root/README.md")"
[ "$n" -lt 300 ] || bad+=("README.md is $n lines, limit 300")

[ "${#bad[@]}" -eq 0 ] && exit 0
printf '%s\n' "${bad[@]}"
exit 1
