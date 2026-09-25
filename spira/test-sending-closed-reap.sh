#!/usr/bin/env bash
#
# test-sending-closed-reap.sh — sending.sh reaps closed-bead branches in three shapes
#   that previously accumulated forever (sp-v4652).
#
#   ./test-sending-closed-reap.sh
#
# THREE SHAPES (all tested in one sending pass):
#
#   A. NON-CODE DELIVERS, EMPTY BRANCH. A bead that carries a delivers: label (note,
#      beads, action, etc.) was never expected to commit. Its branch is 0 ahead and an
#      ancestor of the base. content_landed now returns 0 for any ancestor branch
#      (sp-bf31a), so the branch is reaped via the plain content-landed path — SENT,
#      not REAPED, since the delivers:-specific arm is never reached.
#
#   B. SUPERSEDED BEAD, EMPTY BRANCH. A closed duplicate with a supersedes edge and a
#      zero-ahead branch. content_landed's ancestor check (sp-bf31a) reaps it the same
#      way as shape A, before sending.sh's superseded-specific arm is ever reached — SENT,
#      not REAPED. Positive control: a superseded bead with n>0 and unique content (no
#      conflict) is still KEPT.
#
#   C. LANDED BY OTHER PR. A closed bead whose work was included in a batch PR commit
#      that names the bead id. The bead's own PR was closed unmerged; its branch has
#      commits that conflict with the base (post-squash base movement). Previously
#      landed() was only consulted at n=0. Now: closed + landed() = SENT before KEEP.
#      Positive control: a closed bead whose id does NOT appear on the base is KEPT.
#
# covers: spira/sending.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-sending-closed-reap
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up closed-reap || { echo "test-sending-closed-reap: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"
REPONAME=fixture-repo
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" remote set-head origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH"

cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/sending.sh" "$HERE/suite-covers.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub confine.sh 'exit 0'
stub gh 'exit 1'

printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$REPONAME" "$REPO" push main '' '' > "$SH/repo-map"

sending() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" \
    SPIRA_REPO_MAP="$SH/repo-map" \
        bash "$SH/sending.sh" --no-fetch 2>&1
}

# ---------------------------------------------------------------------------
# Git fixture branches:
#
# sp-groom: 0 ahead, ancestor (Shape A — non-code delivers)
# sp-sup0:  0 ahead, ancestor, supersedes sp-sup1 (Shape B — empty superseded)
# sp-supc:  1 commit unique content, supersedes sp-sup1 (Shape B control — kept)
# sp-btch:  1 commit, named in batch commit on main (Shape C — landed by other PR)
# sp-keep:  1 commit, unique content NOT named on main (positive control — kept)
# ---------------------------------------------------------------------------

# sp-groom: empty branch (no commits ever).
git -C "$REPO" checkout -q -b spira/sp-groom main
git -C "$REPO" checkout -q main

# sp-sup0: superseded, empty branch.
git -C "$REPO" checkout -q -b spira/sp-sup0 main
git -C "$REPO" checkout -q main

# sp-supc: superseded, 1 commit with unique content not on main (no conflict = unsafe).
git -C "$REPO" checkout -q -b spira/sp-supc main
printf 'unique-content-not-on-main\n' > "$REPO/sp-supc.txt"
git -C "$REPO" add sp-supc.txt
git -C "$REPO" commit -q -m "sp-supc: unique content"
git -C "$REPO" checkout -q main

# sp-btch: 1 commit, then batch PR squash lands it (naming the id) and base advances.
git -C "$REPO" checkout -q -b spira/sp-btch main
printf 'shared-content-v1\n' > "$REPO/shared.txt"
git -C "$REPO" add shared.txt
git -C "$REPO" commit -q -m "sp-btch: add shared content"
git -C "$REPO" checkout -q main
# Batch merge: a "spira: land <id>" commit carries the same content onto main. This is the
# only subject shape landed() trusts (law-a-matcher-reads-code-not-prose / sp-dgaig) — a
# commit that merely mentions the id in prose or a trailer is not a landing record.
printf 'shared-content-v1\n' > "$REPO/shared.txt"
git -C "$REPO" add shared.txt
git -C "$REPO" commit -q -m "spira: land sp-btch"
# Base moves on the same file — sp-btch's branch now conflicts with main.
printf 'shared-content-v2\n' > "$REPO/shared.txt"
git -C "$REPO" add shared.txt
git -C "$REPO" commit -q -m "follow-up: advance shared.txt"

# sp-keep: real content not on main — must be kept (positive control).
git -C "$REPO" checkout -q -b spira/sp-keep main
printf 'exclusive-unlanded-content\n' > "$REPO/sp-keep.txt"
git -C "$REPO" add sp-keep.txt
git -C "$REPO" commit -q -m "sp-keep: real work"
git -C "$REPO" checkout -q main

git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

# ---------------------------------------------------------------------------
# Bead database seed. Note: bd import uses "type" for dependency kinds.
# ---------------------------------------------------------------------------
testdb_seed <<'JSONL'
{"id":"sp-groom","title":"groomer pass","status":"closed","issue_type":"task","labels":["delivers:note:/tmp/groom.log"],"updated_at":"2026-09-05T00:00:00Z","closed_at":"2026-09-05T00:00:00Z","dependencies":[]}
{"id":"sp-sup1","title":"successor","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-05T00:00:00Z","closed_at":"2026-09-05T00:00:00Z","dependencies":[]}
{"id":"sp-sup0","title":"superseded empty","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-05T00:00:00Z","closed_at":"2026-09-05T00:00:00Z","dependencies":[{"issue_id":"sp-sup0","depends_on_id":"sp-sup1","type":"supersedes"}]}
{"id":"sp-supc","title":"superseded content","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-05T00:00:00Z","closed_at":"2026-09-05T00:00:00Z","dependencies":[{"issue_id":"sp-supc","depends_on_id":"sp-sup1","type":"supersedes"}]}
{"id":"sp-btch","title":"batch landed","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-05T00:00:00Z","closed_at":"2026-09-05T00:00:00Z","dependencies":[]}
{"id":"sp-keep","title":"unlanded","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-05T00:00:00Z","closed_at":"2026-09-05T00:00:00Z","dependencies":[]}
JSONL

echo "test-sending-closed-reap.sh"

# ---------------------------------------------------------------------------
# Fixture verification.
# ---------------------------------------------------------------------------
# shellcheck disable=SC1090
. "$SH/lib.sh"

if content_landed "$REPO" "spira/sp-groom" "origin/main"; then
    ok "sp-groom: content_landed correctly returns 0 (ancestor branch, sp-bf31a)"
else
    bad "sp-groom: content_landed must return 0 for an ancestor (0-ahead) branch" "returned non-zero"
fi
if content_landed "$REPO" "spira/sp-btch" "origin/main"; then
    bad "sp-btch: content_landed must return non-zero (conflict after base moved)" "returned 0"
else
    ok "sp-btch: content_landed correctly returns non-zero (conflict with post-batch base)"
fi
if landed "sp-btch" "$REPO" 2>/dev/null; then
    ok "sp-btch: landed() finds the naming commit on origin/main"
else
    bad "sp-btch: landed() must return 0 (batch commit on main names it)" "returned non-zero"
fi
if landed "sp-keep" "$REPO" 2>/dev/null; then
    bad "sp-keep: landed() must return non-zero" "returned 0 — fixture is wrong"
else
    ok "sp-keep: landed() correctly returns non-zero (not on main)"
fi

# ---------------------------------------------------------------------------
# THE MAIN ASSERTION: one sending pass covering all three shapes.
# ---------------------------------------------------------------------------
echo ""
echo "All shapes in one pass:"
out="$(sending)"
printf '%s\n' "$out" >&2

# Shape A
want   "A: sp-groom is SENT (ancestor branch, sp-bf31a)"    "SENT sp-groom"    "$out"
nowant "A: sp-groom is not KEPT"                            "KEEP   sp-groom"  "$out"
if git -C "$REPO" show-ref --verify --quiet "refs/heads/spira/sp-groom" 2>/dev/null; then
    bad "A: sp-groom branch is gone" "spira/sp-groom still exists after reap"
else
    ok "A: sp-groom branch is gone after reap"
fi

# Shape B
want   "B: sp-sup0 is SENT (ancestor branch, sp-bf31a)"     "SENT sp-sup0"     "$out"
nowant "B: sp-sup0 is not KEPT"                             "KEEP   sp-sup0"   "$out"
if git -C "$REPO" show-ref --verify --quiet "refs/heads/spira/sp-sup0" 2>/dev/null; then
    bad "B: sp-sup0 branch is gone" "spira/sp-sup0 still exists after reap"
else
    ok "B: sp-sup0 branch is gone after reap"
fi
want   "B: sp-supc is KEPT (superseded but unique content)"  "KEEP   sp-supc"   "$out"
nowant "B: sp-supc is not reaped"                            "REAPED sp-supc"   "$out"
if git -C "$REPO" show-ref --verify --quiet "refs/heads/spira/sp-supc" 2>/dev/null; then
    ok "B: sp-supc branch still exists (correctly kept)"
else
    bad "B: sp-supc branch still exists" "spira/sp-supc was deleted — unique content would be lost"
fi

# Shape C
want   "C: sp-btch is SENT (landed via batch commit)"       "SENT sp-btch"     "$out"
nowant "C: sp-btch is not KEPT"                             "KEEP   sp-btch"   "$out"
if git -C "$REPO" show-ref --verify --quiet "refs/heads/spira/sp-btch" 2>/dev/null; then
    bad "C: sp-btch branch is gone" "spira/sp-btch still exists after send"
else
    ok "C: sp-btch branch is gone after send"
fi
want   "C: sp-keep is KEPT (unlanded, positive control)"    "KEEP   sp-keep"   "$out"
nowant "C: sp-keep is not sent or reaped"                   "SENT sp-keep"     "$out"
if git -C "$REPO" show-ref --verify --quiet "refs/heads/spira/sp-keep" 2>/dev/null; then
    ok "C: sp-keep branch still exists (correctly kept)"
else
    bad "C: sp-keep branch still exists" "spira/sp-keep was deleted — unlanded content lost"
fi

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
