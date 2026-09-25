#!/usr/bin/env bash
#
# test-summon-fayth.sh — summon_fayth's whole refusal ladder, and fayth_free's arithmetic:
#   one lib.sh source and one stub set instead of six (D2/D3, sp-9ce60.2.1).
#
#   ./test-summon-fayth.sh
#
# HOST OF THE MERGE: test-elastic-ceiling.sh, whose counting fayth_ready stub is the
# template every section below reuses. Absorbed here, each losing nothing but its own
# lib.sh-and-stub boilerplate:
#   - test-elastic-ceiling.sh — all rows, minus criterion 3 (a verbatim duplicate of
#     criterion 1's own positive control)
#   - test-lane-ceiling.sh — criteria (a)(b)(c) and their controls, plus (d) rotation,
#     rewritten to call the real lane_rotate (extracted from sentinel.sh's CHECK 7
#     ordering into lib.sh) instead of reimplementing it — closes gap G1 (sp-9ce60.5)
#   - test-drain-expiry.sh — every row, including the world.sh writer/reader seam
#   - test-fayth-free.sh — every row but its own vacuous inline-arithmetic "positive
#     control" (G18), replaced below with a row that calls fayth_free directly with
#     `have` decoupled from `pool`
#   - the summon/free/escape rows of test-lanes.sh (its roster-split and ops.fayth-lane
#     rows stay behind — D5's target is test-fayth.sh, sp-9ce60.2.2)
#   - the CPUQuota rows of test-fayth.sh (UC-dispatch-23)
#
# NEW GAP ROWS (docs/test-plan/dispatch.md section 6):
#   G5 — the halt gate, previously only source-grepped (test-world.sh)
#   G6 — the plain fleet-ceiling refusal; only the last-slot rules had a test
#   G7 — governor withholding: MOOT. spira/governor.sh was deleted from main (sp-8mzsh)
#        before this suite was written, and neither fayth_free nor summon_fayth carries
#        a governor hook left to test against — there is nothing to write a row for.
#   G8 — escape.sh and world.halted: a CHARACTERIZATION row of current behaviour
#        (escape.sh bypasses the halt gate) per sp-6rv05's stated default. That decision
#        bead is still open; if it resolves the other way, the fix belongs to escape.sh
#        itself and this row becomes the regression test for it.
#
# POSITIVE CONTROLS BEFORE EACH REFUSAL (law-absence-needs-a-positive-control).
#
# tier: T1
# covers: spira/lib.sh spira/escape.sh spira/world.sh UC-dispatch-09 UC-dispatch-10 UC-dispatch-11 UC-dispatch-12 UC-dispatch-15 UC-dispatch-23
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

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
# THE SHARED STUB SET. No real database, no real systemd, no real process table.
# ======================================================================================
MOCK_LIVE=0; MOCK_LIVE_LANES=0; MOCK_COUNT=0
aeons_live_total() { printf '%d' "$MOCK_LIVE"; }
aeons_live_lanes() { printf '%d' "$MOCK_LIVE_LANES"; }
aeon_count()       { printf '%d' "$MOCK_COUNT"; }
capacity_paused()  { return 1; }   # no outage

# Per-fayth ready counts, keyed by name; default 0. Calls are appended to a file rather
# than counted in a variable: fayth_ready runs inside a $() subshell, so a variable
# increment there is invisible to the parent (test-elastic-ceiling's original template).
FAYTH_READY_CALL_FILE="$T/fayth-ready-calls"
fayth_ready() {
    printf '%s\n' "$1" >> "$FAYTH_READY_CALL_FILE"
    local _n; _n="MOCK_READY_${1}"
    printf '%d' "${!_n:-0}"
}
fayth_ready_call_count() { grep -c . "$FAYTH_READY_CALL_FILE" 2>/dev/null || printf '0'; }
reset_call_count() { : > "$FAYTH_READY_CALL_FILE"; }

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

# ======================================================================================
# FOUR SYNTHETIC FAYTHS, none named "builder" or "ops" — every rule below must bind to a
# declaration (FAYTH_ELASTIC, FAYTH_LANE), never to a persona's name.
# ======================================================================================
cat > "$T/chamber/stretchy.fayth" <<'F'
FAYTH_NAME=stretchy
FAYTH_LABELS="test,plan"
FAYTH_EXCLUDE_LABELS="test-poison"
FAYTH_MAX_CONCURRENT=4
FAYTH_ELASTIC=1
FAYTH_HEARTBEAT_SECONDS=120
F

cat > "$T/chamber/anchor.fayth" <<'F'
FAYTH_NAME=anchor
FAYTH_LABELS="test,incident"
FAYTH_EXCLUDE_LABELS="test-poison"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=60
F

cat > "$T/chamber/tasker.fayth" <<'F'
FAYTH_NAME=tasker
FAYTH_LABELS="test,plan"
FAYTH_MAX_CONCURRENT=4
FAYTH_HEARTBEAT_SECONDS=120
F

cat > "$T/chamber/laner.fayth" <<'F'
FAYTH_NAME=laner
FAYTH_LABELS="test,ops"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=60
FAYTH_LANE=ops
F

# ======================================================================================
echo
echo "elastic last-slot reservation — criterion 1: refused when last slot and non-elastic has ready work"
# ======================================================================================
export SPIRA_FAYTHS="anchor stretchy"
export SPIRA_MAX_LIVE_AEONS=3
unset SPIRA_LANES_MAX_LIVE 2>/dev/null || true

# POSITIVE CONTROL: with 2 free slots, stretchy IS summoned despite anchor being ready.
MOCK_LIVE=1     # 3-1=2 free slots
MOCK_READY_anchor=1
MOCK_READY_stretchy=1
rm -f "$SUMMONED"
summon_fayth stretchy 4 >/dev/null 2>&1 || true
is "positive: elastic succeeds with 2 free slots (anchor also ready)" \
   "SUMMONED:stretchy" "$(cat "$SUMMONED" 2>/dev/null)"

# N-1=2 live -> exactly 1 slot free. anchor has work -> stretchy must be refused.
MOCK_LIVE=2
rm -f "$SUMMONED"
summon_fayth stretchy 4 >/dev/null 2>&1 || true
is "criterion 1: elastic refused when last slot and non-elastic has ready work" \
   "absent" "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"

log_out="$(summon_fayth stretchy 4 2>&1 || true)"
want   "log names 'held back'"        "held back" "$log_out"
want   "log names the reserved fayth" "anchor"    "$log_out"
nowant "log says 'at concurrency cap'" "at concurrency cap" "$log_out"

echo
echo "criterion 2 — positive control: elastic succeeds when no non-elastic has ready work"
MOCK_LIVE=2
MOCK_READY_anchor=0
MOCK_READY_stretchy=1
rm -f "$SUMMONED"
summon_fayth stretchy 4 >/dev/null 2>&1 || true
is "criterion 2: elastic succeeds when no non-elastic has work" \
   "SUMMONED:stretchy" "$(cat "$SUMMONED" 2>/dev/null)"

echo
echo "criterion 4 — non-elastic persona is never refused by this rule"
MOCK_LIVE=2
MOCK_READY_anchor=1
MOCK_READY_stretchy=1
rm -f "$SUMMONED"
summon_fayth anchor >/dev/null 2>&1 || true
is "criterion 4: non-elastic persona succeeds with 1 slot free" \
   "SUMMONED:anchor" "$(cat "$SUMMONED" 2>/dev/null)"

echo
echo "cost — fayth_ready is called for non-elastic personas only when the last slot is contested"
reset_call_count
MOCK_LIVE=2
rm -f "$SUMMONED"
summon_fayth stretchy 4 >/dev/null 2>&1 || true
is "cost: refused case makes exactly 1 fayth_ready call (anchor reservation only)" \
   "1" "$(fayth_ready_call_count)"
is "cost: that call was for anchor" "anchor" "$(cat "$FAYTH_READY_CALL_FILE" 2>/dev/null)"

reset_call_count
MOCK_LIVE=1     # 2 free slots
rm -f "$SUMMONED"
summon_fayth stretchy 4 >/dev/null 2>&1 || true
is "cost: 2-free-slot path makes 1 fayth_ready call (stretchy only, no reservation)" \
   "1" "$(fayth_ready_call_count)"
is "cost: that call was for stretchy" "stretchy" "$(cat "$FAYTH_READY_CALL_FILE" 2>/dev/null)"

echo
echo "no ceiling — SPIRA_MAX_LIVE_AEONS unset means today's behaviour exactly"
unset SPIRA_MAX_LIVE_AEONS
MOCK_LIVE=999
rm -f "$SUMMONED"
summon_fayth stretchy 4 >/dev/null 2>&1 || true
is "no ceiling: elastic succeeds when SPIRA_MAX_LIVE_AEONS is unset" \
   "SUMMONED:stretchy" "$(cat "$SUMMONED" 2>/dev/null)"

# ======================================================================================
echo
echo "lane ceiling (a) — a non-lane task fayth is held back when last slot and a lane has ready work"
# ======================================================================================
export SPIRA_FAYTHS="tasker laner"
export SPIRA_MAX_LIVE_AEONS=4
export SPIRA_LANES_MAX_LIVE=1

# POSITIVE CONTROL: with 2 free slots, tasker IS summoned even though laner is ready.
MOCK_LIVE=2; MOCK_LIVE_LANES=0
MOCK_READY_laner=1; MOCK_READY_tasker=1
rm -f "$SUMMONED"
summon_fayth tasker >/dev/null 2>&1 || true
is "positive: tasker succeeds with 2 free slots (laner also ready)" \
   "SUMMONED:tasker" "$(cat "$SUMMONED" 2>/dev/null)"

# 3 live -> 1 slot free. laner ready -> tasker must be held back.
MOCK_LIVE=3; MOCK_LIVE_LANES=0
rm -f "$SUMMONED"
summon_fayth tasker >/dev/null 2>&1 || true
is "(a): tasker refused when last slot and laner has ready work" \
   "absent" "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"

log_out="$(summon_fayth tasker 2>&1 || true)"
want   "log says '1 fleet slot remaining'" "1 fleet slot remaining" "$log_out"
want   "log says 'held back'"              "held back"              "$log_out"
nowant "log says 'at concurrency cap'"     "at concurrency cap"     "$log_out"

echo
echo "lane ceiling (b) — the task fayth is allowed when last slot and no lane has ready work"
MOCK_LIVE=3; MOCK_LIVE_LANES=0
MOCK_READY_laner=0; MOCK_READY_tasker=1
rm -f "$SUMMONED"
summon_fayth tasker >/dev/null 2>&1 || true
is "(b): tasker summoned when no lane has ready work" \
   "SUMMONED:tasker" "$(cat "$SUMMONED" 2>/dev/null)"

echo
echo "lane ceiling (c) — collective lane cap: at most SPIRA_LANES_MAX_LIVE lane aeons"
# POSITIVE CONTROL: lane below cap -> summoned.
MOCK_LIVE=0; MOCK_LIVE_LANES=0
MOCK_READY_laner=1; MOCK_READY_tasker=0
rm -f "$SUMMONED"
summon_fayth laner >/dev/null 2>&1 || true
is "positive: laner summoned when below lane cap" \
   "SUMMONED:laner" "$(cat "$SUMMONED" 2>/dev/null)"

MOCK_LIVE=1; MOCK_LIVE_LANES=1
rm -f "$SUMMONED"
summon_fayth laner >/dev/null 2>&1 || true
is "(c): laner refused when lanes at collective cap" \
   "absent" "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"

log_out="$(summon_fayth laner 2>&1 || true)"
want "log says 'lane slot(s) in use'" "lane slot(s) in use" "$log_out"

# The lane cap check is for lane fayths only; a task fayth is not refused by it.
MOCK_LIVE=0; MOCK_LIVE_LANES=1
MOCK_READY_laner=0; MOCK_READY_tasker=1
rm -f "$SUMMONED"
summon_fayth tasker >/dev/null 2>&1 || true
is "(c): tasker not refused by the lane cap check" \
   "SUMMONED:tasker" "$(cat "$SUMMONED" 2>/dev/null)"

echo
echo "no lane cap — SPIRA_LANES_MAX_LIVE unset: no preference, the task fayth fills freely"
unset SPIRA_LANES_MAX_LIVE
MOCK_LIVE=3; MOCK_LIVE_LANES=0
MOCK_READY_laner=1; MOCK_READY_tasker=1
rm -f "$SUMMONED"
summon_fayth tasker >/dev/null 2>&1 || true
is "no lane cap: tasker fills last slot when SPIRA_LANES_MAX_LIVE unset" \
   "SUMMONED:tasker" "$(cat "$SUMMONED" 2>/dev/null)"

# ======================================================================================
echo
echo "lane rotation (G1) — sentinel.sh's CHECK 7 ordering, via the real lane_rotate,"
echo "  not a copy reimplemented in the test (sp-9ce60.5, closing test-lane-ceiling.sh's"
echo "  gap: (d) rotation used to mirror sentinel.sh's logic by hand rather than call it)"
# ======================================================================================
cat > "$T/chamber/groomer.fayth" <<'F'
FAYTH_NAME=groomer
FAYTH_LABELS="test,groomer"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=60
FAYTH_LANE=groomer
F
export SPIRA_FAYTHS="tasker laner groomer"
export SPIRA_MAX_LIVE_AEONS=4
export SPIRA_LANES_MAX_LIVE=1

# Both lanes always ready. Over two simulated passes the rotation must give each one turn,
# calling summon_fayth for each lane in the order the sentinel loop would, with rotation
# applied through the real lane_rotate.
MOCK_LIVE=0; MOCK_LIVE_LANES=0
MOCK_READY_laner=1; MOCK_READY_groomer=1; MOCK_READY_tasker=0
LANE_ORDER="laner groomer"
_lane_rr="$T/run/lane-round-robin"
rm -f "$_lane_rr" "$SUMMONED"

# Pass 1: no prior rotation state -> laner goes first, is summoned (cap reached).
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
is "rotation pass 1: a lane was summoned" "laner" "$pass1_summoned"

# Pass 2: rotate through the real lane_rotate so laner goes last; groomer gets the slot.
_last="$(cat "$_lane_rr" 2>/dev/null)"
is "lane_rotate: laner (last-summoned) moves to the end" \
   "groomer laner" "$(lane_rotate "$_last" $LANE_ORDER)"
ROTATED="$(lane_rotate "$_last" $LANE_ORDER)"
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
is "rotation pass 2: the OTHER lane was summoned (rotation worked)" "groomer" "$pass2_summoned"

# lane_rotate edge cases beyond the rotation exercised above.
is "lane_rotate: no prior last — order unchanged" \
   "laner groomer" "$(lane_rotate "" laner groomer)"
is "lane_rotate: last not among lanes — order unchanged" \
   "laner groomer" "$(lane_rotate "nosuchlane" laner groomer)"
is "lane_rotate: single lane — unchanged regardless of last" \
   "laner" "$(lane_rotate "laner" laner)"
is "lane_rotate: no lanes at all — empty" \
   "" "$(lane_rotate "laner")"

unset SPIRA_MAX_LIVE_AEONS SPIRA_LANES_MAX_LIVE 2>/dev/null || true

# ======================================================================================
echo
echo "drain expiry — positive control: with no drain at all, the fayth IS summoned"
# ======================================================================================
export SPIRA_DRAIN_TTL=1800
MOCK_READY_stretchy=1
DRAIN_STAMP="$T/run/world.draining"
try_drain() { rm -f "$SUMMONED"; summon_fayth stretchy 1 2>&1; }

rm -f "$DRAIN_STAMP"
out="$(try_drain)"
is "no drain: stretchy is summoned" "SUMMONED:stretchy" "$(cat "$SUMMONED" 2>/dev/null)"

echo
echo "drain expiry — a LIVE drain still gates"
{ echo "now"; echo "gated"; printf 'expires %s\n' "$(( $(date +%s) + 3600 ))"; } > "$DRAIN_STAMP"
out="$(try_drain)"
is "live drain: nothing summoned" "" "$(cat "$SUMMONED" 2>/dev/null)"
want "and it says it is draining" "draining — not summoning" "$out"
[ -f "$DRAIN_STAMP" ] && ok "the stamp survives a live drain" || bad "the stamp survives a live drain" "it was removed"

echo
echo "drain expiry — an EXPIRED drain is lifted, loudly, and the stamp is removed"
{ echo "now"; echo "gated"; printf 'expires %s\n' "$(( $(date +%s) - 60 ))"; } > "$DRAIN_STAMP"
out="$(try_drain)"
is "expired drain: stretchy IS summoned" "SUMMONED:stretchy" "$(cat "$SUMMONED" 2>/dev/null)"
want "the lift is loud"             "DRAIN EXPIRED"  "$out"
want "and names the missing resume" "did not resume" "$out"
[ -f "$DRAIN_STAMP" ] && bad "the stamp is removed" "it is still there" || ok "the stamp is removed"

echo
echo "drain expiry — a stamp with NO expires line falls back to mtime + TTL"
{ echo "now"; echo "gated"; } > "$DRAIN_STAMP"
out="$(try_drain)"
is "no expires line, fresh mtime: still gated" "" "$(cat "$SUMMONED" 2>/dev/null)"

{ echo "now"; echo "gated"; } > "$DRAIN_STAMP"
touch -d "@$(( $(date +%s) - SPIRA_DRAIN_TTL - 60 ))" "$DRAIN_STAMP"
out="$(try_drain)"
is "no expires line, mtime past TTL: lifted" "SUMMONED:stretchy" "$(cat "$SUMMONED" 2>/dev/null)"
want "and it is loud about that too" "DRAIN EXPIRED" "$out"

echo
echo "drain expiry — the seam: world.sh drain WRITES the expiry, and --for sets it"
rm -f "$DRAIN_STAMP"
SPIRA_RUN="$SPIRA_RUN" bash "$HERE/world.sh" drain --for 900 --timeout 1 >/dev/null 2>&1
if [ -f "$DRAIN_STAMP" ]; then
    exp="$(sed -n 's/^expires \([0-9][0-9]*\)$/\1/p' "$DRAIN_STAMP" | head -1)"
    if [ -n "$exp" ]; then
        ok "world.sh drain writes an expires line"
        d=$(( exp - $(date +%s) ))
        if [ "$d" -gt 840 ] && [ "$d" -le 900 ]; then
            ok "--for 900 sets the deadline (${d}s out)"
        else
            bad "--for 900 sets the deadline" "expected ~900s out, got ${d}s"
        fi
    else
        bad "world.sh drain writes an expires line" "no expires line in the stamp"
    fi
else
    bad "world.sh drain writes a stamp" "no stamp at $DRAIN_STAMP"
fi
rm -f "$DRAIN_STAMP"

# ======================================================================================
echo
echo "fayth_free — an elastic persona takes the pool remainder whole, never subtracting running twice"
# ======================================================================================
for n in 0 1 2 3 4 5; do
    pool=$((5 - n))
    MOCK_COUNT=$n
    got="$(fayth_free stretchy "$pool")"
    is "elastic n=$n, pool=$pool -> free=$pool" "$pool" "$got"
done

# THE ROW THAT REPLACES fayth-free's OWN VACUOUS "buggy formula" ARITHMETIC-ONLY CONTROL
# (G18): `have` and `pool` are set INDEPENDENTLY here, rather than tied together as
# pool=BASE-have above, so the old bug — subtracting the running count a second time from
# a number that already nets it out — is visible in one direct call rather than only by
# pattern across the loop.
MOCK_COUNT=3
got="$(fayth_free stretchy 5)"
is "elastic: have=3 does not reduce a pool=5 remainder (max-have-on-a-remainder bug)" "5" "$got"

echo
echo "fayth_free — no pool means fall back to the persona's own cap"
cap="$(fayth_get stretchy FAYTH_MAX_CONCURRENT 1)"
for n in 0 1; do
    MOCK_COUNT=$n
    got="$(fayth_free stretchy)"
    want_val=$((cap - n))
    is "no pool, n=$n -> free=$want_val (cap=$cap)" "$want_val" "$got"
done

echo
echo "fayth_free — a non-elastic persona: the running count is subtracted from its own cap"
anchor_cap="$(fayth_get anchor FAYTH_MAX_CONCURRENT 1)"
for n in 0 1; do
    MOCK_COUNT=$n
    got="$(fayth_free anchor "10")"
    want_val=$((anchor_cap - n))
    is "anchor, pool=10, n=$n -> free=$want_val" "$want_val" "$got"
done

echo
echo "fayth_free — a non-elastic lane fayth without pool: cap minus running, clamped at 0"
MOCK_COUNT=0
is "laner: fayth_free without pool returns cap-running" "1" "$(fayth_free laner)"
MOCK_COUNT=1
is "laner at cap returns 0 free" "0" "$(fayth_free laner)"
MOCK_COUNT=0

# ======================================================================================
echo
echo "summon_fayth — SPIRA_AEON_CPU_QUOTA is passed to the summon command"
# ======================================================================================
ARGS_FILE="$T/summon-args"
MOCK_QUOTA="$T/bin/mock-quota"
printf '#!/bin/sh\nprintf "%%s\\n" "$@" > "%s"\nexit 0\n' "$ARGS_FILE" > "$MOCK_QUOTA"
chmod +x "$MOCK_QUOTA"
export SPIRA_SUMMON="$MOCK_QUOTA"
MOCK_READY_stretchy=1

unset SPIRA_AEON_CPU_QUOTA 2>/dev/null || true
rm -f "$ARGS_FILE"
summon_fayth stretchy >/dev/null 2>&1 || true
args="$(cat "$ARGS_FILE" 2>/dev/null)"
want "default quota: CPUQuota=70% appears in args" "CPUQuota=70%" "$args"

export SPIRA_AEON_CPU_QUOTA=90
rm -f "$ARGS_FILE"
summon_fayth stretchy >/dev/null 2>&1 || true
args="$(cat "$ARGS_FILE" 2>/dev/null)"
want   "custom quota: CPUQuota=90% appears in args"     "CPUQuota=90%" "$args"
nowant "custom quota: CPUQuota=70% is absent from args" "CPUQuota=70%" "$args"

unset SPIRA_AEON_CPU_QUOTA
export SPIRA_SUMMON="$T/bin/mock-summon"

# ======================================================================================
echo
echo "summon_fayth — a task fayth honours the pool argument; a lane fayth ignores it"
# ======================================================================================
MOCK_READY_stretchy=1; MOCK_READY_laner=1
MOCK_COUNT=0

rm -f "$SUMMONED"
summon_fayth stretchy 1 >/dev/null 2>&1 || true
is "task fayth with pool=1 is summoned" "SUMMONED:stretchy" "$(cat "$SUMMONED" 2>/dev/null)"

rm -f "$SUMMONED"
summon_fayth stretchy 0 >/dev/null 2>&1 || true
is "task fayth with pool=0 is not summoned" "absent" \
   "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"

rm -f "$SUMMONED"
summon_fayth laner >/dev/null 2>&1 || true   # no pool argument — the lane path
is "lane fayth without pool arg IS summoned even when pool would be 0" \
   "SUMMONED:laner" "$(cat "$SUMMONED" 2>/dev/null)"

# ======================================================================================
echo
echo "escape.sh — reaches a bead when the pool argument would have refused it"
# ======================================================================================
# SUBPROCESS STUB. Shell function overrides (fayth_ready, capacity_paused) do not
# propagate into `bash escape.sh` — it sources lib.sh fresh. SPIRA_BD is exported to a
# shim that answers "ready" queries from a sentinel file, so the subprocess's readiness
# can be toggled without a real Dolt database. Its real capacity_paused runs unstubbed;
# with no pause stamp at $SPIRA_RUN it answers "not paused" on its own.
export FAKE_READY_FILE="$T/run/fake-ready"
cat > "$T/bin/fake-bd" <<'FAKEBD'
#!/usr/bin/env bash
for arg; do
    if [ "$arg" = "ready" ]; then
        [ -f "${FAKE_READY_FILE:-}" ] && printf '[{"id":"sp-test"}]\n' || printf '[]\n'
        exit 0
    fi
done
exit 0
FAKEBD
chmod +x "$T/bin/fake-bd"
export SPIRA_BD="$T/bin/fake-bd"

MOCK_READY_stretchy=1
touch "$T/run/fake-ready"

# POSITIVE CONTROL: normal summon with pool=0 produces nothing.
rm -f "$SUMMONED"
summon_fayth stretchy 0 >/dev/null 2>&1 || true
is "positive: normal summon with pool=0 produces nothing" "absent" \
   "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"

# escape.sh bypasses the pool and still summons.
rm -f "$SUMMONED"
bash "$HERE/escape.sh" stretchy 2>/dev/null || true
want "escape.sh summons despite pool=0 not being passed" "SUMMONED:stretchy" "$(cat "$SUMMONED" 2>/dev/null)"

# THE CONTROL THAT MAKES THE ABOVE MEANINGFUL: escape.sh with nothing ready exits 0 but
# summons nothing — the summon above is about the fayth having work, not the script
# always calling the binary unconditionally.
rm -f "$T/run/fake-ready" "$SUMMONED"
bash "$HERE/escape.sh" stretchy 2>/dev/null || true
is "escape.sh with nothing ready does not summon" "absent" \
   "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"
touch "$T/run/fake-ready"

# ======================================================================================
echo
echo "G5 — summon_fayth refuses under a live halt, loudly (previously only source-grepped)"
# ======================================================================================
HALT_STAMP="$T/run/world.halted"

# POSITIVE CONTROL: with no halt stamp, stretchy is summoned.
rm -f "$HALT_STAMP" "$SUMMONED"
summon_fayth stretchy >/dev/null 2>&1 || true
is "G5 positive control: no halt stamp, stretchy is summoned" \
   "SUMMONED:stretchy" "$(cat "$SUMMONED" 2>/dev/null)"

: > "$HALT_STAMP"
rm -f "$SUMMONED"
out="$(summon_fayth stretchy 2>&1 || true)"
is "G5: halted — nothing summoned" "absent" "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"
want "G5: log says 'halted — not summoning'" "halted — not summoning" "$out"
rm -f "$HALT_STAMP"

# ======================================================================================
echo
echo "G6 — plain fleet-ceiling refusal, not merely the last-slot rules"
# ======================================================================================
export SPIRA_MAX_LIVE_AEONS=3
MOCK_READY_anchor=1

# POSITIVE CONTROL: one slot below the ceiling, anchor is summoned.
MOCK_LIVE=2
rm -f "$SUMMONED"
summon_fayth anchor >/dev/null 2>&1 || true
is "G6 positive control: below the ceiling, anchor is summoned" \
   "SUMMONED:anchor" "$(cat "$SUMMONED" 2>/dev/null)"

MOCK_LIVE=3    # 3/3 — the fleet is AT the ceiling, not merely down to its last slot
rm -f "$SUMMONED"
out="$(summon_fayth anchor 2>&1 || true)"
is "G6: fleet at ceiling — nothing summoned" "absent" "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"
want "G6: log names the live/ceiling count" "3/3 aeon(s) live across the whole fleet" "$out"
want "G6: log says 'not summoning'" "not summoning" "$out"
unset SPIRA_MAX_LIVE_AEONS

# ======================================================================================
echo
echo "G7 — governor withholding: MOOT, spira/governor.sh no longer exists (sp-8mzsh)"
# ======================================================================================
# fayth_free carries no SP_GOVERNOR_MODE clamp and summon_fayth logs no 'withheld by the
# governor' — grepped and confirmed absent from both when this suite was written. There
# is no hook left to write a row against.

# ======================================================================================
echo
echo "G8 — escape.sh ignores world.halted (characterization of current behaviour; sp-6rv05 open)"
# ======================================================================================
MOCK_READY_stretchy=1
touch "$T/run/fake-ready"
: > "$HALT_STAMP"
rm -f "$SUMMONED"
bash "$HERE/escape.sh" stretchy 2>/dev/null || true
want "G8: escape.sh summons even with world.halted present" "SUMMONED:stretchy" "$(cat "$SUMMONED" 2>/dev/null)"
rm -f "$HALT_STAMP"

tl_summary
