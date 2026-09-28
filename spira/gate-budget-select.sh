#!/usr/bin/env bash
#
# gate-budget-select.sh — trims a coverage-selected suite list to SPIRA_GATE_BUDGET, most
# specific first. This is the per-bead gate's own budget (test-plan-2026-09-23 §4.2: "cert
# keep 300 s; the target is all T1 plus the selected T2"); the full corpus stays the round's
# job (spira/testenv-batch.sh's own default selection never calls this script).
#
#   gate-touched.sh's coverage list | gate-budget-select.sh --budget-secs 300
#
# Reads candidate suite names on stdin, one per line — gate-touched.sh's own coverage-based
# selection (# covers:-matched suites and always-run/no-covers suites alike; ejected suites
# are added back by the caller AFTER this script runs and are never seen here, so a suite
# proven red never loses its rerun to a budget cut — law-a-retry-must-change-an-input).
#
# RANK, most specific first:
#   1. tier bucket: T0/T1 before T2/T3 (docs/test-plan/README.md) — an untagged suite
#      counts as T1, same rule suite-covers.sh's suite_tier_budget_ms uses.
#   2. specificity: fewer # covers: tokens outranks more — a suite naming the changed file
#      among a handful of others is a tighter claim on the diff than one that also names
#      dozens more. A suite with no # covers: line (always-run) is the least specific of
#      all: it makes no claim about what it covers, so a tight budget defers it to the round
#      first.
#   3. tagged (any UC-<area>-NN token in # covers:) before untagged, as a tiebreak within
#      equal specificity — the corpus is mid-migration (sp-5ayw5) so most suites carry no
#      tag yet.
#   4. suite name, for a total order and a deterministic result for a given input and
#      timing snapshot.
#
# FILL: walk the ranked candidates, adding each while the running predicted-seconds total,
# divided by --parallel-width, stays at or under --budget-secs. Predicted cost per suite is
# the P90 wall_secs over its last --runs (default 20) run/tsd/suite-timing rows — one
# tsd-query.sh call for the whole candidate set, not one per suite. A suite with no such row
# (or no reachable suite-timing family at all) counts at its tier's budget cap
# (suite_tier_budget_ms) — an unmeasured cost is never treated as free, the same rule
# tier-budget.sh already applies to a fresh suite.
#
# Every candidate dropped for budget is named on stderr with its predicted cost, the same
# convention fast-suites.sh already uses for its own cap, so a log already read for one drop
# reason reads the other. The last stderr line summarises: selected N, dropped M, predicted
# total — the gate's own verdict line, not a separate report.
#
# OPTIONS
#   --budget-secs N       required; the wall-clock budget in seconds
#   --parallel-width N    divides the running total before comparing to the budget (default
#                          1 — assume serial unless the caller knows the gate's own width)
#   --runs N              trailing window for the P90 query (default 20, fast-suites.sh's
#                          own default)
#   --suite-dir DIR       where to find candidate suite files (default: dir of this script)
#
# ENVIRONMENT
#   SPIRA_RUN             where run/tsd/suite-timing.jsonl lives. Unset, or the file
#                          unreachable, means every candidate counts at its tier cap — this
#                          script never sources conf.sh itself to derive a default, so a
#                          caller that does not set SPIRA_RUN gets a deterministic
#                          tier-cap-only result instead of an ambient config file's idea of
#                          where the run directory is.
#
# covers: spira/gate-touched.sh spira/tsd-query.sh spira/suite-covers.sh spira/tier-budget.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/suite-covers.sh"

SUITE_DIR="$HERE"
BUDGET=""
WIDTH=1
RUNS=20

while [ $# -gt 0 ]; do
    case "$1" in
        --budget-secs)    BUDGET="${2:?--budget-secs takes a number}"; shift 2 ;;
        --parallel-width) WIDTH="${2:?--parallel-width takes a number}"; shift 2 ;;
        --runs)           RUNS="${2:?--runs takes a number}"; shift 2 ;;
        --suite-dir)      SUITE_DIR="${2:?--suite-dir takes a path}"; shift 2 ;;
        *) printf 'gate-budget-select: unknown arg: %s\n' "$1" >&2; exit 2 ;;
    esac
done
case "$BUDGET" in
    ''|*[!0-9.]*) printf 'gate-budget-select: --budget-secs must be a number\n' >&2; exit 2 ;;
esac
case "$WIDTH" in ''|*[!0-9]*|0) WIDTH=1 ;; esac
case "$RUNS" in ''|*[!0-9]*) RUNS=20 ;; esac

cands="$(cat | grep -v '^$' | sort -u)"
if [ -z "$cands" ]; then
    printf 'gate-budget-select: selected 0, dropped 0, predicted 0s (budget %ss, width %s) — nothing to rank\n' \
        "$BUDGET" "$WIDTH" >&2
    exit 0
fi

# One tsd-query.sh pass for every candidate's P90 — never one per suite. Skipped entirely
# (never even shells out to tsd-query.sh, which sources conf.sh) unless SPIRA_RUN is set
# AND the family file it names actually exists: a caller that never set SPIRA_RUN gets a
# tier-cap-only result, not a probe of whatever conf.sh would otherwise resolve to.
declare -A P90
if [ -n "${SPIRA_RUN:-}" ] && [ -f "${SPIRA_RUN}/tsd/suite-timing.jsonl" ]; then
    while IFS=$'\t' read -r _p90_s _p90_v; do
        [ -n "$_p90_s" ] || continue
        P90["$_p90_s"]="$_p90_v"
    done < <(bash "$HERE/tsd-query.sh" suite-p90s "$RUNS" 2>/dev/null | python3 -c '
import sys, json
try:
    rows = json.loads(sys.stdin.read() or "[]")
except Exception:
    rows = []
for row in rows:
    if row.get("p90") is not None:
        print("%s\t%s" % (row["suite"], row["p90"]))
' 2>/dev/null || true)
fi

_tier_bucket() {  # _tier_bucket <tier> -> 0 (T0/T1, or untagged) or 1 (T2/T3)
    case "$1" in T0|T1|'') printf 0 ;; *) printf 1 ;; esac
}

# Build one rank line per candidate: "<bucket>\t<specificity>\t<taginv>\t<suite>\t<cost>\t<tier>"
ranked=""
while IFS= read -r s; do
    [ -n "$s" ] || continue
    f="$SUITE_DIR/$s"
    cov="$(suite_covers_of "$f")"
    tier="$(suite_tier_of "$f")"; [ -n "$tier" ] || tier=T1
    if [ -n "$cov" ]; then
        spec=0; tagged=0
        for _t in $cov; do
            spec=$((spec + 1))
            case "$_t" in UC-*) tagged=1 ;; esac
        done
    else
        spec=999999; tagged=0
    fi
    cost="${P90[$s]:-}"
    [ -n "$cost" ] || cost="$(awk -v ms="$(suite_tier_budget_ms "$tier")" 'BEGIN{printf "%.3f", ms/1000}')"
    ranked="$ranked$(printf '%s\t%s\t%s\t%s\t%s\t%s' \
        "$(_tier_bucket "$tier")" "$spec" "$((1 - tagged))" "$s" "$cost" "$tier")
"
done <<< "$cands"
ranked="$(printf '%s' "$ranked" | grep -v '^$')"

sorted="$(printf '%s\n' "$ranked" | sort -t"$(printf '\t')" -k1,1n -k2,2n -k3,3n -k4,4)"

selected=""; dropped=""; total="0"
while IFS=$'\t' read -r _bucket _spec _taginv s cost tier; do
    [ -n "$s" ] || continue
    would="$(awk -v t="$total" -v c="$cost" 'BEGIN{printf "%.3f", t + c}')"
    fits="$(awk -v w="$would" -v width="$WIDTH" -v b="$BUDGET" 'BEGIN{print (w/width<=b) ? 1 : 0}')"
    if [ "$fits" -eq 1 ]; then
        selected="$selected$s
"
        total="$would"
    else
        dropped="$dropped$s
"
        printf 'gate-budget-select: dropped %s (tier=%s predicted=%ss over budget)\n' "$s" "$tier" "$cost" >&2
    fi
done <<< "$sorted"

n_sel="$(printf '%s\n' "$selected" | grep -c . || true)"
n_drop="$(printf '%s\n' "$dropped" | grep -c . || true)"
predicted="$(awk -v t="$total" -v width="$WIDTH" 'BEGIN{printf "%.0f", t/width}')"
printf 'gate-budget-select: selected %s, dropped %s, predicted %ss (budget %ss, width %s)\n' \
    "$n_sel" "$n_drop" "$predicted" "$BUDGET" "$WIDTH" >&2

printf '%s\n' "$selected" | grep -v '^$'
exit 0
