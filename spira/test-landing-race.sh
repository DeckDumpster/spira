#!/usr/bin/env bash
#
# test-landing-race.sh — what the landing pass does when it LOSES the push race.
#
#   ./test-landing-race.sh
#
# The pass has already decided the work is good and then trips over its own machinery,
# ending with the pass saying something untrue about a bead: landing the same branch
# twice in consecutive passes.
#
# A sibling case this file used to also cover — reopening finished work as "conflicts
# with the base" when a missing landing worktree, not a conflict, was the real cause —
# ran with no repo-map row and is deleted (rule 5, per Ryan 2026-10-05 one source of
# config); see below.
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
# tier: T2
# covers: landing-pass/* spira/lib.sh UC-landing-merge-queue-14
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
# THE LANDING PASS IS THE landing-pass BINARY (landing-pass/DESIGN.md §7.4): `land` for a
# pass, `halt` to stop one. Resolved from this tree before any fixture repoints SPIRA_REPO.

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
timeout 5 git -C "$REPO" push -q origin main
timeout 5 git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH"
lc_path_stub "$SH" "$TMP/lcfix"

# conf.sh travels with lib.sh. lib.sh resolves every path through it and refuses to run
# without it, so a fixture harness that copies one and not the other fails at source time —
# every case reporting exit 127 and no landing, which reads as landing being broken rather
# than the fixture being incomplete.
cp "$HERE/lib.sh" "$HERE/conf.sh" "$SH/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
# The gate stub speaks the gate's PROTOCOL, not just its exit status: landing.sh reads the
# machine-readable VERDICT line for the reason it records, so a stub that only exited would
# leave every reason reading "unspecified" and half the contract untested.
stub gate.sh 'echo "gate: VERDICT=PASS reason=stub branch=$1 repo=${2:-?}" >&2; exit 0'
stub confine.sh 'exit 0'
# skew travels with landing-pass: it resolves skew by bare name on PATH and calls "refresh" at the end
# of every pass for push-mode repos. A fixture that does not provide it emits a "No such file"
# error into every landing's output — the call is || true so tests still pass, but the error
# contaminates $out and would break any future assertion that checks for a clean output.
stub skew 'exit 0'

B() { timeout 5 bd -C "$SPIRA_DB" "$@"; }
status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }

# The mailbox is drained by the sentinel in production, so each run here starts from empty —
# otherwise every assertion after the first would be reading an earlier run's lines.
tl_config SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO_MAP="$SH/repo-map"
landing() {
    rm -f "$RUN/landing.progress"
    # SPIRA_REPO_MAP EXPLICITLY, never left to the fallback chain, and nothing else inherited.
    # A pass that falls back reads whatever repositories the operator has registered and
    # counts THEIR branches, so a suite asserting about one fixture branch is quietly
    # asserting about a box (law-gates-run-in-a-clean-environment).
    SPIRA_HOME="$SH" SPIRA_REPO="$REPO" \
        PATH="$SH:$PATH" landing-pass land 2>&1
}
mailbox() { cat "$RUN/landing.progress" 2>/dev/null; }

seed() {
    testdb_reset
    rm -rf "$LC_FIX/bead" "$LC_FIX/show"; mkdir -p "$LC_FIX/bead" "$LC_FIX/show"
    testdb_seed <<'JSONL'
{"id":"sp-epic","title":"epic","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
}
branch() {               # branch <id> — a closed bead with a committed branch of its own
    local id="$1"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    echo "$id" > "$RUN/worktree/$id/$id.txt"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id — work"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-epic","type":"parent-child"}]}\n' \
        "$id" "$id" "$id" | testdb_seed
    # The builder's hand-off is the bead's lifecycle row, not bd `closed` (sp-mve9i).
    lc_bead SUBMITTED "$id" "$(git -C "$RUN/worktree/$id" rev-parse HEAD 2>/dev/null)" 0
}
drop_branch() {          # drop_branch <id> — leave the world as clean as we found it
    local id="$1"
    git -C "$REPO" worktree remove --force "$RUN/worktree/$id" >/dev/null 2>&1
    git -C "$REPO" branch -D "spira/$id" >/dev/null 2>&1
}

echo "test-landing-race.sh"

# The uncontested-land positive control lives in test-landing.sh (duplicate cluster #10,
# plan section 4): this file keeps only race cases, which is every check below.

# THE RACE, THROUGH AN EXPLICIT REPO-MAP ROW (gap G12). Deleted (rule 5, per Ryan
# 2026-10-05 one source of config): the no-map variants of every case below, which
# resolved the home repo through the bare SPIRA_REPO override with no declared base — a
# repo now lands only on its declared base, so "push mode needs no repo-map row" tested a
# fallback that no longer exists. This is the one surviving shape: a real row, matched by
# name, land/base columns read from it.
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

# A PUSH REJECTED WITHOUT THE BASE MOVING (not a race) and A MISSING LANDING WORKTREE
# (not a conflict) used to run here too, both also with no repo-map row — deleted for the
# same reason as the race case above (rule 5, one source of config: a repo lands only on
# its declared base).

# The queue-mode skew-refresh case (duplicate cluster #10, plan section 4) lives in
# test-skew-refresh.sh now, with its own minimal landing-harness fixture — this file
# keeps only race cases.
tl_summary
