#!/usr/bin/env bash
# suite-coverage-json.sh — every spira/test-*.sh's own # tier:/# covers: header, as JSON, for
# the test-plan binary to validate against the typed catalogue.
#
#   suite-coverage-json.sh                working tree's spira/test-*.sh
#   suite-coverage-json.sh --ref <ref>     spira/test-*.sh as they existed at <ref>
#
# ONE PARSER: this calls `suite-select header tier|covers` rather than re-reading the header
# format itself, so the JSON this emits can never drift from what the selector and
# batcher-cut already agree a header means (suite-select/src/header.rs).
#
# A Rust source line `// covers: UC-…` (a #[test] naming the use case it verifies) counts as
# cover: it is listed as a T0 entry under the source file's own path, so the plan and the
# orphan fence read it exactly as they read a suite's header.
#
# --ref reads suites out of git history, not the working tree, so a caller can compare a
# branch's tip against its base (spira/plan-lint.sh --orphans) without a second checkout.
#
# tier: T0
# covers: spira/suite-coverage-json.sh suite-select/
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)"
[ -n "$ROOT" ] || ROOT="$(cd "$HERE/.." && pwd -P)"

json_escape() {
    local s="$1"
    s="${s//\\/\\\\}"
    s="${s//\"/\\\"}"
    printf '%s' "$s"
}

emit_one() {  # emit_one <path-for-json> <file-on-disk>
    local rel="$1" f="$2" tier cov first=1
    tier="$(suite-select header tier "$f")"
    cov="$(suite-select header covers "$f")"
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

emit_rust() {
    local line file ids id first_id
    local -A seen=()
    while IFS= read -r line; do
        file="${line%%:*}"
        ids="$(printf '%s' "${line#*:}" | grep -oE 'UC-[a-z0-9-]+-[0-9]+')"
        for id in $ids; do seen["$file"]="${seen[$file]:-} $id"; done
    done < <(git -C "$ROOT" grep -HE '^[[:space:]]*// covers: UC-' "$@" -- '*.rs' 2>/dev/null || true)
    for file in "${!seen[@]}"; do
        [ "$first" = 1 ] && first=0 || printf ','
        printf '{"path":"%s","tier":"T0","covers":[' "$(json_escape "$file")"
        first_id=1
        for id in $(printf '%s\n' ${seen[$file]} | sort -u); do
            [ "$first_id" = 1 ] && first_id=0 || printf ','
            printf '"%s"' "$id"
        done
        printf ']}'
    done
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
    emit_rust "$ref"
else
    shopt -s nullglob
    for f in "$HERE"/test-*.sh; do
        [ "$first" = 1 ] && first=0 || printf ','
        emit_one "spira/$(basename "$f")" "$f"
    done
    shopt -u nullglob
    emit_rust
fi
printf ']\n'
