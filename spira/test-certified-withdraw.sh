#!/usr/bin/env bash
#
# test-certified-withdraw.sh — a CERTIFIED bead cannot be withdrawn: reopening or
#   ejecting it must clear its certification so the queue cannot admit it (sp-fgbk3).
#
# THE DEFECT. bead_reopen recorded the rework note but left landstate CERTIFIED <tip>,
# so a bead reopened by any of its many callers (an operator via slay.sh, the eviction-
# race path, queue.sh eject) stayed admissible to the next batch cut unchanged. The only
# lever was a hand-written override pinned to one commit, which a rebase defeats.
#
# THREE CASES, EACH SEEN TO FAIL AGAINST THE CODE BEFORE THIS FIX:
#
#   A. bead_reopen (lib.sh) downgrades a CERTIFIED landstate to WITHDRAWN, and leaves
#      every other landstate untouched (positive control — this is not a blanket
#      rewrite of every reopen).
#   B. batch.sh's second line of defence: a CERTIFIED bead whose store status is not
#      closed is refused admission whatever the landstate says.
#   C. End-to-end: reopening a certified, unbatched bead withdraws it from the next
#      cut; once the aeon pushes a new tip and it recertifies, the next cut admits it.
#
# queue.sh eject's own withdrawal of a certified-unbatched bead is covered in
# test-queue-ops.sh, which already carries this file's REAL BD infrastructure.
#
# covers: spira/lib.sh spira/batch.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-certified-withdraw
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up certwithdraw || { echo "test-certified-withdraw: could not build fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; RUN="$TMP/run"; SH="$TMP/spira"
REPONAME=fixture-repo
LANDSTATE="$RUN/landstate"; QUEUEDIR="$RUN/queue"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/$REPONAME"
cp "$HERE"/*.sh "$HERE"/*.py "$SH/"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

cat > "$SH/mail.sh" <<'MAIL'
#!/usr/bin/env bash
exit 0
MAIL
chmod +x "$SH/mail.sh"

FORGE_LOG="$TMP/forge-log"
cat > "$SH/forge-fixture.sh" <<'FORGE'
#!/usr/bin/env bash
cmd="${1:-}"; shift; repo="${1:-}"; shift
case "$cmd" in
    runs-active)      printf '0\n' ;;
    main-gate-status) printf 'green deadbeef\n' ;;
    pr-create)
        n=$(( $(wc -l < "$FORGE_LOG" 2>/dev/null || echo 0) + 1 ))
        printf '%s\n' "$n" >> "$FORGE_LOG"
        printf '%s\n' "$n"
        ;;
    *) printf 'forge-fixture: unknown: %s\n' "$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"
: > "$FORGE_LOG"

batch() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_MAX=8 \
    SPIRA_QUEUE_BATCH_WAIT=0 \
    FORGE_LOG="$FORGE_LOG" \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
        bash "$SH/batch.sh" "$@" 2>&1
}

field() { "${TESTDB_BD:-bd}" -C "$SPIRA_DB" show "$1" --json 2>/dev/null | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get(sys.argv[1]) or "")' "$2" 2>/dev/null; }

echo "test-certified-withdraw.sh"

# =============================================================================
# A. bead_reopen (lib.sh) downgrades CERTIFIED to WITHDRAWN; every other
#    landstate is left exactly as it was.
# =============================================================================
echo
echo "bead_reopen: CERTIFIED landstate is withdrawn on reopen; other states untouched:"

export SPIRA_RUN="$RUN" SPIRA_REAPLOG="$RUN/reap.log" SPIRA_CONF="$TMP/no-such-conf"
# shellcheck disable=SC1090
. "$HERE/lib.sh"

testdb_seed <<'JSONL'
{"id":"sp-wd-cert","title":"wd-cert","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-25T00:00:00Z","closed_at":"2026-09-25T00:00:00Z"}
{"id":"sp-wd-red","title":"wd-red","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-25T00:00:00Z","closed_at":"2026-09-25T00:00:00Z"}
JSONL

WDTIP="1111111111111111111111111111111111111c"
printf 'CERTIFIED %s %s\n' "$WDTIP" "$(date +%s)" > "$LANDSTATE/sp-wd-cert"
printf 'RED %s %s some-other-reason\n' "$WDTIP" "$(date +%s)" > "$LANDSTATE/sp-wd-red"

# POSITIVE CONTROL: a non-CERTIFIED landstate survives a reopen unchanged — the
# mechanism fires only on the one state a reopen must clear.
bead_reopen sp-wd-red some-cause >/dev/null 2>&1
_st=""; _tip=""; _reason=""
{ read -r _st _tip _ _reason < "$LANDSTATE/sp-wd-red"; } 2>/dev/null || true
is "positive control: RED landstate state untouched"  "RED"               "$_st"
is "positive control: RED landstate reason untouched" "some-other-reason" "$_reason"

# FIXED: a CERTIFIED landstate is downgraded to WITHDRAWN, tip preserved, reason=cause.
bead_reopen sp-wd-cert holding-for-fix >/dev/null 2>&1
_st=""; _tip=""; _reason=""
{ read -r _st _tip _ _reason < "$LANDSTATE/sp-wd-cert"; } 2>/dev/null || true
is "CERTIFIED reopen: landstate WITHDRAWN" "WITHDRAWN"       "$_st"
is "CERTIFIED reopen: tip preserved"       "$WDTIP"          "$_tip"
is "CERTIFIED reopen: reason is the cause" "holding-for-fix" "$_reason"
is "CERTIFIED reopen: bead status is open" "open"            "$(field sp-wd-cert status)"

# =============================================================================
# B. batch.sh: second line of defence — a CERTIFIED bead whose store status is
#    not closed is refused admission whatever the landstate says.
# =============================================================================
echo
echo "batch.sh: CERTIFIED bead with a non-closed store status is refused admission:"

testdb_seed <<JSONL
{"id":"sp-nc-closed","title":"nc-closed","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-nc-open","title":"nc-open","status":"open","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-25T00:00:00Z"}
JSONL

git -C "$REPO" checkout -q -b spira/sp-nc-closed main
printf 'nc-closed\n' > "$REPO/sp-nc-closed.txt"
git -C "$REPO" add sp-nc-closed.txt && git -C "$REPO" commit -q -m "sp-nc-closed: work"
_ncc_tip="$(git -C "$REPO" rev-parse spira/sp-nc-closed)"
git -C "$REPO" checkout -q main

git -C "$REPO" checkout -q -b spira/sp-nc-open main
printf 'nc-open\n' > "$REPO/sp-nc-open.txt"
git -C "$REPO" add sp-nc-open.txt && git -C "$REPO" commit -q -m "sp-nc-open: work"
_nco_tip="$(git -C "$REPO" rev-parse spira/sp-nc-open)"
git -C "$REPO" checkout -q main

printf 'CERTIFIED %s %s\n' "$_ncc_tip" "$(date +%s)" > "$LANDSTATE/sp-nc-closed"
printf 'CERTIFIED %s %s\n' "$_nco_tip" "$(date +%s)" > "$LANDSTATE/sp-nc-open"
rm -f "$QUEUEDIR/$REPONAME/open"; : > "$FORGE_LOG"

out="$(batch "$REPONAME")"
want "second line of defence: WARN names the open bead" "not-closed sp-nc-open" "$out"
members_now="$(grep '^members=' "$QUEUEDIR/$REPONAME/open" 2>/dev/null | head -1)"
[[ "$members_now" == *"sp-nc-closed:"* ]] && ok "closed bead admitted" \
    || bad "closed bead admitted" "members=$members_now out=$out"
[[ "$members_now" != *"sp-nc-open:"* ]] && ok "open-status bead refused admission" \
    || bad "open-status bead refused admission" "members=$members_now"
is "refused bead: landstate left CERTIFIED" "CERTIFIED" "$(awk '{print $1}' "$LANDSTATE/sp-nc-open" 2>/dev/null)"
rm -f "$QUEUEDIR/$REPONAME/open"; : > "$FORGE_LOG"

# =============================================================================
# C. END-TO-END: reopening a certified, unbatched bead withdraws it from the
#    next cut; recertifying with a new tip admits it on the cut after that.
# =============================================================================
echo
echo "end-to-end: reopen withdraws a certified bead from the next cut; recertifying admits it:"

testdb_seed <<JSONL
{"id":"sp-e2e","title":"e2e withdraw","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-25T00:00:00Z","closed_at":"2026-09-25T00:00:00Z"}
JSONL

git -C "$REPO" checkout -q -b spira/sp-e2e main
printf 'e2e-v1\n' > "$REPO/sp-e2e.txt"
git -C "$REPO" add sp-e2e.txt && git -C "$REPO" commit -q -m "sp-e2e: work v1"
_e2e_tip1="$(git -C "$REPO" rev-parse spira/sp-e2e)"
git -C "$REPO" checkout -q main

printf 'CERTIFIED %s %s\n' "$_e2e_tip1" "$(date +%s)" > "$LANDSTATE/sp-e2e"
rm -f "$QUEUEDIR/$REPONAME/open"; : > "$FORGE_LOG"

# Reopen it — the same mechanism queue.sh eject, the sentinel, and slay.sh all
# reach through (bead_reopen, sourced above as part of this process's lib.sh).
bead_reopen sp-e2e recertify-needed "test: withdrawing sp-e2e for a fix" >/dev/null 2>&1
is "reopen: landstate WITHDRAWN" "WITHDRAWN" "$(awk '{print $1}' "$LANDSTATE/sp-e2e" 2>/dev/null)"

batch "$REPONAME" >/dev/null
is "cut after reopen: no batch opened (0 certified)" "0" \
    "$([ -f "$QUEUEDIR/$REPONAME/open" ] && echo 1 || echo 0)"

# The aeon pushes a new tip and it recertifies.
git -C "$REPO" checkout -q spira/sp-e2e
printf 'e2e-v2\n' >> "$REPO/sp-e2e.txt"
git -C "$REPO" add sp-e2e.txt && git -C "$REPO" commit -q -m "sp-e2e: fixed"
_e2e_tip2="$(git -C "$REPO" rev-parse spira/sp-e2e)"
git -C "$REPO" checkout -q main
bdq close sp-e2e --reason "test: recertified" >/dev/null 2>&1
printf 'CERTIFIED %s %s\n' "$_e2e_tip2" "$(date +%s)" > "$LANDSTATE/sp-e2e"

out_cut2="$(batch "$REPONAME")"
members_after="$(grep '^members=' "$QUEUEDIR/$REPONAME/open" 2>/dev/null | head -1)"
[[ "$members_after" == *"sp-e2e:"* ]] && ok "recertified: admitted on the next cut" \
    || bad "recertified: admitted on the next cut" "members=$members_after out=$out_cut2"

echo
tl_summary
