#!/usr/bin/env bash
#
# fast-suites.sh [--max-secs N] [--runs N] — keep only the suites that are cheap enough for
# the batch pre-flight.
#
#   gate-touched.sh BASE HEAD | fast-suites.sh --max-secs 60
#
# Reads suite names on stdin, one per line, and writes back those whose MEDIAN wall_secs over
# their last --runs (default 20) run/tsd/ suite-timing rows is at most N seconds (default
# SPIRA_PREFLIGHT_SUITE_MAX_SECS, else 60) — local and CI runs counted together, since both
# write the same family. A suite with no history is KEPT: a new suite is usually
# small, and the pre-flight's own wall bounds the cost of being wrong.
#
# Every suite it drops is named on stderr with its median, so a batch's log says exactly what
# the pre-flight did NOT cover — silence is never mistaken for coverage.
#
# ONE QUERY, not one per suite: tsd-query.sh suite-medians computes every suite's median in a
# single DuckDB pass; this just filters that against the wanted set.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

max="${SPIRA_PREFLIGHT_SUITE_MAX_SECS:-60}"
runs=20
while [ $# -gt 0 ]; do
    case "$1" in
        --max-secs) max="${2:?--max-secs takes a number}"; shift 2 ;;
        --runs)     runs="${2:?--runs takes a number}"; shift 2 ;;
        *) printf 'usage: fast-suites.sh [--max-secs N] [--runs N]\n' >&2; exit 2 ;;
    esac
done
case "$max" in ''|*[!0-9]*) printf 'fast-suites.sh: --max-secs must be a whole number, got %s\n' "$max" >&2; exit 2 ;; esac

want="$(cat)"

medians="$(bash "$HERE/tsd-query.sh" suite-medians "$runs" 2>/dev/null || true)"

# No suite-timing history at all: keep everything and say so. The wall still bounds the run.
if [ -z "$medians" ]; then
    printf 'fast-suites.sh: no suite-timing history yet — keeping every selected suite\n' >&2
    printf '%s\n' "$want"
    exit 0
fi

printf '%s' "$want" | python3 -c '
import sys, json
medians_json, cap = sys.argv[1], int(sys.argv[2])
want = [l.strip() for l in sys.stdin if l.strip()]
hist = {}
try:
    for row in json.loads(medians_json):
        hist[row["suite"]] = (row["median"], row["n"])
except Exception:
    pass
for s in want:
    h = hist.get(s)
    if not h:
        print(s)
        continue
    med, n = h
    if med <= cap:
        print(s)
    else:
        print("fast-suites.sh: dropped %s (median %.0fs over %d runs > %ds)" % (s, med, n, cap), file=sys.stderr)
' "$medians" "$max"
