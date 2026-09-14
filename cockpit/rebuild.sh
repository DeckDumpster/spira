#!/usr/bin/env bash
#
# rebuild.sh — bring the whole cockpit back from nothing, including after the tmux server dies.
#
#   rebuild.sh probe        say what is wrong and change nothing
#   rebuild.sh              do it: clear a dead server, make the sessions, build the cockpit
#   rebuild.sh --force      also clear a wedged server that still holds live panes
#
# WHY THIS EXISTS. `layout.sh up` REPAIRS a cockpit; it cannot create one. Its first line is
# `tmux has-session || exit 1`, so when the tmux server itself dies there is no session for it
# to repair and the operator is left assembling five steps by hand in the right order. On
# 2026-09-09 the server on the default socket wedged and took the cockpit, both dashboards and
# every Monitor with it; the recovery took a dozen commands and two wrong turns, and the
# ordering matters in ways nothing wrote down.
#
# THE ORDERING THAT MATTERS, which is most of what this script is:
#
#   1. A wedged server must be cleared before anything else. It holds the socket, so every
#      later step fails in a way that looks like a different bug.
#   2. The dashboards go in `brain:0`, NOT in a new `cockpit` window. `cockpit` is not a
#      session that owns windows — it holds LINKED copies of `brain:0` and `hunk:0`, and
#      flipping between wiki work and review is a `select-window` over those links. Build the
#      layout in a fresh cockpit window and you get something that renders correctly, passes a
#      casual look, and is not the cockpit: the view watcher goes DEGRADED because the window
#      it steers is not the window it is looking at.
#   3. So: sessions first, layout into `brain:0`, and only then `cockpit-remote build` to link
#      them. Reverse the last two and the links point at a window with no dashboards in it.
#
# WEDGED IS NOT DEAD, AND THE DIFFERENCE DECIDES WHETHER KILLING IS SAFE. The failure that
# prompted this was a server that was alive, sleeping, holding its listening socket, using 36
# of 1024 fds — and answering every client with an instant EOF. `tmux list-sessions` said
# "server exited unexpectedly" in 11ms while the process sat in `ps`. Neither "is the process
# there" nor "does the socket file exist" tells you anything; the only question that
# discriminates is WHETHER IT ANSWERS, so that is the probe.
#
# AND A SERVER THAT HOLDS LIVE PANES IS NOT THIS SCRIPT'S TO KILL. A wedged server whose panes
# are all gone is a corpse holding a socket and clearing it costs nothing. A wedged server with
# live descendants is somebody's unsaved work — an aeon mid-claim, a review, a shell with
# something in it — and killing that is a decision, not a repair. Refused by default, and
# `--force` is the operator saying they have looked.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd -P)"
FORCE=0; MODE="build"
for a in "$@"; do
    case "$a" in
        probe|--probe)  MODE="probe" ;;
        --force)        FORCE=1 ;;
        -h|--help)      sed -n '2,8p' "$0" | sed 's/^# \?//'; exit 0 ;;
        *) echo "rebuild: unknown argument '$a'" >&2; exit 64 ;;
    esac
done

# Configuration, never a hardcoded path (law: every harness path comes from spira.conf).
# Sourced with `set +u` because conf.sh is written for callers that have not set -u.
{ set +u; . "$HERE/../spira/conf.sh" 2>/dev/null; set -u; } || true
# The installed symlink is the normal path. If neither SPIRA_VIEW nor the installed
# symlink exists — as in a fresh container before the harness is fully wired — fall back
# to the copy shipped in this repo. The symlink would point here anyway.
VIEW="${SPIRA_VIEW:-$HOME/.local/bin/cockpit-remote}"
[ -x "$VIEW" ] || VIEW="$HERE/remote/cockpit-remote"
LAYOUT="$HERE/layout.sh"
# COCKPIT_CWD is derived in conf.sh as ${SPIRA_WIKI:-$SPIRA_REPO}: the wiki when one is
# configured, the harness root otherwise. It is the right default for tmux sessions because
# the operator's instructions load from whichever project they work in, not the harness.
CWD="${COCKPIT_CWD:-$SPIRA_REPO}"
# brain and hunk are the two the cockpit LINKS and are structural. Any additional sessions
# are recreated because they died with the server, but nothing depends on them. Override
# COCKPIT_SESSIONS in spira.conf to add or remove sessions; the default covers the common
# three-session layout (brain, hunk, and an operator's working session).
SESSIONS="${COCKPIT_SESSIONS:-brain hunk chat}"

# THE SERVER THIS SCRIPT IS ABOUT TO FORK INHERITS THIS PROCESS'S ENVIRONMENT, AND KEEPS IT
# FOR LIFE. tmux hands every new pane the environment the server was started with, so running
# rebuild.sh from inside a Claude session would stamp that session's identity onto every pane
# the cockpit ever opens. That is not hypothetical: this script was first run by an archivist
# aeon, the server captured CLAUDE_CODE_CHILD_SESSION=1, and the operator's own session —
# opened hours later in a pane of that server — stopped writing a transcript and vanished from
# the CTX meter. Clear them here, before the fork. See cockpit/tmux-env.sh.
unset $(bash "$HERE/tmux-env.sh" names) 2>/dev/null || true

say()  { printf '%s\n' "$*"; }
step() { printf '\n== %s\n' "$*"; }
warn() { printf 'rebuild: %s\n' "$*" >&2; }

# --- the probe ----------------------------------------------------------------------------
# DOES THE SERVER ANSWER? Not "is there a process", not "is there a socket file" — both were
# true of the wedged server. `list-sessions` exits 0 when it answers, and its stderr
# distinguishes the two failures that matter: "no server running" (nothing there, fine, just
# build) from anything else (something is there and not talking).
server_state() {
    local out rc
    out="$(tmux list-sessions 2>&1)"; rc=$?
    if [ $rc -eq 0 ]; then echo "up"; return; fi
    case "$out" in
        *"no server running"*|*"No such file or directory"*|*"Connection refused"*) echo "absent" ;;
        # NOT a catch-all for "anything unexpected". A socket path over the 108-byte unix
        # limit reports "File name too long", which is a broken environment rather than a
        # wedged server, and calling it wedged sends this script hunting for a holder that
        # cannot exist. Named so it is reported rather than acted on.
        *"File name too long"*) echo "unusable" ;;
        *) echo "wedged" ;;
    esac
}

# The PID holding the LISTENING socket for our socket path.
#
# BY INODE FROM /proc, NEVER BY PATTERN. `pgrep -f tmux` matches this script's own command
# line and every client, and killing on that basis is how a `pkill -f` once killed the shell
# that invoked it. /proc/net/unix gives the listening socket's inode for the path; the holder
# is whichever process has that inode open. Exactly one process can.
server_pid() {
    local sock ino p
    sock="$(tmux display-message -p '#{socket_path}' 2>/dev/null)"
    [ -n "$sock" ] || sock="${TMUX_TMPDIR:-/tmp}/tmux-$(id -u)/default"
    # Flags 00010000 is the listening bit; without it we would match connected clients too.
    ino="$(awk -v s="$sock" '$4=="00010000" && $8==s {print $7}' /proc/net/unix 2>/dev/null | head -1)"
    [ -n "$ino" ] || return 1
    for p in $(ls /proc 2>/dev/null | grep -E '^[0-9]+$'); do
        if ls -l "/proc/$p/fd" 2>/dev/null | grep -q "socket:\[$ino\]"; then echo "$p"; return 0; fi
    done
    return 1
}

# How many processes live under that server. Zero means every pane is already gone.
descendants_of() {
    local root="$1"
    ps -eo pid,ppid --no-headers 2>/dev/null | awk -v r="$root" '
        {p[$1]=$2}
        END{c=0; for(x in p){q=x; d=0; while(q!=1 && q!="" && d<64){ if(q==r){c++; break}; q=p[q]; d++ }} print c+0}'
}

report_state() {
    local st pid n
    st="$(server_state)"
    printf '  tmux server      %s\n' "$st"
    if [ "$st" = "unusable" ]; then
        printf '  NOTE             the socket path is unusable (over the 108-byte unix limit?) — fix TMUX_TMPDIR\n'
    fi
    if [ "$st" = "wedged" ]; then
        pid="$(server_pid)" || pid=""
        if [ -n "$pid" ]; then
            n="$(descendants_of "$pid")"
            printf '  holder           pid %s (%s), %s live descendant(s)\n' \
                   "$pid" "$(cat "/proc/$pid/comm" 2>/dev/null || echo '?')" "$n"
            [ "$n" -gt 0 ] && printf '  NOTE             it still holds live panes — clearing it needs --force\n'
        else
            printf '  holder           none found — the socket is orphaned\n'
        fi
    fi
    local s
    for s in $SESSIONS cockpit; do
        if tmux has-session -t "=$s" 2>/dev/null; then
            printf '  session %-9s present (%s window(s))\n' "$s" "$(tmux list-windows -t "=$s" 2>/dev/null | wc -l)"
        else
            printf '  session %-9s MISSING\n' "$s"
        fi
    done
    if tmux has-session -t "=brain" 2>/dev/null; then
        printf '  dashboards       %s\n' "$(tmux list-panes -t brain:0 -F '#{@cockpit}' 2>/dev/null | grep -c .)"
    fi
}

if [ "$MODE" = "probe" ]; then
    say "cockpit probe"
    report_state
    exit 0
fi

# --- 1. clear a wedged server --------------------------------------------------------------
step "server"
state="$(server_state)"
case "$state" in
    up)       say "  answering — leaving it alone" ;;
    absent)   say "  none running — nothing to clear" ;;
    unusable) warn "the socket path is unusable — 'tmux list-sessions' says the file name is too long."
              warn "That is TMUX_TMPDIR, not the server. Nothing here can fix it; shorten the path."
              exit 2 ;;
    wedged)
        pid="$(server_pid)" || pid=""
        if [ -z "$pid" ]; then
            warn "the socket does not answer and no process holds it; removing the stale socket"
            sock="${TMUX_TMPDIR:-/tmp}/tmux-$(id -u)/default"
            rm -f "$sock"
        else
            live="$(descendants_of "$pid")"
            say "  wedged: pid $pid, $live live descendant(s)"
            if [ "$live" -gt 0 ] && [ "$FORCE" -ne 1 ]; then
                warn "REFUSING to kill pid $pid — it still holds $live live process(es)."
                warn "That is somebody's unsaved work, not a corpse. Look first:"
                warn "    ps --ppid $pid -o pid,etime,args"
                warn "Then re-run with --force if you are sure."
                exit 3
            fi
            # TERM then KILL: the wedged server ignored TERM on 2026-09-09 and needed KILL,
            # which is itself the confirmation that it was not merely busy.
            kill -TERM "$pid" 2>/dev/null || true
            for _ in 1 2 3 4 5 6; do [ -d "/proc/$pid" ] || break; sleep 0.5; done
            if [ -d "/proc/$pid" ]; then
                say "  did not exit on TERM — sending KILL"
                kill -KILL "$pid" 2>/dev/null || true
                for _ in 1 2 3 4; do [ -d "/proc/$pid" ] || break; sleep 0.5; done
            fi
            [ -d "/proc/$pid" ] && { warn "pid $pid survived KILL — cannot continue"; exit 1; }
            say "  cleared"
        fi
        ;;
esac

# --- 2. the sessions ------------------------------------------------------------------------
step "sessions"
for s in $SESSIONS; do
    if tmux has-session -t "=$s" 2>/dev/null; then
        say "  $s already present"
    else
        tmux new-session -d -s "$s" -c "$CWD" 2>/dev/null \
            && say "  $s created" \
            || { warn "could not create session '$s'"; exit 1; }
    fi
done

# --- 2b. the server's environment -----------------------------------------------------------
# A server we did NOT fork — one that was already up when this ran — may still be carrying a
# session identity from whoever started it. Scrubbing the global environment fixes every pane
# opened from here on; panes that already exist keep what they were given.
bash "$HERE/tmux-env.sh" scrub 2>/dev/null | sed 's/^/  /'

# --- 3. the dashboards, in brain:0 ----------------------------------------------------------
# --window is passed EXPLICITLY. layout.sh's default_window() falls back to the first pane
# running `claude`, and on a freshly rebuilt server there is none — it would then fall back to
# a literal "cockpit:2" that does not exist yet, which is exactly the failure this script
# exists to stop the operator hitting by hand.
step "dashboards"
if ! bash "$LAYOUT" up --window brain:0 2>&1 | sed 's/^/  /'; then
    warn "layout.sh up failed"; exit 1
fi

# --- 4. link them into the cockpit ----------------------------------------------------------
step "cockpit"
if [ -x "$VIEW" ]; then
    "$VIEW" build 2>&1 | sed 's/^/  /'
    "$VIEW" sync  2>&1 | sed 's/^/  /'
    say "  linked and synced"
else
    warn "no cockpit-remote at $VIEW — brain:0 has its dashboards but nothing links them"
    warn "set SPIRA_VIEW in ~/.config/spira/spira.conf, or run its build step by hand"
fi

# --- 5. verify, and mean it -----------------------------------------------------------------
# A REBUILD THAT REPORTS SUCCESS WITHOUT LOOKING IS THE FAILURE THIS WHOLE DIRECTORY KEEPS
# HITTING. Each check below is a positive control: an EMPTY dashboard capture is a failure,
# not a pass, because "the pane is blank" and "the pane is fine" are the same exit code
# otherwise (law-absence-needs-a-positive-control).
step "verify"
fail=0
chk() { if eval "$2" >/dev/null 2>&1; then printf '  ok    %s\n' "$1"; else printf '  FAIL  %s\n' "$1"; fail=$((fail+1)); fi; }

chk "the server answers"                 'tmux list-sessions'
for s in $SESSIONS cockpit; do chk "session $s exists" "tmux has-session -t '=$s'"; done
chk "brain:0 holds three panes"          '[ "$(tmux list-panes -t brain:0 2>/dev/null | wc -l)" -eq 3 ]'
chk "a pane is tagged panel"             'tmux list-panes -t brain:0 -F "#{@cockpit}" | grep -qx panel'
chk "a pane is tagged health"            'tmux list-panes -t brain:0 -F "#{@cockpit}" | grep -qx health'
chk "cockpit links brain:0"              'tmux list-windows -t "=cockpit" -F "#{window_id}" | grep -qx "$(tmux list-windows -t "=brain" -F "#{window_id}" | head -1)"'
chk "cockpit links hunk:0"               'tmux list-windows -t "=cockpit" -F "#{window_id}" | grep -qx "$(tmux list-windows -t "=hunk" -F "#{window_id}" | head -1)"'

# The dashboards must RENDER, not merely exist. Given a moment to paint first: the panel is a
# binary that opens a database, and asserting on an unpainted pane would fail for the wrong
# reason.
sleep 3
for role in panel health; do
    pid_pane="$(tmux list-panes -t brain:0 -F '#{pane_id} #{@cockpit}' 2>/dev/null | awk -v r="$role" '$2==r{print $1}')"
    if [ -z "$pid_pane" ]; then
        printf '  FAIL  %s pane renders content\n' "$role"; fail=$((fail+1))
    else
        n="$(tmux capture-pane -p -t "$pid_pane" 2>/dev/null | grep -c '[^[:space:]]')"
        if [ "${n:-0}" -gt 0 ]; then printf '  ok    %s pane renders content (%s non-blank line(s))\n' "$role" "$n"
        else printf '  FAIL  %s pane is BLANK\n' "$role"; fail=$((fail+1)); fi
    fi
done

step "watchers"
if [ -x "$SPIRA_HOME/watchd.sh" ]; then
    "$SPIRA_HOME/watchd.sh" status 2>&1 | sed 's/^/  /'
else
    say "  (watchd.sh not found — skipped)"
fi

echo
if [ "$fail" -eq 0 ]; then
    say "cockpit rebuilt. Attach with:  tmux attach -t cockpit"
    say "A watcher's Monitor cannot be started by a script — re-attach them in the session:"
    say "    Monitor: $SPIRA_HOME/watchd.sh tail answers"
    say "    Monitor: $SPIRA_HOME/watchd.sh tail view"
    exit 0
fi
warn "$fail check(s) failed — the cockpit is NOT fully rebuilt"
exit 1
