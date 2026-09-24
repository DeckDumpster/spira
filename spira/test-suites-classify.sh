#!/usr/bin/env bash
# tier: T1
# covers: spira/suites.sh UC-test-infrastructure-25 UC-test-infrastructure-26 UC-test-infrastructure-29
# host-reason: sources suites.sh's pure functions (classify, fingerprint, cause_fp, record_*,
#   unreached_*) directly, through its source guard; no container, no process, no database.
#
# WHAT THIS REPLACES. suites.sh's status-word decision (ok/skip/timeout/setup-fault/red) and
# its two dedupe hashes were previously observable only by running and killing real suites —
# test-suites-unreached.sh, test-suites-watchdog-classify.sh and test-suites-cluster.sh spent
# ~100s of watchdog waits between them to exercise a handful of pure string transforms. This
# is that same logic as a direct table, at effectively zero cost.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
export SPIRA_SUITES_STATE="$TMP/state"
export SPIRA_GATE_SUITES="$TMP/gate-suites-empty"
: > "$SPIRA_GATE_SUITES"

# Sourcing (not executing) suites.sh: BASH_SOURCE[0] != $0 here, so its dispatcher never
# fires and this only defines the functions under test.
. "$HERE/suites.sh"

# --- classify() ------------------------------------------------------------------------
is "classify: rc=0 is ok"             ok      "$(classify 0 '')"
is "classify: rc=77 is skip"          skip    "$(classify 77 '')"
is "classify: rc=124 is timeout"      timeout "$(classify 124 '')"
is "classify: rc=1, no ASSERTIONS trailer, stays red" red \
    "$(classify 1 'FAIL: something broke')"
is "classify: rc=1, ASSERTIONS 0, is setup-fault" setup-fault \
    "$(classify 1 "$(printf 'starting up\nASSERTIONS 0')")"
is "classify: rc=2, ASSERTIONS present but nonzero, stays red" red \
    "$(classify 2 "$(printf 'FAIL: x\nASSERTIONS 3')")"
# POSITIVE CONTROL for the exact-line rule: ASSERTIONS 0 as a SUBSTRING of a longer line
# must not match — grep -qxF requires the whole line, so an un-migrated suite whose output
# happens to contain the digits "0" near the word "ASSERTIONS" stays red, not setup-fault.
is "classify: 'ASSERTIONS 0' as a substring (not the whole line) stays red" red \
    "$(classify 1 'preamble ASSERTIONS 0 trailer')"
is "classify: rc=143 (raw SIGTERM, never remapped here) is red, not timeout" red \
    "$(classify 143 '')"

# --- fingerprint() -----------------------------------------------------------------------
_out_a="$(printf 'ok 1\nFAIL: assertion x failed at /tmp/sptest_abc123/foo\nok 2\n')"
_out_b="$(printf 'ok 1\nFAIL: assertion x failed at /tmp/sptest_xyz987/foo\nok 2\n')"
is "fingerprint: scratch-path digits normalised, same fp" \
    "$(fingerprint 1 "$_out_a")" "$(fingerprint 1 "$_out_b")"
is "fingerprint: deterministic across two calls on the same input" \
    "$(fingerprint 1 "$_out_a")" "$(fingerprint 1 "$_out_a")"

# POSITIVE CONTROL: a genuinely different FAIL line must NOT collapse to the same fp —
# proven before the two rows above are trusted to mean the normalisation, not a bug that
# hashes everything to one constant.
_out_c="$(printf 'ok 1\nFAIL: a completely different assertion\nok 2\n')"
[ "$(fingerprint 1 "$_out_a")" != "$(fingerprint 1 "$_out_c")" ] \
    && ok "fingerprint: different FAIL text gets a different fp (positive control)" \
    || bad "fingerprint: different FAIL text gets a different fp (positive control)" \
        "both hashed to $(fingerprint 1 "$_out_a")"

# No FAIL line: falls back to the output tail, and the leading `rc=<rc>` line still makes
# the exit code part of the hash.
[ "$(fingerprint 0 'plain output, no fail marker')" \
    != "$(fingerprint 1 'plain output, no fail marker')" ] \
    && ok "fingerprint: no FAIL line still varies with rc" \
    || bad "fingerprint: no FAIL line still varies with rc" "collapsed across rc"

# --- cause_fp() — distinct from fingerprint(): keys on the FIRST FAIL line only ----------
_multi_a="$(printf 'FAIL: shared cause\nFAIL: suite-a-specific detail\n')"
_multi_b="$(printf 'FAIL: shared cause\nFAIL: suite-b-specific detail\n')"
is "cause_fp: two outputs sharing a first FAIL line cluster together" \
    "$(cause_fp 1 "$_multi_a")" "$(cause_fp 1 "$_multi_b")"
[ "$(fingerprint 1 "$_multi_a")" != "$(fingerprint 1 "$_multi_b")" ] \
    && ok "fingerprint: the same two outputs do NOT dedupe (distinct from cause_fp)" \
    || bad "fingerprint: the same two outputs do NOT dedupe (distinct from cause_fp)" \
        "collapsed — fingerprint must not share cause_fp's per-suite-agnostic key"
is "cause_fp: no FAIL line keys on rc, not on unrelated text" \
    "$(cause_fp 3 'no markers here')" "$(cause_fp 3 'no markers here either, unrelated text')"

# --- record_write / record_read -----------------------------------------------------------
wantrc "record_read: nothing written yet fails (positive control)" 1 \
    "$(record_read test-fake-a.sh >/dev/null 2>&1; echo $?)"
record_write test-fake-a.sh ok 5 -
is "record_write/read: status field round-trips" ok "$(record_read test-fake-a.sh | awk '{print $1}')"
is "record_write/read: seconds field round-trips" 5 "$(record_read test-fake-a.sh | awk '{print $3}')"
record_write test-fake-a.sh red 9 deadbeef
is "record_write/read: a second write replaces, not appends" red \
    "$(record_read test-fake-a.sh | awk '{print $1}')"
is "record_write/read: fingerprint field round-trips" deadbeef \
    "$(record_read test-fake-a.sh | awk '{print $4}')"

# --- unreached_write / clear / read --------------------------------------------------------
wantrc "unreached_read: nothing written yet fails (positive control)" 1 \
    "$(unreached_read test-fake-b.sh >/dev/null 2>&1; echo $?)"
unreached_write test-fake-b.sh
wantrc "unreached_read: after write, succeeds" 0 \
    "$(unreached_read test-fake-b.sh >/dev/null 2>&1; echo $?)"
# THE INVARIANT UC-29 NAMES: unreached must never overwrite the last verdict (sp-u1g).
record_write test-fake-b.sh red 3 cafef00d
unreached_write test-fake-b.sh
is "unreached_write: does not touch the .result verdict it sits beside" red \
    "$(record_read test-fake-b.sh | awk '{print $1}')"
unreached_clear test-fake-b.sh
wantrc "unreached_clear: read fails again" 1 \
    "$(unreached_read test-fake-b.sh >/dev/null 2>&1; echo $?)"
is "unreached_clear: the .result verdict it sat beside survives the clear" red \
    "$(record_read test-fake-b.sh | awk '{print $1}')"

tl_summary
