#!/usr/bin/env bash
#
# test-sending-closed-reap.sh — sending.sh reaps closed-bead branches in three shapes
#   that previously accumulated forever (sp-v4652).
#
#   ./test-sending-closed-reap.sh
#
# THREE SHAPES:
#
#   A. NON-CODE DELIVERS, EMPTY BRANCH. A bead that carries a delivers: label (note,
#      beads, action, etc.) was never expected to commit. Its branch is 0 ahead and an
#      ancestor of the base. Previously sending.sh required landed() to return 0, but
#      no commit on the base ever names it. Now: closed + delivers: + 0-ahead + ancestor
#      = REAPED.
#
#   B. SUPERSEDED BEAD, EMPTY BRANCH. A closed duplicate with a supersedes edge and a
#      zero-ahead branch. Previously the safety check ran merge-tree on the empty branch;
#      merge-tree trivially exits 0 on nothing, so the check reported "unsafe to reap"
#      for a branch with nothing to protect. Now: n=0 skips merge-tree entirely.
#
#   C. LANDED BY OTHER PR. A closed bead whose work was included in a batch PR commit
#      that names the bead id. The bead's own PR was closed unmerged; its branch has
#      commits that conflict with the base (post-squash base movement). Previously
#      landed() was only consulted at n=0. Now: closed + landed() = SENT before KEEP.
#
# POSITIVE CONTROLS (law-absence-needs-a-positive-control):
#   - An unlanded branch with real content not on the base must be KEPT.
#   - A superseded branch with n>0 and no conflict (possible new content) must be KEPT.
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

cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/sending.sh" "$SH/"
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
# Shape A fixture: sp-groom — closed bead with delivers:note: label, 0 ahead.
# Shape B fixture: sp-sup0 — closed superseded bead with empty branch (n=0).
# Shape B control: sp-supc — closed superseded bead with n>0 and no conflict.
# Shape C fixture: sp-btch — closed bead landed via batch PR commit naming it.
# Positive control: sp-keep — closed bead with unlanded real content, not batch-named.
# ---------------------------------------------------------------------------

# sp-groom: no commits at all — 0 ahead, ancestor of main.
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

# sp-btch: 1 commit, conflicts with post-batch base movement. sp-btch IS named in the
# batch commit on main.
git -C "$REPO" checkout -q -b spira/sp-btch main
printf 'shared-content-v1\n' > "$REPO/shared.txt"
git -C "$REPO" add shared.txt
git -C "$REPO" commit -q -m "sp-btch: add shared content"
git -C "$REPO" checkout -q main
# Batch PR squash: land sp-btch's content with a commit naming it, then advance main further.
printf 'shared-content-v1\n' > "$REPO/shared.txt"
git -C "$REPO" add shared.txt
git -C "$REPO" commit -q -m "batch: sp-btch landed here (batch PR)"
# Base moves on the same file — now sp-btch's branch conflicts with main.
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
# Seed the bead database.
# ---------------------------------------------------------------------------
testdb_seed <<'JSONL'
{"id":"sp-groom","title":"groomer pass","status":"closed","issue_type":"task","labels":["delivers:note:/tmp/groom.log"],"updated_at":"2026-09-05T00:00:00Z","closed_at":"2026-09-05T00:00:00Z","dependencies":[]}
{"id":"sp-sup0","title":"superseded empty","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-05T00:00:00Z","closed_at":"2026-09-05T00:00:00Z","dependencies":[{"issue_id":"sp-sup0","depends_on_id":"sp-sup1","dependency_type":"supersedes"}]}
{"id":"sp-sup1","title":"successor","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-05T00:00:00Z","closed_at":"2026-09-05T00:00:00Z","dependencies":[]}
{"id":"sp-supc","title":"superseded content","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-05T00:00:00Z","closed_at":"2026-09-05T00:00:00Z","dependencies":[{"issue_id":"sp-supc","depends_on_id":"sp-sup1","dependency_type":"supersedes"}]}
{"id":"sp-btch","title":"batch landed","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-05T00:00:00Z","closed_at":"2026-09-05T00:00:00Z","dependencies":[]}
{"id":"sp-keep","title":"unlanded","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-05T00:00:00Z","closed_at":"2026-09-05T00:00:00Z","dependencies":[]}
JSONL

echo "test-sending-closed-reap.sh"

# ---------------------------------------------------------------------------
# Fixture verification: confirm which cases content_landed and landed see.
# ---------------------------------------------------------------------------
# shellcheck disable=SC1090
. "$SH/lib.sh"

if content_landed "$REPO" "spira/sp-groom" "origin/main"; then
    bad "sp-groom: content_landed must return non-zero for 0-ahead branch" "returned 0"
else
    ok "sp-groom: content_landed correctly returns non-zero (0-ahead branch)"
fi
if content_landed "$REPO" "spira/sp-btch" "origin/main"; then
    bad "sp-btch: content_landed must return non-zero (conflict after base moved)" "returned 0"
else
    ok "sp-btch: content_landed correctly returns non-zero (conflict with post-batch base)"
fi
if content_landed "$REPO" "spira/sp-keep" "origin/main"; then
    bad "sp-keep: content_landed must return non-zero for unlanded content" "returned 0"
else
    ok "sp-keep: content_landed correctly returns non-zero (unlanded content)"
fi

# sp-btch commit message names the bead — landed() must return 0.
if landed "sp-btch" "$REPO" 2>/dev/null; then
    ok "sp-btch: landed() finds the naming commit on origin/main"
else
    bad "sp-btch: landed() must return 0 (batch commit on main names it)" "returned non-zero"
fi
# sp-keep is NOT named on main.
if landed "sp-keep" "$REPO" 2>/dev/null; then
    bad "sp-keep: landed() must return non-zero" "returned 0 — fixture is wrong"
else
    ok "sp-keep: landed() correctly returns non-zero (not on main)"
fi

# ---------------------------------------------------------------------------
# Shape A: non-code delivers bead is reaped.
# ---------------------------------------------------------------------------
echo ""
echo "Shape A — non-code delivers, 0-ahead, ancestor:"
out_a="$(sending)"
printf '%s\n' "$out_a" >&2

want   "A: sp-groom is REAPED"                       "REAPED sp-groom"  "$out_a"
nowant "A: sp-groom is not KEPT"                     "KEEP   sp-groom"  "$out_a"
if git -C "$REPO" show-ref --verify --quiet "refs/heads/spira/sp-groom" 2>/dev/null; then
    bad "A: sp-groom branch is gone" "spira/sp-groom still exists after reap"
else
    ok "A: sp-groom branch is gone after reap"
fi

# ---------------------------------------------------------------------------
# Shape B: superseded empty branch is reaped; superseded non-empty with unique
# content (no conflict) is kept.
# ---------------------------------------------------------------------------
echo ""
echo "Shape B — superseded + empty branch:"
# sp-groom was just reaped; sp-sup0 and sp-supc remain.
out_b="$(sending)"
printf '%s\n' "$out_b" >&2

want   "B: sp-sup0 is REAPED (empty + superseded)"    "REAPED sp-sup0"  "$out_b"
nowant "B: sp-sup0 is not KEPT"                       "KEEP   sp-sup0"  "$out_b"
if git -C "$REPO" show-ref --verify --quiet "refs/heads/spira/sp-sup0" 2>/dev/null; then
    bad "B: sp-sup0 branch is gone" "spira/sp-sup0 still exists after reap"
else
    ok "B: sp-sup0 branch is gone after reap"
fi

want   "B: sp-supc is KEPT (superseded but has unique content)"  "KEEP   sp-supc"  "$out_b"
nowant "B: sp-supc is not reaped (content absent from base)"     "REAPED sp-supc"  "$out_b"
if git -C "$REPO" show-ref --verify --quiet "refs/heads/spira/sp-supc" 2>/dev/null; then
    ok "B: sp-supc branch still exists (correctly kept)"
else
    bad "B: sp-supc branch still exists" "spira/sp-supc was deleted — unique content would be lost"
fi

# ---------------------------------------------------------------------------
# Shape C: batch-named branch is sent; unlanded branch is kept.
# ---------------------------------------------------------------------------
echo ""
echo "Shape C — landed by other PR (batch commit names bead):"
out_c="$(sending)"
printf '%s\n' "$out_c" >&2

want   "C: sp-btch is SENT (landed() found naming commit)"  "SENT sp-btch"  "$out_c"
nowant "C: sp-btch is not KEPT"                             "KEEP   sp-btch" "$out_c"
if git -C "$REPO" show-ref --verify --quiet "refs/heads/spira/sp-btch" 2>/dev/null; then
    bad "C: sp-btch branch is gone" "spira/sp-btch still exists after send"
else
    ok "C: sp-btch branch is gone after send"
fi

want   "C: sp-keep is KEPT (unlanded, positive control)"    "KEEP   sp-keep"  "$out_c"
nowant "C: sp-keep is not sent or reaped"                   "SENT sp-keep"    "$out_c"
if git -C "$REPO" show-ref --verify --quiet "refs/heads/spira/sp-keep" 2>/dev/null; then
    ok "C: sp-keep branch still exists (correctly kept)"
else
    bad "C: sp-keep branch still exists" "spira/sp-keep was deleted — unlanded content lost"
fi

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
