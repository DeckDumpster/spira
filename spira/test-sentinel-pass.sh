#!/usr/bin/env bash
#
# test-sentinel-pass.sh — real end-to-end sentinel passes: fill, the landing dispatch, no
#   audit worker, and a database sentinel cannot reach exits 1 without ever reporting a
#   state or a completed pass.
#
#   ./test-sentinel-pass.sh
#
# HOST OF test-sentinel-order.sh (dispatch.md D8, sp-9ce60.5), which also absorbs
# test-sentinel-capacity.sh. Cut from 9 real passes total (5 + 2 + 2, across the three
# merged files) to 2 Dolt-backed passes plus one DB-unreadable check that needs no
# database at all, now that the per-case arithmetic those passes used to be the ONLY
# coverage for has its own T1 tables:
#   - the fill loop's pool math and per-persona cap  -> ck7_pool/ck7_fill_cap (G2, G3),
#     tested directly in sentinel's own call::tests (wave 4.27, sp-gzmd2 — ported from
#     bash, where test-watchtower-throttle.sh used to call them)
#   - lane rotation order                            -> lane_rotate (G1),
#     tested directly in sentinel's own call::tests, ditto
#   - CHECK 8's firing predicate                      -> sentinel's judge::tests::judgement_table
#     (G15; check8_should_judge/test-check8-progressed.sh retired at sp-8itaf, zero live callers)
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
#   SPIRA_LAUNCH, SPIRA_CALL and SPIRA_SYSTEMCTL seams pointed at stubs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"; trap 'testdb_drop 2>/dev/null; rm -rf "$TMP"' EXIT INT TERM

STUBS="$TMP/stubs"
mkdir -p "$STUBS"
# The lifecycle machine is the only mode (sp-v62vn), so the ready set is the machine's: the
# stub's `list` seeds a READY row for each work bead the bd fixture below declares.
cat > "$STUBS/spira-lc" <<'LCSTUB'
#!/bin/sh
[ "$1" = content-rows ] && exec "${SPIRA_BD:-bd}" -C "${SPIRA_DB:-.}" list --id "$3" --status all --limit 0 --json
if [ "$1" = list ]; then
    printf '['
    sep=''
    for id in sp-b1 sp-b2 sp-b3 sp-b4 sp-b5; do
        printf '%s{"bead_id":"%s","state":"READY","holder":null,"lease_until":null,"holds":[]}' "$sep" "$id"
        sep=','
    done
    printf ']\n'
fi
exit 0
LCSTUB
chmod +x "$STUBS/spira-lc"
for _s in pilgrimage.sh reflect.sh; do
    printf '#!/bin/sh\n' > "$STUBS/$_s"; chmod +x "$STUBS/$_s"
done
# strand ALSO backs lib.sh's dwarf_alive/dwarf_count/dwarves_live_total/dwarves_live_lanes shims
# now (wave 4.23, sp-0ffox) — a bare no-op here would make `$(dwarf_count ...)` inside
# lib.sh's pass-start summary print nothing, not "0", and blow up the arithmetic that adds
# it. A plain no-op is still correct for strand's OWN verbs (report/check/throttle-state),
# which is all this suite needs from it.
cat > "$STUBS/strand" <<'STRANDSTUB'
#!/bin/sh
case "$1" in
    dwarf-alive) exit 1 ;;
    dwarf-count|dwarves-live-total|dwarves-live-lanes) printf '0' ;;
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
# mock-launch stands in for systemd-run: it records every dispatch's own argv.
printf '#!/bin/sh\necho "$@" >> "%s"\nexit 0\n' "$LAUNCH_ARGV" > "$STUBS/mock-launch"; chmod +x "$STUBS/mock-launch"
printf '#!/bin/sh\nexit 0\n'         > "$STUBS/mock-notify";    chmod +x "$STUBS/mock-notify"
touch "$STUBS/repo-map"   # empty: no repos, Hel finds nothing to walk
# THE REAL RUNES, symlinked once up front: `ln -sf` onto a directory that already
# exists creates the link INSIDE it instead of replacing it, so this must run before
# anything else ever creates $STUBS/runes as a plain directory.
ln -s "$HERE/runes" "$STUBS/runes"

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
tl_config SPIRA_RUN="$_run_unreadable" SPIRA_DB="$TMP/irrelevant-db" SPIRA_BD="$FAILING_BD" \
    SPIRA_RUNES="" SPIRA_NOTIFY="$STUBS/mock-notify" SPIRA_RUNES_DIR="$STUBS/runes"
out_unreadable="$(env -i \
    PATH="$STUBS:$PATH" HOME="$HOME" \
    SPIRA_HOME="$STUBS" \
    SPIRA_TOML="$SPIRA_TOML" \
    SPIRA_LAND_STALE=999999 \
    SPIRA_SYSTEMCTL="$STUBS/mock-systemctl" \
    SPIRA_LAUNCH="$STUBS/mock-launch" \
    sentinel 2>&1)"
rc=$?
is   "exits 1 when bd cannot reach the database"  "1" "$rc"
want "reports DATABASE UNREADABLE"                 "DATABASE UNREADABLE" "$out_unreadable"
lack "does NOT report a state line"               "state: " "$out_unreadable"
lack "does NOT report a completed pass"            "pass complete" "$out_unreadable"

# ======================================================================================
echo
echo "real passes — fill and dispatch (needs a Dolt fixture):"
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

CALL_LOG="$TMP/call.log"
HEL_LOG="$TMP/hel.log"
printf '#!/bin/sh\necho "called $*" >> "%s"\n' "$HEL_LOG" > "$STUBS/hel"
chmod +x "$STUBS/hel"
# mock-call records each call so the fill assertion can count them.
printf '#!/bin/sh\necho called >> "$CALL_LOG"\n' > "$STUBS/mock-call"; chmod +x "$STUBS/mock-call"

# run_pass <run-dir> [script-arg] [KEY=VAL ...] → combined stdout+stderr of one sentinel
# pass. SPIRA_RUN is the caller-supplied dir (allows pre-populating and reusing stamp
# files). The first positional argument after run-dir is sentinel's OWN argv (empty string
# for a normal pass); everything after that is an extra env-var override.
run_pass() {
    local run="$1" arg="$2"; shift 2
    mkdir -p "$run"
    local -a sarg=(); [ -n "$arg" ] && sarg=("$arg")
    tl_config SPIRA_RUN="$run" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$SPIRA_BD" SPIRA_PATH="$SPIRA_PATH" \
        SPIRA_NOTIFY="$STUBS/mock-notify" SPIRA_RUNES=builder SPIRA_SCOPE_LABEL= \
        SPIRA_MAX_DWARVES=3 SPIRA_RUNES_DIR="$STUBS/runes"
    env -i \
        PATH="$STUBS:$PATH" HOME="$TMP/home" \
        SPIRA_HOME="$STUBS" \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_SKIP_RECLAIM=1 \
        SPIRA_LAND_STALE=999999 \
            SPIRA_SYSTEMCTL="$STUBS/mock-systemctl" \
        SPIRA_LAUNCH="$STUBS/mock-launch" \
        SPIRA_CALL="$STUBS/mock-call" \
        CALL_LOG="$CALL_LOG" \
        HEL_LOG="$HEL_LOG" \
        "$@" \
        sentinel "${sarg[@]}" 2>&1
}

mkdir -p "$TMP/home"
_run="$TMP/run-pass"
rm -f "$CALL_LOG" "$HEL_LOG" "$LAUNCH_ARGV"
pass1_out="$(run_pass "$_run" "")"
is   "pass 1 fill: pool=3 + 5 ready beads -> 3 calls" \
     "3" "$(grep -c . "$CALL_LOG" 2>/dev/null || echo 0)"
want "pass 1 order: CHECK7 line appears in log" "CHECK7" "$pass1_out"
lack "pass 1: does NOT report DATABASE UNREADABLE (positive control: DB is readable)" \
     "DATABASE UNREADABLE" "$pass1_out"
is   "pass 1: hel is not called by the sentinel" \
     "0" "$(grep -c . "$HEL_LOG" 2>/dev/null || echo 0)"
nowant "pass 1: no audit worker is dispatched" \
     "spira-audit" "$(cat "$LAUNCH_ARGV" 2>/dev/null)"

# ======================================================================================
echo
echo "CHECK6 dispatch — no CPUQuota and no Nice on the landing worker:"
# ======================================================================================
# The line's presence is asserted first so an absent dispatch cannot pass the absence
# check (law-absence-needs-a-positive-control).
_land_argv="$(grep -- '--unit=spira-landing' "$LAUNCH_ARGV" 2>/dev/null)"
want   "land dispatch argv was captured"  "--unit=spira-landing" "$_land_argv"
nowant "land dispatch carries no CPUQuota"  "CPUQuota" "$_land_argv"
nowant "land dispatch carries no Nice"      "Nice="    "$_land_argv"

# The retired knob is inert: setting it in the environment adds no quota.
rm -f "$LAUNCH_ARGV" "$CALL_LOG" "$HEL_LOG"
out_customquota="$(run_pass "$_run" "" SPIRA_LAND_CPU_QUOTA=55)"
_land_argv="$(grep -- '--unit=spira-landing' "$LAUNCH_ARGV" 2>/dev/null)"
want   "retired SPIRA_LAND_CPU_QUOTA: the land dispatch still happens" \
       "--unit=spira-landing" "$_land_argv"
nowant "retired SPIRA_LAND_CPU_QUOTA=55 adds no CPUQuota" "CPUQuota" "$_land_argv"
lack "retired quota knob: pass still runs cleanly" "DATABASE UNREADABLE" "$out_customquota"

tl_summary
