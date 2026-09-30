#!/usr/bin/env bash
#
# test-queue-publish.sh — sp-bc49w: the queue.local publish queue. `queue.sh publish`
# pushes local/main's commits since the forge target's own tip to one new forge branch and
# opens one PR; verdict.sh settles it separately from the queue.forge batch machinery: green
# fast-forwards the forge target to IDENTICAL SHAs with no land_mark and no bead close, red
# runs local attribution and files one fix-forward bead without reopening a member, and
# "nothing to publish" is a no-op. The record lives at queue/<repo>/publish, never
# queue/<repo>/open, so it can never be mistaken for a queue.forge batch in flight.
#
# REAL MAIL, NOT A STUB, for the divergence alarm (row 4): mail.sh runs for real so cases 5
# and 7 can assert the actual inbox line landed in $RUN/mail/concierge/new/*, naming the
# foreign commit — the same seam test-publish-backlog.sh uses for its own alarm assertions.
#
# tier: T1
# covers: queue/src/* spira/verdict.sh spira/lib.sh spira/conf.sh spira/attribute.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
# The queue binary (queue/DESIGN.md §7.4): the one conf.sh exports, else the tree under
# test's own build (testenv's SPIRA_ARTIFACTS), else this checkout's bin/.

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-queue-publish
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up qpublish || { echo "test-queue-publish: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

echo "test-queue-publish.sh"

SH="$TMP/spira"; mkdir -p "$SH"
cp -r "$HERE"/*.sh "$HERE"/*.py "$SH/" 2>/dev/null || true
cp -r "$HERE/mail" "$SH/mail" 2>/dev/null || true
chmod +x "$SH"/*.sh 2>/dev/null || true
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }

REMOTE="$TMP/remote.git"; REPO="$TMP/repo"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" branch local/main main

RUN="$TMP/run"; QDIR="$RUN/queue"; REPONAME=fixpub; RELEASES="$TMP/releases"
mkdir -p "$RUN/worktree" "$QDIR" "$RELEASES"
RMAP="$TMP/repo-map"
printf '%s | %s | queue.local | local/main | | |\n' "$REPONAME" "$REPO" > "$RMAP"

# Forge stub. CALL_LOG records every command asked of it (names only, plus pr-close's
# argument) so a test can prove the PR was actually closed, not merely that its status
# was read. FIXTURE_CHECK_STATUS/FIXTURE_RED_SUITES drive check-status's answer.
CALL_LOG="$TMP/call-log"; : > "$CALL_LOG"
PRSEQ="$TMP/pr-seq"
export CALL_LOG PRSEQ
cat > "$SH/forge-fixture.sh" <<'FORGE'
#!/usr/bin/env bash
cmd="${1:-}"; shift; shift  # drop the repo-dir arg every verb takes
case "$cmd" in
    pr-create)
        n=$(( $(cat "$PRSEQ" 2>/dev/null || echo 0) + 1 ))
        printf '%s\n' "$n" > "$PRSEQ"
        printf 'pr-create\n' >> "$CALL_LOG"
        printf '%s\n' "$n"
        ;;
    check-status)
        printf 'check-status\n' >> "$CALL_LOG"
        printf '%s\n' "${FIXTURE_CHECK_STATUS:-green}"
        if [ "${FIXTURE_CHECK_STATUS:-green}" = red ]; then
            for s in ${FIXTURE_RED_SUITES:-}; do printf 'red-suite: %s\n' "$s"; done
            printf 'run-url: %s\n' "${FIXTURE_RUN_URL:-https://ci.example.invalid/run/1}"
        fi
        ;;
    pr-close)
        printf 'pr-close %s\n' "$1" >> "$CALL_LOG"
        ;;
    *) printf '%s\n' "$cmd" >> "$CALL_LOG" ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"

queue() {
    SPIRA_CONF=/nonexistent \
    SPIRA_HOME="$SH" \
    SPIRA_HOME_REPO="$REPONAME" \
    SPIRA_REPO="$REPO" \
    SPIRA_RUN="$RUN" \
    SPIRA_QUEUE_DIR="$QDIR" \
    SPIRA_REPO_MAP="$RMAP" \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
    SPIRA_LAND_UNGATED="fixture: hand-built heads no gate judged (queue/DESIGN.md §8 D12)" \
    SPIRA_RELEASES="$RELEASES" \
        SPIRA_HOME="$SH" command queue "$@" 2>&1
}
verdict() {
    SPIRA_CONF=/nonexistent \
    SPIRA_HOME="$SH" \
    SPIRA_HOME_REPO="$REPONAME" \
    SPIRA_REPO="$REPO" \
    SPIRA_RUN="$RUN" \
    SPIRA_QUEUE_DIR="$QDIR" \
    SPIRA_REPO_MAP="$RMAP" \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
    FIXTURE_CHECK_STATUS="${CHECK_STATUS:-green}" \
    FIXTURE_RED_SUITES="${RED_SUITES:-}" \
        bash "$SH/verdict.sh" "$REPONAME" 2>&1
}
# mk_bins <head> — land-local now refuses without a --with-bins corpus for the tree it is
# landing; every head this suite lands needs one (see test-land-local-release.sh for the
# mechanism in depth).
mk_bins() {
    local head="$1" tree dir
    tree="$(git -C "$REPO" rev-parse "${head}^{tree}")"
    dir="$RUN/cargo-target-bins/$tree/release"
    mkdir -p "$dir"
    printf 'fake\n' > "$dir/fakebin"
    chmod +x "$dir/fakebin"
}

# land <id> <file> <content> — one round as the batcher builds it: the member's work commit,
# merged --no-ff onto local/main under the queue's own land subject ("spira: land <id>",
# queue/DESIGN.md §8 D4 — publish reads its members from those commits), then land-local'd.
land() {
    local id="$1" file="$2" content="$3"
    git -C "$REPO" checkout -qb "work-$id" local/main
    printf '%s\n' "$content" > "$REPO/$file"
    git -C "$REPO" add "$file"
    git -C "$REPO" commit -q -m "$id: the work"
    local tip; tip="$(git -C "$REPO" rev-parse "work-$id")"
    git -C "$REPO" checkout -qb "round-$id" local/main
    git -C "$REPO" merge -q --no-ff -m "spira: land $id" "work-$id"
    local head; head="$(git -C "$REPO" rev-parse "round-$id")"
    git -C "$REPO" checkout -q main
    git -C "$REPO" branch -D "round-$id" "work-$id" >/dev/null 2>&1
    mk_bins "$head"
    queue land-local "$REPONAME" --head "$head" --members "$id:$tip" >/dev/null
}

B() { bd -C "$SPIRA_DB" "$@"; }
field() { B show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
v=d[0].get(sys.argv[1])
print(",".join(v) if isinstance(v,list) else (v or ""))' "$2" 2>/dev/null; }
seed() {   # seed <id>
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":["plan","repo:%s","spira-submitted"],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "$REPONAME" | testdb_seed
}
remote_main()  { git -C "$REMOTE" rev-parse main 2>/dev/null; }
localmain()    { git -C "$REPO" rev-parse local/main; }
publish_file() { cat "$QDIR/$REPONAME/publish" 2>/dev/null; }
callcount()    { grep -c "^$1" "$CALL_LOG" 2>/dev/null; }
clear_calls()  { : > "$CALL_LOG"; }
landing_log()  { cat "$RUN/landing.log" 2>/dev/null; }
clear_log()    { : > "$RUN/landing.log"; }
# Scoped to the divergence alarm's own subject line: land-local's ordinary "local landing"
# notice goes through the same real mailbox once mail.sh is no longer stubbed, and would
# otherwise be indistinguishable from the alarm this suite is asserting on.
divergence_files() { grep -l '^Subject:.*divergence:' "$RUN"/mail/concierge/new/* 2>/dev/null; }
mail_count()   { divergence_files | wc -l | tr -d ' '; }
mail_body()    { divergence_files | xargs -r cat; }
clear_mail()   { rm -f "$RUN"/mail/concierge/new/* 2>/dev/null; }

# ============================================================================
echo
echo "1 — nothing to publish is a no-op: no branch, no PR, no record"
# ============================================================================
out="$(queue publish "$REPONAME")"; rc=$?
[ "$rc" -eq 0 ] && ok "1: exit 0 with nothing to publish" || bad "1: exit 0 with nothing to publish" "got rc=$rc out=$out"
want "1: reports nothing to publish" "nothing to publish" "$out"
[ "$(callcount pr-create)" -eq 0 ] && ok "1: forge never asked to open a PR" \
    || bad "1: forge never asked to open a PR" "pr-create calls: $(callcount pr-create)"
[ ! -f "$QDIR/$REPONAME/publish" ] && ok "1: no publish record written" \
    || bad "1: no publish record written" "got: $(publish_file)"

# ============================================================================
echo
echo "2 — a green publish fast-forwards the forge with an IDENTICAL sha; no land_mark, no bead close"
# ============================================================================
testdb_reset
seed sp-pub1
land sp-pub1 one.txt one
HEAD1="$(localmain)"

out="$(queue publish "$REPONAME")"; rc=$?
[ "$rc" -eq 0 ] && ok "2: publish opens a PR (exit 0)" || bad "2: publish opens a PR (exit 0)" "got rc=$rc out=$out"
want "2: names the PR" "PR 1 opened" "$out"
is "2: publish record's head is local/main's tip" "$HEAD1" "$(sed -n 's/^head=//p' "$QDIR/$REPONAME/publish")"
want "2: publish record names sp-pub1 as a member" "sp-pub1:$HEAD1" "$(publish_file)"
nowant "2: production (the forge's main) has not moved yet" "$HEAD1" "$(git -C "$REMOTE" rev-parse main)"

out="$(CHECK_STATUS=green verdict)"; rc=$?
[ "$rc" -eq 0 ] && ok "2: verdict settles the green publish" || bad "2: verdict settles the green publish" "got rc=$rc out=$out"
want "2: names the fast-forward" "fast-forwarded" "$out"
is "2: the forge's main equals local/main's tip EXACTLY (identical sha)" "$HEAD1" "$(remote_main)"
[ "$(callcount pr-close)" -eq 1 ] && ok "2: the PR was closed" || bad "2: the PR was closed" "$(cat "$CALL_LOG")"
[ ! -f "$QDIR/$REPONAME/publish" ] && ok "2: the publish record is retired" \
    || bad "2: the publish record is retired" "got: $(publish_file)"
is "2: the member bead is STILL closed from land-local (no re-close, no reopen)" \
    closed "$(field sp-pub1 status)"
want "2: close reason still cites the LOCAL landing, not a publish" "OUTCOME: landed" "$(field sp-pub1 close_reason)"
want "2: landing.log records the publish green" "QUEUE PUBLISH_GREEN" "$(landing_log)"

# ============================================================================
echo
echo "3 — a second publish after a green settle only carries what landed since"
# ============================================================================
clear_calls
seed sp-pub2
land sp-pub2 two.txt two
HEAD2="$(localmain)"

out="$(queue publish "$REPONAME")"; rc=$?
[ "$rc" -eq 0 ] && ok "3: second publish opens a new PR" || bad "3: second publish opens a new PR" "got rc=$rc out=$out"
want "3: publish record carries only sp-pub2, not sp-pub1 again" "sp-pub2:$HEAD2" "$(publish_file)"
nowant "3: sp-pub1 is not re-published" "sp-pub1" "$(publish_file)"
is "3: publish record's base is the just-published sha" "$HEAD1" "$(sed -n 's/^base=//p' "$QDIR/$REPONAME/publish")"

CHECK_STATUS=green verdict >/dev/null
is "3: the forge's main advances to the second head" "$HEAD2" "$(remote_main)"

# ============================================================================
echo
echo "4 — a red publish files one fix-forward bead; production untouched, no member reopened"
# ============================================================================
clear_calls
seed sp-pub3
land sp-pub3 three.txt three
HEAD3="$(localmain)"
PRE_MAIN="$(remote_main)"

queue publish "$REPONAME" >/dev/null

out="$(CHECK_STATUS=red RED_SUITES="test-example-suite.sh" verdict)"; rc=$?
[ "$rc" -eq 0 ] && ok "4: verdict settles the red publish" || bad "4: verdict settles the red publish" "got rc=$rc out=$out"
want "4: names the fix-forward filing" "filed fix-forward" "$out"
is "4: production (the forge's main) is untouched by a red publish" "$PRE_MAIN" "$(remote_main)"
is "4: the member bead is STILL closed — never reopened" closed "$(field sp-pub3 status)"
[ "$(callcount pr-close)" -eq 1 ] && ok "4: the red PR was closed" || bad "4: the red PR was closed" "$(cat "$CALL_LOG")"
[ ! -f "$QDIR/$REPONAME/publish" ] && ok "4: the publish record is retired" \
    || bad "4: the publish record is retired" "got: $(publish_file)"
want "4: landing.log records the publish red" "QUEUE PUBLISH_RED" "$(landing_log)"

_fwid="$(sed -n 's/.*fix_forward=\(sp-[a-zA-Z0-9]*\).*/\1/p' "$RUN/landing.log" | tail -1)"
[ -n "$_fwid" ] && ok "4: a fix-forward bead id was recorded" || bad "4: a fix-forward bead id was recorded" "$(landing_log)"
if [ -n "$_fwid" ]; then
    want "4: the fix-forward bead names the red PR" "PR 3" "$(field "$_fwid" description)"
    want "4: the fix-forward bead names sp-pub3" "sp-pub3" "$(field "$_fwid" description)"
    is "4: the fix-forward bead is open, distinct work" open "$(field "$_fwid" status)"
fi

# ============================================================================
echo
echo "4b — row 4: healthy publishes never raise the divergence alarm (positive control for 5)"
# ============================================================================
is "4b: no divergence mail through four healthy/red publishes" "0" "$(mail_count)"

# ============================================================================
echo
echo "5 — origin/main not an ancestor of local/main: publish refuses, alarms the concierge once, nothing changes"
# ============================================================================
clear_calls
# Simulate a foreign write straight to the forge's main, bypassing the publish queue.
CLONE="$TMP/clone"
git clone -q "$REMOTE" "$CLONE"
git -C "$CLONE" commit -q --allow-empty -m "foreign: not from local/main"
git -C "$CLONE" push -q origin main
FOREIGN="$(git -C "$CLONE" rev-parse main)"

out="$(queue publish "$REPONAME")"; rc=$?
[ "$rc" -ne 0 ] && ok "5: publish refuses when the forge diverged" || bad "5: publish refuses when the forge diverged" "got rc=$rc out=$out"
want "5: names the refusal" "not an ancestor" "$out"
[ "$(callcount pr-create)" -eq 0 ] && ok "5: no PR was opened" || bad "5: no PR was opened" "$(cat "$CALL_LOG")"
[ ! -f "$QDIR/$REPONAME/publish" ] && ok "5: no publish record written" \
    || bad "5: no publish record written" "got: $(publish_file)"
is "5: the forge's main is unchanged (still the foreign commit)" "$FOREIGN" "$(remote_main)"
is "5: the divergence alarm fired exactly once" "1" "$(mail_count)"
want "5: the alarm names the foreign commit" "foreign: not from local/main" "$(mail_body)"
want "5: the alarm says never to rebase silently" "Never rebase silently" "$(mail_body)"

out="$(queue publish "$REPONAME")"; rc=$?
[ "$rc" -ne 0 ] && ok "5b: a second publish attempt on the same divergence still refuses" \
    || bad "5b: a second publish attempt on the same divergence still refuses" "got rc=$rc out=$out"
is "5b: the repeated refusal does not re-alarm (one alarm per divergence)" "1" "$(mail_count)"

# ============================================================================
echo
echo "6 — a publish PR never blocks a local cut (land-local)"
# ============================================================================
# Restore the forge to what local/main actually descends from (undo the foreign write),
# then open a fresh publish and prove land-local still succeeds while it is open.
git -C "$REMOTE" update-ref refs/heads/main "$HEAD3" >/dev/null 2>&1
clear_calls
seed sp-pub4
land sp-pub4 four.txt four
[ -f "$QDIR/$REPONAME/publish" ] && bad "6 setup: no stray publish record before this case" "found one" \
    || true
queue publish "$REPONAME" >/dev/null
[ -f "$QDIR/$REPONAME/publish" ] && ok "6: a publish PR is open going into the local cut" \
    || bad "6: a publish PR is open going into the local cut" "no record at $QDIR/$REPONAME/publish"

seed sp-pub5
git -C "$REPO" checkout -qb round-sp-pub5 local/main
printf 'five\n' > "$REPO/five.txt"
git -C "$REPO" add five.txt
git -C "$REPO" commit -q -m "sp-pub5: the work"
HEAD5="$(git -C "$REPO" rev-parse round-sp-pub5)"
git -C "$REPO" checkout -q main
git -C "$REPO" branch -D round-sp-pub5 >/dev/null 2>&1
mk_bins "$HEAD5"

out="$(queue land-local "$REPONAME" --head "$HEAD5" --members "sp-pub5:$HEAD5")"; rc=$?
[ "$rc" -eq 0 ] && ok "6: land-local succeeds with a publish PR open" \
    || bad "6: land-local succeeds with a publish PR open" "got rc=$rc out=$out"
is "6: local/main advanced despite the open publish" "$HEAD5" "$(localmain)"
is "6: the bead landed locally" closed "$(field sp-pub5 status)"
is "6: the forge recovering did not raise a fresh alarm" "1" "$(mail_count)"

# ============================================================================
echo
echo "7 — land-local's own pre-round-build check (row 4) alarms once on a cached divergence, and never blocks the round"
# ============================================================================
clear_calls
clear_mail
# A second foreign write straight to the forge — discovered here only by an explicit fetch,
# never one land-local triggers itself (no forge round trip belongs on the round-build
# critical path). This proves land-local's own check, independent of any publish attempt.
CLONE2="$TMP/clone2"
git clone -q "$REMOTE" "$CLONE2"
git -C "$CLONE2" commit -q --allow-empty -m "foreign2: bypassed the publish queue again"
git -C "$CLONE2" push -q origin main
git -C "$REPO" fetch -q origin main

seed sp-pub6
git -C "$REPO" checkout -qb round-sp-pub6 local/main
printf 'six\n' > "$REPO/six.txt"
git -C "$REPO" add six.txt
git -C "$REPO" commit -q -m "sp-pub6: the work"
HEAD6="$(git -C "$REPO" rev-parse round-sp-pub6)"
git -C "$REPO" checkout -q main
git -C "$REPO" branch -D round-sp-pub6 >/dev/null 2>&1

mk_bins "$HEAD6"
out="$(queue land-local "$REPONAME" --head "$HEAD6" --members "sp-pub6:$HEAD6")"; rc=$?
[ "$rc" -eq 0 ] && ok "7: the round build itself is never blocked by the divergence" \
    || bad "7: the round build itself is never blocked by the divergence" "got rc=$rc out=$out"
is "7: local/main still advances" "$HEAD6" "$(localmain)"
is "7: land-local's own check alarmed exactly once" "1" "$(mail_count)"
want "7: the alarm names the second foreign commit" "foreign2: bypassed the publish queue again" "$(mail_body)"

seed sp-pub7
git -C "$REPO" checkout -qb round-sp-pub7 local/main
printf 'seven\n' > "$REPO/seven.txt"
git -C "$REPO" add seven.txt
git -C "$REPO" commit -q -m "sp-pub7: the work"
HEAD7="$(git -C "$REPO" rev-parse round-sp-pub7)"
git -C "$REPO" checkout -q main
git -C "$REPO" branch -D round-sp-pub7 >/dev/null 2>&1

mk_bins "$HEAD7"
out="$(queue land-local "$REPONAME" --head "$HEAD7" --members "sp-pub7:$HEAD7")"; rc=$?
[ "$rc" -eq 0 ] && ok "7b: a second round build on the same divergence still succeeds" \
    || bad "7b: a second round build on the same divergence still succeeds" "got rc=$rc out=$out"
is "7b: a second round build does not re-alarm (one alarm per divergence)" "1" "$(mail_count)"

tl_summary
