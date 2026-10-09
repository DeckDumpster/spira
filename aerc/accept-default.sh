#!/usr/bin/env bash
# accept-default.sh — compose and send an accept-default reply for a Spira message.
# Receives the full RFC 5322 message on stdin (from aerc :pipe).
# Extracts X-Spira-Default and sends that value as the reply via mail sendmail.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/../spira/conf.sh"

msg="$(cat)"

_hdr() {
    local name="$1"
    printf '%s\n' "$msg" | awk -v h="$name" '
        { sub(/\r$/, "") }
        $0 == "" { exit }
        found && /^[ \t]/ { val = val $0; next }
        found { exit }
        tolower($0) ~ "^" tolower(h) ":" { found = 1; val = $0; sub(/^[^:]*:[ \t]*/, "", val) }
        END { if (found) print val }
    '
}

msgid="$(_hdr message-id)"
default="$(_hdr x-spira-default)"
subject="$(_hdr subject)"
from_orig="$(_hdr from)"

[ -z "$msgid" ]   && { printf 'accept-default: message has no Message-ID\n' >&2; exit 1; }
[ -z "$default" ] && { printf 'accept-default: message has no X-Spira-Default — use a regular reply\n' >&2; exit 1; }

{
    printf 'From: Operator <operator@spira>\n'
    printf 'To: %s\n' "$from_orig"
    printf 'Subject: Re: %s\n' "$subject"
    printf 'In-Reply-To: %s\n' "$msgid"
    printf 'Date: %s\n' "$(date -u '+%a, %d %b %Y %H:%M:%S +0000')"
    printf '\n'
    printf '%s\n' "$default"
} | mail sendmail
