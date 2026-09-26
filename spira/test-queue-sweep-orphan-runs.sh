#!/usr/bin/env bash
#
# test-queue-sweep-orphan-runs.sh — queue_sweep_orphan_runs (lib.sh) cancels a Gate run
# orphaned by a batch PR closed outside queue_cancel_branch_runs (a hand-close, or a run
# left over from before that guard existed), and leaves alone any branch whose PR is
# still open — the live batch's own run in particular (sp-1p04d).
#
# What forge.sh's runs-queue-branches itself excludes — a non-spira/queue/* branch, i.e.
# main's push gate — is covered in test-forge-orphan-runs.sh; this suite is the sweep's own
# decision once handed a list of candidate runs.
#
# covers: spira/lib.sh spira/forge.sh spira/batch.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-queue-sweep-orphan-runs.sh"
TMP="$(mktemp -d)"
export SPIRA_DB="${SPIRA_DB:-$TMP/no-such-db}" SPIRA_RUN="$TMP/run"
trap 'rm -rf "$TMP"' EXIT INT TERM
# shellcheck disable=SC1090
. "$HERE/lib.sh"

FORGE="$TMP/forge-fake.sh"

# Fake forge: runs-queue-branches answers from QUEUE_BRANCHES_FILE ("<id> <branch>
# <status>" per line); pr-number reports a PR open only for a branch listed in
# OPEN_BRANCHES_FILE (empty/absent = closed, matching the real forge on a closed PR);
# run-cancel records the id it was asked to cancel and fails when CANCEL_FAIL exists.
cat > "$FORGE" <<'FAKE'
#!/usr/bin/env bash
cmd="${1:-}"; shift; repo="${1:-}"; shift
case "$cmd" in
    runs-queue-branches)
        [ -f "$QUEUE_BRANCHES_FILE" ] && cat "$QUEUE_BRANCHES_FILE"
        ;;
    pr-number)
        branch="${1:-}"
        printf '%s\n' "$branch" >> "$PRQUERY_LOG"
        if [ -f "$OPEN_BRANCHES_FILE" ] && grep -qxF "$branch" "$OPEN_BRANCHES_FILE" 2>/dev/null; then
            printf '999\n'
        fi
        ;;
    run-cancel)
        run_id="${1:-}"
        printf '%s\n' "$run_id" >> "$CANCEL_LOG"
        [ -f "$CANCEL_FAIL" ] && exit 1
        exit 0
        ;;
    *) printf 'forge-fake: unknown command: %s\n' "$cmd" >&2; exit 1 ;;
esac
FAKE
chmod +x "$FORGE"

export QUEUE_BRANCHES_FILE="$TMP/queue-branches"
export OPEN_BRANCHES_FILE="$TMP/open-branches"
export CANCEL_FAIL="$TMP/cancel-fail"
export CANCEL_LOG="$TMP/cancel.log"
export PRQUERY_LOG="$TMP/pr-query.log"

echo
echo "sweep cancels a planted orphan run and ignores the live batch's own run:"
cat > "$QUEUE_BRANCHES_FILE" <<J
6001 spira/queue/20260924T000000Z orphan-status
6002 spira/queue/20260924T010000Z live-status
J
# Only the live batch's branch has an open PR.
printf 'spira/queue/20260924T010000Z\n' > "$OPEN_BRANCHES_FILE"
: > "$CANCEL_LOG"; : > "$PRQUERY_LOG"
: > "$SPIRA_RUN/landing.log"

queue_sweep_orphan_runs "$FORGE" "$TMP/repo"
rc=$?
is "sweep exits 0 when every cancel succeeds" "0" "$rc"

want "both branches were checked for an open PR" \
    "spira/queue/20260924T000000Z" "$(cat "$PRQUERY_LOG")"
want "both branches were checked for an open PR" \
    "spira/queue/20260924T010000Z" "$(cat "$PRQUERY_LOG")"

cancel_calls="$(cat "$CANCEL_LOG" 2>/dev/null)"
want   "the orphan run (6001) is cancelled"        "6001" "$cancel_calls"
nowant "the live batch's run (6002) is left alone" "6002" "$cancel_calls"

want "the cancel is recorded in landing.log" \
    "SWEEP_CANCEL " "$(cat "$SPIRA_RUN/landing.log")"
want "landing.log names the orphan branch" \
    "spira/queue/20260924T000000Z" "$(cat "$SPIRA_RUN/landing.log")"
nowant "landing.log does not name the live batch's branch" \
    "spira/queue/20260924T010000Z" "$(cat "$SPIRA_RUN/landing.log")"

echo
echo "no orphans: sweep queries and cancels nothing:"
: > "$QUEUE_BRANCHES_FILE"
: > "$CANCEL_LOG"; : > "$SPIRA_RUN/landing.log"
queue_sweep_orphan_runs "$FORGE" "$TMP/repo"
is "sweep exits 0 with nothing to do" "0" "$?"
is "no cancel was attempted" "" "$(cat "$CANCEL_LOG")"

echo
echo "a failed cancel is logged loudly and does not stop the sweep:"
cat > "$QUEUE_BRANCHES_FILE" <<J
6003 spira/queue/20260924T020000Z stuck-status
J
: > "$OPEN_BRANCHES_FILE"
: > "$CANCEL_LOG"; : > "$SPIRA_RUN/landing.log"
: > "$CANCEL_FAIL"
err="$(queue_sweep_orphan_runs "$FORGE" "$TMP/repo" 2>&1 1>/dev/null)"
rc=$?
is  "sweep reports a non-zero exit on a failed cancel" "1" "$rc"
want "the failure is reported loudly on stderr" "WARN" "$err"
want "the failure names the run" "6003" "$err"
want "the failure is recorded in landing.log" \
    "SWEEP_CANCEL_FAILED " "$(cat "$SPIRA_RUN/landing.log")"
rm -f "$CANCEL_FAIL"
tl_summary
