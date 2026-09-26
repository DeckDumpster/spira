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
# covers: spira/gate-diag.sh .github/workflows/gate.yml spira/testenv-batch.sh spira/tap-jsonl.sh
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/tap-jsonl.sh"

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

# --------------------------------------------------------------------------------------
# results.jsonl and junit.xml — for EVERY suite, not only the red ones. This runs
# unconditionally, before the reds-only early exit below, because a reader of the
# machine-readable results (forge.sh's attribution, a human comparing runs) needs the
# green and skipped rows as much as the red ones; red-suites.json further down stays
# red-only because that is the one thing it was built to answer cheaply within GitHub's
# 10-annotation cap.
# --------------------------------------------------------------------------------------
_write_results_jsonl() {
    local f suite status secs out src jsonl_path="$ROOT/results.jsonl"
    : > "$jsonl_path" || return 0
    for f in "$ROOT"/*.result "$ROOT"/*/*.result; do
        [ -f "$f" ] || continue
        suite="$(basename "$f" .result)"
        status="$(awk '{print $1}' "$f" 2>/dev/null)"; [ -n "$status" ] || status="red"
        secs="$(awk '{print $3}' "$f" 2>/dev/null)"; case "$secs" in ''|*[!0-9]*) secs=0 ;; esac
        out="${f%.result}.out"
        src="$HERE/$suite"
        tap_jsonl_rows "$suite" "$src" "$out" "$status" "$secs" >> "$jsonl_path"
    done
}
_write_results_jsonl

_write_junit_xml() {
    command -v python3 >/dev/null 2>&1 || return 0
    python3 -c "
import json, sys
from xml.sax.saxutils import escape
from collections import defaultdict

suites = defaultdict(list)
try:
    with open(sys.argv[1]) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            row = json.loads(line)
            suites[row['suite']].append(row)
except Exception:
    sys.exit(0)

out = ['<?xml version=\"1.0\" encoding=\"UTF-8\"?>', '<testsuites>']
for suite, rows in sorted(suites.items()):
    failures = sum(1 for r in rows if r.get('status') == 'fail')
    skipped = sum(1 for r in rows if r.get('status') in ('skip', 'unreached', 'bail'))
    seconds = rows[0].get('seconds', 0) if rows else 0
    out.append('  <testsuite name=\"%s\" tests=\"%d\" failures=\"%d\" skipped=\"%d\" time=\"%s\">' %
                (escape(suite), len(rows), failures, skipped, seconds))
    for r in rows:
        case = r.get('case') or '(suite)'
        out.append('    <testcase name=\"%s\" classname=\"%s\" time=\"%s\">' %
                    (escape(case), escape(suite), r.get('seconds', 0)))
        if r.get('status') == 'fail':
            out.append('      <failure message=\"%s\"></failure>' % escape(r.get('detail') or ''))
        elif r.get('status') in ('skip', 'unreached', 'bail'):
            out.append('      <skipped message=\"%s\"></skipped>' % escape(r.get('detail') or ''))
        out.append('    </testcase>')
    out.append('  </testsuite>')
out.append('</testsuites>')
with open(sys.argv[2], 'w') as f:
    f.write('\n'.join(out) + '\n')
" "$ROOT/results.jsonl" "$ROOT/junit.xml" 2>/dev/null || true
}
_write_junit_xml

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

    # A suite that dies before its own summary line leaves no FAIL line to show —
    # "(no FAIL line — see log)" is a row that will not be read. Report the same
    # facts a human would open the log for instead: the last thing the suite
    # printed, the rc, and the wall time against its own declared budget.
    _decl_to="$(sed -n 's/^# *timeout: *//p' "$HERE/$suite" 2>/dev/null | head -1 | tr -d '[:space:]')"
    case "$_decl_to" in [0-9]*) ;; *) _decl_to="${SPIRA_SUITE_TIMEOUT:-600}" ;; esac
    if [ -z "$raw_out" ]; then
        first_fail="(died rc=${rc_field} at ${secs}s/${_decl_to}s — no output)"
    elif [ -z "$first_fail" ]; then
        _last_line="$(printf '%s\n' "$raw_out" | sed '/^[[:space:]]*$/d' | tail -1)"
        first_fail="(died rc=${rc_field} at ${secs}s/${_decl_to}s — last: ${_last_line})"
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

# Append the same retry-adjusted verdict to results.jsonl, one "(verdict)" row per
# suite: forge.sh's artifact reader prefers this over red-suites.json because it is
# the one file every consumer of a batch's results already reads.
python3 -c "
import json, sys
def row(s, status):
    return json.dumps({'suite': s, 'tier': '', 'case': '(verdict)', 'status': status,
                        'seconds': 0, 'uc': [], 'detail': ''}, separators=(',', ':'))
reds = [s for s in sys.argv[1].split() if s]
flaky = [s for s in sys.argv[2].split() if s]
with open(sys.argv[3], 'a') as f:
    for s in reds:
        f.write(row(s, 'red-red') + '\n')
    for s in flaky:
        f.write(row(s, 'red-green') + '\n')
" "${_json_red:-}" "${_json_flaky:-}" "$ROOT/results.jsonl" 2>/dev/null || true

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
