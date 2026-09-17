#!/usr/bin/env bash
#
# layout.sh — build the cockpit: session top-left, mail bottom-left, health down the right.
#
#   layout.sh up       create/repair the dashboard pane (idempotent)
#   layout.sh down     remove it, leaving the session pane full-height
#   layout.sh status   what is running
#   layout.sh ensure   heal a cockpit that is already up; SILENT when nothing is wrong
#
#   layout.sh up --window <target>   apply to a different tmux window
#
#                +------------------------------------------+---------------+
#                |   claude session                         |               |
#                |   (flips to hunk for review)             |  ops health   |
#                |                                          |               |
#                +------------------------------------------+  full height  |
#                |   mail (COCKPIT_MAIL), COCKPIT_BOTTOM_PCT |  ~33% wide    |
#                +------------------------------------------+---------------+
#                 <-------- 100% - COCKPIT_RIGHT_PCT -------> <-- RIGHT_PCT ->
#
# ORDER IS THE GEOMETRY
# ---------------------
# The health pane is split off the WHOLE WINDOW, which gives it the window's full height.
# A dashboard with a column's worth of rows is not a cosmetic change: health.sh
# sizes each of its sections to the rows it is given, and in five rows every section is cut
# to one.
#
# PANES ARE ADDRESSED BY TAG, NEVER BY INDEX
# ------------------------------------------
# tmux RENUMBERS pane indices the moment a pane dies, and the session pane is the one most
# likely to die — it holds a shell the operator can exit. So the dashboard pane carries a
# pane-scoped option `@cockpit` (health) and is resolved by pane id through it. Identity
# survives renumbering; an index does not.
#
# `up` is a REPAIR, not just a create. It normalises whatever it finds: if the session pane
# is gone it opens a fresh shell at the top before touching anything else (the window must
# never reach zero panes, or it dies and takes the session with it), then rebuilds the dashboards.
#
# WHAT THIS MUST NOT BREAK
# ------------------------
# `cockpit` is built from LINKED WINDOWS: `cockpit:1` and `hunk:0` are the same window
# object, as are `cockpit:2` and `brain:0` — the flip between review and session is tmux
# window navigation over shared panes, not two copies. So a split here is visible from
# both session names, which is what makes the dashboard persist across the flip back.
#
# The session pane is never respawned or killed. `hunk-open.sh` sends keys to the session's
# ACTIVE pane, so this script always restores the session pane as active before exiting.
# Leaving a bottom pane selected would send the next review's launch command into a dashboard.
#
# The panes run renderers that only read a snapshot file — `$SPIRA_RUN/cockpit.env`, written
# by `spira-cockpit.service`. This script does not supervise that collector and must not start
# one: a probe pass costs seconds, and a pane that shelled out would stall on every repaint.
# Layout repair and metric collection are separate concerns under separate units.
#
# THE SESSION PANE IS THE CONCIERGE
# ----------------------------------
# Option A: when `up` creates a fresh session pane it runs `concierge.sh here`, which starts
# the concierge in its own tmux socket (tmux -L concierge) if it is not running, then
# exec-attaches to it. The result is nested tmux: the cockpit pane shows the concierge
# session. Both layers use Ctrl-b; Ctrl-b Ctrl-b sends the prefix to the outer cockpit when
# the operator is inside the inner concierge session. The concierge keeps its own socket so
# its Remote Control name (`--remote-control concierge`) is independent of the cockpit
# layout and it can be killed and restarted without touching the dashboard.
#
# `ensure` — WHAT THE WATCHER CALLS
# ---------------------------------
# `cockpit-remote watch` calls this on its poll loop so the layout self-heals: the session
# pane runs the concierge and exits when it detaches; `ensure` reopens it with the same
# command so the desk view is always the concierge. It repairs only a cockpit that is ALREADY
# UP — a window with `@cockpit` panes but no session pane left.
#
# The absence of any `@cockpit` pane means `down` was run, and `ensure` must leave that
# alone. An unattended process that rebuilds the dashboards the operator just dismissed is not a fence,
# it is a fight. Same reason it heals wherever the tagged panes actually are rather than
# at a hardcoded `brain:0`.
#
# It is SILENT on a healthy cockpit — a heartbeat that prints "fine" every 15s buries the
# one line that says it repaired something. Repairs are logged to `.runtime/cockpit-heal.log`,
# because self-healing that leaves no trace is how nobody learns the shell keeps dying.
set -uo pipefail

# Every path comes from the harness's one configuration surface. It is two directories
# away because the cockpit ships beside the harness, not inside it.
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/../spira" && pwd -P)/conf.sh"
COCK="$SPIRA_COCKPIT"
# In split-checkout mode the renderer must come from the same release the collector reads
# so reader and writer agree on the snapshot schema. SPIRA_DEV_RENDERER=1 opts back in to
# this checkout's health.sh, e.g. when developing the renderer itself.
if [ -z "${SPIRA_DEV_RENDERER:-}" ] && [ -n "${SPIRA_PROD:-}" ] && ! spira_single_checkout; then
    _prod_cock="$SPIRA_PROD/cockpit"
    [ -d "$_prod_cock" ] && COCK="$_prod_cock"
fi
RUN="$SPIRA_REPO/.runtime"

# SPIRA_CONF, when set, points to the config file for this instance. Pane commands carry it
# in the command string so a respawn — from tmux itself or from the ensure timer — uses the
# same config as the original `up` did. The tmux server's environment is set once at server
# start and cannot be updated per-pane, so a var only in the spawning shell's environment
# is lost the moment the pane respawns. Single-quoting is safe: config paths are always
# simple filesystem paths with no embedded single quotes.
_CONF_PREFIX=""
[ -n "${SPIRA_CONF:-}" ] && _CONF_PREFIX="SPIRA_CONF='${SPIRA_CONF}' "
# The panes open where the operator works — COCKPIT_CWD, a spira.conf key.
CWD="$COCKPIT_CWD"
# COCKPIT_RIGHT_PCT controls how wide the full-height ops column is, as a % of the window.
RIGHT_PCT="$COCKPIT_RIGHT_PCT"
BOTTOM_PCT="$COCKPIT_BOTTOM_PCT"
# The mail pane exists only when its client does: a pane whose program is missing dies at once,
# and ensure would respawn it every minute.
MAIL_CMD="${COCKPIT_MAIL:-}"
MAIL_EXE="${MAIL_CMD%% *}"
[ -n "$MAIL_EXE" ] && command -v "$MAIL_EXE" >/dev/null 2>&1 || MAIL_CMD=""
mkdir -p "$RUN"

# Default to the window this script was invoked from; fall back to the claude window.
default_window() {
    if [ -n "${TMUX_PANE:-}" ]; then
        tmux display-message -p -t "$TMUX_PANE" '#{session_name}:#{window_index}' 2>/dev/null && return
    fi
    local w
    w=$(tmux list-panes -a -F '#{session_name}:#{window_index} #{pane_current_command}' 2>/dev/null \
        | awk '$2=="claude"{print $1; exit}')
    [ -n "$w" ] && { echo "$w"; return; }
    echo "cockpit:2"
}

WINDOW=""
ACTION="${1:-status}"; shift || true
while [ $# -gt 0 ]; do
    case "$1" in
        --window) WINDOW="${2:?}"; shift 2 ;;
        *) shift ;;
    esac
done
[ -n "$WINDOW" ] || WINDOW="$(default_window)"

# --- pane identity ------------------------------------------------------------
# Every listing prints pane IDs (%12), which are stable for the pane's whole life. Index
# is used only where tmux itself demands one, and never stored.

tag_pane()   { tmux set-option -p -t "$1" @cockpit "$2" 2>/dev/null; }

# -g rather than per-session: the cockpit is one session among several; scope to it alone
# leaves the others dead to the mouse, which is the inconsistency that reads as a bug.
apply_mouse_mode() {
    local want="${COCKPIT_MOUSE:-on}"
    case "$want" in
        off|no|0) return 0 ;;   # the operator keeps terminal-native drag-select
    esac
    tmux set-option -g mouse on 2>/dev/null || true
}

# An untagged pane is indistinguishable from the session pane — which is how the first
# repair run picked the ORPHANED decisions pane as the session and split it. So tag it.
#
# Identity comes from what is RUNNING in the pane, not from `pane_start_command`.
# start_command is the command the pane was FIRST created with and it survives
# `respawn-pane`, so after a few swaps the decisions pane still advertised
# "health.sh loop" — both dashboards ended up tagged `health`, `tagged health` resolved
# to the wrong pane, and the real health pane could die unnoticed. Read the pane's
# process and its children instead.
#
# AND READ THEM BY ARGV POSITION, NEVER AS A SUBSTRING OF THE WHOLE COMMAND LINE. A glob over
# the joined argv matches any process that merely MENTIONS a dashboard: an agent session
# launched with a system prompt listing `cockpit/health.sh` among its tools was tagged
# `health`, so the next ensure found no untagged pane, declared the session pane gone, and
# `up` killed the live session along with the dashboards — every other minute. The executable
# is argv[0]; when that is a shell, the script it runs is argv[1].
pane_role() {   # pane_role <pid> -> health|mail on stdout, nothing when neither
    local pid="$1" q exe script
    local -a argv
    for q in "$pid" $(pgrep -P "$pid" 2>/dev/null); do
        mapfile -d '' -t argv < "/proc/$q/cmdline" 2>/dev/null || continue
        [ "${#argv[@]}" -gt 0 ] || continue
        exe="${argv[0]}"; script=""
        case "${exe##*/}" in
            bash|sh|dash) [ "${#argv[@]}" -gt 1 ] && [ "${argv[1]#-}" = "${argv[1]}" ] && script="${argv[1]}" ;;
        esac
        if [[ "$exe" == */cockpit/health.sh ]] || [[ "$script" == */cockpit/health.sh ]]; then
            echo health; return 0
        fi
        if [ -n "$MAIL_CMD" ] && [ "${exe##*/}" = "${MAIL_EXE##*/}" ]; then
            echo mail; return 0
        fi
    done
}

adopt_untagged() {
    tmux list-panes -t "$WINDOW" -F '#{@cockpit}|#{pane_id}|#{pane_pid}' 2>/dev/null \
    | while IFS='|' read -r tag id pid; do
        [ -n "$tag" ] && continue
        local role; role=$(pane_role "$pid")
        [ -n "$role" ] && tag_pane "$id" "$role"
      done
}

# Re-derive every dashboard tag from the running process, correcting stale ones — including
# a tag on a live pane that runs no dashboard at all, which is otherwise permanent: `up` kills
# every tagged pane, so a wrongly tagged session pane is killed on the next repair.
retag_dashboards() {
    tmux list-panes -t "$WINDOW" -F '#{pane_id}|#{pane_pid}|#{pane_dead}|#{@cockpit}' 2>/dev/null \
    | while IFS='|' read -r id pid dead tag; do
        local role; role=$(pane_role "$pid")
        if [ -n "$role" ]; then
            [ "$role" = "$tag" ] || tag_pane "$id" "$role"
        elif [ -n "$tag" ] && [ "$dead" != 1 ]; then
            heal_log "$WINDOW: $id is tagged $tag but runs no dashboard — clearing the tag"
            tmux set-option -p -u -t "$id" @cockpit 2>/dev/null || true
        fi
      done
}

# pane id of the dashboard tagged $1, empty if absent.
tagged()     { tmux list-panes -t "$WINDOW" -F '#{@cockpit} #{pane_id}' 2>/dev/null \
                 | awk -v t="$1" '$1==t {print $2; exit}'; }

# every tagged dashboard pane id, one per line (catches duplicates from a bad run).
all_tagged() { tmux list-panes -t "$WINDOW" -F '#{@cockpit} #{pane_id}' 2>/dev/null \
                 | awk 'NF==2 && ($1=="health" || $1=="mail") {print $2}'; }

# untagged panes — the session pane, plus anything the operator split off himself.
untagged()   { tmux list-panes -t "$WINDOW" -F '#{@cockpit} #{pane_id}' 2>/dev/null \
                 | awk '{ if (NF==1) print $1; else if ($1!="health" && $1!="mail") print $2 }'; }

# The session pane: this script's own pane when it is in this window (an agent running
# `up` from the cockpit is the normal case), else the first untagged pane.
session_pane() {
    local me="${TMUX_PANE:-}"
    if [ -n "$me" ]; then
        untagged | grep -Fxq "$me" && { echo "$me"; return; }
    fi
    untagged | head -1
}

panes_in() { tmux list-panes -t "$WINDOW" 2>/dev/null | wc -l; }

HEAL_LOG="$RUN/cockpit-heal.log"
HEAL_STAMP="$RUN/.cockpit-heal-stamp"
HEAL_COOLDOWN="${COCKPIT_HEAL_COOLDOWN:-60}"

heal_log() {
    printf '%s %s\n' "$(TZ="${SPIRA_TZ:-${TZ:-}}" date '+%Y-%m-%dT%H:%M:%S')" "$*" | tee -a "$HEAL_LOG"
}

# One target per DISTINCT window holding cockpit panes. Dedup is by window_id, not by
# name: `cockpit:2` and `brain:0` are the same window object, so a name-keyed scan would
# find the break twice and repair it twice.
cockpit_windows() {
    tmux list-panes -a -F '#{window_id}|#{session_name}:#{window_index}|#{@cockpit}' 2>/dev/null \
        | awk -F'|' '$3=="health" || $3=="mail" { if (!seen[$1]++) print $2 }'
}

# A repair that fails repeatedly must not be retried on every poll.
heal_ready() {
    [ -f "$HEAL_STAMP" ] || return 0
    [ $(( $(date +%s) - $(stat -c %Y "$HEAL_STAMP") )) -ge "$HEAL_COOLDOWN" ]
}

# Epoch seconds at which a pid started.
# Returns the process's start epoch, or FAILS. It must never answer 0 for "I could not
# tell": every caller compares it against a file mtime, and 0 means infinitely old, so a
# failed probe reads as "running code from before the file existed" and triggers a repair.
#
# That is not hypothetical. `ps` gets slow and drops requests when the box is loaded, and over
# one 70-minute window -- while CI, a container build and the visual suite were all running --
# a caller of this respawned its subject 2212 times on failed probes alone. Each restart added
# load, which made the next probe likelier to fail. A repair loop that feeds on its own load is
# worse than the staleness it exists to catch, so the remaining callers respawn a PANE, which
# is cheap; nothing here supervises a process any more.
#
# The dashboard learned this already: a failed probe renders `?`, never 0.
# GHOST CLIENT DETACHMENT. With window-size latest (the server default), whichever client
# most recently changed its active window determines the cockpit window height. Ghost clients —
# terminals that have been idle for days but remain attached — periodically become "latest"
# and shrink the cockpit window to their small terminal height, causing the NEXT/RECENT
# sections to flap between ~5 and ~14 rows tick to tick.
#
# Two complementary defences:
#
#   1. detach_idle_clients removes the root cause: a client that has been idle longer than
#      COCKPIT_CLIENT_IDLE_SECS (default 6 h) is detached and cannot become "latest".
#
#   2. window-size largest (set in `up` and repaired by `ensure`) provides defence in depth:
#      even if a ghost reconnects briefly before the next ensure run, the server picks the
#      largest attached client — always the operator's live terminal — rather than the most
#      recently active one. `window-size manual` would also prevent the flap, but it freezes
#      the window at the size at creation time and never follows the operator's terminal when
#      they resize it. `window-size latest` with aggressive-resize on is the old per-window
#      smallness heuristic and does not help here. `window-size largest` responds to resize
#      while ignoring activity order, which is exactly what the cockpit needs.
IDLE_SECS="${COCKPIT_CLIENT_IDLE_SECS:-21600}"
detach_idle_clients() {
    [ "$IDLE_SECS" -gt 0 ] 2>/dev/null || return 0
    local now; now=$(date +%s)
    tmux list-clients -F '#{client_tty} #{client_activity}' 2>/dev/null \
    | while read -r tty act; do
        [ -n "$tty" ] && [ -n "$act" ] || continue
        local age=$(( now - act ))
        if [ "$age" -gt "$IDLE_SECS" ]; then
            heal_log "detach: client $tty idle ${age}s — detaching"
            tmux detach-client -t "$tty" 2>/dev/null || true
        fi
    done
}

proc_start() {
    local e; e=$(ps -o etimes= -p "$1" 2>/dev/null | tr -d ' ')
    [ -n "$e" ] || return 1
    echo $(( $(date +%s) - e ))
}

restart_if_stale() { # pane_tag script_path
    local tag="$1" src="$2" pane pid started mtime
    pane="$(tagged "$tag")"; [ -n "$pane" ] || return 0
    [ -f "$src" ] || return 0
    pid=$(tmux list-panes -t "$WINDOW" -F '#{@cockpit} #{pane_pid}' 2>/dev/null | awk -v t="$tag" '$1==t{print $2; exit}')
    [ -n "$pid" ] || return 0
    started=$(proc_start "$pid") || {
        heal_log "$WINDOW: $tag start time unreadable — leaving it alone"
        return 0
    }
    mtime=$(stat -c %Y "$src" 2>/dev/null || echo 0)
    [ "$mtime" -gt "$started" ] || return 0
    heal_log "$WINDOW: $tag is running code older than $src — respawning"
    tmux respawn-pane -k -t "$pane" "${_CONF_PREFIX}$COCK/health.sh loop" 2>/dev/null
    tag_pane "$pane" "$tag"
}

# THE SPIRA COLLECTOR HAS THE SAME PROBLEM AND NONE OF THE COMPLICATIONS. It is only ever
# started by systemd — nothing here ever launched it from a terminal — so there is no orphan
# to hunt and the supervised process is the only one that can exist. Left unwatched it keeps
# running a superseded copy indefinitely, and the symptom is the one this whole surface exists
# to prevent: a key added to the probe is never emitted, the pane renders `?` for it forever,
# and every process involved reports itself healthy.
restart_spira_collector_if_stale() {
    # THE UNIT NAME IS PER-INSTANCE. install.sh renders spira-cockpit-<instance>.service
    # for each installation; the old plain unit is inactive (dead) on a migrated box. Try
    # the instance-qualified name first, fall back to the plain name for pre-migration
    # installs. Return early if no active unit is found — an is-active call on the wrong
    # unit is the first two (cockpit.sh:54, world.sh work_services), and this is the third.
    local unit="" src u main started mtime
    for u in "spira-cockpit${SPIRA_INSTANCE:+-$SPIRA_INSTANCE}.service" spira-cockpit.service; do
        systemctl --user cat "$u" >/dev/null 2>&1 || continue
        [ "$(systemctl --user is-active "$u" 2>/dev/null)" = active ] || continue
        unit="$u"
        break
    done
    [ -n "$unit" ] || return 0
    # THE SOURCE IS THE PROD CHECKOUT, NOT THE DEV CHECKOUT. promote.sh fast-forwards
    # $SPIRA_PROD with git checkout, which stamps the promoted files with the promotion
    # time. $SPIRA_HOME is the dev checkout and is not updated on promotion, so its mtime
    # never reflects a landing — a committed change looks current to the old comparison even
    # when the running collector predates it by an hour (observed 2026-09-09, sp-nfxd).
    src="${SPIRA_PROD:-}/cockpit.sh"
    [ -f "$src" ] || return 0
    main=$(systemctl --user show "$unit" -p MainPID --value 2>/dev/null || echo 0)
    [ -n "$main" ] && [ "$main" != 0 ] || return 0
    started=$(proc_start "$main") || {
        heal_log "spira collector $main start time unreadable — leaving it alone"
        return 0
    }
    mtime=$(stat -c %Y "$src" 2>/dev/null || echo 0)
    [ "$mtime" -gt "$started" ] || return 0
    heal_log "spira collector $main is running code older than cockpit.sh — restarting $unit"
    systemctl --user restart "$unit" 2>/dev/null
}

# LOOM HAS THE SAME GITIGNORED-BINARY TRAP AS THE PANEL. loom/target/release/loom is in
# .gitignore, so landing a source change ships nothing to the running server — the binary
# on disk is still whatever was last hand-built. This pair catches that: rebuild when source
# is newer than binary, then restart the service if the binary is now newer than the process.
rebuild_loom_if_stale() {
    local dir="${SPIRA_REPO:-}/loom" bin="${SPIRA_LOOM_BIN:-}" newest cargo_bin
    [ -n "$bin" ] || return 0
    [ -d "$dir/src" ] || return 0
    cargo_bin="$(command -v cargo 2>/dev/null || true)"
    [ -n "$cargo_bin" ] || cargo_bin="$([ -x "$HOME/.cargo/bin/cargo" ] && echo "$HOME/.cargo/bin/cargo" || true)"
    if [ -z "$cargo_bin" ]; then
        heal_log "loom: cargo not found on PATH or at ~/.cargo/bin — rebuild skipped; add ~/.cargo/bin to SPIRA_PATH"
        return 0
    fi
    newest=$(find "$dir/src" "$dir/Cargo.toml" -newer "$bin" -print -quit 2>/dev/null)
    # No binary at all also means build. -newer against a missing file finds nothing.
    [ -n "$newest" ] || [ ! -x "$bin" ] || return 0
    # One build at a time: the timer fires every minute and a release build is not fast.
    exec 9>"$dir/.build.lock" 2>/dev/null || return 0
    flock -n 9 || return 0
    heal_log "loom: source newer than binary — rebuilding"
    if (cd "$dir" && timeout 600 nice -n 10 "$cargo_bin" build --release >/dev/null 2>&1); then
        heal_log "loom: rebuilt; restart_loom_if_stale will restart the service"
    else
        # A broken build must not silently leave the old binary looking current.
        heal_log "loom: REBUILD FAILED — service still running the previous binary"
    fi
    exec 9>&-
}

restart_loom_if_stale() {
    local unit bin="${SPIRA_LOOM_BIN:-}" main started mtime
    unit="$(spira_unit loom service)"
    [ -n "$bin" ] && [ -x "$bin" ] || return 0
    [ "$unit" != '?' ] || return 0
    systemctl --user cat "$unit" >/dev/null 2>&1 || return 0
    [ "$(systemctl --user is-active "$unit" 2>/dev/null)" = active ] || return 0
    main=$(systemctl --user show "$unit" -p MainPID --value 2>/dev/null || echo 0)
    [ -n "$main" ] && [ "$main" != 0 ] || return 0
    started=$(proc_start "$main") || {
        heal_log "loom $main start time unreadable — leaving it alone"
        return 0
    }
    mtime=$(stat -c %Y "$bin" 2>/dev/null || echo 0)
    [ "$mtime" -gt "$started" ] || return 0
    heal_log "loom $main is running binary older than loom binary — restarting $unit"
    systemctl --user restart "$unit" 2>/dev/null
}

# --- the split that produces the shape -------------------------------------------
# `-f` IS WHAT MAKES A REPAIR PRODUCE THE SAME SHAPE AS A FRESH BUILD. A plain split takes
# its TARGET pane's height, so a health pane respawned after the left column had already
# been divided came back the height of whichever half it was split from — a quadrant, on
# the correct side, the correct width, running the correct program. `-f` spans the full
# window height whatever the target looks like, so `up` and every repair path agree.
split_health() {   # split_health <any pane in the left column> -> pane id on stdout
    tmux split-window -P -F '#{pane_id}' -d -h -f -l "${RIGHT_PCT}%" -t "$1" -c "$CWD" \
        "${_CONF_PREFIX}$COCK/health.sh loop"
}

# The mail pane is cut from the session pane, AFTER health, so health keeps the full height.
split_mail() {     # split_mail <session pane> -> pane id on stdout
    tmux split-window -P -F '#{pane_id}' -d -v -l "${BOTTOM_PCT}%" -t "$1" -c "$CWD" \
        "${_CONF_PREFIX}$MAIL_CMD"
}

geom() { tmux display-message -p -t "$1" "$2" 2>/dev/null; }

# HEALTH MUST BE FULL WINDOW HEIGHT. A half-height health pane is on the right, the right
# width, and running the right program; only its height is wrong, and the eye reads it as the
# layout working. So the height is measured against the window rather than looked at.
normalize_geometry() {
    local h hh wh anchor

    # HEALTH SPANS THE FULL WINDOW HEIGHT. Anything less came back as a quadrant
    # and cannot be resized into the right shape — the column has to be cut from the window again.
    h="$(tagged health)"
    if [ -n "$h" ]; then
        hh="$(geom "$h" '#{pane_height}')"; wh="$(geom "$h" '#{window_height}')"
        if [ -n "$hh" ] && [ -n "$wh" ] && [ "$hh" -lt "$wh" ]; then
            heal_log "$WINDOW: health is $hh of $wh rows — rebuilding it as a full-height column"
            anchor="$(session_pane)"
            if [ -n "$anchor" ] && [ "$anchor" != "$h" ]; then
                tmux kill-pane -t "$h" 2>/dev/null || true
                h="$(split_health "$anchor")" && [ -n "$h" ] && tag_pane "$h" health
            fi
        fi
    fi
}

repair_dashboards() {
    local sess; sess="$(session_pane)"
    [ -n "$sess" ] || return 0

    # DUPLICATES FIRST. A respawn racing this repair could produce two health panes.
    local seen p
    seen=""
    for p in $(tmux list-panes -t "$WINDOW" -F '#{@cockpit} #{pane_id}' 2>/dev/null \
               | awk '$1=="health" {print $2}'); do
        if [ -z "$seen" ]; then seen="$p"; continue; fi
        heal_log "$WINDOW: duplicate health pane $p — closing"
        tmux kill-pane -t "$p" 2>/dev/null || true
    done

    local h m
    h="$(tagged health)"
    if [ -z "$h" ]; then
        heal_log "$WINDOW: health pane gone — respawning as the full-height right column"
        h="$(split_health "$sess")" || return 1
        [ -n "$h" ] && tag_pane "$h" health
    fi
    normalize_geometry
    if [ -n "$MAIL_CMD" ] && [ -z "$(tagged mail)" ]; then
        heal_log "$WINDOW: mail pane gone — respawning under the session"
        m="$(split_mail "$sess")" && [ -n "$m" ] && tag_pane "$m" mail
    fi
    tmux select-pane -t "$sess" 2>/dev/null || true
}

# EVERY PANE THIS SCRIPT OPENS TAKES THE SERVER'S ENVIRONMENT, not this process's, and the
# server's was frozen when it was forked. If it was forked from inside a Claude session it is
# still handing that session's identity to every new pane — which stops the operator's own
# client writing a transcript and empties the CTX meter. Scrubbing on `up` and `ensure` makes
# the one-minute timer the repair: the poison cannot outlive a minute of a healthy cockpit.
# Silent when there is nothing to remove. See cockpit/tmux-env.sh.
case "$ACTION" in
    up|ensure) bash "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/tmux-env.sh" scrub 2>/dev/null ;;
esac

case "$ACTION" in

up)
    tmux has-session -t "${WINDOW%%:*}" 2>/dev/null || { echo "no tmux session for '$WINDOW'" >&2; exit 1; }
    adopt_untagged
    # Before `kill-pane` runs over every tagged pane: a tag on a pane that runs no dashboard
    # would make that pane — usually the session — one of the ones killed.
    retag_dashboards

    sess="$(session_pane)"
    if [ -z "$sess" ]; then
        # Every pane is a dashboard: the session pane died and the dashboards inherited the
        # window. Open its replacement FIRST — killing the dashboards to make room would
        # empty the window, and an empty window is a destroyed window.
        # THE SESSION PANE RUNS THE CONCIERGE. See header comment for the choice rationale.
        _conc="${SPIRA_REPO}/concierge.sh"
        if [ -x "$_conc" ]; then
            sess=$(tmux split-window -P -F '#{pane_id}' -b -v -l 60% -t "$(all_tagged | head -1)" -c "$CWD" \
                "${_CONF_PREFIX}${_conc} here") \
                || { echo "cockpit: could not restore the session pane" >&2; exit 1; }
            echo "cockpit: session pane was gone — opened concierge.sh here at $sess"
        else
            sess=$(tmux split-window -P -F '#{pane_id}' -b -v -l 60% -t "$(all_tagged | head -1)" -c "$CWD") \
                || { echo "cockpit: could not restore the session pane" >&2; exit 1; }
            echo "cockpit: session pane was gone — opened a shell at $sess (concierge.sh not found)"
        fi
    fi

    # Rebuild the dashboard from scratch. Killing first is what makes `up` a repair: a duplicate
    # or stale dashboard from an earlier bad run is removed rather than split again.
    for p in $(all_tagged); do tmux kill-pane -t "$p" 2>/dev/null || true; done

    hea=$(split_health "$sess") \
        || { echo "cockpit: health split failed" >&2; exit 1; }
    tag_pane "$hea" health
    mai=""
    if [ -n "$MAIL_CMD" ]; then
        mai=$(split_mail "$sess") || { echo "cockpit: mail split failed" >&2; exit 1; }
        tag_pane "$mai" mail
    fi

    echo "cockpit: up in $WINDOW (session $sess · health $hea${mai:+ · mail $mai})"
    # window-size largest so the operator's terminal (always the tallest) governs the cockpit
    # window height regardless of which client was most recently active. See detach_idle_clients
    # for the full rationale.
    tmux set-option -t "${WINDOW%%:*}" window-size largest 2>/dev/null || true
    apply_mouse_mode
    # ALWAYS hand focus back to the session pane: hunk-open sends keys to the active pane.
    tmux select-pane -t "$sess" 2>/dev/null || true
    ;;

down)
    adopt_untagged
    retag_dashboards
    sess="$(session_pane)"
    if [ -z "$sess" ]; then
        # Refuse rather than empty the window — `down` promises a full-height session pane,
        # and there is none left to leave behind. `up` is the command that repairs this.
        echo "cockpit: no session pane in $WINDOW — run 'layout.sh up' to restore it" >&2
        exit 1
    fi
    for p in $(all_tagged); do tmux kill-pane -t "$p" 2>/dev/null || true; done
    tmux select-pane -t "$sess" 2>/dev/null || true
    echo "cockpit: down in $WINDOW"
    ;;

ensure)
    # A copy running from a worktree must not heal the operator's live cockpit.
    # `ensure` iterates over every cockpit window on the server and overwrites WINDOW on
    # each pass — so a copy does not merely skip the wrong window, it ADOPTS every live
    # pane and respawns them with its own $COCK, hijacking the installed dashboards. A
    # polite refusal at the top binds the actor (the copy) rather than the path it travels,
    # and exits 0 so a timer calling this does not page on the ordinary case of a worktree
    # having been discarded while the timer still references the old path.
    _self=$(realpath "${BASH_SOURCE[0]}" 2>/dev/null || echo "${BASH_SOURCE[0]}")
    _installed=$(realpath "$SPIRA_COCKPIT/layout.sh" 2>/dev/null || echo "$SPIRA_COCKPIT/layout.sh")
    if [ "$_self" != "$_installed" ]; then
        echo "cockpit: ensure refused — this is a copy, not the installed layout.sh; run $SPIRA_COCKPIT/layout.sh ensure" >&2
        exit 0
    fi
    # Detach clients idle longer than COCKPIT_CLIENT_IDLE_SECS (default 6 h). Ghost clients
    # cause the NEXT/RECENT flap by becoming "latest" with window-size latest, shrinking the
    # cockpit to their small terminal height. This runs unconditionally — before checking for
    # cockpit windows — so ghosts are cleared even on a server where ensure has never seen a
    # cockpit pane yet.
    detach_idle_clients
    # Re-applied here, not only in `up`: a tmux server restarted by hand comes back
    # with mouse off and `up` may never run again on it, so a cockpit that has been
    # merely `ensure`d would stay unclickable.
    apply_mouse_mode
    # Defence in depth: ensure window-size largest is set for every session hosting a cockpit
    # window. This survives a ghost that reconnects between two ensure runs.
    for w in $(cockpit_windows); do
        tmux set-option -t "${w%%:*}" window-size largest 2>/dev/null || true
    done
    for w in $(cockpit_windows); do
        WINDOW="$w"
        adopt_untagged
        retag_dashboards
        if [ -n "$(session_pane)" ]; then
            # Session pane is fine — but the dashboard may still be dead. That is exactly
            # what happened: the health pane exited, the session pane was
            # healthy, so `ensure` skipped the window and nothing ever brought it back.
            repair_dashboards
            restart_if_stale health "$COCK/health.sh"
            continue
        fi
        heal_ready || continue
        : >"$HEAL_STAMP"
        heal_log "$WINDOW: session pane gone — repairing"
        if out=$("$0" up --window "$WINDOW" 2>&1); then
            heal_log "$WINDOW: $(printf '%s' "$out" | tr '\n' ' ')"
        else
            heal_log "$WINDOW: REPAIR FAILED — $(printf '%s' "$out" | tr '\n' ' ')"
        fi
    done
    # NO WATCHDOG OVER A COLLECTOR'S LIVENESS HERE, only over the code it is running. This
    # used to kill and respawn a collector whenever its snapshot looked stale, and a pass of
    # that collector could not finish inside the 60s between `ensure` runs under load — so
    # every pass read the snapshot as stale, killed the collector and started another,
    # forever, each attempt shelling out to probes that cost seconds on the same cores the
    # work runs on. A repair loop feeding on its own load is worse than the staleness it
    # exists to catch, and a snapshot that is merely late is indistinguishable from one whose
    # writer is slow.
    #
    # systemd is where process supervision belongs: `spira-cockpit.service` is Restart=always
    # under a CPUQuota, and it backs off where a one-minute timer cannot. This script owns the
    # tmux layout, and the one process property it still checks is whether a running collector
    # predates its own source — which respawns nothing on a failed probe.
    if [ -n "$(cockpit_windows)" ]; then
        restart_spira_collector_if_stale
    fi
    # Loom is not a cockpit component — rebuild and restart regardless of cockpit state.
    rebuild_loom_if_stale
    restart_loom_if_stale
    ;;

status)
    adopt_untagged
    echo "window:    $WINDOW ($(panes_in) panes)"
    sess="$(session_pane)"
    echo "session:   ${sess:-MISSING — run 'layout.sh up' to restore it}"
    p="$(tagged health)"
    printf '%-10s %s\n' "health:" "${p:-absent}"
    p="$(tagged mail)"
    if [ -z "$MAIL_CMD" ]; then p="off (COCKPIT_MAIL is empty or not on PATH)"; fi
    printf '%-10s %s\n' "mail:" "${p:-absent}"
    # The collector is not this script's to report on: systemctl is the authority,
    # and a second opinion here would drift. FROM $SPIRA_RUN, NOT DERIVED — the same
    # reason health.sh reads it from there.
    _svc="spira-cockpit${SPIRA_INSTANCE:+-$SPIRA_INSTANCE}.service"
    if [ -f "$SPIRA_RUN/cockpit.env" ]; then
        echo "snapshot:  $(( $(date +%s) - $(stat -c %Y "$SPIRA_RUN/cockpit.env") ))s old ($_svc)"
    else
        echo "snapshot:  missing — check 'systemctl --user status $_svc'"
    fi
    ;;

*) sed -n '3,12p' "$0" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
