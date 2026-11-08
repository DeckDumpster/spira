#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: spira/sim/** sim/** spira-runtime/src/tasks/** spira-runtime/src/tasks/d/watch_perf_happy.rs
# lane: sim
# pids: 400
#
# test-sim-happy-path.sh — `sim run happy-path` as `watch.perf_happy` runs it in production
# (`sim run happy-path --seed 7 --keep`, then the kept run's exec.log, then `sim world down`).
# happy-path is a loop world (ruling D6): the service's registrations in-process on doubles, one
# bead filed to LANDED and its landed head published by PR (spira/sim/src/loopworld.rs). What the
# run proves about the loop is asserted in spira/sim/tests/it/loop_ports.rs
# `happy_path_the_shipped_loop_scenario_lands_and_publishes_and_logs_its_effects`; this suite holds
# the staged binary to the command-line contract the perf watch reads.
#
#   1. `sim run happy-path --seed 7 --keep` exits 0 and prints the seed first; it is killed at
#      RUN_LIMIT, the perf watch's own run limit, a hang guard and not a budget.
#   2. The kept run directory `sim-run-*-7` holds an exec.log of `=== t=… exit status: … in …ms`
#      lines, the format perf-watch's `slowest_phase` reads, and `sim world down` removes it.
#   3. CONTROL: the same scenario cut off before the bead can land exits non-zero naming the goal
#      and the seed, so (1) is not a run that exits 0 whatever happens.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=360   # RUN_LIMIT is 280 and the sim run is one case

[ -n "${SPIRA_RELEASE:-}" ] || bail "SPIRA_RELEASE is not set: run via testenv --with-bins"
SIM="$SPIRA_RELEASE/bin/sim"
[ -x "$SIM" ] || bail "no sim in the staged release: $SIM"

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
SEED=7
RUN_LIMIT=280
SCENARIO="$HERE/sim/scenarios/happy-path.toml"

NOLOC=(-u SPIRA_TOML -u SPIRA_RUN -u SPIRA_DB -u SPIRA_LC_PASSWORD_FILE -u SPIRA_LC_SOCKET -u SPIRA_LC_HOST -u SPIRA_LC_PORT -u SPIRA_LC_USER -u SPIRA_HOME -u SPIRA_WORK_BEAD_ID)

sim_here() {  # sim_here <seconds> <args...> — sim from this tree, no production locator
    local limit="$1"; shift
    # batch-job: one in-process loop run; the caller names its wall-time limit
    (cd "$HERE/.." && timeout "$limit" env "${NOLOC[@]}" TMPDIR="$T" "$SIM" "$@")
}

# --- 1. the run, as the perf watch starts it --------------------------------------------------
START=$SECONDS
out="$(sim_here "$RUN_LIMIT" run happy-path --seed "$SEED" --keep 2>&1)"; rc=$?
echo "# sim run took $((SECONDS - START))s"
printf '%s\n' "$out" | grep '^sim: profile' | cut -c1-300 | sed 's/^/# /'
# Said before the assertion: the first failing case ends the suite, so a line after it never prints.
[ "$rc" = 0 ] || printf '%s\n' "$out" | tail -6 | cut -c1-300 | sed 's/^/# run: /'
wantrc "sim run happy-path exits 0 (killed at ${RUN_LIMIT}s if it hangs: rc 124)" 0 "$rc"
is "sim run prints the seed first" "sim seed: $SEED" "$(printf '%s\n' "$out" | head -1)"
want "sim run says ok for the seed" "sim: ok seed $SEED" "$out"

# --- 2. the kept run directory -----------------------------------------------------------------
W="$(ls -d "$T"/sim-run-*-"$SEED" 2>/dev/null | head -1)"
[ -n "$W" ] && [ -d "$W" ] || bail "no run directory was kept under $T"
[ -s "$W/exec.log" ] && ok "the kept run holds an exec.log" || bad "the kept run holds an exec.log" "missing or empty"
is "every exec.log line is one effect in the world's format" 0 "$(grep -cvE '^=== t=[0-9]+ [^ ]+ exit status: [01] in [0-9]+ms$' "$W/exec.log")"
want "the landed head was published by PR" " publish.pr_create exit status: 0 in " "$(cat "$W/exec.log")"
sim_here 60 world down "$W" >/dev/null 2>&1
[ ! -e "$W" ] && ok "sim world down removes the kept run" || bad "sim world down removes the kept run" "$W remains"

# --- 3. CONTROL: a run cut off before the landing fails, naming the goal and the seed ----------
sed 's/^until = .*/until = 300000/' "$SCENARIO" > "$T/never.toml"
out="$(sim_here 90 run "$T/never.toml" --seed 3 2>&1)"; rc=$?
wantrc "control: a run that ends before its bead lands exits non-zero" 1 "$rc"
want "control: the failure names the seed and the unmet goal" "seed 3: goal sp-hp01:LANDED unmet" "$out"
for w in "$T"/sim-run-*-3; do [ -d "$w" ] && sim_here 60 world down "$w" >/dev/null 2>&1; done

tl_summary
