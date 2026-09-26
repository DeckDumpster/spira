#!/usr/bin/env bash
#
# test-landing-race.sh — what the landing pass does when it LOSES the push race, and what it
# does when the tree it lands through is not there.
#
#   ./test-landing-race.sh
#
# Both are cases where the pass has already decided the work is good and then trips over its
# own machinery, and both used to end with the pass saying something untrue about a bead:
# once by landing the same branch twice in consecutive passes, once by reopening finished
# work as "conflicts with the base" when nothing had conflicted with anything.
#
#   01:56:04 landing: push rejected, origin/main moved — retry 1
#   01:56:04 ACT landed spira/<id>
#   KEEP   <id>  unlanded — 2 commit(s) not in origin/main      <- the same pass
#   01:56:27 ACT landed spira/<id>
#
# The retry recovered by rebasing the LANDING branch onto the moved base, which replays the
# branch's commits as new objects and leaves the branch ref on the originals. So the work
# landed and the branch was an ancestor of nothing, the reap kept the ref, and the next pass
# merged it again — a no-op merge whose `git push` answers "Everything up-to-date" and exits
# 0, so it reads as a movement and inflates the action count the judgement tier reads.
#
# The database is a REAL bd on a fixture created for the run and dropped by a trap, and git
# is real too, with a real bare remote, because every claim here is a claim about ancestry or
# about what `git merge` and `git push` do when there is nothing left to do. `gate.sh` and
# `confine.sh` are stubs whose exit status this suite dictates: each has its own suite, and
# what is under test here is what landing does AFTER a verdict, not how one is reached.
#
# defect: sp-dupland
# covers: spira/landing.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-landing-race
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up landingrace || { echo "test-landing-race: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH"

# conf.sh travels with lib.sh. lib.sh resolves every path through it and refuses to run
# without it, so a fixture harness that copies one and not the other fails at source time —
# every case reporting exit 127 and no landing, which reads as landing being broken rather
# than the fixture being incomplete.
cp "$HERE/landing.sh" "$HERE/landing-lib.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
# The gate stub speaks the gate's PROTOCOL, not just its exit status: landing.sh reads the
# machine-readable VERDICT line for the reason it records, so a stub that only exited would
# leave every reason reading "unspecified" and half the contract untested.
stub gate.sh 'echo "gate: VERDICT=PASS reason=stub branch=$1 repo=${2:-?}" >&2; exit 0'
stub confine.sh 'exit 0'
# skew.sh travels with landing.sh: landing.sh calls "$SPIRA_HOME/skew.sh refresh" at the end
# of every pass for push-mode repos. A fixture that does not provide it emits a "No such file"
# error into every landing's output — the call is || true so tests still pass, but the error
# contaminates $out and would break any future assertion that checks for a clean output.
stub skew.sh 'exit 0'

B() { bd -C "$SPIRA_DB" "$@"; }
status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }

# The mailbox is drained by the sentinel in production, so each run here starts from empty —
# otherwise every assertion after the first would be reading an earlier run's lines.
landing() {
    rm -f "$RUN/landing.progress"
    # SPIRA_REPO_MAP EXPLICITLY, never left to the fallback chain, and nothing else inherited.
    # A pass that falls back reads whatever repositories the operator has registered and
    # counts THEIR branches, so a suite asserting about one fixture branch is quietly
    # asserting about a box (law-gates-run-in-a-clean-environment).
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO="$REPO" \
    SPIRA_REPO_MAP="$SH/repo-map" \
        bash "$SH/landing.sh" 2>&1
}
mailbox() { cat "$RUN/landing.progress" 2>/dev/null; }

seed() {
    testdb_reset
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
}
branch() {               # branch <id> — a closed bead with a committed branch of its own
    local id="$1"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    echo "$id" > "$RUN/worktree/$id/$id.txt"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id — work"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$id" "$id" "$id" | testdb_seed
}
drop_branch() {          # drop_branch <id> — leave the world as clean as we found it
    local id="$1"
    git -C "$REPO" worktree remove --force "$RUN/worktree/$id" >/dev/null 2>&1
    git -C "$REPO" branch -D "spira/$id" >/dev/null 2>&1
}

echo "test-landing-race.sh"

# The uncontested-land positive control lives in test-landing.sh (duplicate cluster #10,
# plan section 4): this file keeps only race cases, which is every check below.

# --------------------------------------------------------------------------------------
# THE RACE. The base moves between our fetch and our push — a statute synthesis, a mirror
# export, another repository's cron — the push is rejected, and the landing is rebuilt. What
# lands must still be the branch's OWN commits.
#
# Both assertions matter and they fail in opposite directions: the ancestry one catches the
# rewrite, and the second-run pair catches the false movement it feeds.
# --------------------------------------------------------------------------------------
cat > "$REMOTE/hooks/pre-receive" <<'HOOK'
#!/usr/bin/env bash
# Reject the FIRST push only, and advance the base behind the pusher's back as it goes: a
# rejection without the competing commit leaves nothing for the retry to rebase onto, and
# the rebase is the whole subject of the case.
#
# THE COMPETING COMMIT IS WRITTEN OUTSIDE THE QUARANTINE. A pre-receive hook runs with
# GIT_QUARANTINE_PATH set and git refuses ref updates inside it — "ref updates forbidden
# inside quarantine environment" — so a hook that simply calls update-ref moves nothing, the
# retry rebases onto an unchanged base, and the case passes against the bug it is written
# for. That is exactly the false-clean this suite exists to avoid.
[ -f "$GIT_DIR/rejected-once" ] && exit 0
: > "$GIT_DIR/rejected-once"
env -u GIT_QUARANTINE_PATH -u GIT_OBJECT_DIRECTORY -u GIT_ALTERNATE_OBJECT_DIRECTORIES -u GIT_DIR \
    bash -c '
      export GIT_AUTHOR_NAME=other GIT_AUTHOR_EMAIL=o@o GIT_COMMITTER_NAME=other GIT_COMMITTER_EMAIL=o@o
      old="$(git -C "$1" rev-parse "$2")"
      new="$(git -C "$1" commit-tree "$old^{tree}" -p "$old" -m "someone else moved the base")"
      git -C "$1" update-ref "refs/heads/$2" "$new" "$old"
    ' _ "$(pwd)" main >&2 || echo "the hook could not move the base" >&2
echo "rejected once, on purpose" >&2
exit 1
HOOK
chmod +x "$REMOTE/hooks/pre-receive"
seed; branch sp-race; out="$(landing)"
rm -f "$REMOTE/hooks/pre-receive" "$REMOTE/rejected-once"
want   "a rejected push is retried rather than called a conflict" "push rejected" "$out"
want   "and the branch lands on the base that moved"              "landed spira/sp-race" "$out"
nowant "and the bead is not reopened over a lost race"            "reopened sp-race" "$out"
is     "the bead is still closed"                                 closed "$(status_of sp-race)"
git -C "$REPO" fetch -q origin
git -C "$REPO" merge-base --is-ancestor "spira/sp-race" origin/main \
    && ok  "what landed is the branch itself, not copies of its commits" \
    || bad "the raced land" "spira/sp-race is not an ancestor of origin/main — its work landed under other SHAs"
out="$(landing)"
nowant "so the next pass does not land it a second time" "landed spira/sp-race" "$out"
is     "and no movement is posted for the duplicate"     ""  "$(mailbox)"
drop_branch sp-race

# --------------------------------------------------------------------------------------
# THE SAME RACE, THROUGH AN EXPLICIT REPO-MAP ROW (gap G12). Every case above runs with
# SPIRA_REPO_MAP pointed at a file that does not exist yet, so repo_names() sees nothing
# and the home repo resolves through the SPIRA_REPO override in repo_root() — the map file
# is never actually read for this repo. That leaves the explicit-map path (a real row,
# matched by name, land/base columns read from it) exercised only by the queue-mode case
# below, and never for a push-mode race. This case writes the row first, so repo_field()
# has to parse a real line rather than fail (unread) closed to the override.
# --------------------------------------------------------------------------------------
cat > "$SH/repo-map" <<MAP
$(basename "$REPO") | $REPO | push | origin/main | |
MAP
cat > "$REMOTE/hooks/pre-receive" <<'HOOK'
#!/usr/bin/env bash
[ -f "$GIT_DIR/rejected-once" ] && exit 0
: > "$GIT_DIR/rejected-once"
env -u GIT_QUARANTINE_PATH -u GIT_OBJECT_DIRECTORY -u GIT_ALTERNATE_OBJECT_DIRECTORIES -u GIT_DIR \
    bash -c '
      export GIT_AUTHOR_NAME=other GIT_AUTHOR_EMAIL=o@o GIT_COMMITTER_NAME=other GIT_COMMITTER_EMAIL=o@o
      old="$(git -C "$1" rev-parse "$2")"
      new="$(git -C "$1" commit-tree "$old^{tree}" -p "$old" -m "someone else moved the base")"
      git -C "$1" update-ref "refs/heads/$2" "$new" "$old"
    ' _ "$(pwd)" main >&2 || echo "the hook could not move the base" >&2
echo "rejected once, on purpose" >&2
exit 1
HOOK
chmod +x "$REMOTE/hooks/pre-receive"
seed; branch sp-racemap; out="$(landing)"
rm -f "$REMOTE/hooks/pre-receive" "$REMOTE/rejected-once" "$SH/repo-map"
want "with a repo-map row present, a rejected push is still retried, not called a conflict" \
     "push rejected" "$out"
want "and the branch still lands on the base that moved" "landed spira/sp-racemap" "$out"
is   "and the bead is still closed"                       closed "$(status_of sp-racemap)"
drop_branch sp-racemap

# --------------------------------------------------------------------------------------
# A PUSH THAT IS REJECTED WITHOUT THE BASE MOVING IS NOT A RACE. The retry loop was
# written for the case where a concurrent pusher advanced the base; it does not help
# when the base has not moved. Before this fix every failed push was logged as "the base
# moved" regardless of what actually happened, and the pass retried indefinitely.
#
# This case is also the one that costs a debugging session: "push rejected, origin/main
# moved" sends the reader hunting for a concurrent pusher that does not exist. The fix
# is to verify the base moved (one rev-parse after the fetch) before calling it a race.
# --------------------------------------------------------------------------------------
cat > "$REMOTE/hooks/pre-receive" <<'HOOK'
#!/usr/bin/env bash
# Reject every push without moving the base — simulates a protected branch, a failing
# pre-receive hook, or any other persistent non-race rejection. The keyword "rejected"
# appears in git's own output, so the old code would have called this a lost race.
printf 'error: push rejected by hook\n' >&2
exit 1
HOOK
chmod +x "$REMOTE/hooks/pre-receive"
seed; branch sp-stuck; out="$(landing)"
rm -f "$REMOTE/hooks/pre-receive"
nowant "a rejection where the base did not move is not called a race" "push rejected, " "$out"
want   "and is reported with the base-unchanged fact"                 "did not move"    "$out"
nowant "and the bead is not reopened"                                 "reopened sp-stuck" "$out"
is     "and the bead stays closed"                                    closed "$(status_of sp-stuck)"
drop_branch sp-stuck

# --------------------------------------------------------------------------------------
# A LANDING WORKTREE THAT IS NOT THERE IS NOT A CONFLICT. Falling through to the merge with
# no tree to merge in fails, and the failure arm reopens finished work with a reason that is
# about the branch — a lie about a bead, and one that costs it an attempt toward poison.
#
# The path is blocked with a FILE rather than by revoking write on the directory: this suite
# is run by whoever is at the keyboard and by a timer, and a mode bit stops one of those and
# not root — a case that quietly stops testing anything is worse than one that never ran.
# --------------------------------------------------------------------------------------
seed; branch sp-nowt
LANDPATH="$RUN/worktree/.landing.$(basename "$REPO")"
rm -rf "$LANDPATH"; git -C "$REPO" worktree prune 2>/dev/null
: > "$LANDPATH"
out="$(landing)"
rm -f "$LANDPATH"
nowant "a missing landing worktree does not reopen the bead" "reopened sp-nowt" "$out"
is     "and the bead stays closed"                           closed "$(status_of sp-nowt)"
nowant "and nothing claims to have landed"                   "landed spira/sp-nowt" "$out"
want   "and the pass says which tree it could not find"      "no landing worktree" "$out"
drop_branch sp-nowt

# The queue-mode skew-refresh case (duplicate cluster #10, plan section 4) lives in
# test-skew-refresh.sh now, with its own minimal landing-harness fixture — this file
# keeps only race cases.
tl_summary
