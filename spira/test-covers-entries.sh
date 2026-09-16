#!/usr/bin/env bash
# covers: spira/suite-covers.sh spira/test-*.sh
# hermetic-ok: reads suite source and git-tracked paths; no database, no systemd
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/suite-covers.sh"

REPO="${SPIRA_REPO:-$(cd "$HERE/.." && pwd -P)}"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "$2"; }

printf 'test-covers-entries.sh\n'

_tracked_arr=()
while IFS= read -r _tp; do
    [ -n "$_tp" ] && _tracked_arr+=("$_tp")
done < <(git -C "$REPO" ls-files 2>/dev/null)
[ "${#_tracked_arr[@]}" -gt 0 ] || { printf '  FAIL — git ls-files returned nothing in %s\n' "$REPO"; exit 1; }

# --- positive control -------------------------------------------------------
# Plant a bad token (parenthetical function annotation) and require it to be caught
# before trusting a clean result on real suites.
_tmpdir="$(mktemp -d)"; trap 'rm -rf "$_tmpdir"' EXIT
printf '#!/bin/bash\n# covers: spira/lib.sh (pc_bad)\n' > "$_tmpdir/test-pc.sh"

_pc_found=0
set -f
for _tok in $(suite_covers_of "$_tmpdir/test-pc.sh"); do
    _hit=0
    for _path in "${_tracked_arr[@]}"; do
        case "$_path" in $_tok) _hit=1; break ;; esac
    done
    [ "$_hit" -eq 0 ] && _pc_found=1
done
set +f
[ "$_pc_found" -eq 1 ] && ok "positive control — unresolvable token is detected" \
    || bad "positive control" "(pc_bad) must not match any tracked path"

# --- real suites ------------------------------------------------------------
_checked=0; _skipped=0; _bad=0
for _sf in "$HERE"/test-*.sh; do
    [ -f "$_sf" ] || continue
    _cov="$(suite_covers_of "$_sf")"
    if [ -z "$_cov" ]; then _skipped=$((_skipped+1)); continue; fi
    _checked=$((_checked+1))
    set -f
    for _tok in $_cov; do
        _hit=0
        for _path in "${_tracked_arr[@]}"; do
            case "$_path" in $_tok) _hit=1; break ;; esac
        done
        if [ "$_hit" -eq 0 ]; then
            bad "$(basename "$_sf")" "# covers: token '$_tok' matches no tracked path"
            _bad=$((_bad+1))
        fi
    done
    set +f
done
[ "$_bad" -eq 0 ] \
    && ok "$_checked suite(s) with declarations all resolve; $_skipped without" \
    || printf '  %d unresolvable token(s) across %d suite(s)\n' "$_bad" "$_checked"

printf '\ntest-covers-entries.sh: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
