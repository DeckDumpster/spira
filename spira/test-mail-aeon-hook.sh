#!/usr/bin/env bash
#
# test-mail-aeon-hook.sh — aeon mail delivery: the PostToolUse hook and aeon:<id> routing.
# Split out of test-mail-aeon.sh (coverage-map row 11, DEMOTE-TO-T2): neither the hook nor
# `mail.sh send aeon:<id>` touches a bead store, so this half never pays for testdb_up.
#
# Acceptance criteria (each seen red first):
#   (a) message dropped into the aeon's new/ appears in the hook's additionalContext exactly once
#   (e) empty-mailbox hook adds no context; no BEAD_ID adds no context
#   aeon:<id> routing: live mailbox delivers; no mailbox refuses, naming "aeon"
#
# tier: T2
# covers: spira/hooks/aeon-mail-deliver.sh spira/mail.sh UC-operator-channel-11
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber" "$SPIRA_HOME/hooks"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_MAIL="$TMP/mail"
export SPIRA_CONF=""   # prevent reading a real spira.conf

cp "$HERE/mail.sh" "$SPIRA_HOME/"
cp "$HERE/hooks/aeon-mail-deliver.sh" "$SPIRA_HOME/hooks/"
HOOK="$SPIRA_HOME/hooks/aeon-mail-deliver.sh"

run_mail() { SPIRA_HOME="$SPIRA_HOME" bash "$SPIRA_HOME/mail.sh" "$@"; }

# ==========================================================================
# (e) SEEN RED: empty mailbox hook must have been able to find something — plant
#     a message, verify it appears, then check silence after moving to cur.
# (a) + (e): hook output with and without messages.
# ==========================================================================
echo
echo "hook — positive control (SEEN RED then SEEN GREEN)"

BID="test-bead-$$"
mkdir -p "$SPIRA_MAIL/aeon-$BID/new" "$SPIRA_MAIL/aeon-$BID/cur" "$SPIRA_MAIL/aeon-$BID/tmp"

MSGFILE="$SPIRA_MAIL/aeon-$BID/new/123.msg"
printf 'From: Operator <op@spira>\nSubject: Scope change\nDate: Mon, 1 Jan 2024 00:00:00 +0000\nMessage-ID: <123@spira>\n\nNew requirement added.\n' > "$MSGFILE"

hook_out="$(SPIRA_MAIL="$SPIRA_MAIL" BEAD_ID="$BID" bash "$HOOK" </dev/null 2>&1)"; rc=$?
is  "SEEN RED (a): hook exits 0 with a message" 0 "$rc"
want  "SEEN RED (a): output contains additionalContext"  "additionalContext"  "$hook_out"
want  "SEEN RED (a): output contains the message body"   "New requirement"    "$hook_out"
want  "SEEN RED (a): output contains the sender"         "Operator"           "$hook_out"

cur_count="$(ls "$SPIRA_MAIL/aeon-$BID/cur" 2>/dev/null | wc -l | tr -d ' ')"
is "SEEN RED (a): message moved to cur/"  "1"  "$cur_count"
new_count="$(ls "$SPIRA_MAIL/aeon-$BID/new" 2>/dev/null | wc -l | tr -d ' ')"
is "SEEN RED (a): new/ empty after delivery"  "0"  "$new_count"

echo
echo "hook — empty mailbox (SEEN GREEN for criterion e)"

hook_empty="$(SPIRA_MAIL="$SPIRA_MAIL" BEAD_ID="$BID" bash "$HOOK" </dev/null 2>&1)"; rc=$?
is    "SEEN GREEN (e): hook exits 0 on empty mailbox"      0 "$rc"
is    "SEEN GREEN (e): hook produces no output when empty" "" "$hook_empty"

echo
echo "hook — message delivered exactly once (SEEN GREEN for criterion a)"

printf 'From: Ops <ops@spira>\nSubject: Reminder\nDate: Mon, 1 Jan 2024 00:01:00 +0000\nMessage-ID: <456@spira>\n\nDo not forget to rebase.\n' \
    > "$SPIRA_MAIL/aeon-$BID/new/456.msg"
hook_once="$(SPIRA_MAIL="$SPIRA_MAIL" BEAD_ID="$BID" bash "$HOOK" </dev/null 2>&1)"
want    "SEEN GREEN (a): message appears in output"  "Do not forget"  "$hook_once"
hook_second="$(SPIRA_MAIL="$SPIRA_MAIL" BEAD_ID="$BID" bash "$HOOK" </dev/null 2>&1)"
is     "SEEN GREEN (a): message does not appear twice"  ""  "$hook_second"

echo
echo "hook — no BEAD_ID set -> no output"

hook_nobid="$(SPIRA_MAIL="$SPIRA_MAIL" BEAD_ID="" bash "$HOOK" </dev/null 2>&1)"; rc=$?
is  "hook exits 0 when BEAD_ID is empty"   0 "$rc"
is  "hook is silent when BEAD_ID is empty" "" "$hook_nobid"

# ==========================================================================
# mail.sh aeon: address routing
# ==========================================================================
echo
echo "mail.sh aeon: address routing"

ALIVE_BID="alive-bead-$$"
mkdir -p "$SPIRA_MAIL/aeon-$ALIVE_BID/new" "$SPIRA_MAIL/aeon-$ALIVE_BID/cur" "$SPIRA_MAIL/aeon-$ALIVE_BID/tmp"

out="$(echo "Test body." | SPIRA_MAIL="$SPIRA_MAIL" run_mail send "aeon:$ALIVE_BID" \
    --from "Test <t@t>" --subject "Hello" 2>&1)"; rc=$?
is  "SEEN RED: aeon:<id> with live mailbox delivers"  0 "$rc"
cnt="$(ls "$SPIRA_MAIL/aeon-$ALIVE_BID/new" 2>/dev/null | wc -l | tr -d ' ')"
is   "SEEN RED: message lands in aeon mailbox"  "1"  "$cnt"

DEAD_BID="dead-bead-$$"
out2="$(echo "body" | SPIRA_MAIL="$SPIRA_MAIL" run_mail send "aeon:$DEAD_BID" \
    --from "Test <t@t>" --subject "Hello" 2>&1)"; rc2=$?
[ "$rc2" != 0 ] && ok "SEEN GREEN: aeon:<id> with no mailbox exits non-zero" \
                || bad "SEEN GREEN: aeon:<id> with no mailbox exits non-zero" "exit 0"
want  "SEEN GREEN: refusal mentions aeon"  "aeon"  "$out2"

tl_summary
