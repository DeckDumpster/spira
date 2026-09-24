#!/usr/bin/env bash
# gate-diag.sh <results-root> — diagnostic output for a finished batch run.
#
# For each red or timeout suite: prints failing lines and the last N lines of
# its .out in a collapsible group (CI) or inline block (local); emits one
# ::error annotation per red suite when GITHUB_ACTIONS is set; prints a summary
# table to stdout; writes the same table to GITHUB_STEP_SUMMARY when that is set.
#
# Retry results are read from <results-root>-retry, classifying each suite as
# red-red or red-green (flake).
#
# covers: spira/gate-diag.sh .github/workflows/gate.yml spira/testenv-batch.sh
set -uo pipefail

ROOT="${1:?usage: gate-diag.sh <results-root>}"
TAIL="${SPIRA_BATCH_TAIL_LINES:-50}"
IN_GHA="${GITHUB_ACTIONS:-}"
RETRY="${ROOT}-retry"

# Collect red/timeout suites. Scan one level ($ROOT/*.result) and one level
# down ($ROOT/*/*.result) so the same script works for both a leaf directory
# passed directly from testenv-batch.sh and a root directory passed from CI.
_collect_reds() {
    local f st suite
    for f in "$ROOT"/*.result "$ROOT"/*/*.result; do
        [ -f "$f" ] || continue
        st="$(awk '{print $1}' "$f" 2>/dev/null)" || continue
        case "$st" in
            red|timeout)
                suite="$(basename "$f" .result)"
                case " ${reds} " in *" ${suite} "*) ;; *) reds="${reds} ${suite}" ;; esac
                ;;
        esac
    done
}

reds=""
_collect_reds
reds="${reds# }"

if [ -z "$reds" ]; then
    printf 'gate-diag: all suites passed or skipped\n'
    exit 0
fi

_result_file() {  # _result_file <suite>
    local f
    for f in "$ROOT/$1.result" "$ROOT"/*/"$1.result"; do
        [ -f "$f" ] && { printf '%s' "$f"; return 0; }
    done
    return 1
}

_out_file() {  # _out_file <suite>
    local f
    for f in "$ROOT/$1.out" "$ROOT"/*/"$1.out"; do
        [ -f "$f" ] && { printf '%s' "$f"; return 0; }
    done
    return 1
}

_retry_status() {  # _retry_status <suite> -> ok | red | timeout | none
    [ -d "$RETRY" ] || { printf 'none'; return 0; }
    local f
    for f in "$RETRY/$1.result" "$RETRY"/*/"$1.result"; do
        [ -f "$f" ] || continue
        awk '{print $1}' "$f" 2>/dev/null
        return 0
    done
    printf 'none'
}

_summary_tmp="$(mktemp)"
trap 'rm -f "$_summary_tmp"' EXIT

_json_red="" _json_flaky=""
for suite in $reds; do
    rf="$(_result_file "$suite" 2>/dev/null || true)"
    of="$(_out_file   "$suite" 2>/dev/null || true)"

    secs="?"
    rc_field="-"
    if [ -n "$rf" ]; then
        secs="$(awk '{print $3}' "$rf" 2>/dev/null)"; case "$secs" in ''|*[!0-9]*) secs="?" ;; esac
        rc_field="$(awk '{print $7}' "$rf" 2>/dev/null)"; [ -n "$rc_field" ] || rc_field="-"
    fi

    raw_out=""
    [ -n "$of" ] && [ -s "$of" ] && raw_out="$(cat "$of")"

    # FAIL lines: first try exact prefix match, then substring.
    fail_lines="$(printf '%s\n' "$raw_out" | grep -E '^  FAIL  |^FAIL[: ]|^not ok ' 2>/dev/null || true)"
    [ -n "$fail_lines" ] || \
        fail_lines="$(printf '%s\n' "$raw_out" | grep -E 'FAIL|not ok' 2>/dev/null || true)"
    first_fail=""
    [ -n "$fail_lines" ] && first_fail="$(printf '%s\n' "$fail_lines" | head -1 | sed 's/^[[:space:]]*//')"
    if [ -z "$raw_out" ]; then
        first_fail="(no output — rc=${rc_field})"
    elif [ -z "$first_fail" ]; then
        first_fail="(no FAIL line — see log)"
    fi

    retry_st="$(_retry_status "$suite")"
    case "$retry_st" in
        ok)          verdict="red-green (flake)" ;;
        red|timeout) verdict="red-red" ;;
        *)           verdict="red" ;;
    esac

    case "$retry_st" in
        ok) _json_flaky="${_json_flaky:+$_json_flaky }$suite" ;;
        *)  _json_red="${_json_red:+$_json_red }$suite" ;;
    esac

    if [ -n "$IN_GHA" ]; then
        printf '::group::%s  %s  rc=%s  %ss\n' "$suite" "$verdict" "$rc_field" "$secs"
        if [ -z "$raw_out" ]; then
            printf '(no output)\n'
        else
            [ -n "$fail_lines" ] && { printf -- '--- FAIL lines ---\n'; printf '%s\n' "$fail_lines"; }
            printf -- '--- last %s lines ---\n' "$TAIL"
            printf '%s\n' "$raw_out" | tail -n "$TAIL"
        fi
        printf '::endgroup::\n'
        _ann="$(printf '%s' "$first_fail" | tr -d '\n' | cut -c1-200)"
        printf '::error file=spira/%s::%s\n' "$suite" "$_ann"
    else
        printf '\n=== %s  %s  rc=%s  %ss ===\n' "$suite" "$verdict" "$rc_field" "$secs"
        if [ -z "$raw_out" ]; then
            printf '(no output)\n'
        else
            [ -n "$fail_lines" ] && printf '%s\n' "$fail_lines"
            printf -- '--- last %s lines ---\n' "$TAIL"
            printf '%s\n' "$raw_out" | tail -n "$TAIL"
        fi
    fi

    _fs="$(printf '%s' "$first_fail" | tr '|' '!' | tr -d '\n' | cut -c1-80)"
    printf '| %s | %s | %s | %s |\n' "$suite" "${secs}s (rc=${rc_field})" "$verdict" "$_fs" \
        >> "$_summary_tmp"
done

# Write machine-readable red-suite list. forge.sh reads this from the artifact
# instead of per-suite annotations, which GitHub caps at 10 per step.
python3 -c "
import json, sys
reds = [s for s in sys.argv[1].split() if s]
flaky = [s for s in sys.argv[2].split() if s]
with open(sys.argv[3], 'w') as f:
    json.dump({'red': reds, 'flaky': flaky, 'red_count': len(reds)}, f)
" "${_json_red:-}" "${_json_flaky:-}" "$ROOT/red-suites.json" 2>/dev/null || true

_hdr='| Suite | Duration | Verdict | First FAIL line |
|-------|----------|---------|-----------------|'
printf '\n%s\n' "$_hdr"
cat "$_summary_tmp"

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    {
        printf '## Red suite summary\n\n%s\n' "$_hdr"
        cat "$_summary_tmp"
    } >> "$GITHUB_STEP_SUMMARY"
fi
