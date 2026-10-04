#!/usr/bin/env bash
#
# test-sentinel-pass.sh — real end-to-end sentinel.sh passes: a normal pass never runs
#   CHECK 4/5/6b/7c/7d inline any more (sp-994y9) — it dispatches them as `sentinel.sh
#   --audit` and moves straight to summon; `--audit` is where those checks actually run,
#   and it never lands or summons. Also covers the base-unchanged stamp and a database
#   sentinel that cannot reach exits 1 without ever reporting a state or a completed pass.
#
#   ./test-sentinel-pass.sh
#
# HOST OF test-sentinel-order.sh (dispatch.md D8, sp-9ce60.5), which also absorbs
# test-sentinel-capacity.sh. Cut from 9 real passes total (5 + 2 + 2, across the three
# merged files) to 2 Dolt-backed passes plus one DB-unreadable check that needs no
# database at all, now that the per-case arithmetic those passes used to be the ONLY
# coverage for has its own T1 tables:
#   - the fill loop's pool math and per-persona cap  -> ck7_pool/ck7_fill_cap (G2, G3),
#     tested directly in sentinel's own summon::tests (wave 4.27, sp-gzmd2 — ported from
#     bash, where test-watchtower-throttle.sh used to call them)
#   - lane rotation order                            -> lane_rotate (G1),
#     tested directly in sentinel's own summon::tests, ditto
#   - CHECK 8's firing predicate                      -> sentinel's audit::tests::judgement_table
#     (G15; check8_should_judge/test-check8-progressed.sh retired at sp-8itaf, zero live callers)
#
# SP-994Y9: A NORMAL PASS TOOK 4-5 MINUTES against a 2-minute timer because CHECK 4 (poison,
# walks every dispatchable bead), CHECK 5 (closed-not-landed) and CHECK 6b (the Sending,
# walks every branch) ran inline, ahead of or beside CHECK 7 (summon) — so a fleet slot
# that freed up mid-walk sat empty until the WHOLE pass finished. Those checks now run only
# under `sentinel.sh --audit`, dispatched as a transient unit the same way CHECK 6 already
# dispatches landing; a normal pass never calls sending.sh or walks the poison set at all.
# The regression here is TIMING: sending.sh is stubbed to sleep past the old inline cost,
# and a normal pass must still return near-instantly and still summon every ready bead.
#
# ONE NO-DATABASE CHECK: SPIRA_BD points at a shim that always fails, so `bdq list` fails
# the way a real outage would without paying for a Dolt fixture at all (UC-dispatch-19).
#
# POSITIVE CONTROLS FIRST throughout (law-a-regression-test-must-be-seen-to-fail).
#
# defect: sp-len2q sp-4fss sp-994y9
# tier: T3
# covers: sentinel/src/* spira/lib.sh UC-dispatch-14 UC-dispatch-19
# hermetic-ok: uses a fixture database; systemd/gh/network reached through
#   SPIRA_LAUNCH, SPIRA_SUMMON and SPIRA_SYSTEMCTL seams pointed at stubs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"; trap 'testdb_drop 2>/dev/null; rm -rf "$TMP"' EXIT INT TERM

STUBS="$TMP/stubs"
mkdir -p "$STUBS"
for _s in pilgrimage.sh reflect.sh; do
    printf '#!/bin/sh\n' > "$STUBS/$_s"; chmod +x "$STUBS/$_s"
done
# strand ALSO backs lib.sh's aeon_alive/aeon_count/aeons_live_total/aeons_live_lanes shims
# now (wave 4.23, sp-0ffox) — a bare no-op here would make `$(aeon_count ...)` inside
# lib.sh's pass-start summary print nothing, not "0", and blow up the arithmetic that adds
# it. A plain no-op is still correct for strand's OWN verbs (report/check/throttle-state),
# which is all this suite needs from it.
cat > "$STUBS/strand" <<'STRANDSTUB'
#!/bin/sh
case "$1" in
    aeon-alive) exit 1 ;;
    aeon-count|aeons-live-total|aeons-live-lanes) printf '0' ;;
esac
STRANDSTUB
chmod +x "$STUBS/strand"
# THE SENTINEL IS A BINARY (sentinel.sh is gone). It sources lib.sh from SPIRA_HOME, so the
# stub home carries the real lib.sh and what it sources; strand is the stub first on PATH.
for _s in lib.sh conf.sh suite-covers.sh conf.d; do
    ln -s "$HERE/$_s" "$STUBS/$_s"
done
printf '#!/bin/sh\necho inactive\n'  > "$STUBS/mock-systemctl"; chmod +x "$STUBS/mock-systemctl"
LAUNCH_ARGV="$TMP/launch-argv"
# mock-launch stands in for systemd-run: it records every dispatch's own argv (CHECK 6's
# land dispatch AND the new CHECK 4/5 audit dispatch both go through SPIRA_LAUNCH, so
# assertions on one unit's properties must grep the line naming its own --unit=).
printf '#!/bin/sh\necho "$@" >> "%s"\nexit 0\n' "$LAUNCH_ARGV" > "$STUBS/mock-launch"; chmod +x "$STUBS/mock-launch"
printf '#!/bin/sh\nexit 0\n'         > "$STUBS/mock-notify";    chmod +x "$STUBS/mock-notify"
touch "$STUBS/repo-map"   # empty: no repos, Sending finds nothing to walk
# THE REAL CHAMBER, symlinked once up front: `ln -sf` onto a directory that already
# exists creates the link INSIDE it instead of replacing it, so this must run before
# anything else ever creates $STUBS/chamber as a plain directory.
ln -s "$HERE/chamber" "$STUBS/chamber"

echo "test-sentinel-pass.sh"

# ======================================================================================
echo
echo "DB unreadable — no fixture needed, a failing SPIRA_BD shim fails fast (UC-dispatch-19):"
# ======================================================================================
# THIS NEEDS NO testdb_up AT ALL: the shim exits nonzero unconditionally, exactly the
# shape of "bd cannot reach the database", without spending a Dolt build to prove it.
FAILING_BD="$TMP/failing-bd"
printf '#!/bin/sh\nexit 1\n' > "$FAILING_BD"; chmod +x "$FAILING_BD"
_run_unreadable="$TMP/run-unreadable"; mkdir -p "$_run_unreadable"
out_unreadable="$(env -i \
    PATH="$PATH" HOME="$HOME" \
    SPIRA_HOME="$STUBS" PATH="$STUBS:$PATH" \
    SPIRA_RUN="$_run_unreadable" \
    SPIRA_DB="$TMP/irrelevant-db" \
    SPIRA_BD="$FAILING_BD" \
    SPIRA_FAYTHS="" \
    SPIRA_LAND_STALE=999999 \
    SPIRA_AUDIT_STALE=999999 \
    SPIRA_SYSTEMCTL="$STUBS/mock-systemctl" \
    SPIRA_LAUNCH="$STUBS/mock-launch" \
    SPIRA_NOTIFY="$STUBS/mock-notify" \
    PATH="$STUBS:$PATH" \
    sentinel 2>&1)"
rc=$?
is   "exits 1 when bd cannot reach the database"  "1" "$rc"
want "reports DATABASE UNREADABLE"                 "DATABASE UNREADABLE" "$out_unreadable"
lack "does NOT report a state line"               "state: " "$out_unreadable"
lack "does NOT report a completed pass"            "pass complete" "$out_unreadable"

# ======================================================================================
echo
echo "real passes — fill, dispatch and the audit worker (needs a Dolt fixture):"
# ======================================================================================
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-sentinel-pass
testdb_up sentinel_pass || { echo "test-sentinel-pass: could not build fixture"; exit 1; }
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-epic1","title":"epic","status":"open","issue_type":"epic","labels":["plan"]}
{"id":"sp-b1","title":"bead 1","status":"open","issue_type":"task","labels":["plan"]}
{"id":"sp-b2","title":"bead 2","status":"open","issue_type":"task","labels":["plan"]}
{"id":"sp-b3","title":"bead 3","status":"open","issue_type":"task","labels":["plan"]}
{"id":"sp-b4","title":"bead 4","status":"open","issue_type":"task","labels":["plan"]}
{"id":"sp-b5","title":"bead 5","status":"open","issue_type":"task","labels":["plan"]}
JSONL

SUMMON_LOG="$TMP/summon.log"
SENDING_LOG="$TMP/sending.log"
# sending.sh SLEEPS before it records itself — this is the old inline cost (CHECK 6b's
# per-branch walk) that used to sit between the pass starting and CHECK 7 running. A
# normal pass must never pay this; only `--audit` may.
printf '#!/bin/sh\nsleep 2\necho "called $*" >> "%s"\n' "$SENDING_LOG" > "$STUBS/sending"
chmod +x "$STUBS/sending"
# mock-summon records each summon so the fill assertion can count them.
printf '#!/bin/sh\necho summoned >> "$SUMMON_LOG"\n' > "$STUBS/mock-summon"; chmod +x "$STUBS/mock-summon"

# run_pass <run-dir> [script-arg] [KEY=VAL ...] → combined stdout+stderr of one sentinel
# pass. SPIRA_RUN is the caller-supplied dir (allows pre-populating and reusing stamp
# files). The first positional argument after run-dir is sentinel.sh's OWN argv (empty
# string for a normal pass, "--audit" for the decoupled worker); everything after that is
# an extra env-var override.
run_pass() {
    local run="$1" arg="$2"; shift 2
    mkdir -p "$run"
    local -a sarg=(); [ -n "$arg" ] && sarg=("$arg")
    env -i \
        PATH="$PATH" HOME="$HOME" \
        SPIRA_HOME="$STUBS" PATH="$STUBS:$PATH" \
        SPIRA_RUN="$run" \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_BD="$SPIRA_BD" \
        SPIRA_PATH="$SPIRA_PATH" \
        SPIRA_SKIP_RECLAIM=1 \
        SPIRA_LAND_STALE=999999 \
        SPIRA_AUDIT_STALE=999999 \
        SPIRA_SYSTEMCTL="$STUBS/mock-systemctl" \
        SPIRA_LAUNCH="$STUBS/mock-launch" \
        SPIRA_SUMMON="$STUBS/mock-summon" \
        SPIRA_NOTIFY="$STUBS/mock-notify" \
        SUMMON_LOG="$SUMMON_LOG" \
        SENDING_LOG="$SENDING_LOG" \
        SPIRA_FAYTHS=builder SPIRA_SCOPE_LABEL= SPIRA_MAX_AEONS=3 \
        PATH="$STUBS:$PATH" \
        "$@" \
        sentinel "${sarg[@]}" 2>&1
}

_run="$TMP/run-pass"
rm -f "$SUMMON_LOG" "$SENDING_LOG" "$LAUNCH_ARGV"
pass1_out="$(run_pass "$_run" "")"
is   "pass 1 fill: pool=3 + 5 ready beads -> 3 summons" \
     "3" "$(grep -c . "$SUMMON_LOG" 2>/dev/null || echo 0)"
want "pass 1 order: CHECK7 line appears in log" "CHECK7" "$pass1_out"
lack "pass 1: does NOT report DATABASE UNREADABLE (positive control: DB is readable)" \
     "DATABASE UNREADABLE" "$pass1_out"
# THE REGRESSION (sp-994y9): sending.sh sleeps 2s; a pass that still called it inline (the
# pre-fix shape) would take at least 2s. A wall-clock "<2s" assertion here flipped under
# load (law-a-test-that-flips-is-deleted); the deterministic assertion below — sending.sh
# never called inline — covers the same property without a timing race.
is   "pass 1: sending.sh NOT called inline by a normal pass" \
     "0" "$(grep -c . "$SENDING_LOG" 2>/dev/null || echo 0)"
want "pass 1: dispatches the audit worker (--unit=spira-audit line in launch argv)" \
     "--unit=spira-audit" "$(cat "$LAUNCH_ARGV" 2>/dev/null)"
want "pass 1: audit dispatch's own argv names the sentinel binary --audit" \
     "/sentinel --audit" "$(cat "$LAUNCH_ARGV" 2>/dev/null)"

# ======================================================================================
echo
echo "CHECK6/audit dispatch — no CPUQuota and no Nice on either worker (sp-b4oct):"
# ======================================================================================
# law-isolate-greedy-work-in-vms: the OS schedules the landing and audit workers. Each check
# is on the worker's own argv line, and each line's presence is asserted first so an absent
# dispatch cannot pass the absence check (law-absence-needs-a-positive-control).
_land_argv="$(grep -- '--unit=spira-landing' "$LAUNCH_ARGV" 2>/dev/null)"
_audit_argv="$(grep -- '--unit=spira-audit' "$LAUNCH_ARGV" 2>/dev/null)"
want   "land dispatch argv was captured"  "--unit=spira-landing" "$_land_argv"
want   "audit dispatch argv was captured" "--unit=spira-audit"   "$_audit_argv"
nowant "land dispatch carries no CPUQuota"  "CPUQuota" "$_land_argv"
nowant "land dispatch carries no Nice"      "Nice="    "$_land_argv"
nowant "audit dispatch carries no CPUQuota" "CPUQuota" "$_audit_argv"
nowant "audit dispatch carries no Nice"     "Nice="    "$_audit_argv"

# The retired knob is inert: setting it in the environment adds no quota.
rm -f "$LAUNCH_ARGV" "$SUMMON_LOG" "$SENDING_LOG"
out_customquota="$(run_pass "$_run" "" SPIRA_LAND_CPU_QUOTA=55)"
_land_argv="$(grep -- '--unit=spira-landing' "$LAUNCH_ARGV" 2>/dev/null)"
want   "retired SPIRA_LAND_CPU_QUOTA: the land dispatch still happens" \
       "--unit=spira-landing" "$_land_argv"
nowant "retired SPIRA_LAND_CPU_QUOTA=55 adds no CPUQuota" "CPUQuota" "$_land_argv"
lack "retired quota knob: pass still runs cleanly" "DATABASE UNREADABLE" "$out_customquota"

# ======================================================================================
echo
echo "--audit — the decoupled worker actually does the walk, and never lands or summons:"
# ======================================================================================
rm -f "$SUMMON_LOG" "$SENDING_LOG" "$LAUNCH_ARGV"
audit_out="$(run_pass "$_run" "--audit")"
is   "audit: sending.sh IS called (with --skip-queue, sp-jci6o)" \
     "1" "$(grep -c . "$SENDING_LOG" 2>/dev/null || echo 0)"
want "audit: sending.sh called with --skip-queue" \
     "--skip-queue" "$(cat "$SENDING_LOG" 2>/dev/null)"
want "audit: CHECK4 runs (lifecycle off, so it declines to examine)" "CHECK4 lifecycle_enforce is off" "$audit_out"
is   "audit: never summons (CHECK 7 is normal-pass only)" \
     "0" "$(grep -c . "$SUMMON_LOG" 2>/dev/null || echo 0)"
is   "audit: never dispatches landing (CHECK 6 is normal-pass only)" \
     "0" "$(grep -c -- '--unit=spira-landing' "$LAUNCH_ARGV" 2>/dev/null || echo 0)"
is   "audit: writes its own status file (the next pass's positive control)" \
     "1" "$([ -f "$_run/audit.status" ] && echo 1 || echo 0)"
want "audit: status file records a completion timestamp" \
     "SP_AUDIT_AT=" "$(cat "$_run/audit.status" 2>/dev/null)"

tl_summary
