#!/usr/bin/env bash
#
# test-sentinel.sh — land_escalate does not ask twice while an ask is already open;
#   a CLOSED ask does not suppress a new one.
#
#   ./test-sentinel.sh
#
# WHY THIS EXISTS. land_escalate was rate limited on a clock, so nine identical "Spira is
# landing nothing" decisions reached the operator's pane in one day. He closed eight and the
# ninth arrived anyway. The fix asks the database rather than a clock: ask_already_open finds
# an OPEN bead with the subject and returns 0, which suppresses the re-ask. This suite asserts
# both sides of that check:
#
#   1. POSITIVE CONTROL — no open ask: land_escalate reaches the operator. Without this, a
#      suppression that mutes everything is indistinguishable from one that mutes correctly.
#
#   2. OPEN-ASK SUPPRESSION — an open bead with the subject suppresses the re-ask. The
#      fixture seeds the bead explicitly; an empty database passes for the wrong reason.
#
#   3. CLOSED-ASK PASS-THROUGH — a CLOSED ask is an answered question. The same condition
#      recurring after an answer is new information (law-alerts-must-be-actionable), so a
#      closed ask must NOT suppress a new escalation. This is the half that regresses
#      silently: without it, an ask closed by the operator becomes a permanent mute button.
#
# A REAL bd ON A FIXTURE DATABASE (law-prefer-the-real-dependency). ask_already_open queries
# the database for open beads; a stub would reproduce the surface remembered here, drift
# silently, and prove nothing about the real suppression path.
#
# defect: sp-yki4
# covers: spira/sentinel.sh spira/lib.sh
# hermetic-ok: uses a fixture database, no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-sentinel
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up sentinel || { echo "test-sentinel: could not build a fixture database"; exit 1; }

# land_escalate is in lib.sh; act is defined in sentinel.sh and unavailable here.
# Provide a stub before sourcing so land_escalate can call it.
RUN="$TMP/run"; mkdir -p "$RUN"
export SPIRA_RUN="$RUN"
acted=0
act() { acted=$((acted+1)); }
# shellcheck disable=SC1090
. "$HERE/lib.sh"

B() { bd -C "$SPIRA_DB" "$@"; }

export ASK_LOG="$TMP/ask.log"
cat > "$TMP/ask.sh" <<'ASKSH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$ASK_LOG"
ASKSH
chmod +x "$TMP/ask.sh"
export SPIRA_NOTIFY="$TMP/ask.sh"
export SPIRA_HOME="$TMP"

# Run land_escalate with the clock suppressed so only ask_already_open governs the decision.
do_escalate() {
    SPIRA_LAND_ESCALATE_EVERY=0 \
        land_escalate "the landing worker will not start" "evidence"
}

echo "test-sentinel.sh"

# ======================================================================================
echo
echo "positive control — land_escalate reaches the operator when no ask is open:"
# ======================================================================================
testdb_reset
: > "$ASK_LOG"; rm -f "$RUN/landing.escalated"
do_escalate >/dev/null 2>&1
want "and reaches the operator" "Spira is landing nothing" "$(cat "$ASK_LOG")"

# ======================================================================================
echo
echo "open-ask suppression — does not ask again while one is already open:"
# ======================================================================================
# THE FIXTURE IS NECESSARY. An empty database passes the assertion for the wrong reason —
# ask_already_open would return 1 (nothing found) and the clock's SPIRA_LAND_ESCALATE_EVERY=0
# would let the escalation through — making the test vacuously green.
testdb_reset
# THE SEEDED LABEL MUST MATCH SPIRA_ASK_LABEL. ask_already_open queries with
# --label $SPIRA_ASK_LABEL; a bead carrying a different label is invisible to
# the query even when open — making suppression silently fail. conf.sh defaults
# SPIRA_ASK_LABEL to needs-operator; production spira.conf sets it to needs-ryan.
# Hardcoding either value broke the other environment; use the live variable.
printf '{"id":"sp-ask1","title":"Spira is landing nothing — its last run exited 1","status":"open","issue_type":"decision","labels":["%s"],"updated_at":"2026-09-07T00:00:00Z"}\n' \
    "${SPIRA_ASK_LABEL:-needs-operator}" | testdb_seed
: > "$ASK_LOG"; rm -f "$RUN/landing.escalated"
land_escalate "the landing worker will not start" "evidence" >/dev/null 2>&1
is "and does not ask again while one is still open" "" "$(cat "$ASK_LOG")"

# ======================================================================================
echo
echo "closed-ask pass-through — a closed ask does NOT suppress a new escalation:"
# ======================================================================================
# A closed ask is an answered question; the same condition recurring after an answer is new
# information. Without this half, an operator closing an ask silently mutes the escalation
# forever — which is the exact failure ask_already_open's OPEN filter exists to prevent.
testdb_reset
printf '{"id":"sp-ask1","title":"Spira is landing nothing — its last run exited 1","status":"closed","issue_type":"decision","labels":["%s"],"updated_at":"2026-09-07T00:00:00Z"}\n' \
    "${SPIRA_ASK_LABEL:-needs-operator}" | testdb_seed
: > "$ASK_LOG"; rm -f "$RUN/landing.escalated"
do_escalate >/dev/null 2>&1
want "a closed ask does not suppress a new escalation" "Spira is landing nothing" "$(cat "$ASK_LOG")"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
