#!/usr/bin/env bash
# gate-retry.sh <results-root> <rev> — re-run a failed batch's red suites once, serially.
#
# Red twice fails the gate. Red then green passes it and names each suite as flaky, so the
# flake is recorded without holding the release. Exits 1 without retrying when the first batch
# recorded no red suite: a failure this cannot attribute to a suite is not a flake.
#
# GATE_RETRY_MAX_RETRY (default: 5): skip the serial re-run when more than this many suites
# are genuinely red (non-timeout), or when they exceed half of all recorded results — whichever
# fires first. Timed-out suites (rc=124) never count toward the structural threshold; they are
# always re-run with a longer cap (GATE_RETRY_RERUN_TIMEOUT, default 1200 s).
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
BATCH="${GATE_RETRY_BATCH:-$HERE/testenv-batch.sh}"
ROOT="${1:?usage: gate-retry.sh <results-root> <rev>}"
REV="${2:?usage: gate-retry.sh <results-root> <rev>}"

red_in() {
    local f status out=""
    for f in "$1"/*/*.result; do
        [ -f "$f" ] || continue
        read -r status _ < "$f"
        case "$status" in red|timeout) out="$out $(basename "$f" .result)" ;; esac
    done
    printf '%s' "${out# }"
}

reds="$(red_in "$ROOT")"
if [ -z "$reds" ]; then
    printf 'gate-retry: no red suite recorded under %s — nothing to retry\n' "$ROOT" >&2
    exit 1
fi

_red_count=0
for _r in $reds; do _red_count=$((_red_count+1)); done
_total_count=0
_hard_red_count=0
_timeout_count=0
for _f in "$ROOT"/*/*.result; do
    [ -f "$_f" ] || continue
    _total_count=$((_total_count+1))
    read -r _s _ < "$_f"
    case "$_s" in
        red)     _hard_red_count=$((_hard_red_count+1)) ;;
        timeout) _timeout_count=$((_timeout_count+1)) ;;
    esac
done
: "${GATE_RETRY_MAX_RETRY:=5}"
_structural=0
[ "$_hard_red_count" -gt "$GATE_RETRY_MAX_RETRY" ] && _structural=1
[ "$_structural" -eq 0 ] && [ "$_total_count" -gt "$GATE_RETRY_MAX_RETRY" ] && \
    [ $((_hard_red_count * 2)) -gt "$_total_count" ] && _structural=1
if [ "$_structural" -eq 1 ]; then
    printf 'gate-retry: %d of %d suites red — structural, not flaky; serial re-run skipped\n' \
        "$_red_count" "$_total_count" >&2
    for _s in $reds; do printf '::error title=red suite::%s\n' "$_s"; done
    exit 1
fi

printf 'gate-retry: re-running serially: %s\n' "$reds"
rc=0
if [ "$_timeout_count" -gt 0 ]; then
    printf '%s\n' $reds | SPIRA_SUITE_TIMEOUT="${GATE_RETRY_RERUN_TIMEOUT:-1200}" \
        SPIRA_BATCH_RESULTS="$ROOT-retry" bash "$BATCH" --mode serial --suites - "$REV" || rc=$?
else
    printf '%s\n' $reds | SPIRA_BATCH_RESULTS="$ROOT-retry" bash "$BATCH" --mode serial --suites - "$REV" || rc=$?
fi
case "$rc" in
    0)  for s in $reds; do printf '::warning title=flaky suite::%s was red, then green on a serial re-run\n' "$s"; done
        printf 'gate-retry: flaky, not failing: %s\n' "$reds"
        exit 0 ;;
    2|3) exit "$rc" ;;
    *)  _reds_retry="$(red_in "$ROOT-retry")"
        printf 'gate-retry: red twice: %s\n' "$_reds_retry" >&2
        for _s in $_reds_retry; do
            printf '::error title=red-twice suite::%s\n' "$_s"
        done
        exit 1 ;;
esac
