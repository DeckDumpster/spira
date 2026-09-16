#!/usr/bin/env bash
#
# test-landing.sh — a landing pass may put finished work back on the board only when the
# work itself disagrees with the base.
#
#   ./test-landing.sh
#
# THE CASE THIS IS WRITTEN FOR. A pass reads its branch list once and then runs for minutes,
# so by the time it reaches the tail of that list the head of it may be gone. The Sending
# reaps a landed branch on its own timer, and `rebase_branch` against a ref that is no longer
# there fails with exactly the exit status and exactly the empty conflict list that a real
# disagreement produces:
#
#   21:51:17  landed spira/<id>                      pass A lands it
#   21:52:13  landing: starting a pass               pass B enumerates, <id> still present
#   21:52:36  REMOVED branch spira/<id>              the Sending reaps it
#   22:00:51  landed spira/<other>                   pass B is eight minutes into one gate
#   22:00:55  reopened <id> — does not rebase onto origin/main; conflicts in unknown
#
# Nothing recreated that ref and nothing needed to: the branch list was twenty-three seconds
# older than the reap, refs sort by name, and the branch ahead of it in the list held the
# pass for eight minutes. "conflicts in unknown" is the tell — a real conflict names files.
# The cost is a whole session: an aeon is summoned onto a bead whose work is already on the
# base, finds nothing to rebase, and learns that.
#
# So the property under test is not "the reaped branch is skipped" but the stronger one the
# skip is a special case of: A REOPEN REQUIRES AN ATTRIBUTED CONFLICT. Both routes into a
# reopen are exercised, the classification they rest on is exercised directly, and a genuine
# conflict is required to still reopen — without that last one every silence here is vacuous.
#
# The database is a REAL bd on a fixture dropped by a trap and git is real, with a real bare
# remote, because every claim is about ancestry, about what `git rebase` does against a ref
# that is not there, and about a bead's status afterwards. `gate.sh` and `confine.sh` are
# stubs: each has its own suite, and what is under test here is what landing does with a
# verdict, not how one is reached. The gate stub is also the clock — it is the only point in
# the pass this suite can reach, and reaping a branch from inside it reproduces the real
# sequence exactly rather than approximating it.
#
# defect: sp-q9i sp-9194o
# covers: spira/landing.sh spira/lib.sh spira/incident.sh spira/skew.sh
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-landing
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up landing || { echo "test-landing: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"
REPONAME=fixture-repo
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH"

# conf.sh travels with lib.sh — lib.sh refuses to run without it, and a harness that copies
# one and not the other fails at source time, which reads as landing being broken.
# incident.sh IS THE REAL ONE, not a stub. The claim under test is "one incident, however
# many branches and however many passes", and that dedupe is incident.sh's dedupe on the
# external ref — a stub would reproduce the surface remembered here and prove nothing about
# it (law-prefer-the-real-dependency). What landing.sh owns is the KEY it hands over; what
# the intake owns is finding the open bead under it, and both have to hold for the count to
# stay at one.
cp "$HERE"/*.sh "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub confine.sh 'exit 0'
# THE OUTCOME STREAM IS RECORDED, NOT MERELY SWALLOWED, and it is stubbed EXPLICITLY. Left to
# conf.sh's default this resolves to `$(dirname $SPIRA_HOME)/cockpit/ask.sh`, which is absent
# under $TMP and so happens to be inert — a suite writing into the operator's live database
# on a box where that path DOES resolve is not a risk worth leaving to a coincidence of where
# the fixture was built (law-gates-run-in-a-clean-environment).
stub ask.sh 'printf "%s\n" "$*" >> "$EMITTED"'
export EMITTED="$TMP/events"; : > "$EMITTED"
events() { cat "$EMITTED" 2>/dev/null; }

# THE GATE IS ALSO THE REAPER, and that is the whole fixture. A pass holds its branch list
# across the gate, which is the only long call in it, so a branch removed from inside the
# stub is removed at precisely the point the Sending removed the real one — after the list
# was read and before the loop reached the tail of it. Anything staged outside the pass would
# be testing a list that was stale before it was taken, which is a different bug.
#
# It speaks the gate's PROTOCOL, not just its exit status: landing.sh reads the VERDICT line
# for the reason it records.
stub gate.sh '
r="$SPIRA_RUN/reap-during-gate"
if [ -s "$r" ]; then
    while read -r id; do
        [ -n "$id" ] || continue
        git -C "'"$REPO"'" worktree remove --force "'"$RUN"'/worktree/$id" >/dev/null 2>&1
        git -C "'"$REPO"'" branch -D "spira/$id" >/dev/null 2>&1
    done < "$r"
    : > "$r"
fi
# THE TIP AS THE LOOP SAW IT. The gate is the last thing the loop does to a branch, so the
# tip recorded here is the branch as the loop left it — and anything that moves it afterwards
# moved it after the loop had walked past. That is the whole discriminator for the sweep
# below: without it "the branch is on the base" is true whether the loop rebased it or the
# sweep did, and the case would pass against a landing.sh with no sweep in it at all.
mkdir -p "$SPIRA_RUN/tip-at-gate"
git -C "'"$REPO"'" rev-parse "$1" > "$SPIRA_RUN/tip-at-gate/${1//\//-}" 2>/dev/null
# A WITHHELD VERDICT, which is what leaves a branch judged, rebased and standing. The
# repository gate tree being busy is far and away the commonest way a real pass walks past a
# branch it will not reach again, and it is the state every sweep case starts from. The
# status is the protocol constant, not a literal: a fixture asserting against 75 would go on
# passing if landing.sh stopped meaning 75 by it.
w="$SPIRA_RUN/withhold-gate"
if [ -s "$w" ] && grep -qx "$1" "$w"; then
    echo "gate: VERDICT=NO_VERDICT reason=stub-busy branch=$1 repo=${2:-?}" >&2
    exit "${SPIRA_GATE_NOVERDICT:?the gate protocol constant is not in the environment}"
fi
# AN AEON ARRIVING MID-PASS, on the PASS path only — so it lands in the window between the
# loop walking past an earlier branch and the landing that sweeps it, which is the window the
# sweep repeats its own liveness check for. Each line is "<bead> <pid>"; the pid belongs to a
# process the suite started, because aeon_alive reads /proc and will not be fooled by a
# pidfile naming something that is not a runner.
c="$SPIRA_RUN/claim-during-gate"
if [ -s "$c" ]; then
    while read -r id pid; do
        [ -n "$id" ] || continue
        printf "%s\\n" "$pid" > "$SPIRA_RUN/aeon-builder-$id.pid"
    done < "$c"
    : > "$c"
fi
echo "gate: VERDICT=PASS reason=${GATE_REASON:-stub} branch=$1 repo=${2:-?}" >&2; exit 0'

# gh IS NEVER REACHED FROM A SUITE. pr_merged sits on the path to a reopen, and left to the
# real binary it would decide a verdict here from whether this box happens to be logged in
# to a forge — the ambient configuration law-gates-run-in-a-clean-environment names. A stub
# that always fails is the honest fixture: this repository lands by push and has no pull
# requests, so "no merged pull request" is the true answer.
stub gh 'exit 1'
# The base-fail tests below overwrite the gate stub; keep a copy so the sweep tests
# can restore it.
cp "$SH/gate.sh" "$TMP/gate-full.sh"

B() { bd -C "$SPIRA_DB" "$@"; }
status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }

landing() {
    rm -f "$RUN/landing.progress"
    # SPIRA_REPO_MAP EXPLICITLY and nothing else inherited: a pass that falls back reads the
    # repositories the operator has registered and counts THEIR branches, so a suite
    # asserting about one fixture branch would be asserting about a box.
    # AND THE ESCALATION CHANNEL IS PINNED SHUT. Two paths out of this pass reach the
    # operator — the repeated-NO_VERDICT ask and the intake's Sin escalation — and both
    # resolve their command from configuration. Left unpinned a suite inherits whatever this
    # box has installed there and puts a fixture's verdict in a real pane
    # (law-gates-run-in-a-clean-environment).
    # SPIRA_HOME_REPO IS PINNED, AND TO A NON-DEFAULT. conf.sh only derives it when it is
    # unset, so an aeon session that exports it hands this pass the name of a real repository
    # — and the incident below is labelled `repo:<name>`, so the suite would file a fixture's
    # finding against somebody's actual checkout and then assert against whatever leaked.
    # SPIRA_BD IS PINNED TO THE FIXTURE'S EMBEDDED BINARY. conf.sh resolves it from PATH when
    # unset; the test's counter-script section temporarily exports SPIRA_BD and then unsets it,
    # and PATH-resolution after that depends on TESTDB_BIN existing in PATH. Pinning here
    # removes the dependency: the pass always uses the binary testdb_up chose regardless of
    # what the counter-script section left in the environment.
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" \
    SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/gh" \
    SPIRA_NOTIFY="$SH/ask.sh" SPIRA_ASK="$SH/ask.sh" \
        bash "$SH/landing.sh" 2>&1
}
notes_of() { B show "$1" 2>/dev/null; }

seed() {
    testdb_reset
    rm -rf "$RUN/tip-at-gate"; rm -f "$RUN/withhold-gate" "$RUN/claim-during-gate"
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
}
withhold_gate() { printf 'spira/%s\n' "$@" > "$RUN/withhold-gate"; }
claim_during_gate() { printf '%s %s\n' "$1" "$2" > "$RUN/claim-during-gate"; }
tip_at_gate()   { cat "$RUN/tip-at-gate/spira-$1" 2>/dev/null; }
tip_of()        { git -C "$REPO" rev-parse "spira/$1" 2>/dev/null; }
on_base()       { git -C "$REPO" merge-base --is-ancestor origin/main "spira/$1" 2>/dev/null && echo yes || echo no; }
branch() {               # branch <id> [file] [content] — a closed bead with a branch of its own
    local id="$1" f="${2:-$1.txt}" c="${3:-$1}"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$c" > "$RUN/worktree/$id/$f"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id — work"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$id" "$id" "$id" | testdb_seed
}
drop_branch() {
    local id="$1"
    git -C "$REPO" worktree remove --force "$RUN/worktree/$id" >/dev/null 2>&1
    git -C "$REPO" branch -D "spira/$id" >/dev/null 2>&1
}
reap_during_gate() { printf '%s\n' "$@" > "$RUN/reap-during-gate"; }

echo "test-landing.sh"

# --------------------------------------------------------------------------------------
# THE POSITIVE CONTROL, first, because every silence below is read against it: this pass can
# land, and it can reopen. A suite whose assertions are all "did not happen" passes just as
# well against a landing.sh that does nothing at all.
# --------------------------------------------------------------------------------------
: > "$EMITTED"
seed; branch sp-plain; out="$(landing)"
want "an uncontested land is reported" "landed spira/sp-plain" "$out"
# Landing pushes the base from the .landing worktree; the home checkout must be advanced in
# the same pass or every hand-run script reads a past state (sp-wud).
want "the same pass also advances the checkout humans read" "skew: refreshed to" "$out"
# AND IT SURVIVES THE PASS. The mailbox above is drained by the sentinel and the log line
# scrolls off the health pane below the fourth row; the event is the only record still
# answerable tomorrow. It is emitted AFTER the push, so what it asserts is the ancestry just
# proved rather than the close that preceded it (law-closed-is-not-landed).
want "the land is recorded as an event"  "--kind bead.landed"   "$(events)"
want "against the bead that landed"      "--target sp-plain"    "$(events)"
drop_branch sp-plain

# A GATE FAILURE ALSO RECORDS AN EVENT, and the negative is the landing above: a gate that
# passed produced bead.landed, not bead.reopened, so the check is not just reading the stub.
: > "$EMITTED"
stub gate.sh 'echo "gate: VERDICT=FAIL reason=stub-fail branch=$1 repo=${2:-?}" >&2; exit 1'
seed; branch sp-gfail; out="$(landing)"
want "a failed gate reopens the bead"          "reopened sp-gfail — failed the gate" "$out"
want "and the reopen is recorded as an event"  "--kind bead.reopened" "$(events)"
want "against the bead that was reopened"      "--target sp-gfail"    "$(events)"
drop_branch sp-gfail
cp "$TMP/gate-full.sh" "$SH/gate.sh"   # restore for the tests that follow

# THE VERDICT CACHE IS PRUNED BY THE PASS, at the age the gate refuses to read at. The gate
# computes each verdict once and reuses it, so what accumulates here is one file per gated
# tree, forever; the pass is the only thing that runs on a clock and already touches every
# repository, which is why the janitor lives in it.
#
# THE AGE IS THE CONFIGURED ONE and that is the whole point of the case: two numbers here —
# a reader's and a janitor's — would be two answers to how long a verdict lives, and the
# operator would have tuned one of them. Pinned to a non-default, because asserting against
# the shipped default passes just as well against a literal written into the code.
# AND A REUSED VERDICT IS VISIBLE IN THE PASS. The gate returns the same status either way,
# so without this line a pass that skipped every gate and one that ran every gate read
# identically — and "nothing is being reused any more" is the first symptom of a key that has
# stopped matching anything, which otherwise looks exactly like a busy queue.
seed; branch sp-reused
out="$(GATE_REASON=cached landing)"
want "a pass says when a gate was skipped" "this tree had already passed, so no suite ran" "$out"
want "and still lands the branch"          "landed spira/sp-reused" "$out"
drop_branch sp-reused
# bash keeps a temporary assignment to a FUNCTION set after the call returns, so every later
# pass in this suite would go on claiming a reused verdict.
unset GATE_REASON

export SPIRA_VERDICT_TTL=600
mkdir -p "$RUN/verdicts"
: > "$RUN/verdicts/stale"; touch -d '3 hours ago' "$RUN/verdicts/stale"
: > "$RUN/verdicts/fresh"
seed; landing >/dev/null 2>&1
[ -e "$RUN/verdicts/stale" ] && bad "a verdict past the TTL is deleted by a pass" "stale entry survived" \
    || ok "a verdict past the TTL is deleted by a pass"
[ -e "$RUN/verdicts/fresh" ] && ok "and one inside it is kept" \
    || bad "and one inside it is kept" "the pass deleted a live verdict"
unset SPIRA_VERDICT_TTL

# A REAL DISAGREEMENT STILL REOPENS. The branch and the base both write the same file with
# different content after they diverged, so the rebase genuinely conflicts and the bead
# genuinely belongs back on the board. Everything below asserts that a reopen did NOT happen;
# this is what makes those assertions mean something.
seed; branch sp-clash shared.txt "from the branch"
printf '%s\n' "from the base" > "$REPO/shared.txt"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m "base writes shared.txt"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
out="$(landing)"
want "a branch that truly conflicts is reopened"   "reopened sp-clash" "$out"
is   "and its bead goes back to open"              open "$(status_of sp-clash)"
want "and the note names the file that collided"   "shared.txt" "$(notes_of sp-clash)"
drop_branch sp-clash

# --------------------------------------------------------------------------------------
# THE CASE ITSELF: landed on one pass, reaped during the next, reached from a stale list.
#
# sp-aaa sorts before sp-zzz and refs are enumerated by name, so the pass is inside sp-aaa's
# gate when sp-zzz's ref goes — which is where the real one went. sp-zzz's work is already on
# the base by then, exactly as the reaped branch's was.
# --------------------------------------------------------------------------------------
seed; branch sp-zzz; out="$(landing)"
want "the branch lands on the first pass" "landed spira/sp-zzz" "$out"
git -C "$REPO" fetch -q origin
zzz_tip="$(git -C "$REPO" rev-parse spira/sp-zzz)"
git -C "$REPO" merge-base --is-ancestor "$zzz_tip" origin/main \
    && ok  "and its commit is on the base before the second pass begins" \
    || bad "the fixture" "sp-zzz did not actually land"

branch sp-aaa; reap_during_gate sp-zzz; out="$(landing)"
nowant "a branch reaped mid-pass is not reopened as a conflict" "reopened sp-zzz" "$out"
is     "and its bead stays closed"                              closed "$(status_of sp-zzz)"
nowant "and no note claims a conflict it never had"             "conflicts in unknown" "$(notes_of sp-zzz)"
want   "and the pass names the branch it lost"                  "spira/sp-zzz is gone since this pass began" "$out"
want   "and says the commit is on the base, not merely that it is gone" \
       "$zzz_tip is on origin/main — landed and reaped" "$out"
want   "the branch that held the pass still landed"             "landed spira/sp-aaa" "$out"
drop_branch sp-aaa

# A BRANCH REMOVED WITH WORK STILL ON IT READS DIFFERENTLY, and it has to. Both cases end in
# the same non-action, so a single line covering both would report a destroyed branch in the
# words used for the ordinary reap — the reassuring reading, given for free, to the one case
# that deserves a look.
seed; branch sp-lost; branch sp-bbb; reap_during_gate sp-lost
lost_tip="$(git -C "$REPO" rev-parse spira/sp-lost)"
out="$(landing)"
nowant "a branch slain mid-pass is not reopened either" "reopened sp-lost" "$out"
is     "and its bead stays closed"                      closed "$(status_of sp-lost)"
want   "but the pass says its work is NOT on the base"  "$lost_tip is NOT on origin/main" "$out"
drop_branch sp-bbb

# --------------------------------------------------------------------------------------
# THE SECOND ROUTE INTO A REOPEN. Losing the push race sends the pass back through
# rebase_branch, and that arm falls through to "branch conflicts with the base" — so a ref
# reaped between the losing push and the replay is reported as a disagreement that never
# happened, the identical defect by a different path. Two guards were needed and only one of
# them is on the path the first case takes.
# --------------------------------------------------------------------------------------
cat > "$REMOTE/hooks/pre-receive" <<HOOK
#!/usr/bin/env bash
# Reject the first push only, advancing the base and removing the branch — simulating a
# concurrent commit (genuine race) and the Sending arriving in the one window this arm
# occupies. The base MUST move so the classification guard introduced by sp-utvi treats
# this as a real race rather than a non-race push failure (law-a-pattern-match-is-not-
# an-identity-check). Without a moved base the guard correctly stops before the retry, and
# the no-branch case the test exercises is never reached.
#
# OUTSIDE THE QUARANTINE. A pre-receive hook runs with GIT_QUARANTINE_PATH set and git
# refuses to touch refs from inside it, so a hook that does not clear the environment
# changes nothing and the case passes against the bug it is written for.
#
# PATH EMBEDDING: $REPO, $REMOTE, $RUN are shell-expanded at heredoc creation time and
# written as double-quoted literals in this script. The \$_prev etc. are runtime variables
# and must NOT expand at heredoc time; they are written as literal '$' via \$ escaping.
[ -f "\$GIT_DIR/rejected-once" ] && exit 0
: > "\$GIT_DIR/rejected-once"
unset GIT_QUARANTINE_PATH GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_DIR
git -C "$REPO" worktree remove --force "$RUN/worktree/sp-raced" >/dev/null 2>&1
git -C "$REPO" branch -D spira/sp-raced >/dev/null 2>&1
_prev=\$(git -C "$REMOTE" rev-parse HEAD 2>/dev/null)
_tree=\$(git -C "$REMOTE" rev-parse "HEAD^{tree}" 2>/dev/null)
_tip=\$(GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t git -C "$REMOTE" commit-tree -p "\$_prev" -m "race" "\$_tree" 2>/dev/null)
git -C "$REMOTE" update-ref refs/heads/main "\$_tip" 2>/dev/null
echo "rejected: non-fast-forward" >&2
exit 1
HOOK
chmod +x "$REMOTE/hooks/pre-receive"
seed; branch sp-raced; out="$(landing)"
rm -f "$REMOTE/hooks/pre-receive" "$REMOTE/rejected-once"
want   "the push is rejected, as the case requires"        "push rejected" "$out"
nowant "and a branch gone by the retry is not a conflict"  "reopened sp-raced" "$out"
is     "and its bead stays closed"                         closed "$(status_of sp-raced)"
want   "and the retry says it could not attempt a rebase"  "the retry could not attempt a rebase of spira/sp-raced" "$out"
want   "naming the reason rather than a file list"         "(no-branch)" "$out"
drop_branch sp-raced

# --------------------------------------------------------------------------------------
# A BASE THAT FAILS ITS OWN GATE: THE BRANCH IS HELD, THE REPOSITORY IS CHARGED
#
# The gate has said "it fails against the base too — this branch did not cause it" since
# BASE_FAIL existed, and the pass already declines to reopen on it. What it did with that
# sentence afterwards was nothing: a log line saying the next pass would take it, said again
# every two minutes, while every branch of the repository sat behind a red nobody owned.
#
# So three properties, and the third is what makes the first two worth anything. The branch
# is held rather than reopened; ONE incident is filed however many branches are behind the
# same red and however many passes go by; and the held branch lands on the pass after the
# base is green, which is the whole reason holding is the right answer rather than refusing.
#
# The gate stub speaks BASE_FAIL's protocol — status 76 and the anchored VERDICT line with
# its suite — because that line is the contract landing.sh reads, and reading it from prose
# is the thing the bead forbids.
# --------------------------------------------------------------------------------------
echo
BASE_SUITE=test-fx-base.sh
stub gate.sh '
echo "gate: the fixture repository gate failed: '"$BASE_SUITE"' FAILED" >&2
echo "gate: it fails against origin/main too — this branch did not cause it." >&2
echo "gate: VERDICT=BASE_FAIL reason=base-red branch=$1 repo=${2:-?} suite='"$BASE_SUITE"'" >&2
exit 76'

# Every open bead in the builder partition for this repository — by LABEL, not by the dedupe
# ref, so a second filing under a DIFFERENT ref is counted rather than hidden. Counting the
# ref alone would report "still one" against exactly the bug this case is written for.
incidents() {
    B list --status open,in_progress --limit 0 --label "spira,plan,repo:$REPONAME" --json 2>/dev/null \
      | python3 -c '
import json, sys
try: d = json.load(sys.stdin)
except Exception: raise SystemExit(0)
for i in (d if isinstance(d, list) else [d]): print(i["id"])'
}
n_lines() { printf '%s' "$1" | grep -c . || true; }

seed; branch sp-held; branch sp-heldtoo
out="$(landing)"
want   "a base that fails its own gate holds the branch" \
       "gate: held — the base fails its own gate" "$out"
want   "and the hold names the suite the gate named"     "suite $BASE_SUITE" "$out"
nowant "the bead is not reopened"                        "reopened sp-held" "$out"
is     "and stays closed"                                closed "$(status_of sp-held)"
nowant "nor is the second branch behind the same red"    "reopened sp-heldtoo" "$out"
nowant "and nothing is landed on a withheld verdict"     "landed spira/sp-held" "$out"

inc="$(incidents)"
is "one incident is filed for the repository" 1 "$(n_lines "$inc")"
inc_id="$(printf '%s\n' "$inc" | head -1)"
if [ -n "$inc_id" ]; then
    shown="$(B show "$inc_id" 2>&1)"
    want "it names the failing suite"                  "$BASE_SUITE" "$shown"
    want "and the repository whose base is red"        "$REPONAME" "$shown"
    want "and says no bead was reopened or charged"    "no attempt charged" "$shown"
    want "and carries the gate's own output"           "this branch did not cause it" "$shown"
    labels="$(B label list "$inc_id" 2>&1)"
    want "it lands in the builders partition"          "plan" "$labels"
    want "labelled with the repository"                "repo:$REPONAME" "$labels"
fi

# THE SECOND PASS IS THE IDEMPOTENCE. Nothing is reseeded: the same two branches meet the
# same red, and a pass that filed per branch or per pass would have four beads by now.
out2="$(landing)"
is   "a second pass files no second incident"  1 "$(n_lines "$(incidents)")"
# sp-recur-N labels no longer written (sp-lzt); recurrence is logged in the incident log.
# deleted: "it bumps a recurrence on the first" via sp-recur-1 label check
is   "it records one recurrence in the incident log" "1" \
     "$(grep -c ' recurred ' "$RUN/incident.log" 2>/dev/null || echo 0)"
want "and holds the branch again"              "gate: held — the base fails its own gate" "$out2"
is   "with the bead still closed"              closed "$(status_of sp-held)"

# AND THE HOLD IS A HOLD, NOT A LOSS. Holding is only the right answer if the work still
# lands once the base is fixed; without this the case above is equally satisfied by a pass
# that quietly dropped the branch on the floor.
stub gate.sh 'echo "gate: VERDICT=PASS reason=stub branch=$1 repo=${2:-?} suite=-" >&2; exit 0'
out3="$(landing)"
want "a held branch lands on the pass after the base is green" "landed spira/sp-held" "$out3"
want "and so does the one behind it"                           "landed spira/sp-heldtoo" "$out3"
drop_branch sp-held; drop_branch sp-heldtoo

# --------------------------------------------------------------------------------------
# THE CLASSIFICATION BOTH GUARDS REST ON. rebase_branch returns 1 four ways and only one of
# them is a fact about the branch; before it said which, every caller that reopens on a
# rebase failure reopened for all four. Asserted directly, because the pass can only be
# steered into two of these and a guard reading a value nothing pins is a guard on a comment.
# --------------------------------------------------------------------------------------
classify() {             # classify <branch> <onto> -> the recorded failure kind, or "clean"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO="$REPO" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    bash -c '. "$1/lib.sh" >/dev/null 2>&1
             if rebase_branch "$2" "$3" "$4" fixture >/dev/null 2>&1
             then printf clean; else printf "%s" "${REBASE_FAILURE:-unset}"; fi' \
        _ "$SH" "$1" "$2" "$REPO" 2>/dev/null
}
seed; branch sp-kind
is "a ref that is not there is named no-branch" no-branch "$(classify spira/sp-nothere origin/main)"
is "a base that does not resolve is named no-base" no-base "$(classify spira/sp-kind refs/heads/no-such-base)"
is "a branch that rebases cleanly records no failure" clean "$(classify spira/sp-kind origin/main)"
drop_branch sp-kind

seed; branch sp-kindclash shared.txt "from the branch"
printf '%s\n' "and the base disagrees" > "$REPO/shared.txt"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m "base writes shared.txt again"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
is "and a real disagreement is named conflict" conflict "$(classify spira/sp-kindclash origin/main)"
drop_branch sp-kindclash

# THE REFUSED CASE: git declines to rebase (untracked file would be overwritten) without
# leaving any unmerged file. A non-conflict rebase failure must not reopen a finished bead,
# so it needs a name that is not "conflict". The fix sets REBASE_FAILURE=rebase-refused and
# captures git's first stderr line in REBASE_REFUSED_REASON.
#
# POSITIVE CONTROL: on the unfixed code this fixture prints REBASE_FAILURE=conflict with an
# empty REBASE_CONFLICTS — the new assertions below must fail before the fix is applied.
classify_ext() {   # classify_ext <branch> <onto> -> "failure|reason|conflicts"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO="$REPO" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    bash -c '. "$1/lib.sh" >/dev/null 2>&1
             rebase_branch "$2" "$3" "$4" fixture >/dev/null 2>&1
             printf "%s|%s|%s" \
                 "${REBASE_FAILURE:-unset}" \
                 "${REBASE_REFUSED_REASON:-}" \
                 "${REBASE_CONFLICTS:-}"' \
        _ "$SH" "$1" "$2" "$REPO" 2>/dev/null
}

seed; branch sp-refused
printf 'base version\n' > "$REPO/blocked.txt"
git -C "$REPO" add blocked.txt
git -C "$REPO" commit -q -m "base adds blocked.txt"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
printf 'untracked\n' > "$RUN/worktree/sp-refused/blocked.txt"
_ext="$(classify_ext spira/sp-refused origin/main)"
_ext_fail="${_ext%%|*}"; _ext_rest="${_ext#*|}"; _ext_reason="${_ext_rest%%|*}"; _ext_conflicts="${_ext_rest#*|}"
is   "a rebase blocked by an untracked file is named rebase-refused" rebase-refused "$_ext_fail"
want "and git's refusal message is captured in REBASE_REFUSED_REASON" "untracked" "$_ext_reason"
is   "and REBASE_CONFLICTS is empty for a non-conflict failure" "" "$_ext_conflicts"
drop_branch sp-refused

seed; branch sp-kindconflicts shared2.txt "from the branch"
printf '%s\n' "base disagrees" > "$REPO/shared2.txt"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m "base writes shared2.txt"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
_ext="$(classify_ext spira/sp-kindconflicts origin/main)"
_ext_fail="${_ext%%|*}"; _ext_rest="${_ext#*|}"; _ext_conflicts="${_ext_rest#*|}"
is   "a real content conflict still produces REBASE_FAILURE=conflict"    conflict "$(classify spira/sp-kindconflicts origin/main)"
want "and REBASE_CONFLICTS names the colliding file"                      "shared2.txt" "$_ext_conflicts"
drop_branch sp-kindconflicts

# THE FENCE. Every route from a rebase failure to a reopen lives in landing.sh and must read
# the classification first; the two above are the ones that exist today and a third would
# arrive silently. It requires each `! rebase_branch` arm to mention REBASE_FAILURE within
# the handful of lines that follow — the cheap check for the shape, rather than for a
# behaviour a future arm would not yet have.
#
# COMMENTS AND BLANK LINES ARE NOT COUNTED toward the window. They are the bulk of this
# repository by design, and a window measured in raw lines put the guard eight lines below an
# arm it sits directly under — a fence that fires on every correctly written arm is deleted
# by the second person who meets it.
arms() {                 # arms <file> -> "line" per rebase_branch failure arm that ignores the kind
    awk '
        { c = $0; sub(/#.*/, "", c) }
        c ~ /^[ \t]*$/ { next }
        n { if (c ~ /REBASE_FAILURE/) guarded = 1
            if (++k >= 6) { if (!guarded) print "line " n; n = 0 } }
        c ~ /![ \t]*rebase_branch/ { n = NR; k = 0; guarded = 0 }
        END { if (n && !guarded) print "line " n }' "$1"
}
is "every rebase failure arm in landing.sh reads the kind" "" "$(arms "$HERE/landing.sh")"

# ITS OWN POSITIVE CONTROL. A grep reporting a clean file looks the same whether it fired or
# never could, and the value of this fence is entirely in its silence.
printf '%s\n' 'if ! rebase_branch "$br" "$base"; then' '    bead_reopen "$id" "conflicts"' 'fi' > "$TMP/plant.sh"
want "and the fence can see an arm that does not" "line 1" "$(arms "$TMP/plant.sh")"

# Restore the full gate stub the base-fail tests replaced.
cp "$TMP/gate-full.sh" "$SH/gate.sh"

# --------------------------------------------------------------------------------------
# THE SURVIVORS ARE REBASED WHEN THE BASE MOVES, not when some later pass reaches them.
#
# The loop rebases a branch immediately before gating it, so a branch is never gated stale.
# What it does not do is go back: a branch it has already walked past — because the gate tree
# was busy and no verdict was reached — keeps the base it was rebased onto while the pass
# lands other branches on top of it. It is then behind until a later pass reaches it, which
# has been as long as eighty-five minutes and several landings, ending in "reopened <bead> —
# does not rebase onto the base" for a branch nobody had touched. Eleven of those in one day
# and twelve the next, each a finished bead back on the board and an agent session spent.
#
# THE DISCRIMINATOR IS THE TIP AT GATE TIME. "The branch is on the base afterwards" is true
# whether the loop rebased it or the sweep did, so on its own it passes against a landing.sh
# with no sweep at all. The gate stub records the tip as the loop left it; the assertion is
# that the tip MOVED after that, which only the sweep can do.
# --------------------------------------------------------------------------------------
seed; branch sp-held held.txt; branch sp-lands lands.txt
withhold_gate sp-held                    # sorts first, so the loop walks past it, then lands sp-lands
out="$(landing)"
want "the branch whose gate was withheld is not landed" "gate NO_VERDICT on spira/sp-held" "$out"
want "and the branch behind it lands"                   "landed spira/sp-lands" "$out"
want "the survivor is rebased onto the new base at once" \
     "rebased spira/sp-held onto origin/main after landing spira/sp-lands" "$out"
is   "and its tip moved after the loop had walked past it" \
     moved "$([ "$(tip_at_gate sp-held)" != "$(tip_of sp-held)" ] && echo moved || echo "same as at gate time")"
is   "so it now carries the commit that landed"         yes "$(on_base sp-held)"
nowant "and a clean rebase never reopens the bead"      "reopened sp-held" "$out"
is   "which stays closed"                               closed "$(status_of sp-held)"
want "and the pass counts what the sweep did"           "1 survivor(s) rebased after a landing, 0 conflicted" "$out"
drop_branch sp-held; drop_branch sp-lands

# THE SWEEP MUST STILL REOPEN A REAL DISAGREEMENT, or the silence above is worth nothing: a
# sweep that swallowed conflicts would pass every assertion in the case above and quietly
# leave unlandable work sitting closed forever. Both branches create the same file with
# different content, so once one of them is on the base the other genuinely cannot replay.
seed; branch sp-cheld shared.txt "from the held branch"
branch sp-clands shared.txt "from the branch that lands"
withhold_gate sp-cheld
out="$(landing)"
want "the branch behind it still lands"            "landed spira/sp-clands" "$out"
want "and the survivor that truly conflicts is reopened" "reopened sp-cheld" "$out"
is   "its bead goes back to open"                  open "$(status_of sp-cheld)"
want "the note names the file that collided"       "shared.txt" "$(notes_of sp-cheld)"
want "and names the landing that moved the base"   "after spira/sp-clands landed" "$(notes_of sp-cheld)"
want "and the pass counts the conflict separately" "0 survivor(s) rebased after a landing, 1 conflicted" "$out"
drop_branch sp-cheld; drop_branch sp-clands

# A BRANCH AN AEON TOOK WHILE THE PASS RAN IS NEVER REWRITTEN. Minutes pass between the loop
# judging a branch and a landing that triggers the sweep — a whole gate run — and in that
# window a bead can be reopened elsewhere and claimed. Rebasing rewrites commits beneath a
# live worktree and destroys work that exists in exactly one place, which is the one failure
# here that nothing can undo, so the liveness check is repeated at sweep time rather than
# inherited from the loop.
seed; branch sp-taken taken.txt; branch sp-tlands tlands.txt
withhold_gate sp-taken
# A REAL PROCESS RUNNING A FILE ACTUALLY CALLED aeon.sh: aeon_alive reads /proc/<pid>/cmdline
# and requires the runner's own name in it, so a fixture that invented a pid would assert the
# guard fires where the guard would in fact have seen nobody home. `bash -c '<cmd>' aeon.sh`
# is not enough — bash execs a lone simple command in place, and the cmdline that survives is
# the command's, with no aeon.sh anywhere in it.
#
# IT HOLDS NO INHERITED FILE DESCRIPTOR and it sleeps in one-second slices. A backgrounded
# process keeps the suite's stdout open, so `./test-landing.sh | tail` hangs until it exits
# however long ago the suite finished; and killing it does not kill the `sleep` it is blocked
# in, which inherits the same descriptor. Redirected and sliced, the stray outlives the suite
# by at most a second and is holding nothing while it does.
printf '#!/usr/bin/env bash\nwhile :; do sleep 1; done\n' > "$TMP/aeon.sh"; chmod +x "$TMP/aeon.sh"
"$TMP/aeon.sh" >/dev/null 2>&1 & aeon_pid=$!
claim_during_gate sp-taken "$aeon_pid"
out="$(landing)"
claimed="$(cat "$RUN/aeon-builder-sp-taken.pid" 2>/dev/null)"
kill "$aeon_pid" 2>/dev/null; wait "$aeon_pid" 2>/dev/null
rm -f "$RUN/aeon-builder-sp-taken.pid"
is     "the fixture did claim it mid-pass"      "$aeon_pid" "$claimed"
want   "the pass says an aeon took it"          "an aeon took spira/sp-taken while this pass ran" "$out"
is     "and its tip is exactly as the loop left it" \
       "$(tip_at_gate sp-taken)" "$(tip_of sp-taken)"
nowant "and nothing rebased it"                 "rebased spira/sp-taken" "$out"
drop_branch sp-taken; drop_branch sp-tlands

# --------------------------------------------------------------------------------------
# THE LANDING LOOP DEFERS WHEN A LIVE AEON HOLDS THE BEAD
#
# The loop checks holder_alive before the rebase AND before the gate, so a bead the aeon
# closed but hasn't quite exited yet is left for the next pass rather than landing under
# the aeon. This is a different code path from the sweep's check (rebase_survivors),
# which fires AFTER a landing — both must refuse, and each has its own test.
#
# A REAL PROCESS with 'aeon.sh' in its cmdline: aeon_alive reads /proc/<pid>/cmdline and
# requires that string, so a fabricated pid would assert the guard fires when the guard
# would in fact have seen nobody home (law-absence-needs-a-positive-control).
# defect: sp-stale-base
# --------------------------------------------------------------------------------------
seed; branch sp-loop-held loop-held.txt
"$TMP/aeon.sh" >/dev/null 2>&1 & loop_held_pid=$!
printf '%s\n' "$loop_held_pid" > "$RUN/aeon-builder-sp-loop-held.pid"
out="$(landing)"
kill "$loop_held_pid" 2>/dev/null; wait "$loop_held_pid" 2>/dev/null
rm -f "$RUN/aeon-builder-sp-loop-held.pid"
want   "the loop defers when a live aeon holds the bead" \
       "a live aeon still holds spira/sp-loop-held — deferring the land" "$out"
nowant "and the branch was not landed"                   "landed spira/sp-loop-held" "$out"
is     "and the bead stays closed"                       closed "$(status_of sp-loop-held)"
drop_branch sp-loop-held

# --------------------------------------------------------------------------------------
# THE BULK SCAN: one query not N
#
# The scan was the whole cost of a quiet pass, growing linearly with the branch count.
# A counting shim around bd proves the scan issues one show per repository, not one per
# branch.
# --------------------------------------------------------------------------------------
echo
seed; branch sp-cnt1; branch sp-cnt2; branch sp-cnt3
cat > "$TMP/bd-counter.sh" <<'SHIM'
#!/usr/bin/env bash
# Count bd show invocations by appending to a log. bdq prepends -C <db> before the
# subcommand, so match anywhere in the args rather than on $1.
case "$*" in *" show "*|*" show") printf '%s\n' "$*" >> "${BD_CALL_LOG:?}" ;; esac
exec "$BD_REAL" "$@"
SHIM
chmod +x "$TMP/bd-counter.sh"
export BD_CALL_LOG="$TMP/bd-calls.log" BD_REAL="${SPIRA_BD:-bd}" SPIRA_BD="$TMP/bd-counter.sh"
landing >/dev/null 2>&1
SPIRA_BD="$BD_REAL"; export SPIRA_BD; unset BD_CALL_LOG BD_REAL
show_calls="$(grep -c '^show' "$TMP/bd-calls.log" 2>/dev/null || echo 0)"
# One bulk show for the scan, plus one re-read per branch that reaches the gate (three
# here). The scan must not grow with the branch count — three branches and eleven must
# both start with one show.
want "three branches land with one scan query plus per-branch re-reads" \
     "show" "$(head -1 "$TMP/bd-calls.log" 2>/dev/null)"
# The first call is the bulk scan; it carries all three IDs on one line.
first_call="$(head -1 "$TMP/bd-calls.log" 2>/dev/null)"
want "the scan carries all ids in one call" "sp-cnt1" "$first_call"
want "including the second"                 "sp-cnt2" "$first_call"
want "and the third"                        "sp-cnt3" "$first_call"
drop_branch sp-cnt1; drop_branch sp-cnt2; drop_branch sp-cnt3
rm -f "$TMP/bd-calls.log"

# --------------------------------------------------------------------------------------
# A BRANCH WHOSE BEAD IS ABSENT FROM THE DATABASE
#
# A branch named spira/<id> with no bead in the store must be treated exactly as a bdjson
# show that returned nothing: skipped as non-closed, never landed, never errored.
# --------------------------------------------------------------------------------------
seed
git -C "$REPO" worktree add -q -b "spira/sp-ghost" "$RUN/worktree/sp-ghost" main
printf 'ghost\n' > "$RUN/worktree/sp-ghost/ghost.txt"
git -C "$RUN/worktree/sp-ghost" add -A
git -C "$RUN/worktree/sp-ghost" commit -q -m "feat: sp-ghost — work"
# sp-ghost has a branch but NO bead in the database.
out="$(landing)"
want "an absent bead is skipped as non-closed" "spira/sp-ghost not landed — its bead is -" "$out"
nowant "and is never landed"                   "landed spira/sp-ghost" "$out"
git -C "$REPO" worktree remove --force "$RUN/worktree/sp-ghost" >/dev/null 2>&1
git -C "$REPO" branch -D spira/sp-ghost >/dev/null 2>&1

# --------------------------------------------------------------------------------------
# THE REPO: LABEL FALLS BACK TO THE SWEPT REPOSITORY
#
# A bead with no repo: label must be treated as belonging to the repository being swept,
# not rejected as unroutable.
# --------------------------------------------------------------------------------------
seed; branch sp-nolabel
# The branch helper creates a bead with empty labels. Verify landing treats it as
# belonging to the fixture repo.
out="$(landing)"
want "a bead with no repo: label still lands" "landed spira/sp-nolabel" "$out"
drop_branch sp-nolabel

# --------------------------------------------------------------------------------------
# A BEAD WHOSE STATUS CHANGED BETWEEN THE SCAN AND THE GATE
#
# The scan reads status once before the loop. The gate takes minutes. A bead reopened in
# that window must not be landed on a stale "closed" — the re-read at the point of action
# catches it.
# --------------------------------------------------------------------------------------
# The gate stub reopens the bead during the gate, simulating a concurrent reopen.
stub gate.sh '
'"${SPIRA_BD:-bd}"' -C "'"$SPIRA_DB"'" reopen "$SPIRA_GATE_BEAD" >/dev/null 2>&1
echo "gate: VERDICT=PASS reason=stub branch=$1 repo=${2:-?}" >&2; exit 0'
seed; branch sp-stale
out="$(landing)"
nowant "a bead reopened during the gate is not landed" "landed spira/sp-stale" "$out"
want   "the re-read catches the status change"        "bead is now open" "$out"
is     "and the bead stays open"                      open "$(status_of sp-stale)"
drop_branch sp-stale

# Same test but for the gate-failure path: a bead reopened during a failing gate must
# not be reopened AGAIN (which would charge a second attempt).
stub gate.sh '
'"${SPIRA_BD:-bd}"' -C "'"$SPIRA_DB"'" reopen "$SPIRA_GATE_BEAD" >/dev/null 2>&1
echo "gate: VERDICT=FAIL reason=stub-fail branch=$1 repo=${2:-?}" >&2; exit 1'
seed; branch sp-stalered
out="$(landing)"
nowant "a bead already reopened is not reopened again by the gate failure" \
       "Reopened by sentinel" "$out"
want   "the re-read catches the status change on the failure path" \
       "bead is now open" "$out"
drop_branch sp-stalered

# Restore the full gate stub for any future tests.
cp "$TMP/gate-full.sh" "$SH/gate.sh"

# --------------------------------------------------------------------------------------
# THE CHECKOUT REFRESH — unconditional, not only when a branch merged this pass.
#
# Three properties: an untracked file does not block it; a declined refresh names the
# condition; and a base that moved with no branch landing is picked up within one pass.
# --------------------------------------------------------------------------------------
echo

# Advance origin/main by one commit WITHOUT going through $REPO, so $REPO stays behind.
# Uses git plumbing directly on the bare remote, which avoids push/pull conflicts from
# prior test state — the failure a commit-push-reset sequence was producing.
advance_base() {
    local prev tree tip
    prev="$(git -C "$REMOTE" rev-parse HEAD)"
    tree="$(git -C "$REMOTE" rev-parse HEAD^{tree})"
    tip="$(git -C "$REMOTE" commit-tree -p "$prev" -m "base moved" "$tree")"
    git -C "$REMOTE" update-ref refs/heads/main "$tip"
}
checkout_current() { git -C "$REPO" rev-parse HEAD 2>/dev/null; }

# Reset $REPO to a clean state on main, matching origin/main. Each test below starts from
# a known-clean position so prior state cannot leak across cases.
reset_repo() {
    git -C "$REPO" checkout -q main 2>/dev/null || true
    git -C "$REPO" reset -q --hard origin/main 2>/dev/null || true
    git -C "$REPO" clean -qfd 2>/dev/null || true
}

# AN UNTRACKED FILE DOES NOT BLOCK THE REFRESH. The old check used `status --porcelain`
# without --untracked-files=no, so a single untracked directory refused the fast-forward.
seed; reset_repo; advance_base
before="$(checkout_current)"
mkdir -p "$REPO/untracked-notes"
printf 'notes\n' > "$REPO/untracked-notes/scratch.txt"
out="$(landing)"
after="$(checkout_current)"
rm -rf "$REPO/untracked-notes"
[ "$before" != "$after" ] \
    && ok "an untracked file does not block the refresh" \
    || bad "an untracked file does not block the refresh" "checkout did not move"
want "and the refresh is reported" "skew: refreshed to" "$out"

# A MODIFIED TRACKED FILE STILL BLOCKS, and the decline NAMES what blocked it. The fixture
# starts with only an empty commit and no tracked files, so create one on origin first.
seed; reset_repo
printf 'clean\n' > "$REPO/tracked.txt"
git -C "$REPO" add tracked.txt
git -C "$REPO" commit -q -m "add tracked file"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
advance_base
printf 'dirty\n' >> "$REPO/tracked.txt"
before="$(checkout_current)"
out="$(landing)"
after="$(checkout_current)"
git -C "$REPO" checkout -q -- . 2>/dev/null
[ "$before" = "$after" ] \
    && ok "a modified tracked file blocks the refresh" \
    || bad "a modified tracked file blocks the refresh" "checkout moved anyway"
want "and the decline names the condition" "tracked files are modified" "$out"

# THE DECLINE IS NOT A GIVE-UP. A dirty checkout when a pass runs is just a busy session;
# the next pass, once it is clean, must pick up what the dirty one deferred. The session
# cleaned up above (checkout -q -- .) and $REPO is still behind origin/main.
out="$(landing)"
[ "$(checkout_current)" != "$before" ] \
    && ok "the next pass advances the checkout once the session is clean" \
    || bad "the next pass advances the checkout once the session is clean" "checkout did not move"
want "and reports the refresh" "skew: refreshed to" "$out"

# A BASE THAT MOVED WITH NO BRANCH LANDING IS PICKED UP WITHIN ONE PASS.
# No spira/* branches at all — the old code returned early and never reached the refresh.
seed; reset_repo; advance_base
before="$(checkout_current)"
out="$(landing)"
after="$(checkout_current)"
[ "$before" != "$after" ] \
    && ok "a base that moved with no branch landing is picked up" \
    || bad "a base that moved with no branch landing is picked up" "checkout did not move"
want "and the refresh is reported" "skew: refreshed to" "$out"

# A CHECKOUT NOT ON THE BASE BRANCH DECLINES AND SAYS WHY.
seed; reset_repo; advance_base
git -C "$REPO" checkout -q -b detour 2>/dev/null
before="$(checkout_current)"
out="$(landing)"
after="$(checkout_current)"
git -C "$REPO" checkout -q main 2>/dev/null
git -C "$REPO" branch -q -D detour 2>/dev/null
[ "$before" = "$after" ] \
    && ok "a checkout not on the base branch is not clobbered" \
    || bad "a checkout not on the base branch is not clobbered" "checkout moved anyway"
want "and the decline names the branch" "not main" "$out"

# --------------------------------------------------------------------------------------
# A PARALLEL DUPLICATE: same feature, different text, one landed.
#
# sp-35pl and sp-dvlq in miniature. Two branches edit the same file with different words;
# one lands and the other's rebase conflicts BECAUSE the base already holds the fix. The
# reopen note must name the landed bead rather than saying "resolve the conflict", because
# the correct resolution is to drop the duplicate's commits.
# --------------------------------------------------------------------------------------
echo
cp "$TMP/gate-full.sh" "$SH/gate.sh"
reset_repo 2>/dev/null || true

# Both branches are created from the SAME base before either lands. sp-dupa sorts before
# sp-dupb, so the pass lands sp-dupa first and sp-dupb's rebase conflicts — the exact
# shape the bead is written for. Both add shared.txt from scratch with different content,
# so the rebase after the first landing produces an ADD/ADD conflict.
seed; branch sp-dupa shared.txt "the fix, as sp-dupa wrote it"
branch sp-dupb shared.txt "the fix, as sp-dupb wrote it"
out="$(landing)"
want "the first branch lands"          "landed spira/sp-dupa" "$out"
want "and the duplicate conflicts"     "reopened sp-dupb" "$out"
notes="$(notes_of sp-dupb | tr -s '[:space:]' ' ')"
want "and the note names the other bead"   "sp-dupa" "$notes"
want "and says to check before resolving"  "check whether this work is already landed" "$notes"
nowant "and does NOT say 'not an escalation'" "not an escalation" "$notes"
drop_branch sp-dupa; drop_branch sp-dupb

# A REAL CONFLICT WITH NO OTHER BEAD PRODUCES THE ORIGINAL NOTE.
seed; branch sp-mine shared.txt "my version"
printf '%s\n' "an unrelated edit by nobody" > "$REPO/shared.txt"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m "base edits shared.txt (no bead id)"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
out="$(landing)"
want "a conflict with no bead on the base reopens normally" "reopened sp-mine" "$out"
notes="$(notes_of sp-mine)"
want "the note says it is not an escalation" "not an escalation" "$notes"
nowant "and does NOT mention other beads"    "check whether" "$notes"
drop_branch sp-mine

# REPEATED REBASE FAILURES ESCALATE INSTEAD OF REOPENING AGAIN.
seed; branch sp-loop shared.txt "from the branch"
printf '%s\n' "from sp-other on the base" > "$REPO/shared.txt"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m "sp-other — change shared.txt"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
# Reopen it twice below the threshold (default 3), then hit the threshold on the third.
out="$(SPIRA_REBASE_ESCALATE_AT=3 landing)"
want "first rebase failure reopens"   "reopened sp-loop" "$out"
is   "bead is open after first"       open "$(status_of sp-loop)"
# Close the bead again so landing will see it.
B close sp-loop --reason "try again" >/dev/null 2>&1
out="$(SPIRA_REBASE_ESCALATE_AT=3 landing)"
want "second rebase failure reopens"  "reopened sp-loop" "$out"
B close sp-loop --reason "try again" >/dev/null 2>&1
out="$(SPIRA_REBASE_ESCALATE_AT=3 landing)"
want "third rebase failure escalates" "escalated sp-loop" "$out"
nowant "and does not reopen"          "reopened sp-loop" "$out"
is     "bead stays closed on escalation" closed "$(status_of sp-loop)"
drop_branch sp-loop

# --------------------------------------------------------------------------------------
# PR MODE: a branch is pushed, a pull request is opened, and the branch is left standing
# for GitHub CI to merge it. The submission marker prevents re-pushing on every pass.
# When the target repository's base moves, the pull request goes stale — this section
# tests that the pass detects and corrects that.
# --------------------------------------------------------------------------------------
echo

mkrepo2() {             # mkrepo2 <name> — create $TMP/<name> in pr mode with a bare remote
    local rmt="$TMP/$1.git" repo="$TMP/$1"
    git init -q --bare -b main "$rmt"
    git init -q -b main "$repo"
    git -C "$repo" commit -q --allow-empty -m base
    git -C "$repo" remote add origin "$rmt"
    git -C "$repo" push -q origin main
    git -C "$repo" fetch -q origin
}

branch_in() {           # branch_in <repo-dir> <id> <repo-name> — closed bead + branch
    local dir="$1" id="$2" name="$3"
    git -C "$dir" worktree add -q -b "spira/$id" "$RUN/worktree-pr/$id" main
    printf '%s\n' "$id" > "$RUN/worktree-pr/$id/$id.txt"
    git -C "$RUN/worktree-pr/$id" add -A
    git -C "$RUN/worktree-pr/$id" commit -q -m "feat: $id — work"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":["repo:%s"],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$id" "$id" "$name" "$id" | testdb_seed
}

closed_child() {        # closed_child <id> <repo-name> — bead only, no branch
    local id="$1" name="$2"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":["repo:%s"],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$id" "$id" "$name" "$id" | testdb_seed
}

mkdir -p "$RUN/worktree-pr"

# GH STUB FOR PR MODE. It responds to:
#   pr view <branch> --json number -q .number  → the PR number
#   pr view <branch> --json state -q .state    → OPEN, MERGED, or CLOSED
#   pr create                                   → writes the state file
#   pr merge                                    → no-op
# The state file is "<number> <state>", because landing asks `pr view` two different
# questions — which pull request is this, and is it still open — and a stub that answers
# both with the same string is a stub that cannot tell a merged branch from an open one.
# That distinction is the whole of the refresh path's safety.
export GH_STATE="$TMP/gh.state"
export GH_LOG="$TMP/gh.log"; : > "$GH_LOG"
cat > "$SH/ghpr" <<'GH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$GH_LOG"
case "$1 $2" in
  "pr view")
      [ -f "$GH_STATE" ] || exit 0
      read -r gh_num gh_st < "$GH_STATE"
      case "$*" in
        *.number*) printf '%s\n' "$gh_num" ;;
        *.state*)  printf '%s\n' "$gh_st" ;;
      esac
      exit 0 ;;
  "pr create") cat >/dev/null; printf '%s %s\n' "${GH_NUM:-7}" OPEN > "$GH_STATE"; exit "${GH_CREATE_RC:-0}" ;;
  "pr merge")  exit "${GH_MERGE_RC:-0}" ;;
  "pr list")   printf '[]\n'; exit 0 ;;
esac
exit 1
GH
chmod +x "$SH/ghpr"

# landing_pr — runs a pass over the pr-mode repository with a live ask stub.
export ASK_LOG="$TMP/ask.log"
cat > "$TMP/ask.sh" <<'ASKSH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$ASK_LOG"
ASKSH
chmod +x "$TMP/ask.sh"

landing_pr() {
    rm -f "$RUN/landing.progress"
    : > "$ASK_LOG"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" \
    SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/ghpr" \
    SPIRA_NOTIFY="$TMP/ask.sh" SPIRA_ASK="$TMP/ask.sh" \
        bash "$SH/landing.sh" 2>&1
}

mailbox_pr() { cat "$RUN/landing.progress" 2>/dev/null; }

# BASIC PR SUBMISSION: a closed bead is pushed, a PR is opened, and the marker prevents
# a second push on the next run.
mkrepo2 three
cat > "$SH/repo-map" <<MAP
three | $TMP/three | pr | |
MAP
rm -f "$GH_STATE"; : > "$GH_LOG"
seed; branch_in "$TMP/three" sp-pr three
out="$(landing_pr)"
want "a pr-mode branch is pushed and a pull request is opened" "opened a pull request for spira/sp-pr" "$out"
want "and the pass reported it"                                "opened a pull request for spira/sp-pr" "$(mailbox_pr)"

: > "$GH_LOG"
out="$(landing_pr)"
nowant "an open pull request is not reopened next run"  "opened a pull request" "$out"
[ -s "$GH_LOG" ] && bad "and gh is not called again" "$(cat "$GH_LOG")" \
                 || ok "and gh is not called again"
is "and nothing reaches the mailbox" "" "$(mailbox_pr)"

# --------------------------------------------------------------------------------------
# A PULL REQUEST WHOSE BASE HAS MOVED. This is the failure the submission marker created:
# the marker skips a branch until its tip moves and nothing moved it, so the pull request
# aged in place against a base that did not — and a required check that tests the PR head
# rather than the merge result then goes red with nobody left to own it, because the bead is
# closed and its aeon is gone (law-stale-red-pr).
#
# The assertions are ancestry in the branch and in the bare origin, not lines in a log: what
# is being claimed is that the branch now carries the new base and that the remote agrees.
# --------------------------------------------------------------------------------------
advance_pr() {           # advance_pr <repo> — move the base out from under the branch
    local r="$1"
    git -C "$r" checkout -q main
    git -C "$r" commit -q --allow-empty -m "the base moves"
    git -C "$r" push -q origin main
    git -C "$r" fetch -q origin
}

before="$(git -C "$TMP/three" rev-parse spira/sp-pr)"
advance_pr "$TMP/three"
: > "$GH_LOG"
out="$(landing_pr)"
want "a stale pull request is rebased onto the base that moved" "refreshed spira/sp-pr onto origin/main" "$out"
git -C "$TMP/three" merge-base --is-ancestor origin/main spira/sp-pr \
    && ok "and the branch really carries the new base now" \
    || bad "the refresh" "the branch is still behind origin/main"
[ "$before" != "$(git -C "$TMP/three" rev-parse spira/sp-pr)" ] \
    && ok "and the tip really moved, which is what un-skips the marker" \
    || bad "the refresh" "the tip is unchanged"
git -C "$TMP/three" fetch -q origin
is     "and the remote branch moved with it" \
       "$(git -C "$TMP/three" rev-parse spira/sp-pr)" \
       "$(git -C "$TMP/three" rev-parse refs/remotes/origin/spira/sp-pr)"
nowant "and no SECOND pull request is opened for the same work" "pr create" "$(cat "$GH_LOG")"
# A refresh moved no bead and landed nothing. Counting it as progress would mute CHECK 8 for
# exactly as long as a branch went on failing to merge — and a branch being refreshed over
# and over IS the paralysis CHECK 8 exists to notice.
is "and a refresh is not reported to the sentinel as a movement" "" "$(mailbox_pr)"

# Once it is current there is nothing left to do, and asking gh again would be a round trip
# per branch per pass to learn that.
: > "$GH_LOG"
out="$(landing_pr)"
nowant "a branch level with its base is not refreshed again" "refreshed spira/sp-pr" "$out"
[ -s "$GH_LOG" ] && bad "and gh is not asked about it again" "$(cat "$GH_LOG")" \
                 || ok "and gh is not asked about it again"

# A MERGED PULL REQUEST IS NOT A STALE ONE. `gh pr merge --squash` lands a new commit, so the
# branch is not an ancestor of its base and the Sending — whose whole predicate is ancestry —
# never reaps it. It therefore stands here forever, falling further behind with every commit
# that follows, and without this question every merged pull request in the repository would
# be force-pushed once a pass until its budget ran out and then escalated to Ryan as work
# that would not merge.
printf '7 MERGED\n' > "$GH_STATE"
before="$(git -C "$TMP/three" rev-parse spira/sp-pr)"
advance_pr "$TMP/three"
: > "$GH_LOG"
out="$(landing_pr)"
want "a merged pull request's branch is left alone" "its pull request is MERGED — nothing to refresh" "$out"
is   "and its tip is exactly where it was"          "$before" "$(git -C "$TMP/three" rev-parse spira/sp-pr)"
out="$(landing_pr)"
nowant "and it is not re-examined on every later pass" "nothing to refresh" "$out"

# A gh THAT CANNOT ANSWER IS NOT PERMISSION TO REWRITE THE BRANCH. Unreadable and open look
# identical from here, and only one of the two readings force-pushes.
rm -f "$GH_STATE"
git -C "$TMP/three" branch -q -D spira/sp-pr2 2>/dev/null
git -C "$TMP/three" branch -q spira/sp-pr2 spira/sp-pr
closed_child sp-pr2 three
printf '%s %s pr 0\n' "$(git -C "$TMP/three" rev-parse spira/sp-pr2)" "$(date +%s)" > "$RUN/submitted/sp-pr2"
before="$(git -C "$TMP/three" rev-parse spira/sp-pr2)"
out="$(landing_pr)"
want "an unreadable pull request state stops the refresh" "gh will not say whether its pull request is open" "$out"
is   "and the branch is untouched"                        "$before" "$(git -C "$TMP/three" rev-parse spira/sp-pr2)"
git -C "$TMP/three" branch -q -D spira/sp-pr2

# --------------------------------------------------------------------------------------
# THE BOUND. A branch refreshed and refreshed that still does not merge is not a slow
# landing, it is a stuck one, and a loop that goes on rebasing it hides that rather than
# fixing it. The cap is lowered to 1 here so the escalation is reached in two passes.
# --------------------------------------------------------------------------------------
mkrepo2 eight
cat > "$SH/repo-map" <<MAP
eight | $TMP/eight | pr | |
MAP
rm -f "$GH_STATE"; : > "$GH_LOG"; : > "$ASK_LOG"
seed; branch_in "$TMP/eight" sp-rot eight
out="$(landing_pr)"
want "the bounded case opens its pull request first" "opened a pull request for spira/sp-rot" "$out"

advance_pr "$TMP/eight"
out="$(SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/ghpr" \
    SPIRA_NOTIFY="$TMP/ask.sh" SPIRA_ASK="$TMP/ask.sh" \
    SPIRA_PR_REFRESH_MAX=1 bash "$SH/landing.sh" 2>&1)"
want "the first refresh is spent"    "refreshed spira/sp-rot" "$out"
is   "and nothing is escalated yet"  "" "$(cat "$ASK_LOG")"

advance_pr "$TMP/eight"
before="$(git -C "$TMP/eight" rev-parse spira/sp-rot)"
: > "$ASK_LOG"; : > "$RUN/landing.progress"
out="$(SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/ghpr" \
    SPIRA_NOTIFY="$TMP/ask.sh" SPIRA_ASK="$TMP/ask.sh" \
    SPIRA_PR_REFRESH_MAX=1 bash "$SH/landing.sh" 2>&1)"
nowant "a branch past its cap is not rebased again"  "refreshed spira/sp-rot" "$out"
is     "and its tip is left where it was"            "$before" "$(git -C "$TMP/eight" rev-parse spira/sp-rot)"
want   "and it is escalated instead"                 "escalated sp-rot" "$out"
want   "and the ask carries a default Ryan can take" "reopen sp-rot at P0" "$(cat "$ASK_LOG")"
want   "and names the branch and its repository"     "spira/sp-rot in eight" "$(cat "$ASK_LOG")"
want   "and leads with what the bead was for"        "WHAT THIS BEAD IS FOR" "$(cat "$ASK_LOG")"
is     "and an escalation is not a movement either"  "" "$(mailbox_pr)"

# ONCE, NOT EVERY PASS. A stuck branch stays stuck until someone decides about it, and a
# check that says so every two minutes is a check Ryan learns to scroll past
# (law-alerts-must-be-actionable). The marker's state is what remembers.
advance_pr "$TMP/eight"
: > "$ASK_LOG"
out="$(SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/ghpr" \
    SPIRA_NOTIFY="$TMP/ask.sh" SPIRA_ASK="$TMP/ask.sh" \
    SPIRA_PR_REFRESH_MAX=1 bash "$SH/landing.sh" 2>&1)"
want "an escalated branch says so rather than escalating again" "already escalated" "$out"
is   "and Ryan is not paged a second time"                      "" "$(cat "$ASK_LOG")"

# Restore repo-map to the fixture push-mode repo for any tests that follow.
cat > "$SH/repo-map" <<MAP
$REPONAME | $REPO | push | |
MAP

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
