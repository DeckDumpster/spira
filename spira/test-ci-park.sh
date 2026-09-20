#!/usr/bin/env bash
#
# test-ci-park.sh — a park is only a park where a run can end it.
#
#   ./test-ci-park.sh
#
# THE FAILURE THIS SUITE EXISTS FOR. `$SPIRA_CI_LABEL` means "parked on a run somebody else
# is watching". It is excluded from every persona's predicate AND from the stalled-work
# report, which is the point of it — parked work must not look abandoned — and is exactly
# what makes a park nothing can end strictly worse than a stall: not claimable, not
# reported, and rendered to the operator as "in CI", the one description that stops anybody
# looking for the real cause. One bead reached 22 reclaims that way, not one of them a work
# failure, while the board said it was in CI and the repository it named opens no pull
# requests at all.
#
# Only `pr` mode opens one. `push` merges the branch itself and `hold` leaves it for a
# human, so for both of those the landing gate IS the gate and there is nothing further to
# wait for. Two ways to get it wrong, and the harness closes both: parking such a bead at
# all, and leaving a park standing when the bead MOVED repository afterwards — which no
# check made at the moment of parking could have seen.
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
# defect: sp-jll sp-0092
# covers: spira/lib.sh spira/cockpit.sh spira/aeon.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

BASE_PATH="$PATH"
TMP="$(mktemp -d)"
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


# ======================================================================================
# Shared test database and fixtures used by the brief and ops-pane sections below.
# ======================================================================================
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-ci-park
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up cipark || { echo "test-ci-park: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

SH="$TMP/spira"; RUN="$TMP/run"; ALPHA="$TMP/alpha"
mkdir -p "$SH/chamber" "$RUN/worktree"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$SH/"
# `alpha` is a real checkout so the pr path can be reached at all; `beta` and `gamma` are
# the paths the map already names and nothing created them. That absence is the assertion.
git init -q -b main "$ALPHA"
git -C "$ALPHA" commit -q --allow-empty -m base

seed_parks() {
    testdb_reset
    testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira","plan"]}
{"id":"sp-pk-push","title":"parked in a push repo","status":"open","issue_type":"task","labels":["spira","plan","repo:beta","awaiting-ci"]}
{"id":"sp-pk-hold","title":"parked in a hold repo","status":"open","issue_type":"task","labels":["spira","plan","repo:gamma","awaiting-ci"]}
{"id":"sp-pk-gone","title":"parked in an unmapped repo","status":"open","issue_type":"task","labels":["spira","plan","repo:nowhere","awaiting-ci"]}
{"id":"sp-pk-live","title":"parked on a real run","status":"open","issue_type":"task","labels":["spira","plan","repo:alpha","awaiting-ci"],"updated_at":"$(ago 60)"}
JSONL
}


# ======================================================================================
# THE BRIEF, generated per land mode.
#
# The gate-check sweep resolves gates for pr-mode repos; the brief is what stops a gate
# being created where nothing can resolve it — a push or hold bead told to create a gh:run
# gate would sit gated forever, invisible, while the pane said "in CI".
#
# Driven through the REAL aeon.sh with a shim standing where the model would be, capturing
# the prompt it was actually handed. A structural grep over aeon.sh would assert that both
# branches are written; only running it asserts that the right one is reached, which is the
# half that can break — REPO_LAND comes from the bead's `repo:` label through the map, and a
# lookup that answered nothing would silently give every aeon the `push` brief.
# ======================================================================================
echo
echo "the brief an aeon is handed:"

AH="$TMP/aeonhome"; mkdir -p "$AH/chamber"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/aeon.sh" "$AH/"
cp -r "$HERE/actors" "$AH/" 2>/dev/null || true
AORIGIN="$TMP/aorigin.git"; git init -q --bare -b main "$AORIGIN"
AREPO="$TMP/arepo"; git clone -q "$AORIGIN" "$AREPO" 2>/dev/null
git -C "$AREPO" config user.email t@t; git -C "$AREPO" config user.name t
printf 'seed\n' > "$AREPO/f"
git -C "$AREPO" add f; git -C "$AREPO" commit -qm seed; git -C "$AREPO" push -q origin main 2>/dev/null
cat > "$AH/chamber/builder.fayth" <<'FAYTH'
FAYTH_NAME=builder
FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$AH/chamber/builder.md"

ABIN="$TMP/abin"; mkdir -p "$ABIN"
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-ci-park: aeon.sh has no SPIRA_AGENT injection point — refusing to run the real model" >&2; exit 1; }
cat > "$ABIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$PROMPT_OUT"
printf '{"type":"result","subtype":"success","is_error":false,"result":"read","num_turns":1}\n'
SHIM
chmod +x "$ABIN/claude"

brief_for() {   # brief_for <land> -> the PARK section of the brief an aeon was handed
    local land="$1" map="$TMP/aeon-map-$1"
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:arepo\""
    printf 'arepo | %s | %s | origin/main | |\n' "$AREPO" "$land" > "$map"
    testdb_reset
    printf '{"id":"sp-br-1","title":"t","status":"open","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$_lbl" | testdb_seed
    rm -rf "$TMP/arun"; mkdir -p "$TMP/arun"
    : > "$TMP/prompt"
    SPIRA_HOME="$AH" SPIRA_RUN="$TMP/arun" SPIRA_DB="$SPIRA_DB" SPIRA_REPO="$AREPO" \
    SPIRA_REPO_MAP="$map" SPIRA_CI_PARK_MAX=600 \
    SPIRA_AGENT="$ABIN/claude" PROMPT_OUT="$TMP/prompt" \
        "$AH/aeon.sh" builder >/dev/null 2>&1
    cat "$TMP/prompt"
}

# A repository that DOES open pull requests: the aeon is told to park, and told what ends the
# park. Without this pair, every assertion below would pass against a harness that had simply
# stopped mentioning CI to anyone.
pr_brief="$(brief_for pr)"
want "the prompt was rendered at all"        "work sp-br-1"          "$pr_brief"
nowant "with no placeholder left standing"   "{{PARK}}"              "$pr_brief"
# THE COMMAND, not the bare phrase. The push brief says "do not create a gh:run gate",
# which contains "gate" — a looser match would be satisfied by the negation and the pair
# below would assert nothing.
want "a pr repo's aeon is told to create a gate" "bd gate create --type=gh:run" "$pr_brief"
want "with the repo set on the gate"             "set-metadata"                 "$pr_brief"

# And the other side: the same aeon, the same bead, one column of the map different.
push_brief="$(brief_for push)"
want "a push repo's aeon is told there is no run"  "no CI run to wait for"       "$push_brief"
want "and told not to create a gate"               "do not create a gh:run gate" "$push_brief"
want "naming the land mode it read"                "lands by \`push\`"           "$push_brief"
nowant "and is never told to create a gate"        "bd gate create --type=gh:run" "$push_brief"

# `hold` is the third mode and it takes the same side of the branch. It is asserted because
# the condition is `= pr`, not `!= push`, and those differ on exactly this value.
hold_brief="$(brief_for hold)"
want "a hold repo's aeon is told the same"  "do not create a gh:run gate" "$hold_brief"
want "naming its own land mode"             "lands by \`hold\`"           "$hold_brief"

# ======================================================================================
# THE OPS PANE. Gate state replaces the awaiting-ci label: the pane reads open gh:run
# gates via bd gate list rather than querying a label bag that bd cannot validate.
#
# GATES ARE CLASSIFIED BY AGE ALONE. A gh:run gate only exists for pr-mode repos (the
# aeon brief refuses to create one for push or hold), so the no-ci verdict of the former
# label reader is not needed; age against SPIRA_CI_PARK_MAX decides watch vs stuck.
#
# CALLED VIA core_detail, NOT once. core_detail_keys() emits the SP_AWAITING_* keys;
# probe() (the once-mode backward-compat wrapper) does not call it. The tiered collector
# calls cockpit.sh core_detail — that is the seam driven here.
#
# NON-ZERO EXPECTED VALUE. Every assertion that a count is N>0 uses a fixture with
# exactly N gates, so a collector reading the wrong source (labels → 0) is
# distinguishable from one reading the right source (gates → N).
# ======================================================================================
echo
echo "the ops pane:"

CRUN="$TMP/crun"; mkdir -p "$CRUN"
cp "$HERE/cockpit.sh" "$HERE/cockpit-metrics.py" "$SH/" 2>/dev/null || true

# snapshot [max] -> quoted SP_AWAITING_* lines from one core_detail pass.
# core_detail outputs raw KEY=VALUE; awk wraps to KEY='VALUE' so key() works unchanged.
snapshot() {
    SPIRA_HOME="$SH" SPIRA_RUN="$CRUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO="$ALPHA" \
    SPIRA_REPO_MAP="$MAP" SPIRA_CI_PARK_MAX="${1-600}" SPIRA_GOAL=sp-goal SPIRA_FAYTHS=t \
        bash "$SH/cockpit.sh" core_detail 2>/dev/null | \
    awk -F= '/^SP_AWAITING/{printf "%s='"'"'%s'"'"'\n", $1, substr($0, index($0,"=")+1)}'
}
key() { sed -n "s/^$1='\(.*\)'$/\1/p" <<< "$2"; }

# seed_gates: one gate inside the 600s deadline, one past it. Both seeded via bd import
# with explicit created_at timestamps so the test does not depend on wall-clock timing.
# SEED VIA JSONL NOT bd gate create, because gate create sets created_at=now: an old gate
# requires an explicit past timestamp.
seed_gates() {
    testdb_reset
    printf '{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira","plan"]}\n' | testdb_seed
    # FRESH GATE: 60s old, inside the 600s deadline.
    printf '{"id":"sp-new-gate","title":"Gate: gh:run","description":"Ad-hoc gate blocking sp-wk1","status":"open","issue_type":"gate","await_type":"gh:run","created_at":"%s","updated_at":"%s"}\n' \
        "$(ago 60)" "$(ago 60)" | testdb_seed
    # OLD GATE: 900s old, past any reasonable deadline. Seeded oldest-first so the
    # collector's oldest-first sort emits sp-old-gate as SP_AWAITING0.
    printf '{"id":"sp-old-gate","title":"Gate: gh:run","description":"Ad-hoc gate blocking sp-wk2","status":"open","issue_type":"gate","await_type":"gh:run","created_at":"%s","updated_at":"%s"}\n' \
        "$(ago 900)" "$(ago 900)" | testdb_seed
}

# THE ZERO CASE TESTS BOTH COUNTS. A gate reader aimed at the wrong source (labels)
# answers 0 here AND for the non-zero cases below — both pass, neither signals anything.
testdb_reset
printf '{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira","plan"]}\n' | testdb_seed
snap="$(snapshot 600)"
is "with no gates, nothing is watched" 0 "$(key SP_AWAITING_N "$snap")"
is "and nothing is stuck"              0 "$(key SP_AWAITING_STUCK "$snap")"

# THE NON-ZERO CASE: fresh gate (60s) is watched, old gate (900s) is stuck.
# NON-ZERO EXPECTED VALUES — the critical assertions that distinguish gate-reading
# from label-reading (law-a-regression-test-must-be-seen-to-fail).
seed_gates
snap="$(snapshot 600)"
is "a fresh gate is watched (non-zero expected value)"  1 "$(key SP_AWAITING_N "$snap")"
is "an old gate is stuck (non-zero expected value)"     1 "$(key SP_AWAITING_STUCK "$snap")"
# THE ROW CARRIES ITS OWN REASON. Oldest gate is SP_AWAITING0 (stuck); fresh is SP_AWAITING1.
want  "a stuck gate's row names it"               "gate overdue"  "$snap"
want  "the stuck gate is named for the summary"   "SP_AWAITING_STUCK_ID='sp-" "$snap"
nowant "the fresh gate's row does not say overdue" "gate overdue" \
    "$(grep 'SP_AWAITING1=' <<< "$snap")"

# The deadline moves gates between populations: same database, same instant, deadline=1s.
# Both gates (60s and 900s old) exceed 1s, so both must appear as stuck.
snap="$(snapshot 1)"
is "past the deadline the fresh gate also becomes stuck" 0 "$(key SP_AWAITING_N "$snap")"
is "and both gates are stuck"                           2 "$(key SP_AWAITING_STUCK "$snap")"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
