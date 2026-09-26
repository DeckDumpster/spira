#!/usr/bin/env bash
#
# test-ci-park-state.sh — spira_ci_park_state, the pure decision table behind the CI park.
#
#   ./test-ci-park-state.sh
#
# Replaces test-ci-park.sh (deleted, sp-0r1lv: flipped in Concierge full-corpus round 24 —
# a container-runtime timeout in the batch harness, not a real assertion failure; all 35
# cases passed). This suite keeps only the pure core: no testdb, no git, no aeon.sh, no
# cockpit.sh, so it cannot be caught by the class of infrastructure flake that took the
# original suite down. The aeon-brief and ops-pane halves of the old suite are not
# reproduced here; see docs/test-plan/cockpit-observability.md for that lost coverage.
#
# WHY EVERY CASE HERE IS A PAIR. Each of these mechanisms fails by doing nothing, and doing
# nothing is what a healthy pipeline also looks like: a classifier that answered `no-ci` to
# everything would block any gate from being created where no run can resolve it, and one
# that answered `watch` to everything would gate every bead including those with no run.
# So the same input is driven both ways round wherever a verdict is asserted
# (law-absence-needs-a-positive-control).
#
# THE DEADLINE IS PINNED TO A NON-DEFAULT, 600 rather than the shipped 5400. Asserting
# against the shipped value passes just as well if the number is written into the code,
# which is the thing the configuration key exists to stop.
#
# tier: T1
# covers: spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

BASE_PATH="$PATH"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/home"

# ======================================================================================
# spira_ci_park_state — the whole rule, as a decision table.
#
# Pure: no database, no network, no writes. That is what lets the sweep and the ops pane
# share ONE answer rather than two that can disagree, and it is what lets every branch of
# it be driven here directly instead of inferred from a pass's output.
#
# AN EXPLICIT, MINIMAL ENVIRONMENT. `env -i` with SPIRA_CONF aimed at a file that is not
# there, because a suite that inherits the operator's real spira.conf is asserting about one
# box: this deadline, this repo-map, this set of repositories. Ambient configuration decides
# verdicts silently (law-gates-run-in-a-clean-environment).
# ======================================================================================
MAP="$TMP/repo-map"
cat > "$MAP" <<MAP
# name | path | land | base | format | gate
alpha | $TMP/alpha | pr   | origin/main | |
beta  | $TMP/beta  | push | origin/main | |
gamma | $TMP/gamma | hold | origin/main | |
MAP

# park <repo> <updated-at> [max] -> "<state> <rc>", both halves, because the rc is half the
# contract: `watch` returned with rc 2 means "could not age this", and a caller that reads
# only the word treats an unreadable clock as a healthy park.
park() {
    env -i PATH="$BASE_PATH" HOME="$TMP/home" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$TMP/home" SPIRA_REPO="$TMP/home" \
        SPIRA_RUN="$TMP/home/run" SPIRA_DB="$TMP/home/nodb" \
        SPIRA_REPO_MAP="$MAP" SPIRA_CI_PARK_MAX="${3-600}" \
        bash -c '. "$0"; spira_ci_park_state "$1" "$2"; printf " %s" "$?"' \
        "$HERE/lib.sh" "$1" "$2"
}
ago() { date -u -d "@$(( $(date -u +%s) - $1 ))" +%Y-%m-%dT%H:%M:%SZ; }

echo "spira_ci_park_state:"

# THE PAIR THAT MAKES EVERY OTHER ASSERTION MEAN SOMETHING: one repository, one deadline,
# two ages, two different answers. A classifier stuck on either verdict fails here.
is "a fresh park in a pr repo is watched"       "watch 0"   "$(park alpha "$(ago 60)")"
is "the same park past the deadline is expired" "expired 0" "$(park alpha "$(ago 900)")"

# NO RUN EXISTS AND NONE WILL. The age is identical to the expired case above, so what is
# being read here is the land mode and nothing else.
is "a push repo has no run to wait for"    "no-ci 0" "$(park beta  "$(ago 900)")"
is "nor does a hold repo"                  "no-ci 0" "$(park gamma "$(ago 900)")"
# And a fresh one, so `no-ci` is not the deadline arriving by another name.
is "a push repo has none when fresh either" "no-ci 0" "$(park beta "$(ago 60)")"

# AN UNMAPPED REPOSITORY ANSWERS HERE TOO, AND SHOULD. A repository the map cannot resolve
# cannot land through a pull request this harness knows how to watch, so a park on it is
# waiting for something nothing will ever report. This is also the bead that MOVED
# repository while parked — the case no check made at the moment of parking could see.
is "an unmapped repo has no run to wait for" "no-ci 0" "$(park nowhere "$(ago 60)")"
is "and neither does a nameless one"         "no-ci 0" "$(park "" "$(ago 60)")"

# THE EMPTY TIMESTAMP. `date -d ""` does not fail — it answers midnight today — so a park
# with no updated_at at all would age itself against a clock the caller never supplied and
# expire silently on a field that was never there. The hazard is asserted directly, so the
# guard below is a rule somebody can see fire rather than a line nobody can account for.
hz="$(date -u -d "" +%s 2>/dev/null)"; hz_rc=$?
is "the empty date is accepted by date(1)"  0 "$hz_rc"
is "and answers midnight today"             "$(date -u -d "$(date -u +%Y-%m-%d)" +%s)" "$hz"
# So the guard must refuse it BEFORE date sees it, and reach the caller as "could not age
# this" — rc 2 — never as a verdict.
is "an absent timestamp is refused, not aged" "watch 2" "$(park alpha "")"
is "and so is one that cannot be parsed"      "watch 2" "$(park alpha "the day before")"
# A repository with no run to wait for is decided before the clock is consulted at all, so
# an unreadable timestamp cannot turn a no-ci verdict into a park.
is "an unreadable clock does not save a push park" "no-ci 0" "$(park beta "")"

# THE DEADLINE IS A KEY, AND ZERO DISABLES IT DELIBERATELY. The pair is the point: the same
# ancient park reads expired at 600 and watch at 0, so this cannot pass against a deadline
# that was never applied.
is "zero disables the deadline"       "watch 0"   "$(park alpha "$(ago 900)" 0)"
is "and 600 is what expired it"       "expired 0" "$(park alpha "$(ago 900)" 600)"

# A DEADLINE THAT IS NOT A NUMBER FALLS BACK TO THE SHIPPED ONE, and must not reach `[ -gt ]`
# as a word — that is a shell error, and an errored classifier is a park left standing with
# no reason recorded anywhere. Both sides of the fallback are asserted, so "fell back" cannot
# be satisfied by disabling the deadline.
is "a non-numeric deadline still watches inside 5400" "watch 0"   "$(park alpha "$(ago 900)"  soon)"
is "and still expires outside it"                     "expired 0" "$(park alpha "$(ago 10800)" soon)"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
