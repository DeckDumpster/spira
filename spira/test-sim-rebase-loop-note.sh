#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: spira/sim/** spira/sim-scenario-lib.sh spira/lib.sh sim/actors.toml sim/durations.toml spira/release.sh lifecycle/src/** spira-lc/src/** work/src/** landing-pass/src/** queue/src/** batcher/src/** batcher-cut/src/** gate-worker/** systemd/spira-sentinel.* systemd/spira-rounds.* systemd/spira-sift.* systemd/spira-publish.* systemd/spira-gate-worker.*
# lane: sim
# pids: 400
#
# test-sim-rebase-loop-note.sh — the rebase-loop-note scenario in a sim world under the fixed seed list: it
# reaches its goal with every built-in and scenario invariant holding and every `expect` found.
# One suite per scenario so the worlds run side by side; SIM_SCENARIOS_FRESH=1 adds a fresh seed.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/sim-scenario-lib.sh"
sim_scenario_shard rebase-loop-note
