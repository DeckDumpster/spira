#!/usr/bin/env bash
# test-mail-pane.sh — ops pane MAIL section and mail.sh done command.
#
# Seeds a fixture maildir with one new, one read, and one replied message,
# drives cockpit.sh mail to produce snapshot keys, renders health.sh once,
# and asserts the three states appear. A failed probe renders ?, never 0.
#
# tier: T1
# covers: cockpit/health.sh spira/cockpit.sh spira/mail.sh spira/collect.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
isz()    { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0 got $2"; }
isnz()   { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit got 0"; }

echo "test-mail-pane.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

export SPIRA_MAIL="$TMP/mail"
export SPIRA_CONF=""
export SPIRA_ID_PREFIX="sp"
export SPIRA_DB=""

run_mail() { bash "$HERE/mail.sh" "$@"; }

# ==========================================================================
# POSITIVE CONTROL — mail.sh done marks R flag
# ==========================================================================
echo
echo "done: sets R flag in cur/"

mkdir -p "$SPIRA_MAIL/ctrl/"{new,cur,tmp}
echo -e "From: Test <t@t>\nSubject: Hello\n\nbody" > "$SPIRA_MAIL/ctrl/new/test-msg-1"
run_mail read ctrl >/dev/null 2>&1
is "read moves message to cur/" "1" "$(ls "$SPIRA_MAIL/ctrl/cur" | wc -l | tr -d ' ')"

msgid="$(ls "$SPIRA_MAIL/ctrl/cur" | head -1)"
run_mail done ctrl "$msgid" "acted on it"; rc=$?
isz "done exits 0" "$rc"

rflag_count="$(ls "$SPIRA_MAIL/ctrl/cur" | grep -c ':2,.*R' || true)"
is "R flag set after done" "1" "$rflag_count"

# Mark again — idempotent
run_mail done ctrl "$msgid" "re-done"; rc=$?
isz "done idempotent (exits 0)" "$rc"
rflag_count2="$(ls "$SPIRA_MAIL/ctrl/cur" | grep -c ':2,.*R' || true)"
is "still one file after re-done" "1" "$rflag_count2"

# ==========================================================================
# POSITIVE CONTROL — done on new/ message moves it to cur/ with R flag
# ==========================================================================
echo
echo "done: works on new/ message (moves to cur/)"

echo -e "From: T <t@t>\nSubject: Unread\n\nbody" > "$SPIRA_MAIL/ctrl/new/test-msg-2"
run_mail done ctrl "test-msg-2"; rc=$?
isz "done on new/ exits 0" "$rc"
new_count="$(ls "$SPIRA_MAIL/ctrl/new" | wc -l | tr -d ' ')"
is "message no longer in new/" "0" "$new_count"
rflag3="$(ls "$SPIRA_MAIL/ctrl/cur" | grep 'test-msg-2' | grep -c ':2,.*R' || true)"
is "R flag set on moved message" "1" "$rflag3"

# ==========================================================================
# done: not-found returns non-zero
# ==========================================================================
echo
echo "done: not found returns non-zero"

run_mail done ctrl "nonexistent-id" 2>/dev/null; rc=$?
isnz "done with bad id exits non-zero" "$rc"

# ==========================================================================
# sendmail: sets R flag on original when replying
# ==========================================================================
echo
echo "sendmail: reply sets R flag on original"

mkdir -p "$SPIRA_MAIL/operator/"{new,cur,tmp}
# Send a message to concierge
msgid2="$(date +%s).$$.$RANDOM"
cat > "$SPIRA_MAIL/concierge/new/$msgid2" 2>/dev/null || {
    mkdir -p "$SPIRA_MAIL/concierge/"{new,cur,tmp}
    cat > "$SPIRA_MAIL/concierge/new/$msgid2"
} <<MAIL
From: Operator <operator@spira>
Subject: Please reply
Date: Thu, 01 Jan 2026 00:00:00 +0000
Message-ID: <$msgid2@spira>

Please do something.
MAIL

# Read it (move to cur/)
run_mail read concierge >/dev/null 2>&1

# Reply via sendmail
printf 'In-Reply-To: <%s@spira>\nFrom: Concierge <concierge@spira>\nSubject: Re: Please reply\n\nDone.\n' "$msgid2" \
    | run_mail sendmail; rc=$?
isz "sendmail exits 0" "$rc"

# Original should have R flag
rflag_reply="$(ls "$SPIRA_MAIL/concierge/cur" | grep "^$msgid2" | grep -c ':2,.*R' || true)"
is "R flag set on original after reply" "1" "$rflag_reply"

# ==========================================================================
# cockpit.sh mail — probe keys from fixture maildir
# ==========================================================================
echo
echo "cockpit.sh mail: produces correct state keys"

MAIL_DIR="$TMP/mail2"
mkdir -p "$MAIL_DIR/concierge/"{new,cur,tmp}
NOW="$(date +%s)"

# Seed: one NEW (in new/), one READ (in cur/ no flags), one DONE (in cur/ with R)
printf 'From: Operator <op@h>\nSubject: New message\nDate: %s\n\nbody\n' \
    "$(date -u '+%a, %d %b %Y %H:%M:%S +0000')" > "$MAIL_DIR/concierge/new/new-msg"

printf 'From: Operator <op@h>\nSubject: Read message\nDate: %s\n\nbody\n' \
    "$(date -u '+%a, %d %b %Y %H:%M:%S +0000')" > "$MAIL_DIR/concierge/cur/read-msg"

printf 'From: Operator <op@h>\nSubject: Done message\nDate: %s\n\nbody\n' \
    "$(date -u '+%a, %d %b %Y %H:%M:%S +0000')" > "$MAIL_DIR/concierge/cur/done-msg:2,R"

keys="$(SPIRA_MAIL="$MAIL_DIR" bash "$HERE/cockpit.sh" mail 2>/dev/null)"
isz "cockpit.sh mail exits 0" "$?"

want "SP_MAIL_UNREAD=1" "SP_MAIL_UNREAD=1" "$keys"
want "SP_MAIL_N=3"      "SP_MAIL_N=3"      "$keys"
want "NEW in keys"      "NEW"               "$keys"
want "READ in keys"     "READ"              "$keys"
want "DONE in keys"     "DONE"              "$keys"

# ==========================================================================
# health.sh rendering — MAIL section shows all three states
# ==========================================================================
echo
echo "health.sh: MAIL section renders NEW/READ/DONE"

SNAP="$TMP/cockpit.env"
BUDGET="$TMP/budget.env"
touch "$BUDGET"
# Build a minimal snapshot from cockpit.sh mail probe output
{
    printf 'SP_AT=%s\n' "$(date +%s)"
    SPIRA_MAIL="$MAIL_DIR" bash "$HERE/cockpit.sh" mail 2>/dev/null
    # Minimal keys for health.sh to not crash
    printf 'SP_AEONS=0\nSP_SENTINEL_AGE=5\nSP_OPS_AGE=5\nSP_AURON_AGE=5\n'
    printf 'SP_SENTINEL_TIMER=1\nSP_OPS_TIMER=1\nSP_AURON_TIMER=1\nSP_AURON_FIRING=0\n'
    printf 'SP_TOK_WIN=0\nSP_TOK_WINDOW_H=5\n'
    printf 'SP_RATELIM_5H_PCT=0\nSP_RATELIM_7D_PCT=0\nSP_RATELIM_5H_MIN=0\nSP_RATELIM_7D_MIN=0\n'
    printf 'SP_RATELIM_5H_ETA=-\nSP_RATELIM_7D_ETA=-\nSP_RATELIM_AGE=0\n'
} > "$SNAP"

pane_out="$(SPIRA_RUN="$TMP" bash "$HERE/../cockpit/health.sh" once 2>/dev/null)"
want "MAIL label in pane"  "MAIL"  "$pane_out"
want "NEW state in pane"   "NEW"   "$pane_out"
want "READ state in pane"  "READ"  "$pane_out"
want "DONE state in pane"  "DONE"  "$pane_out"
nowant "CTX label absent"  " CTX " "$pane_out"
nowant "SELF label absent" " SELF " "$pane_out"
nowant "GOV label absent"  " GOV " "$pane_out"

# "Failed probe renders ? not 0" (an absent SP_MAIL_UNREAD) is test-cockpit-probe-fault.sh's
# job (cluster 8, docs/test-plan/cockpit-observability.md): its table asserts against the
# MAIL row specifically, while the case that used to live here only checked that a '?'
# character appeared somewhere in the whole frame — true of nearly any frame regardless of
# whether the MAIL row itself was the one that failed.

# ==========================================================================
echo
tl_summary
