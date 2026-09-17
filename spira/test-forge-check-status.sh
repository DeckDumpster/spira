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
# returns a single job (id=99) for runs-jobs queries, and serves $ANNOTATIONS for annotations.
cat > "$TMP/gh" <<'GH'
#!/usr/bin/env bash
case "$*" in
    *statusCheckRollup*)      cat "$ROLLUP" ;;
    *actions/runs*jobs*)      printf '{"jobs":[{"id":99}]}\n' ;;
    *check-runs*annotations*) [ -n "${ANNOTATIONS:-}" ] && cat "$ANNOTATIONS" || printf '[]\n' ;;
    *)                        printf '{}\n' ;;
esac
GH
chmod +x "$TMP/gh"
ANNOTATIONS=""

rollup() {   # rollup <gate status> <gate conclusion>
    printf '{"statusCheckRollup":[{"__typename":"CheckRun","name":"suites","status":"COMPLETED","conclusion":"SUCCESS","detailsUrl":"https://example.invalid/actions/runs/1/job/2"},{"__typename":"CheckRun","name":"gate","status":"%s","conclusion":"%s","detailsUrl":"https://example.invalid/actions/runs/1/job/3"}]}\n' "$1" "$2" > "$TMP/rollup.json"
}
status() {
    env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$TMP" SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" SPIRA_GH="$TMP/gh" ROLLUP="$TMP/rollup.json" \
        ANNOTATIONS="${ANNOTATIONS:-}" \
        bash "$HERE/forge.sh" check-status "$TMP/repo" 7 2>/dev/null | head -1
}
status_all() {
    env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$TMP" SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" SPIRA_GH="$TMP/gh" ROLLUP="$TMP/rollup.json" \
        ANNOTATIONS="${ANNOTATIONS:-}" \
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
echo "test-forge-check-status.sh: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
