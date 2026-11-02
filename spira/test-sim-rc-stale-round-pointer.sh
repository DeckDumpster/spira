#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: round-controller/src/sifts.rs spira/sim/scenarios/rc-stale-round-pointer.toml spira/sim/scenarios/steps/rc-stale-round.sh spira/sim/** spira/sim-scenario-lib.sh spira/lib.sh sim/actors.toml sim/durations.toml queue/src/** release/src/round_recert.rs landing-pass/src/** lifecycle/src/** spira-lc/src/** batcher/src/** gate-worker/** systemd/spira-sentinel.* systemd/spira-sift.*
# lane: sim
# pids: 400
#
# test-sim-rc-stale-round-pointer.sh — two rounds open, the older one's lifecycle row is abandoned
# behind its record by requeue-orphans, and round-recert still certifies and lands the younger:
# no reader takes the abandoned round's record for the open round.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=180   # slow cases: real git/dolt on a loaded VM
. "$HERE/sim-scenario-lib.sh"
sim_scenario_shard rc-stale-round-pointer
