#!/usr/bin/env bash
#
# test-gate-tree.sh — the gate tree is locked per branch; same-branch gates serialise,
# different-branch gates run concurrently without crossing each other's content.
#
#   ./test-gate-tree.sh
#
# Per-branch trees mean two gates on DIFFERENT branches have separate worktrees and
# separate locks — they run fully concurrently. Two gates on the SAME branch share one
# tree and one lock, so they serialise exactly as before. The defect sp-64v0 (wrong-
# branch verdicts from a shared tree) is impossible with per-branch trees.
#
# defect: sp-64v0, sp-d8h0r
# covers: spira/gate.sh spira/gate-sweep.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want(){ [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; RUN="$TMP/run"; SH="$TMP/spira"
MAP="$TMP/repo-map"; VDIR="$TMP/verdicts"; GATELOG="$TMP/gate.log"; HOMEDIR="$TMP/home"
JUDGED="$TMP/judged.log"
mkdir -p "$RUN/worktree" "$HOMEDIR" "$SH"
: > "$JUDGED"

# The gate under test is a copy — the harness's own bytes are part of the verdict key, so
# the installed copy must not be what is tested.
cp "$HERE/gate.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/exclude.sh" "$HERE/skew.sh" \
   "$HERE/yield.sh" "$HERE/suite-covers.sh" "$SH/"
[ -r "$HERE/gate-sweep.sh" ] && cp "$HERE/gate-sweep.sh" "$SH/" || true

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
printf 'base\n' > "$REPO/marker"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin

# Two branches, each touching its own file — distinguishable in the tree at trial time.
for i in 1 2; do
    w="$TMP/mk$i"
    git -C "$REPO" worktree add -q -b "spira/sp-t$i" "$w" origin/main
    printf 'sp-t%s\n' "$i" > "$w/f$i.txt"
    git -C "$w" add -A; git -C "$w" commit -q -m "feat: sp-t$i — work"
    git -C "$REPO" worktree remove --force "$w"
done

# THE GATE COMMAND: sleep long enough that two concurrent runs would overlap without the
# lock, then record which branch and which file the tree actually holds.
GATE_SECS=3
CMD="sleep $GATE_SECS; printf '%s %s\\n' \"\$SPIRA_GATE_BRANCH\" \"\$(ls f*.txt 2>/dev/null | head -1)\" >> $JUDGED; true"
printf 'repo | %s | push | origin/main |  | %s\n' "$REPO" "$CMD" > "$MAP"

# Branch name → filesystem-safe key, matching gate.sh's TREE_KEY computation.
tree_key() { printf '%s' "$1" | tr '/' '-' | tr -c 'A-Za-z0-9.-' '-'; }

rungate() {              # rungate <branch> [VAR=VAL ...]
    local br="$1"; shift
    env -i HOME="$HOMEDIR" PATH="/usr/bin:/bin" \
        GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
        SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_REPO="$REPO" SPIRA_RUN="$RUN" \
        SPIRA_DB="$TMP/nonexistent-db" SPIRA_REPO_MAP="$MAP" SPIRA_GATE_LOG="$GATELOG" \
        SPIRA_VERDICTS="$VDIR" SPIRA_VERDICT_TTL=0 \
        "$@" bash "$SH/gate.sh" "$br" repo
}

echo "test-gate-tree.sh — per-branch trees: different branches concurrent, same branch serialised"

# Compute the branch key gate.sh uses to name per-branch trees and lock files.
# gate.sh: TREE_KEY="$(printf '%s' "$BR" | tr '/' '-' | tr -c 'A-Za-z0-9.-' '-')"
branch_key() { printf '%s' "$1" | tr '/' '-' | tr -c 'A-Za-z0-9.-' '-'; }
T1_KEY="$(branch_key "spira/sp-t1")"  # spira-sp-t1
T2_KEY="$(branch_key "spira/sp-t2")"  # spira-sp-t2

# --------------------------------------------------------------------------------------
# CASE 1 — CONCURRENT ISOLATION AND CORRECTNESS. Two gates on different branches each have
# their own tree and lock, so they run concurrently without crossing each other's content.
# --------------------------------------------------------------------------------------
: > "$JUDGED"  # start fresh
t0=$(date +%s)
: > "$JUDGED"
( rungate "spira/sp-t1" > "$TMP/g1.out" 2>&1; echo $? > "$TMP/g1.rc" ) &
( rungate "spira/sp-t2" > "$TMP/g2.out" 2>&1; echo $? > "$TMP/g2.rc" ) &
wait

rc1="$(cat "$TMP/g1.rc" 2>/dev/null)"; rc2="$(cat "$TMP/g2.rc" 2>/dev/null)"
is  "gate 1 reached a verdict" 0 "$rc1"
is  "gate 2 reached a verdict" 0 "$rc2"

# CONCURRENT: two gates on different branches have separate trees and locks; the pair
# completes in ~GATE_SECS, not ~GATE_SECS*2.
elapsed=$(( $(date +%s) - t0 ))
[ "$elapsed" -lt $(( GATE_SECS * 2 )) ] \
    && ok "different-branch gates ran concurrently (${elapsed}s < $((GATE_SECS*2))s)" \
    || bad "different-branch gates ran concurrently" "${elapsed}s >= $((GATE_SECS*2))s — they serialised"

# CORRECTNESS: each gate judged its own branch's tree, never the other's.
crossed="$(awk '{ split($1, a, "sp-t"); if ($2 != "f" a[2] ".txt") print }' "$JUDGED")"
is "neither gate observed the other's branch" "" "$crossed"

# --------------------------------------------------------------------------------------
# CASE 1b — SAME-BRANCH SERIALISATION. Two gates on the same branch share one tree and
# one lock, so they serialise. The one that waits records its wait in the gate log
# (law-take-the-simple-fix-with-a-meter).
# --------------------------------------------------------------------------------------
GATELOG1B="$TMP/gate1b.log"
t1b=$(date +%s)
( rungate "spira/sp-t1" SPIRA_GATE_LOG="$GATELOG1B" > "$TMP/g1b1.out" 2>&1; echo $? > "$TMP/g1b1.rc" ) &
( rungate "spira/sp-t1" SPIRA_GATE_LOG="$GATELOG1B" > "$TMP/g1b2.out" 2>&1; echo $? > "$TMP/g1b2.rc" ) &
wait
elapsed1b=$(( $(date +%s) - t1b ))
[ "$elapsed1b" -ge $(( GATE_SECS * 2 - 1 )) ] \
    && ok "same-branch gates were serialised (${elapsed1b}s >= $((GATE_SECS*2-1))s)" \
    || bad "same-branch gates were serialised" "${elapsed1b}s < $((GATE_SECS*2-1))s — they overlapped"
waits1b="$(grep -oE 'waited=[0-9]+s' "$GATELOG1B" 2>/dev/null \
    | sed 's/waited=//;s/s$//' | sort -n | tail -1)"
[ "${waits1b:-0}" -gt 0 ] \
    && ok "the serialisation wait was metered (${waits1b}s)" \
    || bad "the serialisation wait was metered" "no non-zero waited= in the same-branch gate log"

# --------------------------------------------------------------------------------------
# CASE 3 — GRACEFUL TIMEOUT. A gate that cannot obtain the tree in time returns NO_VERDICT
# (exit 75), not FAIL (exit 1). The branch is not charged for a queue — that is a machinery
# fault, not a judgement about the work. The lock file path now includes the branch key.
# --------------------------------------------------------------------------------------
LOCKFILE="$RUN/worktree/.gate.$(basename "$REPO").$T1_KEY.lock"
exec 8>"$LOCKFILE"
flock -x 8

# fd 8 is closed in the subshell so the gate process blocks on the PARENT's lock, not on
# its own inherited descriptor — which is the real shape: two separate processes contending.
out="$( exec 8>&-; rungate "spira/sp-t1" SPIRA_GATE_LOCK_WAIT=1 2>&1 )"; rc=$?
is   "a gate that cannot get the tree returns NO_VERDICT" 75 "$rc"
want "and names the reason as lock-timeout" "lock-timeout" "$out"
want "and says it is a queue, not a fault"  "not a fault"  "$out"

exec 8>&-

# --------------------------------------------------------------------------------------
# CASE 4 — WORKTREE LIFECYCLE. On failure the gate removes its tree; on success it keeps
# it so the next run's checkout touches only changed files and build-tool fingerprints
# (cargo mtime) survive. The tree is detached, so a kept registration does not pin any
# branch ref. gate-sweep.sh removes trees that have been idle longer than MAX_AGE.
# --------------------------------------------------------------------------------------
TREE_PATH="$RUN/worktree/.gate.$(basename "$REPO").$T1_KEY"

# Pass case: a gate that exits 0 keeps the tree for the next run to reuse.
rungate "spira/sp-t1" > "$TMP/g3.out" 2>&1; g3_rc=$?
is "gate exits 0 on a passing branch" 0 "$g3_rc"
registered="$(git -C "$REPO" worktree list --porcelain 2>/dev/null \
    | awk -v p="$TREE_PATH" '/^worktree /{if($2==p)c++} END{print c+0}')"
is "worktree remains registered after a passing gate" 1 "$registered"
[ -d "$TREE_PATH" ] \
    && ok "worktree directory is kept after a passing gate" \
    || bad "worktree directory is kept after a passing gate" "directory is missing at $TREE_PATH"

# Fail case: a gate that exits non-zero must also have removed its tree.
CMD_FAIL='[ "$SPIRA_GATE_BRANCH" = "$SPIRA_GATE_BASE" ] || { echo "gate: test-gate-tree.sh FAILED deliberately"; exit 1; }'
printf 'repo | %s | push | origin/main |  | %s\n' "$REPO" "$CMD_FAIL" > "$MAP"
rungate "spira/sp-t1" > "$TMP/g4.out" 2>&1; g4_rc=$?
is "gate exits 1 on a failing branch" 1 "$g4_rc"
registered="$(git -C "$REPO" worktree list --porcelain 2>/dev/null \
    | awk -v p="$TREE_PATH" '/^worktree /{if($2==p)c++} END{print c+0}')"
is "worktree is deregistered after a failing gate" 0 "$registered"
[ ! -d "$TREE_PATH" ] \
    && ok "worktree directory is removed after a failing gate" \
    || bad "worktree directory is removed after a failing gate" "directory still exists at $TREE_PATH"
printf 'repo | %s | push | origin/main |  | %s\n' "$REPO" "$CMD" > "$MAP"  # restore

# --------------------------------------------------------------------------------------
# CASE 5 — TIMEOUT DOES NOT REMOVE THE HOLDER'S TREE (sp-d8h0r). When the lock wait
# expires, verdict() must not remove the tree — it was never locked by this run.
# --------------------------------------------------------------------------------------
mkdir -p "$TREE_PATH"
touch "$TREE_PATH/.git"    # stand in for the holder's registered tree

exec 8>"$LOCKFILE"
flock -x 8

out5="$( exec 8>&-; rungate "spira/sp-t1" SPIRA_GATE_LOCK_WAIT=1 2>&1 )"; rc5=$?
is   "timed-out gate returns NO_VERDICT" 75 "$rc5"
[ -e "$TREE_PATH/.git" ] \
    && ok "the holder's worktree survives a timeout" \
    || bad "the holder's worktree survives a timeout" "tree was removed by the waiter"

exec 8>&-
rm -rf "$TREE_PATH"

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
