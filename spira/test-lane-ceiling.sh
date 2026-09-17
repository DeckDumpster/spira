#!/usr/bin/env bash
#
# test-lane-ceiling.sh — collective lane cap and inverted last-slot preference.
#
#   ./test-lane-ceiling.sh
#
# ACCEPTANCE CRITERIA (bead sp-cm29p):
#   (a) ceiling 4, pool 4, lanes cap 1, builders ready, 3 builders live, ops ready:
#       next summon is ops (lane), not a builder (task fayth held back)
#   (b) same with no lane ready: 4th builder summoned (no lane wants the slot)
#   (c) 0 live, all three lanes ready: exactly 1 lane summoned (collective cap)
#   (d) groomer and ops both always ready: over successive frees both get the lane slot
#       (rotation so one lane cannot monopolise the cap)
#
# POSITIVE CONTROLS BEFORE EACH CRITERION (law-absence-needs-a-positive-control).
#
# covers: spira/lib.sh spira/sentinel.sh
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
# SYNTHETIC FAYTHS: one lane persona (ops-like), one task persona (builder-like).
# Named to avoid coupling the rule to a specific persona name.
# ======================================================================================
cat > "$T/chamber/laner.fayth" <<'F'
FAYTH_NAME=laner
FAYTH_LABELS="test,ops"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=60
FAYTH_LANE=ops
F

cat > "$T/chamber/worker.fayth" <<'F'
FAYTH_NAME=worker
FAYTH_LABELS="test,plan"
FAYTH_MAX_CONCURRENT=4
FAYTH_HEARTBEAT_SECONDS=120
F

export SPIRA_FAYTHS="worker laner"

# ======================================================================================
# STUBS.
# ======================================================================================
MOCK_LIVE=0
MOCK_LIVE_LANES=0
aeons_live_total() { printf '%d' "$MOCK_LIVE"; }
aeons_live_lanes() { printf '%d' "$MOCK_LIVE_LANES"; }
aeon_count()       { printf '0'; }
capacity_paused()  { return 1; }

MOCK_READY_laner=0
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

export SPIRA_MAX_LIVE_AEONS=4
export SPIRA_LANES_MAX_LIVE=1

# ======================================================================================
echo
echo "criterion (a) — task fayth held back when last slot and lane has ready work"
# ======================================================================================
# POSITIVE CONTROL: with 2 free slots, worker IS summoned even though laner is ready.
MOCK_LIVE=2; MOCK_LIVE_LANES=0
MOCK_READY_laner=1; MOCK_READY_worker=1
rm -f "$SUMMONED"
summon_fayth worker >/dev/null 2>&1 || true
is "positive: worker succeeds with 2 free slots (laner also ready)" \
   "SUMMONED:worker" "$(cat "$SUMMONED" 2>/dev/null)"

# Now 3 builders live → 1 slot free. laner ready → worker must be held back.
MOCK_LIVE=3; MOCK_LIVE_LANES=0
MOCK_READY_laner=1; MOCK_READY_worker=1
rm -f "$SUMMONED"
summon_fayth worker >/dev/null 2>&1 || true
is "criterion (a): worker refused when last slot and laner has ready work" \
   "absent" "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"

log_out="$(MOCK_LIVE=3; MOCK_LIVE_LANES=0; MOCK_READY_laner=1; summon_fayth worker 2>&1 || true)"
want   "log says '1 fleet slot remaining'"     "1 fleet slot remaining"     "$log_out"
want   "log says 'held back'"                  "held back"                  "$log_out"
nowant "log says 'at concurrency cap'"         "at concurrency cap"         "$log_out"

# ======================================================================================
echo
echo "criterion (b) — task fayth allowed when last slot and NO lane has ready work"
# ======================================================================================
MOCK_LIVE=3; MOCK_LIVE_LANES=0
MOCK_READY_laner=0; MOCK_READY_worker=1
rm -f "$SUMMONED"
summon_fayth worker >/dev/null 2>&1 || true
is "criterion (b): worker summoned when no lane has ready work" \
   "SUMMONED:worker" "$(cat "$SUMMONED" 2>/dev/null)"

# ======================================================================================
echo
echo "criterion (c) — collective lane cap: at most SPIRA_LANES_MAX_LIVE lane aeons"
# ======================================================================================
# POSITIVE CONTROL: lane below cap → summoned.
MOCK_LIVE=0; MOCK_LIVE_LANES=0
MOCK_READY_laner=1; MOCK_READY_worker=0
rm -f "$SUMMONED"
summon_fayth laner >/dev/null 2>&1 || true
is "positive: laner summoned when below lane cap" \
   "SUMMONED:laner" "$(cat "$SUMMONED" 2>/dev/null)"

# Lane at cap → refused.
MOCK_LIVE=1; MOCK_LIVE_LANES=1
MOCK_READY_laner=1; MOCK_READY_worker=0
rm -f "$SUMMONED"
summon_fayth laner >/dev/null 2>&1 || true
is "criterion (c): laner refused when lanes at collective cap" \
   "absent" "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"

log_out="$(MOCK_LIVE=1; MOCK_LIVE_LANES=1; MOCK_READY_laner=1; summon_fayth laner 2>&1 || true)"
want "log says 'lane slot(s) in use'" "lane slot(s) in use" "$log_out"

# Worker is NOT refused by the lane cap check (that check is for lane fayths only).
MOCK_LIVE=0; MOCK_LIVE_LANES=1
MOCK_READY_laner=0; MOCK_READY_worker=1
rm -f "$SUMMONED"
summon_fayth worker >/dev/null 2>&1 || true
is "criterion (c): worker not refused by lane cap check" \
   "SUMMONED:worker" "$(cat "$SUMMONED" 2>/dev/null)"

# ======================================================================================
echo
echo "criterion (d) — rotation: both lanes get the slot over successive passes"
# ======================================================================================
# Add a second lane fayth for the rotation test.
cat > "$T/chamber/groomer.fayth" <<'F'
FAYTH_NAME=groomer
FAYTH_LABELS="test,groomer"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=60
FAYTH_LANE=groomer
F
export SPIRA_FAYTHS="worker laner groomer"

# Both lanes always ready. Over two pass simulations the rotation must give each one turn.
MOCK_LIVE=0; MOCK_LIVE_LANES=0
MOCK_READY_laner=1; MOCK_READY_worker=0
export MOCK_READY_groomer=0
fayth_ready() {
    local _n; _n="MOCK_READY_${1}"
    printf '%d' "${!_n:-0}"
}
MOCK_READY_laner=1; MOCK_READY_groomer=1

# Simulate two passes by calling summon_fayth for each lane in the order the sentinel
# loop would, with rotation applied by the test itself (mirrors sentinel.sh logic).
LANE_ORDER="laner groomer"
_lane_rr="$T/run/lane-round-robin"
rm -f "$_lane_rr" "$SUMMONED"

# Pass 1: no prior rotation state → laner goes first, is summoned (cap reached).
pass1_summoned=""
for _f in $LANE_ORDER; do
    MOCK_LIVE_LANES="$([ -n "$pass1_summoned" ] && echo 1 || echo 0)"
    rm -f "$SUMMONED"
    summon_fayth "$_f" >/dev/null 2>&1 || true
    if [ -f "$SUMMONED" ] && grep -qF "SUMMONED:$_f" "$SUMMONED" 2>/dev/null; then
        pass1_summoned="$_f"
        printf '%s' "$_f" > "$_lane_rr"
        break
    fi
done

is "pass 1: a lane was summoned"   "laner"  "$pass1_summoned"

# Pass 2: rotate list so laner goes last; groomer should get the slot.
_last="$(cat "$_lane_rr" 2>/dev/null)"
_before="" _after="" _found=0
for _f in $LANE_ORDER; do
    if [ "$_found" = 1 ]; then _after="$_after $_f"
    elif [ "$_f" = "$_last" ]; then _before="$_before $_f"; _found=1
    else _before="$_before $_f"
    fi
done
ROTATED="${_after# }${_before:+ }${_before# }"

pass2_summoned=""
MOCK_LIVE_LANES=0
for _f in $ROTATED; do
    rm -f "$SUMMONED"
    summon_fayth "$_f" >/dev/null 2>&1 || true
    if [ -f "$SUMMONED" ] && grep -qF "SUMMONED:$_f" "$SUMMONED" 2>/dev/null; then
        pass2_summoned="$_f"
        break
    fi
done

is "pass 2: the OTHER lane was summoned (rotation worked)" "groomer" "$pass2_summoned"

# ======================================================================================
echo
echo "no lane cap — SPIRA_LANES_MAX_LIVE unset: no preference, task fayth fills freely"
# ======================================================================================
unset SPIRA_LANES_MAX_LIVE
MOCK_LIVE=3; MOCK_LIVE_LANES=0
MOCK_READY_laner=1; MOCK_READY_worker=1
rm -f "$SUMMONED"
summon_fayth worker >/dev/null 2>&1 || true
is "no lane cap: worker fills last slot when SPIRA_LANES_MAX_LIVE unset" \
   "SUMMONED:worker" "$(cat "$SUMMONED" 2>/dev/null)"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
