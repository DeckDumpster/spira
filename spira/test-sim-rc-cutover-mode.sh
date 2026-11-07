#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: spira/sim/scenarios/rc-cutover-mode.toml spira/sim/scenarios/steps/rc-cutover-mode.sh spira/sim/scenarios/steps/rc-cutover-actor.sh spira/sim/scenarios/steps/runbook.sh docs/runbooks/rounds.md spira/sim/** spira/sim-scenario-lib.sh spira-world/src/** spira-config/src/scoped_stop.rs sentinel/src/call.rs landing-pass/src/** czar-pass/src/** reconciler/src/** queue/src/** release/src/round_recert.rs round-controller/src/** sim/actors.toml
# lane: sim
# pids: 400
#
# test-sim-rc-cutover-mode.sh — a stopped world entered into cutover mode: round-recert lands a
# round while nothing is claimed, landing-pass lands nothing else and czar-pass and the
# reconciler skip; world stop re-halts. Each move is a checked block of the rounds runbook.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=400   # slow cases: real git/dolt on a loaded VM
. "$HERE/sim-scenario-lib.sh"
sim_scenario_shard rc-cutover-mode
