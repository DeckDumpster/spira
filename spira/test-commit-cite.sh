#!/usr/bin/env bash
#
# test-commit-cite.sh — the landing gate refuses a branch whose commit message cites a
# bead id that does not exist, and never confuses "the store could not answer" with "every
# id exists".
#
#   ./test-commit-cite.sh
#
# DRIVEN THROUGH THE REAL gate.sh, against a REAL bead store (testdb.sh), never a model of
# either. `bd show`'s own shape is the whole mechanism this fence reads: a batch query
# answers with a JSON array holding only the ids that resolved, silently omitting the
# rest, and answers with a bare {"error": ...} object — still valid JSON — when none of
# them resolve. A hand-written stub of that shape would only ever prove the suite agrees
# with itself (law-prefer-the-real-dependency).
#
# defect: sp-vx3oi
# tier: T1
# covers: spira/gate.sh spira/commit-cite.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/testlib/gate-fixture.sh"
. "$HERE/testdb.sh"

command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }
testdb_require test-commit-cite

TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
gate_fixture_init "$TMP"
testdb_up commitcite || { echo "test-commit-cite: could not build fixture database"; exit 1; }

# RESOLVED, ABSOLUTE. conf.sh (sourced by lib.sh, sourced by both gate.sh and
# commit-cite.sh) unconditionally rebuilds PATH from $HOME — gate_fixture_run's own
# sandboxed HOMEDIR, not this box's real one — so a bare "bd-embedded" would resolve
# nowhere no matter what PATH is passed in. An absolute path bypasses PATH lookup
# entirely, in the gate subprocess and in commit-cite.sh's own child process alike.
BD_ABS="$(command -v "$SPIRA_BD")"

# rungate_real <branch> [VAR=VAL ...] — the same fixture runner, with SPIRA_DB/SPIRA_BD
# pointed at the REAL store testdb.sh built instead of gate_fixture_run's own
# SPIRA_DB_NONE.
rungate_real() {
    local br="$1"; shift
    gate_fixture_run "$br" repo SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD_ABS" "$@"
}

# commit_citing <branch> <bead-id> — one commit off origin/main whose subject cites the id,
# the way an aeon's own commit does (law-aeon-commits-name-their-bead).
commit_citing() {
    local br="$1" id="$2" extra="${3:-}" w
    w="$(mktemp -d)"
    git -C "$REPO" worktree add -q -b "$br" "$w" origin/main
    printf 'x\n' > "$w/f.txt"
    git -C "$w" add -A; git -C "$w" commit -q -m "feat: $id — a follow-up was filed as $id $extra"
    git -C "$REPO" worktree remove --force "$w"
}

echo "test-commit-cite.sh — every bead id cited in a landing commit must exist"

# --------------------------------------------------------------------------------------
# POSITIVE CONTROL. An ordinary branch that cites nothing passes, and reaches nowhere near
# the bead store — required before any refusal below means this fence discriminates rather
# than refusing everything (law-absence-needs-a-positive-control).
# --------------------------------------------------------------------------------------
gate_fixture_branch spira/sp-cc0 f0.txt fine
out="$(rungate_real spira/sp-cc0)"; rc=$?
is   "no citation at all: passes"        0 "$rc"
want "and says PASS"                     "VERDICT=PASS" "$out"

# --------------------------------------------------------------------------------------
# SEEN RED. A commit citing a bead id that does not exist in the store fails, naming the id.
# --------------------------------------------------------------------------------------
commit_citing spira/sp-cc1 sp-totallyfake999
out="$(rungate_real spira/sp-cc1)"; rc=$?
is     "a phantom citation: exits FAIL"       1 "$rc"
want   "names the reason"                     "reason=phantom-bead-id" "$out"
want   "names the phantom id"                 "sp-totallyfake999" "$out"
nowant "never a silent PASS"                  "VERDICT=PASS" "$out"

# --------------------------------------------------------------------------------------
# SEEN GREEN (positive control for the case above). The identical shape of commit, now
# citing a bead that is real and OPEN, passes — the fence discriminates rather than
# refusing every citation it sees.
# --------------------------------------------------------------------------------------
OPEN_ID="$(timeout 5 bd -C "$SPIRA_DB" create "commit-cite fixture: open" --json 2>/dev/null \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
[ -n "$OPEN_ID" ] || { echo "test-commit-cite: could not create the fixture bead"; exit 1; }
commit_citing spira/sp-cc2 "$OPEN_ID"
out="$(rungate_real spira/sp-cc2)"; rc=$?
is   "a citation naming a real OPEN bead: passes"  0 "$rc"
want "and says PASS"                               "VERDICT=PASS" "$out"

# --------------------------------------------------------------------------------------
# A citation naming a real bead is valid REGARDLESS OF STATUS — existence is the only
# claim a citation makes. A CLOSED bead satisfies it exactly as well as an open one.
# --------------------------------------------------------------------------------------
CLOSED_ID="$(timeout 5 bd -C "$SPIRA_DB" create "commit-cite fixture: closed" --json 2>/dev/null \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
testdb_restate "$CLOSED_ID" closed     # closed on purpose: fixture data, not a bd close (sp-voip5)
commit_citing spira/sp-cc3 "$CLOSED_ID"
out="$(rungate_real spira/sp-cc3)"; rc=$?
is   "a citation naming a real CLOSED bead: passes too"  0 "$rc"

# --------------------------------------------------------------------------------------
# A hyphenated fixture token is not a citation: sp-ow-mail must not truncate to the phantom
# sp-ow, and beside a real id only the real id is looked up.
# --------------------------------------------------------------------------------------
commit_citing spira/sp-cc4 "$OPEN_ID" "(fixture sp-ow-mail and sp-foo-bar)"
out="$(rungate_real spira/sp-cc4)"; rc=$?
is     "hyphenated fixture tokens beside a real id: passes"  0 "$rc"
nowant "no truncated stem is reported"                       "sp-ow" "$out"

# --------------------------------------------------------------------------------------
# STORE UNREADABLE renders as a gate error, NEVER as "every id exists". gate_fixture_run's
# own default env (SPIRA_DB pointed at a path that cannot exist) is exactly this case, and
# it is reached only because sp-cc1's branch cites something — a branch with no citation at
# all (the positive control above) never touches the store and would pass regardless.
# --------------------------------------------------------------------------------------
out="$(gate_fixture_run spira/sp-cc1 repo)"; rc=$?
is     "store unreadable: exits NO_VERDICT, never FAIL or PASS" 75 "$rc"
want   "names the reason"                                       "reason=commit-cite-fault" "$out"
nowant "never read as every id existing"                        "VERDICT=PASS" "$out"
nowant "and never charged as a branch FAIL"                     "VERDICT=FAIL" "$out"

# --------------------------------------------------------------------------------------
# MISSING-COMMIT-CITE. commit-cite.sh absent from this copy of the harness is a machinery
# fault — the same shape as missing-exclude/missing-skew — and must never be a silent pass.
# --------------------------------------------------------------------------------------
gate_fixture_branch spira/sp-cc4 f4.txt fine
mv "$SH/commit-cite.sh" "$TMP/commit-cite.sh.aside"
out="$(rungate_real spira/sp-cc4)"; rc=$?
is   "missing-commit-cite: exits NO_VERDICT" 75 "$rc"
want "names the reason"                      "reason=missing-commit-cite" "$out"
mv "$TMP/commit-cite.sh.aside" "$SH/commit-cite.sh"

# A hyphen-continued name (a fixture) is not a citation; a real id beside it still is.
commit_citing spira/sp-cc5 "$OPEN_ID"
w5="$(mktemp -d)"; git -C "$REPO" worktree add -q -b spira/sp-cc6 "$w5" origin/main
printf 'x\n' > "$w5/f.txt"; git -C "$w5" add -A
git -C "$w5" commit -q -m "feat: $OPEN_ID fixtures sp-st-livehold and sp-ow-mail"
git -C "$REPO" worktree remove --force "$w5"
out="$(rungate_real spira/sp-cc6)"; rc=$?
is   "hyphenated fixture names are not citations"  0 "$rc"
want "and says PASS"                               "VERDICT=PASS" "$out"
w7="$(mktemp -d)"; git -C "$REPO" worktree add -q -b spira/sp-cc7 "$w7" origin/main
printf 'x\n' > "$w7/f.txt"; git -C "$w7" add -A
git -C "$w7" commit -q -m "feat: sp-st-livehold, and sp-realshape99."
git -C "$REPO" worktree remove --force "$w7"
out="$(rungate_real spira/sp-cc7)"; rc=$?
is   "a real-shaped phantom beside a fixture still fails"  1 "$rc"
want "names the phantom"                                   "sp-realshape99" "$out"
nowant "and not the fixture"                               "sp-st " "$out"

tl_summary
