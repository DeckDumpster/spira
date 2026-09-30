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
# TWO CASES, EACH SEEN TO FAIL AGAINST THE CODE BEFORE THIS FIX:
#
#   A. bead_reopen (lib.sh) downgrades a CERTIFIED landstate to WITHDRAWN, and leaves
#      every other landstate untouched (positive control — this is not a blanket
#      rewrite of every reopen).
#   C. End-to-end: reopening a certified, unbatched bead withdraws it from what the
#      next cut would draw from; once the aeon pushes a new tip and it recertifies, it
#      is admissible again.
#
# queue.sh eject's own withdrawal of a certified-unbatched bead is covered in
# test-queue-ops.sh, which already carries this file's REAL BD infrastructure.
#
# (batch.sh's own "second line of defence" — refusing a CERTIFIED bead whose store
# status was not closed, whatever the landstate said — was this file's case B. Retired
# with batch.sh, sp-uwhx0: no repo runs in `land=queue` mode, and the check was never
# ported. Case C used to exercise "not picked back up" through batch.sh's own admission
# sweep; it now checks queue_certified_list directly, lib.sh's selection primitive that
# any cutter — batch.sh before, the batcher now — draws from, since a WITHDRAWN
# landstate simply drops out of it, no sweep required.)
#
# covers: spira/lib.sh spira/conf.sh
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
# A2. sp-eiatd: the CERTIFIED/submitted-label carve-out is now a declared set
#     (lib.sh:_census_deliberate_reopen_causes), not a bare "work-close-converted"
#     literal. eject must NOT join the exemption — an ejected CERTIFIED bead still
#     needs WITHDRAWN and the label stripped (batch.sh's own comment: "every eject
#     strips the label") or a genuinely-ejected bead stays admissible to the next
#     cut. work-close-converted is the only cause that stays exempt.
# =============================================================================
echo
echo "bead_reopen: eject is deliberate for census but NOT admission-exempt; work-close-converted still is:"

labels_of() { "${TESTDB_BD:-bd}" -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
    | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(",".join(d[0].get("labels") or []))'; }

testdb_seed <<JSONL
{"id":"sp-wd-eject","title":"wd-eject","status":"closed","issue_type":"task","labels":["${SPIRA_SUBMITTED_LABEL:-spira-submitted}"],"updated_at":"2026-09-25T00:00:00Z","closed_at":"2026-09-25T00:00:00Z"}
{"id":"sp-wd-wcc","title":"wd-wcc","status":"closed","issue_type":"task","labels":["${SPIRA_SUBMITTED_LABEL:-spira-submitted}"],"updated_at":"2026-09-25T00:00:00Z","closed_at":"2026-09-25T00:00:00Z"}
JSONL

printf 'CERTIFIED %s %s\n' "$WDTIP" "$(date +%s)" > "$LANDSTATE/sp-wd-eject"
printf 'CERTIFIED %s %s\n' "$WDTIP" "$(date +%s)" > "$LANDSTATE/sp-wd-wcc"

bead_reopen sp-wd-eject eject >/dev/null 2>&1
_st=""; { read -r _st _ < "$LANDSTATE/sp-wd-eject"; } 2>/dev/null || true
is "eject: landstate WITHDRAWN (not admission-exempt)" "WITHDRAWN" "$_st"
nowant "eject: submitted label stripped" "${SPIRA_SUBMITTED_LABEL:-spira-submitted}" "$(labels_of sp-wd-eject)"

bead_reopen sp-wd-wcc work-close-converted >/dev/null 2>&1
_st=""; { read -r _st _ < "$LANDSTATE/sp-wd-wcc"; } 2>/dev/null || true
is "work-close-converted: landstate stays CERTIFIED (admission-exempt)" "CERTIFIED" "$_st"
want "work-close-converted: submitted label kept" "${SPIRA_SUBMITTED_LABEL:-spira-submitted}" "$(labels_of sp-wd-wcc)"

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
rm -f "$QUEUEDIR/$REPONAME/open"

# Reopen it — the same mechanism queue.sh eject, the sentinel, and slay.sh all
# reach through (bead_reopen, sourced above as part of this process's lib.sh).
bead_reopen sp-e2e recertify-needed "test: withdrawing sp-e2e for a fix" >/dev/null 2>&1
is "reopen: landstate WITHDRAWN" "WITHDRAWN" "$(awk '{print $1}' "$LANDSTATE/sp-e2e" 2>/dev/null)"

# queue_certified_list (lib.sh) is what any cutter — the batcher included — draws
# admissible branches from; a WITHDRAWN landstate simply does not match its CERTIFIED
# filter, no separate sweep needed.
nowant "not picked back up: excluded from what the next cut draws from" "sp-e2e " \
    "$(queue_certified_list "$REPO")"
is "still WITHDRAWN after the check" "WITHDRAWN" "$(awk '{print $1}' "$LANDSTATE/sp-e2e" 2>/dev/null)"

# The aeon pushes a new tip and it recertifies.
git -C "$REPO" checkout -q spira/sp-e2e
printf 'e2e-v2\n' >> "$REPO/sp-e2e.txt"
git -C "$REPO" add sp-e2e.txt && git -C "$REPO" commit -q -m "sp-e2e: fixed"
_e2e_tip2="$(git -C "$REPO" rev-parse spira/sp-e2e)"
git -C "$REPO" checkout -q main
bdq close sp-e2e --reason "test: recertified" >/dev/null 2>&1
printf 'CERTIFIED %s %s\n' "$_e2e_tip2" "$(date +%s)" > "$LANDSTATE/sp-e2e"

want "recertified: admissible again — back in what the next cut draws from" "sp-e2e " \
    "$(queue_certified_list "$REPO")"
is "recertified: landstate carries the new tip" "$_e2e_tip2" \
    "$(awk '{print $2}' "$LANDSTATE/sp-e2e" 2>/dev/null)"

echo
tl_summary
