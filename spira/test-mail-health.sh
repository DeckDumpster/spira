#!/usr/bin/env bash
# test-mail-health.sh — mail-health.sh: unread-age check over registered mailboxes.
#
# SEEN RED before every silent check (law-absence-needs-a-positive-control):
#   - fires when threshold exceeded: stub watcher; no mail to operator first
#   - fires once not per pass: first run mails, second does not
#   - re-fires after clear: backlog gone clears state; new backlog fires again
#   - silent below threshold: fresh message never triggers
#
# covers: spira/mail-health.sh spira/mail.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
isz()    { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0 got $2"; }
is1()    { [ "$2" = 1 ] && ok "$1" || bad "$1" "wanted exit 1 got $2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

echo "test-mail-health.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

export SPIRA_MAIL="$TMP/mail"
export SPIRA_RUN="$TMP/run"
export SPIRA_HOME="$HERE"
export SPIRA_MAIL_KINDS="$HERE/mail/kinds"
export SPIRA_CONF=""
export SPIRA_ID_PREFIX="sp"
export SPIRA_MAIL_UNREAD_AGE=60

HEALTH="$HERE/mail-health.sh"
MAIL="$HERE/mail.sh"

# Deliver a message to a mailbox and backdate its mtime to simulate an old backlog.
send_old() {
    local mailbox="$1" age_s="$2"
    echo "body" | SPIRA_MAIL_LINT_CONSIDERED="test" \
        bash "$MAIL" send "$mailbox" --from "S <s@s>" --subject "Old message" 2>/dev/null
    local f; f="$(ls -t "$SPIRA_MAIL/$mailbox/new/" 2>/dev/null | head -1)"
    [ -n "$f" ] || { printf 'send_old: no file in new/\n' >&2; return 1; }
    touch -d "@$(( $(date +%s) - age_s ))" "$SPIRA_MAIL/$mailbox/new/$f"
}

op_count() {
    bash "$MAIL" count operator 2>/dev/null
}

export SPIRA_MAIL_READERS="concierge=echo wake"

echo
echo "=== POSITIVE CONTROL — fires when threshold exceeded ==="

# Confirm operator mailbox has no mail yet: if the check never fires, below
# assertions would still pass — so this confirms the starting state is empty.
initial="$(op_count)"
is "SEEN RED: operator starts with 0 messages" "0" "$initial"

send_old concierge 120

rc=0; bash "$HEALTH" 2>/dev/null; rc=$?
is1 "exits 1 when backlog exceeds threshold" "$rc"

count1="$(op_count)"
is "operator receives exactly one health message" "1" "$count1"

msg="$(bash "$MAIL" list operator --unread 2>/dev/null)"
want "message names the mailbox" "concierge" "$msg"

echo
echo "=== fires once, not per pass ==="

rc=0; bash "$HEALTH" 2>/dev/null; rc=$?
isz "exits 0 on second pass with same backlog" "$rc"

count2="$(op_count)"
is "no second message for same backlog" "1" "$count2"

echo
echo "=== re-fires after backlog clears and recurs ==="

# Read concierge mail — clears the backlog.
bash "$MAIL" read concierge >/dev/null 2>&1 || true

# Health should clear state (no backlog).
rc=0; bash "$HEALTH" 2>/dev/null; rc=$?
isz "exits 0 when mailbox is empty" "$rc"

# State file must be gone.
sf_gone=1; [ -f "$TMP/run/mail-health/concierge" ] && sf_gone=0
is "state file removed when mailbox empties" "1" "$sf_gone"

# New old backlog arrives.
send_old concierge 120

rc=0; bash "$HEALTH" 2>/dev/null; rc=$?
is1 "re-fires when backlog recurs after clearing" "$rc"

count3="$(op_count)"
is "second notification sent after recurrence" "2" "$count3"

echo
echo "=== silent below threshold ==="

# Send fresh mail to a different mailbox — not old enough.
export SPIRA_MAIL_READERS="freshbox=echo wake"
echo "fresh" | SPIRA_MAIL_LINT_CONSIDERED="test" \
    bash "$MAIL" send freshbox --from "T <t@t>" --subject "Fresh message" 2>/dev/null

# SEEN RED: confirm freshbox has mail so that silence below is meaningful.
fresh_count="$(bash "$MAIL" count freshbox 2>/dev/null)"
is "SEEN RED: freshbox has 1 message" "1" "$fresh_count"

rc=0; bash "$HEALTH" 2>/dev/null; rc=$?
isz "exits 0 when age below threshold" "$rc"

count4="$(op_count)"
is "no message for fresh mail" "2" "$count4"

echo
if [ "$fail" -gt 0 ]; then
    printf '\n%d passed, %d FAILED\n' "$pass" "$fail"
    exit 1
fi
printf '\n%d passed\n' "$pass"
