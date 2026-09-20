#!/usr/bin/env bash
# test-forge-run-metadata.sh — forge.sh run-metadata derives last-activity correctly.
#
# Three cases:
#   1. Run with updated_at, no job/step timestamps → last-activity from updated_at.
#   2. Run with updated_at older than a step timestamp → last-activity from step.
#   3. Run with updated_at newer than stale step timestamps (single-step job in
#      progress) → last-activity from updated_at, not the stale step.
#
# No network is reached; ghq is stubbed via SPIRA_GH.
#
# covers: spira/forge.sh
# timeout: 60
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-forge-run-metadata.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
git init -q "$TMP/repo"

# gh stub: returns RUN_JSON for run queries, JOBS_JSON for jobs queries.
cat > "$TMP/gh" <<'GH'
#!/usr/bin/env bash
case "$*" in
    *actions/runs/*/jobs*) cat "$JOBS_JSON" ;;
    *actions/runs/*)       cat "$RUN_JSON" ;;
    *)                     printf '{}\n' ;;
esac
GH
chmod +x "$TMP/gh"

# Helper: run forge.sh run-metadata with controlled JSON responses.
run_metadata() {
    env -i PATH="/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin" \
        HOME="$TMP" SPIRA_CONF=/nonexistent SPIRA_RUN="$TMP/run" \
        SPIRA_GH="$TMP/gh" RUN_JSON="$TMP/run.json" JOBS_JSON="$TMP/jobs.json" \
        bash "$HERE/forge.sh" run-metadata "$TMP/repo" 42 2>/dev/null
}

# epoch_secs_ago <n> → ISO8601 timestamp for n seconds ago
epoch_secs_ago() {
    python3 -c "
import datetime, sys
n = int(sys.argv[1])
t = datetime.datetime.utcnow() - datetime.timedelta(seconds=n)
print(t.strftime('%Y-%m-%dT%H:%M:%SZ'))
" "$1"
}

# epoch_from_iso <ts> → unix epoch
epoch_from_iso() {
    python3 -c "
import datetime, calendar, sys
t = datetime.datetime.strptime(sys.argv[1].rstrip('Z'), '%Y-%m-%dT%H:%M:%S')
print(calendar.timegm(t.timetuple()))
" "$1"
}

# =============================================================================
# 1. Run with updated_at, no job/step timestamps → last-activity from updated_at.
#    Positive control: without the updated_at fix, this would emit no last-activity.
# =============================================================================
updated_at_1="$(epoch_secs_ago 30)"
printf '{"run_started_at":"%s","updated_at":"%s"}\n' \
    "$(epoch_secs_ago 3700)" "$updated_at_1" > "$TMP/run.json"
printf '{"jobs":[]}\n' > "$TMP/jobs.json"
out1="$(run_metadata)"
want   "1. updated_at only: started-at present"      "started-at: "     "$out1"
want   "1. updated_at only: last-activity present"   "last-activity: "  "$out1"
# The last-activity value must be close to epoch_from_iso(updated_at_1).
expected1="$(epoch_from_iso "$updated_at_1")"
got1="$(printf '%s\n' "$out1" | grep '^last-activity: ' | tail -1 | cut -d' ' -f2)"
is     "1. updated_at only: last-activity value"     "$expected1"       "${got1:-}"

# =============================================================================
# 2. Run with updated_at OLDER than a completed step timestamp → last-activity
#    from the step (verdict takes the max).
# =============================================================================
updated_at_2="$(epoch_secs_ago 500)"
step_at_2="$(epoch_secs_ago 60)"
printf '{"run_started_at":"%s","updated_at":"%s"}\n' \
    "$(epoch_secs_ago 3700)" "$updated_at_2" > "$TMP/run.json"
printf '{"jobs":[{"started_at":"%s","completed_at":"%s","steps":[{"started_at":"%s","completed_at":"%s"}]}]}\n' \
    "$(epoch_secs_ago 3700)" "" "$(epoch_secs_ago 3700)" "$step_at_2" > "$TMP/jobs.json"
out2="$(run_metadata)"
expected2="$(epoch_from_iso "$step_at_2")"
# Find the maximum last-activity emitted.
got2="$(printf '%s\n' "$out2" | grep '^last-activity: ' | cut -d' ' -f2 | sort -n | tail -1)"
is "2. step newer than updated_at: last-activity is step epoch" "$expected2" "${got2:-}"

# =============================================================================
# 3. Single-step job in progress: updated_at recent, step timestamps stale.
#    → last-activity from updated_at (not the stale step started_at).
#    Positive control: a verdict taking only job/step timestamps would see a stale
#    last-activity and misread the healthy run as hung.
# =============================================================================
updated_at_3="$(epoch_secs_ago 20)"
printf '{"run_started_at":"%s","updated_at":"%s"}\n' \
    "$(epoch_secs_ago 3700)" "$updated_at_3" > "$TMP/run.json"
# Single step: only started_at set, no completed_at (step is still running).
printf '{"jobs":[{"started_at":"%s","completed_at":null,"steps":[{"started_at":"%s","completed_at":null}]}]}\n' \
    "$(epoch_secs_ago 3700)" "$(epoch_secs_ago 3700)" > "$TMP/jobs.json"
out3="$(run_metadata)"
expected3="$(epoch_from_iso "$updated_at_3")"
got3="$(printf '%s\n' "$out3" | grep '^last-activity: ' | cut -d' ' -f2 | sort -n | tail -1)"
is "3. single-step in-progress: last-activity is updated_at" "$expected3" "${got3:-}"

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
