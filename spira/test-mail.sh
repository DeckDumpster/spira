#!/usr/bin/env bash
# test-mail.sh — mail: Maildir send/read/list/unread-age/done, the operator repeat guard,
# message lint end to end, mail-mute, the stdin deadline, and UC-17 reply routing.
#
# Lint's own table (UC-operator-channel-02/03/04) moved to mail/src/lint.rs's unit tests
# (sp-ooh1k): mail was sourced as bash to call _lint_check in-process; a compiled binary
# cannot be sourced, so the pure-function table lives in Rust now, case for case. This suite
# still exercises the same lint rules end to end, through real `mail send` calls (the
# archivist digest guard section, the lint-override section, the repeat guard's own
# lint-then-repeat ordering).
#
# Each refusal is planted (SEEN RED) before its passing counterpart (SEEN GREEN), so a
# check's silence is evidence, not vacuous truth (law-absence-needs-a-positive-control).
#
# tier: T2
# covers: mail/src/* spira/mail/kinds spira/conf.sh UC-operator-channel-01 UC-operator-channel-02 UC-operator-channel-03 UC-operator-channel-04 UC-operator-channel-06 UC-operator-channel-07 UC-operator-channel-17
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
isz() { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0 got $2"; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

SPIRA_MAIL="$TMP/mail"
SPIRA_MAIL_KINDS="$TMP/kinds"
SPIRA_ID_PREFIX="sp"
mkdir -p "$TMP/watchd"
# SPIRA_MAIL_MUTE=0 up front: the complete fixture's own declared default is true (every
# key needs SOME value), which would silently mute every "lands in new/" assertion in this
# suite, not just the mail-mute section below that tests muting on purpose.
# SPIRA_CONCIERGE_INBOX undeclared resolves to the complete fixture's
# /fixture/userhome/spira/run/watchd/concierge-inbox.log — mail appends every send there, and
# the write fails outright with no such directory (sfail round 3, pattern 7).
tl_config SPIRA_MAIL="$SPIRA_MAIL" SPIRA_MAIL_KINDS="$SPIRA_MAIL_KINDS" \
    SPIRA_ID_PREFIX="$SPIRA_ID_PREFIX" SPIRA_MAIL_MUTE=0 \
    SPIRA_MAIL_INDEX="$SPIRA_MAIL/index" \
    SPIRA_RUN="$TMP/run" SPIRA_DB="$TMP/db" SPIRA_BD="${SPIRA_BD:-bd}" \
    SPIRA_OPERATOR_ACTOR=ryan \
    SPIRA_CONCIERGE_INBOX="$TMP/watchd/concierge-inbox.log"
export SPIRA_CONF=""       # prevent reading a real spira.conf

cp -r "$HERE/mail/kinds/." "$TMP/kinds/"

run() { mail "$@"; }

# T1 — the lint table that used to live here (UC-operator-channel-02/03/04) called
# _lint_check in-process by sourcing mail as bash. mail is gone (sp-ooh1k): the
# check is now compiled Rust, and a compiled binary cannot be sourced. The full table
# moved verbatim to mail/src/lint.rs's own unit tests (`cargo test -p mail`); the
# archivist-digest-guard section below still exercises the same lint end to end, through
# real `mail send` calls.
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

out="$(echo "body" | SPIRA_MAIL_LINT_CONSIDERED="e2e override" mail send lint-override 2>&1)"; rc=$?
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
# sp-ivfu3: mail now resolves spira.run in-process through spira_config, which needs a
# real conf.d registry under SPIRA_HOME to resolve ANY key (including the ones this UC
# never pins, SPIRA_LOOM_BUDGET_MS included) — law-a-binary-resolves-the-config-it-reads;
# a fixture SPIRA_HOME that runs a binary needs conf.d, the same way $HERE already is one.
ln -s "$HERE/conf.d" "$SPIRA_HOME/conf.d"
# SPIRA_CHAMBER is registered and the fixture declares a fixed, nonexistent path — nothing
# derives it from SPIRA_HOME any more (sfail round 2, pattern 6).
tl_config SPIRA_CHAMBER="$SPIRA_HOME/chamber"

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
echo "repeat guard — normalisation (T1: same subject through mail's own hasher)"

tl_config SPIRA_RUN="$TMP/run" SPIRA_MAIL_REPEAT_WINDOW=3600

qbody() { printf '## Question\n%s\n\n## Default\n%s\n\n## Class basis\nneeds a policy ruling\n\nDetailed context goes here.\n' "$1" "$2"; }

SUBJ_A="Spira bead sp-abc — requeued 5 times, never landed — harness cannot land it"
SUBJ_A2="Spira bead sp-abc — requeued 6 times, never landed — harness cannot land it"
SUBJ_B="Spira bead sp-xyz — poisoned after 3 attempts — change the approach or drop it?"

qbody "$SUBJ_A" "close or fix" | run send operator --from "Sentinel <sentinel@spira>" \
    --subject "$SUBJ_A" --kind question --class policy --default "close or fix" >/dev/null 2>&1
rc_first=$?
is "first send to operator exits 0" 0 "$rc_first"

out="$(qbody "$SUBJ_A2" "close or fix" | run send operator --from "Sentinel <sentinel@spira>" \
    --subject "$SUBJ_A2" --kind question --class policy --default "close or fix" 2>&1)"
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
    --subject "$SUBJ_B" --kind question --class policy --default "close or relabel" 2>&1)"
is "different subject (different bead, different verb) gets through" 0 "$?"
nowant "different subject carries no repeat-refused message" "repeat refused" "$out"

echo
echo "repeat guard — override bypasses the guard, recorded on the message"

out="$(qbody "$SUBJ_A2" "close or fix" \
    | SPIRA_MAIL_REPEAT_CONSIDERED="testing override" SPIRA_MAIL_OPERATOR_CONSIDERED="testing override" mail send operator \
        --from "Sentinel <sentinel@spira>" --subject "$SUBJ_A2" \
        --kind question --class policy --default "close or fix" 2>&1)"
is "SPIRA_MAIL_REPEAT_CONSIDERED lets the repeat through" 0 "$?"
msg_file="$(ls -t "$SPIRA_MAIL/operator/new/" 2>/dev/null | head -1)"
msg_content="$(cat "$SPIRA_MAIL/operator/new/$msg_file" 2>/dev/null)"
want "override reason recorded in X-Spira-Repeat-Override header" \
     "X-Spira-Repeat-Override: testing override" "$msg_content"

echo
echo "repeat guard — a lint-refused send writes no stamp; the corrected resend delivers"

SUBJ_LINT="Lint failure test subject for repeat guard"
out_lint1="$(printf '## Question\n\n## Class basis\nx\n\n## Default\n%s\n' "close" \
    | run send operator --from "Sentinel <sentinel@spira>" \
        --subject "$SUBJ_LINT" --kind question --class policy --default "close" 2>&1)"
[ "$?" != 0 ] && ok "lint-refused send exits non-zero" || bad "lint-refused send exits non-zero" "exit 0"
want "refusal message mentions lint" "lint" "$out_lint1"

out_lint2="$(qbody "$SUBJ_LINT" "close" \
    | run send operator --from "Sentinel <sentinel@spira>" \
        --subject "$SUBJ_LINT" --kind question --class policy --default "close" 2>&1)"
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

# SEEN RED FIRST (before the sp-ifh5h fix): two callers that both reached the repeat check
# before either stamped both saw "no stamp yet" and both proceeded. mail's per-fingerprint
# flock (mail/src/repeat.rs) is held from the check through delivery and the stamp, not
# released and reacquired around them, so this needs no artificial interleave any more (that
# trick only existed to force the race while the check was a sourced bash function with its
# own global _REPEAT_FP/_REPEAT_LOCK_FD — a compiled binary has neither to reach into): two
# real, concurrent `mail send` processes for the identical subject already race on the same
# lock file, and the lock guarantees exactly one passes regardless of which starts first.
CONC_SUBJ="Concurrent repeat-guard race subject for G-11"
qbody "$CONC_SUBJ" "pick one" > "$TMP/race-body"

race_a_rc_file="$TMP/race-a-rc"
race_b_rc_file="$TMP/race-b-rc"
( run send operator --from "Sentinel <sentinel@spira>" --subject "$CONC_SUBJ" \
    --kind question --class policy --default "pick one" < "$TMP/race-body" >/dev/null 2>&1
  echo $? > "$race_a_rc_file" ) &
pid_a=$!
( run send operator --from "Sentinel <sentinel@spira>" --subject "$CONC_SUBJ" \
    --kind question --class policy --default "pick one" < "$TMP/race-body" >/dev/null 2>&1
  echo $? > "$race_b_rc_file" ) &
pid_b=$!

wait "$pid_a" "$pid_b"
race_rc_a="$(cat "$race_a_rc_file" 2>/dev/null)"
race_rc_b="$(cat "$race_b_rc_file" 2>/dev/null)"
race_successes=0
[ "$race_rc_a" = 0 ] && race_successes=$((race_successes + 1))
[ "$race_rc_b" = 0 ] && race_successes=$((race_successes + 1))
is "exactly one of the two racing sends succeeds (rc_a=$race_rc_a rc_b=$race_rc_b)" "1" "$race_successes"

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
tl_config SPIRA_LOOM_BUDGET_MS=200
out="$(run send "$DEADLINE_BOX" --from "A <a@a>" --subject "Never closes" < "$FIFO" 2>&1)"
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
# Restore the fixture's own budget — tl_config persists for the rest of the suite,
# unlike the old per-call env prefix, which reverted on its own after this one call.
tl_config SPIRA_LOOM_BUDGET_MS=1500

# ==========================================================================
# MAIL-MUTE (sp-9hwim, design runtime-is-a-release #5): SPIRA_MAIL_MUTE replaces the
# local-overrides tracked edit to this file, which kept the running system's checkout
# dirty on purpose. A muted message must still be RECORDED (a reader listing cur/ finds
# it, and mail read/list still work) but must never land in new/ and wake anyone.
# ==========================================================================
echo
echo "mail-mute: a typed config key, not a file-existence check on the checkout"

MUTE_BOX="mutebox"
mkdir -p "$SPIRA_MAIL/$MUTE_BOX/new" "$SPIRA_MAIL/$MUTE_BOX/cur" "$SPIRA_MAIL/$MUTE_BOX/tmp"

echo
echo "unmuted (default): send lands in new/"
# SPIRA_MAIL_MUTE=0 was already declared at the top of this suite (see the comment there).
echo "body" | run send "$MUTE_BOX" --from "A <a@a>" --subject "Unmuted" >/dev/null 2>&1
is "SPIRA_MAIL_MUTE unset: message lands in new/" "1" \
    "$(ls "$SPIRA_MAIL/$MUTE_BOX/new" 2>/dev/null | wc -l | tr -d ' ')"
is "SPIRA_MAIL_MUTE unset: nothing lands in cur/" "0" \
    "$(ls "$SPIRA_MAIL/$MUTE_BOX/cur" 2>/dev/null | wc -l | tr -d ' ')"

echo
echo "muted: send is recorded in cur/, already Seen, never wakes new/"
before_new="$(ls "$SPIRA_MAIL/$MUTE_BOX/new" 2>/dev/null | wc -l | tr -d ' ')"
tl_config SPIRA_MAIL_MUTE=1
echo "body" | run send "$MUTE_BOX" --from "A <a@a>" --subject "Muted" >/dev/null 2>&1
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
    | run sendmail >/dev/null 2>&1
is "SPIRA_MAIL_MUTE=1 sendmail: new/ does not grow" "$conc_new_before" \
    "$(ls "$SPIRA_MAIL/concierge/new" 2>/dev/null | wc -l | tr -d ' ')"
is "SPIRA_MAIL_MUTE=1 sendmail: recorded in cur/ instead" "$((conc_cur_before + 1))" \
    "$(ls "$SPIRA_MAIL/concierge/cur" 2>/dev/null | wc -l | tr -d ' ')"

# tl_config persists for the rest of the suite — MUTE=1 set above for the mute section would
# otherwise silently discard every "lands in new/" assertion below into cur/ instead.
tl_config SPIRA_MAIL_MUTE=0

echo
echo "escalation class: an aeon's operator ask must declare one (law-escalate-decisions-not-problems)"
CLS_BODY="$(printf '## Question\nwhich shape?\n\n## Default\nthe crate\n\n## Class basis\nneeds a credential\n')"
CLS_NOBASIS="$(printf '## Question\nwhich shape?\n\n## Default\nthe crate\n')"
cls_send() { # <subject> <class-args...> ; body on $CLS_STDIN
    local subj="$1"; shift
    printf '%s\n' "$CLS_STDIN" | BEAD_ID=sp-aaaaa run send operator --from "Builder <builder@spira>" \
        --subject "$subj" --kind question --default "the crate" "$@" 2>&1
}
op_before="$(ls "$SPIRA_MAIL/operator/new" 2>/dev/null | wc -l | tr -d ' ')"
conc_before="$(ls "$SPIRA_MAIL/concierge/new" 2>/dev/null | wc -l | tr -d ' ')"
CLS_STDIN="$CLS_BODY"
out="$(cls_send "Which crate layout should gate-worker take" --class architecture)"
want "architecture class: aeon is told it was routed to the concierge" "routed to the concierge" "$out"
is "architecture class: operator inbox unchanged" "$op_before" "$(ls "$SPIRA_MAIL/operator/new" 2>/dev/null | wc -l | tr -d ' ')"
is "architecture class: concierge got it" "$((conc_before + 1))" "$(ls "$SPIRA_MAIL/concierge/new" | wc -l | tr -d ' ')"
out="$(cls_send "Which verb layout should gate-worker take" )"
want "no class: routed to the concierge" "routed to the concierge" "$out"
is "no class: operator inbox unchanged" "$op_before" "$(ls "$SPIRA_MAIL/operator/new" 2>/dev/null | wc -l | tr -d ' ')"
CLS_STDIN="$CLS_NOBASIS"
out="$(cls_send "Grant the aeon a deploy credential" --class permissions)"
want "class without a basis section: routed to the concierge" "routed to the concierge" "$out"
CLS_STDIN="$CLS_BODY"
out="$(cls_send "Grant the aeon a deploy credential" --class permissions)"; rc=$?
is "permissions ask exits 0" 0 "$rc"
is "permissions ask reaches the operator" "$((op_before + 1))" "$(ls "$SPIRA_MAIL/operator/new" | wc -l | tr -d ' ')"
is "permissions ask is not routed to the concierge" "$((conc_before + 3))" "$(ls "$SPIRA_MAIL/concierge/new" | wc -l | tr -d ' ')"

echo
echo "escalation class binds a sender holding no bead (watchers, sentinel, mail ask path)"
op_before="$(ls "$SPIRA_MAIL/operator/new" | wc -l | tr -d ' ')"
conc_before="$(ls "$SPIRA_MAIL/concierge/new" | wc -l | tr -d ' ')"
nb_send() { printf '%s\n' "$CLS_STDIN" | ( unset BEAD_ID; run send operator --from "Sentinel <sentinel@spira>" \
    --subject "$1" --kind question --default "the crate" "${@:2}" 2>&1 ); }
CLS_STDIN="$CLS_BODY"
out="$(nb_send "Which verb layout should the watcher take" --class architecture)"
want "no-bead sender, out-of-class ask: told it was routed to the concierge" "routed to the concierge" "$out"
is "no-bead sender, out-of-class ask: operator inbox unchanged" "$op_before" "$(ls "$SPIRA_MAIL/operator/new" | wc -l | tr -d ' ')"
is "no-bead sender, out-of-class ask: concierge got it" "$((conc_before + 1))" "$(ls "$SPIRA_MAIL/concierge/new" | wc -l | tr -d ' ')"
out="$(nb_send "Grant the watcher a deploy credential" --class permissions)"
is "no-bead sender, in-class ask reaches the operator" "$((op_before + 1))" "$(ls "$SPIRA_MAIL/operator/new" | wc -l | tr -d ' ')"

echo
tl_summary
