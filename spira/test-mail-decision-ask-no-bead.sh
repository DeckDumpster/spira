#!/usr/bin/env bash
# test-mail-decision-ask-no-bead.sh — mail.sh send --kind question|decision WITHOUT --bead
#   files a decision bead even when no --bead is provided.
#   Previously (sp-5l3zv), questions/decisions without --bead had no tracking bead.
#
# Acceptance criteria:
#   (a) a question without --bead still creates a tracking decision bead
#   (b) the message carries X-Spira-Bead header with the decision bead id
#   (c) decision bead is open and awaiting reply
#   (d) reply closes the decision bead
#
# SEEN RED (law-absence-needs-a-positive-control):
#   (a) decision bead id confirmed present in message header
#   (b) bead confirmed open before reply
#   (c) bead confirmed closed after reply

# covers: spira/mail.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

echo "test-mail-decision-ask-no-bead.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-mail-decision-ask-no-bead
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up mail-decision-ask-no-bead || { echo "test-mail-decision-ask-no-bead: could not build fixture database"; exit 1; }

export SPIRA_MAIL="$TMP/mail"
export SPIRA_CONF=""
export SPIRA_ID_PREFIX="sp"
export SPIRA_HOME="$TMP/home"
mkdir -p "$SPIRA_HOME/chamber"

MAIL="$HERE/mail.sh"
run() { bash "$MAIL" "$@"; }

bead_status() {
    bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")' 2>/dev/null
}

# ==========================================================================
# (a) QUESTION WITHOUT --bead CREATES A DECISION BEAD
# ==========================================================================
echo
echo "(a) question without --bead creates a decision bead"

# Send a question without --bead (no work bead cited)
SPIRA_MAIL_LINT_CONSIDERED="test" run send operator \
    --from "Builder <builder@spira>" \
    --subject "Should we refactor the parser?" \
    --kind question \
    --default "yes, proceed with refactor" <<'BODY' >/dev/null 2>&1
## Question

Should we refactor the parser to improve maintainability?

## Default

yes, proceed with refactor
BODY

# Check that a message was created
newest_msg="$(ls -t "$SPIRA_MAIL/operator/new/" 2>/dev/null | head -1)"
[ -z "$newest_msg" ] && { echo "test-mail-decision-ask-no-bead: no message created"; exit 1; }

# Extract the decision bead ID from X-Spira-Bead header
x_bead="$(awk '/^[[:space:]]*$/ { exit }
    tolower($0) ~ /^x-spira-bead:/ { sub(/^[^:]*:[[:space:]]*/, ""); print; exit }
' "$SPIRA_MAIL/operator/new/$newest_msg")"

is "X-Spira-Bead header is present (decision bead created)" "0" "$([ -n "$x_bead" ] && echo 0 || echo 1)"

# ==========================================================================
# (b) DECISION BEAD SHOULD BE OPEN
# ==========================================================================
echo
echo "(b) decision bead is open and waiting for reply"

dec_status="$(bead_status "$x_bead")"
is "decision bead status is open" "open" "$dec_status"

# ==========================================================================
# (c) EXTRACT MESSAGE ID AND COMPOSE REPLY
# ==========================================================================
echo
echo "(c) reply closes the decision bead"

msgid="$(awk '/^[[:space:]]*$/ { exit }
    tolower($0) ~ /^message-id:/ { sub(/^[^:]*:[[:space:]]*/, ""); gsub(/[<>]/, ""); print; exit }
' "$SPIRA_MAIL/operator/new/$newest_msg")"

compose_reply() {
    local irt="$1"
    printf 'From: Operator <operator@spira>\n'
    printf 'Subject: Re: Should we refactor the parser?\n'
    printf 'In-Reply-To: <%s>\n' "$irt"
    printf 'Date: %s\n' "$(date -u '+%a, %d %b %Y %H:%M:%S +0000')"
    printf '\n'
    printf 'Yes, refactor the parser.\n'
}

compose_reply "$msgid" | run sendmail 2>&1

dec_status_after="$(bead_status "$x_bead")"
is "decision bead is closed after reply" "closed" "$dec_status_after"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
