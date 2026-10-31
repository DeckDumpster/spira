#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: spira/sim/** spira/sim-scenario-lib.sh spira/lib.sh sim/actors.toml sim/durations.toml spira/release.sh lifecycle/src/** spira-lc/src/** work/src/** landing-pass/src/** queue/src/** batcher/src/** batcher-cut/src/** gate-worker/** systemd/spira-sentinel.* systemd/spira-rounds.* systemd/spira-sift.* systemd/spira-publish.* systemd/spira-gate-worker.*
# lane: sim
# pids: 400
#
# test-sim-controls-lock.sh — CONTROL on the run-directory tables: an flock still held between two
# events fails naming the invariant over the table that holds it. The unplanted scenario it is
# derived from passes in test-sim-controls-tables.sh. See test-sim-controls.sh.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=180   # slow cases: real git/dolt on a loaded VM
. "$HERE/sim-scenario-lib.sh"
sim_controls_setup

RS="$HERE/sim/scenarios/run-state-trail.toml"
{ cat "$RS"; cat <<'TOML'

[[step]]
at = 100000
command = """setsid timeout 5 flock -x "$SIM_WORLD/run/landing.lock" sleep 5 </dev/null >/dev/null 2>&1 &"""
TOML
} > "$T/held-lock.toml"

sim_controls_run held-lock "$T/held-lock.toml"

want "control: a lock held between events trips the table invariant" "invariant inv_no_lock_held_between_events violated: seed 3" "$(cat "$T/res.held-lock.out")"
is "control: the held-lock run exits 1" 1 "$(cat "$T/res.held-lock.rc")"

sim_controls_no_serve
tl_summary
