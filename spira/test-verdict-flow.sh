#!/usr/bin/env bash
# test-verdict-flow.sh — mail.sh sendmail: bead closing and decision-bead verdict flow-back.
#
# ONE testdb where four suites each built their own (docs/test-plan/operator-channel.md
# rows 14-16, 18-19, 24; clusters D1/D2): test-mail-decision-ask.sh,
# test-mail-decision-ask-no-bead.sh, test-mail-sendmail.sh's non-routing cases,
# test-sentinel.sh's land_escalate cases, and test-archivist-cited-bead.sh's surviving
# assert (the blocking-edge guard logs its refusal — archivist.sh itself was never run by
# that suite, only mail.sh's own guard, which every question-with-bead send already exercises
# here). Reply ROUTING (no bead cited, no db needed) moved to test-mail.sh (row 17) — it does
# not belong on a testdb it never reads.
#
# SEEN RED before every silent check (law-absence-needs-a-positive-control):
#   - a bead's open/closed status and its open blocking-dep count are read BEFORE the action
#     under test, so a check that always reports the same status is caught.
#   - suit verdicts are asserted by close_reason, not just "closed" — a suit bead closed with
#     the raw paragraph would still read "closed" and pass the weaker assertion.
#
# tier: T3
# covers: spira/mail.sh spira/lib.sh spira/conf.sh aerc/accept-default.sh UC-operator-channel-14 UC-operator-channel-15 UC-operator-channel-16 UC-operator-channel-18 UC-operator-channel-19 UC-operator-channel-24
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
isz()   { [ "$2" = 0 ]  && ok "$1" || bad "$1" "wanted exit 0 got $2"; }
isnz()  { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit got 0"; }
lacks() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-verdict-flow.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-verdict-flow
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up verdict-flow || { echo "test-verdict-flow: could not build fixture database"; exit 1; }

export SPIRA_MAIL="$TMP/mail"
export SPIRA_CONF=""
export SPIRA_ID_PREFIX="sp"
export SPIRA_HOME="$TMP/home"
export SPIRA_RUN="$TMP/run"
mkdir -p "$SPIRA_HOME/chamber" "$SPIRA_RUN"

MAIL="$HERE/mail.sh"
run() { bash "$MAIL" "$@"; }

# Sourced (not run): mail.sh's main guard makes this safe, and it is how the T1 _suit_reason
# table below calls the seam directly instead of forking mail.sh per row.
# shellcheck disable=SC1090
. "$MAIL"

bead_status() {
    bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")' 2>/dev/null
}

bead_close_reason() {
    bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("close_reason") or "")' 2>/dev/null
}

bead_open_deps() {   # count open BLOCKING deps only; relates-to links are not blockers
    bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
deps = d[0].get("dependencies") or []
print(len([x for x in deps if x.get("status") != "closed" and x.get("dependency_type") == "blocks"]))' 2>/dev/null
}

bead_notes() {
    bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("notes") or "")' 2>/dev/null
}

seed_bead() {
    printf '{"id":"%s","title":"test bead %s","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-01-01T00:00:00Z"}\n' \
        "$1" "$1" | testdb_seed >/dev/null 2>&1
}

msgid_of() {   # msgid_of <mailbox> -> bare Message-ID of the newest message
    local mailbox="$1" newest
    newest="$(ls -t "$SPIRA_MAIL/$mailbox/new/" 2>/dev/null | head -1)"
    [ -z "$newest" ] && return 1
    awk '/^[[:space:]]*$/ { exit }
        tolower($0) ~ /^message-id:/ { sub(/^[^:]*:[[:space:]]*/, ""); gsub(/[<>]/, ""); print; exit }
    ' "$SPIRA_MAIL/$mailbox/new/$newest"
}

xbead_of() {   # xbead_of <mailbox> -> X-Spira-Bead of the newest message
    local mailbox="$1" newest
    newest="$(ls -t "$SPIRA_MAIL/$mailbox/new/" 2>/dev/null | head -1)"
    [ -z "$newest" ] && return 1
    awk '/^[[:space:]]*$/ { exit }
        tolower($0) ~ /^x-spira-bead:/ { sub(/^[^:]*:[[:space:]]*/, ""); print; exit }
    ' "$SPIRA_MAIL/$mailbox/new/$newest"
}

# send_question <mailbox> <work-bead|""> [allow-blocking]
# Writes stderr to $TMP/send.err rather than returning it, and leaves the sent message for
# the caller to inspect with msgid_of/xbead_of — called PLAIN, never through $(...), because a
# command substitution runs in a subshell and any variable this set would be lost on return.
send_question() {
    local mailbox="$1" work_bead="${2:-}"
    local args=(send "$mailbox" --from "Builder <builder@spira>" --subject "Should I proceed with option A?" \
        --kind question --default "proceed with option A")
    [ -n "$work_bead" ] && args+=(--bead "$work_bead")
    if [ "${3:-}" = "allow-blocking" ]; then
        SPIRA_MAIL_LINT_CONSIDERED="test" SPIRA_MAIL_REPEAT_CONSIDERED="test" SPIRA_MAIL_ALLOW_BLOCKING=1 \
            run "${args[@]}" <<'BODY' >/dev/null 2>"$TMP/send.err"
## Question

Should I proceed with option A or wait?

## Default

proceed with option A
BODY
    else
        SPIRA_MAIL_LINT_CONSIDERED="test" SPIRA_MAIL_REPEAT_CONSIDERED="test" run "${args[@]}" \
            <<'BODY' >/dev/null 2>"$TMP/send.err"
## Question

Should I proceed with option A or wait?

## Default

proceed with option A
BODY
    fi
}

compose_reply() {   # compose_reply <in-reply-to> <body> [subject]
    local irt="$1" body="$2" subj="${3:-Re: Should I proceed with option A?}"
    printf 'From: Operator <operator@spira>\n'
    printf 'Subject: %s\n' "$subj"
    printf 'In-Reply-To: <%s>\n' "$irt"
    printf 'Date: %s\n' "$(date -u '+%a, %d %b %Y %H:%M:%S +0000')"
    printf '\n'
    printf '%s\n' "$body"
}

# ==========================================================================
# POSITIVE CONTROL — sendmail exits 0 at all; delivers the message.
# ==========================================================================
echo
echo "positive control — sendmail exits 0 and delivers"

mkdir -p "$SPIRA_MAIL/concierge/new" "$SPIRA_MAIL/concierge/tmp" "$SPIRA_MAIL/concierge/cur"
rc=0; printf 'From: Test\nSubject: Direct\n\nbody\n' | run sendmail >/dev/null 2>&1 || rc=$?
isz "sendmail exits 0 on a plain message" "$rc"
delivered="$(ls "$SPIRA_MAIL/concierge/new" 2>/dev/null | wc -l | tr -d ' ')"
is "SEEN RED: plain message lands in concierge/new" "1" "$delivered"

# ==========================================================================
# UC-14 — question with --bead does not block the work bead; guard logs the refusal
# ==========================================================================
echo
echo "UC-14: question with --bead does not block the work bead (default non-blocking)"

WORK_A="sp-vf-work-a"
seed_bead "$WORK_A"
is "SEEN RED: work bead has 0 open blocking-deps before send" "0" "$(bead_open_deps "$WORK_A")"

send_question operator "$WORK_A"
MSGID_A="$(msgid_of operator)"
[ -n "$MSGID_A" ] || { echo "test-verdict-flow: could not send question"; exit 1; }

# D1 (was test-archivist-cited-bead.sh): the guard logs its refusal of the blocking edge.
want "guard logged the blocking-edge refusal" "blocking edge refused" "$(cat "$TMP/send.err")"

x_bead_a="$(xbead_of operator)"
lacks "X-Spira-Bead is not the work bead (it is the decision bead)" "$WORK_A" "$x_bead_a"
is "decision bead id is present" "0" "$([ -n "$x_bead_a" ] && echo 0 || echo 1)"
is "work bead has 0 open blocking-deps after send (guard prevented blocking)" "0" "$(bead_open_deps "$WORK_A")"
is "work bead is still open" "open" "$(bead_status "$WORK_A")"
is "decision bead is open (waiting for reply)" "open" "$(bead_status "$x_bead_a")"

echo
echo "UC-14: a question with no --bead still files a decision bead"

send_question operator ""
MSGID_NB="$(msgid_of operator)"
[ -n "$MSGID_NB" ] || { echo "test-verdict-flow: could not send no-bead question"; exit 1; }
x_bead_nb="$(xbead_of operator)"
is "decision bead id is present with no --bead" "0" "$([ -n "$x_bead_nb" ] && echo 0 || echo 1)"
is "decision bead is open" "open" "$(bead_status "$x_bead_nb")"

# ==========================================================================
# UC-15 — reply closes the decision bead; work bead (never blocked) stays open;
# verdict note lands on the work bead.
# ==========================================================================
echo
echo "UC-15: reply closes decision bead; work bead stays open; verdict note written"

compose_reply "$MSGID_A" "Yes, proceed with option A." | run sendmail >/dev/null 2>&1; rc=$?
isz "sendmail exits 0 on reply" "$rc"
is "decision bead is closed after reply" "closed" "$(bead_status "$x_bead_a")"
is "work bead is still open (reply did not close it)" "open" "$(bead_status "$WORK_A")"
is "work bead still has 0 open blocking-deps after reply" "0" "$(bead_open_deps "$WORK_A")"
want "work bead note contains operator verdict" "Operator verdict on decision bead $x_bead_a" "$(bead_notes "$WORK_A")"

echo
echo "UC-15: the no-bead decision bead also closes on reply"

compose_reply "$MSGID_NB" "Yes, refactor the parser." | run sendmail >/dev/null 2>&1
is "no-bead decision bead is closed after reply" "closed" "$(bead_status "$x_bead_nb")"

echo
echo "UC-15: a reply to a bare tracking bead (no kind) closes that bead directly"

BEAD_T="sp-vf-track"
seed_bead "$BEAD_T"
is "SEEN RED: tracking bead is open before sendmail" "open" "$(bead_status "$BEAD_T")"
SPIRA_MAIL_LINT_CONSIDERED="test" run send operator --from "Gate <gate@spira>" \
    --subject "Test message" --bead "$BEAD_T" <<< "body" >/dev/null 2>&1
MSGID_T="$(msgid_of operator)"
compose_reply "$MSGID_T" "All looks good." "Re: Test message" | run sendmail >/dev/null 2>&1; rc=$?
isz "sendmail exits 0 on reply" "$rc"
is "reply closes the cited bead directly (no decision bead in the way)" "closed" "$(bead_status "$BEAD_T")"

echo
echo "UC-15/G-01d: SPIRA_MAIL_ALLOW_BLOCKING=1 blocks the work bead; reply unblocks it"

WORK_D="sp-vf-work-d"
seed_bead "$WORK_D"
is "SEEN RED: work bead has 0 blocking-deps before override send" "0" "$(bead_open_deps "$WORK_D")"

send_question operator "$WORK_D" allow-blocking
MSGID_D="$(msgid_of operator)"
x_bead_d="$(xbead_of operator)"
is "with override, work bead IS blocked (1 open blocking-dep)" "1" "$(bead_open_deps "$WORK_D")"

compose_reply "$MSGID_D" "Yes, proceed." | run sendmail >/dev/null 2>&1; rc=$?
isz "sendmail exits 0 on blocking reply" "$rc"
is "blocking decision bead is closed after reply" "closed" "$(bead_status "$x_bead_d")"
is "work bead unblocked after reply (0 open blocking-deps)" "0" "$(bead_open_deps "$WORK_D")"
want "work bead note contains operator verdict" "Operator verdict on decision bead $x_bead_d" "$(bead_notes "$WORK_D")"

# ==========================================================================
# UC-16 — the mail client's accept-default key closes the decision with the
# message's X-Spira-Default, using a real spira.conf.
# ==========================================================================
echo
echo "UC-16: accept-default closes the decision bead with the message's default"

BEAD_ACC="sp-vf-accept"
seed_bead "$BEAD_ACC"
SPIRA_MAIL_LINT_CONSIDERED="test" SPIRA_MAIL_REPEAT_CONSIDERED="test" \
    run send operator --from "Gate <gate@spira>" --subject "Accept test" \
    --kind decision --default "take the accept-test default" --bead "$BEAD_ACC" <<< "body" >/dev/null 2>&1
accept_msg_name="$(ls -t "$SPIRA_MAIL/operator/new/" 2>/dev/null | head -1)"
accept_msg="${accept_msg_name:+$SPIRA_MAIL/operator/new/$accept_msg_name}"
[ -n "$accept_msg" ] && [ -f "$accept_msg" ] || { echo "test-verdict-flow: could not send accept test message"; exit 1; }
dec_bead_accept="$(awk '/^[[:space:]]*$/ { exit }
    tolower($0) ~ /^x-spira-bead:/ { sub(/^[^:]*:[[:space:]]*/, ""); print; exit }
' "$accept_msg")"
is "SEEN RED: decision bead is open before accept-default" "open" "$(bead_status "${dec_bead_accept:-none}")"

printf 'SPIRA_MAIL_UNREAD_AGE = 4242\n' > "$TMP/accept.conf"
accept_out="$(SPIRA_CONF="$TMP/accept.conf" bash "$HERE/../aerc/accept-default.sh" < "$accept_msg" 2>&1)"; rc=$?
isz "accept-default exits 0 with a config file present" "$rc"
[ "$rc" = 0 ] || printf '    %s\n' "$accept_out"
is "accept-default closes the decision bead (not the work bead)" "closed" "$(bead_status "${dec_bead_accept:-none}")"
is "work bead stays open" "open" "$(bead_status "$BEAD_ACC")"
want "the verdict is the message's default" "take the accept-test default" "$(bead_close_reason "${dec_bead_accept:-none}")"
want "work bead note contains the operator verdict" "take the accept-test default" "$(bead_notes "$BEAD_ACC")"

# ==========================================================================
# UC-18 / G-03 — suit verdict words -> close reason, at the T1 seam (no bd, no Maildir).
# ==========================================================================
echo
echo "UC-18/G-03: _suit_reason maps verdict words to close reasons (T1 table)"

is "uphold -> upheld"                    "upheld"           "$(_suit_reason suit "uphold this one")"
is "Uphold (case-insensitive) -> upheld" "upheld"            "$(_suit_reason suit "Uphold.")"
is "retire -> retired"                   "retired"           "$(_suit_reason suit "retire the statute")"
is "amend: <clause> -> the clause"       "add a new clause"  "$(_suit_reason suit "amend: add a new clause")"
is "amend with no clause -> amended"     "amended"           "$(_suit_reason suit "amend")"
is "G-03: unrecognised suit word is left unmapped" \
   "overruled, try again" "$(_suit_reason suit "overruled, try again")"
is "a non-suit kind is never mapped, even with a suit word" \
   "uphold this one" "$(_suit_reason question "uphold this one")"

echo
echo "UC-18: one real close — uphold closes the suit bead with reason 'upheld'"

SUIT1="sp-vf-suit1"
seed_bead "$SUIT1"
SPIRA_MAIL_LINT_CONSIDERED="test" SPIRA_MAIL_REPEAT_CONSIDERED="test" \
    run send operator --from "Gate <gate@spira>" --subject "Suit test" --bead "$SUIT1" --kind suit <<< "body" >/dev/null 2>&1
MSGID_S1="$(msgid_of operator)"
is "SEEN RED: suit bead is open before reply" "open" "$(bead_status "$SUIT1")"
compose_reply "$MSGID_S1" "uphold" "Re: Suit test" | run sendmail >/dev/null 2>&1
is "uphold closes suit bead" "closed" "$(bead_status "$SUIT1")"
is "close reason is the mapped word, not the raw paragraph" "upheld" "$(bead_close_reason "$SUIT1")"

# ==========================================================================
# UC-19 — a failed bead close leaves sendmail non-zero and the original unmarked.
# ==========================================================================
echo
echo "UC-19: bead-not-found — close failure is reported, sendmail exits non-zero"

BEAD_MISSING="sp-vf-missing"
missing_msgid="missing-bead-test.$(date +%s).$$"
mkdir -p "$SPIRA_MAIL/operator/new" "$SPIRA_MAIL/operator/tmp" "$SPIRA_MAIL/operator/cur"
printf 'From: Gate <gate@spira>\nTo: Operator <operator@spira>\nSubject: Missing bead test\nMessage-ID: <%s>\nX-Spira-Bead: %s\nX-Spira-Kind: question\n\nbody\n' \
    "$missing_msgid" "$BEAD_MISSING" > "$SPIRA_MAIL/operator/new/$missing_msgid"
is "SEEN RED: original message exists in new/ before sendmail" "1" \
    "$(ls "$SPIRA_MAIL/operator/new/$missing_msgid" 2>/dev/null | wc -l | tr -d ' ')"

err_out="$(compose_reply "$missing_msgid" "the answer" "Missing bead test" | run sendmail 2>&1)"; reply_rc=$?
isnz "sendmail exits non-zero when bead close fails" "$reply_rc"
want "error names the missing bead" "$BEAD_MISSING" "$err_out"
is "original stays unmodified (not marked replied) when close fails" "1" \
    "$(ls "$SPIRA_MAIL/operator/new/$missing_msgid" 2>/dev/null | wc -l | tr -d ' ')"

# ==========================================================================
# UC-24 — land_escalate (was test-sentinel.sh). Renamed section; same testdb.
# ==========================================================================
echo
echo "UC-24: land_escalate reaches the operator unless an OPEN ask with the subject exists"

# land_escalate shells out to "$SPIRA_HOME/mail.sh", so this section points SPIRA_HOME at a
# scratch dir carrying a logging stub rather than the real mail.sh under test above — the
# other sections here are about mail.sh itself, this one is about lib.sh's caller contract.
LESC_HOME="$TMP/landesc-home"; mkdir -p "$LESC_HOME"
MAIL_LOG="$TMP/mail.log"
cat > "$LESC_HOME/mail.sh" <<MAILSH
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$MAIL_LOG"
cat >/dev/null
MAILSH
chmod +x "$LESC_HOME/mail.sh"

acted=0
act() { acted=$((acted+1)); }
export SPIRA_HOME="$LESC_HOME"
# shellcheck disable=SC1090
. "$HERE/lib.sh"

do_escalate() {
    SPIRA_LAND_ESCALATE_EVERY=0 land_escalate "the landing worker will not start" "evidence"
}

echo "land_escalate: positive control — reaches the operator when no ask is open"
testdb_reset
: > "$MAIL_LOG"; rm -f "$SPIRA_RUN/landing.escalated"
do_escalate >/dev/null 2>&1
want "and reaches the operator" "Spira is landing nothing" "$(cat "$MAIL_LOG")"

echo "land_escalate: open-ask suppression — does not ask again while one is already open"
testdb_reset
printf '{"id":"sp-vf-ask1","title":"Spira is landing nothing — its last run exited 1","status":"open","issue_type":"decision","labels":["%s"],"updated_at":"2026-09-07T00:00:00Z"}\n' \
    "${SPIRA_ASK_LABEL:-needs-operator}" | testdb_seed
: > "$MAIL_LOG"; rm -f "$SPIRA_RUN/landing.escalated"
land_escalate "the landing worker will not start" "evidence" >/dev/null 2>&1
is "and does not ask again while one is still open" "" "$(cat "$MAIL_LOG")"

echo "land_escalate: closed-ask pass-through — a closed ask does NOT suppress a new escalation"
testdb_reset
printf '{"id":"sp-vf-ask1","title":"Spira is landing nothing — its last run exited 1","status":"closed","issue_type":"decision","labels":["%s"],"updated_at":"2026-09-07T00:00:00Z"}\n' \
    "${SPIRA_ASK_LABEL:-needs-operator}" | testdb_seed
: > "$MAIL_LOG"; rm -f "$SPIRA_RUN/landing.escalated"
do_escalate >/dev/null 2>&1
want "a closed ask does not suppress a new escalation" "Spira is landing nothing" "$(cat "$MAIL_LOG")"

export SPIRA_HOME="$TMP/home"   # restore, in case anything is ever appended below

tl_summary
