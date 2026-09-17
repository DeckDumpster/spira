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

# A gh stand-in that answers `pr view --json statusCheckRollup` with the rollup in $ROLLUP.
cat > "$TMP/gh" <<'GH'
#!/usr/bin/env bash
case "$*" in
    *statusCheckRollup*) cat "$ROLLUP" ;;
    *) printf '{}\n' ;;
esac
GH
chmod +x "$TMP/gh"

rollup() {   # rollup <gate status> <gate conclusion>
    printf '{"statusCheckRollup":[{"__typename":"CheckRun","name":"suites","status":"COMPLETED","conclusion":"SUCCESS","detailsUrl":"https://example.invalid/actions/runs/1/job/2"},{"__typename":"CheckRun","name":"gate","status":"%s","conclusion":"%s","detailsUrl":"https://example.invalid/actions/runs/1/job/3"}]}\n' "$1" "$2" > "$TMP/rollup.json"
}
status() {
    env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$TMP" SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" SPIRA_GH="$TMP/gh" ROLLUP="$TMP/rollup.json" \
        bash "$HERE/forge.sh" check-status "$TMP/repo" 7 2>/dev/null | head -1
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
echo "test-forge-check-status.sh: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
