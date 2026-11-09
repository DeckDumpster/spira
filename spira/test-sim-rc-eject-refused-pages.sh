#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: round-controller/** incident/** spira-lc/src/** state-store/src/** desired-state/src/sections/** spira/sim/src/world.rs spira/sim/scenarios/rc-eject-refused-pages.toml spira/sim/scenarios/steps/rc-eject-refused-pages.sh spira/sim-scenario-lib.sh
# lane: sim
# pids: 400
#
# test-sim-rc-eject-refused-pages.sh — a refused eject held across twenty passes is one page: the
# round controller raises EjectRefused on every pass while the round's progress holds it, and the
# alarms pass, run as often, sends one page for it and counts that page in the world's mailboxes.
#
#   CONTROL: the same scenario expecting two pages fails naming that expectation, so the world sees
#   the step's exit and the shard's exit 0 is not a run that passes whatever the step counts.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/sim-scenario-lib.sh"

sim_controls_setup
export SPIRA_SIM_CACHE="${SPIRA_SIM_CACHE:-/tmp/sim-seed-cache}"
{ cat "$HERE/sim/scenarios/rc-eject-refused-pages.toml"; printf '\n[[expect]]\nname = "two_pages_sent"\nsql = "SELECT 1 FROM events WHERE kind = '"'"'step'"'"' AND actor = '"'"'one-page'"'"' AND exit = 9"\n'; } > "$T/inverted.toml"
grep -q 'two_pages_sent' "$T/inverted.toml" || bail "control fixture was not derived from rc-eject-refused-pages"
sim_controls_run inverted "$T/inverted.toml"
want "control: a wrong page count is named" "expectation two_pages_sent unmet: seed 3" "$(cat "$T/res.inverted.out")"
is "control: the inverted run exits 1" 1 "$(cat "$T/res.inverted.rc")"
sim_controls_no_serve
sim_controls_cleanup
trap - EXIT

sim_scenario_shard rc-eject-refused-pages
