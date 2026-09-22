#!/usr/bin/env bash
#
# test-census.sh — census.sh: failure classes ranked by frequency, with open-remedy suppression.
#
#   ./test-census.sh
#
# WHAT THIS TESTS
# ---------------
# Given a fixture graph with a known label distribution, census.sh emits the expected
# ranked class list (test 1). A class with an open remedy bead carrying
# "covers:<class>" is excluded from the output (test 2). Closing the remedy bead
# makes the class reappear (test 3, the positive control for suppression).
#
# Tests group by cause, which requires sp-recur-N-<cause> labels from sp-ycvpd.
# All tests use a real bd fixture — no mock (law-prefer-the-real-dependency).
#
# POSITIVE CONTROLS (law-absence-needs-a-positive-control)
# --------------------------------------------------------
# Empty database: census runs silently → proves the query executes and finds nothing.
# Known distribution: expected classes appear, unexpected do not.
# Suppression: remedy bead causes omission; closing it restores the class.
#
# covers: spira/census.sh spira/lib.sh
# hermetic-ok: uses a fixture database; no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want() { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
lack() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1" ;; esac; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-census
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
export SPIRA_TESTDB_MODE=server
testdb_up census || {
    printf 'SKIP test-census: server testdb not available\n' >&2
    exit 77
}

# Source lib.sh for bump_recur/bump_requeue/bump_reclaim. These write 'recurred',
# 'requeued', 'reclaimed' event rows to the events table — what census.sh reads.
# Adding labels via 'bd label add sp-recur-N-cause' only creates 'label_added' events,
# which census never queries. Protect SPIRA_DB since lib.sh re-sources conf.sh.
_PRE_LIB_SPIRA_DB="$SPIRA_DB"
# shellcheck disable=SC1090
. "$HERE/lib.sh"
SPIRA_DB="$_PRE_LIB_SPIRA_DB"; unset _PRE_LIB_SPIRA_DB

CENSUS="$HERE/census.sh"
B() { bd -C "$SPIRA_DB" "$@"; }
REMEDY_LABEL=maechen-remedy

run_census() {
    env SPIRA_DB="$SPIRA_DB" \
        SPIRA_MAECHEN_REMEDY_LABEL="$REMEDY_LABEL" \
        SPIRA_CONF="$TMP/no-conf" \
        SPIRA_HOME="$HERE" \
        bash "$CENSUS" "$@" 2>/dev/null
}

run_census_repo() {  # run_census_repo <repo-path> [census-args...]
    local _rp="$1"; shift
    env SPIRA_DB="$SPIRA_DB" \
        SPIRA_MAECHEN_REMEDY_LABEL="$REMEDY_LABEL" \
        SPIRA_CONF="$TMP/no-conf" \
        SPIRA_HOME="$HERE" \
        SPIRA_REPO="$_rp" \
        bash "$CENSUS" "$@" 2>/dev/null
}

# Git fixture for closed-but-unlanded suppression (tests 6 and 7).
# Push one commit to the bare remote before cloning so origin/HEAD resolves;
# cloning an empty remote leaves origin/HEAD unset and spira_landref falls to
# rung 4 (HEAD), which is wrong in a worktree where HEAD is not on main.
REMEDY_REMOTE="$TMP/remote.git"
REMEDY_REPO="$TMP/workrepo"
REMEDY_SRC="$TMP/worksrc"
git init -q --bare "$REMEDY_REMOTE"
git -C "$REMEDY_REMOTE" symbolic-ref HEAD refs/heads/main
git init -q -b main "$REMEDY_SRC"
git -C "$REMEDY_SRC" config user.email "t@t"
git -C "$REMEDY_SRC" config user.name "t"
git -C "$REMEDY_SRC" remote add origin "$REMEDY_REMOTE"
printf 'base\n' > "$REMEDY_SRC/f"
git -C "$REMEDY_SRC" add f
git -C "$REMEDY_SRC" commit -q -m "base"
git -C "$REMEDY_SRC" push -q origin main
git clone -q "$REMEDY_REMOTE" "$REMEDY_REPO"
git -C "$REMEDY_REPO" config user.email "t@t"
git -C "$REMEDY_REPO" config user.name "t"
git -C "$REMEDY_REPO" checkout -q -b sp-fix-cls
printf 'fix\n' >> "$REMEDY_REPO/f"
git -C "$REMEDY_REPO" add f
git -C "$REMEDY_REPO" commit -q -m "fix"
git -C "$REMEDY_REPO" push -q origin sp-fix-cls
git -C "$REMEDY_REPO" checkout -q main

add_labels() {  # add_labels <bead-id> <label>...
    local id="$1"; shift
    for lbl in "$@"; do
        B label add "$id" "$lbl" >/dev/null 2>&1
    done
}

plant_bead() {  # plant_bead <title> → bead id on stdout
    B create "$1" --type bug --priority 2 --labels spira,incident --silent 2>/dev/null \
        | tr -d '[:space:]'
}

echo "test-census.sh"

# ==============================================================================
echo
echo "POSITIVE CONTROL: seeded database → census reports the seeded class"
# ==============================================================================
# Seeds one bead with one recurrence label and asserts census emits the expected class.
# An empty or absent database produces no output and fails this assertion, making the
# difference between a broken store and a correctly-seeded one visible rather than
# indistinguishable — which was the original "empty → no output" control's flaw.
testdb_reset
pc_bid="$(plant_bead "positive-control-bead")"
[ -n "$pc_bid" ] \
    || { bad "positive control bead created" "create failed"; printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"; exit 1; }
bump_recur "$pc_bid" positive-control

out_pc="$(run_census)"
want "seeded class appears in census output" "sp-recur-positive-control" "$out_pc"

# ==============================================================================
echo
echo "1. Known label distribution → correct ranked class list"
# ==============================================================================
# Fixture distribution (one bump_recur/requeue/reclaim call = one event row):
#   sp-recur-suite-red:    5  (bead-A: 3 events, bead-B: 2 events)
#   sp-requeue-prod-dirty: 3  (bead-C: 3 events — three requeueings)
#   sp-reclaim:            2  (bead-D: 2 events — reclaimed twice)
#   sp-recur-unrecorded:   1  (bead-E: 1 event)
#
# Each bump_* call inserts one row into the events table. Three bump_recur calls for
# suite-red on bead-A count as three occurrences of sp-recur-suite-red.
testdb_reset

bid_a="$(plant_bead "bead-a")"
[ -n "$bid_a" ] \
    || { bad "bead-a created" "create failed"; printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"; exit 1; }
bump_recur "$bid_a" suite-red; bump_recur "$bid_a" suite-red; bump_recur "$bid_a" suite-red

bid_b="$(plant_bead "bead-b")"
bump_recur "$bid_b" suite-red; bump_recur "$bid_b" suite-red

bid_c="$(plant_bead "bead-c")"
bump_requeue "$bid_c" prod-dirty; bump_requeue "$bid_c" prod-dirty; bump_requeue "$bid_c" prod-dirty

bid_d="$(plant_bead "bead-d")"
bump_reclaim "$bid_d"; bump_reclaim "$bid_d"

bid_e="$(plant_bead "bead-e")"
bump_recur "$bid_e" unrecorded

out1="$(run_census)"

want "sp-recur-suite-red: 2 distinct beads, 5 detections"    "2 sp-recur-suite-red (5"    "$out1"
want "sp-requeue-prod-dirty: 1 distinct bead, 3 detections"  "1 sp-requeue-prod-dirty (3" "$out1"
want "sp-reclaim: 1 distinct bead, 2 detections"             "1 sp-reclaim (2"            "$out1"
want "sp-recur-unrecorded: 1 distinct bead, 1 detection"     "1 sp-recur-unrecorded (1"   "$out1"

# Ranking: sp-recur-suite-red (2 beads) must appear before sp-requeue-prod-dirty (1 bead)
first_class="$(printf '%s\n' "$out1" | head -1 | awk '{print $2}')"
is "highest-bead-count class is first" "sp-recur-suite-red" "$first_class"

# Negative: no labels that were not planted
lack "sp-recur-merge-conflict not in output (not planted)"  "sp-recur-merge-conflict"  "$out1"

# ==============================================================================
echo
echo "2. Open remedy bead for top class → class is excluded from output"
# ==============================================================================
# Create a remedy bead covering sp-recur-suite-red. It must carry both the
# remedy label AND "covers:<class>" for census.sh to recognise the suppression.
remedy_id="$(B create "Fix sp-recur-suite-red recurring class" --type task --priority 2 \
    --labels "spira,plan,${REMEDY_LABEL},covers:sp-recur-suite-red" \
    --silent 2>/dev/null | tr -d '[:space:]')"
[ -n "$remedy_id" ] \
    || { bad "remedy bead created" "create failed"; printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"; exit 1; }

out2="$(run_census)"
lack "sp-recur-suite-red excluded when remedy is open" "sp-recur-suite-red" "$out2"
want "sp-requeue-prod-dirty still present after suppression" "sp-requeue-prod-dirty" "$out2"
want "sp-reclaim still present after suppression"            "sp-reclaim"            "$out2"

# With --with-suppressed the class appears annotated
out2s="$(run_census --with-suppressed)"
want "with --with-suppressed, suppressed class appears"  "sp-recur-suite-red" "$out2s"
want "with --with-suppressed, marked [suppressed]"       "[suppressed]"       "$out2s"

# ==============================================================================
echo
echo "2b. Remedy bead in_progress → class still suppressed"
# ==============================================================================
# The fix for sp-1cmo: --status open in _suppressed_classes misses remedy beads
# that are being actively worked. Verify suppression holds for in_progress.
B update "$remedy_id" --status in_progress >/dev/null 2>&1

out2b="$(run_census)"
lack "sp-recur-suite-red excluded when remedy is in_progress" "sp-recur-suite-red" "$out2b"

out2bs="$(run_census --with-suppressed)"
want "in_progress remedy still annotates [suppressed]" "[suppressed]" "$out2bs"

# ==============================================================================
echo
echo "2c. Remedy bead blocked → class still suppressed"
# ==============================================================================
B update "$remedy_id" --status blocked >/dev/null 2>&1

out2c="$(run_census)"
lack "sp-recur-suite-red excluded when remedy is blocked"  "sp-recur-suite-red" "$out2c"

out2cs="$(run_census --with-suppressed)"
want "blocked remedy still annotates [suppressed]" "[suppressed]" "$out2cs"

# ==============================================================================
echo
echo "3. POSITIVE CONTROL: closed remedy keeps suppression until commit is on base"
# ==============================================================================
# On the unfixed tree, closing a remedy bead lifts suppression immediately.
# These assertions are red there (law-a-regression-test-must-be-seen-to-fail).
FIXTURE_REPO="$TMP/fixture-repo"
git init -q "$FIXTURE_REPO" \
    && GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=test \
       GIT_COMMITTER_EMAIL=t@t \
       git -C "$FIXTURE_REPO" commit --allow-empty -q -m "initial" 2>/dev/null

run_census_fixture() {
    env SPIRA_DB="$SPIRA_DB" \
        SPIRA_MAECHEN_REMEDY_LABEL="$REMEDY_LABEL" \
        SPIRA_CONF="$TMP/no-conf" \
        SPIRA_HOME="$HERE" \
        SPIRA_REPO="$FIXTURE_REPO" \
        bash "$CENSUS" "$@" 2>/dev/null
}

B close "$remedy_id" --reason "test: verify closed remedy still suppresses" --force >/dev/null 2>&1

out3_pre="$(run_census_fixture)"
lack "sp-recur-suite-red still suppressed after close, commit not on base" \
    "sp-recur-suite-red" "$out3_pre"

out3_pre_s="$(run_census_fixture --with-suppressed)"
want "closed-unlanded remedy annotated [suppressed: remedy closed, not landed]" \
    "[suppressed: remedy closed, not landed]" "$out3_pre_s"
want "suppressed class appears in annotated output" "sp-recur-suite-red" "$out3_pre_s"

# Land the remedy: add a commit naming the bead to the base.
GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=t@t \
    git -C "$FIXTURE_REPO" commit --allow-empty -q \
    -m "fix: $remedy_id closes sp-recur-suite-red" 2>/dev/null

out3="$(run_census_fixture)"
want "sp-recur-suite-red reappears once remedy commit is on base" \
    "sp-recur-suite-red" "$out3"
lack "no [suppressed] after remedy landed" "[suppressed]" "$out3"

# ==============================================================================
echo
echo "4. Two distinct classes, both below threshold — both counted correctly"
# ==============================================================================
# The Maechen SELECT threshold (three or more) lives in the brief, not in census.sh.
# census.sh reports all classes including those below threshold; Maechen decides.
want "sp-reclaim: 1 distinct bead, 2 detections present"         "1 sp-reclaim (2"        "$out3"
want "sp-recur-unrecorded: 1 distinct bead, 1 detection present" "1 sp-recur-unrecorded (1" "$out3"

# ==============================================================================
echo
echo "5. RANKING: three-bead class outranks single-bead class with more events"
# ==============================================================================
# Positive control (law-a-regression-test-must-be-seen-to-fail):
# Before the fix, census ranks by event count: sp-recur-alpha (5 events) > sp-recur-beta (3).
# After the fix, census ranks by distinct beads: sp-recur-beta (3 beads) > sp-recur-alpha (1).
testdb_reset
bid_one="$(plant_bead "single-bead-many-events")"
bump_recur "$bid_one" alpha
bump_recur "$bid_one" alpha
bump_recur "$bid_one" alpha
bump_recur "$bid_one" alpha
bump_recur "$bid_one" alpha

for ri in 1 2 3; do
    rb="$(plant_bead "multi-bead-$ri")"
    bump_recur "$rb" beta
done

out5="$(run_census)"
first5="$(printf '%s\n' "$out5" | head -1 | awk '{print $2}')"
is "three-bead class (sp-recur-beta) ranks above single-bead class with more events" \
    "sp-recur-beta" "$first5"

# ==============================================================================
echo
echo "6. Empty cause (reopened): distinct bead count is not event count (db-i0jd)"
# ==============================================================================
# Regression for db-i0jd: count.py dropped empty fields by value, collapsing the
# 4-column row to 3 when new_value is empty; the len==3 branch then read the event
# count as the bead count. Discriminating fixture: 2 beads, 3 events — broken code
# would report "3 sp-reopen" (event count), fixed code reports "2 sp-reopen".
testdb_reset
reopen_a="$(plant_bead "reopen-bead-a")"
reopen_b="$(plant_bead "reopen-bead-b")"
bump_reopen "$reopen_a"
bump_reopen "$reopen_a"
bump_reopen "$reopen_b"

out6="$(run_census)"
want "sp-reopen: 2 distinct beads"               "2 sp-reopen"    "$out6"
want "sp-reopen: 3 detections"                   "(3 detections"  "$out6"
lack "sp-reopen not reported as 3 beads (broken would read event count)" "3 sp-reopen" "$out6"

# ==============================================================================
echo
echo "7. Closed remedy with unlanded branch → class still suppressed (db-ista)"
# ==============================================================================
# Positive control: a closed remedy whose branch tip is NOT yet on origin/main
# must continue suppressing the class. Suppression lifts only when the fix lands.
testdb_reset
bid_u="$(plant_bead "unlanded-fix-bead")"
bump_recur "$bid_u" unlanded-cls

closed_remedy_id="$(B create "Fix sp-recur-unlanded-cls" --type task --priority 2 \
    --labels "spira,plan,${REMEDY_LABEL},covers:sp-recur-unlanded-cls,branch:sp-fix-cls" \
    --silent 2>/dev/null | tr -d '[:space:]')"
[ -n "$closed_remedy_id" ] \
    || { bad "closed remedy bead created" "create failed"; printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"; exit 1; }
B close "$closed_remedy_id" --reason "test: closed but not landed" --force >/dev/null 2>&1

out7="$(run_census_repo "$REMEDY_REPO")"
lack "sp-recur-unlanded-cls excluded when remedy is closed-but-unlanded" \
    "sp-recur-unlanded-cls" "$out7"

out7s="$(run_census_repo "$REMEDY_REPO" --with-suppressed)"
want "closed-unlanded remedy: class appears with --with-suppressed" \
    "sp-recur-unlanded-cls" "$out7s"
want "closed-unlanded remedy: annotated [suppressed: remedy closed, not landed]" \
    "[suppressed: remedy closed, not landed]" "$out7s"

# ==============================================================================
echo
echo "8. CONTROL: closed remedy with landed branch → class unsuppressed (db-ista)"
# ==============================================================================
# Add a landing commit naming the bead id and push to origin/main.
# landed() searches commit subjects for the bead id — the same criterion the
# harness uses when a branch is squash-merged with "spira: land <id>".
GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=t@t \
    git -C "$REMEDY_REPO" commit --allow-empty -q -m "spira: land $closed_remedy_id" 2>/dev/null
git -C "$REMEDY_REPO" push -q origin main >/dev/null 2>&1

out8="$(run_census_repo "$REMEDY_REPO")"
want "sp-recur-unlanded-cls reappears after fix branch lands on base" \
    "sp-recur-unlanded-cls" "$out8"
lack "no [suppressed] annotation after branch lands" "[suppressed]" "$out8"

# ==============================================================================
echo
echo "9. Landed remedy beyond commit window → class NOT suppressed (sp-census-suppression-starvation)"
# ==============================================================================
# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail):
# Before this fix, landed() searched only the most recent SPIRA_VERDICT_WINDOW commits.
# With window=3 and the landing commit at depth 4, landed() returned 1 (not found) and
# census suppressed the class. On the unfixed tree this test fails for that reason.
testdb_reset
bid_w="$(plant_bead "window-depth-bead")"
bump_recur "$bid_w" window-depth-cls

window_remedy_id="$(B create "Fix sp-recur-window-depth-cls" --type task --priority 2 \
    --labels "spira,plan,${REMEDY_LABEL},covers:sp-recur-window-depth-cls" \
    --silent 2>/dev/null | tr -d '[:space:]')"
[ -n "$window_remedy_id" ] \
    || { bad "window remedy bead created" "create failed"; printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"; exit 1; }
B close "$window_remedy_id" --reason "test: window depth" --force >/dev/null 2>&1

# Push the landing commit, then 3 more commits — puts the landing commit at depth 4.
GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=t@t \
    git -C "$REMEDY_REPO" commit --allow-empty -q \
    -m "spira: land $window_remedy_id" 2>/dev/null
git -C "$REMEDY_REPO" push -q origin main >/dev/null 2>&1
for _wi in 1 2 3; do
    GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=t@t \
        git -C "$REMEDY_REPO" commit --allow-empty -q \
        -m "post-land noise $window_remedy_id" 2>/dev/null
done
git -C "$REMEDY_REPO" push -q origin main >/dev/null 2>&1

# SPIRA_VERDICT_WINDOW=3: old code would miss the landing commit (depth 4); new code ignores it.
out9="$(SPIRA_VERDICT_WINDOW=3 run_census_repo "$REMEDY_REPO")"
want "sp-recur-window-depth-cls unsuppressed when landing commit is beyond SPIRA_VERDICT_WINDOW" \
    "sp-recur-window-depth-cls" "$out9"
lack "no [suppressed] when landing commit is beyond window" \
    "[suppressed]" "$out9"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
