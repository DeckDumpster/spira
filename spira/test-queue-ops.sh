#!/usr/bin/env bash
# test-queue-ops.sh — queue.sh eject and abandon. Merged from test-queue-eject.sh and
# test-queue-abandon.sh: both built the same REPO/RUN/SH/forge-fake/repo-map scaffolding
# independently. (batch.sh's own automatic DIRTY-PR abandon path — the trigger this suite
# once also carried, folded in from test-batch-conflicting-pr.sh — is retired with
# batch.sh, sp-uwhx0: no repo runs in `land=queue` mode, and batcher-cut owns the round.)
#
# MOST CASES USE A RECORDING BD STUB, not a fixture database: eject and abandon's own
# code paths call bd only to comment/assign, plus reopen for the CERTIFIED-but-unbatched
# case only — an in-batch eject releases the bead through a spira-lc Returned event
# instead (sp-rlyl0; see bead_reopen/release_claim in lib.sh) — so a stub that logs argv
# and exits 0 exercises the same code as a real one, without paying a database build. One
# case near the end uses a real bd (testdb.sh) to verify what the stub cannot: that the
# certified-unbatched reopen actually lands (status, assignee, comment body) and — gap
# G7 — that abandon's return-to-CERTIFIED path makes NO bd call at all (queue.sh
# abandon's bead side effects were previously unread and unverified; nothing here would
# have caught an accidental bdq call added to that path).
#
# tier: T3
# covers: queue/src/* forge/src/* spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
# The queue binary (queue/DESIGN.md §7.4), invoked by name: the tree under test's build is
# on the suite's PATH (sp-gypjk).

echo "test-queue-ops.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-queue-ops

TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"
RUN="$TMP/run"
SH="$TMP/spira"
REPONAME=fixq
LANDSTATE="$RUN/landstate"
QUEUEDIR="$RUN/queue"
FORGE_LOG="$TMP/forge.log"
RUNS_FILE="$TMP/runs-for-branch"
CANCEL_FAIL="$TMP/cancel-fail"
BD_LOG="$TMP/bd-calls.log"

git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m init
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/$REPONAME"
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
# The queue binary reads `bd -C <db> show <id> --json` rows (an exit 0 with no row is
# "cannot resolve"), so show answers one JSON row per id asked for.
[ "${1:-}" = -C ] && shift 2
case "${1:-}" in
    show) shift; sep='['; for a in "$@"; do case "$a" in -*) ;; *) printf '%s{"id":"%s"}' "$sep" "$a"; sep=',' ;; esac; done; printf ']\n'; exit 0 ;;
    *)    exit 0 ;;
esac
BDSTUB
chmod +x "$SH/bd-stub.sh"

# Recording spira-lc stub, for the cases whose subject is the lifecycle machine (an
# in-batch eject is a Returned event there, sp-rlyl0). Those cases run with
# LC_ENFORCE=1 (SPIRA_LIFECYCLE_ENFORCE); every other case runs the default OFF, where
# the queue never reaches spira-lc. `list` answers the reachability probe; `show` answers
# a row in IN_DELIVERY. The queue calls the caller verb `spira-lc returned <id> <reason>`
# (lc.sh's lc_returned until sp-arpjt), which is what the stub records.
LCSTUB_LOG="$TMP/lc-calls.log"; : > "$LCSTUB_LOG"
STUBBIN="$TMP/stubbin"; mkdir -p "$STUBBIN"   # stubs named as the tools they stand in for, first on PATH
cat > "$SH/lc-stub.sh" <<'LCSTUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${LCSTUB_LOG:?}"
case "${1:-}" in
    list) printf '[]\n' ;;
    show) printf '{"bead":{"bead_id":"%s","state":"IN_DELIVERY","version":"3","holds":"[]"}}\n' "${2:-}" ;;
esac
exit 0
LCSTUB
chmod +x "$SH/lc-stub.sh"
ln -sf "$SH/lc-stub.sh" "$STUBBIN/spira-lc"

RMAP="$TMP/repo-map"
printf '%s | %s | queue | main | | |\n' "$REPONAME" "$REPO" > "$RMAP"

run() {
    env -i PATH="$SH:$STUBBIN:$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$SH" \
        SPIRA_RUN="$RUN" \
        SPIRA_DB="${SPIRA_DB:-/nonexistent}" \
        SPIRA_BD="$SH/bd-stub.sh" \
        BD_LOG="$BD_LOG" \
        SPIRA_HOME_REPO="$REPONAME" \
        SPIRA_REPO_MAP="$RMAP" \
        SPIRA_QUEUE_DIR="$QUEUEDIR" \
        SPIRA_FORGE="$SH/forge-fake.sh" \
        FORGE_LOG="$FORGE_LOG" \
        RUNS_FILE="$RUNS_FILE" \
        CANCEL_FAIL="$CANCEL_FAIL" \
        BEADS_ACTOR="aeon-abandontest" \
        SPIRA_EVENT_COOLDOWN=0 \
        SPIRA_LIFECYCLE_ENFORCE="${LC_ENFORCE:-0}" \
        LCSTUB_LOG="$LCSTUB_LOG" \
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

write_abandon_batch() {
    {
        printf 'pr=58\n'
        printf 'head=ffff0000000000000000000000000000000000ff\n'
        printf 'base=0000000000000000000000000000000000000000\n'
        printf 'members=sp-ab01:%s sp-ab02:%s sp-ab03:%s\n' "$TIP01" "$TIP02" "$TIP03"
        printf 'opened=%s\n' "$(date +%s)"
        printf 'branch=spira/queue/20260917T130000Z\n'
    } > "$OPEN_FILE"
}

OPEN_FILE="$QUEUEDIR/$REPONAME/open"

# =============================================================================
# EJECT — recording-bd stub (UC-29).
# =============================================================================

echo
echo "eject: positive control — eject finds a real member (guard is live):"
write_eject_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ej01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ej02"
> "$FORGE_LOG"; > "$BD_LOG"; : > "$LCSTUB_LOG"
out="$(LC_ENFORCE=1 run eject sp-ej01 --reason 'test-foo.sh RED')"; rc=$?
[ "$rc" -eq 0 ] && ok "exit 0 for valid member" || bad "exit 0 for valid member" "rc=$rc out=$out"
want "reports ejection" "ejected sp-ej01" "$out"
st="$(awk '{print $1}' "$LANDSTATE/sp-ej01" 2>/dev/null || true)"
[ "$st" = "RED" ] && ok "landstate RED after eject" || bad "landstate RED" "got $st"
# NOT bd reopen ANY MORE (sp-rlyl0): a batch member's eject is a lifecycle Returned event
# now, not a bd write — the stub records no bd argv for it at all.
nowant "bd reopen is no longer called for an in-batch eject" "reopen sp-ej01" "$(cat "$BD_LOG")"
want   "lifecycle on: the eject is a spira-lc Returned event (the caller verb, sp-arpjt)" "event bead sp-ej01" "$(cat "$LCSTUB_LOG")"

echo
echo "eject: lifecycle_enforce OFF (the default) — an in-batch eject hands the bead back through bd:"
write_eject_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ej01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ej02"
> "$FORGE_LOG"; > "$BD_LOG"; : > "$LCSTUB_LOG"
out="$(run eject sp-ej01 --reason 'test-foo.sh RED')"; rc=$?
[ "$rc" -eq 0 ] && ok "off: exit 0 for valid member" || bad "off: exit 0 for valid member" "rc=$rc out=$out"
st="$(awk '{print $1}' "$LANDSTATE/sp-ej01" 2>/dev/null || true)"
[ "$st" = "RED" ] && ok "off: landstate RED after eject" || bad "off: landstate RED" "got $st"
want "off: bd reopen called for the in-batch eject" "reopen sp-ej01" "$(cat "$BD_LOG")"
is   "off: spira-lc never called"                   ""               "$(cat "$LCSTUB_LOG")"

echo
echo "eject: non-member is refused; batch and landstate unchanged:"
write_eject_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ej01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ej02"
out="$(run eject sp-nonexist)"; rc=$?
[ "$rc" -ne 0 ] && ok "exits non-zero for non-member" || bad "exits non-zero" "rc=$rc"
want "names the id" "sp-nonexist is not a member" "$out"
want "lists batch members" "members:" "$out"
members_now="$(grep '^members=' "$OPEN_FILE" | head -1)"
[[ "$members_now" == *"sp-ej01:"* ]] && ok "sp-ej01 still in batch" || bad "batch unchanged" "$members_now"
[[ "$members_now" == *"sp-ej02:"* ]] && ok "sp-ej02 still in batch" || bad "batch unchanged" "$members_now"
[ ! -f "$LANDSTATE/sp-nonexist" ] && ok "no landstate written for non-member" || bad "no landstate" "file exists"

echo
echo "eject: CERTIFIED, unbatched bead is withdrawn — no open batch exists at all:"
rm -f "$OPEN_FILE"
printf 'CERTIFIED %s %s\n' "$TIP03" "$(date +%s)" > "$LANDSTATE/sp-ej-cert"
> "$BD_LOG"
out="$(run eject sp-ej-cert --reason 'holding for a fix')"; rc=$?
[ "$rc" -eq 0 ] && ok "exit 0 for certified, unbatched bead" || bad "exit 0" "rc=$rc out=$out"
want "reports the certified-unbatched case" "certified, not yet batched" "$out"
st="$(awk '{print $1}' "$LANDSTATE/sp-ej-cert" 2>/dev/null || true)"
[ "$st" = "WITHDRAWN" ] && ok "landstate WITHDRAWN after eject" || bad "landstate WITHDRAWN" "got $st"
tp="$(awk '{print $2}' "$LANDSTATE/sp-ej-cert" 2>/dev/null || true)"
[ "$tp" = "$TIP03" ] && ok "tip preserved across withdrawal" || bad "tip preserved" "got $tp"
want "bd reopen called" "reopen sp-ej-cert" "$(cat "$BD_LOG")"
rm -f "$LANDSTATE/sp-ej-cert"

echo
echo "eject: --suites records the suites in the WITHDRAWN reason and the .ejected sidecar (sp-hkfdp):"
printf 'CERTIFIED %s %s\n' "$TIP03" "$(date +%s)" > "$LANDSTATE/sp-ej-suites"
> "$BD_LOG"
out="$(run eject sp-ej-suites --reason 'suite reds' --suites 'test-x.sh,test-y.sh')"; rc=$?
[ "$rc" -eq 0 ] && ok "exit 0 with --suites" || bad "exit 0 with --suites" "rc=$rc out=$out"
st="$(awk '{print $1}' "$LANDSTATE/sp-ej-suites" 2>/dev/null || true)"
[ "$st" = "WITHDRAWN" ] && ok "landstate WITHDRAWN with --suites" || bad "landstate WITHDRAWN" "got $st"
want "reason names the suites" "suites=test-x.sh,test-y.sh" "$(cat "$LANDSTATE/sp-ej-suites")"
[ "$(cat "$LANDSTATE/sp-ej-suites.ejected" 2>/dev/null)" = "test-x.sh,test-y.sh" ] \
    && ok "sidecar .ejected carries the suites" \
    || bad "sidecar .ejected carries the suites" "got [$(cat "$LANDSTATE/sp-ej-suites.ejected" 2>/dev/null)]"
rm -f "$LANDSTATE/sp-ej-suites" "$LANDSTATE/sp-ej-suites.ejected"

echo
echo "eject: CERTIFIED, unbatched bead — an unrelated open batch does not block it:"
write_eject_batch
printf 'BATCHED %s %s\n'   "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ej01"
printf 'BATCHED %s %s\n'   "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ej02"
printf 'CERTIFIED %s %s\n' "$TIP03" "$(date +%s)" > "$LANDSTATE/sp-ej-cert2"
out="$(run eject sp-ej-cert2)"; rc=$?
[ "$rc" -eq 0 ] && ok "exit 0 while an unrelated batch is open" || bad "exit 0" "rc=$rc out=$out"
st="$(awk '{print $1}' "$LANDSTATE/sp-ej-cert2" 2>/dev/null || true)"
[ "$st" = "WITHDRAWN" ] && ok "landstate WITHDRAWN, unrelated batch untouched" || bad "landstate WITHDRAWN" "got $st"
members_now="$(grep '^members=' "$OPEN_FILE" | head -1)"
[[ "$members_now" == *"sp-ej01:"* && "$members_now" == *"sp-ej02:"* ]] \
    && ok "unrelated open batch members unchanged" || bad "unrelated batch unchanged" "$members_now"
rm -f "$LANDSTATE/sp-ej-cert2"

echo
echo "eject: dry-run for a CERTIFIED, unbatched bead prints plan and changes nothing:"
rm -f "$OPEN_FILE"
printf 'CERTIFIED %s %s\n' "$TIP03" "$(date +%s)" > "$LANDSTATE/sp-ej-cert3"
> "$BD_LOG"
out="$(run eject sp-ej-cert3 --dry-run)"; rc=$?
[ "$rc" -eq 0 ] && ok "dry-run exits 0 for certified, unbatched bead" || bad "dry-run exit 0" "rc=$rc"
want "dry-run mentions WITHDRAWN write" "would write WITHDRAWN" "$out"
want "dry-run mentions bead reopen"     "would reopen bead sp-ej-cert3" "$out"
st="$(awk '{print $1}' "$LANDSTATE/sp-ej-cert3" 2>/dev/null || true)"
[ "$st" = "CERTIFIED" ] && ok "dry-run did not change landstate" || bad "dry-run no change" "got $st"
nowant "dry-run: bd reopen not called" "reopen sp-ej-cert3" "$(cat "$BD_LOG")"
rm -f "$LANDSTATE/sp-ej-cert3"

echo
echo "eject: landstate round-trips through the reader (format check):"
write_eject_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ej01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ej02"
run eject sp-ej01 >/dev/null 2>&1 || true
_st=""; _tip=""
{ read -r _st _tip _ < "$LANDSTATE/sp-ej01"; } 2>/dev/null || true
[ "$_st" = "RED" ] && ok "round-trip: state field is RED" || bad "round-trip state" "got $_st"
[ "$_tip" = "$TIP01" ] && ok "round-trip: tip field preserved" || bad "round-trip tip" "got $_tip"

echo
echo "eject: dry-run prints plan; non-member exits non-zero:"
write_eject_batch
out="$(run eject sp-nonexist --dry-run)"; rc=$?
[ "$rc" -ne 0 ] && ok "dry-run non-member exits non-zero" || bad "dry-run non-member" "rc=$rc"

echo
echo "eject: lock held: refuses and changes nothing:"
write_eject_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ej01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ej02"
lockfile="$QUEUEDIR/$REPONAME/lock"
exec 8>"$lockfile"
flock 8
out="$(run eject sp-ej01)"; rc=$?
exec 8>&-
[ "$rc" -ne 0 ] && ok "exit non-zero when lock held" || bad "exit non-zero" "rc=$rc"
want "mentions lock" "holds the lock" "$out"
[ -f "$OPEN_FILE" ] && ok "open file unchanged" || bad "open file unchanged" "file gone"
st="$(awk '{print $1}' "$LANDSTATE/sp-ej01" 2>/dev/null || true)"
[ "$st" = "BATCHED" ] && ok "landstate unchanged when lock held" || bad "landstate unchanged" "got $st"

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
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ej01"
> "$FORGE_LOG"
out="$(run eject sp-ej01)"; rc=$?
[ "$rc" -eq 0 ] && ok "single-member: exit 0" || bad "single-member: exit 0" "rc=$rc out=$out"
st="$(awk '{print $1}' "$LANDSTATE/sp-ej01" 2>/dev/null || true)"
[ "$st" = "RED" ] && ok "single-member: landstate RED" || bad "single-member: RED" "got $st"
want "single-member: forge pr-close called" "pr-close" "$(cat "$FORGE_LOG")"
[ ! -f "$OPEN_FILE" ] && ok "single-member: batch removed" || bad "single-member: batch removed" "file exists"

echo
echo "eject: dry-run for a member prints plan and changes nothing:"
write_eject_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ej01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ej02"
> "$FORGE_LOG"
out="$(LC_ENFORCE=1 run eject sp-ej01 --dry-run)"; rc=$?
[ "$rc" -eq 0 ] && ok "dry-run exits 0 for member" || bad "dry-run exits 0" "rc=$rc"
want "dry-run mentions RED write"          "would write RED"           "$out"
want "dry-run mentions the lifecycle return" "would return bead sp-ej01 to spira-lc" "$out"
want "dry-run mentions PR close"           "would close PR 42"         "$out"
want "dry-run mentions survivor CERTIFIED" "would return survivors"    "$out"
st="$(awk '{print $1}' "$LANDSTATE/sp-ej01" 2>/dev/null || true)"
[ "$st" = "BATCHED" ] && ok "dry-run did not change landstate" || bad "dry-run no change" "got $st"
[ -f "$OPEN_FILE" ] && ok "dry-run did not remove batch" || bad "dry-run no change" "batch gone"
[ -z "$(cat "$FORGE_LOG")" ] && ok "dry-run: forge not called" || bad "dry-run no forge" "got $(cat "$FORGE_LOG")"

# =============================================================================
# ABANDON — recording-bd stub (UC-30).
# =============================================================================

echo
echo "abandon: --reason is required — refuses, changes no landstate, leaves the open record in place:"
write_abandon_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ab01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ab02"
printf 'BATCHED %s %s\n' "$TIP03" "$(date +%s)" > "$LANDSTATE/sp-ab03"
> "$FORGE_LOG"
: > "$RUN/landing.log"
out="$(run abandon $REPONAME)"; rc=$?
[ "$rc" -ne 0 ] && ok "refuses without --reason" || bad "refuses without --reason" "rc=$rc out=$out"
want "the refusal names the flag" "--reason" "$out"
[ -f "$OPEN_FILE" ] && ok "no --reason: open record left in place" || bad "open record left in place" "file gone"
st01="$(awk '{print $1}' "$LANDSTATE/sp-ab01" 2>/dev/null || true)"
[ "$st01" = "BATCHED" ] && ok "no --reason: sp-ab01 still BATCHED (positive control)" \
    || bad "sp-ab01 still BATCHED" "got $st01"
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
write_abandon_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ab01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ab02"
printf 'BATCHED %s %s\n' "$TIP03" "$(date +%s)" > "$LANDSTATE/sp-ab03"
lockfile="$QUEUEDIR/$REPONAME/lock"
exec 8>"$lockfile"
flock 8
out="$(run abandon $REPONAME --reason 'checking the lock guard')"; rc=$?
exec 8>&-
[ "$rc" -ne 0 ] && ok "exit non-zero when lock held" || bad "exit non-zero" "rc=$rc"
want "mentions lock" "holds the lock" "$out"
[ -f "$OPEN_FILE" ] && ok "open file unchanged" || bad "open file unchanged" "file gone"

echo
echo "abandon: PR closed, innocent members CERTIFIED, RED/EJECTED left alone:"
write_abandon_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ab01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ab02"
printf 'EJECTED %s %s\n' "$TIP03" "$(date +%s)" > "$LANDSTATE/sp-ab03"
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

st01="$(awk '{print $1}' "$LANDSTATE/sp-ab01" 2>/dev/null || true)"
tp01="$(awk '{print $2}' "$LANDSTATE/sp-ab01" 2>/dev/null || true)"
[ "$st01" = "CERTIFIED" ] && ok "sp-ab01 landstate is CERTIFIED" || bad "sp-ab01 CERTIFIED" "got $st01"
[ "$tp01" = "$TIP01"    ] && ok "sp-ab01 tip preserved"          || bad "sp-ab01 tip"       "got $tp01"
st02="$(awk '{print $1}' "$LANDSTATE/sp-ab02" 2>/dev/null || true)"
[ "$st02" = "CERTIFIED" ] && ok "sp-ab02 landstate is CERTIFIED" || bad "sp-ab02 CERTIFIED" "got $st02"
st03="$(awk '{print $1}' "$LANDSTATE/sp-ab03" 2>/dev/null || true)"
[ "$st03" = "EJECTED"   ] && ok "sp-ab03 left as EJECTED"        || bad "sp-ab03 untouched"  "got $st03"

[ ! -f "$OPEN_FILE" ] && ok "open record archived (not open)" || bad "open record gone" "still exists"
archive="$(ls "$QUEUEDIR/$REPONAME/closed-pr58-"* 2>/dev/null | head -1)"
[ -n "$archive" ] && ok "archive exists" || bad "archive exists" "no closed-pr58-* found"
[[ "$archive" == *Z ]] && ok "archive name ends with Z" || bad "archive name Z suffix" "got $archive"
want "reason in forge call" "guilty branch found" "$forge_calls"

# The durable audit line: one row in landing.log naming who ran it, on what PR, with
# every member's disposition — a positive control (sp-ab01 present), not just quiet.
audit_count="$(grep -c '^QUEUE ABANDON ' "$RUN/landing.log" 2>/dev/null || echo 0)"
[ "${audit_count:-0}" -eq 1 ] && ok "exactly one QUEUE ABANDON audit line" \
    || bad "exactly one audit line" "count=$audit_count"
audit_line="$(grep '^QUEUE ABANDON ' "$RUN/landing.log" | head -1)"
want "audit line names the actor"                            "actor=aeon-abandontest"  "$audit_line"
want "audit line names the PR"                                "pr=58"                   "$audit_line"
want "audit line names a present member (positive control)"   "sp-ab01:CERTIFIED"       "$audit_line"
want "audit line names the ejected member's disposition"      "sp-ab03:EJECTED"         "$audit_line"
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
echo "abandon: RED member also left untouched:"
write_abandon_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ab01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ab02"
printf 'RED %s %s\n'     "$TIP03" "$(date +%s)" > "$LANDSTATE/sp-ab03"
> "$FORGE_LOG"
out="$(run abandon $REPONAME --reason 'checking RED survives abandon')"; rc=$?
[ "$rc" -eq 0 ] && ok "exit 0 with RED member" || bad "exit 0 RED" "rc=$rc out=$out"
st03="$(awk '{print $1}' "$LANDSTATE/sp-ab03" 2>/dev/null || true)"
[ "$st03" = "RED" ] && ok "RED member left as RED" || bad "RED untouched" "got $st03"
rm -f "$QUEUEDIR/$REPONAME/closed-pr58-"*

echo
echo "abandon: dry-run prints plan and changes nothing:"
write_abandon_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ab01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ab02"
printf 'EJECTED %s %s\n' "$TIP03" "$(date +%s)" > "$LANDSTATE/sp-ab03"
> "$FORGE_LOG"
: > "$RUN/landing.log"
: > "$RUN/events.log"
out="$(run abandon $REPONAME --dry-run --reason 'dry-run preview check')"; rc=$?
[ "$rc" -eq 0 ] && ok "dry-run exits 0" || bad "dry-run exits 0" "rc=$rc"
want "dry-run mentions PR"             "would close PR 58"   "$out"
want "dry-run innocent member"         "return to CERTIFIED" "$out"
want "dry-run ejected member"          "leave alone"          "$out"
want "dry-run mentions the run cancel" "would cancel"         "$out"
want "dry-run shows archive path"      "archive path"         "$out"
want "dry-run prints the audit line it would write" \
    "dry-run: audit line (landing.log): QUEUE ABANDON" "$out"
want "dry-run audit preview names the actor"  "actor=aeon-abandontest"     "$out"
want "dry-run audit preview names a member"   "sp-ab01:CERTIFIED"          "$out"
want "dry-run audit preview names the reason" "dry-run preview check"      "$out"
[ -f "$OPEN_FILE" ] && ok "dry-run: open file unchanged" || bad "dry-run no change" "open file gone"
[ -z "$(cat "$FORGE_LOG")" ] && ok "dry-run: forge not called" || bad "dry-run no forge" "got $(cat "$FORGE_LOG")"
st01="$(awk '{print $1}' "$LANDSTATE/sp-ab01" 2>/dev/null || true)"
[ "$st01" = "BATCHED" ] && ok "dry-run: landstate unchanged" || bad "dry-run landstate" "got $st01"
nowant "dry-run: no audit line written to landing.log" "QUEUE ABANDON" "$(cat "$RUN/landing.log" 2>/dev/null || true)"
nowant "dry-run: no event written to events.log" "queue.abandoned" "$(cat "$RUN/events.log" 2>/dev/null || true)"

echo
echo "abandon: run cancel fails: logged loudly, not swallowed, abandon still proceeds:"
write_abandon_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ab01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ab02"
printf 'EJECTED %s %s\n' "$TIP03" "$(date +%s)" > "$LANDSTATE/sp-ab03"
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
rm -f "$RUNS_FILE" "$CANCEL_FAIL"
rm -f "$QUEUEDIR/$REPONAME/closed-pr58-"*

echo
echo "abandon: archive name consistency — always ends with Z:"
write_abandon_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ab01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ab02"
printf 'BATCHED %s %s\n' "$TIP03" "$(date +%s)" > "$LANDSTATE/sp-ab03"
run abandon $REPONAME --reason 'checking archive name format' >/dev/null 2>&1 || true
archive="$(ls "$QUEUEDIR/$REPONAME/closed-pr58-"* 2>/dev/null | tail -1)"
[[ "$archive" == *Z ]] && ok "consistent archive name ends with Z" || bad "archive Z suffix" "got $archive"
rm -f "$QUEUEDIR/$REPONAME/closed-pr58-"*

# =============================================================================
# REAL BD — one fixture database, two things a stub cannot verify:
#  * eject reopens the bead, clears the assignee, and posts a comment (UC-29/30).
#  * G7 — abandon's return-to-CERTIFIED path makes NO bd call at all: status and
#    assignee for the surviving members are untouched. A stub always answers "ok",
#    so only a real bd, read back afterwards, can catch an accidental bdq call here.
# =============================================================================

echo
echo "real bd: eject reopens the bead, clears assignee, posts a comment:"
testdb_up queueops || { echo "test-queue-ops: could not build fixture database"; exit 1; }
B() { "${TESTDB_BD:-bd}" -C "$SPIRA_DB" "$@"; }
field() { B show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get(sys.argv[1]) or "")' "$2" 2>/dev/null; }

real_run() {
    env -i PATH="$SH:$STUBBIN:$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent SPIRA_HOME="$SH" SPIRA_RUN="$RUN" \
        SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD:-bd}" \
        SPIRA_HOME_REPO="$REPONAME" SPIRA_REPO_MAP="$RMAP" SPIRA_QUEUE_DIR="$QUEUEDIR" \
        SPIRA_FORGE="$SH/forge-fake.sh" FORGE_LOG="$FORGE_LOG" \
        SPIRA_LIFECYCLE_ENFORCE="${LC_ENFORCE:-0}" \
        LCSTUB_LOG="$LCSTUB_LOG" \
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
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ej01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ej02"
> "$FORGE_LOG"

out="$(LC_ENFORCE=1 real_run eject sp-ej01 --reason 'test-suite-x.sh RED: assertion mismatch at line 42')"; rc=$?
[ "$rc" -eq 0 ] && ok "real bd: eject exit 0" || bad "real bd: eject exit 0" "rc=$rc out=$out"

# NOT reopened via bd ANY MORE (sp-rlyl0): an in-batch eject is a lifecycle Returned event
# now — bd's own status field is left exactly as it was, since it is no longer the write
# this tool makes for the bead's state.
bead_st="$(field sp-ej01 status)"
[ "$bead_st" = "closed" ] && ok "real bd: bd status is left unmoved by eject now" || bad "real bd: bd status unmoved" "status=$bead_st"
assignee="$(field sp-ej01 assignee)"
[ -z "$assignee" ] && ok "real bd: assignee cleared" || bad "real bd: assignee cleared" "got $assignee"
comment_out="$(B comments sp-ej01 2>/dev/null || true)"
[ -n "$comment_out" ] && ok "real bd: comment posted to bead" || bad "real bd: comment posted" "no output from bd comments"

echo
echo "real bd: eject on a CERTIFIED, unbatched bead withdraws it (no open batch at all):"
testdb_seed <<'JSONL'
{"id":"sp-ej-cert","title":"ej-cert","status":"closed","issue_type":"task","labels":[],"assignee":"aeon-someone-else","updated_at":"2026-09-17T00:00:00Z","closed_at":"2026-09-17T00:00:00Z"}
JSONL
rm -f "$OPEN_FILE"
printf 'CERTIFIED %s %s\n' "$TIP03" "$(date +%s)" > "$LANDSTATE/sp-ej-cert"

out="$(real_run eject sp-ej-cert --reason 'holding for a fix')"; rc=$?
[ "$rc" -eq 0 ] && ok "real bd: eject exit 0 for certified, unbatched bead" || bad "real bd: eject exit 0" "rc=$rc out=$out"
bead_st="$(field sp-ej-cert status)"
[ "$bead_st" = "open" ] && ok "real bd: certified-unbatched bead reopened" || bad "real bd: bead open" "status=$bead_st"
assignee="$(field sp-ej-cert assignee)"
[ -z "$assignee" ] && ok "real bd: certified-unbatched assignee cleared" || bad "real bd: assignee cleared" "got $assignee"
comment_out2="$(B comments sp-ej-cert 2>/dev/null || true)"
[ -n "$comment_out2" ] && ok "real bd: comment posted to certified-unbatched bead" || bad "real bd: comment posted" "no output"
st="$(awk '{print $1}' "$LANDSTATE/sp-ej-cert" 2>/dev/null || true)"
[ "$st" = "WITHDRAWN" ] && ok "real bd: landstate WITHDRAWN" || bad "real bd: landstate WITHDRAWN" "got $st"
rm -f "$LANDSTATE/sp-ej-cert"

echo
echo "real bd — gap G7: abandon's return-to-CERTIFIED path makes no bd call at all:"
{
    printf 'pr=91\n'
    printf 'head=ffff0000000000000000000000000000000000ff\n'
    printf 'base=0000000000000000000000000000000000000000\n'
    printf 'members=sp-ab01:%s sp-ab02:%s\n' "$TIP01" "$TIP02"
    printf 'opened=%s\n' "$(date +%s)"
    printf 'branch=spira/queue/20260917T140000Z\n'
} > "$OPEN_FILE"
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ab01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ab02"
> "$FORGE_LOG"

out="$(real_run abandon $REPONAME --reason 'gap G7 probe')"; rc=$?
[ "$rc" -eq 0 ] && ok "real bd: abandon exit 0" || bad "real bd: abandon exit 0" "rc=$rc out=$out"

st01="$(awk '{print $1}' "$LANDSTATE/sp-ab01" 2>/dev/null || true)"
[ "$st01" = "CERTIFIED" ] && ok "real bd: sp-ab01 returned to CERTIFIED" || bad "real bd: sp-ab01 CERTIFIED" "got $st01"

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
