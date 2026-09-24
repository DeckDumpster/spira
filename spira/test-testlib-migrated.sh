#!/usr/bin/env bash
# tier: T0
# covers: spira/test-*.sh spira/testlib.sh
# hermetic-ok: reads suite source and filesystem; no database, no systemd
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

# Suites still carrying their own ok()/bad()/is()/want()/nowant()/wantrc(), grouped by why:
#
# Never used the ok()/want()/nowant() family testlib.sh replaces -- testlib.sh's own
# header puts the mechanical migration at 459 of 466 suites for exactly this reason.
# These 6 build their pass/fail counters a different way (pass()/fail(), or a hand-rolled
# is()/want() with an inline counter, some as multi-line functions) and need a real rewrite,
# not a rename. Migration filed as sp-u1a.
NEVER_OK_FAMILY="test-aeon-heartbeat.sh test-aeon-lease.sh test-concierge.sh test-suite-state.sh test-suites-flake-branch.sh test-thrash-wall.sh"
#
# Did use the ok()/want()/nowant() family but do something testlib.sh cannot yet do.
# Subshell-safe counters (test-canary.sh, test-gate-diag.sh, test-watch-refresh.sh --
# their assertions run inside `( )` subshells, whose variable writes never reach the
# parent, so testlib's own internal counters and TAP case numbering would silently
# corrupt across the subshell boundary): filed as sp-wpr. A per-case skip/note counter
# distinct from testlib's whole-suite skip() (test-timer-templates.sh): filed as sp-bcy.
NEEDS_TESTLIB_EXTENSION="test-canary.sh test-gate-diag.sh test-timer-templates.sh test-watch-refresh.sh"
#
# Shrink either list as a suite converts, and this check catches a name added to either
# list for any other reason.
EXPECTED_OFFENDERS="$NEVER_OK_FAMILY $NEEDS_TESTLIB_EXTENSION"

printf 'test-testlib-migrated.sh\n'

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

offenders_in() {  # offenders_in <dir> -> newline-separated basenames redefining a testlib primitive
    grep -lE '^(ok|bad|fail|is|want|nowant|notwant|wantrc)\(\)' "$1"/test-*.sh 2>/dev/null \
        | xargs -n1 basename 2>/dev/null | LC_ALL=C sort
}

# POSITIVE CONTROL: a suite that still defines ok() must be caught.
cp "$HERE/test-dummy.sh" "$TMP/test-dummy.sh"
printf 'ok() { :; }\n' >> "$TMP/test-dummy.sh"
_planted="$(offenders_in "$TMP")"
want "positive control: planted ok() is detected" "test-dummy.sh" "$_planted"
rm -f "$TMP/test-dummy.sh"

_found="$(offenders_in "$HERE")"
_expected="$(printf '%s\n' $EXPECTED_OFFENDERS | LC_ALL=C sort)"

_unexpected="$(LC_ALL=C comm -23 <(printf '%s\n' "$_found") <(printf '%s\n' "$_expected"))"
is "no suite defines ok()/want()/nowant() outside the declared exceptions" "" "$_unexpected"

_stale="$(LC_ALL=C comm -13 <(printf '%s\n' "$_found") <(printf '%s\n' "$_expected"))"
is "every declared exception still actually needs one" "" "$_stale"

tl_summary
