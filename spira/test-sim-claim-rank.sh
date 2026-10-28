#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: spira/sim/** spira/sim-scenario-lib.sh spira/lib.sh sim/actors.toml sim/durations.toml sentinel/src/** spira-claim/src/** lifecycle/src/** spira-lc/src/** systemd/spira-call.*
# lane: sim
# pids: 400
#
# test-sim-claim-rank.sh — the claim-rank scenario in a sim world under the fixed seed list: the real sentinel calls, each claim goes in spira-claim's rank under a virtual-time lease, and the run reaches its goal with every built-in and scenario invariant holding.
# One suite per scenario so the worlds run side by side; SIM_SCENARIOS_FRESH=1 adds a fresh seed.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/sim-scenario-lib.sh"
sim_scenario_shard claim-rank
