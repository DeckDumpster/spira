#!/usr/bin/env bash
#
# test-aeon-elastic-concurrency.sh — aeon.sh's own concurrency check honours FAYTH_ELASTIC,
#   using the same fayth_free (lib.sh) the sentinel already calls, so the two cannot drift.
#
#   ./test-aeon-elastic-concurrency.sh
#
# THE DEFECT (sp-4gxjo). The sentinel summons an elastic persona up to the declared pool
# (SPIRA_MAX_AEONS) via fayth_free, which honours FAYTH_ELASTIC. aeon.sh's OWN concurrency
# check — run by the aeon itself right after it wakes, before it claims anything — compared
# `have` against FAYTH_MAX_CONCURRENT alone and never looked at FAYTH_ELASTIC at all. With a
# pool of 6 and FAYTH_MAX_CONCURRENT=3 (the fallback for a host with no pool), the sentinel
# kept summoning past the third builder and every one of them exited immediately logging "at
# capacity (3/3)" — five wasted summons in eight minutes on 2026-09-24.
#
# THE FIX. aeon.sh now computes the same pool-remainder the sentinel computes before calling
# fayth_free (pool = SPIRA_MAX_AEONS - have, only when FAYTH_ELASTIC=1), then asks fayth_free
# the same question CHECK 7 already asks. A non-elastic fayth gets no pool argument, so
# fayth_free falls through to FAYTH_MAX_CONCURRENT exactly as the old inline check did.
#
# T1, over the shared function (fayth_free itself is already covered by test-summon-fayth.sh):
#   1. elastic, pool=6, 5 live  -> allowed (aeon proceeds past the capacity gate)
#   2. elastic, pool=6, 6 live  -> refused ("at capacity (6/6)")
#   3. non-elastic (no pool draw), MAX_CONCURRENT=2: 1 live -> allowed, 2 live -> refused
#      (regression check: the fallback path this fix must not disturb)
#
# "ALLOWED" IS OBSERVED WITHOUT A DATABASE. Rather than standing up testdb/bd to let the aeon
# claim a real bead, a world.halted stamp is armed before each "allowed" run: aeon.sh checks
# concurrency FIRST and halted SECOND, so an aeon that clears the capacity gate reaches the
# halted gate immediately afterward and exits there with a distinct, unambiguous log line
# ("halted — claiming nothing") and a distinct ledger reason ("awake $FAYTH halted"). An aeon
# that is refused never reaches that line at all — it exits at the capacity gate with "awake
# $FAYTH capacity". The two ledger reasons cannot be confused for one another.
#
# aeon_count IS STUBBED (a fake lib.sh sourced in place of the real one, which itself sources
# the real lib.sh so fayth_free is exercised unmodified) so the "have" side of the comparison
# is a controlled number rather than a real process count.
#
# defect: sp-4gxjo
# covers: spira/aeon.sh spira/lib.sh spira/chamber/builder.fayth
# hermetic-ok: no database, no systemd; aeon_count is stubbed, SPIRA_AGENT is never reached
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-aeon-elastic-concurrency.sh"

# aeon.sh must still call fayth_free rather than a private comparison — a refusal to run at
# all here is louder than a suite that would otherwise pass against a reverted fix by
# accident (mirrors the SPIRA_AGENT injection-point guard other aeon.sh suites carry).
grep -q 'fayth_free "$FAYTH"' "$HERE/aeon.sh" \
    || { echo "test-aeon-elastic-concurrency.sh: aeon.sh no longer calls fayth_free for its own concurrency check — refusing to run" >&2; exit 1; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# Minimal harness tree: only aeon.sh is copied (and instrumented via a stub lib.sh beside
# it); everything else is read from the real checkout so fayth_free itself is exercised
# unmodified.
export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_CONF="$TMP/no-such.conf"
export SPIRA_DB="$TMP/no-db"
# The fence (fayth_fenced, lib.sh) refuses an empty FAYTH_LABELS and, when SPIRA_SCOPE_LABEL
# is set, requires the label naming this scope. Neither is what this suite is testing, so
# disable scope restriction the same way an operator would to allow an unrestricted predicate.
export SPIRA_SCOPE_LABEL=""
cp "$HERE/aeon.sh" "$SPIRA_HOME/"

# The stub: source the REAL lib.sh (so fayth_free is the genuine article), then override
# aeon_count so "have" is a controlled number rather than a real /proc scan.
cat > "$SPIRA_HOME/lib.sh" <<STUB
# shellcheck disable=SC1090
. "$HERE/lib.sh"
aeon_count() { printf '%s' "\${MOCK_AEON_COUNT:-0}"; }
STUB

run_aeon() {   # run_aeon <fayth> -> stdout+stderr captured
    "$SPIRA_HOME/aeon.sh" "$1" 2>&1
}
refused() { [[ "$1" == *"at capacity"* ]]; }

LEDGER="$SPIRA_RUN/aeon-ledger.log"

# ======================================================================================
echo
echo "1 — elastic, pool declared (SPIRA_MAX_AEONS=6): 5 live is allowed, 6 live is refused"
# ======================================================================================
cat > "$SPIRA_HOME/chamber/builder.fayth" <<'F'
FAYTH_NAME=builder
FAYTH_LABELS="test,plan"
FAYTH_MAX_CONCURRENT=3
FAYTH_ELASTIC=1
F
export SPIRA_MAX_AEONS=6

: > "$LEDGER"
export MOCK_AEON_COUNT=5
rm -f "$SPIRA_RUN/world.halted"; : > "$SPIRA_RUN/world.halted"
out="$(run_aeon builder)"
if refused "$out"; then
    bad "pool=6, 5 live: allowed" "refused: $out"
else
    ok "pool=6, 5 live: not refused by the capacity gate"
fi
want "pool=6, 5 live: reached the halted gate (proves it cleared capacity)" \
     "halted — claiming nothing" "$out"
want "ledger records the halted exit, not a capacity exit" \
     "awake builder halted" "$(cat "$LEDGER")"
nowant "ledger does not record a capacity exit" "awake builder capacity" "$(cat "$LEDGER")"

: > "$LEDGER"
export MOCK_AEON_COUNT=6
rm -f "$SPIRA_RUN/world.halted"
out="$(run_aeon builder)"
want "pool=6, 6 live: refused, and names have/limit" "at capacity (6/6)" "$out"
want "ledger records the capacity exit" "awake builder capacity" "$(cat "$LEDGER")"
nowant "never reached the halted gate" "halted — claiming nothing" "$out"

# ======================================================================================
echo
echo "2 — positive control: with 2 free slots (pool=6, 4 live) elastic is still allowed"
# ======================================================================================
: > "$LEDGER"
export MOCK_AEON_COUNT=4
: > "$SPIRA_RUN/world.halted"
out="$(run_aeon builder)"
if refused "$out"; then
    bad "pool=6, 4 live: allowed" "refused: $out"
else
    ok "pool=6, 4 live: not refused (two slots free, not just the boundary)"
fi

# ======================================================================================
echo
echo "3 — non-elastic fayth: unaffected by SPIRA_MAX_AEONS, still uses its own cap"
# ======================================================================================
# Regression check: a fayth that never declared FAYTH_ELASTIC must keep the old behaviour
# exactly — refused at its own FAYTH_MAX_CONCURRENT regardless of how large the pool is.
cat > "$SPIRA_HOME/chamber/anchor.fayth" <<'F'
FAYTH_NAME=anchor
FAYTH_LABELS="test,incident"
FAYTH_MAX_CONCURRENT=2
F
# SPIRA_MAX_AEONS stays at 6 (non-default, still set from section 1) — a non-elastic fayth
# must ignore it entirely, so a large pool must not let it exceed its own smaller cap.

: > "$LEDGER"
export MOCK_AEON_COUNT=1
: > "$SPIRA_RUN/world.halted"
out="$(run_aeon anchor)"
if refused "$out"; then
    bad "non-elastic, cap=2, 1 live: allowed" "refused: $out"
else
    ok "non-elastic, cap=2, 1 live: not refused"
fi

: > "$LEDGER"
export MOCK_AEON_COUNT=2
rm -f "$SPIRA_RUN/world.halted"
out="$(run_aeon anchor)"
want "non-elastic, cap=2, 2 live: refused at its own cap, not the pool" \
     "at capacity (2/2)" "$out"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
