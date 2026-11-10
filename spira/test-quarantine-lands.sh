#!/usr/bin/env bash
#
# test-quarantine-lands.sh — a quarantine edit lands as a change bead. `testenv suites quarantine
# --change-bead` claims the bead's lifecycle row, commits the suite-state row on spira/<bead> and
# certifies it on spira-lc, keeping no queue record of its own; the round's land-local lands it
# to LANDED and reconcile-content closes it. (UC-landing-merge-queue-27)
#
# tier: T2
# covers: queue/src/* testenv/src/suites/* spira/lib.sh spira-lc/src/reconcile_content.rs spira/testlib/lc-fixture.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=180   # real git/dolt on a loaded VM

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
. "$HERE/testlib/lc-fixture.sh"
testdb_require test-quarantine-lands
TMP="$(mktemp -d)"
trap 'lcfix_down; testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# testdb-mode: server — reconcile-content reads the beads over the lifecycle connection, on the bead store's server.
export SPIRA_TESTDB_MODE=server
testdb_up qlands || { echo "test-quarantine-lands: could not build a fixture database"; exit 1; }
lcfix_up_on_testdb || { echo "test-quarantine-lands: could not build a lifecycle fixture"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

echo "test-quarantine-lands.sh"

SH="$TMP/spira"; mkdir -p "$SH"
cp -r "$HERE"/*.sh "$HERE"/*.py "$SH/" 2>/dev/null || true
cp -r "$HERE/conf.d" "$HERE/conf.toml.d" "$HERE/conf-gen.sh" "$SH/"
printf '#!/usr/bin/env bash\nexit 0\n' > "$SH/mail"; chmod +x "$SH/mail"

REPO="$TMP/repo"
git init -q -b trunk "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
mkdir -p "$REPO/spira"
printf '#!/usr/bin/env bash\nprintf ok\n' > "$REPO/spira/test-q.sh"
: > "$REPO/spira/suite-state"
git -C "$REPO" add spira/test-q.sh spira/suite-state
git -C "$REPO" commit -q -m "fixture: a suite and its lifecycle file"
git -C "$REPO" branch local/main trunk

# Non-default run and queue dirs (law-gates-run-in-a-clean-environment).
RUN="$TMP/run"; QDIR="$RUN/queue"; RELEASES="$TMP/releases"
mkdir -p "$RUN/worktree" "$QDIR" "$RELEASES"
tl_repo_map "fixq | $REPO | queue.local | local/main | | |"
tl_config SPIRA_RUN="$RUN" SPIRA_QUEUE_DIR="$QDIR" \
    SPIRA_RELEASES="$RELEASES" SPIRA_HOME_REPO=fixq SPIRA_QUEUE_LOCK_WAIT=2

# A land packages the round head with its own build: one detached worktree per head, holding it.
bins_wt() {
    local wt="$TMP/round-wt/$1"
    [ -d "$wt" ] || git -C "$REPO" worktree add -q --detach "$wt" "$1" >/dev/null 2>&1
    printf '%s' "$wt"
}
mk_bins() {
    local dir; dir="$(bins_wt "$1")/target/release"
    mkdir -p "$dir"; printf 'round-bin' > "$dir/fakebin"; chmod +x "$dir/fakebin"
}
B() { bd -C "$SPIRA_DB" "$@"; } # batch-job: fixture bd call against the suite's throwaway store
field() { B show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
v=d[0].get(sys.argv[1])
print(",".join(v) if isinstance(v,list) else (v or ""))' "$2" 2>/dev/null; }
localmain() { git -C "$REPO" rev-parse local/main; }
marked() { grep -lzxF "$(lcfix_mark)" /proc/[0-9]*/environ 2>/dev/null; }

testdb_reset
printf '{"id":"sp-ssland1","title":"t","status":"open","issue_type":"task","labels":["plan","repo:fixq","spira-submitted"],"updated_at":"2026-09-04T00:00:00Z"}\n' | testdb_seed
BASE="$(localmain)"
# The gate the queue's submit runs: green, recording what it judged.
STUB="$TMP/stub"; mkdir -p "$STUB"
printf '#!/usr/bin/env bash\nprintf "gate %%s\\n" "$1" >> "%s"\nexit 0\n' "$TMP/gate.log" > "$STUB/gate.sh"
chmod +x "$STUB/gate.sh"

echo
echo "the quarantine edit is certified on its change bead's own branch"
out="$(env "$(lcfix_mark)" PATH="$STUB:$PATH" SPIRA_RELEASE="$(PATH="$STUB:$PATH" tl_release)" SPIRA_TESTENV_HARNESS="$TMP" \
    SPIRA_CONF=/nonexistent SPIRA_HOME="$SH" SPIRA_REPO="$REPO" \
    testenv suites quarantine test-q.sh sp-defect6 "flaky under load" --change-bead sp-ssland1 2>&1)"; rc=$?
[ "$rc" -eq 0 ] && ok "the quarantine transition succeeds" || bad "the quarantine transition succeeds" "rc=$rc out=$out"
is "the edit's branch is the change bead's own" "spira/sp-ssland1" "$(printf '%s' "$out" | tail -1 | tr -d '[:space:]')"
TIP="$(git -C "$REPO" rev-parse -q --verify refs/heads/spira/sp-ssland1 2>/dev/null)"
is "the branch sits on the landing ref" "$BASE" "$(git -C "$REPO" rev-parse -q --verify "${TIP:-none}^" 2>/dev/null)"
want "the queue's gate ran for spira/sp-ssland1" "gate spira/sp-ssland1" "$(cat "$TMP/gate.log" 2>/dev/null)"
is "the change bead's lifecycle row is CERTIFIED" "CERTIFIED" "$(lcfix_state sp-ssland1)"
is "certified at the branch tip" "$TIP" "$(lcfix_tip sp-ssland1)"
[ ! -e "$QDIR/sp-ssland1" ] && ok "the queue holds no certification record of its own" \
    || bad "the queue holds no certification record of its own" "found $QDIR/sp-ssland1"
is "no bead-less spira-suite-state/* branch" "" "$(git -C "$REPO" for-each-ref --format='%(refname)' refs/heads/spira-suite-state/)"

echo
echo "the round's land-local lands it to LANDED, and reconcile-content closes it"
mk_bins "${TIP:-$BASE}"
out="$(env "$(lcfix_mark)" SPIRA_CONF=/nonexistent SPIRA_HOME="$SH" SPIRA_REPO="$REPO" \
    SPIRA_LAND_UNGATED="fixture: a hand-built head no gate judged" SPIRA_LAND_UNGATED_BY=operator \
    SPIRA_LAND_UNGATED_EVIDENCE=local/main \
    queue land-local fixq --head "${TIP:-none}" --members "sp-ssland1:${TIP:-none}" --worktree "$(bins_wt "${TIP:-$BASE}")" 2>&1)"; rc=$?
[ "$rc" -eq 0 ] && ok "the round lands the quarantine edit" || bad "the round lands the quarantine edit" "rc=$rc out=$out"
is "local/main is the edit" "$TIP" "$(localmain)"
is "the change bead's lifecycle row is LANDED" "LANDED" "$(lcfix_state sp-ssland1)"
lcfix_settle
rec="$(env "$(lcfix_mark)" spira-lc reconcile-content --apply sp-ssland1 2>&1)"; rrc=$?
[ "$rrc" -eq 0 ] && ok "reconcile-content settles with exit 0" || bad "reconcile-content settles with exit 0" "rc=$rrc out=$rec"
st="$(field sp-ssland1 status)"
[ "$st" = closed ] && ok "the change bead is closed from its LANDED record" \
    || bad "the change bead is closed from its LANDED record" "status=$st rc=$rrc out=$rec"
want "the landed lifecycle file quarantines the suite" "test-q.sh | quarantined" "$(git -C "$REPO" show local/main:spira/suite-state 2>/dev/null)"
want "...naming the defect bead, not the change bead" "| sp-defect6 |" "$(git -C "$REPO" show local/main:spira/suite-state 2>/dev/null)"
[ ! -e "$QDIR/sp-ssland1" ] && ok "still no queue record after the land" || bad "still no queue record after the land" "found $QDIR/sp-ssland1"

echo
echo "the store is replaced only once a marked process holding it has let it go"
# A marked process holding the store's lock, as the landing's detached rebase pass does, until
# the suite releases it: the reset waits it out instead of being refused "already open".
env "$(lcfix_mark)" flock "$LCFIX_DIR/control.db.lock" \
    sh -c 'while [ ! -e "$1" ]; do sleep 0.1; done' _ "$TMP/release" &
holder=$!
until [ -n "$(marked)" ]; do sleep 0.1; done
out="$(_lcfix_lc show sp-ssland1 2>&1)"; rc=$?
is "the held store is refused to another opener" 2 "$rc"
( sleep 1; : > "$TMP/release" ) &
lcfix_reset && ok "the store resets once the holder lets it go" || bad "the store resets once the holder lets it go" "lcfix_reset failed"
is "no marked process outlives the reset" "" "$(marked)"
wait "$holder" 2>/dev/null
lcfix_seed sp-after READY && is "the fresh store answers" READY "$(lcfix_state sp-after)"

tl_summary
