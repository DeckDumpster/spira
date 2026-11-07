#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: round-controller/** spira-lc/src/** state-store/src/** spira/sim/src/world.rs spira/sim/src/launch.rs spira/sim/src/runstate.rs spira/sim/scenarios/rc-orphan-redrive.toml spira/sim/scenarios/steps/rc-orphan-redrive.sh spira/sim-scenario-lib.sh spira/conf.d/SPIRA_LC_*
# lane: sim
# pids: 400
#
# test-sim-rc-orphan-redrive.sh — the round controller's orphan step on the real store, in a sim
# world: a round whose lease lapsed is found by `round-controller pass`, which cannot show its old
# runner gone and so holds it, taking no lease or slot and writing no attempt; a round never leased
# is not named.
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
{ cat "$HERE/sim/scenarios/rc-orphan-redrive.toml"; printf '\n[[expect]]\nname = "rc_orphan_not_held"\nsql = "SELECT 1 FROM events WHERE kind = '"'"'step'"'"' AND actor = '"'"'orphan-held'"'"' AND exit = 9"\n'; } > "$T/inverted.toml"
grep -q 'rc_orphan_not_held' "$T/inverted.toml" || bail "control fixture was not derived from rc-orphan-redrive"
sim_controls_run inverted "$T/inverted.toml"
want "control: a failed hold check is named" "expectation rc_orphan_not_held unmet: seed 3" "$(cat "$T/res.inverted.out")"
is "control: the inverted run exits 1" 1 "$(cat "$T/res.inverted.rc")"
sim_controls_no_serve
sim_controls_cleanup
trap - EXIT

sim_scenario_shard rc-orphan-redrive
