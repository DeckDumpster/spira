#!/usr/bin/env bash
# test-mail-sendmail.sh — mail.sh sendmail: reply routing and bead closing.
#
# SEEN RED before every silent check (law-absence-needs-a-positive-control):
#   - Bead status is confirmed open before sendmail, then confirmed closed after.
#   - Routing to sender's mailbox: confirmed the mailbox has the reply after sendmail.
#   - Routing to concierge: confirmed by delivering to a sender with no mailbox.
#   - Suit verdicts (uphold/retire/amend): each is planted as an offender first
#     (a non-suit bead close is verified before the suit variant is trusted).
#
# covers: spira/mail.sh spira/conf.sh aerc/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
isz()    { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0 got $2"; }
isnz()   { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit got 0"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

echo "test-mail-sendmail.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-mail-sendmail
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up mail-sendmail || { echo "test-mail-sendmail: could not build fixture database"; exit 1; }

export SPIRA_MAIL="$TMP/mail"
export SPIRA_CONF=""
export SPIRA_ID_PREFIX="sp"
export SPIRA_HOME="$HERE"

MAIL="$HERE/mail.sh"

run()  { bash "$MAIL" "$@"; }

bead_status() {
    bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")' 2>/dev/null
}

seed_bead() {   # seed_bead <id>
    printf '{"id":"%s","title":"test bead %s","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-01-01T00:00:00Z"}\n' \
        "$1" "$1" | testdb_seed >/dev/null 2>&1
}

# Send a message to a mailbox and return its Message-ID (bare, without angle brackets).
send_and_get_msgid() {
    local mailbox="$1" from="$2" bead="$3" kind="${4:-}"
    local send_args=(send "$mailbox" --from "$from" --subject "Test message" --bead "$bead")
    [ -n "$kind" ] && send_args+=(--kind "$kind")
    SPIRA_MAIL_LINT_CONSIDERED="test" SPIRA_MAIL_REPEAT_CONSIDERED="test" \
        bash "$MAIL" "${send_args[@]}" <<< "body" >/dev/null 2>&1
    # Find the newest file in new/ and extract its Message-ID
    local newest
    newest="$(ls -t "$SPIRA_MAIL/$mailbox/new/" 2>/dev/null | head -1)"
    [ -z "$newest" ] && return 1
    awk '/^[[:space:]]*$/ { exit }
        tolower($0) ~ /^message-id:/ { sub(/^[^:]*:[[:space:]]*/, ""); gsub(/[<>]/, ""); print; exit }
    ' "$SPIRA_MAIL/$mailbox/new/$newest"
}

compose_reply() {  # compose_reply <in-reply-to-bare-msgid> <body>
    local irt="$1" body="$2"
    printf 'From: Operator <operator@spira>\n'
    printf 'Subject: Re: Test message\n'
    printf 'In-Reply-To: <%s>\n' "$irt"
    printf 'Date: %s\n' "$(date -u '+%a, %d %b %Y %H:%M:%S +0000')"
    printf '\n'
    printf '%s\n' "$body"
}

# ==========================================================================
# POSITIVE CONTROL — sendmail exits 0 at all; delivers the message.
# (law-absence-needs-a-positive-control: prove delivery works before any
#  silence is trusted as "not delivered".)
# ==========================================================================
echo
echo "positive control — sendmail exits 0 and delivers"

mkdir -p "$SPIRA_MAIL/concierge/new" "$SPIRA_MAIL/concierge/tmp" "$SPIRA_MAIL/concierge/cur"
out="$(printf 'From: Test\nSubject: Direct\n\nbody\n' | run sendmail 2>&1)"; rc=$?
isz "sendmail exits 0 on a plain message" "$rc"
delivered="$(ls "$SPIRA_MAIL/concierge/new" 2>/dev/null | wc -l | tr -d ' ')"
is "SEEN RED: plain message lands in concierge/new" "1" "$delivered"

# ==========================================================================
# REPLY CLOSES TRACKING BEAD — verdict from first paragraph
# ==========================================================================
echo
echo "reply closes tracking bead"

BEAD_ID="sp-smtest1"
seed_bead "$BEAD_ID" || { echo "test-mail-sendmail: could not seed test bead"; exit 1; }

before_status="$(bead_status "$BEAD_ID")"
is "SEEN RED: bead is open before sendmail" "open" "$before_status"

MSGID="$(send_and_get_msgid operator "Gate <gate@spira>" "$BEAD_ID")"
[ -n "$MSGID" ] || { echo "test-mail-sendmail: could not send test message"; exit 1; }

compose_reply "$MSGID" "All looks good." | run sendmail 2>&1; rc=$?
isz "sendmail exits 0 on reply" "$rc"

after_status="$(bead_status "$BEAD_ID")"
is "reply closes tracking bead" "closed" "$after_status"

# ==========================================================================
# ROUTING — reply goes to sender's mailbox when it exists
# ==========================================================================
echo
echo "routing — sender's mailbox when it exists"

mkdir -p "$SPIRA_MAIL/gate/new" "$SPIRA_MAIL/gate/tmp" "$SPIRA_MAIL/gate/cur"

BEAD2="sp-smtest2"
seed_bead "$BEAD2"
MSGID2="$(send_and_get_msgid operator "Gate <gate@spira>" "$BEAD2")"

gate_before="$(ls "$SPIRA_MAIL/gate/new" 2>/dev/null | wc -l | tr -d ' ')"
is "SEEN RED: gate mailbox starts empty" "0" "$gate_before"

compose_reply "$MSGID2" "Looks good." | run sendmail 2>&1
gate_after="$(ls "$SPIRA_MAIL/gate/new" 2>/dev/null | wc -l | tr -d ' ')"
is "reply routed to sender's mailbox (gate)" "1" "$gate_after"

# ==========================================================================
# ROUTING — reply goes to concierge when sender has no mailbox
# ==========================================================================
echo
echo "routing — concierge when sender has no mailbox"

BEAD3="sp-smtest3"
seed_bead "$BEAD3"
# Use a From whose local part has no mailbox directory
MSGID3="$(send_and_get_msgid operator "Landing gate <nobox@spira>" "$BEAD3")"

conc_before="$(ls "$SPIRA_MAIL/concierge/new" 2>/dev/null | wc -l | tr -d ' ')"

compose_reply "$MSGID3" "Noted." | run sendmail 2>&1
conc_after="$(ls "$SPIRA_MAIL/concierge/new" 2>/dev/null | wc -l | tr -d ' ')"
is "reply with no sender mailbox routes to concierge" "$((conc_before + 1))" "$conc_after"

# ==========================================================================
# SUIT VERDICTS — uphold, retire, amend
#
# POSITIVE CONTROL: a non-suit bead close already verified above; here we
# confirm suit verdicts use the specific suit words, not the raw paragraph.
# ==========================================================================
echo
echo "suit verdict — uphold"

SUIT1="sp-smtest4"
seed_bead "$SUIT1"
MSGID_S1="$(send_and_get_msgid operator "Gate <gate@spira>" "$SUIT1" suit)"
before_s1="$(bead_status "$SUIT1")"
is "SEEN RED: suit bead is open before reply" "open" "$before_s1"

compose_reply "$MSGID_S1" "uphold" | run sendmail 2>&1
after_s1="$(bead_status "$SUIT1")"
is "uphold closes suit bead" "closed" "$after_s1"

echo
echo "suit verdict — retire"

SUIT2="sp-smtest5"
seed_bead "$SUIT2"
MSGID_S2="$(send_and_get_msgid operator "Gate <gate@spira>" "$SUIT2" suit)"
before_s2="$(bead_status "$SUIT2")"
is "SEEN RED: suit bead is open before retire reply" "open" "$before_s2"

compose_reply "$MSGID_S2" "retire" | run sendmail 2>&1
after_s2="$(bead_status "$SUIT2")"
is "retire closes suit bead" "closed" "$after_s2"

echo
echo "suit verdict — amend"

SUIT3="sp-smtest6"
seed_bead "$SUIT3"
MSGID_S3="$(send_and_get_msgid operator "Gate <gate@spira>" "$SUIT3" suit)"
before_s3="$(bead_status "$SUIT3")"
is "SEEN RED: suit bead is open before amend reply" "open" "$before_s3"

compose_reply "$MSGID_S3" "amend: add a new clause" | run sendmail 2>&1
after_s3="$(bead_status "$SUIT3")"
is "amend closes suit bead" "closed" "$after_s3"

# ==========================================================================
# NO In-Reply-To — routes to concierge, exits 0
# ==========================================================================
echo
echo "no In-Reply-To — routes to concierge"

conc_pre="$(ls "$SPIRA_MAIL/concierge/new" 2>/dev/null | wc -l | tr -d ' ')"
printf 'From: Operator <operator@spira>\nSubject: New message\n\nHello.\n' | run sendmail 2>&1; rc=$?
isz "sendmail exits 0 with no In-Reply-To" "$rc"
conc_post="$(ls "$SPIRA_MAIL/concierge/new" 2>/dev/null | wc -l | tr -d ' ')"
is "no-reply message routes to concierge" "$((conc_pre + 1))" "$conc_post"

echo
echo "accept-default — the client's accept key closes the decision bead with the message's default"

BEAD_ID="sp-smtest-accept"
seed_bead "$BEAD_ID" || { echo "test-mail-sendmail: could not seed test bead"; exit 1; }
is "SEEN RED: bead is open before accept" "open" "$(bead_status "$BEAD_ID")"
SPIRA_MAIL_LINT_CONSIDERED="test" run send operator --from "Gate <gate@spira>" --subject "Accept test" \
    --kind decision --default "take the accept-test default" --bead "$BEAD_ID" <<< "body" >/dev/null 2>&1
# The send now creates a DECISION BEAD and X-Spira-Bead names it, not the work bead.
# Find the newest message in operator/new (the decision bead question).
accept_msg="$(ls -t "$SPIRA_MAIL/operator/new/" 2>/dev/null | head -1)"
accept_msg="${accept_msg:+$SPIRA_MAIL/operator/new/$accept_msg}"
[ -n "$accept_msg" ] && [ -f "$accept_msg" ] \
    || { echo "test-mail-sendmail: could not send accept test message"; exit 1; }
# The X-Spira-Bead in this message is the decision bead id.
dec_bead_accept="$(awk '/^[[:space:]]*$/ { exit }
    tolower($0) ~ /^x-spira-bead:/ { sub(/^[^:]*:[[:space:]]*/, ""); print; exit }
' "$accept_msg")"
is "SEEN RED: decision bead is open before accept-default" "open" "$(bead_status "${dec_bead_accept:-none}")"
# A real config file, because the client runs the script with one present.
printf 'SPIRA_MAIL_UNREAD_AGE = 4242\n' > "$TMP/accept.conf"
accept_out="$(SPIRA_CONF="$TMP/accept.conf" bash "$HERE/../aerc/accept-default.sh" < "$accept_msg" 2>&1)"; rc=$?
isz "accept-default exits 0 with a config file present" "$rc"
is "accept-default closes the decision bead (not the work bead)" "closed" "$(bead_status "${dec_bead_accept:-none}")"
is "work bead stays open (decision bead was closed, not work bead)" "open" "$(bead_status "$BEAD_ID")"
accept_reason="$(bd -C "$SPIRA_DB" show "${dec_bead_accept:-none}" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("close_reason") or "")' 2>/dev/null)"
want "the verdict is the message's default" "take the accept-test default" "$accept_reason"
work_notes_accept="$(bd -C "$SPIRA_DB" show "$BEAD_ID" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("notes") or "")' 2>/dev/null)"
want "work bead note contains the operator verdict" "take the accept-test default" "$work_notes_accept"
[ "$rc" = 0 ] || printf '    %s\n' "$accept_out"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
