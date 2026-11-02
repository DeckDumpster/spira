#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: round-controller/** desired-state/src/sections/** spira-lc/src/sections.rs sim/actors.toml sim/units/** sim/src/actors.rs spira/sim/src/drive.rs spira/sim/scenarios/rc-section-published.toml spira/sim-scenario-lib.sh
# lane: sim
# pids: 400
#
# test-sim-rc-section-published.sh — the round controller's world twin of section-stale-parent.
# rc-section-published runs the sim-only round-controller actor, its intents and section steps
# registered, in a sim world under the fixed seed list: every pass exits 0, nothing is published,
# acked or changed, and the world's run state is untouched.
#
#   CONTROL: the same scenario expecting the actor never to have run fails naming that
#   expectation, so the world sees the actor and the shard's exit 0 is not a run that exits 0
#   whatever happens.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/sim-scenario-lib.sh"

sim_controls_setup
export SPIRA_SIM_CACHE="${SPIRA_SIM_CACHE:-/tmp/sim-seed-cache}"   # the control and the shard share one seed repo
{ cat "$HERE/sim/scenarios/rc-section-published.toml"; printf '\n[[expect]]\nname = "rc_never_ran"\nsql = "SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM events WHERE actor = '"'"'round-controller'"'"' AND kind = '"'"'complete'"'"')"\n'; } > "$T/inverted.toml"
grep -q 'rc_never_ran' "$T/inverted.toml" || bail "control fixture was not derived from rc-section-published"
sim_controls_run inverted "$T/inverted.toml"
want "control: a world that cannot see the actor is named" "expectation rc_never_ran unmet: seed 3" "$(cat "$T/res.inverted.out")"
is "control: the inverted run exits 1" 1 "$(cat "$T/res.inverted.rc")"
sim_controls_no_serve
sim_controls_cleanup
trap - EXIT

sim_scenario_shard rc-section-published
