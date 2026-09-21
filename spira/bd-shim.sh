#!/usr/bin/env bash
# bd-shim.sh — bd timing wrapper, injected into the test container's PATH.
#
# Records <wall_ms> <rc> <subcommand> to SPIRA_BD_TIMING_LOG for every bd
# call a suite makes.  gate-timing.sh sums the log after all suites finish
# to produce the per-suite bd_ms field in timing.tsv.
#
# The real bd is located by scanning PATH directories in order, skipping this
# script's own directory.  Exits with the real bd's exit code.
#
# If SPIRA_BD_TIMING_LOG is empty or unset, timing is not recorded but the
# call is still passed through — the shim never breaks a suite that calls bd.
#
# host-reason: not a suite; called inside the test container by bd-shim injection

_t0="$(date +%s%3N 2>/dev/null)"
case "${_t0:-}" in *[!0-9]*|'') _t0="$(date +%s)000" ;; esac

_shim_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" 2>/dev/null && pwd -P)"
_real_bd=""
_OIFS="$IFS"; IFS=':'
for _d in $PATH; do
    [ "${_d:-}" != "$_shim_dir" ] || continue
    [ -x "${_d:-}/bd" ] || continue
    _real_bd="${_d}/bd"
    break
done
IFS="$_OIFS"

if [ -z "$_real_bd" ]; then
    printf 'bd-shim: real bd not found in PATH\n' >&2
    exit 127
fi

"$_real_bd" "$@"
_rc=$?

_t1="$(date +%s%3N 2>/dev/null)"
case "${_t1:-}" in *[!0-9]*|'') _t1="$(date +%s)000" ;; esac
_ms=$(( _t1 - _t0 ))

_log="${SPIRA_BD_TIMING_LOG:-}"
if [ -n "$_log" ]; then
    printf '%d %d %s\n' "$_ms" "$_rc" "${1:-}" >> "$_log" 2>/dev/null || true
fi
exit "$_rc"
