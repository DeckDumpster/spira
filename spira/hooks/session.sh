#!/usr/bin/env bash
#
# session.sh — the coding agent's SessionStart hook: what is watching, what is unread,
# and unread mail. Registered by `install-session-hook.sh`.
#
# WHY A HOOK CAN ONLY PRINT. A command hook communicates with the client through stdout,
# stderr and an exit code only — it cannot call a tool. The OUTER HARNESS owns the watcher
# processes; systemd keeps them alive.
#
# IT PRINTS A SUMMARY, NEVER A REPLAY. Everything here lands in a context window that has
# just opened, which is the most expensive place text in this harness can go. An earlier
# version of this idea told a real session "there are 283 unread events, replay them with
# <drain>", which would have put 283 raw lines into the first screen of a fresh session. So
# the output is held to SPIRA_HOOK_LINES, and held by MEASUREMENT rather than by estimate:
# the table and the latch commands are assembled first and the preview is given exactly what
# is left.
#
# AND IT PEEKS RATHER THAN DRAINS. `drain` marks what it prints as read; under a line budget
# that would destroy every event there was no room for, and it would do it precisely when
# there are most of them. `peek` records nothing, so the latch that follows still replays the
# entire backlog.
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
[ -n "${SPIRA_PROD:-}" ] && [ "${SPIRA_HOME:-}" != "$SPIRA_PROD" ] && exit 0

WATCHD="$SPIRA_HOME/watchd.sh"
[ -x "$WATCHD" ] || exit 0
MAIL="$SPIRA_HOME/mail.sh"

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
# but `install-session-hook.sh` deliberately does not register it: with systemd owning the
# watchers there are no processes for a departing session to guarantee.
[ "$event" = SessionEnd ] && exit 0

# FIRE THE ARCHIVIST ON CLEAR. A clear starts a new session while the previous transcript is
# still on disk. The turns between the last drift sweep and now are uncovered; this catches
# them. `archivist.sh now` without an argument picks the most recently written non-empty
# transcript, which at the instant of a clear is the one being discarded — the new session has
# not yet written anything. Fire and forget: the state file is where the result is read, and
# the new session must not wait on the old one's archive.
source="$(printf '%s' "$payload" | python3 -c 'import json,sys
try: print(json.load(sys.stdin).get("source",""))
except Exception: print("")' 2>/dev/null)"
if [ "$source" = "clear" ] && [ -x "$SPIRA_HOME/archivist.sh" ]; then
    "$SPIRA_HOME/archivist.sh" now </dev/null >/dev/null 2>&1 &
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
        printf '%s\n%s\n' "$_csid" "$_ccwd" > "$SPIRA_RUN/concierge-session" 2>/dev/null || true
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
# "everything after line one", and every block below it is read by its own heading.
#
# The difference is not cosmetic. When the latch block was generated from field one of these
# lines, taking the tail of the whole output swept the `DEGRADED` banner and each
# `  <name>: <why>` line in as rows, and emitted `watchd.sh tail DEGRADED` and
# `watchd.sh tail answers:` — latch commands naming watchers that do not exist, in the one
# block whose whole purpose is to be pasted and run. The block is generated from `manifest`
# now, for the reason given where it is read; the table's own boundary still matters, because
# what is printed as the table is taken from here.
header="$(printf '%s\n' "$status" | head -n 1)"
rows="$(printf '%s\n' "$status" | awk 'NR==1 { next } /^[[:space:]]*$/ { exit } { print }')"
[ -n "$rows" ] || exit 0

# ONE LINE FOR THE SESSION MAILBOX — count only; marks nothing read; no Monitor instruction.
mail_line=""
if [ -x "$MAIL" ] && [ -n "${SPIRA_MAIL_SESSION_MAILBOX:-}" ]; then
    _mn="$("$MAIL" count "$SPIRA_MAIL_SESSION_MAILBOX" 2>/dev/null)" || _mn=0
    case "${_mn:-}" in *[!0-9]*|"") _mn=0 ;; esac
    if [ "$_mn" -gt 0 ]; then
        mail_line="You have $_mn unread messages — $MAIL list $SPIRA_MAIL_SESSION_MAILBOX --unread"
    fi
fi

# THE DEGRADED SECTION IS TAKEN FROM `status`, NOT RE-DERIVED FROM THE TABLE. A watcher
# reading the wrong database is silent in exactly the way a watcher with nothing to say is
# silent, so the one fact that separates them is said in its own right rather than left in a
# column to be noticed (law-alerts-must-be-actionable). `status` already lifts it out AND
# carries the reason the probe gave; re-deriving it from the row would reproduce the word
# without the why, which is the half that says what to do next.
#
# AND IT ENDS AT THE BLANK LINE, because DEGRADED is not the last section. `status` prints a
# further block naming watchers this installation has not configured — a fact, not a fault —
# and taking "everything after the word DEGRADED" swept that heading and its rows into the
# call-out, reporting an unconfigured watcher as running and blind. An alert that is not
# actionable is the thing that makes the actionable ones unreadable, and this is the one
# section here whose entire value is that everything in it is a fault.
degraded="$(printf '%s\n' "$status" | awk '/^DEGRADED$/ { f=1; next } f && /^[[:space:]]*$/ { exit } f')"

budget="${SPIRA_HOOK_LINES:-40}"
case "$budget" in ''|*[!0-9]*) budget=40 ;; esac

TMP="$(mktemp -d 2>/dev/null)" || exit 0
trap 'rm -rf "$TMP"' EXIT

# THE TABLE IS REPRINTED FROM ITS PARTS, not echoed whole, so that the DEGRADED section
# appears exactly once and appears with the sentence that says what it means. Echoing
# `$status` and then adding a call-out printed the same thing twice.
{   echo "## Spira watchers"
    echo
    printf '%s\n' "$header"
    printf '%s\n' "$rows"
    echo
    if [ -n "$degraded" ]; then
        echo "DEGRADED — running and blind. Silence from these is not good news:"
        printf '%s\n' "$degraded"
        echo
    fi
    if [ -n "$mail_line" ]; then
        printf '%s\n' "$mail_line"
        echo
    fi
} > "$TMP/head"

: > "$TMP/tail"

# THE PREVIEW GETS WHAT IS LEFT, MEASURED. The table is printed whole — a watcher hidden to
# save a line is a watcher nobody knows exists — so the elastic section is the preview only.
allowance=0
if [ "$budget" = 0 ]; then
    allowance=-1                                    # no budget; peek is uncapped
else
    allowance=$(( budget - $(wc -l < "$TMP/head") - $(wc -l < "$TMP/tail") - 1 ))
    [ "$allowance" -lt 0 ] && allowance=0
fi

if [ "$allowance" != 0 ]; then
    # A FIRST PASS BOUNDED BY THE WHOLE BUDGET, so that a watcher holding tens of thousands of
    # actionable lines is never read in full merely to discover it is too long. No watcher can
    # contribute more than the budget, so this is already a superset of anything printable —
    # and it is what says how many watchers have something to report, which is the divisor the
    # per-watcher cap needs.
    cap=0; [ "$allowance" -gt 0 ] && cap="$budget"
    "$WATCHD" peek --limit "$cap" > "$TMP/peek" 2>/dev/null || : > "$TMP/peek"
    nsec="$(grep -c '^=== ' "$TMP/peek" || true)"

    # THE CAP IS SHARED OUT PER WATCHER RATHER THAN TAKEN OFF THE END, because `peek` keeps
    # each watcher's MOST RECENT lines and a trailing trim would keep its oldest — the exact
    # inversion of what a reader wants from a backlog. Two lines per watcher are reserved for
    # its section header and the note naming what it withheld.
    if [ "$allowance" -gt 0 ] && [ "$nsec" -gt 0 ] \
       && [ "$(wc -l < "$TMP/peek")" -gt "$allowance" ]; then
        per=$(( (allowance - 2 * nsec) / nsec ))
        [ "$per" -lt 1 ] && per=1
        "$WATCHD" peek --limit "$per" > "$TMP/peek" 2>/dev/null || : > "$TMP/peek"
    fi

    # AND A LAST GUARD, for the case the arithmetic above cannot solve: enough watchers with
    # something to say that even one line each overflows. The budget is a promise, so it is
    # kept by measurement rather than by the estimate that produced `per`.
    if [ "$allowance" -gt 0 ] && [ "$(wc -l < "$TMP/peek")" -gt "$allowance" ]; then
        if [ "$allowance" -gt 1 ]; then
            head -n $(( allowance - 1 )) "$TMP/peek" > "$TMP/peek.trim"
            printf '    ... trimmed to fit the %s-line budget; nothing was marked read\n' "$budget" >> "$TMP/peek.trim"
            mv "$TMP/peek.trim" "$TMP/peek"
        else
            : > "$TMP/peek"
        fi
    fi
    [ -s "$TMP/peek" ] && { cat "$TMP/peek"; echo; } >> "$TMP/head"
fi

cat "$TMP/head" "$TMP/tail"
exit 0
