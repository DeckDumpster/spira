#!/usr/bin/env bash
# test-mail-deliver.sh — delivery daemon: one wake per burst, none when nothing arrives.
#
# Tests the daemon against a stub SPIRA_WAKE so no real tmux session is needed.
# inotifywait is exercised live — this tests the real dependency, not a model of it
# (law-prefer-the-real-dependency).
#
# SEEN RED (positive control before silence can be trusted):
#   - Stub wake is called at all: a stub that is never called cannot prove silence.
#   - A burst of N deliveries produces exactly 1 wake, not N.
#   - No event produces no wake.
#
# # requires: inotifywait
# covers: spira/spira-mail-deliver.sh spira/conf.sh spira/mail.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
isz()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0, got $2"; }
isnz() { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero, got 0"; }

echo "test-mail-deliver.sh"

# Minimal environment: no real spira.conf, no real concierge.
TMP="$(mktemp -d)"; trap 'kill_daemon; rm -rf "$TMP"' EXIT INT TERM

MAILDIR="$TMP/mail"
WAKELOG="$TMP/wake.log"

# Stub wake: writes the text it received to WAKELOG, one line per call.
WAKE_STUB="$TMP/wake.sh"
printf '#!/bin/sh\nprintf "%%s\\n" "$1" >> "%s"\n' "$WAKELOG" > "$WAKE_STUB"
chmod +x "$WAKE_STUB"

export SPIRA_MAIL="$MAILDIR"
export SPIRA_MAIL_SETTLE="0.3"      # short but real settle: all burst events arrive within 0.3s
export SPIRA_WAKE="$WAKE_STUB"
export SPIRA_CONF=""
export SPIRA_ID_PREFIX="sp"

DAEMON_PID=""
kill_daemon() {
    [ -n "${DAEMON_PID:-}" ] || return 0
    # Kill the whole process group (daemon bash + inotifywait child).
    kill -- -"$DAEMON_PID" 2>/dev/null || kill "$DAEMON_PID" 2>/dev/null || true
    DAEMON_PID=""
}

start_daemon() {
    kill_daemon
    rm -f "$WAKELOG"
    # Ensure the watch directory exists before the daemon starts.
    mkdir -p "$MAILDIR/concierge/new"
    # setsid: new process group so kill_daemon can reach inotifywait too.
    setsid bash "$HERE/spira-mail-deliver.sh" &
    DAEMON_PID=$!
    # Give inotifywait time to attach before we deliver mail.
    sleep 0.3
}

_deliver_seq=0
deliver() {
    # Atomic Maildir delivery: write to tmp/ then rename into new/.
    # Uses a counter (no subshell fork) so rapid bursts finish in microseconds.
    _deliver_seq=$((_deliver_seq+1))
    local id; id="${_deliver_seq}.${RANDOM}.$$"
    mkdir -p "$MAILDIR/concierge/tmp" "$MAILDIR/concierge/new"
    printf 'From: Sender <s@example.com>\nSubject: test\n\nbody\n' \
        > "$MAILDIR/concierge/tmp/$id"
    mv "$MAILDIR/concierge/tmp/$id" "$MAILDIR/concierge/new/$id"
}

wake_count() {
    [ -f "$WAKELOG" ] || { printf '0'; return; }
    wc -l < "$WAKELOG" | tr -d ' '
}

# ==========================================================================
# POSITIVE CONTROL — the stub wake is actually called when mail arrives.
# Without this, every "count is N" assertion below proves nothing.
# (law-absence-needs-a-positive-control)
# ==========================================================================
echo
echo "positive control — wake is called on mail arrival"

start_daemon
deliver
sleep 1
count="$(wake_count)"
isnz "SEEN RED: stub wake was called at least once after delivery" "$((count == 0 ? 0 : 1))"
is   "wake count is 1 after one delivery" "1" "$count"

# ==========================================================================
# ONE WAKE PER BURST — a burst of N deliveries produces exactly one wake.
# SPIRA_MAIL_SETTLE=0 means no artificial pause; inotifywait fires for the
# first event, the daemon sleeps 0s, drains the burst from the pipe, then
# wakes once.  We deliver 3 messages in rapid succession.
# ==========================================================================
echo
echo "one wake per burst"

start_daemon
deliver; deliver; deliver
# Allow settle (0.3s) + drain (0.1s) + wake propagation.
sleep 1
count="$(wake_count)"
is "three rapid deliveries produce exactly one wake" "1" "$count"

# ==========================================================================
# NO EVENT, NO WAKE — the daemon runs but never wakes when nothing arrives.
# ==========================================================================
echo
echo "no event, no wake"

start_daemon
sleep 0.5
count="$(wake_count)"
is "no delivery produces no wake" "0" "$count"

# ==========================================================================
# WAKE TEXT — the wake carries the standard prompt.
# ==========================================================================
echo
echo "wake text"

start_daemon
deliver
sleep 1
text="$(cat "$WAKELOG" 2>/dev/null || true)"
case "$text" in
    *"You have mail"*) ok "wake text contains 'You have mail'" ;;
    *) bad "wake text" "expected 'You have mail' in [$text]" ;;
esac
case "$text" in
    *"mail.sh read concierge"*) ok "wake text names the read command" ;;
    *) bad "wake text names command" "expected 'mail.sh read concierge' in [$text]" ;;
esac

# ==========================================================================
# EMPTY WAKE — daemon logs and continues when SPIRA_WAKE is unset.
# ==========================================================================
echo
echo "empty SPIRA_WAKE — daemon logs and continues"

kill_daemon
rm -f "$WAKELOG"
mkdir -p "$MAILDIR/concierge/new"
SPIRA_WAKE="" setsid bash "$HERE/spira-mail-deliver.sh" &
DAEMON_PID=$!
sleep 0.3
deliver
sleep 1
# daemon is still running (did not exit)
if kill -0 "$DAEMON_PID" 2>/dev/null; then
    ok "daemon continues after SPIRA_WAKE empty"
else
    bad "daemon continues" "daemon exited after empty SPIRA_WAKE"
fi
kill -- -"$DAEMON_PID" 2>/dev/null || kill "$DAEMON_PID" 2>/dev/null || true
DAEMON_PID=""

kill_daemon

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
