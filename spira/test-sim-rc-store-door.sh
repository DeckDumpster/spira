#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: round-controller/** spira-lc/src/** state-store/src/** spira/sim/src/world.rs spira/sim/scenarios/rc-store-door.toml spira/sim/scenarios/steps/rc-store-door.sh spira/sim-scenario-lib.sh spira/conf.d/SPIRA_LC_*
# lane: sim
# pids: 400
#
# test-sim-rc-store-door.sh — the round controller's door to the control store, in a sim world:
# `round-controller lease probe` takes and releases a lease through `spira-lc rc`, and
# `spira-lc watch` reads the lock's put back from the world's control store.
#
#   CONTROL: the same scenario expecting the step to have failed fails naming that expectation, so
#   the world sees the step's exit and the shard's exit 0 is not a run that passes whatever the
#   step answers.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/sim-scenario-lib.sh"

sim_controls_setup
export SPIRA_SIM_CACHE="${SPIRA_SIM_CACHE:-/tmp/sim-seed-cache}"   # the control and the shard share one seed repo
{ cat "$HERE/sim/scenarios/rc-store-door.toml"; printf '\n[[expect]]\nname = "rc_door_step_failed"\nsql = "SELECT 1 FROM events WHERE kind = '"'"'step'"'"' AND actor = '"'"'store-door'"'"' AND exit = 9"\n'; } > "$T/inverted.toml"
grep -q 'rc_door_step_failed' "$T/inverted.toml" || bail "control fixture was not derived from rc-store-door"
sim_controls_run inverted "$T/inverted.toml"
want "control: a failed door step is named" "expectation rc_door_step_failed unmet: seed 3" "$(cat "$T/res.inverted.out")"
is "control: the inverted run exits 1" 1 "$(cat "$T/res.inverted.rc")"
sim_controls_no_serve
sim_controls_cleanup
trap - EXIT

sim_scenario_shard rc-store-door
