#!/usr/bin/env bash
# test-forge-main-gate-status.sh — forge.sh main-gate-status reads a real gh run list,
# and treats a provision fault as unknown, not red.
#
# sp-221n8: batch.sh holds the queue while main-gate-status reads red or unknown, with
# no way to tell the two apart from the wire shape alone. A push gate that concluded
# failure only because provision never ran the suites (gate exit 75, "branch was not
# tested") is not evidence main is red — it is the same harness fault check-status
# already reports as provision_fault, and main-gate-status must read it the same way,
# or a single bad runner poisons every batch behind it as if main were actually red.
#
# covers: spira/forge.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-forge-main-gate-status.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
git init -q "$TMP/repo"

# A gh stand-in that answers `run list --workflow gate.yml --event push` with the run
# in $RUN_LIST, and `actions/runs/<id>/jobs` with $JOBS_JSON (default: no jobs).
cat > "$TMP/gh" <<'GH'
#!/usr/bin/env bash
case "$*" in
    *"run list"*"gate.yml"*) cat "$RUN_LIST" ;;
    *actions/runs*jobs*)     [ -n "${JOBS_JSON:-}" ] && cat "$JOBS_JSON" || printf '{"jobs":[]}\n' ;;
    *)                       printf '{}\n' ;;
esac
GH
chmod +x "$TMP/gh"
JOBS_JSON=""

run_list() {   # run_list <status> <conclusion> <sha> <run-id>
    printf '[{"status":"%s","conclusion":"%s","headSha":"%s","databaseId":%s}]\n' \
        "$1" "$2" "$3" "$4" > "$TMP/run-list.json"
}
mgs() {
    env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$TMP" SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" SPIRA_GH="$TMP/gh" RUN_LIST="$TMP/run-list.json" \
        JOBS_JSON="${JOBS_JSON:-}" \
        bash "$HERE/forge.sh" main-gate-status "$TMP/repo" 2>/dev/null
}

echo
echo "positive control — a still-running push gate reads unknown:"
run_list in_progress "" abc123 1
is "in-progress run reads unknown" "unknown abc123" "$(mgs)"

echo
echo "positive control — no push run at all reads unknown:"
printf '[]\n' > "$TMP/run-list.json"
is "empty run list reads unknown" "unknown" "$(mgs)"

echo
echo "a completed, successful push gate reads green:"
run_list completed success def456 2
is "successful run reads green" "green def456" "$(mgs)"

echo
echo "a completed, failed push gate reads red:"
run_list completed failure bad0001 3
is "failed run reads red" "red bad0001" "$(mgs)"

echo
echo "provision fault: a failed push gate whose provision job also failed reads unknown, not red:"
run_list completed failure bad0002 4
printf '{"jobs":[{"id":9,"name":"provision","conclusion":"failure"},{"id":10,"name":"gate","conclusion":"failure"}]}\n' \
    > "$TMP/jobs-prov-fail.json"
JOBS_JSON="$TMP/jobs-prov-fail.json"
is "provision fault reads unknown" "unknown bad0002" "$(mgs)"

echo "positive control — provision job succeeded, gate still failed → still red:"
printf '{"jobs":[{"id":9,"name":"provision","conclusion":"success"},{"id":10,"name":"gate","conclusion":"failure"}]}\n' \
    > "$TMP/jobs-prov-ok.json"
JOBS_JSON="$TMP/jobs-prov-ok.json"
is "provision success → still red" "red bad0002" "$(mgs)"
JOBS_JSON=""

echo "positive control — a green run with a failed provision job stays green (jobs are only consulted when red):"
run_list completed success def789 5
JOBS_JSON="$TMP/jobs-prov-fail.json"
is "green ignores jobs" "green def789" "$(mgs)"
JOBS_JSON=""

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
