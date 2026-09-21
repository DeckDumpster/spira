#!/usr/bin/env bash
#
# test-bd-delivers-label-guard.sh — bd-delivers-label-guard.sh refuses
#   bd label remove <id> delivers:* in aeon sessions and names the producer.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): the guard must
# fire on the exact command shape used to escape a delivers criterion before any
# negative-control assertions run. Silence from the negative controls is then
# evidence the guard works, not that it never could.
#
# Acceptance criteria from sp-vv1wi:
#   bd -C "$FIXTURE" label remove <id> delivers:action → rc != 0, count stays 1
#   incident.sh retire-unsatisfiable-delivers --dry-run → not blocked by the guard
#
# defect: sp-vv1wi
# covers: spira/bd-delivers-label-guard.sh spira/aeon.sh spira/incident.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1"; esac; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-bd-delivers-label-guard.sh"

GUARD="$HERE/bd-delivers-label-guard.sh"
[ -f "$GUARD" ] || { printf 'SKIP bd-delivers-label-guard.sh not found at %s\n' "$GUARD"; exit 77; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-bd-delivers-label-guard
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT; trap 'exit 143' INT TERM
testdb_up delivers_guard || { printf 'SKIP test-bd-delivers-label-guard: fixture failed\n'; exit 75; }

# Create a bead and stamp a delivers: label.
PROBE_ID="$("$SPIRA_BD" -C "$SPIRA_DB" create "guard probe" --type task 2>/dev/null \
    | grep -oE 'sp-[a-z0-9]+')"
[ -n "$PROBE_ID" ] || { printf 'SKIP could not create probe bead\n'; exit 75; }
"$SPIRA_BD" -C "$SPIRA_DB" label add "$PROBE_ID" delivers:action 2>/dev/null || true

# run_guard_aeon <command>: pipe a Bash-tool JSON payload to the guard with SPIRA_AEON set.
run_guard_aeon() {
    local cmd="$1"; shift
    local rc=0 out
    out=$(python3 -c '
import json, sys
print(json.dumps({"tool_name": "Bash", "tool_input": {"command": sys.argv[1]}}))
' "$cmd" | SPIRA_AEON=aeon-test SPIRA_DB="$SPIRA_DB" SPIRA_BD="$SPIRA_BD" \
    BEAD_ID="$PROBE_ID" "$@" bash "$GUARD" 2>&1) || rc=$?
    printf '%s' "$out"
    return "$rc"
}

# run_guard_no_aeon: same without SPIRA_AEON, simulating a maintenance session.
run_guard_no_aeon() {
    local cmd="$1"; shift
    local rc=0 out
    out=$(python3 -c '
import json, sys
print(json.dumps({"tool_name": "Bash", "tool_input": {"command": sys.argv[1]}}))
' "$cmd" | SPIRA_AEON="" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$SPIRA_BD" \
    BEAD_ID="$PROBE_ID" "$@" bash "$GUARD" 2>&1) || rc=$?
    printf '%s' "$out"
    return "$rc"
}

# ==========================================================================
echo
echo "POSITIVE CONTROL — delivers: label removal is blocked in an aeon session"
# ==========================================================================
out=$(run_guard_aeon "bd -C $SPIRA_DB label remove $PROBE_ID delivers:action" || true)
want "PC: blocked by guard"              "BLOCKED by bd-delivers-label-guard" "$out"
want "PC: names the producer"            "Producer:"                          "$out"

# Confirm label count stays at 1 (guard blocked, so nothing ran).
_cnt="$("$SPIRA_BD" -C "$SPIRA_DB" sql \
    "SELECT COUNT(*) FROM labels WHERE issue_id='$PROBE_ID' AND label LIKE 'delivers%'" \
    2>/dev/null | tail -1 | tr -d ' |')"
is "PC: delivers: label remains after blocked remove" "1" "$_cnt"

# ==========================================================================
echo
echo "A — various delivers: types are blocked"
# ==========================================================================
out=$(run_guard_aeon "bd -C $SPIRA_DB label remove $PROBE_ID delivers:note:/some/path" || true)
want "A1: delivers:note blocked"         "BLOCKED by bd-delivers-label-guard" "$out"

out=$(run_guard_aeon "bd -C $SPIRA_DB label remove $PROBE_ID delivers:beads" || true)
want "A2: delivers:beads blocked"        "BLOCKED by bd-delivers-label-guard" "$out"

out=$(run_guard_aeon "bd -C $SPIRA_DB label remove $PROBE_ID delivers:check:cmd" || true)
want "A3: delivers:check blocked"        "BLOCKED by bd-delivers-label-guard" "$out"

# ==========================================================================
echo
echo "B — non-delivers: label removals are NOT blocked"
# ==========================================================================
"$SPIRA_BD" -C "$SPIRA_DB" label add "$PROBE_ID" spira 2>/dev/null || true

out=$(run_guard_aeon "bd -C $SPIRA_DB label remove $PROBE_ID spira" 2>&1) || true
nowant "B1: regular label removal not blocked" "BLOCKED" "$out"

out=$(run_guard_aeon "bd -C $SPIRA_DB label remove $PROBE_ID plan" 2>&1) || true
nowant "B2: plan label removal not blocked"    "BLOCKED" "$out"

# ==========================================================================
echo
echo "C — non-aeon sessions pass through"
# ==========================================================================
out=$(run_guard_no_aeon "bd -C $SPIRA_DB label remove $PROBE_ID delivers:action" 2>&1) || true
nowant "C1: no SPIRA_AEON: delivers: removal not blocked" "BLOCKED" "$out"

# ==========================================================================
echo
echo "D — incident.sh retire-unsatisfiable-delivers is not blocked"
# ==========================================================================
# The command an aeon might call: bash incident.sh retire-unsatisfiable-delivers
# This does not contain bd label remove delivers: as a visible command, so the guard
# must pass it through regardless.
out=$(run_guard_aeon "bash $HERE/incident.sh retire-unsatisfiable-delivers --dry-run" 2>&1) || true
nowant "D1: incident.sh retire-unsatisfiable-delivers not blocked" "BLOCKED" "$out"

# ==========================================================================
echo
echo "E — prose and single-quoted spans do not fire the guard"
# ==========================================================================
out=$(run_guard_aeon "echo 'bd label remove sp-x delivers:action'" 2>&1) || true
nowant "E1: single-quoted prose not blocked"  "BLOCKED" "$out"

out=$(run_guard_aeon "cat <<'EOF'
bd label remove sp-x delivers:action
EOF" 2>&1) || true
nowant "E2: heredoc prose not blocked"        "BLOCKED" "$out"

# ==========================================================================
echo
echo "F — non-Bash tools pass through"
# ==========================================================================
out=$(python3 -c '
import json, sys
print(json.dumps({"tool_name": "Read", "tool_input": {"file_path": "labels.txt"}}))
' | SPIRA_AEON=1 SPIRA_DB="$SPIRA_DB" SPIRA_BD="$SPIRA_BD" \
    BEAD_ID="$PROBE_ID" bash "$GUARD" 2>&1) || true
nowant "F1: non-Bash tool not blocked"        "BLOCKED" "$out"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
