#!/usr/bin/env bash
#
# docs-links.sh <root> — prints every broken relative link or anchor in README.md and docs/,
# every README.md#anchor reference in the tree that dangles, and a README over its line cap;
# exits 1 when it printed anything.
set -uo pipefail
root="$1"
export LC_ALL=C.UTF-8

anchors() {   # anchors <file.md> -> the GitHub-style slug of each heading outside a fence
    gawk '
        /^```/ { fence = !fence; next }
        !fence && /^#{1,6} / {
            h = $0; sub(/^#+/, "", h); gsub(/^[[:space:]]+|[[:space:]]+$/, "", h)
            h = tolower(h); gsub(/[`*_]/, "", h); gsub(/[^[:alnum:] -]/, "", h); gsub(/ /, "-", h)
            print h
        }' "$1"
}

declare -A ANCH
has_anchor() {   # has_anchor <file.md> <frag>
    [ -n "${ANCH[$1]+x}" ] || ANCH[$1]=$'\n'"$(anchors "$1")"$'\n'
    [[ "${ANCH[$1]}" == *$'\n'"$2"$'\n'* ]]
}

bad=()
while IFS=$'\t' read -r src target; do
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
done < <(git -C "$root" ls-files README.md docs | grep -E '\.md$' | grep -vE -- '-evidence/|/sources/' |
    while IFS= read -r f; do
        gawk -v src="$f" '
            /^```/ { fence = !fence }
            fence { next }
            {
                s = $0
                while (match(s, /(^|[^!])\[[^]]*\]\(([^)[:space:]]+)\)/, m)) {
                    print src "\t" m[2]
                    s = substr(s, RSTART + RLENGTH)
                }
            }' "$root/$f"
    done)

while IFS= read -r r; do
    has_anchor "$root/README.md" "${r#*#}" || bad+=("$r: dangling README anchor")
done < <(git -C "$root" grep -hoI 'README\.md#[A-Za-z0-9_-]*' -- ':!spira/test-docs-links.sh' | sort -u)

n="$(wc -l < "$root/README.md")"
[ "$n" -lt 300 ] || bad+=("README.md is $n lines, limit 300")

[ "${#bad[@]}" -eq 0 ] && exit 0
printf '%s\n' "${bad[@]}"
exit 1
