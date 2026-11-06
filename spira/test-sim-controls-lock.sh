#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: spira/sim/** spira/sim-scenario-lib.sh spira/lib.sh sim/actors.toml sim/durations.toml spira/release.sh
# lane: sim
# pids: 400
#
# test-sim-controls-lock.sh — CONTROL on the run-directory tables: an flock still held between two
# events fails naming the invariant over the table that holds it. The unplanted bare world it is
# derived from passes in test-sim-controls-tables.sh. See test-sim-controls.sh.
#
# The base was run-state-trail until the loop's units became tasks of the service (ruling D6); the
# queue lock given back by every round verb is queue's
# each_round_verb_holds_the_queue_lock_only_for_its_own_transition_so_a_hand_abandon_gets_in_at_once.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=180   # slow cases: real git/dolt on a loaded VM
. "$HERE/sim-scenario-lib.sh"
sim_controls_setup

# The held lock is taken at 100000 and the next event, at 101000, runs while it is still held.
cat > "$T/held-lock.toml" <<'TOML'
epoch = 1800000000
horizon = 150000

[[step]]
at = 100000
command = """setsid timeout 5 flock -x "$SIM_WORLD/run/landing.lock" sleep 5 </dev/null >/dev/null 2>&1 &"""

[[step]]
at = 101000
command = "true"
TOML

sim_controls_run held-lock "$T/held-lock.toml"

want "control: a lock held between events trips the table invariant" "invariant inv_no_lock_held_between_events violated: seed 3" "$(cat "$T/res.held-lock.out")"
is "control: the held-lock run exits 1" 1 "$(cat "$T/res.held-lock.rc")"

sim_controls_no_serve
tl_summary
