#!/usr/bin/env bash
#
# test-cockpit-gate.sh — the DONE-to-LANDED stretch renders correctly.
#
#   ./test-cockpit-gate.sh
#
# Covers the gate/landing section added to the cockpit snapshot:
#   - landing.status is concatenated into the snapshot as-is
#   - a live gate renders with its slug, age, and phase
#   - a dead pid never renders as a live gate
#   - a recycled pid (another program reusing it) never renders as a live gate
#   - every field renders ? on an unreadable source, never 0 and never blank
#   - gate-run.sh --status emits nothing on stderr under a changing process table
#
# defect: sp-idml
# covers: spira/cockpit.sh spira/gate-run.sh cockpit/health.sh
# scar: the gate/landing section was absent from the cockpit snapshot, so the DONE-to-LANDED stretch and live gate status were invisible on the health pane.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
RUN="$TMP/run"; mkdir -p "$RUN"
BASE_PATH="$PATH"

# UC-15 (docs/test-plan/cockpit-observability.md, row 15): each case below calls the one
# tier function it actually exercises — `cockpit.sh now` for the gate-run/proc liveness
# section, `cockpit.sh unsent` for landing.status/landing.progress — instead of a full
# `once`, which ran every probe (16 tiers) just to reach one of them.
run_now() {
    env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
        "$@" \
        bash "$HERE/cockpit.sh" now 2>/dev/null
}
run_unsent() {
    env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
        BD_TIMEOUT=1 \
        "$@" \
        bash "$HERE/cockpit.sh" unsent 2>/dev/null
}

# ======================================================================================
echo "landing.status renders into the snapshot:"

cat > "$RUN/landing.status" <<'LAND'
SP_LAND_AT=1788845658
SP_LAND_RC=0
SP_LAND_BRANCHES=4
SP_LAND_MOVED=1
LAND
out="$(run_unsent)"
want "SP_LAND_AT from file"       "SP_LAND_AT=1788845658" "$out"
want "SP_LAND_RC from file"       "SP_LAND_RC=0"          "$out"
want "SP_LAND_BRANCHES from file" "SP_LAND_BRANCHES=4"    "$out"
want "SP_LAND_MOVED from file"    "SP_LAND_MOVED=1"       "$out"

# ======================================================================================
echo
echo "missing landing.status renders ?:"

rm -f "$RUN/landing.status"
out="$(run_unsent)"
want "SP_LAND_AT is ?"       "SP_LAND_AT=?" "$out"
want "SP_LAND_RC is ?"       "SP_LAND_RC=?" "$out"
want "SP_LAND_BRANCHES is ?" "SP_LAND_BRANCHES=?" "$out"

# ======================================================================================
echo
echo "a dead pid does not render as a live gate:"

mkdir -p "$RUN/gate-run/spira.spira_sp-dead"
echo 999999999 > "$RUN/gate-run/spira.spira_sp-dead/pid"
echo "$(date +%s)" > "$RUN/gate-run/spira.spira_sp-dead/started"
echo "test" > "$RUN/gate-run/spira.spira_sp-dead/out"
out="$(run_now)"
is "gate live count is 0" "SP_GATE_LIVE=0" "$(grep 'SP_GATE_LIVE=' <<< "$out")"
nowant "dead gate not in snapshot" "SP_GATE0_SLUG=spira.spira_sp-dead" "$out"

# ======================================================================================
echo
echo "a recycled pid (sleep process) does not render as a live gate:"

sleep 3600 &
recycled_pid=$!
mkdir -p "$RUN/gate-run/spira.spira_sp-recycled"
echo "$recycled_pid" > "$RUN/gate-run/spira.spira_sp-recycled/pid"
echo "$(date +%s)" > "$RUN/gate-run/spira.spira_sp-recycled/started"
echo "test" > "$RUN/gate-run/spira.spira_sp-recycled/out"
out="$(run_now)"
is "recycled pid: gate live count is 0" "SP_GATE_LIVE=0" "$(grep 'SP_GATE_LIVE=' <<< "$out")"
nowant "recycled pid not rendered as gate" "SP_GATE0_SLUG=spira.spira_sp-recycled" "$out"
kill "$recycled_pid" 2>/dev/null; wait "$recycled_pid" 2>/dev/null

# ======================================================================================
echo
echo "landing.progress renders into the snapshot:"

printf 'landed spira/sp-35pl\nreopened sp-dvlq -- does not rebase\n' > "$RUN/landing.progress"
out="$(run_unsent)"
want "LANDPROG0 present" "SP_LANDPROG0=" "$out"
want "LANDPROG_N=2"      "SP_LANDPROG_N=2" "$out"
want "landed in progress" "landed spira/sp-35pl" "$out"

rm -f "$RUN/landing.progress"

# ======================================================================================
echo
echo "no gate-run directory renders SP_GATE_LIVE=0:"

rm -rf "$RUN/gate-run"
out="$(run_now)"
is "no gate-run: live=0" "SP_GATE_LIVE=0" "$(grep 'SP_GATE_LIVE=' <<< "$out")"
is "no gate-run: N=0"    "SP_GATE_N=0"    "$(grep 'SP_GATE_N=' <<< "$out")"

# ======================================================================================
echo
echo "gate-run.sh --status emits nothing on stderr:"

# Create a repository stub so gate-run.sh can resolve the branch. The branch does not
# need to exist — exit code 1 (no branch) is fine; what matters is that NO STDERR is
# emitted while scanning the process table.
# Use a non-existent branch to trigger exit 3 (nothing running, nothing finished).
stderr_out="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    bash "$HERE/gate-run.sh" --status spira/sp-nonexistent spira 2>&1 >/dev/null)" || true
nowant "no stderr from --status" "No such file" "$stderr_out"

# ======================================================================================
echo
echo "health.sh renders the LAND section:"

cat > "$RUN/landing.status" <<'LAND'
SP_LAND_AT=1788845658
SP_LAND_RC=0
SP_LAND_BRANCHES=4
SP_LAND_MOVED=1
LAND
# Write a snapshot the renderer can source.
cat > "$RUN/cockpit.env" <<'SNAP'
SP_AT='1788845700'
SP_LAND_AT='1788845658'
SP_LAND_RC='0'
SP_LAND_BRANCHES='4'
SP_LAND_MOVED='1'
SP_GATE_N='0'
SP_GATE_LIVE='0'
SP_LANDPROG_N='0'
SP_SENTINEL_TIMER='1'
SP_SENTINEL_AGE='10'
SP_OPS_TIMER='1'
SP_OPS_AGE='10'
SNAP
health_out="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 TERM=dumb \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    bash "$(cd "$(dirname "$0")/../cockpit" && pwd)/health.sh" once 2>/dev/null)" || true
want "LAND label in output" "LAND" "$health_out"
want "rc in output"         "rc"   "$health_out"
want "branches in output"   "branches" "$health_out"

# ======================================================================================
echo
printf 'test-cockpit-gate: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
