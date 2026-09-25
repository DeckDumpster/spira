#!/usr/bin/env bash
# test-forge-batch-ci-status.sh — batch-ci-status reads the Gate workflow's latest run,
# never an arbitrary workflow's.
#
# sp-zqhhj: main's push gate failed while a later, unrelated "Test image" run on the
# same branch came back green. Without a --workflow filter, `gh run list --branch`
# returns the latest run of ANY workflow, so the green non-gate run masked the red
# gate for an hour. Plants exactly that shape — a green non-gate run newer than a red
# gate run — and requires batch-ci-status to still report the gate run's failure.
#
# covers: spira/forge.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
has() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

echo "test-forge-batch-ci-status.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
git init -q "$TMP/repo"

# A gh stand-in that distinguishes a Gate-filtered run-list query from an unfiltered
# one: the two answer with different fixtures so a caller that forgets the filter is
# caught reading the wrong run.
cat > "$TMP/gh" <<'GH'
#!/usr/bin/env bash
case "$*" in
    *"run list"*"--workflow Gate"*) cat "$RUN_LIST_GATE" ;;
    *"run list"*)                   cat "$RUN_LIST_ANY" ;;
    *actions/runs*jobs*)            printf '{"jobs":[]}\n' ;;
    *)                              printf '{}\n' ;;
esac
GH
chmod +x "$TMP/gh"

bcs() {
    env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$TMP" SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" SPIRA_GH="$TMP/gh" \
        RUN_LIST_GATE="$TMP/run-list-gate.json" RUN_LIST_ANY="$TMP/run-list-any.json" \
        bash "$HERE/forge.sh" batch-ci-status "$TMP/repo" main 2>/dev/null
}

echo
echo "positive control — Gate's own run is green, no unrelated run to confuse it:"
printf '[{"status":"completed","conclusion":"success","headSha":"good0001","databaseId":1,"updatedAt":"2026-09-24T10:00:00Z","url":"https://example.invalid/1"}]\n' \
    > "$TMP/run-list-gate.json"
cp "$TMP/run-list-gate.json" "$TMP/run-list-any.json"
_out="$(bcs)"
has "green gate: run-id reported" "run-id: 1" "$_out"
has "green gate: conclusion success" "run-conclusion: success" "$_out"

echo
echo "SEEN RED — a later, unrelated green run must not mask the Gate run's own failure:"
printf '[{"status":"completed","conclusion":"failure","headSha":"bad0002","databaseId":2,"updatedAt":"2026-09-24T11:00:00Z","url":"https://example.invalid/2"}]\n' \
    > "$TMP/run-list-gate.json"
printf '[{"status":"completed","conclusion":"success","headSha":"unrelated0003","databaseId":3,"updatedAt":"2026-09-24T11:30:00Z","url":"https://example.invalid/3"}]\n' \
    > "$TMP/run-list-any.json"
_out="$(bcs)"
has "red gate: run-id is the gate run, not the later unrelated run" "run-id: 2" "$_out"
has "red gate: conclusion is failure" "run-conclusion: failure" "$_out"
has "red gate: sha is the gate run's own sha" "head-sha: bad0002" "$_out"
[[ "$_out" != *"unrelated0003"* ]] && ok "red gate: unrelated run's sha never appears" \
    || bad "red gate: unrelated run's sha never appears" "found unrelated0003 in [$_out]"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
