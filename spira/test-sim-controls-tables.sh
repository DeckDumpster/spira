#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: spira/sim/** spira/sim-scenario-lib.sh spira/lib.sh sim/actors.toml sim/durations.toml spira/release.sh lifecycle/src/** spira-lc/src/** work/src/** landing-pass/src/** queue/src/** batcher/src/** batcher-cut/src/** gate-worker/** systemd/spira-sentinel.* systemd/spira-rounds.* systemd/spira-sift.* systemd/spira-publish.* systemd/spira-gate-worker.*
# lane: sim
# pids: 400
#
# test-sim-controls-tables.sh — CONTROL on the run-directory tables: a ROUND-LAND line in the landing
# log with no certificate on disk fails naming the invariant over the table that holds it, while the
# unplanted run-state-trail scenario passes. See test-sim-controls.sh and test-sim-controls-lock.sh.
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
command = """printf 'QUEUE ROUND-LAND 1 repo=sim batch=forged head=0 members=sp-rs01:0\\n' >> "$SIM_WORLD/run/landing.log" """
TOML
} > "$T/forged-land.toml"

sim_controls_run trail "$RS" &
sim_controls_run forged-land "$T/forged-land.toml" &
wait

want "control: a landing line with no certificate trips the table invariant" "invariant inv_round_land_has_certificate violated: seed 3" "$(cat "$T/res.forged-land.out")"
is "control: the forged-landing run exits 1" 1 "$(cat "$T/res.forged-land.rc")"
is "the run-state-trail scenario itself passes" 0 "$(cat "$T/res.trail.rc")"

sim_controls_no_serve
tl_summary
