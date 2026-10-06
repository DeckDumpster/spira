#!/usr/bin/env bash
#
# test-concierge-inbox.sh — the durable inbox that replaced keystroke wakes into the
#   concierge pane: inbox-append.sh, inbox-triage, the inbox-keeper watchd row, and
#   SPIRA_MAIL_READERS defaulting the concierge mailbox to it.
#
# WHAT THIS REPLACED. Every watcher and mail-deliver used to wake the concierge with a
# tmux keystroke (`concierge.sh wake`), which could land mid-keystroke inside the
# operator's own half-written message. The mechanism here decouples "an event happened"
# (append a line) from "the pane gets touched" (only the keeper, and only a re-arm, held
# until the input line is empty — see the wake-hold tests in test-concierge.sh).
#
# A CHECK THAT FINDS NOTHING MUST FIRST PROVE IT COULD HAVE FOUND SOMETHING
# (law-absence-needs-a-positive-control): the drop/dedup section plants a line each of
# the three drop patterns and a duplicate before trusting the triage's silence on them.
#
# EVERY CONFIGURED VALUE IS PINNED TO A NON-DEFAULT, so a literal written into the
# mechanism cannot pass by coincidence.
#
# covers: spira/inbox-append.sh inbox-triage/* spira/inbox-keeper.sh spira/conf.sh spira/watchers watchd/*
# tier: T1
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

export SPIRA_HOME="$HERE"
export SPIRA_CONF=""
# THE LEGACY .conf MAY NOT BE READ (SPIRA_CONF="" blocks it). Registered config now comes
# only from SPIRA_TOML — testlib.sh's complete fixture plus this suite's own override file,
# declared below via tl_config — so there is no env fallback left that could leak the
# operator's real ~/.config/spira/spira.toml.
SPIRA_CONCIERGE_INBOX="$TMP/elsewhere/inbox.log"
tl_config SPIRA_RUN="$TMP/elsewhere/run" SPIRA_CONCIERGE_INBOX="$SPIRA_CONCIERGE_INBOX" \
    SPIRA_CONCIERGE_INBOX_DEDUP=2

echo "inbox-append.sh — one line per call, at the configured (non-default) path"
is "the inbox does not exist yet" "0" "$([ -f "$SPIRA_CONCIERGE_INBOX" ] && echo 1 || echo 0)"
inbox-append.sh "hello world"
want "a line lands with a UTC timestamp"  "hello world" "$(cat "$SPIRA_CONCIERGE_INBOX")"
want "and an ISO-8601 Z timestamp prefix" "T" "$(head -1 "$SPIRA_CONCIERGE_INBOX")"
n="$(wc -l < "$SPIRA_CONCIERGE_INBOX")"
is  "exactly one line" "1" "$n"
inbox-append.sh "second event"
n="$(wc -l < "$SPIRA_CONCIERGE_INBOX")"
is  "a second call appends, never truncates" "2" "$n"

echo
echo "SPIRA_MAIL_READERS: an explicit override still wins over the fixture default"
# DELETED: the "nothing set -> code default" case. Under the one-source-of-config law
# there is no env-read default left to observe (SPIRA_MAIL_READERS, like every registered
# key, now resolves from SPIRA_TOML alone); the complete fixture supplies its own baseline
# value, so "env -u SPIRA_MAIL_READERS" no longer exercises a fallback path at all.
tl_config SPIRA_MAIL_READERS="concierge=echo wake"
overridden="$(bash -c '. "'"$HERE"'/conf.sh"; printf %s "$SPIRA_MAIL_READERS"')"
is   "an explicit SPIRA_MAIL_READERS is never overwritten" "concierge=echo wake" "$overridden"

echo
echo "the inbox-keeper watchd row — a harness watchd row, not an operator overlay file"
want "spira/watchers carries an inbox-keeper daemon row" \
    "inbox-keeper|daemon|inbox-keeper.sh" "$(cat "$HERE/watchers")"
MAN="$TMP/elsewhere/manifest-check"
tl_config SPIRA_WATCHERS_OVERLAY="$TMP/elsewhere/no-overlay"
watchd manifest \
    > "$MAN" 2>"$TMP/elsewhere/manifest.err"; rc=$?
is   "the shipped manifest, alone, still parses" "0" "$rc"
want "and names inbox-keeper as a daemon row" "inbox-keeper|daemon" "$(cat "$MAN")"

echo
echo "inbox-triage — drops its own known noise, dedups, passes the rest"
# THE POSITIVE CONTROL FIRST: an ordinary line must pass, so the drops proven below are
# about the pattern and not about the fixture being unable to pass anything at all.
: > "$SPIRA_CONCIERGE_INBOX"
OUT="$TMP/elsewhere/triage.out"
inbox-triage > "$OUT" 2>/dev/null &
TRIAGE_PID=$!
trap 'kill "$TRIAGE_PID" 2>/dev/null; rm -rf "$TMP"' EXIT INT TERM

# Positive evidence, never a fixed sleep: inbox-triage starts at the inbox's current end,
# so a line appended before it has attached is lost. Probe until one comes out.
wait_until() {  # wait_until <seconds> <command...>
    local end=$((SECONDS + $1)); shift
    until "$@"; do [ "$SECONDS" -ge "$end" ] && return 1; sleep 0.1; done
}
out_has() { grep -qF -- "$1" "$OUT"; }
i=0
until out_has "attach-probe"; do
    i=$((i + 1)); [ "$i" -gt 300 ] && break
    inbox-append.sh "attach-probe $i"; sleep 0.1
done
sleep 0.5
: > "$OUT"

inbox-append.sh "a decision needs a bead: sp-example wants review"
inbox-append.sh "ROUND RESULT: nothing to do"
inbox-append.sh "watcher x OPENED a new pane"
inbox-append.sh "pool: NEW CERTIFIED tip abc123"
inbox-append.sh "end-marker"
wait_until 30 out_has "end-marker"

want "an ordinary event passes through (positive control)" "a decision needs a bead" "$(cat "$OUT")"
nowant "ROUND RESULT is dropped" "ROUND RESULT" "$(cat "$OUT")"
nowant "an OPENED line is dropped"  " OPENED "     "$(cat "$OUT")"
nowant "a pool NEW CERTIFIED line is dropped" "pool: NEW CERTIFIED" "$(cat "$OUT")"

: > "$OUT"
inbox-append.sh "escalation: needs a decision"
sleep 0.3
inbox-append.sh "escalation: needs a decision"
inbox-append.sh "end-marker-2"
wait_until 30 out_has "end-marker-2"
n="$(grep -c "escalation: needs a decision" "$OUT")"
is "a repeat within the dedup window is suppressed" "1" "$n"

: > "$OUT"
sleep 2.5   # past SPIRA_CONCIERGE_INBOX_DEDUP=2
inbox-append.sh "escalation: needs a decision"
wait_until 30 out_has "escalation: needs a decision"
n="$(grep -c "escalation: needs a decision" "$OUT")"
is "the same text past the dedup window passes again" "1" "$n"

kill "$TRIAGE_PID" 2>/dev/null; wait "$TRIAGE_PID" 2>/dev/null || true
trap 'rm -rf "$TMP"' EXIT INT TERM

tl_summary
