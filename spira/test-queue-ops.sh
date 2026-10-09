#!/usr/bin/env bash
# test-queue-ops.sh — queue.sh eject and abandon. Merged from test-queue-eject.sh and
# test-queue-abandon.sh: both built the same REPO/RUN/SH/forge-fake/repo-map scaffolding
# independently. (batch.sh's own automatic DIRTY-PR abandon path — the trigger this suite
# once also carried, folded in from test-batch-conflicting-pr.sh — is retired with
# batch.sh, sp-uwhx0: no repo runs in `land=queue` mode, and batcher-cut owns the round.)
#
# MOST CASES USE A RECORDING BD STUB, not a fixture database: eject and abandon's own
# code paths call bd only to comment/assign, plus reopen for the CERTIFIED-but-unbatched
# case. Delivery state is read and written only through spira-lc, which is real here: a
# private lifecycle database (testlib/lc-fixture.sh) whose rows the cases seed and read
# back. One case near the end uses a real bd (testdb.sh) to verify what the stub cannot:
# that the certified-unbatched reopen actually lands (status, assignee, comment body) and
# that abandon's return-to-CERTIFIED path makes NO bd call at all.
#
# tier: T2
# covers: queue/src/* forge/src/* spira/conf.sh UC-landing-merge-queue-29
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/testlib/lc-fixture.sh"

echo "test-queue-ops.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-queue-ops

TMP="$(mktemp -d)"; trap 'lcfix_down; testdb_drop; rm -rf "$TMP"' EXIT INT TERM
lcfix_up || { echo "test-queue-ops: could not build a lifecycle fixture"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"
RUN="$TMP/run"
SH="$TMP/spira"
REPONAME=fixq
QUEUEDIR="$RUN/queue"
FORGE_LOG="$TMP/forge.log"
RUNS_FILE="$TMP/runs-for-branch"
CANCEL_FAIL="$TMP/cancel-fail"
BD_LOG="$TMP/bd-calls.log"

git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m init
mkdir -p "$RUN/worktree" "$SH" "$QUEUEDIR/$REPONAME"
cp "$HERE"/*.sh "$HERE"/*.py "$SH/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SH/"

# Fake forge: records arguments, succeeds silently. runs-for-branch answers from
# RUNS_FILE (empty/absent = no in-flight runs); run-cancel fails when CANCEL_FAIL
# exists, so the retry/loud-failure path can be exercised.
cat > "$SH/forge-fake.sh" <<ENDFAKE
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "\$FORGE_LOG"
case "\${1:-}" in
    runs-for-branch) [ -f "\$RUNS_FILE" ] && cat "\$RUNS_FILE"; exit 0 ;;
    run-cancel)      [ -f "\$CANCEL_FAIL" ] && exit 1; exit 0 ;;
esac
exit 0
ENDFAKE
chmod +x "$SH/forge-fake.sh"

# Recording bd stub: logs every call, answers `show` positively for any id so the
# eject dry-run's existence guard passes, and no-ops everything else. Good enough for
# every case that never reads bd state back — which is every case except the real-bd
# section below.
cat > "$SH/bd-stub.sh" <<'BDSTUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${BD_LOG:?}"
# A comment body arrives on stdin (`comment <id> --stdin`): record it too, so a case can
# assert what the comment says, not only that one was posted.
case " $* " in *" --stdin "*) cat >> "$BD_LOG"; printf '\n' >> "$BD_LOG" ;; esac
# The queue binary reads `bd -C <db> show <id> --json` rows (an exit 0 with no row is
# "cannot resolve"), so show answers one JSON row per id asked for.
[ "${1:-}" = -C ] && shift 2
case "${1:-}" in
    show) shift; sep='['; for a in "$@"; do case "$a" in -*) ;; *) printf '%s{"id":"%s"}' "$sep" "$a"; sep=',' ;; esac; done; printf ']\n'; exit 0 ;;
    *)    exit 0 ;;
esac
BDSTUB
chmod +x "$SH/bd-stub.sh"

LC_DOWN="SPIRA_LC_HOST=127.0.0.1 SPIRA_LC_PORT=1 SPIRA_LC_DB=spira_lifecycle SPIRA_LC_USER=root"
RMAP="$TMP/repo-map"
printf '%s | %s | queue | main | | |\n' "$REPONAME" "$REPO" > "$RMAP"

run() {
    # SPIRA_RUN/SPIRA_DB/SPIRA_BD/SPIRA_HOME_REPO/SPIRA_REPO_MAP/SPIRA_QUEUE_DIR/
    # SPIRA_FORGE are registered keys (per Ryan 2026-10-05, ONE SOURCE OF CONFIG): declare
    # via tl_config and thread SPIRA_TOML through env -i, which clears it.
    tl_config SPIRA_RUN="$RUN" SPIRA_DB="${SPIRA_DB:-/nonexistent}" SPIRA_BD="$SH/bd-stub.sh" \
        SPIRA_HOME_REPO="$REPONAME" SPIRA_REPO_MAP="$RMAP" SPIRA_QUEUE_DIR="$QUEUEDIR" \
        SPIRA_FORGE="$SH/forge-fake.sh"
    env -i ${LCENV:-$(lcfix_env)} PATH="$SH:$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$SH" \
        BD_LOG="$BD_LOG" \
        FORGE_LOG="$FORGE_LOG" \
        RUNS_FILE="$RUNS_FILE" \
        CANCEL_FAIL="$CANCEL_FAIL" \
        BEADS_ACTOR="aeon-abandontest" \
        SPIRA_EVENT_COOLDOWN=0 \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_HOME="$SH" queue "$@" 2>&1
}

TIP01="aabbcc1100000000000000000000000000000001"
TIP02="ddeeff2200000000000000000000000000000002"
TIP03="eeff003300000000000000000000000000000003"

write_eject_batch() {
    {
        printf 'pr=42\n'
        printf 'head=ffff0000000000000000000000000000000000ff\n'
        printf 'base=0000000000000000000000000000000000000000\n'
        printf 'members=sp-ej01:%s sp-ej02:%s\n' "$TIP01" "$TIP02"
        printf 'opened=%s\n' "$(date +%s)"
        printf 'branch=spira/queue/20260917T120000Z\n'
    } > "$OPEN_FILE"
}

BATCH_SEQ=0
BATCH_ID=""
lc_reset() { local t; for t in event delivery batch_member batch bead; do lcfix_sql -q "DELETE FROM $t" >/dev/null 2>&1; done; }
batch_state() { lcfix_sql -q "SELECT state FROM batch WHERE batch_id='$1'" -r csv 2>/dev/null | sed -n 2p; }
seed_eject() {
    lcfix_seed sp-ej01 IN_DELIVERY "$TIP01"
    lcfix_seed sp-ej02 IN_DELIVERY "$TIP02"
}
seed_abandon() {
    lcfix_seed sp-ab01 IN_DELIVERY "$TIP01"
    lcfix_seed sp-ab02 IN_DELIVERY "$TIP02"
    lcfix_seed sp-ab03 IN_DELIVERY "$TIP03"
}

# write_abandon_batch [cut] — the open record; with "cut" the three members are CERTIFIED
# rows cut into a real batch on spira-lc and the record names it.
write_abandon_batch() {
    local extra=""
    if [ "${1:-}" = cut ]; then
        BATCH_SEQ=$((BATCH_SEQ + 1)); BATCH_ID="$REPONAME-20260917T13000${BATCH_SEQ}Z"
        lc_reset
        lcfix_seed sp-ab01 CERTIFIED "$TIP01"
        lcfix_seed sp-ab02 CERTIFIED "$TIP02"
        lcfix_seed sp-ab03 CERTIFIED "$TIP03"
        spira-lc cut "$BATCH_ID" --repo "$REPONAME" --head ffff0000000000000000000000000000000000ff \
            --base 0000000000000000000000000000000000000000 \
            --members "sp-ab01:$TIP01,sp-ab02:$TIP02,sp-ab03:$TIP03" --actor fixture >/dev/null 2>&1
        extra="batch_id=$BATCH_ID"
    fi
    {
        printf 'pr=58\n'
        printf 'head=ffff0000000000000000000000000000000000ff\n'
        printf 'base=0000000000000000000000000000000000000000\n'
        printf 'members=sp-ab01:%s sp-ab02:%s sp-ab03:%s\n' "$TIP01" "$TIP02" "$TIP03"
        printf 'opened=%s\n' "$(date +%s)"
        printf 'branch=spira/queue/20260917T130000Z\n'
        [ -n "$extra" ] && printf '%s\n' "$extra"
    } > "$OPEN_FILE"
}

OPEN_FILE="$QUEUEDIR/$REPONAME/open"

# =============================================================================
# EJECT — recording-bd stub, real spira-lc (UC-29).
# =============================================================================

echo
echo "eject: positive control — eject finds a real member (guard is live):"
write_eject_batch
seed_eject
> "$FORGE_LOG"; > "$BD_LOG"
out="$(run eject sp-ej01 --reason 'test-foo.sh RED')"; rc=$?
[ "$rc" -eq 0 ] && ok "exit 0 for valid member" || bad "exit 0 for valid member" "rc=$rc out=$out"
want "reports ejection" "ejected sp-ej01" "$out"
is   "the ejected bead is returned to REWORK on spira-lc" "REWORK" "$(lcfix_state sp-ej01)"
is   "the other member is untouched" "IN_DELIVERY" "$(lcfix_state sp-ej02)"
nowant "bd reopen is not called for an in-batch eject" "reopen sp-ej01" "$(cat "$BD_LOG")"

echo
echo "eject: non-member is refused; batch and lifecycle unchanged:"
write_eject_batch
seed_eject
out="$(run eject sp-nonexist)"; rc=$?
[ "$rc" -ne 0 ] && ok "exits non-zero for non-member" || bad "exits non-zero" "rc=$rc"
want "names the id" "sp-nonexist is not a member" "$out"
want "lists batch members" "members:" "$out"
members_now="$(grep '^members=' "$OPEN_FILE" | head -1)"
[[ "$members_now" == *"sp-ej01:"* ]] && ok "sp-ej01 still in batch" || bad "batch unchanged" "$members_now"
[[ "$members_now" == *"sp-ej02:"* ]] && ok "sp-ej02 still in batch" || bad "batch unchanged" "$members_now"
is "no lifecycle row written for the non-member" "" "$(lcfix_state sp-nonexist)"
is "sp-ej01 is still IN_DELIVERY" "IN_DELIVERY" "$(lcfix_state sp-ej01)"

echo
echo "eject: CERTIFIED, unbatched bead is withdrawn — no open batch exists at all:"
rm -f "$OPEN_FILE"
lcfix_seed sp-ej-cert CERTIFIED "$TIP03"
> "$BD_LOG"
out="$(run eject sp-ej-cert --reason 'holding for a fix')"; rc=$?
[ "$rc" -eq 0 ] && ok "exit 0 for certified, unbatched bead" || bad "exit 0" "rc=$rc out=$out"
want "reports the certified-unbatched case" "certified, not yet batched" "$out"
is   "the withdrawn bead is returned to REWORK on spira-lc" "REWORK" "$(lcfix_state sp-ej-cert)"
# The reopen door (sp-swh8b8) moves the row, then reopens the store: bd sees an update, never a raw reopen.
want "the store is reopened after the row" "update sp-ej-cert --status open" "$(cat "$BD_LOG")"

echo
echo "eject: --suites reaches the bead's comment, and the bead is returned to REWORK:"
lcfix_seed sp-ej-suites CERTIFIED "$TIP03"
> "$BD_LOG"
out="$(run eject sp-ej-suites --reason 'suite reds' --suites 'test-x.sh,test-y.sh')"; rc=$?
[ "$rc" -eq 0 ] && ok "exit 0 with --suites" || bad "exit 0 with --suites" "rc=$rc out=$out"
is   "REWORK on spira-lc with --suites" "REWORK" "$(lcfix_state sp-ej-suites)"
want "the reopen is recorded as a judged eject" "eject-red" "$(lcfix_fact_causes sp-ej-suites reopen)"
want "the comment names the suites recertification must force" \
    "Recertification will force these suites regardless of SPIRA_CERTIFY_SUITES: test-x.sh,test-y.sh" "$(cat "$BD_LOG")"

echo
echo "eject: CERTIFIED, unbatched bead — an unrelated open batch does not block it:"
write_eject_batch
seed_eject
lcfix_seed sp-ej-cert2 CERTIFIED "$TIP03"
out="$(run eject sp-ej-cert2)"; rc=$?
[ "$rc" -eq 0 ] && ok "exit 0 while an unrelated batch is open" || bad "exit 0" "rc=$rc out=$out"
is "the withdrawn bead is REWORK, the unrelated batch's members untouched" \
    "REWORK IN_DELIVERY IN_DELIVERY" "$(lcfix_state sp-ej-cert2) $(lcfix_state sp-ej01) $(lcfix_state sp-ej02)"
members_now="$(grep '^members=' "$OPEN_FILE" | head -1)"
[[ "$members_now" == *"sp-ej01:"* && "$members_now" == *"sp-ej02:"* ]] \
    && ok "unrelated open batch members unchanged" || bad "unrelated batch unchanged" "$members_now"

echo
echo "eject: dry-run for a CERTIFIED, unbatched bead prints plan and changes nothing:"
rm -f "$OPEN_FILE"
lcfix_seed sp-ej-cert3 CERTIFIED "$TIP03"
> "$BD_LOG"
out="$(run eject sp-ej-cert3 --dry-run)"; rc=$?
[ "$rc" -eq 0 ] && ok "dry-run exits 0 for certified, unbatched bead" || bad "dry-run exit 0" "rc=$rc"
want "dry-run mentions the lifecycle return" "return it to REWORK on spira-lc" "$out"
want "dry-run mentions bead reopen"     "would reopen bead sp-ej-cert3" "$out"
is "dry-run did not change the lifecycle row" "CERTIFIED" "$(lcfix_state sp-ej-cert3)"
nowant "dry-run: bd reopen not called" "reopen sp-ej-cert3" "$(cat "$BD_LOG")"

echo
echo "eject: dry-run prints plan; non-member exits non-zero:"
write_eject_batch
out="$(run eject sp-nonexist --dry-run)"; rc=$?
[ "$rc" -ne 0 ] && ok "dry-run non-member exits non-zero" || bad "dry-run non-member" "rc=$rc"

echo
echo "eject: lock held: refuses and changes nothing:"
write_eject_batch
seed_eject
lockfile="$QUEUEDIR/$REPONAME/lock"
exec 8>"$lockfile"
flock 8
out="$(run eject sp-ej01)"; rc=$?
exec 8>&-
[ "$rc" -ne 0 ] && ok "exit non-zero when lock held" || bad "exit non-zero" "rc=$rc"
want "mentions lock" "holds the lock" "$out"
[ -f "$OPEN_FILE" ] && ok "open file unchanged" || bad "open file unchanged" "file gone"
is "lifecycle row unchanged when lock held" "IN_DELIVERY" "$(lcfix_state sp-ej01)"

echo
echo "eject: single-member batch closes PR and removes batch:"
{
    printf 'pr=55\n'
    printf 'head=ffff0000000000000000000000000000000000ff\n'
    printf 'base=0000000000000000000000000000000000000000\n'
    printf 'members=sp-ej01:%s\n' "$TIP01"
    printf 'opened=%s\n' "$(date +%s)"
    printf 'branch=spira/queue/20260917T130000Z\n'
} > "$OPEN_FILE"
seed_eject
> "$FORGE_LOG"
out="$(run eject sp-ej01)"; rc=$?
[ "$rc" -eq 0 ] && ok "single-member: exit 0" || bad "single-member: exit 0" "rc=$rc out=$out"
is "single-member: REWORK on spira-lc" "REWORK" "$(lcfix_state sp-ej01)"
want "single-member: forge pr-close called" "pr-close" "$(cat "$FORGE_LOG")"
[ ! -f "$OPEN_FILE" ] && ok "single-member: batch removed" || bad "single-member: batch removed" "file exists"

echo
echo "eject: dry-run for a member prints plan and changes nothing:"
write_eject_batch
seed_eject
> "$FORGE_LOG"
out="$(run eject sp-ej01 --dry-run)"; rc=$?
[ "$rc" -eq 0 ] && ok "dry-run exits 0 for member" || bad "dry-run exits 0" "rc=$rc"
want "dry-run mentions the cause"          "would record cause"        "$out"
want "dry-run mentions the lifecycle return" "would return bead sp-ej01 to spira-lc" "$out"
want "dry-run mentions PR close"           "would close PR 42"         "$out"
want "dry-run mentions survivor CERTIFIED" "would return survivors"    "$out"
is "dry-run did not change the lifecycle row" "IN_DELIVERY" "$(lcfix_state sp-ej01)"
[ -f "$OPEN_FILE" ] && ok "dry-run did not remove batch" || bad "dry-run no change" "batch gone"
[ -z "$(cat "$FORGE_LOG")" ] && ok "dry-run: forge not called" || bad "dry-run no forge" "got $(cat "$FORGE_LOG")"

echo
echo "eject: spira-lc unreachable refuses before anything changes:"
write_eject_batch
seed_eject
out="$(LCENV="$LC_DOWN" run eject sp-ej01)"; rc=$?
[ "$rc" -ne 0 ] && ok "eject refuses when spira-lc is unreachable" || bad "eject refuses when spira-lc is unreachable" "rc=$rc out=$out"
want "names spira-lc" "spira-lc is unreachable" "$out"
[ -f "$OPEN_FILE" ] && ok "unreachable: open record left in place" || bad "unreachable: open record left in place" "file gone"
is "unreachable: the lifecycle row is unchanged" "IN_DELIVERY" "$(lcfix_state sp-ej01)"

# =============================================================================
# ABANDON — recording-bd stub, real spira-lc (UC-30).
# =============================================================================

echo
echo "abandon: --reason is required — refuses, changes no lifecycle row, leaves the open record in place:"
write_abandon_batch
seed_abandon
> "$FORGE_LOG"
: > "$RUN/landing.log"
out="$(run abandon $REPONAME)"; rc=$?
[ "$rc" -ne 0 ] && ok "refuses without --reason" || bad "refuses without --reason" "rc=$rc out=$out"
want "the refusal names the flag" "--reason" "$out"
[ -f "$OPEN_FILE" ] && ok "no --reason: open record left in place" || bad "open record left in place" "file gone"
is "no --reason: sp-ab01 still IN_DELIVERY (positive control)" "IN_DELIVERY" "$(lcfix_state sp-ab01)"
[ -z "$(cat "$FORGE_LOG")" ] && ok "no --reason: forge not called" || bad "no --reason: forge untouched" "got $(cat "$FORGE_LOG")"
nowant "no --reason: no audit line written" "QUEUE ABANDON" "$(cat "$RUN/landing.log" 2>/dev/null || true)"

echo
echo "abandon: positive control — no-open-batch is detected (guard is live):"
rm -f "$OPEN_FILE"
out="$(run abandon $REPONAME --reason 'checking the no-open-batch guard')"; rc=$?
[ "$rc" -ne 0 ] && ok "exits non-zero when no open batch" || bad "exits non-zero" "rc=$rc"
want "message says no open batch" "no open batch for $REPONAME" "$out"

echo
echo "abandon: lock held: refuses and changes nothing:"
write_abandon_batch cut
lockfile="$QUEUEDIR/$REPONAME/lock"
exec 8>"$lockfile"
flock 8
out="$(run abandon $REPONAME --reason 'checking the lock guard')"; rc=$?
exec 8>&-
[ "$rc" -ne 0 ] && ok "exit non-zero when lock held" || bad "exit non-zero" "rc=$rc"
want "mentions lock" "holds the lock" "$out"
[ -f "$OPEN_FILE" ] && ok "open file unchanged" || bad "open file unchanged" "file gone"
is "lock held: the batch is still not abandoned on spira-lc" "OPEN" "$(batch_state "$BATCH_ID")"

echo
echo "abandon: PR closed, innocent members back to CERTIFIED on spira-lc, a REWORK member left alone:"
write_abandon_batch cut
lcfix_seed sp-ab03 REWORK "$TIP03"
> "$FORGE_LOG"
printf '55501 in_progress\n' > "$RUNS_FILE"
: > "$RUN/landing.log"
: > "$RUN/events.log"

out="$(run abandon $REPONAME --reason 'guilty branch found')"; rc=$?
[ "$rc" -eq 0 ] && ok "exit 0" || bad "exit 0" "rc=$rc out=$out"

forge_calls="$(cat "$FORGE_LOG" 2>/dev/null || true)"
want "forge pr-comment called"   "pr-comment" "$forge_calls"
want "pr-comment mentions PR 58" "58"          "$forge_calls"
want "forge pr-close called"     "pr-close"    "$forge_calls"
want "forge queried runs-for-branch on the batch branch" \
    "runs-for-branch $REPO spira/queue/20260917T130000Z" "$forge_calls"
want "forge cancelled the in-flight run" "run-cancel $REPO 55501" "$forge_calls"
cancel_line="$(grep -n 'run-cancel' <<< "$forge_calls" | head -1 | cut -d: -f1)"
close_line="$(grep -n 'pr-close'   <<< "$forge_calls" | head -1 | cut -d: -f1)"
if [ -n "$cancel_line" ] && [ -n "$close_line" ] && [ "$cancel_line" -lt "$close_line" ]; then
    ok "run cancel happens before pr-close"
else
    bad "run cancel happens before pr-close" "cancel@$cancel_line close@$close_line"
fi
want "the cancel is recorded in landing.log" \
    "RUN_CANCEL " "$(cat "$RUN/landing.log" 2>/dev/null || true)"
rm -f "$RUNS_FILE"

is "the batch is ABANDONED on spira-lc" "ABANDONED" "$(batch_state "$BATCH_ID")"
is "sp-ab01 is CERTIFIED on spira-lc" "CERTIFIED" "$(lcfix_state sp-ab01)"
is "sp-ab01 tip preserved" "$TIP01" "$(lcfix_tip sp-ab01)"
is "sp-ab02 is CERTIFIED on spira-lc" "CERTIFIED" "$(lcfix_state sp-ab02)"
is "sp-ab03 left as REWORK" "REWORK" "$(lcfix_state sp-ab03)"

[ ! -f "$OPEN_FILE" ] && ok "open record archived (not open)" || bad "open record gone" "still exists"
archive="$(ls "$QUEUEDIR/$REPONAME/closed-pr58-"* 2>/dev/null | head -1)"
[ -n "$archive" ] && ok "archive exists" || bad "archive exists" "no closed-pr58-* found"
[[ "$archive" == *Z ]] && ok "archive name ends with Z" || bad "archive name Z suffix" "got $archive"
want "reason in forge call" "guilty branch found" "$forge_calls"

audit_count="$(grep -c '^QUEUE ABANDON ' "$RUN/landing.log" 2>/dev/null || echo 0)"
[ "${audit_count:-0}" -eq 1 ] && ok "exactly one QUEUE ABANDON audit line" \
    || bad "exactly one audit line" "count=$audit_count"
audit_line="$(grep '^QUEUE ABANDON ' "$RUN/landing.log" | head -1)"
want "audit line names the actor"                            "actor=aeon-abandontest"  "$audit_line"
want "audit line names the PR"                                "pr=58"                   "$audit_line"
want "audit line names a present member (positive control)"   "sp-ab01:CERTIFIED"       "$audit_line"
want "audit line names the REWORK member's disposition"       "sp-ab03:REWORK"          "$audit_line"
want "audit line names the reason"                            "guilty branch found"     "$audit_line"

events_out="$(cat "$RUN/events.log" 2>/dev/null || true)"
want "an event was emitted for the abandon"      "kind: queue.abandoned"    "$events_out"
want "the event names the actor"                      "actor=aeon-abandontest"   "$events_out"
want "the event names a present member (positive control)" "sp-ab01:CERTIFIED"  "$events_out"

archive_body="$(cat "$archive" 2>/dev/null || true)"
want "the archived record keeps the reason" "reason=guilty branch found" "$archive_body"
want "the archived record keeps the actor"  "actor=aeon-abandontest"     "$archive_body"

rm -f "$archive"

echo
echo "abandon: dry-run prints plan and changes nothing:"
write_abandon_batch cut
lcfix_seed sp-ab03 REWORK "$TIP03"
> "$FORGE_LOG"
: > "$RUN/landing.log"
: > "$RUN/events.log"
out="$(run abandon $REPONAME --dry-run --reason 'dry-run preview check')"; rc=$?
[ "$rc" -eq 0 ] && ok "dry-run exits 0" || bad "dry-run exits 0" "rc=$rc"
want "dry-run mentions PR"             "would close PR 58"   "$out"
want "dry-run innocent member"         "return to CERTIFIED" "$out"
want "dry-run REWORK member"           "leave alone"          "$out"
want "dry-run mentions the run cancel" "would cancel"         "$out"
want "dry-run shows archive path"      "archive path"         "$out"
want "dry-run prints the audit line it would write" \
    "dry-run: audit line (landing.log): QUEUE ABANDON" "$out"
want "dry-run audit preview names the actor"  "actor=aeon-abandontest"     "$out"
want "dry-run audit preview names a member"   "sp-ab01:CERTIFIED"          "$out"
want "dry-run audit preview names the reason" "dry-run preview check"      "$out"
[ -f "$OPEN_FILE" ] && ok "dry-run: open file unchanged" || bad "dry-run no change" "open file gone"
[ -z "$(cat "$FORGE_LOG")" ] && ok "dry-run: forge not called" || bad "dry-run no forge" "got $(cat "$FORGE_LOG")"
is "dry-run: sp-ab01 still IN_DELIVERY" "IN_DELIVERY" "$(lcfix_state sp-ab01)"
is "dry-run: the batch is still not abandoned" "OPEN" "$(batch_state "$BATCH_ID")"
nowant "dry-run: no audit line written to landing.log" "QUEUE ABANDON" "$(cat "$RUN/landing.log" 2>/dev/null || true)"
nowant "dry-run: no event written to events.log" "queue.abandoned" "$(cat "$RUN/events.log" 2>/dev/null || true)"

echo
echo "abandon: run cancel fails: logged loudly, not swallowed, abandon still proceeds:"
write_abandon_batch cut
printf '55502 in_progress\n' > "$RUNS_FILE"
: > "$CANCEL_FAIL"
> "$FORGE_LOG"
: > "$RUN/landing.log"
out="$(run abandon $REPONAME --reason 'checking run-cancel failure is logged')"; rc=$?
[ "$rc" -eq 0 ] && ok "abandon still exits 0 when a run cancel fails" \
    || bad "abandon exits 0" "rc=$rc out=$out"
want "failure is reported loudly, not swallowed" "WARN" "$out"
want "failure names the run" "55502" "$out"
want "the failure is recorded in landing.log" \
    "RUN_CANCEL_FAILED " "$(cat "$RUN/landing.log" 2>/dev/null || true)"
[ ! -f "$OPEN_FILE" ] && ok "batch still abandoned despite the cancel failure" \
    || bad "batch abandoned" "open file still present"
is "the batch is ABANDONED on spira-lc despite the cancel failure" "ABANDONED" "$(batch_state "$BATCH_ID")"
rm -f "$RUNS_FILE" "$CANCEL_FAIL"
rm -f "${QUEUEDIR:?}/${REPONAME:?}/closed-pr58-"*

echo
echo "abandon: archive name consistency — always ends with Z:"
write_abandon_batch cut
run abandon $REPONAME --reason 'checking archive name format' >/dev/null 2>&1 || true
archive="$(ls "$QUEUEDIR/$REPONAME/closed-pr58-"* 2>/dev/null | tail -1)"
[[ "$archive" == *Z ]] && ok "consistent archive name ends with Z" || bad "archive Z suffix" "got $archive"
rm -f "${QUEUEDIR:?}/${REPONAME:?}/closed-pr58-"*

echo
echo "abandon: spira-lc unreachable refuses before anything changes:"
write_abandon_batch
out="$(LCENV="$LC_DOWN" run abandon $REPONAME --reason 'unreachable probe')"; rc=$?
[ "$rc" -ne 0 ] && ok "abandon refuses when spira-lc is unreachable" || bad "abandon refuses when spira-lc is unreachable" "rc=$rc out=$out"
want "names spira-lc" "spira-lc is unreachable" "$out"
[ -f "$OPEN_FILE" ] && ok "unreachable: open record left in place" || bad "unreachable: open record left in place" "file gone"
rm -f "$OPEN_FILE"

# =============================================================================
# REAL BD — one fixture database, two things a stub cannot verify:
#  * eject reopens the bead, clears the assignee, and posts a comment (UC-29/30).
#  * G7 — abandon's return-to-CERTIFIED path makes NO bd call at all: status and
#    assignee for the surviving members are untouched. A stub always answers "ok",
#    so only a real bd, read back afterwards, can catch an accidental bdq call here.
# =============================================================================

echo
echo "real bd: eject clears assignee, posts a comment, leaves bd status unmoved:"
testdb_up queueops || { echo "test-queue-ops: could not build fixture database"; exit 1; }
B() { "${TESTDB_BD:-bd}" -C "$SPIRA_DB" "$@"; }
field() { B show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get(sys.argv[1]) or "")' "$2" 2>/dev/null; }

real_run() {
    # SPIRA_RUN/SPIRA_DB/SPIRA_BD/SPIRA_HOME_REPO/SPIRA_REPO_MAP/SPIRA_QUEUE_DIR/
    # SPIRA_FORGE are registered keys (per Ryan 2026-10-05, ONE SOURCE OF CONFIG): declare
    # via tl_config and thread SPIRA_TOML through env -i, which clears it.
    tl_config SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD:-bd}" \
        SPIRA_HOME_REPO="$REPONAME" SPIRA_REPO_MAP="$RMAP" SPIRA_QUEUE_DIR="$QUEUEDIR" \
        SPIRA_FORGE="$SH/forge-fake.sh"
    env -i $(lcfix_env) PATH="$SH:$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent SPIRA_HOME="$SH" \
        FORGE_LOG="$FORGE_LOG" \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_HOME="$SH" queue "$@" 2>&1
}

testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ej01","title":"ej01","status":"closed","issue_type":"task","labels":[],"assignee":"aeon-someone","updated_at":"2026-09-17T00:00:00Z","closed_at":"2026-09-17T00:00:00Z"}
{"id":"sp-ej02","title":"ej02","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-17T00:00:00Z","closed_at":"2026-09-17T00:00:00Z"}
{"id":"sp-ab01","title":"ab01","status":"closed","issue_type":"task","labels":[],"assignee":"aeon-holder","updated_at":"2026-09-17T00:00:00Z","closed_at":"2026-09-17T00:00:00Z"}
{"id":"sp-ab02","title":"ab02","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-17T00:00:00Z","closed_at":"2026-09-17T00:00:00Z"}
JSONL

write_eject_batch
seed_eject
> "$FORGE_LOG"

out="$(real_run eject sp-ej01 --reason 'test-suite-x.sh RED: assertion mismatch at line 42')"; rc=$?
[ "$rc" -eq 0 ] && ok "real bd: eject exit 0" || bad "real bd: eject exit 0" "rc=$rc out=$out"
is "real bd: the ejected bead is REWORK on spira-lc" "REWORK" "$(lcfix_state sp-ej01)"

bead_st="$(field sp-ej01 status)"
[ "$bead_st" = "closed" ] && ok "real bd: bd status is left unmoved by eject" || bad "real bd: bd status unmoved" "status=$bead_st"
# (No assignee check: a batch eject hands the bead back on its lifecycle row; bd's assignee is
# content no claim reads, and no reopen path writes it any more — sp-swh8b8.)
comment_out="$(B comments sp-ej01 2>/dev/null || true)"
[ -n "$comment_out" ] && ok "real bd: comment posted to bead" || bad "real bd: comment posted" "no output from bd comments"

echo
echo "real bd: eject on a CERTIFIED, unbatched bead withdraws it (no open batch at all):"
testdb_seed <<'JSONL'
{"id":"sp-ej-cert","title":"ej-cert","status":"closed","issue_type":"task","labels":[],"assignee":"aeon-someone-else","updated_at":"2026-09-17T00:00:00Z","closed_at":"2026-09-17T00:00:00Z"}
JSONL
rm -f "$OPEN_FILE"
lcfix_seed sp-ej-cert CERTIFIED "$TIP03"

out="$(real_run eject sp-ej-cert --reason 'holding for a fix')"; rc=$?
[ "$rc" -eq 0 ] && ok "real bd: eject exit 0 for certified, unbatched bead" || bad "real bd: eject exit 0" "rc=$rc out=$out"
bead_st="$(field sp-ej-cert status)"
[ "$bead_st" = "open" ] && ok "real bd: certified-unbatched bead reopened" || bad "real bd: bead open" "status=$bead_st"
assignee="$(field sp-ej-cert assignee)"
[ -z "$assignee" ] && ok "real bd: certified-unbatched assignee cleared" || bad "real bd: assignee cleared" "got $assignee"
comment_out2="$(B comments sp-ej-cert 2>/dev/null || true)"
[ -n "$comment_out2" ] && ok "real bd: comment posted to certified-unbatched bead" || bad "real bd: comment posted" "no output"
is "real bd: the withdrawn bead is REWORK on spira-lc" "REWORK" "$(lcfix_state sp-ej-cert)"

echo
echo "real bd — gap G7: abandon's return-to-CERTIFIED path makes no bd call at all:"
write_abandon_batch cut
{
    printf 'pr=91\n'
    printf 'head=ffff0000000000000000000000000000000000ff\n'
    printf 'base=0000000000000000000000000000000000000000\n'
    printf 'members=sp-ab01:%s sp-ab02:%s\n' "$TIP01" "$TIP02"
    printf 'opened=%s\n' "$(date +%s)"
    printf 'branch=spira/queue/20260917T140000Z\n'
    printf 'batch_id=%s\n' "$BATCH_ID"
} > "$OPEN_FILE"
> "$FORGE_LOG"

out="$(real_run abandon $REPONAME --reason 'gap G7 probe')"; rc=$?
[ "$rc" -eq 0 ] && ok "real bd: abandon exit 0" || bad "real bd: abandon exit 0" "rc=$rc out=$out"

is "real bd: sp-ab01 returned to CERTIFIED" "CERTIFIED" "$(lcfix_state sp-ab01)"
is "real bd: the batch is ABANDONED" "ABANDONED" "$(batch_state "$BATCH_ID")"

ab01_st="$(field sp-ab01 status)"
[ "$ab01_st" = "closed" ] && ok "G7: sp-ab01 bead status untouched by abandon (still closed)" \
    || bad "G7: sp-ab01 status untouched" "got $ab01_st"
ab01_assignee="$(field sp-ab01 assignee)"
[ "$ab01_assignee" = "aeon-holder" ] && ok "G7: sp-ab01 assignee untouched by abandon" \
    || bad "G7: sp-ab01 assignee untouched" "got $ab01_assignee"
ab01_comments="$(B comments sp-ab01 2>/dev/null || true)"
want "G7: abandon posted no comment to sp-ab01" "No comments" "$ab01_comments"

echo
tl_summary
