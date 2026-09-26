#!/usr/bin/env bash
# test-forge-check-status.sh — forge.sh check-status reads the run scoped to this
# PR's own head branch, never the PR-view rollup keyed to the head commit.
#
# The rollup `gh pr view --json statusCheckRollup` is keyed to the head COMMIT: when a batch
# is re-opened on an unchanged tree, the new PR shares its predecessor's commit and the old
# rollup approach saw the predecessor's already-failed check run from the moment the new PR
# opened, closing a batch that never ran (sp-uwwt3). check-status instead looks up the
# workflow run for the PR's own branch — unique per PR even when the commit is shared.
#
# tier: T1
# covers: spira/forge.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-forge-check-status.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
git init -q "$TMP/repo"

# A gh stand-in: `run list --branch X` answers from the run-list fixture registered for
# branch X (BRANCH_A/RUNLIST_A, BRANCH_B/RUNLIST_B) — an empty array for any other branch,
# same as GitHub would return for a branch with no runs. Jobs/artifacts/annotations queries
# are unscoped, as in the real API (they key off a run id, not a branch).
cat > "$TMP/gh" <<'GH'
#!/usr/bin/env bash
if [ "${1:-}" = "run" ] && [ "${2:-}" = "list" ]; then
    branch="" prev=""
    for a in "$@"; do
        [ "$prev" = "--branch" ] && branch="$a"
        prev="$a"
    done
    if [ -n "${BRANCH_A:-}" ] && [ "$branch" = "$BRANCH_A" ]; then
        cat "${RUNLIST_A:-/dev/null}" 2>/dev/null || printf '[]\n'
    elif [ -n "${BRANCH_B:-}" ] && [ "$branch" = "$BRANCH_B" ]; then
        cat "${RUNLIST_B:-/dev/null}" 2>/dev/null || printf '[]\n'
    else
        printf '[]\n'
    fi
    exit 0
fi
case "$*" in
    *actions/runs*artifacts*)  [ -n "${ARTIFACTS_JSON:-}" ] && cat "$ARTIFACTS_JSON" || printf '{"artifacts":[]}\n' ;;
    *actions/artifacts*zip*)   [ -n "${ARTIFACT_ZIP:-}" ] && cat "$ARTIFACT_ZIP" || true ;;
    *actions/runs*jobs*)       [ -n "${JOBS_JSON:-}" ] && cat "$JOBS_JSON" || printf '{"jobs":[{"id":99}]}\n' ;;
    *check-runs*annotations*)  [ -n "${ANNOTATIONS:-}" ] && cat "$ANNOTATIONS" || printf '[]\n' ;;
    *)                         printf '{}\n' ;;
esac
GH
chmod +x "$TMP/gh"
ANNOTATIONS=""
JOBS_JSON=""
ARTIFACTS_JSON=""
ARTIFACT_ZIP=""

BRANCH="spira/queue/369"
SHA="abc123def456abc123def456abc123def456abc123"

runlist() {   # runlist <run-status> <conclusion> [sha] [url]
    printf '[{"databaseId":1,"status":"%s","conclusion":"%s","headSha":"%s","url":"%s"}]\n' \
        "$1" "$2" "${3:-$SHA}" "${4:-https://example.invalid/actions/runs/1}" > "$TMP/runlist.json"
}
runlist_none() { printf '[]\n' > "$TMP/runlist.json"; }

status() {
    env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$TMP" SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" SPIRA_GH="$TMP/gh" BRANCH_A="$BRANCH" RUNLIST_A="$TMP/runlist.json" \
        ANNOTATIONS="${ANNOTATIONS:-}" JOBS_JSON="${JOBS_JSON:-}" \
        ARTIFACTS_JSON="${ARTIFACTS_JSON:-}" ARTIFACT_ZIP="${ARTIFACT_ZIP:-}" \
        bash "$HERE/forge.sh" check-status "$TMP/repo" 7 "$BRANCH" 2>/dev/null | head -1
}
status_all() {
    env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$TMP" SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" SPIRA_GH="$TMP/gh" BRANCH_A="$BRANCH" RUNLIST_A="$TMP/runlist.json" \
        ANNOTATIONS="${ANNOTATIONS:-}" JOBS_JSON="${JOBS_JSON:-}" \
        ARTIFACTS_JSON="${ARTIFACTS_JSON:-}" ARTIFACT_ZIP="${ARTIFACT_ZIP:-}" \
        bash "$HERE/forge.sh" check-status "$TMP/repo" 7 "$BRANCH" 2>/dev/null
}

echo
echo "positive control — a gate still running is pending:"
runlist in_progress ""
is "in-progress run reads pending" "pending" "$(status)"

echo
echo "a completed run is read from the run-list shape:"
runlist completed success
is "successful run reads green" "green" "$(status)"
runlist completed failure
is "failed run reads red" "red" "$(status)"
runlist completed skipped
is "skipped run reads harness_fault" "harness_fault" "$(status)"

echo
echo "positive control — no run at all for this branch reads pending, not red:"
runlist_none
is "no run for branch reads pending" "pending" "$(status)"

echo
echo "THE BUG THIS SUITE PINS (sp-uwwt3): a re-issued PR must never inherit a prior"
echo "PR's red just because they share a head commit. Two branches, same head SHA:"
echo "the first is red and closed; the second has its own, still-running run."
BRANCH_A_SAVE="spira/queue/369"
BRANCH_B_SAVE="spira/queue/370"
printf '[{"databaseId":1,"status":"completed","conclusion":"failure","headSha":"%s","url":"https://example.invalid/actions/runs/1"}]\n' \
    "$SHA" > "$TMP/runlist-a.json"
printf '[{"databaseId":2,"status":"in_progress","conclusion":"","headSha":"%s","url":"https://example.invalid/actions/runs/2"}]\n' \
    "$SHA" > "$TMP/runlist-b.json"
_status_for_branch() {
    env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$TMP" SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" SPIRA_GH="$TMP/gh" \
        BRANCH_A="$BRANCH_A_SAVE" RUNLIST_A="$TMP/runlist-a.json" \
        BRANCH_B="$BRANCH_B_SAVE" RUNLIST_B="$TMP/runlist-b.json" \
        bash "$HERE/forge.sh" check-status "$TMP/repo" "$1" "$2" 2>/dev/null | head -1
}
is "PR 369 (its own branch) reads red" "red" "$(_status_for_branch 369 "$BRANCH_A_SAVE")"
is "PR 370 (same commit, its own still-running branch) is NOT red" "pending" \
    "$(_status_for_branch 370 "$BRANCH_B_SAVE")"

echo
echo "positive control — an empty branch argument cannot be attributed: pending, not red"
echo "(this is the fail-closed direction: a caller that forgot to pass the branch must"
echo "never read a red it cannot prove belongs to this PR):"
runlist completed failure
is "no branch given reads pending" "pending" \
    "$(env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$TMP" SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" SPIRA_GH="$TMP/gh" BRANCH_A="$BRANCH" RUNLIST_A="$TMP/runlist.json" \
        bash "$HERE/forge.sh" check-status "$TMP/repo" 7 "" 2>/dev/null | head -1)"

echo
echo "gate-diag.sh format: annotation_level=failure, spira/test-*.sh path → red-suite:"
runlist completed failure
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
echo "head-sha is reported from the run's own headSha:"
runlist completed success
_out3="$(status_all)"
printf '%s\n' "$_out3" | grep -qF "head-sha: $SHA" \
    && ok "head-sha line present when the run reports one" \
    || bad "head-sha line present when the run reports one" "not in output: [$_out3]"

echo "positive control — no head-sha line when no run exists:"
runlist_none
_out4="$(status_all)"
printf '%s\n' "$_out4" | grep -qF "head-sha:" \
    && bad "no head-sha when no run" "found head-sha in: [$_out4]" \
    || ok "no head-sha when no run"

echo
echo "the run's own URL is reported as run-url (verdict.sh's ejection mail links it):"
runlist completed success
_out3b="$(status_all)"
printf '%s\n' "$_out3b" | grep -qF "run-url: https://example.invalid/actions/runs/1" \
    && ok "run-url line present for a completed run" \
    || bad "run-url line present for a completed run" "not in output: [$_out3b]"

echo "positive control — no run-url line while the run is still pending:"
runlist in_progress ""
_out3c="$(status_all)"
printf '%s\n' "$_out3c" | grep -qF "run-url:" \
    && bad "no run-url while pending" "found run-url in: [$_out3c]" \
    || ok "no run-url while pending"

echo
echo "provision_fault: red run with failed provision job → provision_fault:"
runlist completed failure
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
runlist completed success
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
echo "artifact-based suite list: 28 suites in artifact, only 10 in annotations:"
runlist completed failure
python3 -c "
import zipfile, json, io, sys
buf = io.BytesIO()
suites = ['test-suite-%02d.sh' % i for i in range(1, 29)]
with zipfile.ZipFile(buf, 'w') as z:
    z.writestr('red-suites.json', json.dumps({'red': suites, 'flaky': [], 'red_count': 28}))
buf.seek(0)
sys.stdout.buffer.write(buf.read())
" > "$TMP/artifact-28.zip"
_ann10='[{"annotation_level":"failure","title":"","path":"spira/test-aaa-01.sh","message":"x"},{"annotation_level":"failure","title":"","path":"spira/test-aaa-02.sh","message":"x"},{"annotation_level":"failure","title":"","path":"spira/test-aaa-03.sh","message":"x"},{"annotation_level":"failure","title":"","path":"spira/test-aaa-04.sh","message":"x"},{"annotation_level":"failure","title":"","path":"spira/test-aaa-05.sh","message":"x"},{"annotation_level":"failure","title":"","path":"spira/test-aaa-06.sh","message":"x"},{"annotation_level":"failure","title":"","path":"spira/test-aaa-07.sh","message":"x"},{"annotation_level":"failure","title":"","path":"spira/test-aaa-08.sh","message":"x"},{"annotation_level":"failure","title":"","path":"spira/test-aaa-09.sh","message":"x"},{"annotation_level":"failure","title":"","path":"spira/test-aaa-10.sh","message":"x"}]'
printf '%s\n' "$_ann10" > "$TMP/ann-10.json"
printf '{"artifacts":[{"id":7,"name":"batch-results-1"}]}\n' > "$TMP/artifacts-7.json"
ANNOTATIONS="$TMP/ann-10.json"
ARTIFACTS_JSON="$TMP/artifacts-7.json"
ARTIFACT_ZIP="$TMP/artifact-28.zip"
_out28="$(status_all)"
is "artifact path: overall status is red" "red" "$(printf '%s\n' "$_out28" | head -1)"
_cnt28="$(printf '%s\n' "$_out28" | grep -c '^red-suite: ' || true)"
is "artifact path: all 28 red-suite lines emitted" "28" "$_cnt28"
ANNOTATIONS="" ARTIFACTS_JSON="" ARTIFACT_ZIP=""

echo
echo "positive control — no artifact falls back to annotations:"
runlist completed failure
ANNOTATIONS="$TMP/ann-10.json"
_out_fb="$(status_all)"
is "annotation fallback: status still red" "red" "$(printf '%s\n' "$_out_fb" | head -1)"
_cnt_fb="$(printf '%s\n' "$_out_fb" | grep -c '^red-suite: ' || true)"
is "annotation fallback: 10 lines from annotations" "10" "$_cnt_fb"
ANNOTATIONS=""

echo
echo "truncated artifact (red_count > len) → harness_fault:"
runlist completed failure
python3 -c "
import zipfile, json, io, sys
buf = io.BytesIO()
suites = ['test-suite-%02d.sh' % i for i in range(1, 11)]
with zipfile.ZipFile(buf, 'w') as z:
    z.writestr('red-suites.json', json.dumps({'red': suites, 'flaky': [], 'red_count': 28}))
buf.seek(0)
sys.stdout.buffer.write(buf.read())
" > "$TMP/artifact-truncated.zip"
printf '{"artifacts":[{"id":8,"name":"batch-results-1"}]}\n' > "$TMP/artifacts-8.json"
ARTIFACTS_JSON="$TMP/artifacts-8.json"
ARTIFACT_ZIP="$TMP/artifact-truncated.zip"
is "truncated artifact → harness_fault" "harness_fault" "$(status)"
ARTIFACTS_JSON="" ARTIFACT_ZIP=""

echo
echo "suites job cancelled (job timeout): red run, no annotations → harness_fault:"
runlist completed failure
printf '{"jobs":[{"id":99,"name":"provision","conclusion":"success"},{"id":100,"name":"build","conclusion":"success"},{"id":101,"name":"suites","conclusion":"cancelled","steps":[{"name":"Suites","conclusion":"cancelled"}]},{"id":102,"name":"gate","conclusion":"failure"}]}\n' \
    > "$TMP/jobs-suites-cancelled.json"
JOBS_JSON="$TMP/jobs-suites-cancelled.json"
is "suites job cancelled → harness_fault" "harness_fault" "$(status)"
JOBS_JSON=""

echo "suites job timed_out (step-level only) → harness_fault:"
runlist completed failure
printf '{"jobs":[{"id":99,"name":"provision","conclusion":"success"},{"id":100,"name":"build","conclusion":"success"},{"id":101,"name":"suites","conclusion":"failure","steps":[{"name":"Suites","conclusion":"timed_out"}]},{"id":102,"name":"gate","conclusion":"failure"}]}\n' \
    > "$TMP/jobs-suites-step-timeout.json"
JOBS_JSON="$TMP/jobs-suites-step-timeout.json"
is "suites step timed_out → harness_fault" "harness_fault" "$(status)"
JOBS_JSON=""

echo "positive control — suites job success with red suites → still red:"
runlist completed failure
printf '{"jobs":[{"id":99,"name":"provision","conclusion":"success"},{"id":100,"name":"build","conclusion":"success"},{"id":101,"name":"suites","conclusion":"success","steps":[{"name":"Suites","conclusion":"failure"}]},{"id":102,"name":"gate","conclusion":"failure"}]}\n' \
    > "$TMP/jobs-suites-ok.json"
JOBS_JSON="$TMP/jobs-suites-ok.json"
is "suites job success (red suite failure) → still red" "red" "$(status)"
JOBS_JSON=""

echo
tl_summary
