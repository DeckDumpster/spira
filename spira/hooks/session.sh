#!/usr/bin/env bash
#
# session.sh — the coding agent's SessionStart hook: what is watching, what is unread,
# unread mail, and — on the concierge socket only — the mandatory first action to arm the
# inbox-triage.sh Monitor. Registered by `release session-hook install`.
#
# WHY A HOOK CAN ONLY PRINT. A command hook communicates with the client through stdout,
# stderr and an exit code only — it cannot call a tool. The OUTER HARNESS owns the watcher
# processes; systemd keeps them alive.
#
# IT PRINTS A SUMMARY, NEVER A REPLAY. Everything here lands in a context window that has
# just opened, which is the most expensive place text in this harness can go. One line per
# watcher — name, health, unit state, unread count — and nothing else; the session re-attaches
# with `watchd.sh tail <name>`, which replays from its cursor anyway, so an event preview here
# would be paid twice.
#
# NO WATCHERS MEANS NO OUTPUT. This is registered in the client's own settings file, so it
# runs in every session on the box whatever repository that session is in. A harness with an
# empty manifest therefore prints nothing at all — a banner in every session for a thing the
# operator does not use is exactly the noise this replaced.
#
# IT ALWAYS EXITS 0. A SessionStart hook that exits non-zero is surfaced to the user as a
# hook error, and a watcher summary is never worth a broken session start.
#
# THIS DIRECTORY ALSO SERVES AS git's `core.hooksPath`, which is why this file has a suffix
# and the git hooks beside it do not: git resolves a hook by its exact name, so anything named
# after one — `pre-commit`, `pre-push` — becomes a git hook whatever it was written for. Give a
# client hook a `.sh` name and the two cannot collide.
set -uo pipefail

# AN AEON MUST NEVER SEE THIS OUTPUT. aeon.sh exports SPIRA_AEON into every session it
# summons, and this hook fires for every Claude session on the box. An aeon that sees the
# latch block obeys it — it attaches a persistent Monitor, which holds its session open for
# the timeout after the work is done, blocking landing for as long as the lease is held.
# Worse, the Monitor's cursor advances as it reads, consuming the operator's verdicts and
# marking them delivered to a session that cannot act on them.
[ -n "${SPIRA_AEON:-}" ] && exit 0

# RESOLVED FROM THIS FILE, NEVER FROM THE WORKING DIRECTORY. A session starts in whatever
# repository the operator is in, so cwd says nothing about where the harness is; conf.sh
# derives SPIRA_HOME from its own location, which is the only stable answer. It is also why
# the registered command must be an absolute path.
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)/conf.sh"

# NOR MUST A HARNESS THAT IS NOT IN FORCE. This hook is registered in the CLIENT's settings
# file, so it runs in every session on the box — and a second harness checkout that registers
# itself gets its banner printed into every one of the operator's real sessions alongside the
# real harness's. That happened: a second checkout kept for testing had registered itself on
# SessionStart and PostCompact, so each context reset printed two watcher tables and offered
# two sets of Monitor commands, the second pointing at fixture runtime paths whose state files
# do not exist — the duplicate-reader defect this file was just fixed for, arriving by a route
# the reader lock cannot see because the two harnesses have separate runtime directories.
#
# SPIRA_PROD IS THE HARNESS systemd ExecStarts FROM, which is the definition of in force. A
# checkout that is not it may be tested, landed and read; it may not print into the operator's
# sessions. Silent, because a second harness is a legitimate thing to have and a warning in
# every session is the noise this whole file exists to have replaced.
_prod="$(readlink -f "${SPIRA_PROD:-}" 2>/dev/null || printf '%s' "${SPIRA_PROD:-}")"
_home="$(readlink -f "${SPIRA_HOME:-}" 2>/dev/null || printf '%s' "${SPIRA_HOME:-}")"
[ -n "$_prod" ] && [ "$_home" != "$_prod" ] && exit 0

# THE PAYLOAD IS READ ONLY IF SOMETHING SENT ONE. The client pipes a JSON object in; a person
# running this by hand has a terminal on stdin, and a bare `cat` there blocks forever, holding
# the session start open until the hook's timeout expires.
payload=""
[ -t 0 ] || payload="$(cat 2>/dev/null || true)"
event="$(printf '%s' "$payload" | python3 -c 'import json,sys
try: print(json.load(sys.stdin).get("hook_event_name",""))
except Exception: print("")' 2>/dev/null)"

# SessionEnd has nothing to say and nowhere to say it — the context it would print into is
# the one going away. It is handled rather than refused so that registering it is harmless,
# but `release session-hook install` deliberately does not register it: with systemd owning the
# watchers there are no processes for a departing session to guarantee. Checked before the
# concierge block below too — a departing session gets no arm instruction either.
[ "$event" = SessionEnd ] && exit 0

# THE CONCIERGE'S MANDATORY FIRST ACTION, ON EVERY SessionStart SOURCE — startup, resume,
# clear, compact and fork all open a context with no Monitor attached, which is the only
# condition this is about (same reasoning as the "no source is special" comment below). It
# does not wait on WATCHD or a manifest: the inbox-triage.sh Monitor is how mail and watcher
# events reach this session AT ALL now that a keystroke wake no longer does, so it must stay
# constantly attached. SPIRA_CONCIERGE is exported only by concierge.sh's own launcher, so
# every other session on the box gets none of this.
if [ -n "${SPIRA_CONCIERGE:-}" ]; then
    _cinbox="$SPIRA_CONCIERGE_INBOX"
    _cunread="$(wc -l < "$_cinbox" 2>/dev/null || echo 0)"
    case "$_cunread" in *[!0-9]*|'') _cunread=0 ;; esac
    printf 'MANDATORY FIRST ACTION: arm the Concierge inbox monitor before anything else — Monitor command=%s, timeout_ms=1800000, description="concierge inbox (triaged)". Every watcher and mail event reaches you ONLY through %s (%s lines); nothing types into the pane. Re-arm it at every 30-minute expiry. Then read recent inbox lines: tail -20 %s\n' \
        "$(command -v inbox-triage.sh || printf inbox-triage.sh)" "$_cinbox" "$_cunread" "$_cinbox"
fi

WATCHD=watchd.sh
MAIL=mail.sh

# FIRE THE ARCHIVIST ON CLEAR. A clear starts a new session while the previous transcript is
# still on disk. The turns between the last drift sweep and now are uncovered; this catches
# them. `archivist now` without an argument picks the most recently written non-empty
# transcript, which at the instant of a clear is the one being discarded — the new session has
# not yet written anything. Fire and forget: the state file is where the result is read, and
# the new session must not wait on the old one's archive.
source="$(printf '%s' "$payload" | python3 -c 'import json,sys
try: print(json.load(sys.stdin).get("source",""))
except Exception: print("")' 2>/dev/null)"
if [ "$source" = "clear" ]; then
    archivist now </dev/null >/dev/null 2>&1 &
fi

# RECORD THE CONCIERGE'S OWN SESSION ID. The launcher sets SPIRA_CONCIERGE=1 so that no other
# brain session's context reset overwrites the file. Every clear, compact or fork mints a new
# session id; this hook is the only place the client names it reliably.
if [ -n "${SPIRA_CONCIERGE:-}" ]; then
    _hook_record="$(printf '%s' "$payload" | python3 -c '
import json,sys
try:
    d = json.load(sys.stdin)
    print(d.get("session_id",""))
    print(d.get("cwd",""))
except Exception: pass
' 2>/dev/null)"
    _csid="$(printf '%s\n' "$_hook_record" | sed -n '1p')"
    _ccwd="$(printf '%s\n' "$_hook_record" | sed -n '2p')"
    if [ -n "$_csid" ] && [ -n "$_ccwd" ]; then
        mkdir -p "$SPIRA_RUN" 2>/dev/null
        _cf="$SPIRA_RUN/concierge-session"
        # A THIRD LINE, NEVER OVERWRITTEN BLINDLY. concierge.sh's own fallback-fresh path
        # (sp-aaew9) never deletes this file before a client can record a new id over it, so
        # whatever id was here before this write is about to be replaced — carry it forward as
        # `previous:` rather than let it vanish, so recovering a session this hook just
        # replaced is one `claude --resume` away instead of a grep through old transcripts.
        _cprev="$(sed -n '1p' "$_cf" 2>/dev/null)"
        {
            printf '%s\n%s\n' "$_csid" "$_ccwd"
            [ -n "$_cprev" ] && [ "$_cprev" != "$_csid" ] && printf 'previous:%s\n' "$_cprev"
        } > "$_cf" 2>/dev/null || true
    fi
fi

# THE SOURCE IS NOT BRANCHED ON FOR THE STATUS OUTPUT, and that is deliberate. A SessionStart
# carries a `source` of `startup`, `resume`, `clear`, `compact` or `fork`, and every one of
# them is a context window that has just opened with no Monitor attached — which is the only
# condition the output below is about. Branching on it could only ever print less on some of
# them. The archivist fire above is a separate concern from what this hook prints.
status="$("$WATCHD" status 2>/dev/null)" || status=""

# `status` PRINTS SEVERAL THINGS AND ONLY THE FIRST IS A TABLE: one line per watcher under a
# header, then a blank line and any further block it has to add — DEGRADED when a probe
# failed, NOT INSTALLED when a row names something this installation has not configured. So
# the table is read as the region BETWEEN the header and the first blank line, and never as
# "everything after line one" — taking the tail of the whole output would sweep the `DEGRADED`
# heading and its `  <name>: <why>` lines in as rows, printing an unconfigured or blind watcher
# as one more entry in the table it is actually a call-out about.
rows="$(printf '%s\n' "$status" | awk 'NR==1 { next } /^[[:space:]]*$/ { exit } { print }')"
[ -n "$rows" ] || exit 0

# ONE LINE FOR THE SESSION MAILBOX — count only; marks nothing read; no Monitor instruction.
mail_line=""
if [ -n "${SPIRA_MAIL_SESSION_MAILBOX:-}" ]; then
    _mn="$("$MAIL" count "$SPIRA_MAIL_SESSION_MAILBOX" 2>/dev/null)" || _mn=0
    case "${_mn:-}" in *[!0-9]*|"") _mn=0 ;; esac
    if [ "$_mn" -gt 0 ]; then
        mail_line="You have $_mn unread messages — $MAIL list $SPIRA_MAIL_SESSION_MAILBOX --unread"
    fi
fi

# ONE LINE PER WATCHER, AND NOTHING ELSE. A fresh context window is the
# most expensive place text can go, and the session re-attaches with `watchd.sh tail <name>`,
# which replays everything from its cursor anyway — so a preview of unread events here is paid
# twice. Each line is the watcher, its health, and its unread count; a DEGRADED one carries
# the reason `status` gave, because that is the half that says what to do. No peek, no
# prose. Scar: after one compaction this hook printed 38 lines of watcher backlog and a
# 409-line summary into a fresh context.
reason() { printf '%s\n' "$status" | awk -v n="$1" '/^DEGRADED$/ {f=1; next} f && /^[[:space:]]*$/ {exit} f { sub(/^[[:space:]]+/, ""); if (index($0, n": ") == 1) { print substr($0, length(n) + 3); exit } }'; }
echo "## Spira watchers — re-attach with: $WATCHD tail <name>"
printf '%s\n' "$rows" | while read -r name unit health unread _; do
    line="$name $health ($unit), $unread unread"
    [ "$health" = DEGRADED ] && line="$line — $(reason "$name")"
    printf '%s\n' "$line"
done
[ -n "$mail_line" ] && printf '%s\n' "$mail_line"
exit 0
