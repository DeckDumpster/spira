#!/usr/bin/env bash
# spira-mail-deliver.sh — watch concierge/new and wake the session on arrival.
#
# Watches $SPIRA_MAIL/concierge/new with inotifywait. On any arrival it
# sleeps SPIRA_MAIL_SETTLE seconds to let a burst finish, then calls
# SPIRA_WAKE once with the standard prompt. One wake per burst; the settle
# period absorbs concurrent deliveries so the session is not typed at
# repeatedly for a single batch.
#
# A failed wake (session not running) logs one line to stderr and leaves
# the mail unread — the delivery daemon never fails the loop.
#
# NOTE: SPIRA_WAKE types text into the concierge's tmux pane. If the
# operator is locally attached and mid-keystroke at the moment the wake
# fires, the injected text can mix into their half-written line. This is
# a property of the shared wake path, not specific to this daemon.
# Typing via the phone is not affected.

set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"

MAILDIR="${SPIRA_MAIL}/concierge/new"
mkdir -p "$MAILDIR"

if ! command -v inotifywait >/dev/null 2>&1; then
    printf '%s spira-mail-deliver: inotifywait not found — install inotify-tools\n' \
        "$(date -u +%FT%TZ)" >&2
    exit 1
fi

# -m: monitor mode streams events without exiting; -q: suppress the banner;
# -e close_write: file written and closed (tmp->new rename uses MOVED_TO);
# -e moved_to: covers mv/rename into the directory (the Maildir delivery path).
inotifywait -m -q -e close_write -e moved_to "$MAILDIR" 2>/dev/null | \
while IFS= read -r _event; do
    sleep "${SPIRA_MAIL_SETTLE:-2}"
    # Drain events that piled up during the settle period. Use a short timeout so each
    # read either consumes a queued event or exits the loop promptly when the pipe is
    # empty. read -t 0 checks availability without consuming, so it cannot drain.
    while IFS= read -r -t 0.1 _burst 2>/dev/null; do :; done
    if [ -z "${SPIRA_WAKE:-}" ]; then
        printf '%s spira-mail-deliver: SPIRA_WAKE is unset — cannot notify concierge\n' \
            "$(date -u +%FT%TZ)" >&2
        continue
    fi
    if ! $SPIRA_WAKE "You have mail — run mail.sh read concierge." 2>/dev/null; then
        printf '%s spira-mail-deliver: wake failed (session not running?)\n' \
            "$(date -u +%FT%TZ)" >&2
    fi
done
