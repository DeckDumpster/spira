#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: round-controller/** sim/actors.toml sim/units/** sim/src/actors.rs spira/sim/src/drive.rs spira/sim/src/launch.rs spira/sim/src/main.rs spira/sim/src/runstate.rs spira/sim/src/world.rs spira/sim/scenarios/rc-launch.toml spira/sim-scenario-lib.sh
# lane: sim
# pids: 400
#
# test-sim-rc-launch.sh — the round controller's world twin of launch-collision. rc-launch runs
# `round-controller launch` in a sim world under the fixed seed list: the world's systemd stub
# sees the runner's unit named round-controller-run-<batch>-<lease-token> and runs it inline.
#
#   CONTROL: the same scenario expecting the unit under the name without its token fails naming
#   that expectation, so the world sees the unit names the stub records and the shard's exit 0 is
#   not a run that exits 0 whatever is launched.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/sim-scenario-lib.sh"

sim_controls_setup
export SPIRA_SIM_CACHE="${SPIRA_SIM_CACHE:-/tmp/sim-seed-cache}"   # the control and the shard share one seed repo
{ cat "$HERE/sim/scenarios/rc-launch.toml"; printf '\n[[expect]]\nname = "rc_unit_without_token"\nsql = "SELECT 1 FROM run_units WHERE name = '"'"'round-controller-run-rc-b1'"'"'"\n'; } > "$T/inverted.toml"
grep -q 'rc_unit_without_token' "$T/inverted.toml" || bail "control fixture was not derived from rc-launch"
sim_controls_run inverted "$T/inverted.toml"
want "control: a unit named without its token is named" "expectation rc_unit_without_token unmet: seed 3" "$(cat "$T/res.inverted.out")"
is "control: the inverted run exits 1" 1 "$(cat "$T/res.inverted.rc")"
sim_controls_no_serve
sim_controls_cleanup
trap - EXIT

sim_scenario_shard rc-launch
