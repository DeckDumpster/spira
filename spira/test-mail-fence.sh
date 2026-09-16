#!/usr/bin/env bash
#
# test-mail-fence.sh — mail-fence.sh refuses direct writes into SPIRA_MAIL
#   and allows mail.sh send; honours SPIRA_MAIL_WRITE_CONSIDERED.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): the guard MUST
# fire on a direct write before any negative control runs. Silence from the
# negative controls is then evidence the guard works, not that it never could.
#
# covers: spira/mail-fence.sh spira/mail.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1"; esac; }

echo "test-mail-fence.sh"

GUARD="$HERE/mail-fence.sh"
[ -f "$GUARD" ] || { printf 'SKIP mail-fence.sh not found at %s\n' "$GUARD"; exit 77; }

TEST_MAIL="/tmp/spira-test-mail-$$"

# run_guard_bash <cmd> [KEY=VAL ...] — Bash PreToolUse JSON through the guard.
run_guard_bash() {
    local cmd="$1"; shift
    local rc out
    out=$(python3 -c '
import json, sys
print(json.dumps({"tool_name": "Bash", "tool_input": {"command": sys.argv[1]}}))
' "$cmd" | env -i PATH="$PATH" HOME="$HOME" SPIRA_AEON=1 SPIRA_MAIL="$TEST_MAIL" "$@" bash "$GUARD" 2>&1); rc=$?
    printf '%s' "$out"
    return "$rc"
}

# run_guard_write <path> [KEY=VAL ...] — Write PreToolUse JSON through the guard.
run_guard_write() {
    local path="$1"; shift
    local rc out
    out=$(python3 -c '
import json, sys
print(json.dumps({"tool_name": "Write", "tool_input": {"file_path": sys.argv[1], "content": "From: test\n"}}))
' "$path" | env -i PATH="$PATH" HOME="$HOME" SPIRA_AEON=1 SPIRA_MAIL="$TEST_MAIL" "$@" bash "$GUARD" 2>&1); rc=$?
    printf '%s' "$out"
    return "$rc"
}

# run_guard_edit <path> [KEY=VAL ...] — Edit PreToolUse JSON through the guard.
run_guard_edit() {
    local path="$1"; shift
    local rc out
    out=$(python3 -c '
import json, sys
print(json.dumps({"tool_name": "Edit", "tool_input": {"file_path": sys.argv[1], "old_string": "x", "new_string": "y"}}))
' "$path" | env -i PATH="$PATH" HOME="$HOME" SPIRA_AEON=1 SPIRA_MAIL="$TEST_MAIL" "$@" bash "$GUARD" 2>&1); rc=$?
    printf '%s' "$out"
    return "$rc"
}

# run_guard_no_aeon <cmd> — Bash without SPIRA_AEON (interactive session).
run_guard_no_aeon() {
    local cmd="$1"
    local rc out
    out=$(python3 -c '
import json, sys
print(json.dumps({"tool_name": "Bash", "tool_input": {"command": sys.argv[1]}}))
' "$cmd" | env -i PATH="$PATH" HOME="$HOME" SPIRA_MAIL="$TEST_MAIL" bash "$GUARD" 2>&1); rc=$?
    printf '%s' "$out"
    return "$rc"
}

# ==========================================================================
echo
echo "POSITIVE CONTROL — direct writes into SPIRA_MAIL are refused"
# ==========================================================================
# Plant the offender before trusting silence (law-absence-needs-a-positive-control).

out=$(run_guard_bash "printf 'From: test\\n' > \$SPIRA_MAIL/operator/tmp/test.msg" || true)
want "redirect > \$SPIRA_MAIL refused"        "BLOCKED by mail-fence" "$out"
want "names mail.sh send as the correct path" "mail.sh send"          "$out"
want "names the override"                     "SPIRA_MAIL_WRITE_CONSIDERED" "$out"

out=$(run_guard_bash "mv /tmp/draft.msg \$SPIRA_MAIL/operator/new/msg001" || true)
want "mv into \$SPIRA_MAIL refused"            "BLOCKED by mail-fence" "$out"

out=$(run_guard_bash "cp /tmp/draft.msg \$SPIRA_MAIL/operator/new/msg002" || true)
want "cp into \$SPIRA_MAIL refused"            "BLOCKED by mail-fence" "$out"

out=$(run_guard_bash "touch \$SPIRA_MAIL/operator/new/msg003" || true)
want "touch in \$SPIRA_MAIL refused"           "BLOCKED by mail-fence" "$out"

# Using the expanded path (SPIRA_MAIL env value, not the literal \$SPIRA_MAIL).
out=$(run_guard_bash "printf 'From: x\\n' > ${TEST_MAIL}/operator/tmp/x" || true)
want "redirect to expanded SPIRA_MAIL refused" "BLOCKED by mail-fence" "$out"

out=$(run_guard_bash "mv /tmp/x ${TEST_MAIL}/operator/new/y" || true)
want "mv into expanded path refused"           "BLOCKED by mail-fence" "$out"

# Write tool: file_path directly under SPIRA_MAIL.
out=$(run_guard_write "\$SPIRA_MAIL/operator/new/msg001" || true)
want "Write tool to \$SPIRA_MAIL refused"      "BLOCKED by mail-fence" "$out"

out=$(run_guard_write "${TEST_MAIL}/operator/new/msg001" || true)
want "Write tool to expanded path refused"     "BLOCKED by mail-fence" "$out"

# Edit tool: file_path under SPIRA_MAIL.
out=$(run_guard_edit "\$SPIRA_MAIL/operator/cur/msg001" || true)
want "Edit tool to \$SPIRA_MAIL refused"       "BLOCKED by mail-fence" "$out"

# ==========================================================================
echo
echo "mail.sh send is allowed"
# ==========================================================================

out=$(run_guard_bash 'bash spira/mail.sh send operator --from "Gate <gate@spira>" --subject "Done" <<< "body"' 2>&1) || true
nowant "mail.sh send allowed"                  "BLOCKED" "$out"

out=$(run_guard_bash 'SPIRA_MAIL=/tmp/x bash spira/mail.sh send operator --from "x" --subject "y" < /dev/null' 2>&1) || true
nowant "mail.sh with env prefix allowed"       "BLOCKED" "$out"

# A pipeline that routes through mail.sh is allowed.
out=$(run_guard_bash 'cat /tmp/body | bash spira/mail.sh send operator --from "x" --subject "y"' 2>&1) || true
nowant "pipe through mail.sh allowed"          "BLOCKED" "$out"

# ==========================================================================
echo
echo "read operations are not refused"
# ==========================================================================

out=$(run_guard_bash 'cat $SPIRA_MAIL/operator/new/msg001' 2>&1) || true
nowant "cat (read) not refused"                "BLOCKED" "$out"

out=$(run_guard_bash 'grep "Subject:" $SPIRA_MAIL/operator/new/msg001' 2>&1) || true
nowant "grep (read) not refused"               "BLOCKED" "$out"

out=$(run_guard_bash 'ls $SPIRA_MAIL/operator/new/' 2>&1) || true
nowant "ls (read) not refused"                 "BLOCKED" "$out"

out=$(run_guard_bash 'stat $SPIRA_MAIL/operator/new/msg001' 2>&1) || true
nowant "stat (read) not refused"               "BLOCKED" "$out"

# mv OUT of SPIRA_MAIL (reading to another location) — not a write into SPIRA_MAIL.
out=$(run_guard_bash "mv \$SPIRA_MAIL/operator/new/x /tmp/copy" 2>&1) || true
nowant "mv out of \$SPIRA_MAIL not refused"    "BLOCKED" "$out"

# ==========================================================================
echo
echo "override is honoured"
# ==========================================================================

out=$(run_guard_bash "printf 'From: x\\n' > \$SPIRA_MAIL/operator/tmp/x" SPIRA_MAIL_WRITE_CONSIDERED=1 2>&1) || true
nowant "env override allows redirect write"    "BLOCKED" "$out"

out=$(run_guard_bash "SPIRA_MAIL_WRITE_CONSIDERED=1 mv /tmp/x \$SPIRA_MAIL/operator/new/y" 2>&1) || true
nowant "inline override allows mv"             "BLOCKED" "$out"

out=$(run_guard_write "\$SPIRA_MAIL/operator/new/z" SPIRA_MAIL_WRITE_CONSIDERED=1 2>&1) || true
nowant "env override allows Write tool"        "BLOCKED" "$out"

# ==========================================================================
echo
echo "non-aeon sessions are not blocked"
# ==========================================================================

out=$(run_guard_no_aeon "printf 'From: x\\n' > \$SPIRA_MAIL/operator/tmp/y" 2>&1) || true
nowant "no SPIRA_AEON: write not refused"      "BLOCKED" "$out"

# ==========================================================================
echo
echo "single-quoted prose is not refused"
# ==========================================================================

out=$(run_guard_bash "echo 'never write to \$SPIRA_MAIL directly; use mail.sh send'" 2>&1) || true
nowant "single-quoted prose not refused"       "BLOCKED" "$out"

# ==========================================================================
echo
echo "non-Bash, non-Write, non-Edit tools pass through"
# ==========================================================================

out=$(python3 -c '
import json, sys
print(json.dumps({"tool_name": "Read", "tool_input": {"file_path": sys.argv[1]}}))
' "\$SPIRA_MAIL/operator/new/x" | env -i PATH="$PATH" HOME="$HOME" SPIRA_AEON=1 SPIRA_MAIL="$TEST_MAIL" bash "$GUARD" 2>&1) || true
nowant "Read tool not blocked"                 "BLOCKED" "$out"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
