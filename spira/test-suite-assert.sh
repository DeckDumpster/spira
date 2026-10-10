#!/usr/bin/env bash
# test-suite-assert.sh — suite-assert.sh's report grammar: ok/FAIL lines and the ASSERTIONS
# trailer, including on an early exit, driven through fixture suites run as child processes.
# tier: T1
# covers: spira/suite-assert.sh UC-test-infrastructure-34
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

fixture() {   # fixture <name> <body...> — a suite that sources suite-assert.sh, then runs the body
    local f="$TMP/$1"; shift
    { printf '#!/usr/bin/env bash\nset -uo pipefail\n. "%s/suite-assert.sh"\n' "$HERE"; printf '%s\n' "$@"; } > "$f"
    printf '%s' "$f"
}
run() { env -i PATH="$PATH" HOME="$TMP" bash "$1" 2>&1; }

f="$(fixture mixed 'ok "first"' 'is "same" a a' 'is "differs" a b' 'want "has" ell hello' 'nowant "lacks" z hello' 'bad "explicit" "why"')"
out="$(run "$f")"
want   "ok line grammar"                      "  ok    first" "$out"
want   "passing is() emits an ok line"        "  ok    same" "$out"
want   "failing is() emits a FAIL line"       "  FAIL  differs: wanted [a] got [b]" "$out"
want   "want() passes on a substring"         "  ok    has" "$out"
want   "nowant() passes on absence"           "  ok    lacks" "$out"
want   "bad() carries its detail"             "  FAIL  explicit: why" "$out"
is     "trailer counts every assertion"       "ASSERTIONS 6" "$(printf '%s\n' "$out" | grep '^ASSERTIONS')"

f="$(fixture early 'ok "one"' 'exit 3')"
out="$(run "$f")"; rc=$?
want   "early exit still emits the trailer"   "ASSERTIONS 1" "$out"

f="$(fixture none 'exit 1')"
out="$(run "$f")"
want   "zero assertions reads as ASSERTIONS 0" "ASSERTIONS 0" "$out"

f="$(fixture nowant-fails 'nowant "present" ell hello')"
out="$(run "$f")"
want   "nowant() fails on presence"           "  FAIL  present" "$out"

tl_summary
