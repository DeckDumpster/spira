#!/usr/bin/env bash
#
# test-sentinel-order.sh — CHECK 7 (summon) runs before the Sending; the fill loop
#   exhausts the task pool in one pass; and when the base has not moved the Sending
#   is skipped.
#
#   ./test-sentinel-order.sh
#
# THREE PAIRS:
#   1. Fill: pool=3 + 5 ready builder beads → 3 summons. Pair: pool=1 → 1.
#   2. Sending skip: empty stamp + 0 repos → skipped. Pair: no stamp → walks.
#   3. Order: CHECK7 log appears before the sending line in one pass.
#
# POSITIVE CONTROLS. The fill pair (pool=1 → 1) runs first so a while loop that fires
# at most once would pass it but fail the pool=3 assertion. The sending pair (no stamp →
# walks) runs first so a stub that never calls sending.sh would fail it.
#
# defect: sp-len2q
# covers: spira/sentinel.sh spira/lib.sh
# hermetic-ok: uses a fixture database; systemd/gh/network reached through
#   SPIRA_LAUNCH, SPIRA_SUMMON and SPIRA_SYSTEMCTL seams pointed at stubs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
before() {
    local a="$1" b="$2" out="$3"
    local pa pb
    pa="$(grep -n "$a" <<< "$out" | head -1 | cut -d: -f1)"
    pb="$(grep -n "$b" <<< "$out" | head -1 | cut -d: -f1)"
    [ -n "$pa" ] && [ -n "$pb" ] && [ "$pa" -lt "$pb" ] \
        && ok "$a appears before $b in log" \
        || bad "$a before $b" "pa=[${pa:-missing}] pb=[${pb:-missing}]"
}

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-sentinel-order
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up sentinel_order || { echo "test-sentinel-order: could not build fixture"; exit 1; }

SUMMON_LOG="$TMP/summon.log"
SENDING_LOG="$TMP/sending.log"

STUBS="$TMP/stubs"
mkdir -p "$STUBS"
ln -s "$HERE/chamber" "$STUBS/chamber"

for _s in pilgrimage.sh strand.sh governor.sh reflect.sh; do
    printf '#!/bin/sh\n' > "$STUBS/$_s"; chmod +x "$STUBS/$_s"
done
printf '#!/bin/sh\necho inactive\n'  > "$STUBS/mock-systemctl"; chmod +x "$STUBS/mock-systemctl"
printf '#!/bin/sh\nexit 0\n'         > "$STUBS/mock-launch";    chmod +x "$STUBS/mock-launch"
printf '#!/bin/sh\nexit 0\n'         > "$STUBS/mock-notify";    chmod +x "$STUBS/mock-notify"
# sending.sh records each invocation so assertions can verify whether it was called.
printf '#!/bin/sh\necho called >> "$SENDING_LOG"\n' > "$STUBS/sending.sh"; chmod +x "$STUBS/sending.sh"
# mock-summon records each summon so the fill assertions can count them.
printf '#!/bin/sh\necho summoned >> "$SUMMON_LOG"\n' > "$STUBS/mock-summon"; chmod +x "$STUBS/mock-summon"
touch "$STUBS/repo-map"   # empty: no repos, Sending finds nothing to walk

# run_pass <run-dir> [KEY=VAL ...] → combined stdout+stderr of one sentinel pass.
# SPIRA_RUN is the caller-supplied dir (allows pre-populating stamp files).
run_pass() {
    local run="$1"; shift
    mkdir -p "$run"
    env -i \
        PATH="$PATH" HOME="$HOME" \
        SPIRA_HOME="$STUBS" \
        SPIRA_RUN="$run" \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_BD="$SPIRA_BD" \
        SPIRA_PATH="$SPIRA_PATH" \
        SPIRA_GOAL="sp-goal1" \
        SPIRA_SKIP_RECLAIM=1 \
        SPIRA_LAND_STALE=999999 \
        SPIRA_SYSTEMCTL="$STUBS/mock-systemctl" \
        SPIRA_LAUNCH="$STUBS/mock-launch" \
        SPIRA_SUMMON="$STUBS/mock-summon" \
        SPIRA_NOTIFY="$STUBS/mock-notify" \
        SUMMON_LOG="$SUMMON_LOG" \
        SENDING_LOG="$SENDING_LOG" \
        "$@" \
        bash "$HERE/sentinel.sh" 2>&1
}

testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-goal1","title":"goal","status":"open","issue_type":"epic","labels":["plan"]}
{"id":"sp-b1","title":"bead 1","status":"open","issue_type":"task","labels":["plan"]}
{"id":"sp-b2","title":"bead 2","status":"open","issue_type":"task","labels":["plan"]}
{"id":"sp-b3","title":"bead 3","status":"open","issue_type":"task","labels":["plan"]}
{"id":"sp-b4","title":"bead 4","status":"open","issue_type":"task","labels":["plan"]}
{"id":"sp-b5","title":"bead 5","status":"open","issue_type":"task","labels":["plan"]}
JSONL

echo "test-sentinel-order.sh"

# ======================================================================================
echo
echo "fill — task pool exhausted in one pass:"
# ======================================================================================
# POSITIVE CONTROL FIRST. pool=1 → 1 summon. If the while loop fires at most once
# regardless of pool size, this passes and the fill assertion below catches it.
rm -f "$SUMMON_LOG"
run_pass "$TMP/run-f1" \
    SPIRA_FAYTHS=builder SPIRA_SCOPE_LABEL= SPIRA_MAX_AEONS=1 > /dev/null
is "pool=1 → 1 summon" "1" "$(grep -c . "$SUMMON_LOG" 2>/dev/null || echo 0)"

rm -f "$SUMMON_LOG"
run_pass "$TMP/run-f3" \
    SPIRA_FAYTHS=builder SPIRA_SCOPE_LABEL= SPIRA_MAX_AEONS=3 > /dev/null
is "pool=3 → 3 summons (fill loop fills the pool)" "3" "$(grep -c . "$SUMMON_LOG" 2>/dev/null || echo 0)"

# ======================================================================================
echo
echo "sending skip — base unchanged stamp → skipped; no stamp → walks:"
# ======================================================================================
# POSITIVE CONTROL FIRST (pair). No stamp: sending.sh must be called.
# The same run that proves "no stamp → walks" also writes the stamp; the second run
# reuses it to prove "stamp present and matching → skipped". One pair, two runs.
_sr="$TMP/run-send"
rm -f "$SENDING_LOG"
out_nostamp="$(run_pass "$_sr" SPIRA_FAYTHS=)"
is   "no stamp → sending.sh called" \
     "1" "$(grep -c . "$SENDING_LOG" 2>/dev/null || echo 0)"
lack "no stamp → log does NOT say skipped" "base unchanged" "$out_nostamp"
# Stamp was written by the run above (stamp update loop). Reuse the same SPIRA_RUN.
rm -f "$SENDING_LOG"
out_stamp="$(run_pass "$_sr" SPIRA_FAYTHS=)"
is   "stamp matches → sending.sh not called" \
     "0" "$(grep -c . "$SENDING_LOG" 2>/dev/null || echo 0)"
want "stamp matches → log says skipped" "base unchanged" "$out_stamp"

# ======================================================================================
echo
echo "log order — CHECK7 appears before sending in one pass:"
# ======================================================================================
# Reuse the run-send dir (stamp from sending tests is still valid): the Sending logs
# "sending: base unchanged — skipped", giving a "sending:" line to order against CHECK7.
# Using "sending:" (with colon) avoids matching the "finishing the sending" line
# emitted earlier by the goal-reached path.
order_out="$(run_pass "$_sr" \
    SPIRA_FAYTHS=builder SPIRA_SCOPE_LABEL= SPIRA_MAX_AEONS=1)"
want   "CHECK7 line appears in log"   "CHECK7"   "$order_out"
want   "sending: line appears in log" "sending:" "$order_out"
before "CHECK7" "sending:" "$order_out"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
