#!/usr/bin/env bash
# covers: spira/gate-diag.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"

_RESULTS="$(mktemp)"
trap 'rm -f "$_RESULTS"' EXIT
ok()     { printf 'ok\n'  >> "$_RESULTS"; printf '  ok    %s\n' "$1"; }
bad()    { printf 'bad\n' >> "$_RESULTS"; printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "unwanted [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

printf 'test-gate-diag\n'

# Build a results root with one red suite.
# result file fields: status _ secs _ _ _ rc
_make_root() {
    local root suite has_fail
    root="$(mktemp -d)"
    suite="${1:-test-foo}"
    has_fail="${2:-yes}"
    # field positions: 1=status 2=_ 3=secs 4=_ 5=_ 6=_ 7=rc
    printf 'red _ 5 _ _ _ 1\n' > "$root/${suite}.result"
    if [ "$has_fail" = "yes" ]; then
        printf 'some setup line\nFAIL: assertion blew up\nanother line\n' \
            > "$root/${suite}.out"
    else
        printf 'some setup line\nanother line\n' \
            > "$root/${suite}.out"
    fi
    printf '%s' "$root"
}

# ── T1: red suite with FAIL lines in GHA mode — both headers appear, stderr empty
printf '\nT1: red suite with FAIL lines (GHA) shows both headers, stderr clean\n'
(
    root="$(_make_root test-suite yes)"
    trap 'rm -rf "$root"' EXIT
    stderr_tmp="$(mktemp)"
    trap 'rm -f "$stderr_tmp"' EXIT

    out="$(GITHUB_ACTIONS=1 bash "$HERE/gate-diag.sh" "$root" 2>"$stderr_tmp")"
    err="$(cat "$stderr_tmp")"

    want "FAIL lines header present"    '--- FAIL lines ---'    "$out"
    want "last N lines header present"  '--- last'              "$out"
    is   "stderr is empty"              ""                      "$err"
)

# ── T2: red suite with no FAIL lines — only last-N header, no FAIL header ──────
printf '\nT2: red suite with no FAIL lines shows only last-N header, stderr clean\n'
(
    root="$(_make_root test-suite no)"
    trap 'rm -rf "$root"' EXIT
    stderr_tmp="$(mktemp)"
    trap 'rm -f "$stderr_tmp"' EXIT

    out="$(bash "$HERE/gate-diag.sh" "$root" 2>"$stderr_tmp")"
    err="$(cat "$stderr_tmp")"

    nowant "FAIL lines header absent"    '--- FAIL lines ---'   "$out"
    want   "last N lines header present" '--- last'             "$out"
    is     "stderr is empty"             ""                     "$err"
)

# ── T3: positive control — the unfixed printf would have printed an error ──────
printf '\nT3: positive control — printf -- strips dashes without error\n'
(
    stderr_tmp="$(mktemp)"
    trap 'rm -f "$stderr_tmp"' EXIT
    printf '%s\n' '--- FAIL lines ---' 2>"$stderr_tmp"
    err="$(cat "$stderr_tmp")"
    is "printf with %s format is error-free" "" "$err"
)

# ── summary ────────────────────────────────────────────────────────────────────
pass="$(grep -c '^ok$'  "$_RESULTS" 2>/dev/null || true)"
fail="$(grep -c '^bad$' "$_RESULTS" 2>/dev/null || true)"
printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "${fail:-0}" -eq 0 ]
