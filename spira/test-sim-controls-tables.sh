#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: spira/sim/** spira/sim-scenario-lib.sh spira/lib.sh sim/actors.toml sim/durations.toml spira/release.sh
# lane: sim
# pids: 400
#
# test-sim-controls-tables.sh — CONTROL on the run-directory tables: a ROUND-LAND line in the landing
# log with no certificate on disk fails naming the invariant over the table that holds it, while the
# same bare world unplanted passes. See test-sim-controls.sh and test-sim-controls-lock.sh.
#
# The base was run-state-trail, a real landing, until the loop's units became tasks of the service
# (ruling D6): a world lands nothing now. Its property (the landing logged with its member, after
# its certificate) is spira/sim/tests/it/loop_rounds.rs
# run_state_trail_the_landing_names_its_member_after_its_certificate_and_the_publish_is_at_its_head
# and queue's open_certify_land_is_one_round_the_whole_way; this suite keeps only the invariant's
# control, on a world with no actors.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=180   # slow cases: real git/dolt on a loaded VM
. "$HERE/sim-scenario-lib.sh"
sim_controls_setup

cat > "$T/bare.toml" <<'TOML'
epoch = 1800000000
horizon = 150000

[[step]]
at = 100000
command = "true"

[[step]]
at = 101000
command = "true"
TOML
{ cat "$T/bare.toml"; cat <<'TOML'

[[step]]
at = 100500
command = """printf 'QUEUE ROUND-LAND 1 repo=sim batch=forged head=0 members=sp-rs01:0\\n' >> "$SIM_WORLD/run/landing.log" """
TOML
} > "$T/forged-land.toml"

sim_controls_run bare "$T/bare.toml" &
sim_controls_run forged-land "$T/forged-land.toml" &
wait

want "control: a landing line with no certificate trips the table invariant" "invariant inv_round_land_has_certificate violated: seed 3" "$(cat "$T/res.forged-land.out")"
is "control: the forged-landing run exits 1" 1 "$(cat "$T/res.forged-land.rc")"
is "the bare world itself passes" 0 "$(cat "$T/res.bare.rc")"

sim_controls_no_serve
tl_summary
