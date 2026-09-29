#!/usr/bin/env bash
# test-forge-dispatch.sh — forge.sh dispatch and fail-lines: the two primitives
# that moved red-batch attribution's per-member reproduction into the batch PR's
# own CI instead of rerunning suites on the concierge box (sp-2hee5).
#
# dispatch triggers a Gate workflow_dispatch run on a throwaway branch, scoped to
# the suite(s) verdict.sh wants re-checked, via the same `suites` input
# spira-lint's gate-workflow rule holds gate.yml to. fail-lines reads an ejected
# member's evidence lines back out of that run's own batch-results artifact,
# rather than capturing them from a local rerun's stdout.
#
# tier: T1
# covers: spira/forge.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-forge-dispatch.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
git init -q "$TMP/repo"

# A gh stand-in: `workflow run` logs its full argument line so the test can
# assert exactly what was dispatched; DISPATCH_RC lets a case simulate gh
# refusing the dispatch. Artifact lookups mirror test-forge-check-status.sh.
cat > "$TMP/gh" <<'GH'
#!/usr/bin/env bash
if [ "${1:-}" = "workflow" ] && [ "${2:-}" = "run" ]; then
    printf '%s\n' "$*" >> "$DISPATCH_LOG"
    exit "${DISPATCH_RC:-0}"
fi
case "$*" in
    *actions/runs*artifacts*)  [ -n "${ARTIFACTS_JSON:-}" ] && cat "$ARTIFACTS_JSON" || printf '{"artifacts":[]}\n' ;;
    *actions/artifacts*zip*)   [ -n "${ARTIFACT_ZIP:-}" ] && cat "$ARTIFACT_ZIP" || true ;;
    *)                         printf '{}\n' ;;
esac
GH
chmod +x "$TMP/gh"

DISPATCH_LOG="$TMP/dispatch-log"
: > "$DISPATCH_LOG"

dispatch() {   # dispatch <ref> <suites-csv> [rc]
    env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$TMP" SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" SPIRA_GH="$TMP/gh" \
        DISPATCH_LOG="$DISPATCH_LOG" DISPATCH_RC="${3:-0}" \
        bash "$HERE/forge.sh" dispatch "$TMP/repo" "$1" "$2" 2>/dev/null
}

echo
echo "dispatch triggers a Gate workflow_dispatch run scoped to the branch and suites:"
: > "$DISPATCH_LOG"
dispatch "spira/attr/61-sp-vd-bl2-abc123456789" "test-real-offender.sh"
is "dispatch exits 0 on success" "0" "$?"
want "dispatch invokes gh workflow run Gate" "workflow run Gate" "$(cat "$DISPATCH_LOG")"
want "dispatch passes --ref with the throwaway branch" \
    "--ref spira/attr/61-sp-vd-bl2-abc123456789" "$(cat "$DISPATCH_LOG")"
want "dispatch passes the suites as the suites= input" \
    "suites=test-real-offender.sh" "$(cat "$DISPATCH_LOG")"

echo
echo "positive control — a refused dispatch is reported, not swallowed:"
: > "$DISPATCH_LOG"
dispatch "spira/attr/x" "test-owned.sh" 1
is "dispatch propagates a failing gh exit code" "1" "$?"

echo
echo "positive control — dispatch refuses with no ref rather than guessing one:"
out_noref="$(env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$TMP" SPIRA_CONF=/nonexistent \
    SPIRA_RUN="$TMP/run" SPIRA_GH="$TMP/gh" DISPATCH_LOG="$DISPATCH_LOG" \
    bash "$HERE/forge.sh" dispatch "$TMP/repo" "" "test-owned.sh" 2>&1)"
is "dispatch with no ref exits 1" "1" "$?"
want "dispatch with no ref names the missing argument" "ref required" "$out_noref"

# ─── fail-lines ────────────────────────────────────────────────────────────────

ARTIFACTS_JSON=""
ARTIFACT_ZIP=""

fail_lines() {   # fail_lines <run-id> <suites-space-sep>
    env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$TMP" SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" SPIRA_GH="$TMP/gh" \
        ARTIFACTS_JSON="${ARTIFACTS_JSON:-}" ARTIFACT_ZIP="${ARTIFACT_ZIP:-}" \
        bash "$HERE/forge.sh" fail-lines "$TMP/repo" "$1" "$2" 2>/dev/null
}

python3 -c "
import zipfile, io, sys
buf = io.BytesIO()
with zipfile.ZipFile(buf, 'w') as z:
    z.writestr('test-frobnicator.sh.out',
        'ok line 1\nFAIL: test_frobnicator_overflow (expected 200 got 500)\nmore output\n')
    z.writestr('test-quiet.sh.out', 'line1\nline2\nline3\n')
buf.seek(0)
sys.stdout.buffer.write(buf.read())
" > "$TMP/artifact-fail.zip"
printf '{"artifacts":[{"id":9,"name":"batch-results-1"}]}\n' > "$TMP/artifacts-9.json"

echo
echo "fail-lines reads a suite's own .out from the run's batch-results artifact:"
ARTIFACTS_JSON="$TMP/artifacts-9.json"
ARTIFACT_ZIP="$TMP/artifact-fail.zip"
_out="$(fail_lines 42 test-frobnicator.sh)"
want "fail-lines extracts the FAIL line" "test_frobnicator_overflow" "$_out"
want "fail-lines prefixes each line with the suite name" \
    "fail-line: test-frobnicator.sh:" "$_out"
nowant "fail-lines does not leak an unrequested suite's lines" "test-quiet" "$_out"

echo
echo "positive control — a suite with no FAIL line falls back to its last lines:"
_out2="$(fail_lines 42 test-quiet.sh)"
want "fail-lines falls back to tail output" "line3" "$_out2"
nowant "fail-lines fallback does not claim a FAIL that never happened" "FAIL" "$_out2"

echo
echo "positive control — no artifact for the run prints nothing:"
ARTIFACTS_JSON="" ARTIFACT_ZIP=""
_out3="$(fail_lines 42 test-frobnicator.sh)"
is "fail-lines with no artifact prints nothing" "" "$_out3"

echo
echo "positive control — no run-id given prints nothing:"
ARTIFACTS_JSON="$TMP/artifacts-9.json"
ARTIFACT_ZIP="$TMP/artifact-fail.zip"
_out4="$(fail_lines "" test-frobnicator.sh)"
is "fail-lines with no run-id prints nothing" "" "$_out4"

echo
tl_summary
