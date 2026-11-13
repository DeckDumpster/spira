#!/usr/bin/env bash
# tier: T3
# requires: testenv
# covers: bead/** spira/sim/scenarios/bead-contract.toml spira/runes/builder.rune spira/sim-scenario-lib.sh
# lane: sim
# pids: 400
#
# test-sim-bead-contract.sh — the bead.sh contract scenario, run through the real filing path.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/sim-scenario-lib.sh"

sim_scenario_shard bead-contract
