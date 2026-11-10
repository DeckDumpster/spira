#!/usr/bin/env bash
#
# test-queue-land-local.sh — sp-ae08w: queue.sh land-local, the queue.local ending. A local
# round's own GREEN verdict fast-forwards the repo's local landing ref straight to the round
# head — no PR — archives that head and marks every member LANDED. bd's status is not the
# landing's to write: reconcile-content, its one writer, closes the bead from the LANDED record,
# citing the landed commit. A head that does not descend from the ref's current tip is refused
# and nothing is written. One writer: the per-repo queue lock, skippable only via
# SPIRA_QUEUE_LOCK_HELD=1 for a caller that already holds it (sp-91hb5's shape, so the
# batcher cannot deadlock on itself).
#
# tier: T1
# covers: queue/src/* gate/src/cert.rs spira/lib.sh spira/conf.sh testenv/src/suites/* spira-lc/src/reconcile_content.rs spira/testlib/lc-fixture.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=180   # slow cases: real git/dolt on a loaded VM
# The queue binary (queue/DESIGN.md §7.4), invoked by name: the tree under test's build is
# on the suite's PATH (sp-gypjk).

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
. "$HERE/testlib/lc-fixture.sh"
testdb_require test-queue-land-local
TMP="$(mktemp -d)"
trap 'lcfix_down; testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# testdb-mode: server — reconcile-content reads the beads over the lifecycle connection, so the
# lifecycle database lives on the bead store's own server.
export SPIRA_TESTDB_MODE=server
testdb_up qlandlocal || { echo "test-queue-land-local: could not build a fixture database"; exit 1; }
lcfix_up_on_testdb || { echo "test-queue-land-local: could not build a lifecycle fixture"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

echo "test-queue-land-local.sh"

SH="$TMP/spira"; mkdir -p "$SH"
cp -r "$HERE"/*.sh "$HERE"/*.py "$SH/" 2>/dev/null || true
cp -r "$HERE/conf.d" "$HERE/conf.toml.d" "$HERE/conf-gen.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub mail 'exit 0'

REPO="$TMP/repo"
git init -q -b trunk "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" branch local/main trunk

# A NON-DEFAULT run dir and queue dir throughout (law-gates-run-in-a-clean-environment).
RUN="$TMP/run"; QDIR="$RUN/queue"; RELEASES="$TMP/releases"
mkdir -p "$RUN/worktree" "$QDIR" "$RELEASES"
RMAP="$TMP/repo-map"
printf 'fixq | %s | queue.local | local/main | | |\n' "$REPO" > "$RMAP"

# sp-sf60f: a land now packages the round head with its own --with-bins corpus and
# activates it (see test-land-local-release.sh for that mechanism in depth) — every head
# this suite lands needs one, or the packaging refusal masks the ref-move assertions below.
# The round worktree queue land-local reads (--worktree, queue/DESIGN.md §8 D2/D3): one
# detached worktree per tree, at <head>; its target/release is the round's own build.
bins_wt() {   # bins_wt <head> -> the round worktree for <head>'s tree (created on first use)
    local head="$1" tree wt
    tree="$(git -C "$REPO" rev-parse "${head}^{tree}")"
    wt="$TMP/round-wt/$tree"
    [ -d "$wt" ] || git -C "$REPO" worktree add -q --detach "$wt" "$head" >/dev/null 2>&1
    printf '%s' "$wt"
}
mk_bins() {   # mk_bins <head> <content> -> the round's own release build in its worktree
    local head="$1" content="$2" dir
    dir="$(bins_wt "$head")/target/release"
    mkdir -p "$dir"
    printf '%s' "$content" > "$dir/fakebin"
    chmod +x "$dir/fakebin"
}

B() { bd -C "$SPIRA_DB" "$@"; } # batch-job: fixture bd call against the suite's throwaway store
field() { B show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
v=d[0].get(sys.argv[1])
print(",".join(v) if isinstance(v,list) else (v or ""))' "$2" 2>/dev/null; }
seed() {   # seed <id>
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":["plan","repo:fixq","spira-submitted"],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" | testdb_seed
}

tl_config SPIRA_RUN="$RUN" SPIRA_QUEUE_DIR="$QDIR" SPIRA_REPO_MAP="$RMAP" \
    SPIRA_RELEASES="$RELEASES" SPIRA_HOME_REPO=fixq SPIRA_QUEUE_LOCK_WAIT=2
run() {
    env "$(lcfix_mark)" \
    SPIRA_CONF=/nonexistent \
    SPIRA_HOME="$SH" \
    SPIRA_REPO="$REPO" \
    SPIRA_LAND_UNGATED="${LAND_UNGATED-fixture: hand-built heads no gate judged}" \
    SPIRA_LAND_UNGATED_BY=operator \
    SPIRA_LAND_UNGATED_EVIDENCE=local/main \
        SPIRA_HOME="$SH" queue "$@" 2>&1
}
run_lockheld() {
    env "$(lcfix_mark)" \
    SPIRA_CONF=/nonexistent \
    SPIRA_HOME="$SH" \
    SPIRA_REPO="$REPO" \
    SPIRA_QUEUE_LOCK_HELD=1 \
    SPIRA_LAND_UNGATED="${LAND_UNGATED-fixture: hand-built heads no gate judged}" \
    SPIRA_LAND_UNGATED_BY=operator \
    SPIRA_LAND_UNGATED_EVIDENCE=local/main \
        SPIRA_HOME="$SH" queue "$@" 2>&1
}
localmain() { git -C "$REPO" rev-parse local/main; }
# The reconcile-content pass the timer runs: bd's status follows the lifecycle record.
reconcile() { lcfix_settle; spira-lc reconcile-content --apply "$@" 2>&1; }
roundseq()  { cat "$QDIR/fixq/round-seq" 2>/dev/null; }

# ============================================================================
echo
echo "1 — a fixture round lands: fast-forward, LANDED, bead closed citing the commit, archived"
# ============================================================================
testdb_reset
seed sp-lloc1

echo "1a — a marked process still holding the store is waited out, not answered cannot-tell"
env "$(lcfix_mark)" flock "$LCFIX_DIR/control.db.lock" \
    sh -c 'while [ ! -e "$1" ]; do sleep 0.1; done' _ "$TMP/release" &
holder=$!
until [ -n "$(grep -lzxF "$(lcfix_mark)" /proc/[0-9]*/environ 2>/dev/null)" ]; do sleep 0.1; done
out="$(spira-lc reconcile-content --apply sp-lloc1 2>&1)"; rc=$?
is "1a: unsettled, reconcile-content cannot tell while the store is held" 2 "$rc"
( sleep 1; : > "$TMP/release" ) &
rec="$(reconcile sp-lloc1)"; rcc=$?
[ "$rcc" -eq 0 ] && ok "1a: reconcile settles the holder first and answers" || bad "1a: reconcile settles the holder first and answers" "rc=$rcc out=$rec"
wait "$holder" 2>/dev/null

git -C "$REPO" checkout -qb round-1 local/main
printf 'one\n' > "$REPO/one.txt"
git -C "$REPO" add one.txt
git -C "$REPO" commit -q -m "sp-lloc1: the work"
HEAD1="$(git -C "$REPO" rev-parse round-1)"
lcfix_seed sp-lloc1 CERTIFIED "$HEAD1"
git -C "$REPO" checkout -q trunk
git -C "$REPO" branch -D round-1 >/dev/null 2>&1
mk_bins "$HEAD1" round-1-bin

# queue/DESIGN.md §8 D12: no round GREEN for this tree, no override -> refused.
out="$(LAND_UNGATED='' run land-local fixq --head "$HEAD1" --members "sp-lloc1:$HEAD1" --worktree "$(bins_wt "$HEAD1")")"; rc=$?
[ "$rc" -ne 0 ] && ok "1: an uncertified tree is refused" || bad "1: an uncertified tree is refused" "rc=$rc out=$out"
want "1: the refusal says no round GREEN certifies it" "no round GREEN for $HEAD1" "$out"
is "1: the refusal moved nothing" "$(git -C "$REPO" rev-parse trunk)" "$(localmain)"

out="$(run land-local fixq --head "$HEAD1" --members "sp-lloc1:$HEAD1" --worktree "$(bins_wt "$HEAD1")")"; rc=$?
want "1: the override is loud" "UNGATED LANDING of $HEAD1" "$out"
[ "$rc" -eq 0 ] && ok "1: exit 0 on a real fast-forward" || bad "1: exit 0 on a real fast-forward" "got rc=$rc out=$out"
is "1: local/main equals the round head" "$HEAD1" "$(localmain)"
is "1: the member's lifecycle row is LANDED" "LANDED" "$(lcfix_state sp-lloc1)"
is "1: the landing writes no bd status — reconcile-content does" open "$(field sp-lloc1 status)"
rec="$(reconcile sp-lloc1)"; rcc=$?; [ "$rcc" -eq 0 ] && ok "1: reconcile-content exits 0" || bad "1: reconcile-content exits 0" "rc=$rcc out=$rec"
want "1: reconcile-content closes the bead from its LANDED record" "closed sp-lloc1 (lifecycle LANDED)" "$rec"
is "1: the bead is closed"                closed "$(field sp-lloc1 status)"
want "1: close reason declares landed"    "OUTCOME: landed" "$(field sp-lloc1 close_reason)"
want "1: close reason cites the round head" "$HEAD1"           "$(field sp-lloc1 close_reason)"
is "1: round-seq advances to 1"           "1" "$(roundseq)"
is "1: the round head is archived"        "$HEAD1" "$(git -C "$REPO" rev-parse -q --verify refs/archive/rounds/1 2>/dev/null)"
want "1: reports the archive ref"         "refs/archive/rounds/1" "$out"

# ============================================================================
echo
echo "2 — a non-fast-forward head is refused: nothing changes"
# ============================================================================
testdb_reset
seed sp-lloc2
PRE_MAIN="$(localmain)"
PRE_SEQ="$(roundseq)"

# Cut from trunk, which never moved — this does NOT descend from local/main's current tip
# (round 1 already moved it forward), so land-local must refuse it outright.
git -C "$REPO" checkout -qb stray-round trunk
printf 'stray\n' > "$REPO/stray.txt"
git -C "$REPO" add stray.txt
git -C "$REPO" commit -q -m "sp-lloc2: stray work"
STRAY="$(git -C "$REPO" rev-parse stray-round)"
lcfix_seed sp-lloc2 CERTIFIED "$STRAY"
git -C "$REPO" checkout -q trunk
git -C "$REPO" branch -D stray-round >/dev/null 2>&1

out="$(run land-local fixq --head "$STRAY" --members "sp-lloc2:$STRAY" --worktree "$(bins_wt "$STRAY")")"; rc=$?
[ "$rc" -ne 0 ] && ok "2: exit non-zero on a non-fast-forward head" || bad "2: exit non-zero on a non-fast-forward head" "got rc=$rc"
want "2: names the refusal"               "does not fast-forward" "$out"
nowant "2: never claims success"          "fast-forwarded to"     "$out"
is "2: local/main is unchanged"           "$PRE_MAIN" "$(localmain)"
is "2: round-seq is unchanged"            "$PRE_SEQ"  "$(roundseq)"
is "2: the refused member's lifecycle row is still CERTIFIED" "CERTIFIED" "$(lcfix_state sp-lloc2)"
rec="$(reconcile sp-lloc2)"; rcc=$?; [ "$rcc" -eq 0 ] && ok "2: reconcile-content exits 0" || bad "2: reconcile-content exits 0" "rc=$rcc out=$rec"
is "2: the bead is left open, even after reconcile-content" open "$(field sp-lloc2 status)"

# ============================================================================
echo
echo "3 — repo not in queue.local mode is refused"
# ============================================================================
RMAP2="$TMP/repo-map-other"
printf 'fixq | %s | queue | local/main | | |\n' "$REPO" > "$RMAP2"
tl_config SPIRA_REPO_MAP="$RMAP2"
out="$(SPIRA_CONF=/nonexistent SPIRA_HOME="$SH" SPIRA_REPO="$REPO" \
    SPIRA_HOME="$SH" queue land-local fixq --head "$HEAD1" --members "sp-lloc1:$HEAD1" --worktree "$(bins_wt "$HEAD1")" 2>&1)"; rc=$?
[ "$rc" -ne 0 ] && ok "3: exit non-zero for a non-queue.local mode" || bad "3: exit non-zero for a non-queue.local mode" "got rc=$rc"
want "3: names the actual mode" "mode=queue" "$out"
# Restore: sections 4+ reuse run()/run_lockheld(), which rely on SPIRA_REPO_MAP="$RMAP".
tl_config SPIRA_REPO_MAP="$RMAP"

# ============================================================================
echo
echo "4 — the per-repo lock is held for section 5 (the refusal is a unit test in queue)"
# ============================================================================
testdb_reset
seed sp-lloc4
git -C "$REPO" checkout -qb round-4 local/main
printf 'four\n' > "$REPO/four.txt"
git -C "$REPO" add four.txt
git -C "$REPO" commit -q -m "sp-lloc4: the work"
HEAD4="$(git -C "$REPO" rev-parse round-4)"
lcfix_seed sp-lloc4 CERTIFIED "$HEAD4"
git -C "$REPO" checkout -q trunk
git -C "$REPO" branch -D round-4 >/dev/null 2>&1
mk_bins "$HEAD4" round-4-bin

mkdir -p "$QDIR/fixq"
exec 8>"$QDIR/fixq/lock"
flock -n 8

# ============================================================================
echo
echo "5 — SPIRA_QUEUE_LOCK_HELD=1 skips the wait for a caller that already holds it"
# ============================================================================
out="$(run_lockheld land-local fixq --head "$HEAD4" --members "sp-lloc4:$HEAD4" --worktree "$(bins_wt "$HEAD4")")"; rc=$?
[ "$rc" -eq 0 ] && ok "5: SPIRA_QUEUE_LOCK_HELD=1 proceeds despite the outside lock" \
    || bad "5: SPIRA_QUEUE_LOCK_HELD=1 proceeds despite the outside lock" "got rc=$rc out=$out"
is "5: local/main advances to the round head" "$HEAD4" "$(localmain)"
is "5: the member's lifecycle row is LANDED" "LANDED" "$(lcfix_state sp-lloc4)"
rec="$(reconcile sp-lloc4)"; rcc=$?; [ "$rcc" -eq 0 ] && ok "5: reconcile-content exits 0" || bad "5: reconcile-content exits 0" "rc=$rcc out=$rec"
want "5: reconcile-content closes the bead from its LANDED record" "closed sp-lloc4 (lifecycle LANDED)" "$rec"
is "5: the bead is closed"                    closed "$(field sp-lloc4 status)"
is "5: round-seq advances to 2"               "2" "$(roundseq)"
is "5: round head 4 is archived"              "$HEAD4" "$(git -C "$REPO" rev-parse -q --verify refs/archive/rounds/2 2>/dev/null)"

flock -u 8
exec 8>&-

# ============================================================================
echo
echo "6 — a hand landing of a SUBMITTED member leaves it LANDED, the override on the transition"
# ============================================================================
testdb_reset
seed sp-lloc6
git -C "$REPO" checkout -qb round-6 local/main
printf 'six\n' > "$REPO/six.txt"
git -C "$REPO" add six.txt
git -C "$REPO" commit -q -m "sp-lloc6: the work"
HEAD6="$(git -C "$REPO" rev-parse round-6)"
lcfix_seed sp-lloc6 SUBMITTED "$HEAD6"
git -C "$REPO" checkout -q trunk
git -C "$REPO" branch -D round-6 >/dev/null 2>&1
mk_bins "$HEAD6" round-6-bin
out="$(LAND_UNGATED='fixture: a hand landing of a submitted member' run land-local fixq --head "$HEAD6" --members "sp-lloc6:$HEAD6" --worktree "$(bins_wt "$HEAD6")")"; rc=$?
[ "$rc" -eq 0 ] && ok "6: exit 0 for a SUBMITTED member" || bad "6: exit 0 for a SUBMITTED member" "got rc=$rc out=$out"
is "6: local/main equals the head" "$HEAD6" "$(localmain)"
is "6: the SUBMITTED member's row is LANDED" "LANDED" "$(lcfix_state sp-lloc6)"
want "6: the transition records the override's reason" "fixture: a hand landing of a submitted member" "$(lcfix_reason sp-lloc6)"
want "6: the transition records the override's actor" "operator" "$(lcfix_reason sp-lloc6)"
want "6: the transition records the override's evidence" "evidence=" "$(lcfix_reason sp-lloc6)"

tl_summary
