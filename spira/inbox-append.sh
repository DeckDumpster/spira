#!/usr/bin/env bash
#
# inbox-append.sh "<text>" — record an event for the Concierge WITHOUT touching the pane.
#
# This is the default `concierge` reader in SPIRA_MAIL_READERS (see conf.sh), and every
# watchd watcher able to reach the concierge's inbox writes here too. The concierge's own
# inbox-triage.sh Monitor tails SPIRA_CONCIERGE_INBOX and is the only thing that reads it —
# a keystroke wake (`concierge.sh wake`) is no longer how mail or watcher events reach this
# session, because a wake mid-keystroke can land inside the operator's own half-written
# message and split it.
set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"
mkdir -p "$(dirname "$SPIRA_CONCIERGE_INBOX")" || exit 1
printf '%s %s\n' "$(date -u +%FT%TZ)" "$*" >> "$SPIRA_CONCIERGE_INBOX"
