#!/usr/bin/env bash
# test-forge-check-status.sh — forge.sh check-status reads a real gh rollup.
#
# The verdict suite drives a fixture forge, so the real forge's parsing of gh's JSON was never
# run: a parameter default of `{}` inside `${...}` appended a stray brace to every non-empty
# rollup, the parse failed, and the fallback printed `pending` for a green batch forever.
#
# covers: spira/forge.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-forge-check-status.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
git init -q "$TMP/repo"

# A gh stand-in that answers `pr view --json statusCheckRollup` with the rollup in $ROLLUP,
# returns jobs from $JOBS_JSON for runs-jobs queries (default: one job id=99, no name),
# and serves $ANNOTATIONS for annotations.
cat > "$TMP/gh" <<'GH'
#!/usr/bin/env bash
case "$*" in
    *statusCheckRollup*)      cat "$ROLLUP" ;;
    *actions/runs*jobs*)      [ -n "${JOBS_JSON:-}" ] && cat "$JOBS_JSON" || printf '{"jobs":[{"id":99}]}\n' ;;
    *check-runs*annotations*) [ -n "${ANNOTATIONS:-}" ] && cat "$ANNOTATIONS" || printf '[]\n' ;;
    *)                        printf '{}\n' ;;
esac
GH
chmod +x "$TMP/gh"
ANNOTATIONS=""
JOBS_JSON=""

rollup() {   # rollup <gate status> <gate conclusion>
    printf '{"headRefOid":"abc123def456abc123def456abc123def456abc123","statusCheckRollup":[{"__typename":"CheckRun","name":"suites","status":"COMPLETED","conclusion":"SUCCESS","detailsUrl":"https://example.invalid/actions/runs/1/job/2"},{"__typename":"CheckRun","name":"gate","status":"%s","conclusion":"%s","detailsUrl":"https://example.invalid/actions/runs/1/job/3"}]}\n' "$1" "$2" > "$TMP/rollup.json"
}
status() {
    env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$TMP" SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" SPIRA_GH="$TMP/gh" ROLLUP="$TMP/rollup.json" \
        ANNOTATIONS="${ANNOTATIONS:-}" JOBS_JSON="${JOBS_JSON:-}" \
        bash "$HERE/forge.sh" check-status "$TMP/repo" 7 2>/dev/null | head -1
}
status_all() {
    env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$TMP" SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" SPIRA_GH="$TMP/gh" ROLLUP="$TMP/rollup.json" \
        ANNOTATIONS="${ANNOTATIONS:-}" JOBS_JSON="${JOBS_JSON:-}" \
        bash "$HERE/forge.sh" check-status "$TMP/repo" 7 2>/dev/null
}

echo
echo "positive control — a gate still running is pending:"
rollup IN_PROGRESS ""
is "in-progress gate reads pending" "pending" "$(status)"

echo
echo "a completed gate is read from the real rollup shape:"
rollup COMPLETED SUCCESS
is "successful gate reads green" "green" "$(status)"
rollup COMPLETED FAILURE
is "failed gate reads red" "red" "$(status)"
rollup COMPLETED SKIPPED
is "skipped gate reads harness_fault" "harness_fault" "$(status)"

echo
echo "gate-diag.sh format: annotation_level=failure, spira/test-*.sh path → red-suite:"
rollup COMPLETED FAILURE
printf '[{"annotation_level":"failure","title":"","path":"spira/test-auron.sh","message":"FAIL x"}]\n' \
    > "$TMP/annot.json"
ANNOTATIONS="$TMP/annot.json"
_out="$(status_all)"
is "overall status is still red" "red" "$(printf '%s\n' "$_out" | head -1)"
printf '%s\n' "$_out" | grep -qxF "red-suite: test-auron.sh" \
    && ok "red-suite line extracted from annotation path" \
    || bad "red-suite line extracted from annotation path" "not in output: [$_out]"

echo "positive control — non-test path is not emitted as red-suite:"
printf '[{"annotation_level":"failure","title":"","path":".github/workflows/ci.yml","message":"x"}]\n' \
    > "$TMP/annot2.json"
ANNOTATIONS="$TMP/annot2.json"
_out2="$(status_all)"
printf '%s\n' "$_out2" | grep -qF "red-suite:" \
    && bad "non-test path not emitted as red-suite" "found red-suite in: [$_out2]" \
    || ok "non-test path not emitted as red-suite"
ANNOTATIONS=""

echo
echo "headRefOid is reported as head-sha line:"
rollup COMPLETED SUCCESS
_out3="$(status_all)"
printf '%s\n' "$_out3" | grep -qF "head-sha: abc123def456abc123def456abc123def456abc123" \
    && ok "head-sha line present when headRefOid in rollup" \
    || bad "head-sha line present when headRefOid in rollup" "not in output: [$_out3]"

echo "positive control — no head-sha line when headRefOid absent:"
printf '{"statusCheckRollup":[{"__typename":"CheckRun","name":"gate","status":"IN_PROGRESS","conclusion":"","detailsUrl":"https://example.invalid/actions/runs/1/job/3"}]}\n' \
    > "$TMP/rollup.json"
_out4="$(status_all)"
printf '%s\n' "$_out4" | grep -qF "head-sha:" \
    && bad "no head-sha when headRefOid absent" "found head-sha in: [$_out4]" \
    || ok "no head-sha when headRefOid absent"

echo
echo "provision_fault: red run with failed provision job → provision_fault:"
rollup COMPLETED FAILURE
printf '{"jobs":[{"id":99,"name":"provision","conclusion":"failure"},{"id":100,"name":"gate","conclusion":"failure"}]}\n' \
    > "$TMP/jobs-prov-fail.json"
JOBS_JSON="$TMP/jobs-prov-fail.json"
is "provision failure → provision_fault" "provision_fault" "$(status)"

echo "positive control — provision job success with other failure → still red:"
printf '{"jobs":[{"id":99,"name":"provision","conclusion":"success"},{"id":100,"name":"gate","conclusion":"failure"}]}\n' \
    > "$TMP/jobs-prov-ok.json"
JOBS_JSON="$TMP/jobs-prov-ok.json"
is "provision success → still red" "red" "$(status)"
JOBS_JSON=""

echo "positive control — provision_fault only when status is red (green ignores jobs):"
rollup COMPLETED SUCCESS
JOBS_JSON="$TMP/jobs-prov-fail.json"
is "green with failed provision job → still green" "green" "$(status)"
JOBS_JSON=""

echo
echo "workflow-rerun: no --failed in rerun argv; cancels in_progress runs first:"
RERUN_LOG="$TMP/rerun-log"
: > "$RERUN_LOG"
# gh stand-in: records all calls; answers run-view with $RUN_STATUS.
cat > "$TMP/gh-rr" <<'GHRR'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$RERUN_LOG"
case "$*" in *run\ view*) printf '%s\n' "${RUN_STATUS:-completed}" ;; esac
GHRR
chmod +x "$TMP/gh-rr"

# Positive control: verify the detector can find --failed when planted.
printf 'run rerun 99 --failed\n' > "$RERUN_LOG"
grep -q -- '--failed' "$RERUN_LOG" \
    && ok  "rerun: positive control detects --failed" \
    || bad "rerun: positive control detects --failed" "planted --failed not found"

# Test: completed run → rerun with no --failed, no cancel.
: > "$RERUN_LOG"
env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$TMP" SPIRA_CONF=/nonexistent \
    SPIRA_RUN="$TMP/run" SPIRA_GH="$TMP/gh-rr" RERUN_LOG="$RERUN_LOG" RUN_STATUS=completed \
    bash "$HERE/forge.sh" workflow-rerun "$TMP/repo" 99 2>/dev/null
grep -q -- '--failed' "$RERUN_LOG" \
    && bad "rerun: no --failed when run completed" "found --failed in: $(cat "$RERUN_LOG")" \
    || ok  "rerun: no --failed when run completed"
grep -q 'run cancel' "$RERUN_LOG" \
    && bad "rerun: no cancel when run is completed" "found cancel in: $(cat "$RERUN_LOG")" \
    || ok  "rerun: no cancel when run is completed"

# Test: in_progress run → cancel called before rerun, still no --failed.
: > "$RERUN_LOG"
env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$TMP" SPIRA_CONF=/nonexistent \
    SPIRA_RUN="$TMP/run" SPIRA_GH="$TMP/gh-rr" RERUN_LOG="$RERUN_LOG" RUN_STATUS=in_progress \
    bash "$HERE/forge.sh" workflow-rerun "$TMP/repo" 99 2>/dev/null
grep -q 'run cancel' "$RERUN_LOG" \
    && ok  "rerun: cancel called when in_progress" \
    || bad "rerun: cancel called when in_progress" "no cancel in: $(cat "$RERUN_LOG")"
grep -q -- '--failed' "$RERUN_LOG" \
    && bad "rerun: no --failed when in_progress" "found --failed in: $(cat "$RERUN_LOG")" \
    || ok  "rerun: no --failed when in_progress"

echo
echo "test-forge-check-status.sh: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
