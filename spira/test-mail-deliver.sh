#!/usr/bin/env bash
# test-mail-deliver.sh — delivery daemon: per-mailbox wakes with count, burst coalescing.
#
# Tests the daemon against stub wake commands so no real tmux session is needed.
# inotifywait is exercised live — this tests the real dependency, not a model of it
# (law-prefer-the-real-dependency).
#
# SEEN RED (positive control before silence can be trusted):
#   - Stub wake is called at all: a stub that is never called cannot prove silence.
#   - A burst of N deliveries produces exactly 1 wake, not N.
#   - Unregistered mailbox: a wake stub that is called would expose the absence of filtering.
#
# # requires: inotifywait
# covers: spira/spira-mail-deliver.sh spira/conf.sh spira/mail.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
isnz() { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero, got 0"; }

echo "test-mail-deliver.sh"

TMP="$(mktemp -d)"; trap 'kill_daemon; rm -rf "$TMP"' EXIT INT TERM

MAILDIR="$TMP/mail"
WAKELOG="$TMP/wake.log"

# Stub wake: writes the text it received to WAKELOG, exits 0.
WAKE_STUB="$TMP/wake.sh"
printf '#!/bin/sh\nprintf "%%s\\n" "$1" >> "%s"\n' "$WAKELOG" > "$WAKE_STUB"
chmod +x "$WAKE_STUB"

# Failing wake stub: exits 1 so the daemon logs and continues.
FAIL_STUB="$TMP/fail-wake.sh"
printf '#!/bin/sh\nexit 1\n' > "$FAIL_STUB"
chmod +x "$FAIL_STUB"

export SPIRA_MAIL="$MAILDIR"
export SPIRA_MAIL_SETTLE="0.3"
export SPIRA_CONF=""
export SPIRA_ID_PREFIX="sp"

DAEMON_PID=""
kill_daemon() {
    [ -n "${DAEMON_PID:-}" ] || return 0
    kill -- -"$DAEMON_PID" 2>/dev/null || kill "$DAEMON_PID" 2>/dev/null || true
    DAEMON_PID=""
}

start_daemon() {
    kill_daemon
    rm -f "$WAKELOG"
    # Reset mailboxes so each section starts with empty new/.
    rm -rf "$MAILDIR"
    mkdir -p "$MAILDIR/concierge/new"
    # setsid: new process group so kill_daemon can reach inotifywait too.
    SPIRA_MAIL_READERS="concierge=$WAKE_STUB" \
        setsid bash "$HERE/spira-mail-deliver.sh" &
    DAEMON_PID=$!
    sleep 0.3
}

_deliver_seq=0
deliver() {
    local mailbox="${1:-concierge}"
    _deliver_seq=$((_deliver_seq+1))
    local id; id="${_deliver_seq}.${RANDOM}.$$"
    mkdir -p "$MAILDIR/$mailbox/tmp" "$MAILDIR/$mailbox/new"
    printf 'From: Sender <s@example.com>\nSubject: test\n\nbody\n' \
        > "$MAILDIR/$mailbox/tmp/$id"
    mv "$MAILDIR/$mailbox/tmp/$id" "$MAILDIR/$mailbox/new/$id"
}

wake_count() {
    [ -f "$WAKELOG" ] || { printf '0'; return; }
    wc -l < "$WAKELOG" | tr -d ' '
}

wake_text() {
    cat "$WAKELOG" 2>/dev/null || true
}

# ==========================================================================
# POSITIVE CONTROL — stub wake is actually called when mail arrives.
# Without this, every "count is N" assertion below proves nothing.
# (law-absence-needs-a-positive-control)
# ==========================================================================
echo
echo "positive control — wake is called on mail arrival"

start_daemon
deliver
sleep 1
count="$(wake_count)"
isnz "SEEN RED: stub wake was called at least once" "$((count == 0 ? 0 : 1))"
is   "wake count is 1 after one delivery" "1" "$count"

# ==========================================================================
# WAKE TEXT — count and command in the wake message.
# start_daemon resets the mailbox so exactly 1 message lands.
# ==========================================================================
echo
echo "wake text — count and list command"

start_daemon
deliver
sleep 1
text="$(wake_text)"
case "$text" in
    *"You have 1 unread messages"*) ok "wake text contains 'You have 1 unread messages'" ;;
    *) bad "wake text count" "expected 'You have 1 unread messages' in [$text]" ;;
esac
case "$text" in
    *"mail.sh list concierge --unread"*) ok "wake text names 'mail.sh list concierge --unread'" ;;
    *) bad "wake text command" "expected 'mail.sh list concierge --unread' in [$text]" ;;
esac

# ==========================================================================
# ONE WAKE PER BURST — three rapid deliveries produce exactly one wake.
# ==========================================================================
echo
echo "one wake per burst"

start_daemon
deliver; deliver; deliver
sleep 1
count="$(wake_count)"
is "three rapid deliveries produce exactly one wake" "1" "$count"

# ==========================================================================
# NO EVENT, NO WAKE — daemon runs but never wakes when nothing arrives.
# ==========================================================================
echo
echo "no event, no wake"

start_daemon
sleep 0.5
count="$(wake_count)"
is "no delivery produces no wake" "0" "$count"

# ==========================================================================
# UNREGISTERED MAILBOX — deliver to a mailbox not in SPIRA_MAIL_READERS;
# the wake must not be called.
# SEEN RED: we first confirm the stub IS called for the registered mailbox.
# ==========================================================================
echo
echo "unregistered mailbox — no wake"

start_daemon
# Positive control: registered mailbox wakes first.
deliver concierge
sleep 1
registered_count="$(wake_count)"
isnz "SEEN RED: stub called for registered mailbox" "$((registered_count == 0 ? 0 : 1))"

# Now deliver to an unregistered mailbox; count must not increase.
rm -f "$WAKELOG"
deliver operator
sleep 1
count="$(wake_count)"
is "unregistered mailbox produces no wake" "0" "$count"

# ==========================================================================
# FAILED WAKE — wake exits non-zero; mail stays unread, daemon continues.
# start_daemon resets the mailbox so we can count precisely.
# ==========================================================================
echo
echo "failed wake — mail stays unread, daemon continues"

kill_daemon
rm -f "$WAKELOG"
rm -rf "$MAILDIR"
mkdir -p "$MAILDIR/concierge/new"
SPIRA_MAIL_READERS="concierge=$FAIL_STUB" \
    setsid bash "$HERE/spira-mail-deliver.sh" &
DAEMON_PID=$!
sleep 0.3
deliver
sleep 1
unread="$(ls "$MAILDIR/concierge/new" 2>/dev/null | wc -l | tr -d ' ')"
is "mail stays in new/ after failed wake" "1" "$unread"
if kill -0 "$DAEMON_PID" 2>/dev/null; then
    ok "daemon continues after failed wake"
else
    bad "daemon continues" "daemon exited after failed wake"
fi

kill_daemon

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
