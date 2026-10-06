#!/usr/bin/env bash
#
# test-mail-mailbox-arg.sh — mail's mailbox argument is positional, so a flag that
# lands in that slot (a caller's typo, an argument shifted one place — e.g.
# `mail list --unread` with no mailbox given, or `mail count --inbox`) is
# otherwise a legal directory name. Before this fix mail silently created and then
# truthfully reported on an empty mailbox named after the flag: nine of these
# (--all --bead --box --from --help --inbox --kind --subject --to) accumulated on the
# real box, each one telling a caller "no mail" from a mailbox its own question had
# just brought into existence. This suite plants that shape and requires a loud refusal
# instead (sp-ly6l9).
#
# Also covers the read-verb half of the same fix: list/read/count/unread-age/done must
# refuse a mailbox that does not exist rather than creating it — only `send` and the
# explicit `ensure` command may create one.
#
# tier: T1
# covers: mail/src/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

SPIRA_MAIL="$TMP/mail"
SPIRA_MAIL_KINDS="$TMP/kinds"
SPIRA_ID_PREFIX="sp"
mkdir -p "$TMP/watchd"
# SPIRA_CONCIERGE_INBOX undeclared resolves to the complete fixture's
# /fixture/home/spira/run/watchd/concierge-inbox.log — mail appends every send there, and
# the write fails outright with no such directory (sfail round 3, pattern 7).
# SPIRA_MAIL_MUTE=0: the complete fixture's own declared default is true, which silently
# writes every "creates the mailbox on demand" send to cur/ Seen instead of new/.
tl_config SPIRA_MAIL="$SPIRA_MAIL" SPIRA_MAIL_KINDS="$SPIRA_MAIL_KINDS" \
    SPIRA_ID_PREFIX="$SPIRA_ID_PREFIX" SPIRA_MAIL_INDEX="$SPIRA_MAIL/index" \
    SPIRA_MAIL_MUTE=0 \
    SPIRA_RUN="$TMP/run" SPIRA_DB="$TMP/db" SPIRA_BD="${SPIRA_BD:-bd}" \
    SPIRA_OPERATOR_ACTOR=ryan \
    SPIRA_CONCIERGE_INBOX="$TMP/watchd/concierge-inbox.log"
export SPIRA_CONF=""
# SPIRA_HOME IS THE HOME now (locate_home no longer searches): every binary reads
# <home>/conf.d (sfail round 2, pattern 1); $HERE already carries the real one.
export SPIRA_HOME="$HERE"
mkdir -p "$SPIRA_MAIL_KINDS"

run() { timeout 30 mail "$@"; }

no_mailbox_created() {  # <name> <label>
    [ -e "$SPIRA_MAIL/$1" ] && bad "$2: no mailbox created" "found $SPIRA_MAIL/$1" \
                            || ok  "$2: no mailbox created"
}

echo
echo "=== a flag in the mailbox slot is refused, not silently created ==="

# POSITIVE CONTROL: exactly the real-world shape (list --unread with the mailbox
# omitted) is caught before any other case is trusted.
out="$(run list --unread 2>&1)"; rc=$?
wantrc "list --unread: refused"        1 "$rc"
want   "list --unread: names the flag" "--unread" "$out"
no_mailbox_created "--unread" "list --unread"

out="$(run count --inbox 2>&1)"; rc=$?
wantrc "count --inbox: refused" 1 "$rc"
no_mailbox_created "--inbox" "count --inbox"

out="$(run list --help 2>&1)"; rc=$?
wantrc "list --help: refused" 1 "$rc"
no_mailbox_created "--help" "list --help"

out="$(run unread-age --from 2>&1)"; rc=$?
wantrc "unread-age --from: refused" 1 "$rc"
no_mailbox_created "--from" "unread-age --from"

out="$(run read --bead msgid 2>&1)"; rc=$?
wantrc "read --bead: refused" 1 "$rc"
no_mailbox_created "--bead" "read --bead"

out="$(run done --box msgid 2>&1)"; rc=$?
wantrc "done --box: refused" 1 "$rc"
no_mailbox_created "--box" "done --box"

out="$(printf 'body\n' | run send --subject "Hi" --from "A <a@a>" 2>&1)"; rc=$?
wantrc "send --subject: refused" 1 "$rc"
no_mailbox_created "--subject" "send --subject"

out="$(run ensure --to 2>&1)"; rc=$?
wantrc "ensure --to: refused" 2 "$rc"
no_mailbox_created "--to" "ensure --to"

out="$(run tidy --all 2>&1)"; rc=$?
wantrc "tidy --all: refused" 1 "$rc"
no_mailbox_created "--all" "tidy --all"

out="$(run sweep-dismissed --kind 2>&1)"; rc=$?
wantrc "sweep-dismissed --kind: refused" 1 "$rc"
no_mailbox_created "--kind" "sweep-dismissed --kind"

echo
echo "=== a read verb on a mailbox that was never sent to fails loudly, not zero ==="

# POSITIVE CONTROL: the mailbox genuinely does not exist yet — before this fix, each of
# these silently created it and reported empty (rc 0) instead of refusing.
out="$(run count neverexisted 2>&1)"; rc=$?
wantrc "count on absent mailbox: refused" 1 "$rc"
no_mailbox_created "neverexisted" "count on absent mailbox"

out="$(run list neverexisted 2>&1)"; rc=$?
wantrc "list on absent mailbox: refused" 1 "$rc"

out="$(run unread-age neverexisted 2>&1)"; rc=$?
wantrc "unread-age on absent mailbox: refused" 1 "$rc"

out="$(run read neverexisted 2>&1)"; rc=$?
wantrc "read on absent mailbox: refused" 1 "$rc"

echo
echo "=== send and ensure still create a mailbox on demand ==="

printf 'body\n' | run send freshbox --from "A <a@a>" --subject "Hi" >/dev/null 2>&1
[ -d "$SPIRA_MAIL/freshbox" ] && ok "send: creates the mailbox on demand" \
                              || bad "send: creates the mailbox on demand" "not created"
out="$(run count freshbox 2>&1)"; rc=$?
wantrc "count after send: now succeeds" 0 "$rc"
is    "count after send: one unread"    "1" "$out"

run ensure ensured-box >/dev/null 2>&1
[ -d "$SPIRA_MAIL/ensured-box" ] && ok "ensure: creates the mailbox on demand" \
                                  || bad "ensure: creates the mailbox on demand" "not created"

tl_summary
