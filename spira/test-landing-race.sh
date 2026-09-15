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
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

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
cp "$HERE/landing.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$SH/"
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

# --------------------------------------------------------------------------------------
# THE POSITIVE CONTROL, first, because every silence below is read against it. An
# uncontested land must reach the bare origin AS THE BRANCH — the assertion is ancestry in
# the remote, not a line in a log (law-absence-needs-a-positive-control).
# --------------------------------------------------------------------------------------
seed; branch sp-plain; out="$(landing)"
want "an uncontested land is reported" "landed spira/sp-plain" "$out"
git -C "$REPO" fetch -q origin
git -C "$REPO" merge-base --is-ancestor "spira/sp-plain" origin/main \
    && ok  "and what landed is the branch itself" \
    || bad "the uncontested land" "spira/sp-plain is not an ancestor of origin/main"
drop_branch sp-plain

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

# --------------------------------------------------------------------------------------
# AND NOTHING IN THE HARNESS REBASES THE LANDING WORKTREE. The defect was one token — `git
# -C "$land" rebase` where it had to be the branch — and the case above catches it only by
# way of a hook, a race and an ancestry check three assertions apart. This catches it by
# reading, for the reason a one-token edit deserves a one-line fence: it is trivial to
# reintroduce and it fails silently for hours.
#
# The landing worktree is scratch, rebuilt from the base on every attempt, so it has no
# history worth replaying; rebasing it produces COPIES of the branch's commits, and copies
# land work under SHAs the branch ref does not point at. Comments are stripped rather than
# excluded, because this file and landing.sh both explain the rule at length.
# --------------------------------------------------------------------------------------
offends() {              # offends <dir> -> "<file>: <hit>" lines, comments stripped
    local d="$1" f hit out=""
    for f in "$d"/*.sh; do
        [ -e "$f" ] || continue
        case "$(basename "$f")" in test-*) continue ;; esac
        # The `cd` arm reaches PAST the connector on purpose. Written `[^;&|]*` — which is
        # how the first draft of this fence had it — the alternation stops dead at the `&&`
        # in `cd "$land" && git rebase`, so half the fence matched nothing and read as a
        # clean tree. The positive control below is what caught that.
        hit="$(sed 's/#.*//' "$f" | grep -nE 'git +-C +"\$land"[^;&|]*rebase|cd +"\$land".*git +rebase' || true)"
        [ -n "$hit" ] && out="$out$(basename "$f"): $hit
"
    done
    printf '%s' "$out"
}
is "no harness program rebases the landing worktree" "" "$(offends "$HERE")"

# THE FENCE'S OWN POSITIVE CONTROL. A grep that reports a clean tree looks identical whether
# its matcher fired or never could — and the whole value of this fence is its silence, so the
# silence has to be earned. Plant one offender of each shape and require both to be named.
PLANT="$TMP/plant"; mkdir -p "$PLANT"
printf '#!/usr/bin/env bash\ngit -C "$land" rebase -q "$base"\n'     > "$PLANT/one.sh"
printf '#!/usr/bin/env bash\ncd "$land" && git rebase -q "$base"\n'  > "$PLANT/two.sh"
planted="$(offends "$PLANT")"
want 'the fence names a git -C $land rebase'  "one.sh" "$planted"
want 'and a cd $land followed by git rebase'  "two.sh" "$planted"

# --------------------------------------------------------------------------------------
# NO FETCH IN THE LANDING PATH WRITES FETCH_HEAD. Concurrent landing passes share the
# same .git object store, so concurrent git-fetch calls race to write .git/FETCH_HEAD.
# Under FETCH_HEAD lock contention a fetch can fail silently (2>/dev/null swallows it),
# leaving local remote-tracking refs stale. A stale origin/main makes content_landed
# return false when the content IS already there; the landing worktree's checkout then
# picks up the current (newer) ref, the merge is a no-op, the push says "Everything
# up-to-date" and exits 0 — merged=1 and pushed=1 both land, and "landed" fires with no
# new commit on the base. --no-write-fetch-head removes the FETCH_HEAD write entirely,
# eliminating the lock; the no-op merge guard above closes the remaining window.
# --------------------------------------------------------------------------------------
bare_fetch_in() {        # bare_fetch_in <dir> -> "<file>: <hit>" lines, comments stripped
    local d="$1" f hit out=""
    for f in "$d/landing.sh" "$d/skew.sh"; do
        [ -e "$f" ] || continue
        # Strip comments before grepping so a commented-out example does not trigger.
        hit="$(sed 's/#.*//' "$f" | grep -nE '\bgit\b.*\bfetch\b' | grep -vE -- '--no-write-fetch-head' || true)"
        [ -n "$hit" ] && out="$out$(basename "$f"): $hit
"
    done
    printf '%s' "$out"
}
is "every fetch in the landing path uses --no-write-fetch-head" "" "$(bare_fetch_in "$HERE")"

# THE FENCE'S OWN POSITIVE CONTROL. A grep that reports a clean result looks identical
# whether the pattern matched nothing or it could not have matched — the silence has to
# be earned. Plant one offender of each shape and require both to be named.
PLANT2="$TMP/plant2"; mkdir -p "$PLANT2"
printf '#!/usr/bin/env bash\ngit -C "$repo" fetch -q "$remote" 2>/dev/null\n' \
    > "$PLANT2/landing.sh"
printf '#!/usr/bin/env bash\ngit -C "$repo" fetch -q "$remote" 2>/dev/null\n' \
    > "$PLANT2/skew.sh"
planted2="$(bare_fetch_in "$PLANT2")"
want 'the fence catches a bare fetch in landing.sh' "landing.sh" "$planted2"
want 'and a bare fetch in skew.sh'                  "skew.sh"    "$planted2"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
