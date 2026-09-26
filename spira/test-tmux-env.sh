#!/usr/bin/env bash
#
# test-tmux-env.sh — cockpit/tmux-env.sh removes an inherited Claude session identity from a
#   tmux server's environment, so panes opened afterwards do not claim to be that session.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control). The whole defect is invisible
# unless you can first SEE a pane inherit the marker: a server forked from a process carrying
# CLAUDE_CODE_CHILD_SESSION=1 hands it to every pane for the server's whole life, and the
# operator's client then silently stops writing a transcript. This suite plants that state on a
# scratch socket and asserts a pane sees it, BEFORE asserting that the scrub takes it away.
#
# defect: sp-51j9b (the cockpit server forked by an archivist aeon on 2026-09-09)
# tier: T1
# covers: cockpit/tmux-env.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
TOOL="$HERE/../cockpit/tmux-env.sh"

echo "test-tmux-env.sh"

[ -f "$TOOL" ] || { printf 'SKIP tmux-env.sh not found at %s\n' "$TOOL"; exit 77; }
command -v tmux >/dev/null || { printf 'SKIP tmux not on PATH\n'; exit 77; }

SOCK="tmuxenv-test-$$"
TM=(tmux -L "$SOCK")
cleanup() { "${TM[@]}" kill-server 2>/dev/null || true; }
trap cleanup EXIT

# ---- names -----------------------------------------------------------------------------
names="$(bash "$TOOL" names)"
case "$names" in
    *CLAUDE_CODE_CHILD_SESSION*) ok "names includes the transcript-killing marker" ;;
    *) bad "names includes the transcript-killing marker" "got [$names]" ;;
esac
[ "$(printf '%s\n' "$names" | grep -c .)" -ge 5 ] \
    && ok "names lists the whole identity set" \
    || bad "names lists the whole identity set" "only $(printf '%s\n' "$names" | grep -c .) name(s)"

# ---- scrub with no server is silent and succeeds ----------------------------------------
out="$(bash "$TOOL" scrub -L "$SOCK" 2>&1)"; rc=$?
[ "$rc" -eq 0 ] && ok "scrub on a dead socket exits 0" \
                || bad "scrub on a dead socket exits 0" "rc=$rc"
[ -z "$out" ]   && ok "scrub on a dead socket is silent" \
                || bad "scrub on a dead socket is silent" "said [$out]"

# ---- POSITIVE CONTROL: a pane inherits the marker ---------------------------------------
# The server is forked by this `new-session`, so it captures the environment set here — the
# same way rebuild.sh running inside an aeon captured the aeon's.
CLAUDE_CODE_CHILD_SESSION=1 \
CLAUDE_CODE_SESSION_ID=aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee \
CLAUDECODE=1 \
    "${TM[@]}" new-session -d -s t 2>/dev/null \
    || { printf 'SKIP could not start a tmux server on %s\n' "$SOCK"; exit 77; }

# What a NEW pane would be given: tmux composes it from the server's global environment.
pane_env() { "${TM[@]}" show-environment -g "$1" 2>/dev/null; }

case "$(pane_env CLAUDE_CODE_CHILD_SESSION)" in
    CLAUDE_CODE_CHILD_SESSION=1) ok "POSITIVE CONTROL — a fresh pane inherits the marker" ;;
    *) printf 'SKIP this tmux does not propagate the caller environment to the server\n'; exit 77 ;;
esac
case "$(pane_env CLAUDE_CODE_SESSION_ID)" in
    *aaaaaaaa-bbbb*) ok "POSITIVE CONTROL — and the stale session id with it" ;;
    *) bad "POSITIVE CONTROL — and the stale session id with it" "got [$(pane_env CLAUDE_CODE_SESSION_ID)]" ;;
esac

# ---- the scrub --------------------------------------------------------------------------
out="$(bash "$TOOL" scrub -L "$SOCK" 2>&1)"; rc=$?
[ "$rc" -eq 0 ] && ok "scrub exits 0" || bad "scrub exits 0" "rc=$rc"
case "$out" in
    *scrubbed*) ok "scrub reports what it removed" ;;
    *) bad "scrub reports what it removed" "said [$out]" ;;
esac

for v in CLAUDE_CODE_CHILD_SESSION CLAUDE_CODE_SESSION_ID CLAUDECODE; do
    if [ -z "$(pane_env "$v")" ]; then ok "$v is gone from the server environment"
    else bad "$v is gone from the server environment" "still [$(pane_env "$v")]"; fi
done

# A pane opened after the scrub must not see it either — this is the property that matters,
# and it is checked through a real pane rather than through show-environment alone.
"${TM[@]}" new-window -d -n after 2>/dev/null
got="$("${TM[@]}" new-window -d -P -F '#{pane_id}' -n probe 2>/dev/null)"
if [ -n "$got" ]; then
    "${TM[@]}" send-keys -t "$got" 'printf "MARK[%s]\n" "${CLAUDE_CODE_CHILD_SESSION:-unset}" > '"/tmp/tmuxenv-$$.out" Enter
    for _ in 1 2 3 4 5 6 7 8 9 10; do [ -s "/tmp/tmuxenv-$$.out" ] && break; sleep 0.3; done
    case "$(cat "/tmp/tmuxenv-$$.out" 2>/dev/null)" in
        *"MARK[unset]"*) ok "a pane opened after the scrub sees no marker" ;;
        *) bad "a pane opened after the scrub sees no marker" "got [$(cat "/tmp/tmuxenv-$$.out" 2>/dev/null)]" ;;
    esac
    rm -f "/tmp/tmuxenv-$$.out"
else
    bad "a pane opened after the scrub sees no marker" "could not open a probe pane"
fi

# ---- idempotent -------------------------------------------------------------------------
out="$(bash "$TOOL" scrub -L "$SOCK" 2>&1)"
[ -z "$out" ] && ok "a second scrub is silent" || bad "a second scrub is silent" "said [$out]"
tl_summary
