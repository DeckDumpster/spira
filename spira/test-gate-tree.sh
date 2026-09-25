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
# CONCURRENCY IS PROVEN BY START/END MARKERS, NOT ELAPSED WALL-CLOCK TIME. Each gate
# command timestamps its own run window (ns resolution) to a shared events file; "ran
# concurrently" and "serialised" are then read off whether those windows overlap, not off
# a "faster/slower than N seconds" threshold. The prior version compared elapsed time
# against GATE_SECS*2 and flipped under load from unrelated batches — a busy host slows
# every run by the same amount and a threshold built for an idle host reads that as
# "serialised" (law-a-test-that-flips-is-deleted; this suite replaces it, sp-78xpb/sp-fxvgo).
#
# defect: sp-64v0, sp-d8h0r
# tier: T2
# covers: spira/gate.sh spira/gate-lib.sh UC-gate-verdict-16 UC-gate-verdict-17 UC-gate-verdict-18
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
. "$HERE/gate-lib.sh"

command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; RUN="$TMP/run"; SH="$TMP/spira"
MAP="$TMP/repo-map"; VDIR="$TMP/verdicts"; GATELOG="$TMP/gate.log"; HOMEDIR="$TMP/home"
JUDGED="$TMP/judged.log"; EVENTS="$TMP/events.log"
mkdir -p "$RUN/worktree" "$HOMEDIR" "$SH"
: > "$JUDGED"; : > "$EVENTS"

# The gate under test is a copy — the harness's own bytes are part of the verdict key, so
# the installed copy must not be what is tested.
cp "$HERE/gate.sh" "$HERE/gate-lib.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/exclude.sh" "$HERE/skew.sh" \
   "$HERE/yield.sh" "$HERE/suite-covers.sh" "$SH/"

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

# THE GATE COMMAND: mark its own start/end (ns), keyed by SPIRA_GATE_BRANCH — the one
# label gate.sh's own env -i still lets through the sandboxed command environment (a
# custom marker var does not survive it: gate.sh L551-563 execs the command under a fresh
# env -i naming only its own SPIRA_GATE_* contract) — then record which branch and which
# file the tree actually holds.
GATE_SECS=1
CMD="printf 'START %s %s\\n' \"\$SPIRA_GATE_BRANCH\" \"\$(date +%s%N)\" >> $EVENTS; sleep $GATE_SECS; printf 'END %s %s\\n' \"\$SPIRA_GATE_BRANCH\" \"\$(date +%s%N)\" >> $EVENTS; printf '%s %s\\n' \"\$SPIRA_GATE_BRANCH\" \"\$(ls f*.txt 2>/dev/null | head -1)\" >> $JUDGED; true"
printf 'repo | %s | push | origin/main |  | %s\n' "$REPO" "$CMD" > "$MAP"

rungate() {              # rungate <branch> [VAR=VAL ...]
    local br="$1"; shift
    env -i HOME="$HOMEDIR" PATH="/usr/bin:/bin" \
        GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
        SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_REPO="$REPO" SPIRA_RUN="$RUN" \
        SPIRA_DB="$TMP/nonexistent-db" SPIRA_REPO_MAP="$MAP" SPIRA_GATE_LOG="$GATELOG" \
        SPIRA_VERDICTS="$VDIR" SPIRA_VERDICT_TTL=0 \
        "$@" bash "$SH/gate.sh" "$br" repo
}

# marker_start/marker_end <label> — ns timestamp gate_tree wrote for that run, from $EVENTS.
marker_start() { awk -v l="$1" '$1=="START" && $2==l {print $3; exit}' "$EVENTS"; }
marker_end()   { awk -v l="$1" '$1=="END"   && $2==l {print $3; exit}' "$EVENTS"; }

# overlaps <s1> <e1> <s2> <e2> — 0 iff both windows are present and intersect.
overlaps() {
    [ -n "$1" ] && [ -n "$2" ] && [ -n "$3" ] && [ -n "$4" ] \
        && [ "$1" -lt "$4" ] && [ "$3" -lt "$2" ]
}

# label_self_overlaps <label> <events-file> — 0 (true) iff two START/END windows recorded
# under the same label (two runs of the same branch) overlap in time: sort all of that
# label's START/END events chronologically and look for a START before the prior one's END.
label_self_overlaps() {
    awk -v l="$1" '$2==l {print $3, $1}' "$2" | sort -n | awk '
        $2=="START" { if (open==1) found=1; open=1 }
        $2=="END"   { open=0 }
        END         { exit !found }
    '
}

echo "test-gate-tree.sh — per-branch trees: different branches concurrent, same branch serialised"

T1_KEY="$(gate_tree_key "spira/sp-t1")"

# --------------------------------------------------------------------------------------
# CASE 1 (UC-16) — CONCURRENT ISOLATION AND CORRECTNESS. Two gates on different branches
# each have their own tree and lock, so they run concurrently without crossing each
# other's content, and their command windows actually overlap.
# --------------------------------------------------------------------------------------
( rungate "spira/sp-t1" > "$TMP/g1.out" 2>&1; echo $? > "$TMP/g1.rc" ) &
( rungate "spira/sp-t2" > "$TMP/g2.out" 2>&1; echo $? > "$TMP/g2.rc" ) &
wait

rc1="$(cat "$TMP/g1.rc" 2>/dev/null)"; rc2="$(cat "$TMP/g2.rc" 2>/dev/null)"
is  "gate 1 reached a verdict" 0 "$rc1"
is  "gate 2 reached a verdict" 0 "$rc2"

s1="$(marker_start "spira/sp-t1")"; e1="$(marker_end "spira/sp-t1")"
s2="$(marker_start "spira/sp-t2")"; e2="$(marker_end "spira/sp-t2")"
if overlaps "$s1" "$e1" "$s2" "$e2"; then
    ok "different-branch gates ran concurrently (their command windows overlapped)"
else
    bad "different-branch gates ran concurrently" "g1=[$s1,$e1] g2=[$s2,$e2] did not overlap"
fi

# CORRECTNESS: each gate judged its own branch's tree, never the other's.
crossed="$(awk '{ split($1, a, "sp-t"); if ($2 != "f" a[2] ".txt") print }' "$JUDGED")"
is "neither gate observed the other's branch" "" "$crossed"

# --------------------------------------------------------------------------------------
# CASE 2 (UC-16) — SAME-BRANCH SERIALISATION. Two gates on the same branch share one tree
# and one lock, so their command windows never overlap, and the one that waits records its
# wait in the gate log (law-take-the-simple-fix-with-a-meter).
# --------------------------------------------------------------------------------------
: > "$EVENTS"  # fresh: case 1's spira/sp-t1 window must not be mistaken for one of this pair
GATELOG1B="$TMP/gate1b.log"
( rungate "spira/sp-t1" SPIRA_GATE_LOG="$GATELOG1B" > "$TMP/g1b1.out" 2>&1; echo $? > "$TMP/g1b1.rc" ) &
( rungate "spira/sp-t1" SPIRA_GATE_LOG="$GATELOG1B" > "$TMP/g1b2.out" 2>&1; echo $? > "$TMP/g1b2.rc" ) &
wait

if label_self_overlaps "spira/sp-t1" "$EVENTS"; then
    bad "same-branch gates were serialised" "the two spira/sp-t1 command windows overlapped"
else
    ok "same-branch gates were serialised (their command windows did not overlap)"
fi
waits1b="$(grep -oE 'waited=[0-9]+s' "$GATELOG1B" 2>/dev/null \
    | sed 's/waited=//;s/s$//' | sort -n | tail -1)"
[ "${waits1b:-0}" -gt 0 ] \
    && ok "the serialisation wait was metered (${waits1b}s)" \
    || bad "the serialisation wait was metered" "no non-zero waited= in the same-branch gate log"

# --------------------------------------------------------------------------------------
# CASE 3 (UC-17) — LOCK TIMEOUT DOES NOT FAULT THE BRANCH AND NEVER TOUCHES THE HOLDER'S
# TREE. A gate that cannot obtain the tree lock in time returns NO_VERDICT (exit 75), not
# FAIL — a queue is not a judgement about the work — and it must not remove the tree it
# never held. One held lock proves both facts (sp-d8h0r; §4 point 4 of the plan folds the
# old case 3 and case 5 into this single run).
# --------------------------------------------------------------------------------------
LOCKFILE="$RUN/worktree/.gate.$(basename "$REPO").$T1_KEY.lock"
TREE_PATH="$RUN/worktree/.gate.$(basename "$REPO").$T1_KEY"
mkdir -p "$TREE_PATH"
touch "$TREE_PATH/.git"    # stand in for the holder's registered tree

exec 8>"$LOCKFILE"
flock -x 8

# fd 8 is closed in the subshell so the gate process blocks on the PARENT's lock, not on
# its own inherited descriptor — which is the real shape: two separate processes contending.
out="$( exec 8>&-; rungate "spira/sp-t1" SPIRA_GATE_LOCK_WAIT=1 2>&1 )"; rc=$?
is   "a gate that cannot get the tree returns NO_VERDICT" 75 "$rc"
want "and names the reason as lock-timeout" "lock-timeout" "$out"
want "and says it is a queue, not a fault"  "not a fault"  "$out"
[ -e "$TREE_PATH/.git" ] \
    && ok "the holder's worktree survives a timeout" \
    || bad "the holder's worktree survives a timeout" "tree was removed by the waiter"

exec 8>&-
rm -rf "$TREE_PATH"

# --------------------------------------------------------------------------------------
# CASE 4 (UC-18) — WORKTREE LIFECYCLE. On failure the gate removes its tree; on success it
# keeps it so the next run's checkout touches only changed files and build-tool
# fingerprints (cargo mtime) survive. The tree is detached, so a kept registration does
# not pin any branch ref.
# --------------------------------------------------------------------------------------
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

tl_summary
