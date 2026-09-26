#!/usr/bin/env bash
#
# test-batch-red-main.sh — main's own push-gate state carries no back pressure on
#   the merge queue: neither the CUT (batch.sh) nor the LANDING fast-forward
#   (verdict.sh) ever holds on it, whatever it reads.
#
# THE PROPERTY UNDER TEST (sp-x54re, superseding sp-221n8/sp-wmn0w). The queue's
# only back pressure is whether a batch PR is open for the repo — not the state of
# a gate that tested a different tree than the one about to cut or land. Detecting
# a red main is still wanted (czar-pass's base-red stage, sp-tb5jp), but doing it
# HERE duplicated that detection as a rejection (law-detection-outranks-rejection),
# so both holds are gone along with the forge.sh verb they read: main-gate-status no
# longer exists, and neither batch.sh nor verdict.sh calls the forge at all for this.
#
# SEEN HELD WITHOUT THE FIX. Against the pre-sp-x54re batch.sh/verdict.sh, every
# "no HOLD" and "never asked the forge" assertion below fails: red or unknown holds
# the cut, and red, unknown or pending holds the landing fast-forward
# (law-a-regression-test-must-be-seen-to-fail).
#
# tier: T2
# covers: spira/batch.sh spira/verdict.sh spira/forge.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-batch-red-main
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up batchredmain || { echo "test-batch-red-main: could not build fixture database"; exit 1; }

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
cp "$HERE"/*.sh "$SH/"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

cat > "$SH/mail.sh" <<'MAIL'
#!/usr/bin/env bash
exit 0
MAIL
chmod +x "$SH/mail.sh"

# Forge stub. CALL_LOG records every command name asked of it, so a test can prove
# the forge was never consulted about main's gate state at all — not just that the
# answer was ignored. main-gate-status is stubbed anyway (answering whatever
# FIXTURE_GATE_STATUS says) so a caller that regresses and asks again still gets a
# deterministic answer instead of the fixture's unknown-command fallback.
CALL_LOG="$TMP/call-log"; : > "$CALL_LOG"
FORGE_LOG="$TMP/forge-log"; : > "$FORGE_LOG"
cat > "$SH/forge-fixture.sh" <<'FORGE'
#!/usr/bin/env bash
cmd="${1:-}"; shift; shift  # skip repo arg
printf '%s\n' "$cmd" >> "$CALL_LOG"
case "$cmd" in
    runs-active) printf '1\n' ;;
    main-gate-status) printf '%s\n' "${FIXTURE_GATE_STATUS:-green deadbeef}" ;;
    # Report the open batch's head the way the real forge does: verdict.sh now refuses to
    # land a green batch whose check-status omits head-sha (sp-2711c).
    check-status) printf '%s\n' "${FIXTURE_CHECK_STATUS:-green}"
        _h="$(sed -n 's/^head=//p' "${SPIRA_QUEUE_DIR:-/nonexistent}"/*/open 2>/dev/null | head -1)"
        [ -n "$_h" ] && printf 'head-sha: %s\n' "$_h" ;;
    pr-create)
        n=$(( $(wc -l < "$FORGE_LOG" 2>/dev/null || echo 0) + 1 ))
        printf '%s\n' "$n" >> "$FORGE_LOG"
        printf '%s\n' "$n"
        ;;
    *) exit 0 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"
export CALL_LOG

# BATCH_MAX and BATCH_WAIT are huge; express is the only ordinary trigger allowed to
# fire, so every cut case here is testing what main's gate state does (nothing) to a
# batch that has already been triggered.
batch() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_MAX=99 \
    SPIRA_QUEUE_BATCH_WAIT=999999 \
    SPIRA_QUEUE_BATCH_IDLE_CUT=0 \
    SPIRA_EXPRESS_LABEL=express \
    FIXTURE_GATE_STATUS="${GATE_STATUS:-green deadbeef}" \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
        bash "$SH/batch.sh" "$@" 2>&1
}

# verdict() drives the LANDING half (verdict.sh) against whatever batch is
# currently open — built by build_batch below. FIXTURE_CHECK_STATUS is pinned to
# green: these cases test only what main's gate state (FIXTURE_GATE_STATUS) does
# to the landing decision, never the batch's own CI result.
verdict() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_CI_MAXSEC=3600 \
    SPIRA_QUEUE_CI_IDLE_SEC=600 \
    SPIRA_QUEUE_INFRA_RETRIES=2 \
    FIXTURE_GATE_STATUS="${GATE_STATUS:-green deadbeef}" \
    FIXTURE_CHECK_STATUS="${CHECK_STATUS:-green}" \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
        bash "$SH/verdict.sh" "$@" 2>&1
}

# Seed a LANDED anchor so the stuck-queue check has something to read.
NOW="$(date +%s)"
printf 'LANDED fakeshafakeshafakeshafakeshafakeshafake %s\n' "$(( NOW - 60 ))" > "$LANDSTATE/anchor"

testdb_reset
testdb_seed <<JSONL
{"id":"sp-plain1","title":"plain express bead","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME","express"],"updated_at":"2026-09-23T00:00:00Z"}
JSONL

git -C "$REPO" checkout -q -b spira/sp-plain1 main
printf 'p\n' > "$REPO/plain1.txt"
git -C "$REPO" add plain1.txt && git -C "$REPO" commit -q -m "sp-plain1: work"
tip_p="$(git -C "$REPO" rev-parse spira/sp-plain1)"
git -C "$REPO" checkout -q main

certify_plain() { printf 'CERTIFIED %s %s' "$tip_p" "$NOW" > "$LANDSTATE/sp-plain1"; }
clear_batch()   { rm -f "$QUEUEDIR/$REPONAME/open"; : > "$FORGE_LOG"; }
landing_log()   { cat "$RUN/landing.log" 2>/dev/null; }
clear_log()     { : > "$RUN/landing.log"; }
clear_calls()   { : > "$CALL_LOG"; }
remote_main()   { git -C "$REMOTE" rev-parse main 2>/dev/null; }

# build_batch <id:tip> [<id:tip> ...] — write an open batch record directly
# (bypassing batch.sh's own gate/cut) so the LANDING cases below can drive
# verdict.sh against a known batch shape without paying for a real gate run or
# risking a same-second branch-name collision from repeated real cuts.
build_batch() {
    local base_sha; base_sha="$(git -C "$REPO" rev-parse origin/main)"
    local wt="$RUN/worktree/.batch-build"
    git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    git -C "$REPO" worktree add -q --detach "$wt" "$base_sha"
    local members=() spec id tip now; now="$(date +%s)"
    for spec in "$@"; do
        id="${spec%%:*}"; tip="${spec##*:}"
        git -C "$wt" merge -q --no-edit --no-ff -m "spira: land $id" "$tip" >/dev/null 2>&1
        members+=("$id:$tip")
        printf 'BATCHED %s %s\n' "$tip" "$now" > "$LANDSTATE/$id"
    done
    local batch_head; batch_head="$(git -C "$wt" rev-parse HEAD)"
    git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    local branch="spira/queue/build-$$-${RANDOM}"
    git -C "$REPO" branch -f "$branch" "$batch_head" 2>/dev/null
    git -C "$REPO" push -q origin "$branch" 2>/dev/null
    {
        printf 'pr=99\n'
        printf 'head=%s\n'    "$batch_head"
        printf 'base=%s\n'    "$base_sha"
        printf 'members=%s\n' "${members[*]}"
        printf 'opened=%s\n'  "$now"
        printf 'branch=%s\n'  "$branch"
    } > "$QUEUEDIR/$REPONAME/open"
    printf '%s\n' "$batch_head"
}

echo "test-batch-red-main.sh"

# =============================================================================
# CUT (batch.sh): no gate state ever holds it.
# =============================================================================
for status in 'green deadbeef' 'pending abc123' 'red badc0de' 'unknown'; do
    certify_plain; clear_batch; clear_log; clear_calls
    out="$(GATE_STATUS="$status" batch "$REPONAME")"
    is     "cut [$status]: batch opened"           "1" "$(ls "$QUEUEDIR/$REPONAME/open" 2>/dev/null | wc -l)"
    is     "cut [$status]: landstate BATCHED"      "BATCHED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-plain1")"
    nowant "cut [$status]: no HOLD in landing.log" "QUEUE HOLD"      "$(landing_log)"
    nowant "cut [$status]: forge never asked about main's gate" "main-gate-status" "$(cat "$CALL_LOG")"
done

# =============================================================================
# LANDING (verdict.sh): no gate state ever holds the fast-forward.
# =============================================================================
for status in 'green deadbeef' 'pending abc123' 'red badc0de' 'unknown'; do
    clear_batch; clear_log; clear_calls
    batch_head="$(build_batch "sp-plain1:$tip_p")"
    out="$(GATE_STATUS="$status" CHECK_STATUS='green' verdict "$REPONAME")"
    is     "landing [$status]: fast-forwarded" "$batch_head" "$(remote_main)"
    is     "landing [$status]: batch closed"   "0" "$(ls "$QUEUEDIR/$REPONAME/open" 2>/dev/null | wc -l)"
    is     "landing [$status]: sp-plain1 LANDED" "LANDED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-plain1")"
    want   "landing [$status]: reported landed"  "landed by fast-forward" "$out"
    nowant "landing [$status]: no HOLD in landing.log" "QUEUE HOLD" "$(landing_log)"
    nowant "landing [$status]: forge never asked about main's gate" "main-gate-status" "$(cat "$CALL_LOG")"
    git -C "$REPO" fetch -q origin
done

tl_summary
