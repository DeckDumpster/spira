#!/usr/bin/env bash
#
# test-bd-update-inflight-guard.sh — bd-update-inflight-guard.sh refuses spec-field
#   edits on an in-flight bead, and honours its named override.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): the guard MUST fire
# on the exact command shape that produced the three mismatched PRs before any negative
# control runs. Silence from the negative controls is then evidence the guard works,
# not that it never could.
#
# The exact command driven here: `bd -C <db> update <id> --description "<text>"`
# on a bead whose record shows status=in_progress, an assignee, and a branch:* label —
# the three evidence signals the guard checks.
#
# law-a-regression-test-must-be-seen-to-fail: run this suite against the pre-fix tree
# (without bd-update-inflight-guard.sh installed as a PreToolUse hook) and confirm that
# the POSITIVE CONTROL cases below report FAIL before the guard is in place.
#
# defect: sp-lbij
# covers: spira/bd-update-inflight-guard.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1"; esac; }

echo "test-bd-update-inflight-guard.sh"

GUARD="$HERE/bd-update-inflight-guard.sh"
[ -f "$GUARD" ] || { printf 'SKIP bd-update-inflight-guard.sh not found at %s\n' "$GUARD"; exit 77; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-bd-update-inflight-guard
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT; trap 'exit 143' INT TERM
testdb_up inflight_guard || { printf 'SKIP test-bd-update-inflight-guard: fixture failed\n'; exit 75; }

# Create an in-flight bead: status=in_progress, assignee, branch:* label.
INFLIGHT_ID="$("$SPIRA_BD" -C "$SPIRA_DB" create "guard test bead" -l "spira,plan" 2>/dev/null \
    | grep -oE 'sp-[a-z0-9]+')"
[ -n "$INFLIGHT_ID" ] || { printf 'SKIP could not create test bead\n'; exit 75; }
"$SPIRA_BD" -C "$SPIRA_DB" update "$INFLIGHT_ID" \
    --assignee "aeon-test" --add-label "branch:spira/sp-test" 2>/dev/null || true
# Use --force to move status to in_progress without claiming.
"$SPIRA_BD" -C "$SPIRA_DB" update "$INFLIGHT_ID" --status in_progress --force 2>/dev/null || true

# Create an open (not in-flight) bead for negative controls.
OPEN_ID="$("$SPIRA_BD" -C "$SPIRA_DB" create "open test bead" -l "spira,plan" 2>/dev/null \
    | grep -oE 'sp-[a-z0-9]+')"
[ -n "$OPEN_ID" ] || { printf 'SKIP could not create open test bead\n'; exit 75; }

# run_guard <command> [KEY=VAL ...] → combined stdout+stderr; returns guard exit code.
# Builds the PreToolUse JSON payload and pipes it to the guard WITHOUT SPIRA_AEON set
# (simulating a concierge/interactive session). Inherits the current environment so that
# SPIRA_DB and SPIRA_BD reach the fixture; SPIRA_AEON is explicitly cleared.
run_guard() {
    local cmd="$1"; shift
    local rc=0 out
    out=$(python3 -c '
import json, sys
print(json.dumps({"tool_name": "Bash", "tool_input": {"command": sys.argv[1]}}))
' "$cmd" | SPIRA_AEON="" "$@" bash "$GUARD" 2>&1) || rc=$?
    printf '%s' "$out"
    return "$rc"
}

# run_guard_aeon: same but WITH SPIRA_AEON set (simulating an aeon session).
run_guard_aeon() {
    local cmd="$1"; shift
    local rc=0 out
    out=$(python3 -c '
import json, sys
print(json.dumps({"tool_name": "Bash", "tool_input": {"command": sys.argv[1]}}))
' "$cmd" | SPIRA_AEON=1 "$@" bash "$GUARD" 2>&1) || rc=$?
    printf '%s' "$out"
    return "$rc"
}

# ==========================================================================
echo
echo "POSITIVE CONTROL — spec-field edit on an in-flight bead is blocked"
# ==========================================================================
# The exact command shape that produced the three mismatched PRs:
# bd -C <db> update <id> --description "<new spec>"
# The bead has status=in_progress, an assignee, and a branch:* label.

out=$(run_guard "bd -C $SPIRA_DB update $INFLIGHT_ID --description revised-spec" || true)
want "BLOCKED by guard name"              "BLOCKED by bd-update-inflight-guard" "$out"
want "shows the bead id"                  "$INFLIGHT_ID"                         "$out"
want "shows in_progress evidence"         "status=in_progress"                   "$out"
want "names comment as safe alternative"  "comment"                              "$out"
want "names the override"                 "BEAD_EDIT_IN_FLIGHT_CONSIDERED"       "$out"

out=$(run_guard "bd -C $SPIRA_DB update $INFLIGHT_ID --design new-design" || true)
want "--design on in-flight bead blocked" "BLOCKED by bd-update-inflight-guard"  "$out"

out=$(run_guard "bd -C $SPIRA_DB update $INFLIGHT_ID --acceptance new-criteria" || true)
want "--acceptance on in-flight bead blocked" "BLOCKED by bd-update-inflight-guard" "$out"

out=$(run_guard "bd -C $SPIRA_DB update $INFLIGHT_ID --body-file /tmp/new.md" || true)
want "--body-file on in-flight bead blocked"  "BLOCKED by bd-update-inflight-guard" "$out"

out=$(run_guard "bd -C $SPIRA_DB update $INFLIGHT_ID --design-file /tmp/design.md" || true)
want "--design-file on in-flight bead blocked" "BLOCKED by bd-update-inflight-guard" "$out"

# Short form -d is also a spec flag.
out=$(run_guard "bd -C $SPIRA_DB update $INFLIGHT_ID -d new-desc" || true)
want "-d on in-flight bead blocked"           "BLOCKED by bd-update-inflight-guard" "$out"

# ==========================================================================
echo
echo "not-in-flight bead — spec edits are allowed"
# ==========================================================================
out=$(run_guard "bd -C $SPIRA_DB update $OPEN_ID --description new-spec" 2>&1) || true
nowant "open bead description edit allowed"   "BLOCKED"                           "$out"

out=$(run_guard "bd -C $SPIRA_DB update $OPEN_ID --design new-design" 2>&1) || true
nowant "open bead design edit allowed"        "BLOCKED"                           "$out"

# ==========================================================================
echo
echo "non-spec flags on in-flight bead — allowed"
# ==========================================================================
out=$(run_guard "bd -C $SPIRA_DB update $INFLIGHT_ID --add-label spira" 2>&1) || true
nowant "label update on in-flight bead allowed"   "BLOCKED"                       "$out"

out=$(run_guard "bd -C $SPIRA_DB update $INFLIGHT_ID --priority P2" 2>&1) || true
nowant "priority update on in-flight bead allowed" "BLOCKED"                      "$out"

out=$(run_guard "bd -C $SPIRA_DB update $INFLIGHT_ID --status in_progress" 2>&1) || true
nowant "status update on in-flight bead allowed"   "BLOCKED"                      "$out"

# ==========================================================================
echo
echo "aeon sessions — never blocked"
# ==========================================================================
out=$(run_guard_aeon "bd -C $SPIRA_DB update $INFLIGHT_ID --description new-spec" 2>&1) || true
nowant "aeon: spec edit not blocked"               "BLOCKED"                      "$out"

# ==========================================================================
echo
echo "override is honoured"
# ==========================================================================
out=$(run_guard "bd -C $SPIRA_DB update $INFLIGHT_ID --description new-spec" \
    BEAD_EDIT_IN_FLIGHT_CONSIDERED=1 2>&1) || true
nowant "env override passes"                       "BLOCKED"                      "$out"

out=$(run_guard "BEAD_EDIT_IN_FLIGHT_CONSIDERED=1 bd -C $SPIRA_DB update $INFLIGHT_ID --description x" \
    2>&1) || true
nowant "inline override passes"                    "BLOCKED"                      "$out"

# ==========================================================================
echo
echo "prose and single-quoted spans are not refused"
# ==========================================================================
out=$(run_guard "echo 'bd update sp-xxxx --description new'" 2>&1) || true
nowant "single-quoted prose not refused"           "BLOCKED"                      "$out"

out=$(run_guard "cat <<'EOF'
never use bd update sp-xxxx --description without care
EOF" 2>&1) || true
nowant "heredoc prose not refused"                 "BLOCKED"                      "$out"

# ==========================================================================
echo
echo "non-Bash tools pass through"
# ==========================================================================
out=$(python3 -c '
import json, sys
print(json.dumps({"tool_name": "Read", "tool_input": {"file_path": "spec.txt"}}))
' | env -i PATH="$PATH" HOME="$HOME" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$SPIRA_BD" \
    bash "$GUARD" 2>&1) || true
nowant "non-Bash tool not blocked"                 "BLOCKED"                      "$out"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
