#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: spira/sim/** spira/sim-scenario-lib.sh spira/lib.sh sim/actors.toml sim/durations.toml spira/release.sh lifecycle/src/** spira-lc/src/** work/src/** landing-pass/src/** queue/src/** batcher/src/** batcher-cut/src/** gate-worker/** systemd/spira-sentinel.* systemd/spira-rounds.* systemd/spira-sift.* systemd/spira-publish.* systemd/spira-gate-worker.*
# lane: sim
# pids: 400
#
# test-sim-controls.sh — what makes the per-scenario test-sim-<scenario>.sh suites believable.
#
#   1. Every scenario under spira/sim/scenarios/ has a suite that runs it.
#   2. CONTROLS: a scenario whose agent moves the branch after submitting and never submits
#      again fails naming the tip invariant, and one with an expectation nothing can meet fails
#      naming it, so a shard's exit 0 is not a run that exits 0 whatever happens.
#   3. CONTROLS on the run-directory tables: a ROUND-LAND line in the landing log with no
#      certificate on disk, and an flock still held between two events, each fail naming the
#      invariant over the table that holds it.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=180   # slow cases: real git/dolt on a loaded VM
. "$HERE/sim-scenario-lib.sh"

[ -n "${SPIRA_RELEASE:-}" ] || bail "SPIRA_RELEASE is not set: run via testenv --with-bins"
SIM="$SPIRA_RELEASE/bin/sim"
[ -x "$SIM" ] || bail "no sim in the staged release: $SIM"

n=0
for f in "$HERE"/sim/scenarios/*.toml; do
    s="$(basename "$f" .toml)"; n=$((n + 1))
    [ -f "$HERE/test-sim-$s.sh" ] && ok "scenario $s has a suite" || bad "scenario $s has a suite" "no spira/test-sim-$s.sh"
done
[ "$n" -ge 9 ] || bail "fewer than nine scenarios under spira/sim/scenarios"

T="$(mktemp -d)"
cleanup() {
    local w
    for w in "$T"/sim-run-*; do [ -d "$w" ] && timeout 60 "$SIM" world down "$w" >/dev/null 2>&1; done # batch-job: world teardown stops a whole scratch world
    rm -rf "$T"
}
trap cleanup EXIT
REPO="$T/repo"
sim_fixture_repo "$REPO"
NOLOC=(-u SPIRA_RUN -u SPIRA_DB -u SPIRA_LC_PASSWORD_FILE -u SPIRA_LC_SOCKET -u SPIRA_LC_HOST -u SPIRA_LC_PORT -u SPIRA_LC_USER -u SPIRA_HOME -u SPIRA_WORK_BEAD_ID)

sed 's/"submit"\]$/"commit out\/a3.txt"]/' "$HERE/sim/scenarios/resubmit-moved-tip.toml" > "$T/stale-tip.toml"
grep -q 'a3.txt' "$T/stale-tip.toml" || bail "control fixture was not derived from the resubmit scenario"
{ cat "$HERE/sim/scenarios/resubmit-moved-tip.toml"; printf '\n[[expect]]\nname = "nothing_can_meet_this"\nsql = "SELECT 1 FROM bead_state WHERE bead = '"'"'sp-nobody'"'"'"\n'; } > "$T/unmet.toml"

RS="$HERE/sim/scenarios/run-state-trail.toml"
{ cat "$RS"; cat <<'TOML'

[[step]]
at = 100000
command = """printf 'QUEUE ROUND-LAND 1 repo=sim batch=forged head=0 members=sp-rs01:0\\n' >> "$SIM_WORLD/run/landing.log" """
TOML
} > "$T/forged-land.toml"
{ cat "$RS"; cat <<'TOML'

[[step]]
at = 100000
command = """setsid timeout 5 flock -x "$SIM_WORLD/run/landing.lock" sleep 5 </dev/null >/dev/null 2>&1 &"""
TOML
} > "$T/held-lock.toml"
run_one() {  # run_one <label> <scenario file> — result in $T/res.<label>.{out,rc}
    # batch-job: a whole simulated run, world up included; 280 s is a hang guard, not a budget
    (cd "$REPO" && timeout 280 env "${NOLOC[@]}" TMPDIR="$T" SPIRA_SIM_RELEASE="$SPIRA_RELEASE" SPIRA_IN_TESTENV=1 "$SIM" run "$2" --seed 3 >"$T/res.$1.out" 2>&1)
    echo "$?" >"$T/res.$1.rc"
}
run_one stale-tip "$T/stale-tip.toml" &
run_one unmet "$T/unmet.toml" &
run_one trail "$RS" &
run_one forged-land "$T/forged-land.toml" &
run_one held-lock "$T/held-lock.toml" &
wait

want "control: a branch moved after submit trips the tip invariant" "invariant inv_tip_matches_branch violated: seed 3" "$(cat "$T/res.stale-tip.out")"
want "control: an expectation nothing meets is named" "expectation nothing_can_meet_this unmet: seed 3" "$(cat "$T/res.unmet.out")"
want "control: a landing line with no certificate trips the table invariant" "invariant inv_round_land_has_certificate violated: seed 3" "$(cat "$T/res.forged-land.out")"
want "control: a lock held between events trips the table invariant" "invariant inv_no_lock_held_between_events violated: seed 3" "$(cat "$T/res.held-lock.out")"
is "control: the forged-landing run exits 1" 1 "$(cat "$T/res.forged-land.rc")"
is "control: the held-lock run exits 1" 1 "$(cat "$T/res.held-lock.rc")"
is "the run-state-trail scenario itself passes" 0 "$(cat "$T/res.trail.rc")"
is "control: the stale-tip run exits 1" 1 "$(cat "$T/res.stale-tip.rc")"
is "control: the unmet run exits 1" 1 "$(cat "$T/res.unmet.rc")"

for w in "$T"/sim-run-*; do [ -d "$w" ] && timeout 60 "$SIM" world down "$w" >/dev/null 2>&1; done # batch-job: world teardown stops a whole scratch world
sleep 5
is "no world serve outlives its run" 0 "$(pgrep -fc "$T/sim-run-" || true)"

tl_summary
