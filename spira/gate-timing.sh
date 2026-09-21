#!/usr/bin/env bash
# gate-timing.sh <results-dir> <verdict>
#
# Append one timing row to the batch timing ledger for a completed gate run.
# Called by testenv-batch.sh after green and red verdicts.
#
# LEDGER FIELDS (tab-separated, no header line):
#   run_id  when  branch  base  verdict  wall_s
#   n_ok  n_red  n_skip  n_other  sum_s
#   maxpar  nproc  memtotal_kb  cpu_busy_pct
#
# wall_s is the wall-clock seconds of the suites step only (not the full batch
# setup time).  sum_s is the sum of per-suite seconds across all suites that
# produced a result file; at maxpar workers, sum_s / wall_s approximates the
# effective parallelism.
#
# ENVIRONMENT
#   SPIRA_BATCH_LEDGER  path to the ledger file
#                       (default: $SPIRA_RUN/batch-timing.tsv)
#   SPIRA_RUN           used for the ledger path default
#
# covers: spira/gate-timing.sh spira/testenv-batch.sh
set -uo pipefail

RESULTS="${1:?gate-timing: usage: gate-timing.sh <results-dir> <verdict>}"
VERDICT="${2:?gate-timing: usage: gate-timing.sh <results-dir> <verdict>}"

LEDGER="${SPIRA_BATCH_LEDGER:-${SPIRA_RUN:-}/batch-timing.tsv}"
[ -n "$LEDGER" ] || {
    printf 'gate-timing: set SPIRA_BATCH_LEDGER or SPIRA_RUN\n' >&2; exit 1
}
[ -d "$RESULTS" ] || {
    printf 'gate-timing: results dir not found: %s\n' "$RESULTS" >&2; exit 1
}

_kv() { awk -F= -v k="$1" '$1==k{print $2; exit}' "$2" 2>/dev/null; }

run_id="-"; branch="-"; base="-"
if [ -r "$RESULTS/batch.meta" ]; then
    _v="$(_kv key    "$RESULTS/batch.meta")"; [ -n "${_v:-}" ] && run_id="$_v"
    _v="$(_kv branch "$RESULTS/batch.meta")"; [ -n "${_v:-}" ] && branch="$_v"
    _v="$(_kv base   "$RESULTS/batch.meta")"; [ -n "${_v:-}" ] && base="$_v"
fi

nproc="-"; memtotal_kb="-"; maxpar="-"; cpu_busy_pct="-"; wall_s="-"
if [ -r "$RESULTS/runner.meta" ]; then
    _v="$(_kv nproc         "$RESULTS/runner.meta")"; [ -n "${_v:-}" ] && nproc="$_v"
    _v="$(_kv memtotal_kb   "$RESULTS/runner.meta")"; [ -n "${_v:-}" ] && memtotal_kb="$_v"
    _v="$(_kv maxpar        "$RESULTS/runner.meta")"; [ -n "${_v:-}" ] && maxpar="$_v"
    _v="$(_kv cpu_busy_pct  "$RESULTS/runner.meta")"; [ -n "${_v:-}" ] && cpu_busy_pct="$_v"
    _v="$(_kv suites_wall_s "$RESULTS/runner.meta")"; [ -n "${_v:-}" ] && wall_s="$_v"
fi

n_ok=0; n_red=0; n_skip=0; n_other=0; sum_s=0
for _f in "$RESULTS"/*.result; do
    [ -f "$_f" ] || continue
    _st="$(awk '{print $1}' "$_f" 2>/dev/null)" || continue
    [ -n "${_st:-}" ] || continue
    _s="$(awk '{print $3}' "$_f" 2>/dev/null)"; case "${_s:-x}" in ''|*[!0-9]*) _s=0 ;; esac
    case "$_st" in
        ok)                     n_ok=$(( n_ok+1   )); sum_s=$(( sum_s+_s )) ;;
        red|timeout)            n_red=$(( n_red+1  )); sum_s=$(( sum_s+_s )) ;;
        skip|skip-req|disabled) n_skip=$(( n_skip+1 ))                       ;;
        *)                      n_other=$(( n_other+1 )); sum_s=$(( sum_s+_s )) ;;
    esac
done

when="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
mkdir -p "$(dirname "$LEDGER")"
printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$run_id" "$when" "$branch" "$base" "$VERDICT" \
    "$wall_s" "$n_ok" "$n_red" "$n_skip" "$n_other" "$sum_s" \
    "$maxpar" "$nproc" "$memtotal_kb" "$cpu_busy_pct" \
    >> "$LEDGER"
