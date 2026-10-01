#!/usr/bin/env bash
#
# publish-backlog.sh — the unpublished-backlog alarm for every queue.local repo (row 5 of
# the local/main design). Compares a repo's local landing ref (spira_landref) against its
# forge target (spira_publish_forge) and alarms the concierge mailbox when the unpublished
# range holds more than SPIRA_LOCAL_BACKLOG_COUNT commits, or when the oldest of them is
# older than SPIRA_LOCAL_BACKLOG_AGE seconds — configurable in spira.toml, default 50
# commits / 3h. A queue.forge repo is skipped outright: this alarm exists only because
# queue.local's publish queue is asynchronous, and a repo that publishes on merge has no
# backlog to measure.
#
#   publish-backlog.sh            one scan; same as --show
#   publish-backlog.sh --show     one scan; prints to stdout
#   publish-backlog.sh watch [--interval S] [--ticks N]   loop forever (or N times); watchd daemon
#   publish-backlog.sh health     exit non-zero when the watch loop has stopped polling
#
# TRANSITIONS, NOT STATE (pr-notify.sh's own rule). A repo whose backlog was already over
# threshold last tick and still is emits nothing new — only the crossing, in either
# direction, is worth an interruption. Mailing on every tick a backlog stays over threshold
# would make the alarm the very thing law-alerts-must-be-actionable warns against: a siren
# nobody can afford to read.
#
# EVERY CROSSING IS ALSO MAILED to the concierge mailbox directly (--kind event), not only
# printed — delivery this way does not depend on a session holding an in-session Monitor or
# on the watch-notify escalation timer, same reasoning as pr-notify.sh's _report.

# covers: spira/publish-backlog.sh spira/watchers spira/conf.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

STATE_DIR="$SPIRA_RUN/watchd/publish-backlog-state"
HEALTH_FILE="$SPIRA_RUN/watchd/publish-backlog.health"

_pb_state_file() { printf '%s/%s' "$STATE_DIR" "$1"; }

_emit() { printf '%s\n' "$1"; }

# _report <line> — print it (the watchd log, for history) and mail it straight to the
# concierge mailbox, same shape as pr-notify.sh's own _report.
_report() {
    local line="$1"
    _emit "$line"
    mail send concierge \
        --from "Publish Backlog <publish-backlog@spira>" \
        --subject "publish-backlog: $line" \
        --kind event <<MAILEOF >/dev/null || printf 'publish-backlog: mail send failed for: %s\n' "$line" >&2
## Event
$line
MAILEOF
}

# _pb_arrival_ts <repo> <base> <sha> -> the unix time <sha> first became reachable from
# <base>, read from <base>'s own reflog (git log -g --date=unix). land-local's update-ref
# writes that reflog entry itself, so its timestamp is when the commit ARRIVED on the
# landing ref — not git log %at, which is the author date, hours earlier. Non-zero, never a
# guess, when the reflog has nothing to say (law-a-control-that-cannot-check-must-refuse):
# `-g` and `--reverse` cannot combine, so entries are read newest-first into an array and
# walked backwards to find the earliest (oldest) one that already contains <sha>.
_pb_arrival_ts() {
    local repo="$1" base="$2" sha="$3" i entry_sha gd ts lines
    mapfile -t lines < <(git -C "$repo" log -g --date=unix --format='%H %gd' "$base" 2>/dev/null)
    for (( i=${#lines[@]}-1; i>=0; i-- )); do
        read -r entry_sha gd <<< "${lines[$i]}"
        [ -n "$entry_sha" ] || continue
        ts="${gd##*\{}"; ts="${ts%\}}"
        case "$ts" in ''|*[!0-9]*) continue ;; esac
        git -C "$repo" merge-base --is-ancestor "$sha" "$entry_sha" 2>/dev/null || continue
        printf '%s\n' "$ts"
        return 0
    done
    return 1
}

# _pb_backlog <name> <repo> -> "<count> <age> <head>" for the unpublished range
# (forge-tip, local-tip], or non-zero if it could not be computed — a tick that cannot
# check skips the repo rather than reporting a zero backlog
# (law-a-control-that-cannot-check-must-refuse). <age> is how long the oldest unpublished
# commit has sat on the landing ref, from _pb_arrival_ts — not how long ago it was authored.
_pb_backlog() {
    local name="$1" repo="$2" base remote branch forge_sha head_sha count oldest_sha arrival_ts now
    base="$(spira_landref "$repo" 2>/dev/null)" || return 1
    # queue.local's ref is a bare local branch (land-local and publish both refuse a
    # remote-tracking one) — same guard queue.sh's own land-local/publish apply.
    ref_remote "$base" "$repo" >/dev/null 2>&1 && return 1
    read -r remote branch < <(spira_publish_forge "$name" 2>/dev/null) || return 1
    [ -n "$remote" ] && [ -n "$branch" ] || return 1
    git -C "$repo" fetch -q "$remote" "$branch" 2>/dev/null || return 1
    forge_sha="$(git -C "$repo" rev-parse -q --verify "refs/remotes/$remote/$branch" 2>/dev/null)" || return 1
    head_sha="$(git -C "$repo" rev-parse -q --verify "$base" 2>/dev/null)" || return 1
    count="$(git -C "$repo" rev-list --count "$forge_sha..$head_sha" 2>/dev/null)"
    case "$count" in ''|*[!0-9]*) return 1 ;; esac
    if [ "$count" -eq 0 ]; then
        printf '0 0 %s\n' "$head_sha"; return 0
    fi
    oldest_sha="$(git -C "$repo" log --format=%H --reverse "$forge_sha..$head_sha" 2>/dev/null | head -1)"
    [ -n "$oldest_sha" ] || return 1
    arrival_ts="$(_pb_arrival_ts "$repo" "$base" "$oldest_sha")" || return 1
    case "$arrival_ts" in ''|*[!0-9]*) return 1 ;; esac
    now="$(date +%s)"
    printf '%s %s %s\n' "$count" "$(( now - arrival_ts ))" "$head_sha"
}

# _pb_tick_repo <name> — one repo's worth of the tick: skips anything not queue.local,
# computes the backlog, and mails only on a threshold crossing (ok<->over).
_pb_tick_repo() {
    local name="$1" repo out count age head over prev statefile
    repo="$(repo_root "$name" 2>/dev/null)" || return 0
    [ -d "$repo/.git" ] || return 0
    [ "$(repo_land "$name" 2>/dev/null)" = "queue.local" ] || return 0

    out="$(_pb_backlog "$name" "$repo")" || return 0
    read -r count age head <<< "$out"

    over=0
    [ "$count" -gt "${SPIRA_LOCAL_BACKLOG_COUNT:-50}" ] && over=1
    [ "$age" -gt "${SPIRA_LOCAL_BACKLOG_AGE:-10800}" ] && over=1

    statefile="$(_pb_state_file "$name")"
    prev=ok
    [ -r "$statefile" ] && read -r prev < "$statefile" 2>/dev/null
    case "$prev" in over) ;; *) prev=ok ;; esac

    if [ "$over" -eq 1 ] && [ "$prev" != over ]; then
        _report "OVER $name: $count unpublished commit(s), oldest ${age}s old (threshold ${SPIRA_LOCAL_BACKLOG_COUNT:-50} commits / ${SPIRA_LOCAL_BACKLOG_AGE:-10800}s) — the publish queue is falling behind"
    elif [ "$over" -eq 0 ] && [ "$prev" = over ]; then
        _report "CLEAR $name: back under threshold ($count unpublished, oldest ${age}s)"
    fi

    mkdir -p "$STATE_DIR"
    printf '%s\n' "$([ "$over" -eq 1 ] && echo over || echo ok)" \
        > "$statefile.tmp" && mv "$statefile.tmp" "$statefile"
}

# _pb_tick — one pass over every repo in the repo-map.
_pb_tick() {
    local name
    while IFS= read -r name; do
        [ -n "$name" ] || continue
        _pb_tick_repo "$name"
    done < <(repo_names 2>/dev/null)
}

_pb_write_health() {
    local interval="$1"
    mkdir -p "$(dirname "$HEALTH_FILE")"
    printf 'ok %s %s\n' "$(date +%s)" "$interval" > "$HEALTH_FILE.tmp" \
        && mv "$HEALTH_FILE.tmp" "$HEALTH_FILE"
}

# cmd_health — the watchd health probe. Fails when the watch loop has never polled, or its
# last poll is older than three intervals (it is hung or dead).
cmd_health() {
    [ -r "$HEALTH_FILE" ] || {
        echo "publish-backlog: never polled ($HEALTH_FILE absent)" >&2
        return 1
    }
    local st t iv now age
    read -r st t iv < "$HEALTH_FILE"
    case "$t" in ''|*[!0-9]*) t=0 ;; esac
    case "$iv" in ''|*[!0-9]*) iv=900 ;; esac
    now="$(date +%s)"
    age=$(( now - t ))
    if [ "$age" -gt $(( 3 * iv )) ]; then
        echo "publish-backlog: last poll ${age}s ago (interval ${iv}s) — the watcher is hung or dead" >&2
        return 1
    fi
    return 0
}

# cmd_watch [--interval S] [--ticks N] — the watchd daemon body. Runs forever (or --ticks
# times, for a test), scanning every repo and writing a health record each pass.
cmd_watch() {
    local interval=900 ticks="" tick=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --interval) interval="${2:?--interval needs a value}"; shift 2 ;;
            --ticks)    ticks="${2:?--ticks needs a value}"; shift 2 ;;
            *) echo "publish-backlog.sh: watch: unknown argument '$1'" >&2; return 2 ;;
        esac
    done
    while :; do
        _pb_tick
        _pb_write_health "$interval"
        tick=$((tick + 1))
        [ -n "$ticks" ] && [ "$tick" -ge "$ticks" ] && return 0
        sleep "$interval"
    done
}

# SOURCEABLE, AND SILENT WHEN IT IS: a T1 test wanting only _pb_backlog or _pb_tick_repo
# (pure functions once the repo-map and fixture repos are set up) would otherwise trigger a
# live repo-map scan on source — the same seam pr-notify.sh and watchd already open.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    case "${1:-}" in
        --show|-s|"") _pb_tick ;;
        watch)  shift; cmd_watch "$@" ;;
        health) cmd_health; exit $? ;;
        *) echo "usage: publish-backlog.sh [--show] | watch [--interval S] [--ticks N] | health" >&2
           exit 2 ;;
    esac
fi
