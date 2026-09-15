#!/usr/bin/env bash
#
# test-testenv-batch-fence.sh — testenv-batch-fence.sh refuses direct spira/test-*.sh
#   runs in aeon sessions and honours its named override.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): the guard MUST fire
# on the exact commands that have been measured in live aeon sessions before any
# negative control runs. Silence from the negative controls is then evidence the guard
# works, not that it never could.
#
# law-a-regression-test-must-be-seen-to-fail: run this suite against the pre-fix tree
# (without testenv-batch-fence.sh installed) and confirm that the POSITIVE CONTROL
# cases below report FAIL before the guard is in place.
#
# covers: spira/testenv-batch-fence.sh spira/testenv-batch.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1"; esac; }

echo "test-testenv-batch-fence.sh"

GUARD="$HERE/testenv-batch-fence.sh"
[ -f "$GUARD" ] || { printf 'SKIP testenv-batch-fence.sh not found at %s\n' "$GUARD"; exit 77; }

# run_guard <command> [KEY=VAL ...] -> combined stdout+stderr; returns guard exit code.
# Builds the PreToolUse JSON, pipes it to the guard with SPIRA_AEON=1 in the
# environment (simulating an aeon session), plus any extra KEY=VAL overrides.
run_guard() {
    local cmd="$1"; shift
    local rc out
    out=$(python3 -c '
import json, sys
print(json.dumps({"tool_name": "Bash", "tool_input": {"command": sys.argv[1]}}))
' "$cmd" | env -i PATH="$PATH" HOME="$HOME" SPIRA_AEON=1 "$@" bash "$GUARD" 2>&1); rc=$?
    printf '%s' "$out"
    return "$rc"
}

# run_guard_no_aeon: same but WITHOUT SPIRA_AEON, simulating a brain / maintenance session.
run_guard_no_aeon() {
    local cmd="$1"; shift
    local rc out
    out=$(python3 -c '
import json, sys
print(json.dumps({"tool_name": "Bash", "tool_input": {"command": sys.argv[1]}}))
' "$cmd" | env -i PATH="$PATH" HOME="$HOME" "$@" bash "$GUARD" 2>&1); rc=$?
    printf '%s' "$out"
    return "$rc"
}

# ==========================================================================
echo
echo "POSITIVE CONTROL — exact forms measured in live aeon session logs"
# ==========================================================================

out=$(run_guard 'bash spira/test-check4-events.sh | tail -3' || true)
want "bare run piped to tail refused"       "BLOCKED by testenv-batch-fence" "$out"
want "names testenv-batch.sh as correction" "testenv-batch.sh"               "$out"
want "names the override"                   "TESTENV_BATCH_FENCE_OVERRIDE"   "$out"

out=$(run_guard 'bash spira/test-check4-events.sh 2>&1 | tail -3' || true)
want "run with stderr-redirect piped refused" "BLOCKED by testenv-batch-fence" "$out"

out=$(run_guard 'cd /some/checkout && bash spira/test-check4-events.sh' || true)
want "cd && bare run refused"               "BLOCKED by testenv-batch-fence" "$out"

out=$(run_guard 'time bash spira/test-check4-events.sh' || true)
want "time bash run refused"                "BLOCKED by testenv-batch-fence" "$out"

# ==========================================================================
echo
echo "POSITIVE CONTROL — generic spira/test-*.sh path forms"
# ==========================================================================

out=$(run_guard 'bash spira/test-sentinel.sh' || true)
want "bash spira/test-sentinel.sh refused"  "BLOCKED by testenv-batch-fence" "$out"

out=$(run_guard 'bash spira/test-aeon-exit.sh' || true)
want "bash spira/test-aeon-exit.sh refused" "BLOCKED by testenv-batch-fence" "$out"

# ==========================================================================
echo
echo "testenv-batch.sh invocations are allowed (positive control on the allow path)"
# ==========================================================================

out=$(run_guard 'bash spira/testenv-batch.sh --suites test-check4-events.sh spira/sp-23ev' 2>&1) || true
nowant "testenv-batch --suites allowed"     "BLOCKED"                        "$out"

out=$(run_guard 'bash spira/testenv-batch.sh spira/sp-23ev' 2>&1) || true
nowant "testenv-batch branch-only allowed"  "BLOCKED"                        "$out"

# ==========================================================================
echo
echo "reading a suite is allowed"
# ==========================================================================

out=$(run_guard 'grep -n testdb spira/test-check4-events.sh' 2>&1) || true
nowant "grep on test file allowed"          "BLOCKED"                        "$out"

out=$(run_guard 'head -20 spira/test-check4-events.sh' 2>&1) || true
nowant "head on test file allowed"          "BLOCKED"                        "$out"

out=$(run_guard 'cat spira/test-check4-events.sh | head -20' 2>&1) || true
nowant "cat | head on test file allowed"    "BLOCKED"                        "$out"

out=$(run_guard 'bash -n spira/test-check4-events.sh' 2>&1) || true
nowant "bash -n (syntax check) allowed"     "BLOCKED"                        "$out"

# ==========================================================================
echo
echo "non-aeon sessions are not blocked"
# ==========================================================================

out=$(run_guard_no_aeon 'bash spira/test-check4-events.sh' 2>&1) || true
nowant "no SPIRA_AEON: direct run not blocked" "BLOCKED"                     "$out"

# ==========================================================================
echo
echo "override is honoured"
# ==========================================================================

out=$(run_guard 'bash spira/test-check4-events.sh' TESTENV_BATCH_FENCE_OVERRIDE=1 2>&1) || true
nowant "env override allows direct run"     "BLOCKED"                        "$out"

out=$(run_guard 'TESTENV_BATCH_FENCE_OVERRIDE=1 bash spira/test-check4-events.sh' 2>&1) || true
nowant "inline override allows direct run"  "BLOCKED"                        "$out"

# ==========================================================================
echo
echo "prose and single-quoted spans are not refused"
# ==========================================================================

out=$(run_guard "echo 'never use bash spira/test-x.sh directly'" 2>&1) || true
nowant "single-quoted prose not refused"    "BLOCKED"                        "$out"

out=$(run_guard 'cat > note.md <<'"'"'EOF'"'"'
Do NOT run bash spira/test-check4-events.sh on the host
EOF' 2>&1) || true
nowant "heredoc prose not refused"          "BLOCKED"                        "$out"

# ==========================================================================
echo
echo "non-Bash tools pass through"
# ==========================================================================

out=$(python3 -c '
import json, sys
print(json.dumps({"tool_name": "Read", "tool_input": {"file_path": "spira/test-check4-events.sh"}}))
' | env -i PATH="$PATH" HOME="$HOME" SPIRA_AEON=1 bash "$GUARD" 2>&1) || true
nowant "non-Bash tool not blocked"          "BLOCKED"                        "$out"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
