#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: round-controller/** sim/actors.toml sim/units/** sim/src/actors.rs spira/sim/src/drive.rs spira/sim/src/roundvm.rs spira/sim/scenarios/rc-runner-lease-loss.toml spira/sim-scenario-lib.sh
# lane: sim
# pids: 400
#
# test-sim-rc-runner-lease-loss.sh — the round controller's world twin of lease-lost-kills-vm.
# rc-runner-lease-loss runs `round-controller run` in a sim world under the fixed seed list beside
# the sim-only pass: a runner whose first renewal is refused, with no round under it, exits 9, starts no round VM and leaves
# the world's run state untouched.
#
#   CONTROL: the same scenario expecting the runner to have recorded a pass (exit 0) fails naming
#   that expectation, so the world sees the runner's exit and the shard's exit 0 is not a run that
#   exits 0 whatever happens.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/sim-scenario-lib.sh"

sim_controls_setup
export SPIRA_SIM_CACHE="${SPIRA_SIM_CACHE:-/tmp/sim-seed-cache}"   # the control and the shard share one seed repo
{ cat "$HERE/sim/scenarios/rc-runner-lease-loss.toml"; printf '\n[[expect]]\nname = "rc_runner_recorded"\nsql = "SELECT 1 FROM events WHERE actor = '"'"'rc-runner'"'"' AND kind = '"'"'complete'"'"' AND exit = 0"\n'; } > "$T/inverted.toml"
grep -q 'rc_runner_recorded' "$T/inverted.toml" || bail "control fixture was not derived from rc-runner-lease-loss"
sim_controls_run inverted "$T/inverted.toml"
want "control: a runner that recorded nothing is named" "expectation rc_runner_recorded unmet: seed 3" "$(cat "$T/res.inverted.out")"
is "control: the inverted run exits 1" 1 "$(cat "$T/res.inverted.rc")"
sim_controls_no_serve
sim_controls_cleanup
trap - EXIT

sim_scenario_shard rc-runner-lease-loss
