#!/usr/bin/env bash
#
# cockpit.sh — make the cockpit exist. Run it from anywhere, in any state, as often as you like.
#
#   cockpit.sh            build or repair the cockpit; safe when it is already fine
#   cockpit.sh status     say what is there and change nothing
#   cockpit.sh --force    also clear a wedged tmux server that still holds live panes
#
# WHY THIS IS AT THE ROOT. Everything below already existed — `cockpit/rebuild.sh` does the
# whole job and does it well. The problem was finding it. When the server dies the operator is
# looking for the thing that CREATES a cockpit, and the script that creates one is called
# "rebuild" and lives one directory down next to eleven others; `layout.sh up` is the obvious
# guess and it is the wrong one, because its first line is `tmux has-session || exit 1` — it
# repairs a cockpit and cannot make one. So the recovery got assembled by hand, in the right
# order, under time pressure, twice.
#
# A control nobody can find is not a control. This file is the name you would guess, in the
# place you would look, and it is idempotent so there is never a question of whether it is
# safe to run — which is the other half of why the recovery was slow.
#
# IT PICKS BETWEEN TWO TOOLS, WHICH IS THE PART THAT WAS NOT WRITTEN DOWN.
#
#   cockpit/rebuild.sh   makes a cockpit from nothing, including after the server dies.
#   cockpit/layout.sh    ensure   heals one that is already up, touching nothing that is fine.
#
# Reaching for the wrong one is not harmless in either direction. `layout.sh` cannot create:
# its first line is `tmux has-session || exit 1`. And `rebuild.sh` on a HEALTHY cockpit runs
# `layout.sh up`, which RESPAWNS the dashboard panes rather than leaving them — measured here:
# two runs in a row moved the panel and health panes from %4/%3 to %7/%6. Nothing is lost, but
# both dashboards restart, which is not what "run it again to be safe" should do.
#
# So: check first, then heal or rebuild. `layout.sh ensure` is the idempotent path and is used
# whenever the cockpit is structurally intact — verified by running it against a healthy
# cockpit and confirming the pane ids did not move.
#
# WHY THE CHECK IS HERE AND NOT A CALL TO `layout.sh ensure`. `ensure` exits 0 whether it healed
# a cockpit or found no session at all, so its exit code cannot decide anything.
#
# THE CONCIERGE IS PART OF "THE COCKPIT" AND LIVES ON A DIFFERENT SOCKET. concierge.sh runs
# `tmux -L <socket>`, so the concierge session is invisible to `tmux list-sessions` and to
# every check in rebuild.sh — which is why rebuilding the cockpit used to leave the operator
# to start it by hand, having just run the one command that was supposed to restore
# everything. `concierge.sh start` is idempotent and scrubs its own environment, so it is
# simply run at the end of both paths.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REBUILD="$HERE/cockpit/rebuild.sh"
[ -r "$REBUILD" ] || { echo "cockpit: no rebuild.sh at $REBUILD" >&2; exit 1; }

MODE=build; FORCE=0
for a in "$@"; do
    case "$a" in
        status|probe|--status|--probe) MODE=probe ;;
        --force)                       FORCE=1 ;;
        -h|--help) sed -n '2,7p' "$0" | sed 's/^# \?//'; exit 0 ;;
        *) echo "cockpit: unknown argument '$a'" >&2; exit 64 ;;
    esac
done

if [ "$MODE" = probe ]; then exec bash "$REBUILD" probe; fi

# ------------------------------------------------------------------------------------------
# THE ONE GUARD THIS ADDS: DO NOT LET THE OPERATOR KILL THE PANE THEY ARE SITTING IN.
#
# rebuild.sh refuses to clear a wedged server that still holds live panes, and `--force` is
# the operator saying they have looked. But "I have looked" and "I am not inside it" are
# different claims, and only the first one is being made. Run `cockpit.sh --force` from a pane
# OF the wedged server and the kill takes this script's own shell with it: the command dies
# mid-run, having killed the thing it was repairing, and what the operator sees is a terminal
# that vanished for no stated reason.
#
# $TMUX is set inside a tmux pane and carries the socket path of the server that owns it.
# Comparing it to the socket rebuild.sh would act on is the whole test. This is refused rather
# than warned about, because the failure is silent and immediate and there is a trivial
# alternative: run it from a terminal that is not tmux, or detach first.
# ------------------------------------------------------------------------------------------
if [ "$FORCE" = 1 ] && [ -n "${TMUX:-}" ]; then
    _ck_here="${TMUX%%,*}"                       # $TMUX is <socket>,<pid>,<session>
    _ck_target="$(tmux display-message -p '#{socket_path}' 2>/dev/null || true)"
    [ -n "$_ck_target" ] || _ck_target="${TMUX_TMPDIR:-/tmp}/tmux-$(id -u)/default"
    if [ "$_ck_here" = "$_ck_target" ]; then
        echo "cockpit: refusing --force from inside the server it would clear." >&2
        echo "         This shell is a pane of $_ck_here; killing it kills this command." >&2
        echo "         Detach (prefix-d) and re-run, or run it from a terminal outside tmux." >&2
        exit 3
    fi
fi

# --- is the cockpit structurally there? ----------------------------------------------------
# The sessions come from COCKPIT_SESSIONS so this cannot disagree with rebuild.sh, which reads
# the same key. The FIRST of them is the one holding the dashboards, and `cockpit` is the
# session that links them.
{ set +u; . "$HERE/spira/conf.sh" 2>/dev/null; set -u; } || true
SESSIONS="${COCKPIT_SESSIONS:-brain hunk chat}"
DASH_SESSION="${SESSIONS%% *}"

cockpit_intact() {
    tmux list-sessions >/dev/null 2>&1 || return 1
    local s
    for s in $SESSIONS cockpit; do
        tmux has-session -t "=$s" 2>/dev/null || return 1
    done
    # Both dashboards must be TAGGED AND PRESENT. A window with three panes but no @cockpit
    # tags is a split somebody made by hand, not a cockpit, and healing it is the right move.
    local tags; tags="$(tmux list-panes -t "${DASH_SESSION}:0" -F '#{@cockpit}' 2>/dev/null)"
    printf '%s\n' "$tags" | grep -qx panel  || return 1
    printf '%s\n' "$tags" | grep -qx health || return 1
    return 0
}

# The concierge session, on its own `tmux -L` socket. Idempotent: it prints "already running"
# and changes nothing when it is up. Run on both paths, because a cockpit without it is the
# state the operator just had to fix by hand.
ensure_concierge() {
    local c="$HERE/concierge.sh"
    [ -x "$c" ] || { echo "  concierge: no concierge.sh at $c" >&2; return 0; }
    bash "$c" start 2>&1 | sed 's/^/  /'
}

if [ "$FORCE" != 1 ] && cockpit_intact; then
    echo "cockpit: already up — healing in place (no panes respawned)"
    bash "$HERE/cockpit/layout.sh" ensure 2>&1 | sed 's/^/  /'
    ensure_concierge
    echo "cockpit: attach with  tmux attach -t cockpit"
    exit 0
fi

# Not intact (or --force): rebuild.sh is the one that can make a cockpit from nothing. It also
# scrubs the session-identity variables before forking a server — a tmux server keeps the
# environment it was started with for the life of every pane it ever opens — which is the
# other reason this delegates rather than reimplementing the steps.
_ck_args=()
[ "$FORCE" = 1 ] && _ck_args+=(--force)
bash "$REBUILD" "${_ck_args[@]}"; _ck_rc=$?
# Not `exec`: the concierge still has to be started, and it is the half of "the cockpit" that
# rebuild.sh cannot see.
ensure_concierge
exit "$_ck_rc"
