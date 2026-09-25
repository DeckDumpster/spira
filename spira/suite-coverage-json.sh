#!/usr/bin/env bash
# suite-coverage-json.sh — every spira/test-*.sh's own # tier:/# covers: header, as JSON, for
# the test-plan binary to validate against the typed catalogue.
#
#   suite-coverage-json.sh                working tree's spira/test-*.sh
#   suite-coverage-json.sh --ref <ref>     spira/test-*.sh as they existed at <ref>
#
# ONE PARSER: this sources spira/suite-covers.sh (suite_tier_of/suite_covers_of) rather than
# re-reading the header format itself, so the JSON this emits can never drift from what
# suites.sh and gate-spira.sh already agree a header means.
#
# --ref reads suites out of git history, not the working tree, so a caller can compare a
# branch's tip against its base (spira/plan-lint.sh --orphans) without a second checkout.
#
# tier: T0
# covers: spira/suite-coverage-json.sh spira/suite-covers.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)"
[ -n "$ROOT" ] || ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/suite-covers.sh"

json_escape() {
    local s="$1"
    s="${s//\\/\\\\}"
    s="${s//\"/\\\"}"
    printf '%s' "$s"
}

emit_one() {  # emit_one <path-for-json> <file-on-disk>
    local rel="$1" f="$2" tier cov first=1
    tier="$(suite_tier_of "$f")"
    cov="$(suite_covers_of "$f")"
    printf '{"path":"%s","tier":' "$(json_escape "$rel")"
    if [ -n "$tier" ]; then
        printf '"%s"' "$(json_escape "$tier")"
    else
        printf 'null'
    fi
    printf ',"covers":['
    for tok in $cov; do
        [ "$first" = 1 ] && first=0 || printf ','
        printf '"%s"' "$(json_escape "$tok")"
    done
    printf ']}'
}

ref=""
case "${1:-}" in
    --ref) ref="${2:?usage: suite-coverage-json.sh --ref <ref>}" ;;
    "") ;;
    *) printf 'suite-coverage-json.sh: unknown argument %s\n' "$1" >&2; exit 2 ;;
esac

printf '['
first=1
if [ -n "$ref" ]; then
    tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
    while IFS= read -r sp; do
        [ -n "$sp" ] || continue
        sn="$(basename "$sp")"
        git -C "$ROOT" show "$ref:$sp" > "$tmp/$sn" 2>/dev/null || continue
        [ "$first" = 1 ] && first=0 || printf ','
        emit_one "spira/$sn" "$tmp/$sn"
    done < <(git -C "$ROOT" ls-tree -r "$ref" --name-only 2>/dev/null | grep '^spira/test-[^/]*\.sh$' || true)
else
    shopt -s nullglob
    for f in "$HERE"/test-*.sh; do
        [ "$first" = 1 ] && first=0 || printf ','
        emit_one "spira/$(basename "$f")" "$f"
    done
    shopt -u nullglob
fi
printf ']\n'
