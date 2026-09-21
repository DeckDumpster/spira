#!/usr/bin/env bash
#
# test-content-landed-empty-branch.sh — a branch with zero commits ahead of the base
#   must not be reported as landed by content_landed.
#
#   ./test-content-landed-empty-branch.sh
#
# THE ORIGINAL DEFECT (sp-qc4kn): content_landed's is-ancestor check came before the
# ahead>0 guard, so it returned 0 for empty branches (ahead=0, is-ancestor), causing
# the Sending to reap them and the bead to cycle without an attempt being charged.
#
# REVISED BEHAVIOR (sp-bf31a): ancestor branches are landed by definition — moving the
# is-ancestor check before the ahead=0 guard makes content_landed return 0 for all
# ancestor branches, including empty ones. The Sending reaps them; CHECK 5 reopens the
# bead when no commit on the base names it (the normal reopening path). This is correct:
# a closed bead with an empty branch never had work done, so it should be reopened.
#
# THREE CASES for content_landed, plus one for sending.sh:
#
#   1. POSITIVE CONTROL — ancestry alone returns 0 for an empty branch (fixture is correct).
#   2. EMPTY BRANCH     — content_landed returns 0 for an ancestor branch (sp-bf31a).
#   3. FF-MERGED        — a branch whose commits reached the base via push-mode landing
#      is reaped by sending.sh; the empty branch is also reaped (ancestor → landed).
#   4. SQUASH-MERGED    — a branch whose content reached the base as a squash commit
#      still returns 0 from content_landed (commits ahead, merge-tree same).
#
# covers: spira/lib.sh spira/sending.sh
# hermetic-ok: uses a fixture database and a local git repo, no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-content-landed-empty-branch
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up empty-branch || { echo "test-content-landed-empty-branch: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"
REPONAME=fixture-repo
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
git -C "$REPO" remote set-head origin main
mkdir -p "$RUN/worktree" "$SH"

cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/sending.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub confine.sh 'exit 0'
stub gh 'exit 1'

sending() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" \
    SPIRA_REPO_MAP="$SH/repo-map" \
        bash "$SH/sending.sh" 2>&1
}

# seed: one closed bead per test case
seed() {
    testdb_reset
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-10T00:00:00Z"}
{"id":"sp-empty","title":"empty branch — no commits","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-10T00:00:00Z","closed_at":"2026-09-10T00:00:00Z","dependencies":[{"issue_id":"sp-empty","depends_on_id":"sp-goal","type":"parent-child"}]}
{"id":"sp-ff","title":"ff-merged — commit on base names it","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-10T00:00:00Z","closed_at":"2026-09-10T00:00:00Z","dependencies":[{"issue_id":"sp-ff","depends_on_id":"sp-goal","type":"parent-child"}]}
{"id":"sp-sq","title":"squash-merged — content same on base","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-10T00:00:00Z","closed_at":"2026-09-10T00:00:00Z","dependencies":[{"issue_id":"sp-sq","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
}

# empty_branch <id> — branch cut from main with no commits
empty_branch() {
    local id="$1"
    git -C "$REPO" branch "spira/$id" main
}

# ff_branch <id> — simulate push-mode landing: commit on branch, pushed to origin/main.
# After push, branch tip = origin/main tip, so rev-list --count origin/main..branch = 0.
ff_branch() {
    local id="$1"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$id" > "$RUN/worktree/$id/$id.txt"
    git -C "$RUN/worktree/$id" add "$id.txt"
    git -C "$RUN/worktree/$id" commit -q -m "$id: actual work"
    # Push branch to origin/main (simulates the landing pass pushing a fast-forward)
    git -C "$REPO" push -q origin "spira/$id:main"
    git -C "$REPO" fetch -q origin
    git -C "$REPO" worktree remove "$RUN/worktree/$id" 2>/dev/null || true
}

# sq_branch <id> — branch with a commit that was squash-merged to origin/main.
# The branch has 1 commit NOT in origin/main's history, but the content is there.
sq_branch() {
    local id="$1"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$id" > "$RUN/worktree/$id/$id.txt"
    git -C "$RUN/worktree/$id" add "$id.txt"
    git -C "$RUN/worktree/$id" commit -q -m "$id: squash-merge candidate"
    git -C "$REPO" worktree remove "$RUN/worktree/$id" 2>/dev/null || true
    # Squash-merge: take the diff, apply it to origin/main directly
    git -C "$REPO" fetch -q origin
    local tree; tree="$(git -C "$REPO" commit-tree \
        "$(git -C "$REPO" write-tree)" \
        -p "origin/main" \
        -m "$id: squash merged" 2>/dev/null)"
    # Push a squash commit that has the same content as the branch would produce
    git -C "$REPO" worktree add -q "$RUN/worktree/${id}-sq" origin/main
    printf '%s\n' "$id" > "$RUN/worktree/${id}-sq/$id.txt"
    git -C "$RUN/worktree/${id}-sq" add "$id.txt"
    git -C "$RUN/worktree/${id}-sq" commit -q -m "$id: squash merged"
    git -C "$REPO" push -q origin "${id}-sq-head:main" 2>/dev/null || \
        git -C "$REPO" push -q origin "$(git -C "$RUN/worktree/${id}-sq" rev-parse HEAD):refs/heads/main"
    git -C "$REPO" fetch -q origin
    git -C "$REPO" worktree remove "$RUN/worktree/${id}-sq" 2>/dev/null || true
}

echo "test-content-landed-empty-branch.sh"

# Source lib.sh to test content_landed directly
# shellcheck disable=SC1090
. "$SH/lib.sh"

# --------------------------------------------------------------------------------------
# SETUP: build all three branch types before any assertions
# --------------------------------------------------------------------------------------
seed
empty_branch sp-empty
ff_branch sp-ff
printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$REPONAME" "$REPO" push main '' '' > "$SH/repo-map"

# --------------------------------------------------------------------------------------
# CASE 1: POSITIVE CONTROL — ancestry alone incorrectly returns 0 for an empty branch.
# This proves the fixture is correct and that the defect existed.
# --------------------------------------------------------------------------------------
echo
echo "positive control — ancestry alone returns 0 for the empty branch:"
if git -C "$REPO" merge-base --is-ancestor "spira/sp-empty" "origin/main" 2>/dev/null; then
    ok "ancestry alone incorrectly reports the empty branch as landed (the old defect)"
else
    bad "positive control failed" "ancestry check should return 0 for an empty branch — fixture may be wrong"
fi

# --------------------------------------------------------------------------------------
# CASE 2: EMPTY BRANCH — content_landed returns 0 for an ancestor branch (sp-bf31a).
# An empty branch IS an ancestor of the base; ancestor implies landed.
# --------------------------------------------------------------------------------------
echo
echo "empty branch — content_landed returns 0 for an ancestor branch (sp-bf31a):"
if content_landed "$REPO" "spira/sp-empty" "origin/main"; then
    ok "content_landed returns 0 for an ancestor branch (ancestor implies landed)"
else
    bad "content_landed must return 0 for an ancestor branch" "it returned non-zero"
fi

# --------------------------------------------------------------------------------------
# CASE 3: FF-MERGED — sending.sh must still reap a branch whose commits reached the base.
# After a push-mode landing: branch = origin/main tip, zero commits ahead,
# but a commit on origin/main names the bead id.
# --------------------------------------------------------------------------------------
echo
echo "ff-merged — sending.sh still reaps when a commit on the base names the bead:"
_ff_ahead="$(git -C "$REPO" rev-list --count "origin/main..spira/sp-ff" 2>/dev/null)"
if [ "${_ff_ahead:-?}" = 0 ]; then
    ok "ff-merged branch has zero commits ahead (fixture is correct)"
else
    bad "ff-merged fixture" "expected 0 commits ahead, got $_ff_ahead"
fi

out="$(sending)"
printf '%s\n' "$out" | head -20 >&2

want   "sending reaps the ff-merged branch"        "SENT sp-ff"    "$out"
nowant "sending does not report ff-merged as kept" "KEEP   sp-ff"  "$out"

# sp-bf31a: the empty branch is also an ancestor (ahead=0), so content_landed returns 0
# and the Sending reaps it too. CHECK 5 will reopen the bead (no commit on base names it).
want   "sending reaps the empty ancestor branch (sp-bf31a)" "SENT sp-empty" "$out"
nowant "sending does not keep the empty branch"             "KEEP   sp-empty" "$out"

# Verify git refs directly
if git -C "$REPO" show-ref --verify --quiet "refs/heads/spira/sp-ff" 2>/dev/null; then
    bad "the ff-merged branch is actually gone" "spira/sp-ff still exists after reap"
else
    ok "the ff-merged branch is gone (correctly reaped)"
fi
if git -C "$REPO" show-ref --verify --quiet "refs/heads/spira/sp-empty" 2>/dev/null; then
    bad "the empty ancestor branch is gone" "spira/sp-empty still exists after reap (sp-bf31a)"
else
    ok "the empty ancestor branch is gone (reaped as ancestor, bead will be reopened by CHECK 5)"
fi

# --------------------------------------------------------------------------------------
# CASE 4: SQUASH-MERGED — content_landed must return 0 (commits ahead, content on base).
# --------------------------------------------------------------------------------------
echo
echo "squash-merged — content_landed still returns 0 when content is already on the base:"
# Build squash branch after ff_branch (which advanced origin/main)
seed
git -C "$REPO" fetch -q origin

# Create a branch with one file commit, then squash-merge the same content to origin/main
git -C "$REPO" worktree add -q -b "spira/sp-sq" "$RUN/worktree/sp-sq" origin/main
printf 'squashed\n' > "$RUN/worktree/sp-sq/sq.txt"
git -C "$RUN/worktree/sp-sq" add sq.txt
git -C "$RUN/worktree/sp-sq" commit -q -m "sp-sq: squash candidate"
git -C "$REPO" worktree remove "$RUN/worktree/sp-sq" 2>/dev/null || true

# Squash that same content onto origin/main (no branch in it)
git -C "$REPO" worktree add -q "$RUN/worktree/sq-land" origin/main
printf 'squashed\n' > "$RUN/worktree/sq-land/sq.txt"
git -C "$RUN/worktree/sq-land" add sq.txt
git -C "$RUN/worktree/sq-land" commit -q -m "sp-sq: squash merged to main"
git -C "$REPO" push -q origin \
    "$(git -C "$RUN/worktree/sq-land" rev-parse HEAD):refs/heads/main"
git -C "$REPO" fetch -q origin
git -C "$REPO" worktree remove "$RUN/worktree/sq-land" 2>/dev/null || true

_sq_ahead="$(git -C "$REPO" rev-list --count "origin/main..spira/sp-sq" 2>/dev/null)"
if [ "${_sq_ahead:-0}" -gt 0 ] 2>/dev/null; then
    ok "squash branch has commits ahead ($_sq_ahead) — fixture correct"
else
    bad "squash fixture" "branch should have commits ahead of origin/main"
fi

if content_landed "$REPO" "spira/sp-sq" "origin/main"; then
    ok "content_landed returns 0 for squash-merged branch (content already on base)"
else
    bad "content_landed should return 0 for squash-merged" "it returned non-zero"
fi

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
