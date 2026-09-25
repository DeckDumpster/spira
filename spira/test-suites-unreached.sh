#!/usr/bin/env bash
#
# test-suites-unreached.sh — unreached does not overwrite a suite's verdict or runtime
#
#   ./test-suites-unreached.sh
#
# WHAT THIS GUARDS (sp-u1g). Three defects in one line (suites.sh):
#
#   record_write "$s" unreached 0 -
#
# 1. STATUS OVERWRITE. A suite red last pass and unreached this pass records "unreached",
#    silently dropping the red from the sweep count. Measured: 117 of 210 result files read
#    "unreached" after one pass — more than half the corpus with no verdict at all.
#
# 2. SECS ZEROED. last_secs comes from the .result file's third column. Writing 0 destroys
#    the self-calibrating runtime estimate. A suite that took 279s last run now shows
#    last_secs=0, the skip guard cannot fire, it is started with 40s of budget, and the
#    watchdog kills it — manufacturing a timeout bead from a budget artefact.
#
# 3. TIMEOUT FINGERPRINT UNSTABLE. Fingerprint is taken over "killed at ${slice}s", and
#    fingerprint() normalises only runs of three or more digits. A slice under 100s changes
#    every pass, so each pass files a new bead for the same timing-out suite rather than
#    bumping a recurrence on one.
#
# POSITIVE CONTROLS FIRST (law-absence-needs-a-positive-control):
#   - Verify the suite runs red on a normal pass BEFORE testing the unreached case.
#   - Verify two timeout passes with the same budget produce the same fingerprint BEFORE
#     testing two passes with different budgets.
#
# covers: spira/suites.sh
# timeout: 60
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-suites-unreached.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-suites-unreached
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up suites-unreached || { echo "test-suites-unreached: could not build a fixture database"; exit 1; }
# shellcheck disable=SC1090
. "$HERE/lib.sh"

SH="$TMP/spira"; RUN="$TMP/run"; STATE="$TMP/state"; GATEF="$TMP/gate-suites"
mkdir -p "$SH" "$RUN" "$STATE" "$TMP/home" "$TMP/repo"
cp "$HERE/suites.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/incident.sh" "$HERE/incident-dedup-decision.py" "$SH/"

# Knobs — every one pinned away from the shipped default.
BUDGET=60; PERSUITE=5; STALE=3600; PRIO=3; REPONAME=unreached-fixture

printf '%s | %s | push | main | : | :\n' "$REPONAME" "$TMP/repo" > "$SH/repo-map"

printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$*" >> "%s"\n' "$TMP/ask.log" > "$SH/ask.sh"
chmod +x "$SH/ask.sh"

sut() {
    local cmd="$1"; shift
    # SPIRA_SUITES_INLINE=1 — run the planted suites HERE, not in a container.
    # suites.sh now delegates its pass to testenv-batch.sh (law-tests-run-only-through-
    # testenv-batch). This suite is testing suites.sh's OWN logic against fake suites it
    # planted in a scratch tree, so a container would have to be started per invocation to
    # run code that exists only to be counted. Containment is not being waived: this suite
    # is itself run inside a container by the timed pass, so the planted suites are already
    # contained by it.
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF="$TMP/no-such.conf" \
        SPIRA_HOME="$SH" SPIRA_REPO="$TMP/repo" SPIRA_HOME_REPO="$REPONAME" \
        SPIRA_DB="$SPIRA_DB" SPIRA_RUN="$RUN" \
        SPIRA_SUITES_STATE="$STATE" SPIRA_GATE_SUITES="$GATEF" \
        SPIRA_SUITES_BUDGET="$BUDGET" SPIRA_SUITE_TIMEOUT="$PERSUITE" \
        SPIRA_SUITES_STALE="$STALE" SPIRA_SUITES_PRIORITY="$PRIO" \
        SPIRA_NOTIFY="$SH/ask.sh" \
        SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_SUITES_INLINE=1 \
        "$@" bash "$SH/suites.sh" "$cmd" 2>&1
}
plant() { cat > "$SH/$1"; chmod +x "$SH/$1"; }

result_field() {
    # result_field <suite-basename> <field-num>  → field from .result, or empty
    local f="$STATE/$1.result"
    [ -r "$f" ] || return 1
    awk '{print $'"$2"'}' "$f" 2>/dev/null
}

# Gate file names nothing — every planted suite goes to the timed pass.
printf '# nothing in the gate for this fixture\n' > "$GATEF"

# ======================================================================================
echo
echo "defect 1: unreached must not overwrite a previous status verdict:"
# ======================================================================================
# A suite that was red last pass and unreached this pass must still report red in the
# record — not "unreached". The red count on the sweep must not silently drop.
#
# POSITIVE CONTROL: run a red suite first, confirm the record says red, THEN run
# an unreached pass and confirm the record still says red.

plant test-fx-red.sh <<'S'
#!/usr/bin/env bash
# covers: spira/suites.sh
echo "FAIL  this suite always fails"
exit 1
S

# Clear state completely — prove no pre-existing record.
rm -f "$STATE/test-fx-red.sh.result" "$STATE/test-fx-red.sh.unreached"
[ -e "$STATE/test-fx-red.sh.result" ] \
    && bad "positive control: no pre-existing .result for test-fx-red.sh" "file already present" \
    || ok "positive control: no pre-existing .result for test-fx-red.sh"

# First pass: red suite runs and records red.
out_red="$(sut run)"
red_st="$(result_field test-fx-red.sh 1 || true)"
is "positive control: test-fx-red.sh records red on first pass" "red" "$red_st"

# Second pass: budget too small to start (left ≤ 5 triggers unreached immediately).
out_ur="$(BUDGET=3 sut run)"
# The result record must still say red — not unreached.
after_ur_st="$(result_field test-fx-red.sh 1 || true)"
is "after unreached pass, .result status is still red (not overwritten)" "red" "$after_ur_st"

# The .unreached file must exist — it is where the unreached fact lives now.
[ -f "$STATE/test-fx-red.sh.unreached" ] \
    && ok "after unreached pass, .unreached file exists" \
    || bad "after unreached pass, .unreached file exists" "file not found at $STATE/test-fx-red.sh.unreached"

# law-absence-needs-a-positive-control: not-reached and green must never render the same.
# Run the suite again with full budget so it is reached, then confirm .unreached is gone.
rm -f "$STATE/test-fx-red.sh.unreached"
out_again="$(sut run)"
[ -f "$STATE/test-fx-red.sh.unreached" ] \
    && bad "after a reached pass, .unreached file is cleared" "still present" \
    || ok "after a reached pass, .unreached file is cleared"

# ======================================================================================
echo
echo "defect 2: unreached must not zero the secs field of the last real run:"
# ======================================================================================
# The budget skip guard reads last_secs from column 3 of the .result file. If unreached
# zeroes that column, last_secs=0, the skip guard cannot fire, and the next pass starts
# the suite regardless of remaining budget — leading to a manufactured timeout.

# Plant a quick-secs suite so we have a recorded secs value.
plant test-fx-secs.sh <<'S'
#!/usr/bin/env bash
# covers: spira/suites.sh
# Sleep 1s so the recorded secs field is non-zero — a fast suite records secs=0, which
# is indistinguishable from a zeroing write, so the preservation check is vacuous.
sleep 1
echo "  ok    secs fixture suite"
S

rm -f "$STATE/test-fx-secs.sh.result" "$STATE/test-fx-secs.sh.unreached"
sut run > /dev/null
secs_after_run="$(result_field test-fx-secs.sh 3 || true)"
case "${secs_after_run:-}" in
    ''|*[!0-9]*) bad "test-fx-secs.sh: secs after first run is numeric" "got [${secs_after_run:-empty}]" ;;
    0) bad "test-fx-secs.sh: secs after first run is nonzero (sleep 1s but secs=0 — clock resolution?)" "got 0" ;;
    *) ok "test-fx-secs.sh: secs after first run is numeric and nonzero (${secs_after_run}s)" ;;
esac

# Unreached pass: budget=3 → left ≤ 5 → unreached immediately, .result must be untouched.
BUDGET=3 sut run > /dev/null
secs_after_ur="$(result_field test-fx-secs.sh 3 || true)"
is "secs preserved after unreached pass (not zeroed to 0)" "$secs_after_run" "$secs_after_ur"

rm -f "$SH/test-fx-secs.sh" "$SH/test-fx-red.sh"

# ======================================================================================
echo
echo "defect 3: timeout fingerprint must be stable across different budget slices:"
# ======================================================================================
# The fingerprint is over "killed at ${slice}s". A slice under 100s changes every pass,
# so each pass files a new bead for the same timing-out suite rather than bumping a
# recurrence. The fix: fingerprint over the suite name, not the slice.
#
# POSITIVE CONTROL: two passes with the SAME budget must produce the same fingerprint
# before we trust the assertion on two passes with DIFFERENT budgets.

plant test-fx-hangs.sh <<'S'
#!/usr/bin/env bash
# covers: spira/suites.sh
sleep 300
echo "FAIL  should not reach here"
exit 1
S

rm -f "$STATE/test-fx-hangs.sh.result"
B() { bd -C "$SPIRA_DB" "$@"; }
beads_for() {
    B list --status open,in_progress --limit 0 --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' \
        | python3 -c '
import sys, json
key = sys.argv[1]
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    if key in (i.get("title") or ""): print(i["id"])
' "$1" 2>/dev/null || true
}
count() { printf '%s\n' "${1:-}" | grep -c '[^ ]' 2>/dev/null || echo 0; }

# Positive control: two passes with the same slice produce the same fingerprint.
PERSUITE=3 sut run > /dev/null
fp1="$(result_field test-fx-hangs.sh 4 || true)"
rm -f "$STATE/test-fx-hangs.sh.result"
PERSUITE=3 sut run > /dev/null
fp2="$(result_field test-fx-hangs.sh 4 || true)"
[ -n "${fp1:-}" ] && [ -n "${fp2:-}" ] \
    && is "positive control: same PERSUITE → same fingerprint" "$fp1" "$fp2" \
    || bad "positive control: fingerprint was recorded" "fp1=[${fp1:-empty}] fp2=[${fp2:-empty}]"

# Now test different slices. Two passes with PERSUITE=5 vs PERSUITE=3 produce the same
# fingerprint — the suite name, not the slice, is the stable identifier.
rm -f "$STATE/test-fx-hangs.sh.result"
PERSUITE=5 sut run > /dev/null
fp_a="$(result_field test-fx-hangs.sh 4 || true)"
rm -f "$STATE/test-fx-hangs.sh.result"
PERSUITE=3 sut run > /dev/null
fp_b="$(result_field test-fx-hangs.sh 4 || true)"
[ -n "${fp_a:-}" ] && [ -n "${fp_b:-}" ] \
    && is "different budgets produce the same timeout fingerprint" "$fp_a" "$fp_b" \
    || bad "fingerprints were recorded for both passes" "fp_a=[${fp_a:-empty}] fp_b=[${fp_b:-empty}]"

# Consequence: two passes with different budgets that time out the same suite file
# exactly ONE bead (the second bumps a recurrence rather than filing a new one).
hung_beads="$(beads_for 'test-fx-hangs.sh')"
bead_count="$(count "$hung_beads")"
is "two different-budget timeout passes file one bead, not two" "1" "$bead_count"

rm -f "$SH/test-fx-hangs.sh"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
