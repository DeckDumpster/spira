#!/usr/bin/env bash
#
# dwarf-watch.sh — one status line per live dwarf, plus a flag line for each fleet fault:
# STALL, GATE, TESTENV, CHURN, REOPENS. A watchd daemon.
#
#   dwarf-watch.sh [--show]                       one pass to stdout
#   dwarf-watch.sh watch [--interval S] [--ticks N]
#   dwarf-watch.sh health
#
# STATUS ONLY: log mtimes, process ages, systemd, the claim pidfiles and the dwarf ledger. A dwarf's
# session transcript is its work and is never read. A process is matched on comm and printed as
# pid, age and unit — never its argv, which carries credentials.
#
# covers: spira/dwarf-watch.sh spira/watchers
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
HEALTH_FILE="$SPIRA_RUN/watchd/dwarf-watch.health"
PROC="${DWARF_WATCH_PROC:-/proc}"
LEDGER="${SPIRA_DWARF_LEDGER:-$SPIRA_RUN/dwarf-ledger.log}"
STALL_MIN="${DWARF_WATCH_STALL_MIN:-15}"
GATE_MIN="${DWARF_WATCH_GATE_MIN:-5}"
TEST_MIN="${DWARF_WATCH_TEST_MIN:-15}"
CHURN_MIN_COUNT="${DWARF_WATCH_CHURN_COUNT:-5}"
REOPEN_MIN_COUNT="${DWARF_WATCH_REOPEN_COUNT:-3}"
WINDOW="${DWARF_WATCH_WINDOW:-600}"
ID_PREFIX="${SPIRA_ID_PREFIX:-sp}"

_aw_unit_of() {
    sed -n 's#.*/\([^/]*\.service\).*#\1#p' "$PROC/$1/cgroup" 2>/dev/null | head -1
}

_aw_count() {
    local since
    since="$(date -u -d "-${WINDOW} sec" +%Y-%m-%dT%H:%M:%SZ)"
    awk -v since="$since" -v pat="$1" '$1 >= since && index($0, pat) { n++ } END { print n + 0 }' "$LEDGER" 2>/dev/null
}

_aw_claim_of() {
    local pf pid
    for pf in "$SPIRA_RUN"/dwarf-*-"$ID_PREFIX"-*.pid; do
        [ -f "$pf" ] || continue
        pid="$(cat "$pf" 2>/dev/null)"
        [ -n "$pid" ] || continue
        if [ "$(_aw_unit_of "$pid")" = "$1" ]; then
            printf '%s\n' "${pf##*/}"
            return
        fi
    done
}

_aw_tick() {
    local now ts u n=0 unknown=0 st stS pfname role name bead rune age idle flag
    now="$(date +%s)"; ts="$(date -u +%H:%MZ)"
    local lines=""
    while read -r u; do
        [ -n "$u" ] || continue
        n=$((n + 1))
        st="$(systemctl --user show "$u" -p ActiveEnterTimestamp --value 2>/dev/null)"
        stS="$(date -d "$st" +%s 2>/dev/null || echo "$now")"
        pfname="$(_aw_claim_of "$u")"
        bead="${pfname#dwarf-*-}"; bead="${bead%.pid}"
        [ -n "$pfname" ] || bead=""
        role="${pfname#dwarf-}"; role="${role%%-*}"
        name="$(cat "$SPIRA_RUN/${pfname%.pid}.name" 2>/dev/null)"
        rune="${role:-?}/${name:-?}"
        age=$(( (now - stS) / 60 )); idle="?"; flag=""
        if [ -n "$bead" ] && [ -f "$SPIRA_RUN/$bead.log" ]; then
            idle=$(( (now - $(stat -c %Y "$SPIRA_RUN/$bead.log")) / 60 ))
            [ "$idle" -gt "$STALL_MIN" ] && flag=" STALL(log idle ${idle}m > ${STALL_MIN}m)"
        fi
        if [ -z "$bead" ]; then flag=" (no claim seen yet)"; unknown=$((unknown + 1)); fi
        lines="$lines
  ${rune} ${bead:-?}: ${age}m on it, log idle ${idle}m$flag"
    done < <(systemctl --user list-units 'spira-dwarf-*' --state=active --no-legend --plain 2>/dev/null | awk '{print $1}')

    local pid et comm m long=""
    [ "$unknown" -ge "$CHURN_MIN_COUNT" ] && long="
  THRASH $unknown of $n live dwarves have no claim yet — builders exiting within seconds, the churn seen from the dwarf side"
    while read -r pid et comm; do
        [ -n "$pid" ] || continue
        m=$(( et / 60 ))
        case "$comm" in
            gate)    [ "$m" -gt "$GATE_MIN" ] && long="$long
  GATE pid $pid running ${m}m (> ${GATE_MIN}m) in $(_aw_unit_of "$pid")" ;;
            testenv) [ "$m" -gt "$TEST_MIN" ] && long="$long
  TESTENV pid $pid running ${m}m (> ${TEST_MIN}m) in $(_aw_unit_of "$pid")" ;;
        esac
    done < <(ps -eo pid=,etimes=,comm= 2>/dev/null | awk '$3=="gate"||$3=="testenv"')

    local churn reopen
    churn="$(_aw_count ' awake builder idle')"
    reopen="$(_aw_count ' reopened ')"
    [ "$churn" -ge "$CHURN_MIN_COUNT" ] && long="$long
  CHURN $churn builders exited 'nothing ready to claim' in the last $((WINDOW / 60))m"
    [ "$reopen" -ge "$REOPEN_MIN_COUNT" ] && long="$long
  REOPENS $reopen fast-tier refusals in the last $((WINDOW / 60))m (check for a base-wide cause)"

    echo "[$ts] dwarves live: $n$lines$long"
}

_aw_write_health() {
    mkdir -p "$(dirname "$HEALTH_FILE")"
    printf 'ok %s %s\n' "$(date +%s)" "$1" > "$HEALTH_FILE.tmp" \
        && mv "$HEALTH_FILE.tmp" "$HEALTH_FILE"
}

cmd_health() {
    [ -r "$HEALTH_FILE" ] || {
        echo "dwarf-watch: never polled ($HEALTH_FILE absent)" >&2
        return 1
    }
    local st t iv age
    read -r st t iv < "$HEALTH_FILE"
    case "$t" in ''|*[!0-9]*) t=0 ;; esac
    case "$iv" in ''|*[!0-9]*) iv=600 ;; esac
    age=$(( $(date +%s) - t ))
    if [ "$age" -gt $(( 3 * iv )) ]; then
        echo "dwarf-watch: last poll ${age}s ago (interval ${iv}s) — the watcher is hung or dead" >&2
        return 1
    fi
}

cmd_watch() {
    local interval=600 ticks="" tick=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --interval) interval="${2:?--interval needs a value}"; shift 2 ;;
            --ticks)    ticks="${2:?--ticks needs a value}"; shift 2 ;;
            *) echo "dwarf-watch.sh: watch: unknown argument '$1'" >&2; return 2 ;;
        esac
    done
    WINDOW="${DWARF_WATCH_WINDOW:-$interval}"
    while :; do
        _aw_tick
        _aw_write_health "$interval"
        tick=$((tick + 1))
        [ -n "$ticks" ] && [ "$tick" -ge "$ticks" ] && return 0
        sleep "$interval"
    done
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    case "${1:-}" in
        --show|-s|"") _aw_tick ;;
        watch)  shift; cmd_watch "$@" ;;
        health) cmd_health; exit $? ;;
        *) echo "usage: dwarf-watch.sh [--show] | watch [--interval S] [--ticks N] | health" >&2
           exit 2 ;;
    esac
fi
