#!/usr/bin/env bash
# tier: T3
# requires: testenv
# covers: queue/src/ops/batch.rs queue/src/ops/forge_batch.rs queue/src/ops/verdict.rs batcher-cut/** sim/actors.toml sim/units/** spira/sim/src/gh.rs spira/sim/src/world.rs spira/sim/scenarios/rc-forge-open-crash.toml spira/sim/scenarios/steps/rc-forge-open-crash.sh spira/sim-scenario-lib.sh UC-landing-merge-queue-58
# lane: sim
# pids: 400
#
# test-sim-rc-forge-open-crash.sh — a `queue batch open` killed between the forge's PR create and
# the move of the batch record out of forge.phase=opening leaves one batch PR and a record the
# forge batcher's next passes adopt: never a second PR, never a merge.
#
#   CONTROL: the same scenario expecting the check step to fail names that expectation, so the
#   world sees the step's exit and the shard's exit 0 is not a run that passes whatever it counts.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/sim-scenario-lib.sh"

sim_controls_setup
export SPIRA_SIM_CACHE="${SPIRA_SIM_CACHE:-/tmp/sim-seed-cache}"
{ cat "$HERE/sim/scenarios/rc-forge-open-crash.toml"; printf '\n[[expect]]\nname = "batch_check_failed"\nsql = "SELECT 1 FROM events WHERE kind = '"'"'step'"'"' AND actor = '"'"'one-adopted-batch'"'"' AND exit = 9"\n'; } > "$T/inverted.toml"
grep -q 'batch_check_failed' "$T/inverted.toml" || bail "control fixture was not derived from rc-forge-open-crash"
sim_controls_run inverted "$T/inverted.toml"
want "control: a passing check is named" "expectation batch_check_failed unmet: seed 3" "$(cat "$T/res.inverted.out")"
is "control: the inverted run exits 1" 1 "$(cat "$T/res.inverted.rc")"
sim_controls_no_serve
sim_controls_cleanup
trap - EXIT

sim_scenario_shard rc-forge-open-crash
