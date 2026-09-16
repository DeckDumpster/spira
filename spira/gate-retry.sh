#!/usr/bin/env bash
# gate-retry.sh <results-root> <rev> — re-run a failed batch's red suites once, serially.
#
# Red twice fails the gate. Red then green passes it and names each suite as flaky, so the
# flake is recorded without holding the release. Exits 1 without retrying when the first batch
# recorded no red suite: a failure this cannot attribute to a suite is not a flake.
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

printf 'gate-retry: re-running serially: %s\n' "$reds"
rc=0
printf '%s\n' $reds | SPIRA_BATCH_RESULTS="$ROOT-retry" bash "$BATCH" --mode serial --suites - "$REV" || rc=$?
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
