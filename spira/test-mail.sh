#!/usr/bin/env bash
# test-mail.sh — mail.sh: Maildir send/read/list/unread-age/done, the operator repeat
# guard, and message lint as a table over _lint_check.
#
# Lint (UC-operator-channel-02/03/04) is table-driven and calls _lint_check directly —
# mail.sh sourced, not forked (mail.sh:BASH_SOURCE guard) — because the check is already a
# pure function of its arguments plus the kind files (test-plan-2026-09-23 §5). Maildir
# delivery, the repeat guard's on-disk stamp and `done`'s flag rewrite stay T2: they are
# properties of the files mail.sh writes, not of a function's return value.
#
# Each refusal is planted (SEEN RED) before its passing counterpart (SEEN GREEN), so a
# check's silence is evidence, not vacuous truth (law-absence-needs-a-positive-control).
#
# tier: T2
# covers: spira/mail.sh spira/mail/kinds spira/conf.sh UC-operator-channel-01 UC-operator-channel-02 UC-operator-channel-03 UC-operator-channel-04 UC-operator-channel-06 UC-operator-channel-07 UC-operator-channel-17
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
isz() { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0 got $2"; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

export SPIRA_MAIL="$TMP/mail"
export SPIRA_MAIL_KINDS="$TMP/kinds"
export SPIRA_CONF=""       # prevent reading a real spira.conf
export SPIRA_ID_PREFIX="sp"

cp -r "$HERE/mail/kinds/." "$TMP/kinds/"

run() { mail.sh "$@"; }

# ==========================================================================
# T1 — LINT TABLE over _lint_check (UC-operator-channel-02, -03, -04)
# ==========================================================================
# mail.sh is sourced, not forked, so every row below is an in-process function call.
. "$HERE/mail.sh"

lint_case() {   # lint_case <name> <want:0|1> <from> <subject> <kind> <default> <urgent> <body>
    local name="$1" want_ok="$2" from="$3" subject="$4" kind="$5" default="$6" urgent="$7" body="$8"
    local out rc
    out="$(_lint_check "$from" "$subject" "$kind" "$default" "$urgent" "$body" 2>&1)"; rc=$?
    if [ "$want_ok" = 0 ]; then
        is "$name" 0 "$rc"
    else
        [ "$rc" != 0 ] && ok "$name" || bad "$name" "wanted a refusal, lint accepted it"
    fi
    printf '%s' "$out"
}

echo
echo "lint table — mail transport discipline (SEEN RED then SEEN GREEN per rule)"

# -- missing From --
lint_case "SEEN RED: missing From is refused" 1 "" "Hello" "" "" "" "body" >/dev/null
out="$(_lint_check "" "Hello" "" "" "" "body" 2>&1)"
want "refusal names the rule (From)" "rule" "$out"
lint_case "SEEN GREEN: present From is accepted" 0 "Sender <s@s>" "Hello" "" "" "" "body" >/dev/null

# -- missing Subject --
lint_case "SEEN RED: missing Subject is refused" 1 "Sender <s@s>" "" "" "" "" "body" >/dev/null
out="$(_lint_check "Sender <s@s>" "" "" "" "" "body" 2>&1)"
want "refusal names the rule (Subject)" "rule" "$out"
lint_case "SEEN GREEN: present Subject is accepted" 0 "Sender <s@s>" "A topic" "" "" "" "body" >/dev/null

# -- subject leads with a bead id --
lint_case "SEEN RED: 'sp-XXXXX: ...' subject is refused" 1 "Gate <g@g>" "sp-q0k3k: landed" "" "" "" "body" >/dev/null
lint_case "SEEN RED: subject that IS a bead id is refused" 1 "Gate <g@g>" "sp-q0k3k" "" "" "" "body" >/dev/null
lint_case "SEEN GREEN: human-topic subject is accepted" 0 "Gate <g@g>" "Mail delivery landed" "" "" "" "body" >/dev/null

# -- body names a bead id without context --
lint_case "SEEN RED: sparse bead id context is refused" 1 "Gate <g@g>" "Landed" "" "" "" "Fixed sp-q0k3k." >/dev/null
lint_case "SEEN GREEN: body with sufficient bead id context is accepted" 0 "Gate <g@g>" "Summary" "" "" "" \
    "Implemented Maildir mail delivery with atomic send and lint in sp-q0k3k." >/dev/null
lint_case "SEEN GREEN: key-value metadata line with bead id is accepted" 0 "Gate <g@g>" "Result" "" "" "" "target: sp-q0k3k" >/dev/null

# -- RFC 5322 From: bare display name / free text --
lint_case "SEEN RED: bare display name is refused" 1 "Archivist" "Hello" "" "" "" "body" >/dev/null
out="$(_lint_check "Archivist" "Hello" "" "" "" "body" 2>&1)"
want "refusal mentions no address"  "no address"  "$out"
want "refusal names the From value" "Archivist"   "$out"
lint_case "SEEN RED: free-text phrase with no @ is refused" 1 "the archivist from session abc123-def456" "Hello" "" "" "" "body" >/dev/null
lint_case "SEEN GREEN: display-name addr-spec is accepted" 0 "Archivist <archivist@spira>" "Hello" "" "" "" "body" >/dev/null
lint_case "SEEN GREEN: bare addr-spec is accepted" 0 "archivist@spira" "Hello" "" "" "" "body" >/dev/null

# -- RFC 6854 group syntax --
lint_case "SEEN RED: RFC 6854 group syntax is refused" 1 "Archivist:;" "Hello" "" "" "" "body" >/dev/null
out="$(_lint_check "Archivist:;" "Hello" "" "" "" "body" 2>&1)"
want "refusal mentions RFC 6854" "RFC 6854" "$out"
lint_case "SEEN GREEN: mailbox form accepted after group test" 0 "Archivist <archivist@spira>" "Hello" "" "" "" "body" >/dev/null

# -- unknown kind / known kind --
lint_case "SEEN RED: unknown kind is refused" 1 "Sender <s@s>" "Hello" "nosuchkind" "" "" "body" >/dev/null
out="$(_lint_check "Sender <s@s>" "Hello" "nosuchkind" "" "" "body" 2>&1)"
want "refusal names the kind" "nosuchkind" "$out"
lint_case "SEEN GREEN: known kind is accepted" 0 "Sender <s@s>" "Hello" "note" "" "" "$(printf '## Note\n\nContent.\n')" >/dev/null

# -- decision/question: missing X-Spira-Default --
decision_body="$(printf '## Decision\n\nApprove.\n\n## Default\n\nYes.\n\n## What is blocked\n\nNothing.\n\n## Cost of the wrong choice\n\nLow.\n')"
lint_case "SEEN RED: decision without --default is refused" 1 "Gate <g@g>" "Enable feature?" "decision" "" "" "$decision_body" >/dev/null
out="$(_lint_check "Gate <g@g>" "Enable feature?" "decision" "" "" "$decision_body" 2>&1)"
want "refusal mentions default" "default" "$out"
lint_case "SEEN GREEN: decision with --default is accepted" 0 "Gate <g@g>" "Enable feature?" "decision" "yes" "" "$decision_body" >/dev/null

question_body="$(printf '## Question\n\nWhen to start?\n\n## Default\n\nNow.\n')"
lint_case "SEEN RED: question without --default is refused" 1 "Gate <g@g>" "When to start?" "question" "" "" "$question_body" >/dev/null
lint_case "SEEN GREEN: question with --default is accepted" 0 "Gate <g@g>" "When to start?" "question" "now" "" "$question_body" >/dev/null

# -- empty required section --
lint_case "SEEN RED: note with empty ## Note section is refused" 1 "Sender <s@s>" "Empty note" "note" "" "" "$(printf '## Note\n\n')" >/dev/null
out="$(_lint_check "Sender <s@s>" "Empty note" "note" "" "" "$(printf '## Note\n\n')" 2>&1)"
want "refusal mentions the section" "Note" "$out"
lint_case "SEEN GREEN: note with filled ## Note section is accepted" 0 "Sender <s@s>" "Good note" "note" "" "" "$(printf '## Note\n\nSome content here.\n')" >/dev/null

suit_missing="$(printf '## Suit\n\nI claim the service is down.\n\n## Grounds\n\n## Relief sought\n\nFix it.\n')"
lint_case "SEEN RED: suit with empty ## Grounds section is refused" 1 "Sender <s@s>" "Filing suit" "suit" "" "" "$suit_missing" >/dev/null
suit_full="$(printf '## Suit\n\nI claim the service is down.\n\n## Grounds\n\nThe monitor reported no response for 30 minutes.\n\n## Relief sought\n\nRestart the service.\n')"
lint_case "SEEN GREEN: suit with all sections filled is accepted" 0 "Sender <s@s>" "Filing suit" "suit" "" "" "$suit_full" >/dev/null

# -- urgent without "## Why it is urgent" --
lint_case "SEEN RED: urgent message without ## Why it is urgent is refused" 1 "Sender <s@s>" "Urgent matter" "note" "" 1 "$(printf '## Note\n\nContent.\n')" >/dev/null
out="$(_lint_check "Sender <s@s>" "Urgent matter" "note" "" 1 "$(printf '## Note\n\nContent.\n')" 2>&1)"
want "refusal mentions urgency" "urgent" "$out"
lint_case "SEEN GREEN: urgent message with ## Why it is urgent is accepted" 0 "Sender <s@s>" "Urgent matter" "note" "" 1 \
    "$(printf '## Note\n\nContent.\n\n## Why it is urgent\n\nThe system is on fire.\n')" >/dev/null

# -- archivist per-finding note vs the daily digest (sp-9zthk) --
finding_body="$(printf '## Note\n\nFound something.\n')"
out="$(_lint_check "Archivist <archivist@spira>" "What I found" "note" "" "" "$finding_body" 2>&1)"; rc=$?
[ "$rc" != 0 ] && ok "SEEN RED: archivist per-finding note is refused" \
                || bad "SEEN RED: archivist per-finding note is refused" "exit 0"
want "refusal names the rule (archivist note)" "law-fail-closed-at-the-source" "$out"
out="$(_lint_check "Archivist <archivist@spira>" "Not a note" "question" "default" "" \
    "$(printf '## Question\n\nOK?\n\n## Default\n\nYes.\n')" 2>&1)"
is "SEEN GREEN: archivist question (not a note) is unaffected" 0 "$?"
out="$(_lint_check "Sender <s@s>" "What I found" "note" "" "" "$finding_body" 2>&1)"
is "SEEN GREEN: the same note from a non-archivist sender is unaffected" 0 "$?"
out="$(_lint_check "Archivist <archivist@spira>" "Today's digest" "note" "" "" "$finding_body" "1" 2>&1)"
is "SEEN GREEN: archivist note sent with --digest is accepted" 0 "$?"

# -- "the ask below" promise with no section --
lint_case "SEEN RED: body promises ask below but has no section" 1 "Sentinel <sentinel@spira>" "Some event" "" "" "" \
    "$(printf 'not retried until a human changes the approach; the ask below carries the failure\n')" >/dev/null
lint_case "SEEN RED: 'the question below' with no section is also refused" 1 "Sender <s@s>" "An event" "" "" "" \
    "$(printf 'See the question below for details.\n')" >/dev/null
lint_case "SEEN GREEN: body promises ask below and has a Question section" 0 "Sentinel <sentinel@spira>" "Some event" "" "" "" \
    "$(printf 'the ask below carries the failure\n\n## Question\n\nChange the approach?\n')" >/dev/null
lint_case "SEEN GREEN: body with no promise phrase is accepted" 0 "Sender <s@s>" "An event" "" "" "" \
    "$(printf 'The bead was poisoned.\n')" >/dev/null

# -- editing a kind file changes acceptance, no code change --
cat > "$TMP/kinds/custom.md" <<'EOF'
---
---

## Body
EOF
custom_body="$(printf '## Body\n\nHello.\n')"
lint_case "SEEN GREEN: custom kind with no required header is accepted" 0 "Sender <s@s>" "Custom" "custom" "" "" "$custom_body" >/dev/null
cat > "$TMP/kinds/custom.md" <<'EOF'
---
requires: X-Spira-Default
---

## Body
EOF
lint_case "SEEN RED: after tightening kind file, same message is refused" 1 "Sender <s@s>" "Custom" "custom" "" "" "$custom_body" >/dev/null
out="$(_lint_check "Sender <s@s>" "Custom" "custom" "" "" "$custom_body" 2>&1)"
want "refusal mentions default" "default" "$out"

# -- lint override --
echo
echo "lint override: SPIRA_MAIL_LINT_CONSIDERED bypasses lint and is recorded"
SPIRA_MAIL_LINT_CONSIDERED="testing override" out="$(_lint_check "" "" "" "" "" "" 2>&1)"; rc=$?
is "SPIRA_MAIL_LINT_CONSIDERED=1 bypasses missing From and Subject" 0 "$rc"

# ==========================================================================
# T2 — MAILDIR: send lands atomically, read/list/unread-age (UC-operator-channel-01)
# ==========================================================================
echo
echo "positive control — send, read, list"

out="$(echo "Positive control body." | run send ctrl --from "Test <t@t>" --subject "Hello world" 2>&1)"
is "send exits 0" 0 "$?"

new_count="$(ls "$SPIRA_MAIL/ctrl/new" 2>/dev/null | wc -l | tr -d ' ')"
is "message lands in new/" "1" "$new_count"
tmp_count="$(ls "$SPIRA_MAIL/ctrl/tmp" 2>/dev/null | wc -l | tr -d ' ')"
is "tmp/ is empty after delivery (atomic send)" "0" "$tmp_count"

echo
echo "read: prints and moves new -> cur"

read_out="$(run read ctrl 2>&1)"; rc=$?
is "read exits 0" 0 "$rc"
want "read output contains From"    "From:"            "$read_out"
want "read output contains Subject" "Subject:"         "$read_out"
want "read output contains body"    "Positive control" "$read_out"

new_after="$(ls "$SPIRA_MAIL/ctrl/new" 2>/dev/null | wc -l | tr -d ' ')"
cur_after="$(ls "$SPIRA_MAIL/ctrl/cur" 2>/dev/null | wc -l | tr -d ' ')"
is "new/ is empty after read" "0" "$new_after"
is "cur/ gains the message"   "1" "$cur_after"

run read ctrl >/dev/null 2>&1; rc=$?
[ "$rc" != 0 ] && ok "read with no unread mail exits non-zero" || bad "read with no unread mail exits non-zero" "exit 0"

echo
echo "list"

echo "Second." | run send ctrl --from "A <a@a>" --subject "Second" >/dev/null
echo "Third."  | run send ctrl --from "B <b@b>" --subject "Third"  >/dev/null

list_unread="$(run list ctrl --unread 2>&1)"
want "list --unread shows [new]"   "new"     "$list_unread"
want "list --unread shows Subject" "Subject" "$list_unread"

list_all="$(run list ctrl 2>&1)"
want "list (all) shows [cur] messages" "cur" "$list_all"
want "list (all) shows [new] messages" "new" "$list_all"

echo
echo "unread-age"

age="$(run unread-age ctrl 2>&1)"; rc=$?
is "unread-age exits 0 with unread mail" 0 "$rc"
case "$age" in
    ''|*[!0-9]*) bad "unread-age is a number when there is unread mail" "got [$age]" ;;
    *)           ok  "unread-age is a number with unread mail (${age}s)" ;;
esac

run read ctrl >/dev/null 2>&1 || true
run read ctrl >/dev/null 2>&1 || true

age_none="$(run unread-age ctrl 2>&1)"; rc=$?
is "unread-age exits 0 when no unread mail"    0  "$rc"
is "unread-age is empty when no unread mail"   "" "$age_none"

echo
echo "SPIRA_MAIL_FROM: omitted --from defaults from environment (SEEN RED then SEEN GREEN)"

out="$(echo "body" | run send lint-mailfrom-env --subject "Hello" 2>&1)"; rc=$?
[ "$rc" != 0 ] && ok "SEEN RED: missing --from with no SPIRA_MAIL_FROM is refused" \
                || bad "SEEN RED: missing --from with no SPIRA_MAIL_FROM is refused" "exit 0"

out="$(echo "body" | SPIRA_MAIL_FROM="Aeon <aeon@spira>" run send lint-mailfrom-env --subject "Hello" 2>&1)"; rc=$?
is "SEEN GREEN: omitted --from with SPIRA_MAIL_FROM is accepted" 0 "$rc"
msg="$(cat "$SPIRA_MAIL/lint-mailfrom-env/new"/* 2>/dev/null)"
want "SEEN GREEN: From header carries SPIRA_MAIL_FROM value" "Aeon <aeon@spira>" "$msg"

echo
echo "lint override end to end: header recorded on the delivered message"

out="$(echo "body" | SPIRA_MAIL_LINT_CONSIDERED="e2e override" mail.sh send lint-override 2>&1)"; rc=$?
is "SPIRA_MAIL_LINT_CONSIDERED=1 bypasses missing From and Subject end to end" 0 "$rc"
msg="$(cat "$SPIRA_MAIL/lint-override/new"/* 2>/dev/null)"
want "X-Spira-Lint-Override header is present"  "X-Spira-Lint-Override" "$msg"
want "X-Spira-Lint-Override records the reason" "e2e override"          "$msg"

# ==========================================================================
# T2 — DONE: sets the R flag idempotently, fails on an unknown id (UC-operator-channel-07)
# ==========================================================================
echo
echo "done: sets the Maildir R flag idempotently"

echo "body" | run send donebox --from "A <a@a>" --subject "To be done" >/dev/null
msgid="$(ls "$SPIRA_MAIL/donebox/new" | head -1)"
out="$(run done donebox "$msgid" 2>&1)"; rc=$?
is "done exits 0" 0 "$rc"
flagged="$(ls "$SPIRA_MAIL/donebox/cur" | grep -c ':2,.*R' || true)"
is "message carries the R flag after done" "1" "$flagged"

out="$(run done donebox "$msgid" 2>&1)"; rc=$?
is "done is idempotent: second call on the same id still exits 0" 0 "$rc"
flagged2="$(ls "$SPIRA_MAIL/donebox/cur" | grep -c ':2,.*R' || true)"
is "R flag is not doubled by a second done" "1" "$flagged2"

out="$(run done donebox "no-such-id" 2>&1)"; rc=$?
[ "$rc" != 0 ] && ok "done on an unknown id fails" || bad "done on an unknown id fails" "exit 0"

# ==========================================================================
# UC-17 — reply routing (no bead cited, SPIRA_DB unset: routing needs no bead
# store). Moved here from test-mail-decision-ask.sh and test-mail-sendmail.sh
# (docs/test-plan/operator-channel.md row 17): both built a testdb this
# behaviour never reads.
# ==========================================================================
echo
echo "UC-17: reply routing"

export SPIRA_HOME="$TMP/uc17-home"
mkdir -p "$SPIRA_HOME/chamber" "$SPIRA_MAIL/concierge/new" "$SPIRA_MAIL/concierge/tmp" "$SPIRA_MAIL/concierge/cur"

send_plain() {   # send_plain <mailbox> <from> <subject> -> bare Message-ID on stdout
    local mailbox="$1" from="$2" subject="$3" newest
    echo "body" | run send "$mailbox" --from "$from" --subject "$subject" >/dev/null 2>&1
    newest="$(ls -t "$SPIRA_MAIL/$mailbox/new/" 2>/dev/null | head -1)"
    [ -z "$newest" ] && return 1
    awk '/^[[:space:]]*$/ { exit }
        tolower($0) ~ /^message-id:/ { sub(/^[^:]*:[[:space:]]*/, ""); gsub(/[<>]/, ""); print; exit }
    ' "$SPIRA_MAIL/$mailbox/new/$newest"
}

reply_uc17() {   # reply_uc17 <in-reply-to|""> -> an RFC 5322 reply on stdout
    printf 'From: Operator <operator@spira>\nSubject: Re: routing test\n'
    [ -n "$1" ] && printf 'In-Reply-To: <%s>\n' "$1"
    printf 'Date: %s\n\nNoted.\n' "$(date -u '+%a, %d %b %Y %H:%M:%S +0000')"
}

echo
echo "reply routes to the sender's mailbox when one exists"

mkdir -p "$SPIRA_MAIL/gate/new" "$SPIRA_MAIL/gate/tmp" "$SPIRA_MAIL/gate/cur"
MSGID_G="$(send_plain uc17-orig1 "Gate <gate@spira>" "routing test")"
is "SEEN RED: gate mailbox starts empty" "0" "$(ls "$SPIRA_MAIL/gate/new" 2>/dev/null | wc -l | tr -d ' ')"
reply_uc17 "$MSGID_G" | run sendmail >/dev/null 2>&1
is "reply routed to sender's mailbox (gate)" "1" "$(ls "$SPIRA_MAIL/gate/new" 2>/dev/null | wc -l | tr -d ' ')"

echo
echo "reply to a chamber persona routes to concierge, even if a same-named mailbox exists"

printf '# builder persona\n' > "$SPIRA_HOME/chamber/builder.md"
mkdir -p "$SPIRA_MAIL/builder/new" "$SPIRA_MAIL/builder/tmp" "$SPIRA_MAIL/builder/cur"
MSGID_B="$(send_plain uc17-orig2 "Builder <builder@spira>" "routing test")"
builder_before="$(ls "$SPIRA_MAIL/builder/new" 2>/dev/null | wc -l | tr -d ' ')"
conc_before="$(ls "$SPIRA_MAIL/concierge/new" 2>/dev/null | wc -l | tr -d ' ')"
reply_uc17 "$MSGID_B" | run sendmail >/dev/null 2>&1
is "SEEN RED: builder mailbox did not grow (persona beats mailbox existence)" \
    "$builder_before" "$(ls "$SPIRA_MAIL/builder/new" 2>/dev/null | wc -l | tr -d ' ')"
is "reply to persona (builder) routes to concierge" \
    "$((conc_before + 1))" "$(ls "$SPIRA_MAIL/concierge/new" 2>/dev/null | wc -l | tr -d ' ')"

echo
echo "reply with no sender mailbox and no persona routes to concierge"

MSGID_N="$(send_plain uc17-orig3 "Landing gate <nobox@spira>" "routing test")"
conc_before2="$(ls "$SPIRA_MAIL/concierge/new" 2>/dev/null | wc -l | tr -d ' ')"
reply_uc17 "$MSGID_N" | run sendmail >/dev/null 2>&1
is "reply with no sender mailbox routes to concierge" \
    "$((conc_before2 + 1))" "$(ls "$SPIRA_MAIL/concierge/new" 2>/dev/null | wc -l | tr -d ' ')"

echo
echo "reply with no In-Reply-To routes to concierge"

conc_before3="$(ls "$SPIRA_MAIL/concierge/new" 2>/dev/null | wc -l | tr -d ' ')"
reply_uc17 "" | run sendmail >/dev/null 2>&1; rc=$?
isz "sendmail exits 0 with no In-Reply-To" "$rc"
is "no-reply message routes to concierge" \
    "$((conc_before3 + 1))" "$(ls "$SPIRA_MAIL/concierge/new" 2>/dev/null | wc -l | tr -d ' ')"

# ==========================================================================
# T2 — REPEAT GUARD (UC-operator-channel-06)
# ==========================================================================
echo
echo "repeat guard — normalisation (T1: same subject through mail.sh's own hasher)"

export SPIRA_RUN="$TMP/run"
export SPIRA_MAIL_REPEAT_WINDOW=3600

qbody() { printf '## Question\n%s\n\n## Default\n%s\n\nDetailed context goes here.\n' "$1" "$2"; }

SUBJ_A="Spira bead sp-abc — requeued 5 times, never landed — harness cannot land it"
SUBJ_A2="Spira bead sp-abc — requeued 6 times, never landed — harness cannot land it"
SUBJ_B="Spira bead sp-xyz — poisoned after 3 attempts — change the approach or drop it?"

qbody "$SUBJ_A" "close or fix" | run send operator --from "Sentinel <sentinel@spira>" \
    --subject "$SUBJ_A" --kind question --default "close or fix" >/dev/null 2>&1
rc_first=$?
is "first send to operator exits 0" 0 "$rc_first"

out="$(qbody "$SUBJ_A2" "close or fix" | run send operator --from "Sentinel <sentinel@spira>" \
    --subject "$SUBJ_A2" --kind question --default "close or fix" 2>&1)"
rc_second=$?
[ "$rc_second" != 0 ] && ok "second send with same normalised subject is refused" \
    || bad "second send with same normalised subject is refused" "exit 0"
want "refusal message names the override" "SPIRA_MAIL_REPEAT_CONSIDERED" "$out"
want "refusal message says already sent"  "already sent"                "$out"

echo
echo "repeat guard — refusal is counted exactly (not merely non-zero)"

refused_count="$(find "$TMP/run/mail-repeat" -name "*.refused" -exec wc -l {} + 2>/dev/null \
    | awk '/total/ { print $1 } NR==1 && !/total/ { print $1 }' | head -1)"
is "exactly one refusal is recorded after one repeat" "1" "${refused_count:-0}"

echo
echo "repeat guard — negative control: different subject gets through"

out="$(qbody "$SUBJ_B" "close or relabel" | run send operator --from "Sentinel <sentinel@spira>" \
    --subject "$SUBJ_B" --kind question --default "close or relabel" 2>&1)"
is "different subject (different bead, different verb) gets through" 0 "$?"
nowant "different subject carries no repeat-refused message" "repeat refused" "$out"

echo
echo "repeat guard — override bypasses the guard, recorded on the message"

out="$(qbody "$SUBJ_A2" "close or fix" \
    | SPIRA_MAIL_REPEAT_CONSIDERED="testing override" mail.sh send operator \
        --from "Sentinel <sentinel@spira>" --subject "$SUBJ_A2" \
        --kind question --default "close or fix" 2>&1)"
is "SPIRA_MAIL_REPEAT_CONSIDERED lets the repeat through" 0 "$?"
msg_file="$(ls -t "$SPIRA_MAIL/operator/new/" 2>/dev/null | head -1)"
msg_content="$(cat "$SPIRA_MAIL/operator/new/$msg_file" 2>/dev/null)"
want "override reason recorded in X-Spira-Repeat-Override header" \
     "X-Spira-Repeat-Override: testing override" "$msg_content"

echo
echo "repeat guard — a lint-refused send writes no stamp; the corrected resend delivers"

SUBJ_LINT="Lint failure test subject for repeat guard"
out_lint1="$(printf '## Question\n\n## Default\n%s\n' "close" \
    | run send operator --from "Sentinel <sentinel@spira>" \
        --subject "$SUBJ_LINT" --kind question --default "close" 2>&1)"
[ "$?" != 0 ] && ok "lint-refused send exits non-zero" || bad "lint-refused send exits non-zero" "exit 0"
want "refusal message mentions lint" "lint" "$out_lint1"

out_lint2="$(qbody "$SUBJ_LINT" "close" \
    | run send operator --from "Sentinel <sentinel@spira>" \
        --subject "$SUBJ_LINT" --kind question --default "close" 2>&1)"
is "corrected resend after lint failure is delivered" 0 "$?"
nowant "corrected resend is not refused as repeat" "repeat refused" "$out_lint2"

echo
echo "repeat guard — does not apply to non-operator mailboxes"

printf 'Simple note body.\n' | run send concierge --from "Builder <builder@spira>" \
    --subject "Build complete for sp-abc" >/dev/null 2>&1 || true
rc_nc="$(printf 'Simple note body.\n' | run send concierge --from "Builder <builder@spira>" \
    --subject "Build complete for sp-abc" 2>/dev/null; echo $?)"
is "concierge mailbox allows repeat" "0" "$rc_nc"

# ==========================================================================
# G-11 — CONCURRENT SENDS (gap: two senders in the same second; repeat-guard atomicity)
# ==========================================================================
echo
echo "G-11: two senders in the same second — no message id collision, nothing lost"

CONC_BOX="concbox"
( echo "one" | run send "$CONC_BOX" --from "A <a@a>" --subject "Concurrent A" >/dev/null 2>&1 ) &
p1=$!
( echo "two" | run send "$CONC_BOX" --from "B <b@b>" --subject "Concurrent B" >/dev/null 2>&1 ) &
p2=$!
wait "$p1"; rc1=$?
wait "$p2"; rc2=$?
is "concurrent sender 1 exits 0" 0 "$rc1"
is "concurrent sender 2 exits 0" 0 "$rc2"
conc_count="$(ls "$SPIRA_MAIL/$CONC_BOX/new" 2>/dev/null | wc -l | tr -d ' ')"
is "both concurrent sends land as distinct files (no msgid collision)" "2" "$conc_count"

echo
echo "G-11: repeat guard's check-then-stamp is atomic under a forced race (sp-ifh5h)"

# SEEN RED FIRST (before the sp-ifh5h fix): two callers that both reached _repeat_check
# before either reached _repeat_stamp both saw "no stamp yet" and both proceeded. The forced
# interleave below reproduces exactly that window: caller A's check is left holding the
# fingerprint's lock, unstamped, while caller B's check for the identical subject runs
# concurrently. B must block in flock() rather than read a stale "no stamp" state, and
# only see the fresh stamp once A stamps and releases.
CONC_SUBJ="Concurrent repeat-guard race subject for G-11"
_REPEAT_FP=""
_repeat_check operator "$CONC_SUBJ" >/dev/null 2>&1; race_rc_a=$?
is "caller A's check passes and holds the lock, unstamped" 0 "$race_rc_a"

race_b_rc_file="$TMP/race-b-rc"
(
    # Each subshell inherits A's lock fd by fork(2): flock(2) locks belong to the open file
    # DESCRIPTION, not a single descriptor, so closing only A's copy below would not release
    # it while this inherited copy stays open. Close it here first.
    exec {_REPEAT_LOCK_FD}>&-
    _REPEAT_LOCK_FD=""
    _REPEAT_FP=""
    _repeat_check operator "$CONC_SUBJ" >/dev/null 2>&1
    echo $? > "$race_b_rc_file"
) &
pid_b=$!

# Give B time to reach flock() and start blocking on A's still-held lock (best effort; the
# lock guarantees correctness regardless of exactly how long this is).
sleep 0.3
_repeat_stamp   # stamps and releases A's lock — exactly one of A/B may pass unrefused

wait "$pid_b"
race_rc_b="$(cat "$race_b_rc_file" 2>/dev/null)"
is "caller B's racing check for the same subject is refused once A has stamped" "1" "${race_rc_b:-unset}"

# ==========================================================================
# T2 — ARCHIVIST DIGEST GUARD END TO END (sp-9zthk)
# ==========================================================================
echo
echo "archivist digest guard end to end: refused without --digest, delivered and headered with it"

finding_body="$(printf '## Note\n\nFound something loose in a transcript.\n')"
out="$(printf '%s' "$finding_body" | run send operator --from "Archivist <archivist@spira>" \
    --subject "A finding straight from a transcript" --kind note 2>&1)"; rc=$?
[ "$rc" != 0 ] && ok "archivist per-finding note is refused end to end" \
                || bad "archivist per-finding note is refused end to end" "exit 0"
want "refusal names the rule end to end" "law-fail-closed-at-the-source" "$out"
before_count="$(ls "$SPIRA_MAIL/operator/new" 2>/dev/null | wc -l | tr -d ' ')"

out="$(printf '%s' "$finding_body" | run send operator --from "Archivist <archivist@spira>" \
    --subject "Archivist digest: 1 item recorded today" --kind note --digest 2>&1)"; rc=$?
is "archivist digest note is delivered end to end" 0 "$rc"
after_count="$(ls "$SPIRA_MAIL/operator/new" 2>/dev/null | wc -l | tr -d ' ')"
is "the refused note landed nothing; the digest landed one message" \
    "$((before_count + 1))" "$after_count"
digest_file="$(ls -t "$SPIRA_MAIL/operator/new/" 2>/dev/null | head -1)"
digest_msg="$(cat "$SPIRA_MAIL/operator/new/$digest_file" 2>/dev/null)"
want "delivered digest carries X-Spira-Digest" "X-Spira-Digest: yes" "$digest_msg"

# ==========================================================================
# T2 — STDIN READ DEADLINE (sp-znoj6): a body source that never closes is
# refused near SPIRA_LOOM_BUDGET_MS, not hung (sop-mail-send-loom-splice-hang)
# ==========================================================================
echo
echo "stdin read deadline: a body source that never closes is refused, not hung"

DEADLINE_BOX="deadlinebox"
FIFO="$TMP/deadline.fifo"
mkfifo "$FIFO"

# The write end is held open by a holder that outlives the budget many times over, so a
# refusal that arrives near the budget can only be the deadline firing — not a read that
# happened to finish fast on its own (positive control, per CLAUDE.md).
( exec 3>"$FIFO"; sleep 5; exec 3>&- ) &
holder_pid=$!

start_ts="$(date +%s)"
out="$(SPIRA_LOOM_BUDGET_MS=200 run send "$DEADLINE_BOX" --from "A <a@a>" --subject "Never closes" < "$FIFO" 2>&1)"
rc=$?
elapsed=$(( $(date +%s) - start_ts ))

kill "$holder_pid" 2>/dev/null || true
wait "$holder_pid" 2>/dev/null || true

[ "$rc" != 0 ] && ok "send on a stdin that never closes is refused, not hung" \
                || bad "send on a stdin that never closes is refused, not hung" "exit 0"
want "refusal names the deadline" "deadline" "$out"
want "refusal names the responsible key" "SPIRA_LOOM_BUDGET_MS" "$out"
if [ "$elapsed" -lt 3 ]; then
    ok "refusal arrives near the budget (${elapsed}s), not after the full hold"
else
    bad "refusal arrives near the budget, not after the full hold" "${elapsed}s elapsed"
fi
no_msg="$(ls "$SPIRA_MAIL/$DEADLINE_BOX/new" 2>/dev/null | wc -l | tr -d ' ')"
is "no message was delivered from the timed-out send" "0" "${no_msg:-0}"

# ==========================================================================
# MAIL-MUTE (sp-9hwim, design runtime-is-a-release #5): SPIRA_MAIL_MUTE replaces the
# local-overrides tracked edit to this file, which kept the running system's checkout
# dirty on purpose. A muted message must still be RECORDED (a reader listing cur/ finds
# it, and mail.sh read/list still work) but must never land in new/ and wake anyone.
# ==========================================================================
echo
echo "mail-mute: a typed config key, not a file-existence check on the checkout"

MUTE_BOX="mutebox"
mkdir -p "$SPIRA_MAIL/$MUTE_BOX/new" "$SPIRA_MAIL/$MUTE_BOX/cur" "$SPIRA_MAIL/$MUTE_BOX/tmp"

echo
echo "unmuted (default): send lands in new/"
echo "body" | run send "$MUTE_BOX" --from "A <a@a>" --subject "Unmuted" >/dev/null 2>&1
is "SPIRA_MAIL_MUTE unset: message lands in new/" "1" \
    "$(ls "$SPIRA_MAIL/$MUTE_BOX/new" 2>/dev/null | wc -l | tr -d ' ')"
is "SPIRA_MAIL_MUTE unset: nothing lands in cur/" "0" \
    "$(ls "$SPIRA_MAIL/$MUTE_BOX/cur" 2>/dev/null | wc -l | tr -d ' ')"

echo
echo "muted: send is recorded in cur/, already Seen, never wakes new/"
before_new="$(ls "$SPIRA_MAIL/$MUTE_BOX/new" 2>/dev/null | wc -l | tr -d ' ')"
echo "body" | SPIRA_MAIL_MUTE=1 run send "$MUTE_BOX" --from "A <a@a>" --subject "Muted" >/dev/null 2>&1
is "SPIRA_MAIL_MUTE=1: new/ does not grow" "$before_new" \
    "$(ls "$SPIRA_MAIL/$MUTE_BOX/new" 2>/dev/null | wc -l | tr -d ' ')"
muted_file="$(ls "$SPIRA_MAIL/$MUTE_BOX/cur" 2>/dev/null | grep ':2,S$' | head -1)"
is "SPIRA_MAIL_MUTE=1: the message is recorded in cur/, flagged Seen" "1" \
    "$([ -n "$muted_file" ] && echo 1 || echo 0)"
want "muted message content survives — it is recorded, not discarded" "Muted" \
    "$(cat "$SPIRA_MAIL/$MUTE_BOX/cur/$muted_file" 2>/dev/null)"

echo
echo "muted sendmail (raw path, no In-Reply-To -> concierge) is muted the same way as send"
mkdir -p "$SPIRA_MAIL/concierge/new" "$SPIRA_MAIL/concierge/cur" "$SPIRA_MAIL/concierge/tmp"
conc_new_before="$(ls "$SPIRA_MAIL/concierge/new" 2>/dev/null | wc -l | tr -d ' ')"
conc_cur_before="$(ls "$SPIRA_MAIL/concierge/cur" 2>/dev/null | wc -l | tr -d ' ')"
printf 'From: Someone <s@s>\nSubject: raw muted\n\nbody\n' \
    | SPIRA_MAIL_MUTE=1 run sendmail >/dev/null 2>&1
is "SPIRA_MAIL_MUTE=1 sendmail: new/ does not grow" "$conc_new_before" \
    "$(ls "$SPIRA_MAIL/concierge/new" 2>/dev/null | wc -l | tr -d ' ')"
is "SPIRA_MAIL_MUTE=1 sendmail: recorded in cur/ instead" "$((conc_cur_before + 1))" \
    "$(ls "$SPIRA_MAIL/concierge/cur" 2>/dev/null | wc -l | tr -d ' ')"

echo
tl_summary
