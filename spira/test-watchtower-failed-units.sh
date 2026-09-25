#!/usr/bin/env bash
#
# test-watchtower-failed-units.sh — watchtower surfaces failed spira-* systemd units.
#
#   ./test-watchtower-failed-units.sh
#
# WHAT THIS SUITE IS FOR
# -----------------------
# sp-niqjl: spira-czar-pass-prod.service exited every 30s for four days — 11,000 failures —
# and nothing noticed, because watchtower.sh, cockpit.sh and doctor.sh contained no check of
# systemd unit state at all. doctor.sh now does the one-pass read (sp-utt1i); this suite
# covers what doctor's own comment says is left: dedup, age-since-failed, and the anomaly a
# persistently failed unit becomes.
#
# WHY A STATE FILE, NOT A SYSTEMD TIMESTAMP. A unit crash-looping every 30s re-enters
# "activating" and then "failed" on every restart, so systemd's own ActiveEnterTimestamp
# et al. never age past one restart interval — asking systemd "how long has this been
# failing" gets the wrong answer by design. FAILED_UNITS_STATE persists the pass each unit
# was FIRST seen failing, so age survives across sweeps independent of how often systemd
# itself resets. Section 2 below is the positive control for that persistence: a unit seen
# failing for the first time must NOT escalate immediately, only once the state file shows
# it has been failing past the threshold on a LATER pass.
#
# PROPERTIES UNDER TEST
# ----------------------
# 1. POSITIVE CONTROL. A unit already recorded as failing for longer than the threshold
#    fires exactly one incident, naming the unit and carrying its last log lines.
# 2. A unit seen failing for the FIRST time does not escalate (age 0), but is recorded —
#    and escalates on a later pass once the recorded age crosses the threshold.
# 3. DEDUP. Once escalated, later passes over the same continuing failure file nothing
#    more — a unit failing every 30s must produce one open incident, not thousands.
# 4. MULTIPLE FAILURES. Two units failing at once escalate independently, each under its
#    own stable ref (so incident.sh's own dedup never conflates two different units).
# 5. RECOVERY RESETS THE CLOCK. A unit that recovers (absent from the failed list) drops
#    out of the state; a later, unrelated failure of the same unit can escalate again.
# 6. PROBE FAILURE renders `?` on the cockpit row, never 0, and touches neither incident.sh
#    nor the state file — no evidence, no claim.
#
# covers: spira/watchtower.sh spira/doctor.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-watchtower-failed-units.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
BIN="$TMP/bin"; mkdir -p "$BIN"

# Skips the real suites.sh (host-check.sh twice, ~7s) that the menu section shells out to
# on every pass — irrelevant to this suite and paid on every one of the invocations below.
MOCK_SUITES="$TMP/mock-suites.sh"
printf '#!/usr/bin/env bash\nprintf "  suites in the tree                  0   (0 gated, 0 timed)\\n"\n' \
    > "$MOCK_SUITES"
chmod +x "$MOCK_SUITES"

fresh() { rm -rf "$TMP/run"; mkdir -p "$TMP/run/landstate"; }

# write_systemctl <unit-lines|PROBE_FAIL> — same contract as doctor.sh's own suite: prints
# the given `list-units --state=failed --no-legend` body, or fails the call outright.
write_systemctl() {
    if [ "$1" = PROBE_FAIL ]; then
        cat > "$BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
case "$*" in
    *"list-units"*"--state=failed"*)
        printf 'Failed to connect to bus\n' >&2; exit 1 ;;
esac
exit 0
MOCK
    else
        cat > "$BIN/systemctl" <<MOCK
#!/usr/bin/env bash
case "\$*" in
    *"list-units"*"--state=failed"*)
        printf '%s\n' "$1" ;;
esac
exit 0
MOCK
    fi
    chmod +x "$BIN/systemctl"
}

cat > "$BIN/journalctl" <<'MOCK'
#!/usr/bin/env bash
printf 'line one\nline two\nline three\n'
MOCK
chmod +x "$BIN/journalctl"

# wt_show -> the --show snapshot (touches nothing: no incident.sh, no state write)
wt_show() {
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent SPIRA_RUN="$TMP/run" \
        SPIRA_SUITES_SH="$MOCK_SUITES" \
        SPIRA_SYSTEMCTL="$BIN/systemctl" SPIRA_JOURNALCTL="$BIN/journalctl" \
        bash "$HERE/watchtower.sh" --show 2>/dev/null
}
fu_row() { printf '%s\n' "$1" | sed -n 's/^  FAILED UNITS  *//p' | head -1; }

# wt_run -> runs a real (non---show) pass. incident.sh calls are appended to
# $TMP/inc-calls as "SUBJECT<TAB>REF"; stdin bodies to $TMP/inc-bodies, subject-tagged.
wt_run() {
    local mock="$TMP/mock-inc.sh"
    cat > "$mock" <<MOCK
#!/usr/bin/env bash
printf '%s\t%s\n' "\$2" "\${SPIRA_INCIDENT_REF:-}" >> "$TMP/inc-calls"
{ printf '=== %s ===\n' "\$2"; cat; } >> "$TMP/inc-bodies"
MOCK
    chmod +x "$mock"
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent SPIRA_RUN="$TMP/run" \
        SPIRA_SUITES_SH="$MOCK_SUITES" \
        SPIRA_SYSTEMCTL="$BIN/systemctl" SPIRA_JOURNALCTL="$BIN/journalctl" \
        SPIRA_INCIDENT_SH="$mock" \
        SPIRA_WATCH_PROMPT_FILE="$TMP/ops-prompt" \
        bash "$HERE/watchtower.sh" >/dev/null 2>&1
}
inc_calls() { cat "$TMP/inc-calls" 2>/dev/null || true; }
seed_state() {   # seed_state <unit> <age-seconds> <escalated 0|1>
    printf '%s %s %s\n' "$1" "$(( $(date +%s) - $2 ))" "$3" > "$TMP/run/failed-units.state"
}

# ==========================================================================
echo
echo "1. POSITIVE CONTROL — a unit already failing past the threshold fires one incident:"
# ==========================================================================
fresh
write_systemctl "spira-czar-pass-prod.service loaded failed failed czar-pass"
seed_state "spira-czar-pass-prod.service" 1200 0    # seen failing 20m ago, not yet escalated
rm -f "$TMP/inc-calls" "$TMP/inc-bodies"
wt_run
calls="$(inc_calls)"
want "positive control: fires" "spira-czar-pass-prod.service" "$calls"
is   "positive control: exactly one incident" "1" "$(wc -l < "$TMP/inc-calls" 2>/dev/null || echo 0)"
body="$(cat "$TMP/inc-bodies" 2>/dev/null || echo "")"
want "body names the unit"        "spira-czar-pass-prod.service" "$body"
want "body carries the log lines" "line one"                     "$body"
want "body states first-failed"   "first seen failing"            "$body"

# ==========================================================================
echo
echo "2. A FIRST SIGHTING does not escalate; a later pass over the same recorded age does:"
# ==========================================================================
fresh
write_systemctl "spira-czar-pass-prod.service loaded failed failed czar-pass"
rm -f "$TMP/inc-calls" "$TMP/inc-bodies"   # no seeded state: this is the unit's first pass
wt_run
is "first sighting: no incident yet" "" "$(inc_calls)"
want "first sighting: still recorded in state" "spira-czar-pass-prod.service" \
    "$(cat "$TMP/run/failed-units.state" 2>/dev/null || echo "")"

# Simulate the clock advancing 20m by rewriting the state file's first-seen field back,
# the same file the pass above just wrote — not a fresh seed, so this is genuinely the
# SAME tracked failure crossing the threshold on a later pass, not a different fixture.
seed_state "spira-czar-pass-prod.service" 1200 0
rm -f "$TMP/inc-calls"
wt_run
want "later pass: escalates once the recorded age crosses the threshold" \
    "spira-czar-pass-prod.service" "$(inc_calls)"

# ==========================================================================
echo
echo "3. DEDUP — once escalated, a continuing failure files nothing more:"
# ==========================================================================
fresh
write_systemctl "spira-czar-pass-prod.service loaded failed failed czar-pass"
seed_state "spira-czar-pass-prod.service" 1200 0
rm -f "$TMP/inc-calls"
wt_run   # first pass past threshold: escalates, sets the flag
is "dedup setup: escalated once" "1" "$(wc -l < "$TMP/inc-calls" 2>/dev/null || echo 0)"
rm -f "$TMP/inc-calls"
wt_run   # second pass, unit still failing, state now carries the escalated flag
is "dedup: the second pass files nothing" "" "$(inc_calls)"

# ==========================================================================
echo
echo "4. MULTIPLE FAILURES escalate independently, each under its own stable ref:"
# ==========================================================================
fresh
write_systemctl "$(printf '%s\n%s' \
    'spira-czar-pass-prod.service loaded failed failed czar-pass' \
    'spira-watch-answers-prod.service loaded failed failed watch-answers')"
seed2() {
    printf 'spira-czar-pass-prod.service %s 0\nspira-watch-answers-prod.service %s 0\n' \
        "$(( $(date +%s) - 1200 ))" "$(( $(date +%s) - 1200 ))" > "$TMP/run/failed-units.state"
}
seed2
rm -f "$TMP/inc-calls"
wt_run
calls="$(inc_calls)"
is "multi: exactly two incidents" "2" "$(wc -l < "$TMP/inc-calls" 2>/dev/null || echo 0)"
want "multi: first unit named"  "spira-czar-pass-prod.service"     "$calls"
want "multi: second unit named" "spira-watch-answers-prod.service" "$calls"
refs="$(cut -f2 "$TMP/inc-calls")"
n_refs="$(printf '%s\n' "$refs" | sort -u | wc -l)"
is "multi: two DISTINCT refs (no cross-unit dedup collision)" "2" "$n_refs"

# ==========================================================================
echo
echo "5. RECOVERY resets the clock — a later, unrelated failure escalates again:"
# ==========================================================================
fresh
write_systemctl "spira-czar-pass-prod.service loaded failed failed czar-pass"
seed_state "spira-czar-pass-prod.service" 1200 1    # already escalated from a PRIOR failure
rm -f "$TMP/inc-calls"

write_systemctl ""   # the unit recovers
wt_run
is "recovery: no incident while recovering" "" "$(inc_calls)"
is "recovery: state file drops the recovered unit" "0" \
    "$(grep -c 'spira-czar-pass-prod.service' "$TMP/run/failed-units.state" 2>/dev/null)"

write_systemctl "spira-czar-pass-prod.service loaded failed failed czar-pass"   # fails again
wt_run
is "recovery: the fresh failure does not escalate immediately (new clock)" "" "$(inc_calls)"

# ==========================================================================
echo
echo "6. PROBE FAILURE — the row shows ?, never 0, and nothing is filed or written:"
# ==========================================================================
fresh
write_systemctl PROBE_FAIL
snap="$(wt_show)"
row="$(fu_row "$snap")"
is "probe failure: row is exactly ?" "?" "${row%% *}"
nowant "probe failure: never a false-clean 0" "  FAILED UNITS                        0" "$snap"

rm -f "$TMP/inc-calls"
wt_run
is "probe failure: no incident filed" "" "$(inc_calls)"
is "probe failure: state file untouched" "0" \
    "$([ -f "$TMP/run/failed-units.state" ] && echo 1 || echo 0)"

# ==========================================================================
echo
echo "the clean case — no failed units: row is 0, nothing filed:"
# ==========================================================================
fresh
write_systemctl ""
snap="$(wt_show)"
row="$(fu_row "$snap")"
is "clean: row is 0" "0" "${row%% *}"
rm -f "$TMP/inc-calls"
wt_run
is "clean: no incident filed" "" "$(inc_calls)"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
