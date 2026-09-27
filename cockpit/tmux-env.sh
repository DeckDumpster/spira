#!/usr/bin/env bash
#
# tmux-env.sh — keep one Claude session's identity out of the tmux server's environment.
#
#   tmux-env.sh names            the variable names, one per line
#   tmux-env.sh scrub [-L sock]  remove them from a live server's global environment
#
# WHY THIS EXISTS
# ---------------
# tmux gives every new pane THE SERVER'S environment, captured once when the server was
# forked and then frozen for the server's whole life. So a tmux server started from inside a
# Claude Code session hands that session's identity to every pane it will ever open — for
# days, across reboots of everything except tmux itself.
#
# On 2026-09-09 the cockpit server (pid 1640264) had been forked by cockpit/rebuild.sh
# running inside an archivist AEON. It captured CLAUDE_CODE_CHILD_SESSION=1, and the
# operator's own brain session — opened in a pane of that server hours later — inherited it.
# A client that believes it is a child session DOES NOT WRITE A TRANSCRIPT. Two things then
# broke at once, and neither error named the cause:
#
#   ⚠ Transcript saving is off — inherited CLAUDE_CODE_CHILD_SESSION marker
#   CTX    no session at the keyboard          (ctx-meter.sh finds no transcript to read)
#
# It also carried a STALE CLAUDE_CODE_SESSION_ID and a CLAUDE_CODE_EXECPATH pinned to a
# client version that had since been replaced, so the operator's session reported the aeon's
# session id and an older binary's path.
#
# WHY SCRUB RATHER THAN SET. There is no correct value for any of these in a shared server:
# they identify one session, and a pane is not that session. Absent is the only right answer;
# a real client sets its own on startup.
#
# CLAUDE_EFFORT AND THE THEME ARE DELIBERATELY LEFT. They are operator preferences that mean
# the same thing in any pane. Only session IDENTITY is poison.
#
# covers: cockpit/rebuild.sh cockpit/layout.sh concierge.sh
set -uo pipefail

# `scrub` with no -L targets the default socket — but a bare `tmux` prefers $TMUX, the
# server the INVOKING shell is already attached to, over the default. Run this by hand
# from inside a session and it would scrub that session's server, not the cockpit's own.
# Cleared before the first tmux call; an explicit -L/-S in "$@" already outranks $TMUX and
# is unaffected.
unset TMUX

# Session identity, in the order the client sets them. Anything that names ONE session,
# ONE process or ONE installed client belongs here.
VARS="
CLAUDE_CODE_CHILD_SESSION
CLAUDE_CODE_SESSION_ID
CLAUDE_CODE_BRIDGE_SESSION_ID
CLAUDE_CODE_MESSAGING_SOCKET
CLAUDE_CODE_MESSAGING_TOKEN
CLAUDE_CODE_ENTRYPOINT
CLAUDE_CODE_EXECPATH
CLAUDE_PID
CLAUDECODE
AI_AGENT
"

case "${1:-names}" in
    names)
        printf '%s\n' $VARS
        ;;
    scrub)
        shift
        # A server that is not running has no environment to scrub, and that is not an error:
        # the caller is about to fork a clean one.
        tmux "$@" has-session 2>/dev/null || exit 0
        n=0
        for v in $VARS; do
            tmux "$@" show-environment -g "$v" >/dev/null 2>&1 || continue
            tmux "$@" set-environment -gu "$v" 2>/dev/null && n=$((n+1))
        done
        # Silent when there was nothing to do — this runs from a one-minute timer.
        [ "$n" -gt 0 ] && printf 'cockpit: scrubbed %d inherited Claude session variable(s) from the tmux server\n' "$n"
        exit 0
        ;;
    *)
        echo "usage: tmux-env.sh names | scrub [-L <socket>]" >&2
        exit 2
        ;;
esac
