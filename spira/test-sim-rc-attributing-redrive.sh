#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: round-controller/src/sifts.rs spira/sim/** spira/sim-scenario-lib.sh lifecycle/src/** spira-lc/src/** queue/src/** round-vm/src/** batcher-cut/src/**
# lane: sim
# pids: 400
#
# test-sim-rc-attributing-redrive.sh — the rc-attributing-redrive scenario in a sim world under the fixed
# seed list: a round whose pass 1 is red at lint is recorded PassRed, its late boundaries change
# nothing, and pass 2 starts from ATTRIBUTING and lands.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=180   # slow cases: real git/dolt on a loaded VM
. "$HERE/sim-scenario-lib.sh"
sim_scenario_shard rc-attributing-redrive
