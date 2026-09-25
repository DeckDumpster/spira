#!/usr/bin/env bash
#
# test-yield.sh — a gate red lands in the right column, and an unreadable record says `?`.
#
#   ./test-yield.sh
#
# WHAT THIS IS FOR. A gate may sit between work and its landings only while it is catching
# real defects (law-gate-earns-its-place). The gate that ran before this one was seventeen
# minutes a branch, found nothing on the morning it mattered, and produced two failures that
# were its own suites reading the state of the box — and it was deleted after twelve hours of
# fallout rather than on that evidence, because nobody was counting. yield.sh is the count.
#
# A COUNT NOBODY CHECKS IS WORSE THAN NO COUNT, because it is believed. The specific way this
# measurement fails is that it silently stops recording: the gate stops calling it, a path
# moves, a verdict field is renamed — and every one of those renders as a tidy, reassuring
# "no gate faults". So every assertion below about a column being empty is preceded by proof
# that the same read finds something when something is there (law-absence-needs-a-positive-
# control), and the `?` cases are asserted as `?` rather than as 0.
#
# DRIVEN THROUGH THE REAL gate.sh AGAINST A REAL GIT REPOSITORY. What is under test is a
# classification made from a tree id and an exit status, and a model of either would be a
# second implementation of the thing in question (law-prefer-the-real-dependency). No
# database is needed and none is touched: the gate reaches no beads, so this whole suite is a
# couple of seconds.
#
# THE TWO PLANTED REDS ARE THE POINT, and they differ in exactly one property — whether the
# repository's own gate command also fails against the base. That is the distinction the
# whole metric rests on, and if it ever stops being visible to the gate, both reds land in
# one column and the number stops meaning anything.
#
# defect: sp-1xb0
# tier: T1
# covers: spira/yield.sh spira/gate.sh spira/watchtower.sh cockpit/health.sh
# scar: yield.sh was absent; gate faults landed in the wrong column and no count existed, so the gate's cost-vs-catch ratio was unmeasured.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-yield.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
REPO="$TMP/repo"; RUN="$TMP/run"; MAP="$TMP/map"
YDIR="$TMP/yield"; GATELOG="$TMP/gate.log"; VDIR="$TMP/verdicts"; HOMEDIR="$TMP/home"
mkdir -p "$RUN/worktree" "$HOMEDIR"

# PINNED TO A NON-DEFAULT. The shipped window is a day; asserting against it would pass just
# as well if the code had 86400 written in, which is the thing the key exists to stop.
WINDOW=7200

git init -q --bare -b main "$TMP/remote.git"
git init -q -b main "$REPO"
printf 'base\n' > "$REPO/marker"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m base
git -C "$REPO" remote add origin "$TMP/remote.git"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin

branch() {               # branch <name> <file> — a branch off the base carrying one file
    local br="$1" f="$2" w="$TMP/w.$1"
    w="$TMP/w.$(printf '%s' "$br" | tr / _)"
    git -C "$REPO" worktree add -q -b "$br" "$w" origin/main
    printf 'x\n' > "$w/$f"
    git -C "$w" add -A; git -C "$w" commit -q -m "$br work"
    git -C "$REPO" worktree remove --force "$w"
}
amend() {                # amend <branch> <file-to-remove> — the fix an aeon would make
    local br="$1" f="$2" w
    w="$TMP/w.fix.$(printf '%s' "$br" | tr / _)"
    git -C "$REPO" worktree add -q "$w" "$br"
    git -C "$w" rm -q "$f"
    git -C "$w" commit -q -m "$br fix"
    git -C "$REPO" worktree remove --force "$w"
}

# THE GATE COMMAND IS SET PER CASE, and it is the whole fixture. `guilty.txt` exists only on
# the branch, so the command fails on the branch and passes on the base — a genuine defect.
# `always-red` fails on both, which is what a suite reading the state of the box looks like
# from the gate: the branch did not cause it.
map_gate() { printf 'repo | %s | push | origin/main |  | %s\n' "$REPO" "$1" > "$MAP"; }
GUILTY='test ! -e guilty.txt || { echo "gate: test-guilty.sh FAILED (rc=1)" >&2; false; }'
ALWAYS='echo "gate: test-boxreader.sh FAILED (rc=1)" >&2; false'

# THE ENVIRONMENT IS EXPLICIT AND MINIMAL. A suite that inherits a real spira.conf asserts
# against one box, and one that inherits SPIRA_YIELD writes its planted reds into the real
# record (law-gates-run-in-a-clean-environment).
rungate() {              # rungate <branch> [VAR=VAL ...]
    local br="$1"; shift
    env -i HOME="$HOMEDIR" PATH="$PATH" \
        GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
        SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_REPO="$REPO" SPIRA_RUN="$RUN" \
        SPIRA_DB="$TMP/nonexistent-db" SPIRA_REPO_MAP="$MAP" SPIRA_GATE_LOG="$GATELOG" \
        SPIRA_VERDICTS="$VDIR" SPIRA_YIELD="$YDIR" \
        "$@" bash "$HERE/gate.sh" "$br" repo
}
yield() {                # yield <args...> -> yield.sh, reading the same record the gate wrote
    env -i HOME="$HOMEDIR" PATH="$PATH" \
        SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_REPO="$REPO" SPIRA_RUN="$RUN" \
        SPIRA_DB="$TMP/nonexistent-db" SPIRA_YIELD="$YDIR" SPIRA_GATE_LOG="$GATELOG" \
        SPIRA_YIELD_WINDOW="$WINDOW" \
        bash "$HERE/yield.sh" "$@"
}
f() { printf '%s\n' "$1" | sed -n "s/^$2=//p" | head -1; }

# ======================================================================================
echo
echo "the positive control — nothing is recorded before a gate refuses anything:"
# ======================================================================================
# THE DIRECTORY'S ABSENCE MUST READ AS UNREADABLE, NOT AS EMPTY. This is the state the whole
# measurement collapses to when it silently stops being called, and it is the one state that
# must never render as a clean bill of health.
R="$(yield report)"
is "an unwritten record reports reds as ?"        "?" "$(f "$R" YIELD_REDS)"
# WITH NO METER LOG EITHER, there is no positive control to be had and it says so rather than
# guessing. The three other states — absent, silent, ok — are proved further down, each
# against a meter log of the shape that produces it.
is "and admits it has no positive control yet" "?" "$(f "$R" YIELD_RECORDER)"
is "and gate faults as ?, never 0"               "?" "$(f "$R" YIELD_FAULT)"
is "and unknowns as ?, never 0"                  "?" "$(f "$R" YIELD_UNKNOWN)"
is "the window it reports is the configured one" "$WINDOW" "$(f "$R" YIELD_WINDOW)"

# ======================================================================================
echo
echo "a branch that is genuinely wrong — the gate was right to refuse it:"
# ======================================================================================
map_gate "$GUILTY"
branch spira/sp-guilty guilty.txt
rungate spira/sp-guilty > "$TMP/guilty.out" 2>&1; rc=$?
is "the gate blames the branch" 1 "$rc"
want "the verdict line says FAIL" "VERDICT=FAIL" "$(cat "$TMP/guilty.out")"

R="$(yield report)"
is "the red was recorded"                   1 "$(f "$R" YIELD_REDS)"
is "and it is UNKNOWN until somebody says"  1 "$(f "$R" YIELD_UNKNOWN)"
is "not counted as a defect"                0 "$(f "$R" YIELD_DEFECT)"
is "and not counted as a gate fault"        0 "$(f "$R" YIELD_FAULT)"

REC="$(cat "$YDIR"/* 2>/dev/null)"
want "the record names the branch" "branch=spira/sp-guilty" "$REC"
want "the record names the bead"   "bead=sp-guilty"         "$REC"
want "the record names the suite that refused it" "suite=test-guilty.sh" "$REC"

# GATED TWICE, AS EVERY BEAD IS — once by its aeon and once by the landing pass. Two arrivals
# of one fact are one row: counting arrivals would double every red and turn this ratio into
# a measure of how many times each branch was gated.
rungate spira/sp-guilty >/dev/null 2>&1
R="$(yield report)"
is "gating the same tree again does not double the red" 1 "$(f "$R" YIELD_REDS)"

# ======================================================================================
echo
echo "the byproduct classification — the branch changed, and then it passed:"
# ======================================================================================
amend spira/sp-guilty guilty.txt
rungate spira/sp-guilty >/dev/null 2>&1; rc=$?
is "the fixed branch passes" 0 "$rc"
R="$(yield report)"
is "the red is now a DEFECT"                1 "$(f "$R" YIELD_DEFECT)"
is "and no longer UNKNOWN"                  0 "$(f "$R" YIELD_UNKNOWN)"
# THE INFERRED HALF IS MARKED AS INFERRED. "The branch changed and then passed" is strong
# evidence and not proof — a rebase changes the tree too — and a reader who cannot tell a
# stated verdict from a deduced one will eventually believe a deduction that was wrong.
is "and it is reported as inferred, not stated" 1 "$(f "$R" YIELD_DEFECT_INFERRED)"

# ======================================================================================
echo
echo "a red the branch did not cause — the same command fails on the base:"
# ======================================================================================
map_gate "$ALWAYS"
branch spira/sp-innocent innocent.txt
rungate spira/sp-innocent > "$TMP/innocent.out" 2>&1; rc=$?
is "the gate refuses it as BASE_FAIL, not FAIL" 76 "$rc"
# AND THE VERDICT LINE CARRIES THE SUITE, because the landing pass keys an incident on it.
# A BASE_FAIL holds every branch of the repository and is filed as one bead against it,
# deduped on repository-plus-suite; read from the prose instead, that key would change the
# day somebody rewords a message and one broken base would file a fresh bead every pass.
want "and its verdict line names the suite, not only its prose" \
     "VERDICT=BASE_FAIL reason=base-red branch=spira/sp-innocent repo=repo suite=test-boxreader.sh" \
     "$(cat "$TMP/innocent.out")"

R="$(yield report)"
is "it lands as a GATE FAULT on arrival" 1 "$(f "$R" YIELD_FAULT)"
is "the defect column is untouched by it" 1 "$(f "$R" YIELD_DEFECT)"
is "and it is not left UNKNOWN"           0 "$(f "$R" YIELD_UNKNOWN)"
is "both reds are counted"                2 "$(f "$R" YIELD_REDS)"

# NAMING THE OFFENDER IS THE ACTION THIS NUMBER EXISTS FOR: a single suite responsible for a
# run of gate faults can be removed without touching the rest, and a fault attributed only to
# "the gate" gets the whole gate deleted.
want "the worst offender is named by its suite" "test-boxreader.sh" "$(f "$R" YIELD_TOP_FAULT)"

# AND A RED THAT NAMES NO SUITE IS ATTRIBUTED TO THE GATE'S OWN REASON, not to a guess. A
# wrongly named suite is worse than an unnamed one, because the wrong suite is the one
# somebody deletes.
map_gate 'echo "everything is broken" >&2; false'
branch spira/sp-anon anon.txt
rungate spira/sp-anon > "$TMP/anon.out" 2>&1
want "a red naming no suite records no suite" "suite=-" "$(cat "$YDIR"/*sp-anon* 2>/dev/null)"
want "and its verdict line says so too, rather than omitting the field" \
     "suite=-" "$(cat "$TMP/anon.out")"
R="$(yield report)"
want "and is attributed to the gate's reason instead" "base-red" "$(f "$R" YIELD_TOP_FAULT)"

# ======================================================================================
echo
echo "somebody says otherwise — a stated verdict overrides an inferred one:"
# ======================================================================================
# The DEFECT above was inferred. An aeon that knows the red was the box's fault says so, and
# what it says must win: a mechanism that overwrites a person's answer with its own guess is
# one nobody uses twice.
yield classify spira/sp-guilty GATE_FAULT "the suite read the state of the box" >/dev/null 2>&1
is "classify exits 0 on a branch it knows" 0 $?
R="$(yield report)"
is "the stated verdict replaced the inferred one" 0 "$(f "$R" YIELD_DEFECT)"
is "and it is now a gate fault"                   3 "$(f "$R" YIELD_FAULT)"
is "with nothing left inferred"                   0 "$(f "$R" YIELD_DEFECT_INFERRED)"
want "the reason it gave is kept" "read the state of the box" "$(yield list)"

yield classify spira/sp-nosuchbranch DEFECT >/dev/null 2>&1
is "classifying a branch with no red refuses rather than inventing one" 1 $?
yield classify spira/sp-guilty NOT_A_VERDICT >/dev/null 2>&1
is "and a verdict that is not one of the three is refused" 1 $?

# ======================================================================================
echo
echo "the window is a duration, and it bounds what is counted:"
# ======================================================================================
# A red older than the window is not recent news. The reads above all used the fixture's own
# window; this proves the bound is real rather than decorative, by asking for one so short
# that everything just recorded falls outside it.
# PROVED AGAINST A PLANTED OLD RED, not against the clock. Asserting that a window of one
# second excludes a record written a moment ago is a race the suite loses whenever the box is
# fast, and a flaky suite in a landing gate is the exact defect this bead exists to measure.
# The record format is the one the real writer produced above, several times over.
now_reds="$(f "$(yield report)" YIELD_REDS)"
{ printf 'at=%s\nlast=%s\nseen=1\nrepo=repo\nbranch=spira/sp-ancient\n' \
    "$(( $(date +%s) - WINDOW * 3 ))" "$(( $(date +%s) - WINDOW * 3 ))"
  printf 'bead=sp-ancient\ntree=-\noutcome=FAIL\nreason=branch-red\nsuite=-\n'
  printf 'verdict=UNKNOWN\nby=\nwhy=\n'
} > "$YDIR/repo.spira_sp-ancient.-.branch-red"
R="$(yield report)"
is "a red older than the window is not counted" "$now_reds" "$(f "$R" YIELD_REDS)"
is "and it does not show up as an unknown"      0 "$(f "$R" YIELD_UNKNOWN)"
# THE POSITIVE CONTROL: the same record IS found by a window wide enough to hold it, so the
# absence above is a bound doing its job rather than a reader that cannot see the file at all.
R="$(yield report --window $(( WINDOW * 4 )))"
is "a wide enough window does find it"     "$(( now_reds + 1 ))" "$(f "$R" YIELD_REDS)"
is "and it is UNKNOWN, counted as neither" 1 "$(f "$R" YIELD_UNKNOWN)"
rm -f "$YDIR/repo.spira_sp-ancient.-.branch-red"

# ======================================================================================
echo
echo "the cost, as a distribution against concurrency:"
# ======================================================================================
# A LANDING GATE NEVER RUNS SOLO. Every aeon runs one before it closes and the landing pass
# runs one per branch, so concurrency is the operating condition and not an edge case — and
# quoting a solo figure is how a gate gets adopted at one cost and turns out to have another
# (law-fixtures-carry-real-cadence). The fixture is the real meter format, at the real shape:
# two runs of one repository overlapping in time, and one that does not overlap anything.
row() {                  # row <seconds-ago-it-finished> <branch> <waited> <ran> [reason]
    printf '%s repo %s waited=%ss ran=%ss rc=0 %s\n' \
        "$(date -u -d "@$(( $(date +%s) - $1 ))" +%Y-%m-%dT%H:%M:%SZ)" "$2" "$3" "$4" "${5:-pass}"
}
# sp-x ran [now-400, now-300]; sp-y ran [now-350, now-310] — overlapping, so both contended.
# sp-z ran [now-100, now-40] — alone.
{ row 300 spira/sp-x 0 100
  row 310 spira/sp-y 0 40
  row  40 spira/sp-z 0 60
} > "$GATELOG"
R="$(yield report)"
is "the run that overlapped nothing is solo"        1 "$(f "$R" YIELD_SOLO_N)"
is "and its cost is reported"                      60 "$(f "$R" YIELD_SOLO_MED)"
is "the two that overlapped are contended"          2 "$(f "$R" YIELD_CONC_N)"
is "and the contended worst case is the real one" 100 "$(f "$R" YIELD_CONC_MAX)"

# A CACHED PASS IS NOT A COST. It neither queues for the tree nor runs a suite; leaving it in
# drags the median towards zero and hides the figure this exists to show. Planted as the gate
# itself writes it — by its reason, which is the gate's own word for it, not by being fast.
{ row 300 spira/sp-x 0 100
  row 310 spira/sp-y 0 40
  row  40 spira/sp-z 0 60
  row  30 spira/sp-c 0 0 cached
  row  20 spira/sp-c 0 0 cached
} > "$GATELOG"
R="$(yield report)"
is "reused verdicts are left out of the cost"       1 "$(f "$R" YIELD_SOLO_N)"
is "so the solo median is still the real one"      60 "$(f "$R" YIELD_SOLO_MED)"

# THE POSITIVE CONTROL FOR THE COST HALF: the same read against no log at all must say `?`,
# and it must not say 0. The assertions above are worth nothing unless this one holds — a
# cost probe that reports a missing log as "0s" is a gate that appears free.
rm -f "$GATELOG"
R="$(yield report)"
is "a missing gate log renders the solo median as ?" "?" "$(f "$R" YIELD_SOLO_MED)"
is "and the contended worst case as ?"               "?" "$(f "$R" YIELD_CONC_MAX)"
nowant "and nothing anywhere claims the gate costs 0s" "MED=0" "$R"

# ======================================================================================
echo
echo "the count has a positive control — a recorder that stopped is not a clean sheet:"
# ======================================================================================
# THE FAILURE THIS CATCHES is the only one this measurement actually has: the gate stops
# calling the recorder — a path moves, a call is dropped in a refactor — and every column
# reads 0 forever. That is the reassuring one of the two readings and nobody looks.
#
# The gate METER is the control, because it writes a row on every exit whether or not
# anything is measuring yield. Both halves are proved: the meter's reds are seen, and then
# they are seen to make the counts be withheld.
CTL="$TMP/ctl"; mkdir -p "$CTL"
ctl() {                  # ctl -> a report over an EMPTY record, against $GATELOG
    env -i HOME="$HOMEDIR" PATH="$PATH" \
        SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_REPO="$REPO" SPIRA_RUN="$RUN" \
        SPIRA_DB="$TMP/nonexistent-db" SPIRA_YIELD="$CTL" SPIRA_GATE_LOG="$GATELOG" \
        SPIRA_YIELD_WINDOW="$WINDOW" bash "$HERE/yield.sh" report
}
# FIRST, THE HEALTHY SHAPE: a log of passes and an empty record agree that nothing went red.
{ row 300 spira/sp-x 0 100; row 40 spira/sp-z 0 60; } > "$GATELOG"
# WITH NO RECORD DIRECTORY AT ALL and a meter that saw no reds, nothing is wrong: the gate
# has simply never refused anything here. That is `absent`, and it must not read the same as
# a recorder that stopped — which is the very next case.
R="$(env -i HOME="$HOMEDIR" PATH="$PATH" SPIRA_CONF="$TMP/nonexistent.conf" \
      SPIRA_REPO="$REPO" SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nonexistent-db" \
      SPIRA_YIELD="$TMP/never-written" SPIRA_GATE_LOG="$GATELOG" \
      SPIRA_YIELD_WINDOW="$WINDOW" bash "$HERE/yield.sh" report)"
is "no record and no reds in the meter is absent, not broken" absent "$(f "$R" YIELD_RECORDER)"

R="$(ctl)"
is "a log of passes and an empty record agree"        0   "$(f "$R" YIELD_REDS)"
is "and the recorder is reported as running"        ok    "$(f "$R" YIELD_RECORDER)"
is "with the meter seeing no reds either"             0   "$(f "$R" YIELD_LOG_REDS)"

# NOW THE BROKEN SHAPE, which is the same record and one red row in the meter.
printf '%s repo spira/sp-q waited=0s ran=90s rc=1 branch-red\n' \
    "$(date -u -d "@$(( $(date +%s) - 200 ))" +%Y-%m-%dT%H:%M:%SZ)" >> "$GATELOG"
R="$(ctl)"
is "the meter's red is seen"                          1   "$(f "$R" YIELD_LOG_REDS)"
is "and the recorder is called out as silent"    silent   "$(f "$R" YIELD_RECORDER)"
is "so the reds count is WITHHELD, not reported as 0" "?" "$(f "$R" YIELD_REDS)"
is "and so is the gate-fault column"                  "?" "$(f "$R" YIELD_FAULT)"
is "and the unknown column"                           "?" "$(f "$R" YIELD_UNKNOWN)"
want "and the human view says which side went quiet" "THE RECORDER IS NOT RUNNING" \
     "$(env -i HOME="$HOMEDIR" PATH="$PATH" SPIRA_CONF="$TMP/nonexistent.conf" \
        SPIRA_REPO="$REPO" SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nonexistent-db" \
        SPIRA_YIELD="$CTL" SPIRA_GATE_LOG="$GATELOG" SPIRA_YIELD_WINDOW="$WINDOW" \
        bash "$HERE/yield.sh" show)"

# AND THE REAL RECORD IS NOT WITHHELD BY THE SAME LOG. Dedupe means the two counts never
# agree on magnitude — a tree gated twice is two meter rows and one record — so a control
# that compared magnitudes would withhold every honest reading it was built to protect.
R="$(yield report)"
is "a record holding reds is reported, not withheld" ok "$(f "$R" YIELD_RECORDER)"
[ "$(f "$R" YIELD_REDS)" -gt 0 ] 2>/dev/null \
    && ok "and its count survives a meter that disagrees on magnitude" \
    || bad "and its count survives a meter that disagrees on magnitude" "got [$(f "$R" YIELD_REDS)]"
rm -f "$GATELOG"

# ======================================================================================
echo
echo "it reaches the actor that acts on it — the Ops sweep, not only a human at a pane:"
# ======================================================================================
# Ops is what reads the watchtower's snapshot and cuts beads from it. A yield only visible to
# somebody who went looking would be the same failure one layer up, since going to look is
# exactly what nobody did on the morning this measurement exists because of.
#
# `--show` gathers and prints and touches nothing, so nothing here can reach a database.
snap() {
    env -i HOME="$HOMEDIR" PATH="$PATH" \
        SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_REPO="$REPO" SPIRA_RUN="$RUN" \
        SPIRA_DB="$TMP/nonexistent-db" SPIRA_YIELD="$YDIR" SPIRA_GATE_LOG="$GATELOG" \
        SPIRA_YIELD_WINDOW="$WINDOW" \
        bash "$HERE/watchtower.sh" --show 2>/dev/null
}
S="$(snap)"
want "the sweep carries the reds"          "gate reds, last 2h" "$S"
want "it names the window it counted over" "last 2h"            "$S"
want "the defect column is there"          "the branch really was wrong" "$S"
want "so is the gate's own"                "the gate's own fault"        "$S"
want "and the unclassified count, on its own line" "never classified"    "$S"
want "the cost is split by concurrency"    "another gate overlapping"    "$S"
# A UNIT ON AN UNREADABLE FIELD INVITES READING IT AS A MEASUREMENT. The gate log was deleted
# above, so both cost figures are `?` here — and `?s` would read as a duration somebody
# forgot to fill in rather than as a probe that could not read.
nowant "an unreadable cost is not rendered with a unit" "?s median" "$S"

# AND THE SAME SNAPSHOT WITH NO RECORD AT ALL. This is the shape the measurement takes when
# it quietly stops being called, and it must arrive at Ops as `?` rather than as a gate that
# has caught nothing because there was nothing to catch.
mv "$YDIR" "$TMP/yield.away"
S="$(snap)"
want "a missing record reaches Ops as ?, never as a clean bill of health" \
     "gate reds, last 2h                  ?" "$S"
mv "$TMP/yield.away" "$YDIR"

# ======================================================================================
echo
echo "and the pane renders it without inventing a number:"
# ======================================================================================
# The collector's probe and the pane's row, checked as text rather than run: health.sh reads
# a snapshot written by a service and driving it here would be a test of tmux. What can go
# wrong silently is the pair drifting — the collector emitting a key the pane does not read —
# and that is what this compares.
COCK="$HERE/cockpit.sh"; PANE="$HERE/../cockpit/health.sh"
for k in SP_YIELD_REDS SP_YIELD_DEFECT SP_YIELD_FAULT SP_YIELD_UNKNOWN; do
    if grep -q "$k" "$COCK" 2>/dev/null && grep -q "$k" "$PANE" 2>/dev/null; then
        ok "$k is both written by the collector and read by the pane"
    else
        bad "$k is both written by the collector and read by the pane" \
            "written=$(grep -c "$k" "$COCK" 2>/dev/null) read=$(grep -c "$k" "$PANE" 2>/dev/null)"
    fi
done
# THE PANE'S DEFAULT FOR EVERY YIELD FIELD IS `?`. A `:-0` anywhere here would render a
# collector that never ran as a gate with a perfect record.
if grep -oE 'SP_YIELD_[A-Z_]*:-[^}]*' "$PANE" | grep -qv ':-?$'; then
    bad "every yield field on the pane defaults to ?" "one of them defaults to something else"
else
    ok "every yield field on the pane defaults to ?"
fi

echo
tl_summary
