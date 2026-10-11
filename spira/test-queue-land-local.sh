#!/usr/bin/env bash
#
# test-queue-land-local.sh — a quarantine edit lands through land-local as a change bead:
# `testenv suites quarantine` (testenv) -> the queue's submit (certifies on spira-lc) ->
# land-local, across three binaries. land-local's own fast-forward, archive, lock and refusal
# rows are Rust tests in the queue crate.
#
# tier: T1
# covers: queue/src/* gate/src/cert.rs spira/lib.sh spira/conf.sh testenv/src/suites/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
# The queue binary (queue/DESIGN.md §7.4), invoked by name: the tree under test's build is
# on the suite's PATH (sp-gypjk).

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
. "$HERE/testlib/lc-fixture.sh"
testdb_require test-queue-land-local
TMP="$(mktemp -d)"
trap 'lcfix_down; testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up qlandlocal || { echo "test-queue-land-local: could not build a fixture database"; exit 1; }
lcfix_up || { echo "test-queue-land-local: could not build a lifecycle fixture"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

echo "test-queue-land-local.sh"

SH="$TMP/spira"; mkdir -p "$SH"
cp -r "$HERE"/*.sh "$HERE"/*.py "$SH/" 2>/dev/null || true
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SH/"
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
    SPIRA_RELEASES="$RELEASES" SPIRA_HOME_REPO=fixq
run() {
    SPIRA_CONF=/nonexistent \
    SPIRA_HOME="$SH" \
    SPIRA_REPO="$REPO" \
    SPIRA_LAND_UNGATED="${LAND_UNGATED-fixture: hand-built heads no gate judged}" \
        SPIRA_HOME="$SH" queue "$@" 2>&1
}
localmain() { git -C "$REPO" rev-parse local/main; }

# ============================================================================
echo
echo "6 — a quarantine edit lands as a change bead: spira-lc row READY -> CERTIFIED -> LANDED (sp-lck63)"
# ============================================================================
# `testenv suites quarantine` is the writer. Handed its change bead, it claims the bead's
# lifecycle row through lib.sh, commits the suite-state row on spira/<bead> and submits it;
# the queue certifies it on spira-lc and keeps no record of its own; the round's land-local
# ending lands it like any work bead. Before sp-lck63 the edit rode a bead-less
# spira-suite-state/* branch certified only in the queue's private record — no row at all.
testdb_reset
seed sp-ssland1
# The home repository's landing ref carries a suite and its lifecycle file.
git -C "$REPO" checkout -qb prep6 local/main
mkdir -p "$REPO/spira"
printf '#!/usr/bin/env bash\nprintf ok\n' > "$REPO/spira/test-q.sh"
: > "$REPO/spira/suite-state"
git -C "$REPO" add spira/test-q.sh spira/suite-state
git -C "$REPO" commit -q -m "fixture: a suite and its lifecycle file"
git -C "$REPO" checkout -q trunk
git -C "$REPO" branch -f local/main prep6
git -C "$REPO" branch -D prep6 >/dev/null 2>&1
BASE6="$(localmain)"
# The gate the queue's submit runs: green (this case is about the route, not the suites).
STUB6="$TMP/stub6"; mkdir -p "$STUB6"
printf '#!/usr/bin/env bash\nprintf "gate %%s\\n" "$1" >> "%s"\nexit 0\n' "$TMP/gate6.log" > "$STUB6/gate.sh"
chmod +x "$STUB6/gate.sh"

out="$(PATH="$STUB6:$PATH" SPIRA_TESTENV_HARNESS="$TMP" SPIRA_CONF=/nonexistent SPIRA_HOME="$SH" \
    SPIRA_REPO="$REPO" \
    testenv suites quarantine test-q.sh sp-defect6 "flaky under load" --change-bead sp-ssland1 2>&1)"; rc=$?
[ "$rc" -eq 0 ] && ok "6: the quarantine transition succeeds" || bad "6: the quarantine transition succeeds" "rc=$rc out=$out"
is "6: the edit's branch is the change bead's own" "spira/sp-ssland1" "$(printf '%s' "$out" | tail -1 | tr -d '[:space:]')"
TIP6="$(git -C "$REPO" rev-parse -q --verify refs/heads/spira/sp-ssland1 2>/dev/null)"
is "6: the branch sits on the landing ref" "$BASE6" "$(git -C "$REPO" rev-parse -q --verify "${TIP6:-none}^" 2>/dev/null)"
want "6: the queue's gate ran for spira/sp-ssland1" "gate spira/sp-ssland1" "$(cat "$TMP/gate6.log" 2>/dev/null)"
is "6: the change bead's lifecycle row is CERTIFIED" "CERTIFIED" "$(lcfix_state sp-ssland1)"
is "6: certified at the branch tip" "$TIP6" "$(lcfix_tip sp-ssland1)"
[ ! -e "$QDIR/sp-ssland1" ] && ok "6: the queue holds no certification record of its own" \
    || bad "6: the queue holds no certification record of its own" "found $QDIR/sp-ssland1: $(cat "$QDIR/sp-ssland1" 2>/dev/null)"
is "6: no bead-less spira-suite-state/* branch" "" "$(git -C "$REPO" for-each-ref --format='%(refname)' refs/heads/spira-suite-state/)"

# The round's ending: the certified member lands.
mk_bins "${TIP6:-$BASE6}" round-6-bin
out="$(run land-local fixq --head "${TIP6:-none}" --members "sp-ssland1:${TIP6:-none}" --worktree "$(bins_wt "${TIP6:-$BASE6}")")"; rc=$?
[ "$rc" -eq 0 ] && ok "6: the round lands the quarantine edit" || bad "6: the round lands the quarantine edit" "rc=$rc out=$out"
is "6: local/main is the edit" "$TIP6" "$(localmain)"
is "6: the change bead's lifecycle row is LANDED" "LANDED" "$(lcfix_state sp-ssland1)"
is "6: the change bead is closed" closed "$(field sp-ssland1 status)"
want "6: the landed lifecycle file quarantines the suite against the defect bead" \
    "test-q.sh | quarantined" "$(git -C "$REPO" show local/main:spira/suite-state 2>/dev/null)"
want "6: ...naming the defect bead, not the change bead" "| sp-defect6 |" "$(git -C "$REPO" show local/main:spira/suite-state 2>/dev/null)"
[ ! -e "$QDIR/sp-ssland1" ] && ok "6: still no queue record after the land" \
    || bad "6: still no queue record after the land" "found $QDIR/sp-ssland1"

tl_summary
