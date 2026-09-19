#!/usr/bin/env bash
# test-mail.sh — mail.sh: Maildir send/list/read/unread-age and lint rules.
#
# Each lint rule is tested with a planted offender first (SEEN RED) then a
# passing message (SEEN GREEN) so the check's silence is evidence
# (law-absence-needs-a-positive-control).
#
# covers: spira/mail.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
isz()    { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0 got $2"; }
isnz()   { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit got 0"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-mail.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

export SPIRA_MAIL="$TMP/mail"
export SPIRA_CONF=""       # prevent reading a real spira.conf
export SPIRA_ID_PREFIX="sp"

run() { bash "$HERE/mail.sh" "$@"; }

# ==========================================================================
# POSITIVE CONTROL — send works at all; proves every check below is real
# ==========================================================================
echo
echo "positive control — send, read, list"

out="$(echo "Positive control body." | run send ctrl --from "Test <t@t>" --subject "Hello world" 2>&1)"
isz "send exits 0" "$?"

new_count="$(ls "$SPIRA_MAIL/ctrl/new" 2>/dev/null | wc -l | tr -d ' ')"
is "message lands in new/" "1" "$new_count"

tmp_count="$(ls "$SPIRA_MAIL/ctrl/tmp" 2>/dev/null | wc -l | tr -d ' ')"
is "tmp/ is empty after delivery (atomic send)" "0" "$tmp_count"

# ==========================================================================
# READ — prints message and moves new -> cur
# ==========================================================================
echo
echo "read: prints and moves new -> cur"

read_out="$(run read ctrl 2>&1)"; rc=$?
isz "read exits 0" "$rc"
want "read output contains From"     "From:"               "$read_out"
want "read output contains Subject"  "Subject:"            "$read_out"
want "read output contains body"     "Positive control"    "$read_out"

new_after="$(ls "$SPIRA_MAIL/ctrl/new" 2>/dev/null | wc -l | tr -d ' ')"
cur_after="$(ls "$SPIRA_MAIL/ctrl/cur" 2>/dev/null | wc -l | tr -d ' ')"
is "new/ is empty after read" "0" "$new_after"
is "cur/ gains the message"   "1" "$cur_after"

run read ctrl >/dev/null 2>&1; rc=$?
isnz "read with no unread mail exits non-zero" "$rc"

# ==========================================================================
# LIST — --unread shows new; default shows new + cur
# ==========================================================================
echo
echo "list"

echo "Second." | run send ctrl --from "A <a@a>" --subject "Second" >/dev/null
echo "Third."  | run send ctrl --from "B <b@b>" --subject "Third"  >/dev/null

list_unread="$(run list ctrl --unread 2>&1)"
want "list --unread shows [new]"   "new"  "$list_unread"
want "list --unread shows Subject" "Subject" "$list_unread"

list_all="$(run list ctrl 2>&1)"
want "list (all) shows [cur] messages" "cur" "$list_all"
want "list (all) shows [new] messages" "new" "$list_all"

# ==========================================================================
# UNREAD-AGE — non-empty with mail, empty with none
# ==========================================================================
echo
echo "unread-age"

age="$(run unread-age ctrl 2>&1)"; rc=$?
isz "unread-age exits 0 with unread mail" "$rc"
case "$age" in
    ''|*[!0-9]*) bad "unread-age prints a number when there is unread mail" "got [$age]" ;;
    *)           ok  "unread-age is a number with unread mail ($age s)" ;;
esac

# read all remaining mail then check age is empty
run read ctrl >/dev/null 2>&1 || true
run read ctrl >/dev/null 2>&1 || true

age_none="$(run unread-age ctrl 2>&1)"; rc=$?
isz  "unread-age exits 0 when no unread mail"      "$rc"
is   "unread-age is empty when no unread mail"  "" "$age_none"

# ==========================================================================
# LINT: missing From  (SEEN RED then SEEN GREEN)
# ==========================================================================
echo
echo "lint: missing From (SEEN RED then SEEN GREEN)"

out="$(echo "body" | run send lint-from --subject "Hello" 2>&1)"; rc=$?
isnz "SEEN RED: missing From is refused"       "$rc"
want "refusal names the rule"   "rule" "$out"
want "refusal mentions From"    "From" "$out"

out="$(echo "body" | run send lint-from --from "Sender <s@s>" --subject "Hello" 2>&1)"; rc=$?
isz  "SEEN GREEN: present From is accepted" "$rc"

# ==========================================================================
# LINT: missing Subject  (SEEN RED then SEEN GREEN)
# ==========================================================================
echo
echo "lint: missing Subject (SEEN RED then SEEN GREEN)"

out="$(echo "body" | run send lint-subj --from "Sender <s@s>" 2>&1)"; rc=$?
isnz "SEEN RED: missing Subject is refused"    "$rc"
want "refusal names the rule"    "rule"    "$out"
want "refusal mentions Subject"  "Subject" "$out"

out="$(echo "body" | run send lint-subj --from "Sender <s@s>" --subject "A topic" 2>&1)"; rc=$?
isz  "SEEN GREEN: present Subject is accepted" "$rc"

# ==========================================================================
# LINT: subject that is or leads with a bead id  (SEEN RED then SEEN GREEN)
# ==========================================================================
echo
echo "lint: subject leads with bead id (SEEN RED then SEEN GREEN)"

out="$(echo "body" | run send lint-sid --from "Gate <g@g>" --subject "sp-q0k3k: landed" 2>&1)"; rc=$?
isnz "SEEN RED: 'sp-XXXXX: ...' subject is refused" "$rc"
want "refusal names the rule" "rule" "$out"

out="$(echo "body" | run send lint-sid --from "Gate <g@g>" --subject "sp-q0k3k" 2>&1)"; rc=$?
isnz "SEEN RED: subject that IS a bead id is refused" "$rc"

out="$(echo "body" | run send lint-sid --from "Gate <g@g>" --subject "Mail delivery landed" 2>&1)"; rc=$?
isz  "SEEN GREEN: human-topic subject is accepted" "$rc"

# ==========================================================================
# LINT: body names bead id without context  (SEEN RED then SEEN GREEN)
# ==========================================================================
echo
echo "lint: body names bead id without context (SEEN RED then SEEN GREEN)"

out="$(echo "Fixed sp-q0k3k." | run send lint-bid --from "Gate <g@g>" --subject "Landed" 2>&1)"; rc=$?
isnz "SEEN RED: sparse bead id context is refused" "$rc"
want "refusal names the rule" "rule" "$out"

out="$(printf 'Implemented Maildir mail delivery with atomic send and lint in sp-q0k3k.' | \
    run send lint-bid --from "Gate <g@g>" --subject "Summary" 2>&1)"; rc=$?
isz  "SEEN GREEN: body with sufficient bead id context is accepted" "$rc"

out="$(printf 'target: sp-q0k3k' | run send lint-bid --from "Gate <g@g>" --subject "Result" 2>&1)"; rc=$?
isz  "SEEN GREEN: key-value metadata line with bead id is accepted" "$rc"

# ==========================================================================
# LINT: decision/question with no default  (SEEN RED then SEEN GREEN)
# ==========================================================================
echo
echo "lint: decision with no default (SEEN RED then SEEN GREEN)"

out="$(echo "body" | run send lint-dec --from "Gate <g@g>" --subject "Enable feature?" --kind decision 2>&1)"; rc=$?
isnz "SEEN RED: decision without default is refused"    "$rc"
want "refusal names the rule"    "rule"    "$out"
want "refusal mentions default"  "default" "$out"

_decision_body="$(printf '## Decision\n\nApprove.\n\n## Default\n\nYes.\n\n## What is blocked\n\nNothing.\n\n## Cost of the wrong choice\n\nLow.\n')"
out="$(printf '%s' "$_decision_body" | run send lint-dec --from "Gate <g@g>" --subject "Enable feature?" --kind decision --default "yes" 2>&1)"; rc=$?
isz  "SEEN GREEN: decision with default is accepted" "$rc"

echo
echo "lint: question with no default (SEEN RED then SEEN GREEN)"

out="$(echo "body" | run send lint-q --from "Gate <g@g>" --subject "When to start?" --kind question 2>&1)"; rc=$?
isnz "SEEN RED: question without default is refused" "$rc"

_question_body="$(printf '## Question\n\nWhen to start?\n\n## Default\n\nNow.\n')"
out="$(printf '%s' "$_question_body" | run send lint-q --from "Gate <g@g>" --subject "When to start?" --kind question --default "now" 2>&1)"; rc=$?
isz  "SEEN GREEN: question with default is accepted" "$rc"

# ==========================================================================
# LINT: "ask below" promise with no section
# ==========================================================================
echo
echo "lint: 'the ask below' promise without a section (SEEN RED then SEEN GREEN)"

out="$(printf 'not retried until a human changes the approach; the ask below carries the failure\n' \
    | run send lint-ask --from "Sentinel <sentinel@spira>" --subject "Some event" 2>&1)"; rc=$?
isnz "SEEN RED: body promises ask below but has no section" "$rc"
want "refusal names the rule"   "rule"     "$out"

out="$(printf 'the ask below carries the failure\n\n## Question\n\nChange the approach?\n' \
    | run send lint-ask --from "Sentinel <sentinel@spira>" --subject "Some event" 2>&1)"; rc=$?
isz  "SEEN GREEN: body promises ask below and has a Question section" "$rc"

# ==========================================================================
# LINT OVERRIDE — SPIRA_MAIL_LINT_CONSIDERED=1 bypasses all checks
# ==========================================================================
echo
echo "lint override: SPIRA_MAIL_LINT_CONSIDERED=1 bypasses all checks"

# Missing From and Subject — both bypassed when the override is set.
out="$(echo "body" | SPIRA_MAIL_LINT_CONSIDERED=1 bash "$HERE/mail.sh" send lint-override 2>&1)"; rc=$?
isz "SPIRA_MAIL_LINT_CONSIDERED=1 bypasses missing From and Subject" "$rc"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
