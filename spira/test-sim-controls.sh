#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: spira/sim/** spira/sim-scenario-lib.sh spira/lib.sh sim/actors.toml sim/durations.toml spira/release.sh lifecycle/src/** spira-lc/src/** work/src/** landing-pass/src/** queue/src/** batcher/src/** batcher-cut/src/** gate-worker/** systemd/spira-sentinel.* systemd/spira-rounds.* systemd/spira-sift.* systemd/spira-publish.* systemd/spira-gate-worker.*
# lane: sim
# pids: 400
#
# test-sim-controls.sh — what makes the per-scenario test-sim-<scenario>.sh suites believable.
# The run-directory table controls are test-sim-controls-tables.sh and test-sim-controls-lock.sh.
#
#   1. Every scenario under spira/sim/scenarios/ has a suite that runs it.
#   2. CONTROLS: a scenario whose agent moves the branch after submitting and never submits
#      again fails naming the tip invariant, and one with an expectation nothing can meet fails
#      naming it, so a shard's exit 0 is not a run that exits 0 whatever happens.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=180   # slow cases: real git/dolt on a loaded VM
. "$HERE/sim-scenario-lib.sh"
sim_controls_setup

n=0
for f in "$HERE"/sim/scenarios/*.toml; do
    s="$(basename "$f" .toml)"; n=$((n + 1))
    [ -f "$HERE/test-sim-$s.sh" ] && ok "scenario $s has a suite" || bad "scenario $s has a suite" "no spira/test-sim-$s.sh"
done
[ "$n" -ge 9 ] || bail "fewer than nine scenarios under spira/sim/scenarios"

sed 's/"submit"\]$/"commit out\/a3.txt"]/; s/^horizon = .*/horizon = 200000/' "$HERE/sim/scenarios/resubmit-moved-tip.toml" > "$T/stale-tip.toml"
grep -q 'a3.txt' "$T/stale-tip.toml" || bail "control fixture was not derived from the resubmit scenario"
{ cat "$HERE/sim/scenarios/resubmit-moved-tip.toml"; printf '\n[[expect]]\nname = "nothing_can_meet_this"\nsql = "SELECT 1 FROM bead_state WHERE bead = '"'"'sp-nobody'"'"'"\n'; } > "$T/unmet.toml"

sim_controls_run stale-tip "$T/stale-tip.toml" &
sim_controls_run unmet "$T/unmet.toml" &
wait

want "control: a branch moved after submit trips the tip invariant" "invariant inv_tip_matches_branch violated: seed 3" "$(cat "$T/res.stale-tip.out")"
want "control: an expectation nothing meets is named" "expectation nothing_can_meet_this unmet: seed 3" "$(cat "$T/res.unmet.out")"
is "control: the stale-tip run exits 1" 1 "$(cat "$T/res.stale-tip.rc")"
is "control: the unmet run exits 1" 1 "$(cat "$T/res.unmet.rc")"

sim_controls_no_serve
tl_summary
