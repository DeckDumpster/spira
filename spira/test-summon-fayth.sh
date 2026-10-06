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
#   - test-lane-ceiling.sh — criteria (a)(b)(c) and their controls (rotation (d) moved on,
#     see the note below)
#   - test-drain-expiry.sh — every row, including the world.sh writer/reader seam
#   - test-fayth-free.sh — every row but its own vacuous inline-arithmetic "positive
#     control" (G18), replaced below with a row that calls fayth_free directly with
#     `have` decoupled from `pool`
#   - the summon/free/escape rows of test-lanes.sh (its roster-split and ops.fayth-lane
#     rows stay behind — D5's target is test-fayth.sh, sp-9ce60.2.2)
#   - the CPUQuota rows of test-fayth.sh (UC-dispatch-23; now absence rows, sp-b4oct)
#
# NEW GAP ROWS (docs/test-plan/dispatch.md section 6):
#   G5 — the halt gate, previously only source-grepped (test-world.sh)
#   G6 — the plain fleet-ceiling refusal; only the last-slot rules had a test
#   G7 — governor withholding: MOOT. spira/governor.sh was deleted from main (sp-8mzsh)
#        before this suite was written, and neither fayth_free nor summon_fayth carries
#        a governor hook left to test against — there is nothing to write a row for.
#   G8 — escape.sh and world.halted/world.draining: escape.sh now calls world_gate, the
#        same refusal summon_fayth uses (sp-uyw4n settled the question; sp-2w2wu wired it).
#        escape.sh itself is retired into `aeon --escape <fayth>` (sp-zpaq0); the refusal
#        is unchanged because it is still world_gate, reached through the aeon binary's
#        own lib.sh seam (aeon/src/escape.rs) rather than escape.sh's own sourcing.
#
# WAVE 4.27 (family G, sp-gzmd2): `world_gate`/`summon_argv`/`summon_fayth`/
# `ck7_summon_pass` moved in-process into the sentinel crate; lib.sh's own copies are
# one-line shims onto a fresh `sentinel` process. A fresh process cannot see a shell
# function this script defines after sourcing lib.sh — the OLD stub set (bash functions
# named `aeons_live_total`/`aeons_live_lanes`/`fayth_ready`/`capacity_paused`) stopped
# reaching anything the day that landed. Every row below that calls `summon_fayth`
# directly now drives the SAME questions through what sentinel itself reads: real
# pidfiles under $SPIRA_RUN for the live/concurrency counts (sentinel falls back to them
# whenever SPIRA_SUMMON is not literally "systemd-run", true throughout this suite — the
# same fallback production uses off a real systemd user session), and a `spira-claim`
# stub on PATH for readiness. `fayth_free` ITSELF IS UNCHANGED (lib.sh keeps it real bash
# by design — see lib.sh's own comment on the exec-boundary trap) and still calls
# `aeon_count` by name in this shell, so the fayth_free-only rows below are untouched.
#
# Lane rotation's own integration (G1(d)) moved to a Rust test
# (`sentinel::tests::ck7_summon_pass_rotates_across_two_real_passes`), which can make a
# summoned lane's unit actually appear for the SECOND lane's own check within the same
# pass — a mock summon script here cannot do that (its mock aeon never becomes a real
# unit), so the bash version of this row was always a hand simulation of the loop, not a
# call to it. The pure rotation arithmetic is `summon::tests::
# lane_rotate_moves_last_and_everything_before_it_to_the_end`.
#
# POSITIVE CONTROLS BEFORE EACH REFUSAL (law-absence-needs-a-positive-control).
#
# tier: T1
# covers: spira/lib.sh sentinel/src/summon.rs aeon/src/escape.rs spira-world/src/bin/world.rs UC-dispatch-09 UC-dispatch-10 UC-dispatch-11 UC-dispatch-12 UC-dispatch-15 UC-dispatch-23
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/run" "$T/chamber" "$T/bin"
# SPIRA_CHAMBER no longer derives from SPIRA_HOME (the fixture declares its own path) —
# point it at this suite's own fixture chamber explicitly.
tl_config SPIRA_CHAMBER="$T/chamber"

# `aeon --escape <fayth>` (spira/escape.sh retired, sp-zpaq0) resolves its home from
# SPIRA_HOME and requires <home>/lib.sh to exist; this one-line stub sources the REAL
# lib.sh from $HERE.
printf '. "%s/lib.sh"\n' "$HERE" > "$T/lib.sh"

# conf.d IS COPIED IN (matching test-aeon-sweep.sh, ...):
# aeon derives its config registry from --home/SPIRA_HOME and now REFUSES to start if
# conf.d is missing (sp-1cdgq) -- a --home with no conf.d used to resolve silently to
# nothing instead of refusing. Without this, `aeon --escape` below dies at config
# resolution before it ever reaches escape.rs's own logic.
cp -r "$HERE/conf.d" "$T/"

# A MINIMAL ENVIRONMENT, non-default everywhere so that assertions cannot pass by reading
# literals out of conf.sh (law-gates-run-in-a-clean-environment). $T/bin goes FIRST so our
# own spira-claim/mock-summon stubs shadow the release's real ones; the release's real
# `aeon` (never placed in $T/bin) still resolves further down the same PATH.
SPIRA_RUN="$T/run"; tl_config SPIRA_RUN="$SPIRA_RUN"
export SPIRA_CONF="$T/no-such.conf"
export SPIRA_HOME="$T" PATH="$T/bin:$T:$PATH"
SPIRA_DB="$T/no-db"; tl_config SPIRA_DB="$SPIRA_DB"

# THE AEON IS A BINARY (aeon.sh is gone): summon_fayth hands systemd-run the aeon it finds
# on PATH (sp-gypjk). The mock SPIRA_SUMMON never execs it.
command -v aeon >/dev/null 2>&1 \
    || { echo "test-summon-fayth: aeon is not on PATH" >&2; exit 1; }

. "$HERE/lib.sh"

# ======================================================================================
# THE SHARED STUB SET. No real database, no real systemd, no real process table.
# ======================================================================================

# fayth_free (lib.sh) is UNCHANGED by wave 4.27 — it stays real bash, calling aeon_count
# by name in THIS shell, so a bash-function stub still reaches it directly.
MOCK_COUNT=0
aeon_count() { printf '%d' "$MOCK_COUNT"; }

# A small pool of real, long-lived processes whose /proc cmdline reads as an aeon
# (aeon_alive's own check: argv[0] ends in "/aeon" or contains "aeon.sh") — exactly the
# `exec -a aeon.sh` trick test-summon-fast-path.sh's own section A already uses. set_live
# writes pidfiles pointing at them; sentinel's pidfile fallback (SPIRA_SUMMON is never
# literally "systemd-run" in this suite) counts a pidfile as live iff the pid is one of
# these and still running.
FAKE_AEON_PIDS=()
for _i in 1 2 3 4 5; do
    ( exec -a aeon.sh sleep 300 ) &
    FAKE_AEON_PIDS+=("$!")
done
# Kill-on-exit (sp-r70dc): these five `sleep 300` fixtures were never killed anywhere in
# this suite — every run, pass or fail, orphaned all five for up to 5 minutes.
trap 'kill "${FAKE_AEON_PIDS[@]}" 2>/dev/null; wait "${FAKE_AEON_PIDS[@]}" 2>/dev/null; rm -rf "$T"' EXIT INT TERM

# set_live <fayth> <n> -> exactly <n> live pidfiles for <fayth> (clears its own first).
set_live() {
    local fayth="$1" n="${2:-0}" i
    rm -f "$SPIRA_RUN"/aeon-"$fayth"-mock*.pid
    for ((i = 1; i <= n; i++)); do
        # Reused cyclically past the pool's own size (callers that just need "a lot" —
        # e.g. 999, to prove an unset ceiling ignores the count entirely — never need
        # that many DISTINCT pids, only that many pidfiles).
        printf '%s' "${FAKE_AEON_PIDS[$(((i - 1) % ${#FAKE_AEON_PIDS[@]}))]}" > "$SPIRA_RUN/aeon-$fayth-mock$i.pid"
    done
}
clear_live() { rm -f "$SPIRA_RUN"/aeon-*-mock*.pid; }

# set_ready <fayth> <n> -> fayth_ready(<fayth>) as `summon_fayth` (now a `sentinel` shim,
# wave 4.27) reaches it: `spira-claim fayth-ready`, a real subprocess on PATH, never a
# bash function the fresh sentinel process could see.
FAYTH_READY_CALL_FILE="$T/fayth-ready-calls"
set_ready() { printf '%s' "${2:-0}" > "$T/ready-$1"; }
cat > "$T/bin/spira-claim" <<SPIRACLAIM
#!/usr/bin/env bash
if [ "\${1:-}" = fayth-ready ]; then
    printf '%s\n' "\${2:-}" >> "$FAYTH_READY_CALL_FILE"
    cat "$T/ready-\${2:-}" 2>/dev/null || printf 0
    exit 0
fi
exit 0
SPIRACLAIM
chmod +x "$T/bin/spira-claim"
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
tl_config SPIRA_FAYTHS="anchor stretchy" SPIRA_MAX_LIVE_AEONS=3
clear_live
# The fleet-total pidfiles are tagged under "stretchy" throughout this section, never
# "anchor": stretchy is elastic AND every call below passes a pool, so its OWN
# concurrency check (`is_remainder`) uses the pool verbatim and never looks at its own
# `have` at all — tagging the fleet total under anchor instead would corrupt anchor's own
# cap (1) the moment criterion 4 asks for it.

# POSITIVE CONTROL: with 2 free slots, stretchy IS summoned despite anchor being ready.
set_live stretchy 1     # 3-1=2 free slots
set_ready anchor 1
set_ready stretchy 1
rm -f "$SUMMONED"
summon_fayth stretchy 4 >/dev/null 2>&1 || true
is "positive: elastic succeeds with 2 free slots (anchor also ready)" \
   "SUMMONED:stretchy" "$(cat "$SUMMONED" 2>/dev/null)"

# N-1=2 live -> exactly 1 slot free. anchor has work -> stretchy must be refused.
set_live stretchy 2
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
set_live stretchy 2
set_ready anchor 0
set_ready stretchy 1
rm -f "$SUMMONED"
summon_fayth stretchy 4 >/dev/null 2>&1 || true
is "criterion 2: elastic succeeds when no non-elastic has work" \
   "SUMMONED:stretchy" "$(cat "$SUMMONED" 2>/dev/null)"

echo
echo "criterion 4 — non-elastic persona is never refused by this rule"
set_live stretchy 2     # fleet total only — never under "anchor" itself (see note above)
set_ready anchor 1
set_ready stretchy 1
rm -f "$SUMMONED"
summon_fayth anchor >/dev/null 2>&1 || true
is "criterion 4: non-elastic persona succeeds with 1 slot free" \
   "SUMMONED:anchor" "$(cat "$SUMMONED" 2>/dev/null)"

echo
echo "cost — fayth_ready is called for non-elastic personas only when the last slot is contested"
reset_call_count
set_live stretchy 2
rm -f "$SUMMONED"
summon_fayth stretchy 4 >/dev/null 2>&1 || true
is "cost: refused case makes exactly 1 fayth_ready call (anchor reservation only)" \
   "1" "$(fayth_ready_call_count)"
is "cost: that call was for anchor" "anchor" "$(cat "$FAYTH_READY_CALL_FILE" 2>/dev/null)"

reset_call_count
set_live stretchy 1     # 2 free slots
rm -f "$SUMMONED"
summon_fayth stretchy 4 >/dev/null 2>&1 || true
is "cost: 2-free-slot path makes 1 fayth_ready call (stretchy only, no reservation)" \
   "1" "$(fayth_ready_call_count)"
is "cost: that call was for stretchy" "stretchy" "$(cat "$FAYTH_READY_CALL_FILE" 2>/dev/null)"

# CASE DELETED (per Ryan 2026-10-06, rule 5): "no ceiling — SPIRA_MAX_LIVE_AEONS unset
# means today's behaviour exactly" pinned empty-means-no-cap, a default. Every registered
# key now has a declared value (an empty string is no longer a valid u32), so "unset" is
# not a state SPIRA_MAX_LIVE_AEONS can be in any more — the premise this case tested is gone.

# ======================================================================================
echo
echo "lane ceiling (a) — a non-lane task fayth is held back when last slot and a lane has ready work"
# ======================================================================================
tl_config SPIRA_FAYTHS="tasker laner" SPIRA_MAX_LIVE_AEONS=4 SPIRA_LANES_MAX_LIVE=1
clear_live
# The fleet-total pidfiles below are tagged under "tasker": tasker's own cap is 4, well
# above every value used in this section, so tagging the fleet total there never corrupts
# tasker's own concurrency check the way tagging it under "laner" (cap 1) would.

# POSITIVE CONTROL: with 2 free slots, tasker IS summoned even though laner is ready.
set_live tasker 2
set_ready laner 1
set_ready tasker 1
rm -f "$SUMMONED"
summon_fayth tasker >/dev/null 2>&1 || true
is "positive: tasker succeeds with 2 free slots (laner also ready)" \
   "SUMMONED:tasker" "$(cat "$SUMMONED" 2>/dev/null)"

# 3 live -> 1 slot free. laner ready -> tasker must be held back.
set_live tasker 3
rm -f "$SUMMONED"
summon_fayth tasker >/dev/null 2>&1 || true
is "(a): tasker refused when last slot and laner has ready work" \
   "absent" "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"

log_out="$(summon_fayth tasker 2>&1 || true)"
want   "log says '1 fleet slot remaining'" "1 fleet slot remaining" "$log_out"
want   "log says 'held back'"              "held back"              "$log_out"
nowant "log says 'at concurrency cap'"     "at concurrency cap"     "$log_out"

echo
echo "lane ceiling (a2) — tasker IS summoned when the only ready lane is at its own concurrency cap"
# laner has FAYTH_MAX_CONCURRENT=1; a real laner pidfile means it is simultaneously at its
# own cap AND counted in the collective lane total — unlike the old independent mocks,
# those two facts cannot be pulled apart with real state, so SPIRA_LANES_MAX_LIVE is
# raised to 2 for this one case to keep the SAME path exercised (a ready lane refused only
# because IT ITSELF has no room, not because the collective cap already absorbed it).
tl_config SPIRA_LANES_MAX_LIVE=2
set_live tasker 2        # total 3/4 -> exactly 1 fleet slot free, the reservation's gate
set_live laner 1        # laner's own cap (1) is now met, AND the lane total is 1 (< 2)
set_ready laner 1
set_ready tasker 1
rm -f "$SUMMONED"
summon_fayth tasker >/dev/null 2>&1 || true
is "(a2): tasker summoned when laner is ready but at its own concurrency cap" \
   "SUMMONED:tasker" "$(cat "$SUMMONED" 2>/dev/null)"
tl_config SPIRA_LANES_MAX_LIVE=1
set_live laner 0

echo
echo "lane ceiling (b) — the task fayth is allowed when last slot and no lane has ready work"
set_live tasker 3
set_ready laner 0
set_ready tasker 1
rm -f "$SUMMONED"
summon_fayth tasker >/dev/null 2>&1 || true
is "(b): tasker summoned when no lane has ready work" \
   "SUMMONED:tasker" "$(cat "$SUMMONED" 2>/dev/null)"

echo
echo "lane ceiling (c) — collective lane cap: at most SPIRA_LANES_MAX_LIVE lane aeons"
# POSITIVE CONTROL: lane below cap -> summoned.
set_live tasker 0
set_live laner 0
set_ready laner 1
set_ready tasker 0
rm -f "$SUMMONED"
summon_fayth laner >/dev/null 2>&1 || true
is "positive: laner summoned when below lane cap" \
   "SUMMONED:laner" "$(cat "$SUMMONED" 2>/dev/null)"

# One real laner pidfile is simultaneously "laner's own cap reached" and "the lane total
# is at SPIRA_LANES_MAX_LIVE (1)" — both true at once in reality, and either alone
# refuses laner here, so this still proves the collective-cap message fires.
set_live laner 1
rm -f "$SUMMONED"
summon_fayth laner >/dev/null 2>&1 || true
is "(c): laner refused when lanes at collective cap" \
   "absent" "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"

log_out="$(summon_fayth laner 2>&1 || true)"
want "log says 'lane slot(s) in use'" "lane slot(s) in use" "$log_out"

# The lane cap check is for lane fayths only; a task fayth is not refused by it.
set_live tasker 0
set_ready laner 0
set_ready tasker 1
rm -f "$SUMMONED"
summon_fayth tasker >/dev/null 2>&1 || true
is "(c): tasker not refused by the lane cap check" \
   "SUMMONED:tasker" "$(cat "$SUMMONED" 2>/dev/null)"
set_live laner 0

# CASE DELETED (per Ryan 2026-10-06, rule 5): "no lane cap — SPIRA_LANES_MAX_LIVE unset:
# no preference, the task fayth fills freely" pinned empty-means-no-preference, a default.
# Every registered key now has a declared value; "unset" is gone as a state to test.

clear_live

# ======================================================================================
echo
echo "drain expiry — positive control: with no drain at all, the fayth IS summoned"
# ======================================================================================
export SPIRA_DRAIN_TTL=1800
set_ready stretchy 1
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
SPIRA_RUN="$SPIRA_RUN" world drain --for 900 --timeout 1 >/dev/null 2>&1
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
echo "summon_fayth — no CPUQuota and no Nice reach the summon command (sp-b4oct)"
# ======================================================================================
ARGS_FILE="$T/summon-args"
MOCK_QUOTA="$T/bin/mock-quota"
printf '#!/bin/sh\nprintf "%%s\\n" "$@" > "%s"\nexit 0\n' "$ARGS_FILE" > "$MOCK_QUOTA"
chmod +x "$MOCK_QUOTA"
export SPIRA_SUMMON="$MOCK_QUOTA"
set_ready stretchy 1
clear_live

# law-isolate-greedy-work-in-vms: the OS schedules aeons. The positive control is that the
# summon happened at all (TimeoutStartSec is in the captured argv), so an empty capture
# cannot pass the absence rows. The retired SPIRA_AEON_CPU_QUOTA is set to prove it is inert.
export SPIRA_AEON_CPU_QUOTA=90
rm -f "$ARGS_FILE"
summon_fayth stretchy >/dev/null 2>&1 || true
args="$(cat "$ARGS_FILE" 2>/dev/null)"
want   "summon_fayth: the summon argv was captured" "TimeoutStartSec=" "$args"
nowant "summon_fayth: no CPUQuota, even with the retired knob set" "CPUQuota" "$args"
nowant "summon_fayth: no Nice"                                    "Nice="    "$args"

unset SPIRA_AEON_CPU_QUOTA
export SPIRA_SUMMON="$T/bin/mock-summon"

# ======================================================================================
echo
echo "summon_fayth — a task fayth honours the pool argument; a lane fayth ignores it"
# ======================================================================================
set_ready stretchy 1
set_ready laner 1
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
echo "aeon --escape — reaches a bead when the pool argument would have refused it"
# ======================================================================================
# SUBPROCESS STUB. `aeon --escape`'s own lib.sh seam sources lib.sh fresh in a new bash
# process (aeon/src/seam.rs), exactly as escape.sh did and exactly as `summon_fayth`'s own
# `sentinel` shim now does for THIS script's direct calls too — neither sees a function
# this shell defines. SPIRA_BD is exported to a shim that answers "ready" queries from a
# sentinel file, so the subprocess's readiness can be toggled without a real Dolt
# database. Its real capacity_paused runs unstubbed; with no pause stamp at $SPIRA_RUN it
# answers "not paused" on its own.
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
tl_config SPIRA_BD="$T/bin/fake-bd"

# `summon_fayth`'s OWN readiness, below, goes through the spira-claim stub (set_ready) —
# $T/bin is ahead of the release's real spira-claim on PATH for every subprocess this
# whole suite spawns, `aeon --escape` included, so fake-bd's "ready" answer (read through
# a REAL spira-claim this stub shadows) is never reached either way; kept in sync here so
# both readinesses agree.
touch "$T/run/fake-ready"
set_ready stretchy 1

# POSITIVE CONTROL: normal summon with pool=0 produces nothing.
rm -f "$SUMMONED"
summon_fayth stretchy 0 >/dev/null 2>&1 || true
is "positive: normal summon with pool=0 produces nothing" "absent" \
   "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"

# escape.sh bypasses the pool and still summons.
rm -f "$SUMMONED"
aeon --escape stretchy 2>/dev/null || true
want "escape.sh summons despite pool=0 not being passed" "SUMMONED:stretchy" "$(cat "$SUMMONED" 2>/dev/null)"

# THE CONTROL THAT MAKES THE ABOVE MEANINGFUL: escape.sh with nothing ready exits 0 but
# summons nothing — the summon above is about the fayth having work, not the script
# always calling the binary unconditionally.
rm -f "$T/run/fake-ready" "$SUMMONED"
set_ready stretchy 0
aeon --escape stretchy 2>/dev/null || true
is "escape.sh with nothing ready does not summon" "absent" \
   "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"
touch "$T/run/fake-ready"
set_ready stretchy 1

# ======================================================================================
echo
echo "G5 — summon_fayth refuses under a live halt, loudly (previously only source-grepped)"
# ======================================================================================
HALT_STAMP="$T/run/world.halted"
set_ready stretchy 1

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
# SPIRA_FAYTHS restored: the lane-ceiling section above left it at "tasker laner", and
# live_total/aeon_count only count pidfiles tagged under the CURRENT roster — unlike the
# old independent MOCK_LIVE, a real fleet total is the roster's own sum.
tl_config SPIRA_FAYTHS="anchor stretchy" SPIRA_MAX_LIVE_AEONS=3
set_ready anchor 1
clear_live

# POSITIVE CONTROL: one slot below the ceiling, anchor is summoned.
set_live stretchy 2     # fleet total only — tagged away from "anchor" itself (cap 1)
rm -f "$SUMMONED"
summon_fayth anchor >/dev/null 2>&1 || true
is "G6 positive control: below the ceiling, anchor is summoned" \
   "SUMMONED:anchor" "$(cat "$SUMMONED" 2>/dev/null)"

set_live stretchy 3    # 3/3 — the fleet is AT the ceiling, not merely down to its last slot
rm -f "$SUMMONED"
out="$(summon_fayth anchor 2>&1 || true)"
is "G6: fleet at ceiling — nothing summoned" "absent" "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"
want "G6: log names the live/ceiling count" "3/3 aeon(s) live across the whole fleet" "$out"
want "G6: log says 'not summoning'" "not summoning" "$out"
clear_live

# ======================================================================================
echo
echo "G7 — governor withholding: MOOT, spira/governor.sh no longer exists (sp-8mzsh)"
# ======================================================================================
# fayth_free carries no SP_GOVERNOR_MODE clamp and summon_fayth logs no 'withheld by the
# governor' — grepped and confirmed absent from both when this suite was written. There
# is no hook left to write a row against.

# ======================================================================================
echo
echo "G8 — escape.sh honours world_gate: a halt or live drain refuses the escape summon too (sp-uyw4n, sp-2w2wu)"
# ======================================================================================
touch "$T/run/fake-ready"

# POSITIVE CONTROL: with no halt or drain stamp, escape.sh still summons.
rm -f "$HALT_STAMP" "$T/run/world.draining" "$SUMMONED"
aeon --escape stretchy 2>/dev/null || true
is "G8 positive control: no halt/drain, escape.sh summons" \
   "SUMMONED:stretchy" "$(cat "$SUMMONED" 2>/dev/null)"

: > "$HALT_STAMP"
rm -f "$SUMMONED"
out="$(aeon --escape stretchy 2>&1)"; rc=$?
is "G8: halted — escape.sh does not summon" "absent" "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"
want "G8: escape.sh logs the halt refusal" "halted — not summoning" "$out"
is "G8: escape.sh exits non-zero when halted" "1" "$rc"
rm -f "$HALT_STAMP"

DRAIN_STAMP_G8="$T/run/world.draining"
{ echo "now"; echo "gated"; printf 'expires %s\n' "$(( $(date +%s) + 3600 ))"; } > "$DRAIN_STAMP_G8"
rm -f "$SUMMONED"
out="$(aeon --escape stretchy 2>&1)"; rc=$?
is "G8: live drain — escape.sh does not summon" "absent" "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"
want "G8: escape.sh logs the drain refusal" "draining — not summoning" "$out"
is "G8: escape.sh exits non-zero when draining" "1" "$rc"

{ echo "now"; echo "gated"; printf 'expires %s\n' "$(( $(date +%s) - 60 ))"; } > "$DRAIN_STAMP_G8"
rm -f "$SUMMONED"
aeon --escape stretchy 2>/dev/null || true
is "G8: expired drain — escape.sh summons" "SUMMONED:stretchy" "$(cat "$SUMMONED" 2>/dev/null)"
rm -f "$DRAIN_STAMP_G8"

# ======================================================================================
echo
echo "escape.sh passes no CPUQuota and no Nice (sp-b4oct; it shares summon_argv since sp-9ce60.4)"
# ======================================================================================
# escape.sh shares summon_argv() with summon_fayth, the same builder UC-dispatch-23 exercises
# directly below, so the absence is verified here at the escape.sh call site and again there.
QUOTA_ARGV="$T/escape-quota-argv"
cat > "$T/bin/mock-summon" <<MOCK
#!/usr/bin/env bash
printf '%s\n' "\$@" > "$QUOTA_ARGV"
exit 0
MOCK
chmod +x "$T/bin/mock-summon"

rm -f "$QUOTA_ARGV"
export SPIRA_AEON_CPU_QUOTA=55
aeon --escape stretchy 2>/dev/null || true
want   "escape.sh: the summon argv was captured" "TimeoutStartSec=" "$(cat "$QUOTA_ARGV" 2>/dev/null)"
nowant "escape.sh: no CPUQuota, even with the retired knob set" "CPUQuota" "$(cat "$QUOTA_ARGV" 2>/dev/null)"
nowant "escape.sh: no Nice"                                    "Nice="    "$(cat "$QUOTA_ARGV" 2>/dev/null)"
unset SPIRA_AEON_CPU_QUOTA

cat > "$T/bin/mock-summon" <<'MOCK'
#!/usr/bin/env bash
fayth="${@: -1}"
[ "$fayth" = "--dry-run" ] && fayth="${@: -2:1}"
printf 'SUMMONED:%s\n' "$fayth" >> "$SUMMONED_FILE"
exit 0
MOCK
chmod +x "$T/bin/mock-summon"

# ======================================================================================
echo
echo "UC-dispatch-23 — summon_argv: TimeoutStartSec and no CPUQuota/Nice, direct on the builder itself"
# ======================================================================================
sargv="$(summon_argv anchor | tr '\n' ' ')"
want "summon_argv: TimeoutStartSec from the fayth" \
     "TimeoutStartSec=$(fayth_get anchor FAYTH_TIMEOUT_SECONDS 3600)" "$sargv"
nowant "summon_argv: no CPUQuota (sp-b4oct)" "CPUQuota" "$sargv"
nowant "summon_argv: no Nice (sp-b4oct)"     "Nice="    "$sargv"

# UC-dispatch-23 / G17 — the claude CLI argv (model, tools, --setting-sources) is not built
# here. aeon's two launch sites both build it through the same argv table (aeon/src/run.rs;
# aeon_claude_argv, lib.sh, retired dead by sp-j89pd — zero live callers), a pure function of
# the fayth's own FAYTH_* knobs already driven directly, in both system-prompt modes, by
# test-aeon-prompt-layers.sh's table — that table IS the bead-mode row G17 asked for, since
# the function makes no sweep/bead distinction at all.

tl_summary
