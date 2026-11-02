#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: round-controller/** spira-lc/src/** state-store/src/** spira/sim/src/** spira/sim/scenarios/rc-cutover-readiness.toml spira/sim/scenarios/steps/rc-cutover-readiness.sh spira/sim-scenario-lib.sh
# lane: sim
# pids: 400
#
# test-sim-rc-cutover-readiness.sh — round-controller cutover-check --readiness in a sim world of
# two round repos, one ready and one with no SUBMITTED bead: the checker exits 1 naming the
# unready repo's submitted check, and the ready repo's row and the global row are green.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=180   # slow cases: real git/dolt on a loaded VM
. "$HERE/sim-scenario-lib.sh"
sim_scenario_shard rc-cutover-readiness
