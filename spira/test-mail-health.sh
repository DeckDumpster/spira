#!/usr/bin/env bash
# test-mail-health.sh — mail-health.sh: unread-age check over registered mailboxes.
#
# SEEN RED before every silent check (law-absence-needs-a-positive-control):
#   - fires when threshold exceeded: stub watcher; no mail to operator first
#   - fires once not per pass: first run mails, second does not
#   - re-fires after clear: backlog gone clears state; new backlog fires again
#   - silent below threshold: fresh message never triggers
#
# tier: T2
# covers: spira/mail-health.sh mail/src/* spira/conf.sh UC-operator-channel-09
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
isz()    { is "$1" 0 "$2"; }
is1()    { is "$1" 1 "$2"; }
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

# SPIRA_MAIL/SPIRA_MAIL_KINDS are registered but mail/src/env.rs reads them with a plain
# std::env::var, not spira_config::process::cfg — so the "mail" binary itself only ever
# sees the plain export below. But mail-health.sh (unlike a direct `mail` invocation) is a
# bash script that sources conf.sh itself, and conf.sh's resolve --sh-all RE-EXPORTS every
# registered key from SPIRA_TOML into mail-health.sh's own process — overwriting this plain
# export with the complete fixture's bogus SPIRA_MAIL default before mail-health.sh ever
# spawns its own `mail unread-age`/`mail count` children, which then inherit the bogus
# value (same shape as the SPIRA_DB pattern elsewhere, round 5 — this suite's own core
# detect-and-alert path was silently looking at the wrong maildir the whole time).
export SPIRA_MAIL="$TMP/mail"
export SPIRA_MAIL_KINDS="$HERE/mail/kinds"
export SPIRA_MAIL_OPERATOR_CONSIDERED=test-suite
export SPIRA_HOME="$HERE"
export HOME="$TMP/home"; mkdir -p "$HOME"
export SPIRA_CONF="$TMP/no-such-spira.conf"
# SPIRA_MAIL_MUTE is a raw env read too (mail/src/env.rs), but conf.sh resolves it from
# SPIRA_TOML and EXPORTS it (resolve --sh-all) into every subprocess's own environment —
# the complete fixture now declares mail_mute=true, which would silently mute every
# delivery this suite counts on. Declared false here so conf.sh exports the override.
tl_config SPIRA_RUN="$TMP/run" SPIRA_ID_PREFIX="sp" SPIRA_MAIL_UNREAD_AGE=60 SPIRA_MAIL_MUTE=false \
    SPIRA_MAIL="$TMP/mail"

HEALTH=mail-health.sh   # invoked by name on the suite's PATH (sp-gypjk)
MAIL=mail   # invoked by name on the suite's PATH (sp-gypjk)

# Deliver a message to a mailbox and backdate its mtime to simulate an old backlog.
send_old() {
    local mailbox="$1" age_s="$2"
    echo "body" | SPIRA_MAIL_LINT_CONSIDERED="test" \
        "$MAIL" send "$mailbox" --from "S <s@s>" --subject "Old message" 2>/dev/null
    local f; f="$(ls -t "$SPIRA_MAIL/$mailbox/new/" 2>/dev/null | head -1)"
    [ -n "$f" ] || { printf 'send_old: no file in new/\n' >&2; return 1; }
    touch -d "@$(( $(date +%s) - age_s ))" "$SPIRA_MAIL/$mailbox/new/$f"
}

op_count() {
    "$MAIL" count operator 2>/dev/null
}

tl_config SPIRA_MAIL_READERS="concierge=echo wake"

# install.sh runs this before any timer can read the operator mailbox; a read verb now
# refuses a mailbox that was never provisioned, so the fixture must match that order.
"$MAIL" ensure operator

echo
echo "=== POSITIVE CONTROL — fires when threshold exceeded ==="

# Confirm operator mailbox has no mail yet: if the check never fires, below
# assertions would still pass — so this confirms the starting state is empty.
initial="$(op_count)"
is "SEEN RED: operator starts with 0 messages" "0" "$initial"

send_old concierge 120

rc=0; "$HEALTH" 2>/dev/null; rc=$?
is1 "exits 1 when backlog exceeds threshold" "$rc"

count1="$(op_count)"
is "operator receives exactly one health message" "1" "$count1"

msg="$("$MAIL" list operator --unread 2>/dev/null)"
want "message names the mailbox" "concierge" "$msg"

echo
echo "=== fires once, not per pass ==="

rc=0; "$HEALTH" 2>/dev/null; rc=$?
isz "exits 0 on second pass with same backlog" "$rc"

count2="$(op_count)"
is "no second message for same backlog" "1" "$count2"

echo
echo "=== re-fires after backlog clears and recurs ==="

# Read concierge mail — clears the backlog.
"$MAIL" read concierge >/dev/null 2>&1 || true

# Health should clear state (no backlog).
rc=0; "$HEALTH" 2>/dev/null; rc=$?
isz "exits 0 when mailbox is empty" "$rc"

# State file must be gone.
sf_gone=1; [ -f "$TMP/run/mail-health/concierge" ] && sf_gone=0
is "state file removed when mailbox empties" "1" "$sf_gone"

# New old backlog arrives.
send_old concierge 120

rc=0; "$HEALTH" 2>/dev/null; rc=$?
is1 "re-fires when backlog recurs after clearing" "$rc"

count3="$(op_count)"
is "second notification sent after recurrence" "2" "$count3"

echo
echo "=== silent below threshold ==="

# Send fresh mail to a different mailbox — not old enough.
tl_config SPIRA_MAIL_READERS="freshbox=echo wake"
echo "fresh" | SPIRA_MAIL_LINT_CONSIDERED="test" \
    "$MAIL" send freshbox --from "T <t@t>" --subject "Fresh message" 2>/dev/null

# SEEN RED: confirm freshbox has mail so that silence below is meaningful.
fresh_count="$("$MAIL" count freshbox 2>/dev/null)"
is "SEEN RED: freshbox has 1 message" "1" "$fresh_count"

rc=0; "$HEALTH" 2>/dev/null; rc=$?
isz "exits 0 when age below threshold" "$rc"

count4="$(op_count)"
is "no message for fresh mail" "2" "$count4"

echo
echo "=== outbound: aerc's outgoing command ==="

tl_config SPIRA_MAIL_READERS=""
mkdir -p "$HOME/.config/aerc"
CONF="$HOME/.config/aerc/accounts.conf"
op_before="$(op_count)"

rc=0; "$HEALTH" 2>/dev/null; rc=$?
isz "no accounts.conf: silent" "$rc"

printf '[spira]\noutgoing = %s/gone/mail.sh sendmail\n' "$TMP" > "$CONF"
rc=0; "$HEALTH" 2>/dev/null; rc=$?
is1 "SEEN RED: outgoing at a deleted path mails the operator" "$rc"
is "one message for the dead outgoing path" "$((op_before + 1))" "$(op_count)"

rc=0; "$HEALTH" 2>/dev/null; rc=$?
isz "same fault is not mailed twice" "$rc"
is "no second message" "$((op_before + 1))" "$(op_count)"

mkdir -p "$TMP/wr"
printf '#!/bin/sh\n[ "$1" = --check ] && exit 1\nexit 0\n' > "$TMP/wr/spira-sendmail"; chmod +x "$TMP/wr/spira-sendmail"
printf '[spira]\noutgoing = %s/wr/spira-sendmail\n' "$TMP" > "$CONF"
rc=0; "$HEALTH" 2>/dev/null; rc=$?
is1 "an executable spira-sendmail whose --check fails is a fault" "$rc"

printf '#!/bin/sh\nexit 0\n' > "$TMP/wr/spira-sendmail"
rc=0; "$HEALTH" 2>/dev/null; rc=$?
isz "a working spira-sendmail is silent" "$rc"
[ -e "$TMP/run/mail-health/outbound" ] && bad "state cleared when the fault clears" "still present" || ok "state cleared when the fault clears"

tl_summary
