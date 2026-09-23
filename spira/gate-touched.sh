#!/usr/bin/env bash
# gate-touched.sh <base> <head> — suite selector for the landing gate.
#
# Delegates to select.sh (the ONE selector) for coverage-based selection, then
# appends SPIRA_GATE_EJECTED_SUITES so re-certification re-runs suites that
# proved red (law-a-retry-must-change-an-input).
#
# When SPIRA_GATE_FILES names a readable file the pre-computed diff list from
# gate.sh is used; otherwise the diff is computed from BASE and HEAD.
#
# SPIRA_GATE_EJECTED_SUITES  comma-separated suite names always included.
# SPIRA_GATE_SELECT_CAP      maximum suite count (0 = no cap). Ejected suites
#                             are always kept; excluded suites are logged to stderr.
set -uo pipefail
BASE="${1:?usage: gate-touched.sh <base> <head>}"
# SPIRA_GATE_SUITES=off: select NOTHING, so the gate command's `[ -n "$_s" ] || exit 0`
# passes after its fences have run. landing.sh sets it for queue-mode certification when
# SPIRA_CERTIFY_SUITES=off; the batch's CI run is then where the suites run.
if [ "${SPIRA_GATE_SUITES:-on}" = off ]; then
    printf 'gate-touched: SPIRA_GATE_SUITES=off — no suites here; the batch CI run is the suite gate\n' >&2
    exit 0
fi
HEAD="${2:?usage: gate-touched.sh <base> <head>}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repo="${SPIRA_GATE_REPO:-.}"

if [ -f "${SPIRA_GATE_FILES:-}" ]; then
    # Build corpus from BASE tree so suites added by the branch are not self-selected
    # through coverage. Falls back to SUITE_DIR when BASE is not a resolvable ref.
    _suite_dir="${SPIRA_BATCH_SUITE_DIR:-$HERE}"
    _tmp_corpus="$(mktemp -d)"
    while IFS= read -r _sp; do
        [ -n "$_sp" ] || continue
        _sn="$(basename "$_sp")"
        [ -f "$_suite_dir/$_sn" ] && ln -s "$_suite_dir/$_sn" "$_tmp_corpus/$_sn" 2>/dev/null || true
    done < <(git -C "$repo" ls-tree -r "$BASE" --name-only 2>/dev/null \
        | grep '^spira/test-[^/]*\.sh$' || true)
    if [ -n "$(ls -A "$_tmp_corpus" 2>/dev/null)" ]; then
        _corpus="$_tmp_corpus"
    else
        rm -rf "$_tmp_corpus"
        _corpus="$_suite_dir"
    fi
    _covered="$(bash "$HERE/select.sh" --files "$SPIRA_GATE_FILES" --suite-dir "$_corpus" \
        --repo "$repo" --no-all-fallback 2>/dev/null || true)"
    [ "$_corpus" = "$_tmp_corpus" ] && rm -rf "$_tmp_corpus" 2>/dev/null || true
else
    _covered="$(bash "$HERE/select.sh" --base "$BASE" --head "$HEAD" --repo "$repo" \
        --no-all-fallback 2>/dev/null || true)"
fi

_ejected=""
if [ -n "${SPIRA_GATE_EJECTED_SUITES:-}" ]; then
    _ejected="$(printf '%s\n' "$SPIRA_GATE_EJECTED_SUITES" | tr ',' '\n' \
        | while IFS= read -r s; do
            [ -n "$s" ] && [ -f "spira/$s" ] && printf '%s\n' "$s"
          done)"
fi

_all="$(
    { printf '%s\n' "$_covered"; printf '%s\n' "$_ejected"; } \
    | grep -v '^$' | sort -u
)"

_cap="${SPIRA_GATE_SELECT_CAP:-0}"
case "$_cap" in ''|*[!0-9]*) _cap=0 ;; esac
if [ "$_cap" -gt 0 ] && [ -n "$_all" ]; then
    _total="$(printf '%s\n' "$_all" | grep -c . 2>/dev/null || printf '0')"
    if [ "$_total" -gt "$_cap" ]; then
        _ej_norm="$(printf '%s\n' "$_ejected" | grep -v '^$' | sort -u)"
        _ej_n=0; [ -n "$_ej_norm" ] && \
            _ej_n="$(printf '%s\n' "$_ej_norm" | grep -c . 2>/dev/null || printf '0')"
        _slots=$(( _cap - _ej_n ))
        [ "$_slots" -lt 0 ] && _slots=0
        _cv_not_ej="$(printf '%s\n' "$_covered" | grep -v '^$' | sort -u \
            | while IFS= read -r _s; do
                printf '%s\n' "$_ej_norm" | grep -qxF "$_s" || printf '%s\n' "$_s"
              done)"
        if [ "$_slots" -gt 0 ]; then
            _cv_kept="$(printf '%s\n' "$_cv_not_ej" | head -"$_slots")"
            _cv_excl="$(printf '%s\n' "$_cv_not_ej" | tail -n +"$((_slots + 1))")"
        else
            _cv_kept=""
            _cv_excl="$_cv_not_ej"
        fi
        [ -n "$_cv_excl" ] && printf 'gate-touched: cap=%d; excluded: %s\n' "$_cap" \
            "$(printf '%s\n' "$_cv_excl" | grep -v '^$' | tr '\n' ',' | sed 's/,$//')" >&2
        _all="$(
            { printf '%s\n' "$_ej_norm"; printf '%s\n' "$_cv_kept"; } \
            | grep -v '^$' | sort -u
        )"
    fi
fi

printf '%s\n' "$_all" | grep -v '^$'
exit 0
