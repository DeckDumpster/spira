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
# covers: spira/landing.sh spira/lib.sh
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
testdb_up landing || {
    printf 'SKIP test-landing: testdb not available\n' >&2
    exit 77
}
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
# THE OUTCOME STREAM IS RECORDED, NOT MERELY SWALLOWED, and it is stubbed EXPLICITLY. Without
# a stub, mail.sh would write to $SPIRA_MAIL on the host — a suite writing into the operator's
# live mailbox is not a risk worth leaving to an unset variable
# (law-gates-run-in-a-clean-environment).
stub mail.sh '[ "${1:-}" = send ] || exit 0; printf "%s\n" "$*" >> "$EMITTED"; cat >> "$EMITTED"; printf "\n" >> "$EMITTED"'
export EMITTED="$TMP/events"; : > "$EMITTED"
events() { cat "$RUN/events.log" 2>/dev/null; }

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
    # SPIRA_ID_PREFIX IS PINNED. A session running on a non-default-prefix harness hands
    # other_beads_on_conflicts a pattern that does not match the sp- ids used in this
    # fixture's commits, so the function returns empty and the parallel-duplicate note
    # omits the bead name it is written to carry (law-gates-run-in-a-clean-environment).
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" SPIRA_ID_PREFIX=sp \
    SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/gh" \
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
: > "$EMITTED"; : > "$RUN/events.log"
seed; branch sp-plain; out="$(landing)"
want "an uncontested land is reported" "landed spira/sp-plain" "$out"
# Landing pushes the base from the .landing worktree; the home checkout must be advanced in
# the same pass or every hand-run script reads a past state (sp-wud).
want "the same pass also advances the checkout humans read" "skew: refreshed to" "$out"
# AND IT SURVIVES THE PASS. The mailbox above is drained by the sentinel and the log line
# scrolls off the health pane below the fourth row; the event is the only record still
# answerable tomorrow. It is emitted AFTER the push, so what it asserts is the ancestry just
# proved rather than the close that preceded it (law-closed-is-not-landed).
want "the land is recorded as an event"  "kind: bead.landed"   "$(events)"
want "against the bead that landed"      "target: sp-plain"    "$(events)"
drop_branch sp-plain

# A GATE FAILURE ALSO RECORDS AN EVENT, and the negative is the landing above: a gate that
# passed produced bead.landed, not bead.reopened, so the check is not just reading the stub.
: > "$EMITTED"; : > "$RUN/events.log"
stub gate.sh 'echo "gate: VERDICT=FAIL reason=stub-fail branch=$1 repo=${2:-?}" >&2; exit 1'
seed; branch sp-gfail; out="$(landing)"
want "a failed gate reopens the bead"          "reopened sp-gfail — failed the gate" "$out"
want "and the reopen is recorded as an event"  "kind: bead.reopened" "$(events)"
want "against the bead that was reopened"      "target: sp-gfail"    "$(events)"
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
# A PARALLEL DUPLICATE: same feature, different text, one landed.
#
# sp-35pl and sp-dvlq in miniature. Two branches edit the same file with different words;
# one lands and the other's rebase conflicts BECAUSE the base already holds the fix. The
# reopen note must name the landed bead rather than saying "resolve the conflict", because
# the correct resolution is to drop the duplicate's commits.
# --------------------------------------------------------------------------------------
echo
cp "$TMP/gate-full.sh" "$SH/gate.sh"

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
# THE TRIGGER IS THE REASON CLASS, not the requeue counter. The first RED reopens once;
# on the second RED with the same reason class ("no-rebase") the pass escalates regardless
# of how many times the tip or base sha changed between marks.
seed; branch sp-loop shared.txt "from the branch"
printf '%s\n' "from sp-other on the base" > "$REPO/shared.txt"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m "sp-other — change shared.txt"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
out="$(landing)"
want "first rebase failure reopens"       "reopened sp-loop" "$out"
is   "bead is open after first"           open "$(status_of sp-loop)"
B close sp-loop --reason "try again" >/dev/null 2>&1
git -C "$RUN/worktree/sp-loop" commit -q --allow-empty -m "sp-loop aeon attempt 1"
out="$(landing)"
want "second rebase failure escalates"    "escalated sp-loop" "$out"
nowant "and does not reopen on second"    "reopened sp-loop"  "$out"
is     "bead stays closed on escalation"  closed "$(status_of sp-loop)"
drop_branch sp-loop

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
