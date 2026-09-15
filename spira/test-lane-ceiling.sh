#!/usr/bin/env bash
#
# test-lane-ceiling.sh — a lane fayth may not take the last fleet slot while any task
#   fayth has ready work.
#
#   ./test-lane-ceiling.sh
#
# ACCEPTANCE CRITERIA (bead sp-f342):
#   1. REFUSED: fleet N, N-1 live, task fayth has ready work → lane summon refused
#   2. POSITIVE CONTROL: same fleet state, no task work ready → lane succeeds
#   3. TWO FREE SLOTS: lane succeeds even with task fayth ready (rule binds last slot only)
#   4. TASK FAYTH: a task persona with 1 slot free is never refused by this rule
#
# WHAT DISTINGUISHES THIS FROM ELASTIC CEILING. The elastic reservation (test-elastic-ceiling.sh)
# holds back elastic task personas. This rule holds back LANE fayths — personas that draw
# from their own FAYTH_MAX_CONCURRENT rather than from SPIRA_MAX_AEONS. The two checks are
# independent: an elastic persona that is also a lane fayth (unusual) is subject to both.
#
# covers: spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()    { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()   { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "${2:-}"; }
is()    { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want()  { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/run" "$T/chamber" "$T/bin"

# A MINIMAL ENVIRONMENT, non-default everywhere so that assertions cannot pass by reading
# literals out of conf.sh (law-gates-run-in-a-clean-environment).
export SPIRA_RUN="$T/run"
export SPIRA_CONF="$T/no-such.conf"
export SPIRA_HOME="$T"
export SPIRA_DB="$T/no-db"

. "$HERE/lib.sh"

# ======================================================================================
# TWO SYNTHETIC FAYTHS: one lane persona, one task persona.
# Neither is named "ops" or "builder" — the rule must bind to the declaration, not the
# name. FAYTH_LANE identifies the lane; absence of it (with FAYTH_ELASTIC absent too)
# identifies the fixed task fayth.
# ======================================================================================
cat > "$T/chamber/sentinel.fayth" <<'F'
FAYTH_NAME=sentinel
FAYTH_LABELS="test,incident"
FAYTH_EXCLUDE_LABELS="test-poison"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=60
FAYTH_LANE=watch
F

cat > "$T/chamber/worker.fayth" <<'F'
FAYTH_NAME=worker
FAYTH_LABELS="test,plan"
FAYTH_EXCLUDE_LABELS="test-poison"
FAYTH_MAX_CONCURRENT=4
FAYTH_HEARTBEAT_SECONDS=120
F

export SPIRA_FAYTHS="worker sentinel"

# ======================================================================================
# STUBS. No real database, no real systemd, no real process table.
# ======================================================================================
MOCK_LIVE=0
aeons_live_total() { printf '%d' "$MOCK_LIVE"; }
aeon_count()       { printf '0'; }
capacity_paused()  { return 1; }   # no outage

MOCK_READY_sentinel=0
MOCK_READY_worker=0
fayth_ready() {
    local _n; _n="MOCK_READY_${1}"
    printf '%d' "${!_n:-0}"
}

SUMMONED="$T/summoned.log"
cat > "$T/bin/mock-summon" <<'MOCK'
#!/usr/bin/env bash
fayth="${@: -1}"
[ "$fayth" = "--dry-run" ] && fayth="${@: -2:1}"
printf 'SUMMONED:%s\n' "$fayth" >> "$SUMMONED_FILE"
exit 0
MOCK
chmod +x "$T/bin/mock-summon"
export SPIRA_SUMMON="$T/bin/mock-summon"
export SUMMONED_FILE="$SUMMONED"

# Fleet ceiling: 3 slots total.
export SPIRA_MAX_LIVE_AEONS=3

# ======================================================================================
echo
echo "criterion 1 — lane refused when last slot and task fayth has ready work"
# ======================================================================================
# N=3, N-1=2 live, 1 slot free. worker (task fayth) has ready work.
# sentinel (lane) must be refused; worker would succeed.
#
# POSITIVE CONTROL runs first so "sentinel refused" cannot pass by a broken summon path
# that refuses everything.

# Positive control: with 2 free slots, sentinel IS summoned despite worker having ready work.
MOCK_LIVE=1     # 3-1=2 free slots
MOCK_READY_worker=1
MOCK_READY_sentinel=1
rm -f "$SUMMONED"
summon_fayth sentinel >/dev/null 2>&1 || true
is "positive: lane succeeds with 2 free slots (worker also ready)" \
   "SUMMONED:sentinel" "$(cat "$SUMMONED" 2>/dev/null)"

# Now set N-1=2 live → exactly 1 slot free. worker has work → sentinel must be refused.
MOCK_LIVE=2     # 3-2=1 free slot
MOCK_READY_worker=1
MOCK_READY_sentinel=1
rm -f "$SUMMONED"
summon_fayth sentinel >/dev/null 2>&1 || true
is "criterion 1: lane refused when last slot and task fayth has ready work" \
   "absent" "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"

# Verify the log names the reason.
MOCK_LIVE=2
MOCK_READY_worker=1
MOCK_READY_sentinel=1
rm -f "$SUMMONED"
log_out="$(summon_fayth sentinel 2>&1 || true)"
want   "log says '1 fleet slot remaining'"    "1 fleet slot remaining"    "$log_out"
want   "log says 'held back for task work'"   "held back for task work"   "$log_out"
nowant "log says 'at concurrency cap'"        "at concurrency cap"        "$log_out"

# ======================================================================================
echo
echo "criterion 2 — positive control: lane succeeds when no task fayth has ready work"
# ======================================================================================
# Same fleet state (N-1 live, 1 slot free), but worker has NO ready work.
# sentinel must be summoned.
MOCK_LIVE=2     # 1 slot free
MOCK_READY_worker=0
MOCK_READY_sentinel=1
rm -f "$SUMMONED"
summon_fayth sentinel >/dev/null 2>&1 || true
is "criterion 2: lane succeeds when no task fayth has ready work (positive control)" \
   "SUMMONED:sentinel" "$(cat "$SUMMONED" 2>/dev/null)"

# ======================================================================================
echo
echo "criterion 3 — two free slots: lane succeeds even with task fayth ready"
# ======================================================================================
# N=3, 1 live → 2 free slots. The rule binds only the last slot.
MOCK_LIVE=1     # 3-1=2 free slots
MOCK_READY_worker=1
MOCK_READY_sentinel=1
rm -f "$SUMMONED"
summon_fayth sentinel >/dev/null 2>&1 || true
is "criterion 3: lane succeeds with 2 free slots despite task fayth ready" \
   "SUMMONED:sentinel" "$(cat "$SUMMONED" 2>/dev/null)"

# ======================================================================================
echo
echo "criterion 4 — task persona is never refused by this rule"
# ======================================================================================
# Fleet at exactly 1 free slot. worker (task) must succeed regardless.
MOCK_LIVE=2     # 1 slot free
MOCK_READY_worker=1
MOCK_READY_sentinel=1
rm -f "$SUMMONED"
summon_fayth worker >/dev/null 2>&1 || true
is "criterion 4: task persona succeeds with 1 slot free" \
   "SUMMONED:worker" "$(cat "$SUMMONED" 2>/dev/null)"

# ======================================================================================
echo
echo "no ceiling — SPIRA_MAX_LIVE_AEONS unset means today's behaviour exactly"
# ======================================================================================
unset SPIRA_MAX_LIVE_AEONS
MOCK_LIVE=999
MOCK_READY_worker=1
MOCK_READY_sentinel=1
rm -f "$SUMMONED"
summon_fayth sentinel >/dev/null 2>&1 || true
is "no ceiling: lane succeeds when SPIRA_MAX_LIVE_AEONS is unset" \
   "SUMMONED:sentinel" "$(cat "$SUMMONED" 2>/dev/null)"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
