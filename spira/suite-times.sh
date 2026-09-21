#!/usr/bin/env bash
# suite-times.sh — read the per-suite timing ledger and report.
#
# USAGE
#   suite-times.sh report [N] [--log FILE]
#     Print the top 20 suites by wall time, their bd share, and suites whose
#     wall time moved more than 25% against the previous run.
#
#     N           how many runs to look back (default: 2)
#     --log FILE  read from FILE instead of $SPIRA_RUN/suite-times.log
#
# TSV COLUMNS (tab-separated; lines starting with # are ignored)
#   run_id  branch  suite  rc  wall_secs  bd_calls  bd_ms  mode
#
# The suite name __batch__ is a reserved row written by testenv-batch.sh once
# per batch with wall_secs = end-to-end wall time; it is excluded from
# per-suite output but included in the per-run summary.
#
# covers: spira/testenv-batch.sh spira/suite-times.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/lib.sh"

SUBCMD="${1:-report}"
shift || true
case "$SUBCMD" in
    report) ;;
    *) printf 'usage: suite-times.sh report [N] [--log FILE]\n' >&2; exit 2 ;;
esac

N=2
LOG="${SPIRA_SUITE_TIMES_LOG:-$SPIRA_RUN/suite-times.log}"
while [ $# -gt 0 ]; do
    case "$1" in
        --log)   LOG="$2"; shift 2 ;;
        --log=*) LOG="${1#--log=}"; shift ;;
        [0-9]*)  N="$1"; shift ;;
        *)       printf 'suite-times.sh: unknown arg: %s\n' "$1" >&2; exit 2 ;;
    esac
done

[ -r "$LOG" ] || { printf 'suite-times.sh: no ledger at %s\n' "$LOG" >&2; exit 2; }

awk -F'\t' -v n_want="$N" -v top=20 -v pct=25 '
/^[[:space:]]*$/ || /^#/ { next }
NF < 8 { next }
{
    rid=$1; suite=$3; wall=$5+0; bd_ms=$7+0
    if (!(rid in seen)) { seen[rid]=1; runs[++nr]=rid }
    if (suite != "__batch__") {
        sw[suite,rid] = wall
        sb[suite,rid] = bd_ms
        if (!(suite in snames)) snames[suite]=1
    } else {
        batch_wall[rid] = wall
    }
}
END {
    if (nr == 0) { print "no data"; exit 0 }
    last = runs[nr]
    prev = (nr >= 2) ? runs[nr-1] : ""

    # Sum of suite walls and end-to-end wall for the last run
    sum_wall = 0
    for (s in snames) {
        k = s SUBSEP last
        if (k in sw) sum_wall += sw[k]
    }
    bw = (last in batch_wall) ? batch_wall[last] : "?"
    printf "run %s  sum %ds  wall %s%s\n", last, sum_wall, bw, (bw == "?" ? "" : "s")
    printf "\n"
    printf "Top %d suites by wall (last run)\n", top
    printf "  %-44s  %6s  %7s  %5s\n", "suite", "wall(s)", "bd(ms)", "bd%%"

    # Collect last-run walls into an indexed array for sorting
    n_sw = 0
    for (s in snames) {
        k = s SUBSEP last
        if (k in sw) { sw_sort_w[++n_sw] = sw[k]; sw_sort_s[n_sw] = s }
    }
    # Insertion sort (small N, no gawk needed)
    for (i = 2; i <= n_sw; i++) {
        j = i
        while (j > 1 && sw_sort_w[j] > sw_sort_w[j-1]) {
            tmp_w = sw_sort_w[j]; tmp_s = sw_sort_s[j]
            sw_sort_w[j] = sw_sort_w[j-1]; sw_sort_s[j] = sw_sort_s[j-1]
            sw_sort_w[j-1] = tmp_w; sw_sort_s[j-1] = tmp_s
            j--
        }
    }
    limit = (n_sw < top) ? n_sw : top
    for (i = 1; i <= limit; i++) {
        s = sw_sort_s[i]; w = sw_sort_w[i]
        bd = sb[s SUBSEP last]+0
        bd_pct = (w > 0) ? int(bd * 100.0 / (w * 1000.0)) : 0
        printf "  %-44s  %6d  %7d  %4d%%\n", s, w, bd, bd_pct
    }
    if (n_sw == 0) printf "  (no data)\n"

    if (prev != "") {
        printf "\nSuites with wall change > %d%% (%s vs %s)\n", pct, prev, last
        found = 0
        for (s in snames) {
            kl = s SUBSEP last; kp = s SUBSEP prev
            if ((kl in sw) && (kp in sw)) {
                curr = sw[kl]+0; prv = sw[kp]+0
                if (prv > 0) {
                    delta = (curr - prv) * 100.0 / prv
                    if (delta > pct || delta < -pct) {
                        printf "  %-44s  %ds -> %ds  (%+.0f%%)\n", s, prv, curr, delta
                        found++
                    }
                }
            }
        }
        if (!found) printf "  (none moved more than %d%%)\n", pct
    }
}
' "$LOG"
