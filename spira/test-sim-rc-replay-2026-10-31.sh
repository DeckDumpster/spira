#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: round-controller/** sim/actors.toml sim/units/** sim/src/actors.rs spira/sim/src/drive.rs spira/sim/scenarios/rc-replay-2026-10-31.toml spira/sim/scenarios/steps/rc-replay-seed.sh spira/sim/scenarios/steps/rc-replay-check.sh spira/sim-scenario-lib.sh
# lane: sim
# pids: 400
#
# test-sim-rc-replay-2026-10-31.sh — the round controller's world twin of the 10-31 replay.
# rc-replay-2026-10-31 seeds one shard of the 10-31 slice into the world's control store (five
# ended passes of the repeat-red member and their Image fault, a hand-driven parent with three
# GREEN children stacked on it, a stale record) and runs the sim-only round-controller actor over
# it under the fixed seed list: every pass exits 0, each check step named for a replay
# expectation holds, and the world's run state is untouched.
#
#   CONTROL: the same scenario expecting the actor never to have run fails naming that
#   expectation, so the world sees the actor; with its seed step emptied it fails
#   no_green_round_abandoned by name, so the checks read what the store holds; and seeded with
#   no hand driver it fails driver_recut_wastes_at_most_one_pass by name, so that check reads
#   what the controller's Lander wrote.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/sim-scenario-lib.sh"

sim_controls_setup
export SPIRA_SIM_CACHE="${SPIRA_SIM_CACHE:-/tmp/sim-seed-cache}"   # the control and the shard share one seed repo
{ cat "$HERE/sim/scenarios/rc-replay-2026-10-31.toml"; printf '\n[[expect]]\nname = "rc_never_ran"\nsql = "SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM events WHERE actor = '"'"'round-controller'"'"' AND kind = '"'"'complete'"'"')"\n'; } > "$T/inverted.toml"
grep -q 'rc_never_ran' "$T/inverted.toml" || bail "control fixture was not derived from rc-replay-2026-10-31"
sim_controls_run inverted "$T/inverted.toml"
want "control: a world that cannot see the actor is named" "expectation rc_never_ran unmet: seed 3" "$(cat "$T/res.inverted.out")"
is "control: the inverted run exits 1" 1 "$(cat "$T/res.inverted.rc")"
sed 's|command = "sh spira/sim/scenarios/steps/rc-replay-seed.sh"|command = "true"|' "$HERE/sim/scenarios/rc-replay-2026-10-31.toml" > "$T/unseeded.toml"
grep -q 'command = "true"' "$T/unseeded.toml" || bail "control fixture did not drop rc-replay-2026-10-31's seed step"
sim_controls_run unseeded "$T/unseeded.toml"
want "control: a check step reads the store, so an unseeded world fails it by name" "expectation no_green_round_abandoned unmet: seed 3" "$(cat "$T/res.unseeded.out")"
is "control: the unseeded run exits 1" 1 "$(cat "$T/res.unseeded.rc")"
sed 's|command = "sh spira/sim/scenarios/steps/rc-replay-seed.sh"|command = "sh spira/sim/scenarios/steps/rc-replay-seed.sh no-driver"|' "$HERE/sim/scenarios/rc-replay-2026-10-31.toml" > "$T/undriven.toml"
grep -q 'rc-replay-seed.sh no-driver' "$T/undriven.toml" || bail "control fixture did not drop rc-replay-2026-10-31's hand driver"
sim_controls_run undriven "$T/undriven.toml"
want "control: the Lander reads the hand driver's lease, so a parent without one fails by name" "expectation driver_recut_wastes_at_most_one_pass unmet: seed 3" "$(cat "$T/res.undriven.out")"
is "control: the undriven run exits 1" 1 "$(cat "$T/res.undriven.rc")"
sim_controls_no_serve
sim_controls_cleanup
trap - EXIT

sim_scenario_shard rc-replay-2026-10-31
