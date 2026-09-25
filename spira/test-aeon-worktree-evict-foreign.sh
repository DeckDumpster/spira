#!/usr/bin/env bash
#
# test-aeon-worktree-evict-foreign.sh — worktree_evict_foreign (lib.sh): a worktree path is
#   keyed on the bead id, and a bead's repo: label can be corrected after the worktree
#   already exists there. aeon.sh must not reuse whatever tree it finds at that path unless
#   it demonstrably belongs to the bead's (possibly corrected) repository — reusing a stale
#   tree silently attaches every later summon to the WRONG repository's checkout, and
#   nothing fails because `git worktree add` is never reached.
#
# gap G10: this function had no test at all before this suite.
#
# rc 0 = moved aside (preserved, never deleted); rc 1 = nothing to do (the tree already
# belongs, or there is no tree there yet); rc 2 = refused (could not move it).
#
# Real git repositories only — no database, no aeon.sh run: the function takes two paths.
#
# covers: spira/aeon.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'chmod -R u+rwx "$TMP" 2>/dev/null; rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

echo "test-aeon-worktree-evict-foreign.sh"

wef() {   # wef <work> <repo> -> sets WEF_OUT (the printed path, if any) and WEF_RC
    WEF_OUT="$(env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_RUN="$TMP/run" \
        bash -c '. "$1"/lib.sh; worktree_evict_foreign "$2" "$3"' _ "$HERE" "$1" "$2" 2>/dev/null)"
    WEF_RC=$?
}

# ===========================================================================================
echo
echo "positive control: no tree at the path at all — nothing to do (rc 1)"
# ===========================================================================================
REPO_A="$TMP/repo-a"; git init -q -b main "$REPO_A"
git -C "$REPO_A" commit -q --allow-empty -m seed

wef "$TMP/nowhere" "$REPO_A"
wantrc "no .git at the work path: rc 1" 1 "$WEF_RC"
is     "nothing is printed" "" "$WEF_OUT"

# ===========================================================================================
echo
echo "a worktree that genuinely belongs to the repo — nothing to do (rc 1), left in place"
# ===========================================================================================
OWN="$TMP/own"
git -C "$REPO_A" worktree add -q "$OWN" -b spira/own main

wef "$OWN" "$REPO_A"
wantrc "own worktree: rc 1" 1 "$WEF_RC"
is     "own worktree is left exactly where it was" yes \
    "$([ -d "$OWN/.git" ] || [ -f "$OWN/.git" ] && echo yes || echo no)"

# ===========================================================================================
echo
echo "a worktree belonging to a DIFFERENT repository — moved aside, never deleted (rc 0)"
# ===========================================================================================
REPO_B="$TMP/repo-b"; git init -q -b main "$REPO_B"
git -C "$REPO_B" commit -q --allow-empty -m seed
FOREIGN="$TMP/foreign"
git -C "$REPO_B" worktree add -q "$FOREIGN" -b spira/foreign main
printf 'uncommitted salvage bait\n' > "$FOREIGN/scratch.txt"

wef "$FOREIGN" "$REPO_A"
wantrc "foreign worktree: rc 0 (moved)" 0 "$WEF_RC"
want   "the printed path names where it went" "$FOREIGN." "$WEF_OUT"
is     "nothing lives at the original path anymore" no \
    "$([ -e "$FOREIGN" ] && echo yes || echo no)"
is     "the moved tree still exists" yes \
    "$([ -d "$WEF_OUT" ] && echo yes || echo no)"
is     "and its uncommitted work is preserved, not deleted" "uncommitted salvage bait" \
    "$(cat "$WEF_OUT/scratch.txt" 2>/dev/null)"
want   "the aside suffix names the repo it actually belonged to" "repo-b" "$WEF_OUT"

# ===========================================================================================
echo
echo "a worktree whose .git resolves to nothing — evicted too, not left as a silent trap"
# ===========================================================================================
# \"Not demonstrably ours\" is the test, not \"demonstrably another's\": a broken checkout
# left in place hands the next aeon a directory `git worktree add` never reaches, exactly
# like a genuinely foreign one — and nothing fails, so nothing says so either.
BROKEN="$TMP/broken"; mkdir -p "$BROKEN"
printf 'gitdir: %s/nonexistent-gitdir\n' "$TMP" > "$BROKEN/.git"

wef "$BROKEN" "$REPO_A"
wantrc "unresolvable .git: rc 0 (evicted, not left in place)" 0 "$WEF_RC"
is     "nothing lives at the original path anymore" no \
    "$([ -e "$BROKEN" ] && echo yes || echo no)"

# ===========================================================================================
echo
echo "refused: the tree cannot be moved (rc 2), nothing lost"
# ===========================================================================================
# Both `git worktree move` and the mv fallback need to write into the PARENT directory to
# rename an entry within it; making that directory unwritable makes both fail the same way
# a real permissions problem would, without needing to fabricate a destination collision.
PARENT="$TMP/locked-parent"; mkdir -p "$PARENT"
REFUSED="$PARENT/foreign"
git -C "$REPO_B" worktree add -q "$REFUSED" -b spira/refused main
printf 'must not be lost\n' > "$REFUSED/scratch.txt"
chmod 555 "$PARENT"

wef "$REFUSED" "$REPO_A"
wantrc "unmovable tree: rc 2" 2 "$WEF_RC"
chmod 755 "$PARENT"
is "the tree is still exactly where it was — refused, not silently dropped" \
    "must not be lost" "$(cat "$REFUSED/scratch.txt" 2>/dev/null)"

tl_summary
