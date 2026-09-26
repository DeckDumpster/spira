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
# These build their pass/fail counters a different way (pass()/fail(), or a hand-rolled
# is()/want() with an inline counter, some as multi-line functions) and need a real rewrite,
# not a rename. Migration filed as sp-u1a.
NEVER_OK_FAMILY=""
#
# Did use the ok()/want()/nowant() family but do something testlib.sh cannot yet do.
# Subshell-safe counters (test-canary.sh, test-gate-diag.sh --
# their assertions run inside `( )` subshells, whose variable writes never reach the
# parent, so testlib's own internal counters and TAP case numbering would silently
# corrupt across the subshell boundary): filed as sp-wpr.
# Custom assertion helpers beyond ok/bad/want/is (test-install-refusal.sh defines
# is1/absent/present): filed as sp-u1b.
NEEDS_TESTLIB_EXTENSION="test-canary.sh
test-gate-diag.sh
test-install-refusal.sh"
#
# Suites carrying unmigrated ok()/want()/nowant() awaiting migration strategy decision
# from sp-l2be6. These 358 suites were not in the scope of sp-29g55 (which handled
# 9 incident suites + test-install-refusal.sh extension). Mechanical migration (like
# sp-qvjzb's 459 suites) is blocked until sp-l2be6 decides whether to migrate or exempt.
# Track this in test-testlib-migrated.sh to prevent silent growth of unmigrated suites.
DEFERRED_MIGRATION="
"
#
# Shrink any list as a suite converts, and this check catches a name added to either
# list for any other reason. DEFERRED_MIGRATION should shrink as sp-l2be6 makes
# decisions and migrations proceed.
EXPECTED_OFFENDERS="$NEVER_OK_FAMILY $NEEDS_TESTLIB_EXTENSION $DEFERRED_MIGRATION"

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
