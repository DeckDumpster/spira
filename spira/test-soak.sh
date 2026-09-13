#!/usr/bin/env bash
#
# test-soak.sh — does the gate and the landing pass make forward progress under contention?
#
#   ./test-soak.sh              # the default shape, ~90s
#   SOAK_AEONS=8 ./test-soak.sh
#
# THE QUESTION NO OTHER SUITE HERE ASKS. Every other case is about one call: this verdict,
# this reopen, this lock. Every deadlock this harness has shipped was correct in every one of
# those and wrong in their composition — a per-repository flock that was exactly right about
# two gates sharing a tree, and which four hours later had produced 11 consecutive gate runs
# that judged nothing while origin/main sat still for fifty minutes. Nothing in the suite
# could have caught it, because nothing ran two of these programs at once.
#
# THE SHAPE IS TODAY'S, SCALED DOWN. Several aeons gate their own branches concurrently while
# a landing pass on a clock tries to gate and land the same repository — and the landing pass
# allows itself the SMALLEST wait of any caller, deliberately, so it is the one that starves.
# The scaling is honest about what it preserves: the ratio of a gate's runtime to the landing
# pass's patience, which is what decides whether the pass can ever win the tree. Measured on
# 2026-09-07 that ratio was 660s to 120s; here it is 3s to 1s, which is worse.
#
# WHAT IT ASSERTS IS PROGRESS, NOT ABSENCE. "No deadlock" cannot be observed directly — a
# livelock and a slow queue produce identical logs for any finite window, which is exactly why
# fifty minutes of it read as normal operation. So the claim is positive and measurable: every
# branch reaches a terminal state inside the budget, and the landing pass lands its share
# rather than being starved of the tree by the aeons. A soak that only checked "nothing hung"
# would have passed all morning today.
#
# covers: spira/gate.sh spira/landing.sh
# scar: a per-repository flock correct for two concurrent gates was wrong in composition with a landing pass; four aeons held the tree for 50 minutes while origin/main sat still.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }

AEONS="${SOAK_AEONS:-5}"           # concurrent gate runs, as aeons produce them
ROUNDS="${SOAK_ROUNDS:-3}"         # landing passes interleaved with them
GATE_SECS="${SOAK_GATE_SECS:-3}"   # how long one gate command takes
LAND_WAIT="${SOAK_LAND_WAIT:-1}"   # what a pass on a clock will spend queueing
DEADLINE="${SOAK_DEADLINE:-180}"   # the whole soak must finish inside this

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; MAP="$TMP/repo-map"
GATELOG="$TMP/gate.log"; HOMEDIR="$TMP/home"; VDIR="$TMP/verdicts"
mkdir -p "$RUN/worktree" "$HOMEDIR"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
printf 'base\n' > "$REPO/marker"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin

# Each branch touches its OWN file, so nothing here conflicts: a soak that produced real merge
# conflicts would be measuring conflict handling, and a stuck branch would be correct rather
# than evidence of a deadlock.
for i in $(seq 1 "$AEONS"); do
    w="$TMP/mk$i"
    git -C "$REPO" worktree add -q -b "spira/sp-s$i" "$w" origin/main
    printf 'sp-s%s\n' "$i" > "$w/f$i.txt"
    git -C "$w" add -A; git -C "$w" commit -q -m "feat: sp-s$i — work"
    git -C "$REPO" worktree remove --force "$w"
done

# A gate that takes real time, and that RECORDS which branch it judged in which tree — the
# soak's other job is to prove the answers did not get crossed under contention.
JUDGED="$TMP/judged.log"; : > "$JUDGED"
CMD="sleep $GATE_SECS; printf '%s %s\\n' \"\$SPIRA_GATE_BRANCH\" \"\$(ls f*.txt 2>/dev/null | head -1)\" >> $JUDGED; true"
printf 'repo | %s | push | origin/main |  | %s\n' "$REPO" "$CMD" > "$MAP"

rungate() {              # rungate <branch> [VAR=VAL ...]
    local br="$1"; shift
    env -i HOME="$HOMEDIR" PATH="/usr/bin:/bin" \
        GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
        SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_REPO="$REPO" SPIRA_RUN="$RUN" \
        SPIRA_DB="$TMP/nonexistent-db" SPIRA_REPO_MAP="$MAP" SPIRA_GATE_LOG="$GATELOG" \
        SPIRA_VERDICTS="$VDIR" \
        "$@" bash "$HERE/gate.sh" "$br" repo
}

echo "test-soak.sh — $AEONS concurrent gates, ${GATE_SECS}s each, against a pass that waits ${LAND_WAIT}s"

# --------------------------------------------------------------------------------------
# ROUND 1 — the aeons gate their own branches, all at once, exactly as the sentinel produces
# them. Then the pass tries to gate the same branches with its much smaller patience.
# --------------------------------------------------------------------------------------
started="$(date +%s)"
pids=""
for i in $(seq 1 "$AEONS"); do
    ( rungate "spira/sp-s$i" > "$TMP/aeon$i.out" 2>&1; echo $? > "$TMP/aeon$i.rc" ) &
    pids="$pids $!"
done
for p in $pids; do wait "$p"; done
aeon_elapsed=$(( $(date +%s) - started ))

# COUNTED FROM THE EXIT STATUS, not from the VERDICT line. 75 has meant "no verdict" since
# before this rewrite, so a soak that counted the newer machine-readable line would fail
# against the old code for a protocol reason and never reach the starvation it exists to
# measure — a red that proves the wrong thing is not a red.
nv=0; passed=0
for i in $(seq 1 "$AEONS"); do
    case "$(cat "$TMP/aeon$i.rc" 2>/dev/null)" in
        0)  passed=$((passed+1)) ;;
        75) nv=$((nv+1)) ;;
    esac
done
is "every aeon's own gate reached a verdict" "$AEONS" "$passed"
is "and none of them was starved of the tree" 0 "$nv"

# THE ANSWERS WERE NOT CROSSED. Each line pairs the branch the gate was told about with a file
# that exists only on that branch; a tree swapped mid-trial makes the pair disagree. This is
# the original defect (sp-64v0) and it must stay fixed under exactly this load.
crossed="$(awk '{ split($1, a, "sp-s"); if ($2 != "f" a[2] ".txt") print }' "$JUDGED")"
is "no gate judged another branch's tree" "" "$crossed"

# --------------------------------------------------------------------------------------
# ROUND 2 — THE STARVATION CASE, and the point of the whole exercise. The pass runs while the
# aeons are still gating, with the small wait a caller on a clock must use. Before the verdict
# cache it could only get the tree by outwaiting a full gate, which it is forbidden to do, so
# it withheld every verdict forever; with the cache it needs no tree at all, because the tree
# it would have judged has already been judged.
# --------------------------------------------------------------------------------------
: > "$JUDGED"
for i in $(seq 1 "$AEONS"); do
    ( rungate "spira/sp-s$i" SPIRA_GATE_ALL=1 > /dev/null 2>&1 ) &
done
sleep 1                   # the aeons are inside their trials now
land_nv=0; land_pass=0
for r in $(seq 1 "$ROUNDS"); do
    for i in $(seq 1 "$AEONS"); do
        out="$(rungate "spira/sp-s$i" SPIRA_GATE_LOCK_WAIT="$LAND_WAIT" 2>&1)"; rc=$?
        case "$rc" in
            0)  land_pass=$((land_pass+1)) ;;
            75) land_nv=$((land_nv+1)) ;;
        esac
    done
done
wait 2>/dev/null

total=$(( ROUNDS * AEONS ))
is "the pass reached a verdict on every branch it asked about" "$total" "$land_pass"
is "and was never starved of the tree" 0 "$land_nv"
# THE MEASUREMENT THAT MATTERS, said out loud rather than only asserted: on 2026-09-07 this
# number was 11 of 14.
printf '  ....  landing-shaped runs: %s verdicts, %s withheld (2026-09-07: 3 of 14 reached one)\n' \
    "$land_pass" "$land_nv"

# --------------------------------------------------------------------------------------
# AND IT ALL FINISHED. A livelock and a slow queue look identical in any finite window, so the
# deadline is part of the claim rather than a convenience.
# --------------------------------------------------------------------------------------
elapsed=$(( $(date +%s) - started ))
[ "$elapsed" -lt "$DEADLINE" ] && ok "the whole soak finished in ${elapsed}s, inside its ${DEADLINE}s deadline" \
    || bad "the whole soak finished inside its deadline" "took ${elapsed}s"

# THE POSITIVE CONTROL FOR THE CACHE ITSELF. If the second round had passed because the gate
# had quietly stopped running at all, every one of the assertions above would still hold. It
# must have judged the trees in round 1 and reused them in round 2 — measured, not assumed.
# `grep -c` PRINTS 0 AND EXITS 1 when it matches nothing, so `|| echo 0` appends a second
# line and the variable becomes "0\n0" — which `[` then rejects as not an integer, turning a
# clean red into a syntax error that hides it. Take the count and drop the status.
r1="$(grep -c . "$GATELOG" 2>/dev/null)"; r1="${r1:-0}"
cached="$(grep -c 'rc=0 cached' "$GATELOG" 2>/dev/null)"; cached="${cached:-0}"
[ "$cached" -ge "$total" ] && ok "the pass's verdicts were reused, not re-run ($cached of $r1 rows)" \
    || bad "the pass's verdicts were reused, not re-run" "only $cached cached rows in $r1"

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
