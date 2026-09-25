#!/usr/bin/env bash
# tier: T1
# covers: spira/suite-covers.sh spira/test-*.sh
# hermetic-ok: reads suite source and filesystem; no database, no systemd
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/suite-covers.sh"

ROOT="$(cd "$HERE/.." && pwd -P)"

printf 'test-covers-entries.sh\n'

# covers_hit <token>: exits 0 if any path under ROOT matches the token as a bash glob.
# $token must be unquoted when passed to `for` so the shell expands wildcards.
# A UC-<area>-NN token names a use-case catalogue entry (sp-qu948) and a G-NN token
# names a gap row (docs/test-plan/*.md section 6); neither is a file, so neither is
# resolved as a path. plan-lint.sh validates UC ids, not G ids; this suite trusts both.
covers_hit() {
    local tok="$1" _f
    case "$tok" in UC-*-[0-9][0-9]|G-[0-9][0-9]) return 0 ;; esac
    for _f in "$ROOT/"$tok; do
        [ -e "$_f" ] && return 0
    done
    return 1
}

# is_catalogue_token <token>: exits 0 for a UC-<area>-NN or G-NN token (never a file path).
is_catalogue_token() {
    case "$1" in UC-*-[0-9][0-9]|G-[0-9][0-9]) return 0 ;; esac
    return 1
}

# --- positive control -------------------------------------------------------
# Plant a bad token and require the checker to find it before trusting a silent result.
_tmpdir="$(mktemp -d)"; trap 'rm -rf "$_tmpdir"' EXIT
printf '#!/bin/bash\n# covers: spira/lib.sh (pc_bad)\n' > "$_tmpdir/test-pc.sh"

_pc_found=0
_cov="$(suite_covers_of "$_tmpdir/test-pc.sh")"
read -ra _toks <<< "$_cov"
for _tok in "${_toks[@]}"; do
    covers_hit "$_tok" || _pc_found=1
done
[ "$_pc_found" -eq 1 ] && ok "positive control — unresolvable token is detected" \
    || bad "positive control" "(pc_bad) must not match any file under $ROOT"

# --- real suites ------------------------------------------------------------
_checked=0; _skipped=0; _bad=0
for _sf in "$HERE"/test-*.sh; do
    [ -f "$_sf" ] || continue
    _cov="$(suite_covers_of "$_sf")"
    if [ -z "$_cov" ]; then _skipped=$((_skipped+1)); continue; fi
    _checked=$((_checked+1))
    read -ra _toks <<< "$_cov"
    for _tok in "${_toks[@]}"; do
        is_catalogue_token "$_tok" && continue
        if ! covers_hit "$_tok"; then
            bad "$(basename "$_sf")" "# covers: token '$_tok' matches no file under $ROOT"
            _bad=$((_bad+1))
        fi
    done
done
[ "$_bad" -eq 0 ] \
    && ok "$_checked suite(s) with declarations all resolve; $_skipped without" \
    || printf '  %d unresolvable token(s) across %d suite(s)\n' "$_bad" "$_checked"
tl_summary
