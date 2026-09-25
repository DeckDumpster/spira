#!/usr/bin/env bash
#
# fast-suites.sh [--max-secs N] [--log FILE] — keep only the suites that are cheap enough
# for the batch pre-flight.
#
#   gate-touched.sh BASE HEAD | fast-suites.sh --max-secs 60
#
# Reads suite names on stdin, one per line, and writes back those whose MEDIAN recorded
# duration over their last 20 runs in SPIRA_SUITE_TIMES_LOG is at most N seconds (default
# SPIRA_PREFLIGHT_SUITE_MAX_SECS, else 60). A suite with no history is KEPT: a new suite is
# usually small, and the pre-flight's own wall bounds the cost of being wrong.
#
# Every suite it drops is named on stderr with its median, so a batch's log says exactly
# what the pre-flight did NOT cover — silence is never mistaken for coverage.
#
# The log is tab-separated: run-id, branch, suite, rc, seconds, ... (suites.sh and
# testenv-batch.sh write it); `__batch__` rows are whole-run totals and are ignored.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

max="${SPIRA_PREFLIGHT_SUITE_MAX_SECS:-60}"
log="${SPIRA_SUITE_TIMES_LOG:-${SPIRA_RUN:+$SPIRA_RUN/suite-times.log}}"
while [ $# -gt 0 ]; do
    case "$1" in
        --max-secs) max="${2:?--max-secs takes a number}"; shift 2 ;;
        --log)      log="${2:?--log takes a file}"; shift 2 ;;
        *) printf 'usage: fast-suites.sh [--max-secs N] [--log FILE]\n' >&2; exit 2 ;;
    esac
done
case "$max" in ''|*[!0-9]*) printf 'fast-suites.sh: --max-secs must be a whole number, got %s\n' "$max" >&2; exit 2 ;; esac

# No timing log at all: keep everything and say so. The wall still bounds the run.
if [ -z "${log:-}" ] || [ ! -r "$log" ]; then
    printf 'fast-suites.sh: no suite timing log (%s) — keeping every selected suite\n' "${log:-unset}" >&2
    cat
    exit 0
fi

# The program is passed with -c, NOT a heredoc: a heredoc would take stdin, and the suite
# list arrives on stdin.
python3 -c '
import sys, collections, statistics
log, cap = sys.argv[1], int(sys.argv[2])
want = [l.strip() for l in sys.stdin if l.strip()]
wanted = set(want)
hist = collections.defaultdict(lambda: collections.deque(maxlen=20))
with open(log, errors="replace") as f:
    for line in f:
        p = line.rstrip("\n").split("\t")
        if len(p) < 5 or p[2] not in wanted:
            continue
        try:
            hist[p[2]].append(int(float(p[4])))
        except ValueError:
            pass
for s in want:
    h = hist.get(s)
    if not h:
        print(s)
        continue
    med = statistics.median(h)
    if med <= cap:
        print(s)
    else:
        print("fast-suites.sh: dropped %s (median %.0fs over %d runs > %ds)" % (s, med, len(h), cap), file=sys.stderr)
' "$log" "$max"
