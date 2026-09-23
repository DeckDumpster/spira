#!/usr/bin/env bash
#
# world.sh — stop and start Spira as a whole.
#
#   world.sh stop [--why "..."]   halt the loop: no summons, no landing, no live aeons
#   world.sh start                bring it back
#   world.sh drain [--timeout N | --deadline N]  no NEW aeons; loop and landing keep running until the pool empties
#   world.sh resume               lift a drain
#   world.sh status               what is up, what is down, what is running
#
# WHY THIS EXISTS. Stopping Spira meant remembering six unit names and then hunting aeon
# processes by hand, so in practice nobody stopped it — they stopped ONE timer and left the
# rest running, or killed a runner and left its model session headless. A control that is a
# checklist is a control nobody uses in the moment they need it (2026-09-07: the loop spent
# two days re-summoning aeons onto a bead that could not finish, and halting it took a
# session of archaeology).
#
# WHAT IT DOES NOT TOUCH, deliberately:
#   dolt-beads*.service   the databases. Stopping the world must never risk the data, and a
#                         stopped server makes every diagnostic you are about to run fail.
#   cockpit*, concierge   the panes the operator is reading. Halting the loop must not also
#                         blind the person halting it.
# `stop --hard` adds the watchers; nothing here ever stops Dolt.
set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"

# systemctl behind a seam so test suites can stub it without reaching the box.
# The same seam sentinel.sh carries, named the same way so one variable stubs both.
SC="${SPIRA_SYSTEMCTL:-systemctl}"

# The loop, in the order that stops cleanly: summons first so nothing new is born, then the
# legs that act on what is already there.
#
# TIMER NAMES ARE INSTANCE-QUALIFIED after install.sh's per-instance migration (sp-trn1).
# Each base is tried in instance-qualified form first (spira-<base>-<instance>.timer); if
# neither is-enabled nor is-active confirms it exists, the plain name is used as a fallback.
# This makes world.sh correct before a migration and after. c0e2f8c applied the same shape
# to cockpit.sh and health.sh when the rename broke them identically (sp-4biz, 2026-09-08).
# TIMERS ARE ENUMERATED FROM SYSTEMD, NOT WRITTEN DOWN. This was a hand-written list of six
# bases, and on 2026-09-11 a stop left spira-groom, spira-maechen, spira-auron, spira-promote,
# spira-moot-sweep, spira-verify-asks, spira-suites and both spira-watch-* timers running —
# every persona added after the list was written. work_services() one function below already
# enumerates for exactly this reason ("a new work unit cannot silently escape a halt"); the
# timers simply never got the same treatment. A list is a thing that goes stale silently.
#
# ORDER IS STILL LOAD-BEARING: summons first so nothing new is born, then the legs that act on
# what already exists, then everything else systemd reports. Anything not named in the priority
# list still gets stopped — it just stops after the ones whose order matters.
TIMER_PRIORITY=(spira-sentinel spira-ops spira-watchtower spira-archivist spira-archive spira-skew)
# Stopped only on --hard; a plain halt leaves them running (summon nothing, create no worktree).
CI_WATCHER_BASES=(spira-gate-check spira-pr-notify)
_inst_sfx="${SPIRA_INSTANCE:+-$SPIRA_INSTANCE}"
TIMERS=()
_timer_seen=" "
_timer_add() {   # _timer_add <unit>
    case "$_timer_seen" in *" $1 "*) return 0 ;; esac
    _timer_seen="$_timer_seen$1 "
    TIMERS+=("$1")
}
for _b in "${TIMER_PRIORITY[@]}"; do
    _i="${_b}${_inst_sfx}.timer"
    if "$SC" --user is-enabled "$_i" >/dev/null 2>&1 ||
       "$SC" --user is-active  "$_i" >/dev/null 2>&1; then
        _timer_add "$_i"
    else
        _timer_add "${_b}.timer"
    fi
done
# Everything else systemd knows about, loaded or not — --all so a stopped-but-enabled timer
# is still named, and so `start` has the same population to work from.
while IFS= read -r _u; do
    [ -n "$_u" ] || continue
    _timer_add "$_u"
done < <("$SC" --user list-unit-files 'spira-*.timer' --no-legend 2>/dev/null | awk '{print $1}'
         "$SC" --user list-units      'spira-*.timer' --all --no-legend 2>/dev/null | awk '{print $1}')
unset _b _i _u _inst_sfx
_is_ci_watcher() {
    local b
    for b in "${CI_WATCHER_BASES[@]}"; do
        case "$1" in "$b".timer|"$b"-*.timer) return 0 ;; esac
    done
    return 1
}
STAMP="$SPIRA_RUN/world.halted"
DRAIN_STAMP="$SPIRA_RUN/world.draining"

# argv_has <pid-dir> <path>... -> 0 when one of the paths is an EXACT argv element.
#
# Substring-matching the whole command line nominates any shell whose -c text merely MENTIONS
# the path — the /proc form of law-pgrep-nominates. An operator session discussing
# $SPIRA_PROD/aeon.sh made `world.sh status` report three live aeons when one was running.
# argv elements are NUL-separated, so an exact element test costs nothing and cannot be
# fooled by a script that quotes the path.
argv_has() {
    local d="$1"; shift
    local tok want
    while IFS= read -r -d '' tok; do
        for want in "$@"; do [ "$tok" = "$want" ] && return 0; done
    done < "$d/cmdline" 2>/dev/null
    return 1
}

# live_aeons -> "<pid> <unit>" per running aeon, resolved from /proc argv.
# NEVER pgrep -f: the pattern is a substring of this script's own command line.
live_aeons() {
    local p c pid
    for p in /proc/[0-9]*; do
        # -r first: a pid can exit between the glob and the read, and the shell reports the
        # redirection failure before `tr`'s own 2>/dev/null can suppress it.
        [ -r "$p/cmdline" ] || continue
        c="$(tr '\0' ' ' < "$p/cmdline" 2>/dev/null)" || continue
        # MATCH THE EXECUTING COPY, NOT THE ONE THIS SCRIPT LIVES IN. In split-checkout mode
        # every aeon runs $SPIRA_PROD/aeon.sh while SPIRA_HOME is the development checkout, so
        # matching SPIRA_HOME alone made this blind to every real aeon: on 2026-09-11 `stop`
        # printed "no live aeons" with four running. law-verify-through-the-executing-copy.
        argv_has "$p" "$SPIRA_HOME/aeon.sh" "${SPIRA_PROD:-$SPIRA_HOME}/aeon.sh" || continue
        pid="${p#/proc/}"
        printf '%s %s\n' "$pid" "$("$SC" --user status "$pid" 2>/dev/null | head -1 | awk '{print $2}')"
    done
}

# work_services -> one service unit name per line, for the active spira-*.service units that
# execute work. Enumerated from what systemd reports rather than from a hand-written list, so
# a new work unit cannot silently escape a halt.
#
# EXCLUDED DELIBERATELY:
#   spira-cockpit[-<instance>].service   the ops dashboard the operator is reading
#   spira-loom[-<instance>].service      the Loom query engine the dashboard depends on
#   spira-watch@*.service                handled separately by --hard
# Both the bare (pre-migration) and instance-qualified forms are matched so the pattern
# holds during and after the per-instance migration (sp-4biz: a rename repoints no reader;
# sp-xfg4: the bare-name exclusion here let the collector be swept on every stop).
# Dolt is never named spira-*; the exclusions above are the only ones needed.
work_services() {
    local sfx="${SPIRA_INSTANCE:+-$SPIRA_INSTANCE}"
    "$SC" --user list-units 'spira-*.service' --state=active --no-legend 2>/dev/null \
        | awk '{print $1}' \
        | grep -Ev "^spira-cockpit(\\.service|${sfx}\\.service)$|^spira-loom(\\.service|${sfx}\\.service)$|^spira-watch@"
}

# live_workers -> one pid per line for any process running gate.sh or landing.sh from this
# home. These are the processes that survive a stop of spira-landing.service if it was killed
# before they finished — the evidence that the halt was incomplete.
# NEVER pgrep -f: the pattern is a substring of this script's own command line.
live_workers() {
    local p c
    for p in /proc/[0-9]*; do
        [ -r "$p/cmdline" ] || continue
        c="$(tr '\0' ' ' < "$p/cmdline" 2>/dev/null)" || continue
        # Both homes, for the same reason live_aeons matches both.
        if argv_has "$p" "$SPIRA_HOME/gate.sh" "$SPIRA_HOME/landing.sh" \
                        "${SPIRA_PROD:-$SPIRA_HOME}/gate.sh" "${SPIRA_PROD:-$SPIRA_HOME}/landing.sh"
        then printf '%s\n' "${p#/proc/}"; fi
    done
}

case "${1:-status}" in
stop)
    shift; why=""
    [ "${1:-}" = "--why" ] && { why="${2:-}"; shift 2; }
    hard=0; [ "${1:-}" = "--hard" ] && hard=1

    echo "spira: halting the loop"
    for t in "${TIMERS[@]}"; do
        [ "$hard" = 0 ] && _is_ci_watcher "$t" && continue
        "$SC" --user stop "$t" 2>/dev/null && printf '  stopped %s\n' "$t"
    done
    # WATCHER UNITS ARE INSTANCE-QUALIFIED after the per-instance migration. The template
    # form (spira-watch@<name>.service) was renamed to spira-watch-<name>-<instance>.service;
    # list-units 'spira-watch@*' finds nothing after that rename. Both forms are queried so
    # --hard works during a migration and after (sp-4biz, 2026-09-08).
    if [ "$hard" = 1 ]; then
        for u in $(
            {
                "$SC" --user list-units 'spira-watch@*' --no-legend 2>/dev/null
                "$SC" --user list-units \
                      "spira-watch-*${SPIRA_INSTANCE:+-$SPIRA_INSTANCE}.service" \
                      --state=active --no-legend 2>/dev/null
            } | awk '{print $1}'
        ); do
            "$SC" --user stop "$u" 2>/dev/null && printf '  stopped %s\n' "$u"
        done
    fi

    # WORK SERVICES execute work the timers do not — spira-landing is the critical one, because
    # it runs the same gate.sh passes an aeon does and survives a timer stop. Discovered from
    # what systemd reports rather than from a hand-written list (work_services above), so a new
    # unit cannot silently escape.
    svc_failed=0
    while IFS= read -r svc; do
        [ -n "$svc" ] || continue
        if "$SC" --user stop "$svc" 2>/dev/null; then
            printf '  stopped %s\n' "$svc"
        else
            printf '  WARNING: could not stop %s — still running\n' "$svc" >&2
            svc_failed=1
        fi
    done < <(work_services)

    # AEONS ARE STOPPED THROUGH slay.sh, not killed. It writes the marker that makes the
    # aeon's own exit path release its bead with NO ATTEMPT CHARGED — a bare kill leaves the
    # bead held until the lease expires and charges the attempt anyway, which is how a halt
    # for an unrelated reason walks a bead toward poison.
    n=0
    # FROM THE PIDFILE, NOT THE ENVIRONMENT. BEAD_ID is a shell variable in aeon.sh and was
    # never exported, so reading /proc/<pid>/environ for it returns empty for every live aeon
    # — and a loop that only counts what it found then reported "no live aeons" while four
    # were running. The pidfile is named aeon-<fayth>-<bead>.pid and is the harness's own
    # record of the pairing.
    for pf in "$SPIRA_RUN"/aeon-*.pid; do
        [ -e "$pf" ] || continue
        pid="$(cat "$pf" 2>/dev/null)"
        [ -n "$pid" ] && [ -d "/proc/$pid" ] || { rm -f "$pf"; continue; }
        bead="$(basename "$pf" .pid)"; bead="${bead#aeon-}"; bead="${bead#*-}"
        if [ -n "$bead" ]; then
            printf '  slaying %s (pid %s)\n' "$bead" "$pid"
            "$SPIRA_HOME/slay.sh" --bead "$bead" --keep-work --why "${why:-the world was stopped}" >/dev/null 2>&1 \
                || printf '    slay.sh could not stop %s — left running, say so rather than pretend\n' "$bead"
            n=$((n+1))
        fi
    done
    # RE-READ PIDFILES now — after the timers are stopped — to resolve each surviving process
    # to a bead. An aeon spawned between the timer stop and the first scan may have written
    # its pidfile by this point; it is not an orphan, it is a late starter the first pass
    # missed. A warning that fires on a routine race is one nobody reads on the day it is real.
    stray=0
    while IFS=' ' read -r apid aunit; do
        bead_for_pid=""
        for pf in "$SPIRA_RUN"/aeon-*.pid; do
            [ -e "$pf" ] || continue
            pp="$(cat "$pf" 2>/dev/null)" || continue
            if [ "$pp" = "$apid" ]; then
                bb="$(basename "$pf" .pid)"; bb="${bb#aeon-}"; bb="${bb#*-}"
                bead_for_pid="$bb"
                break
            fi
        done
        if [ -n "$bead_for_pid" ]; then
            printf '  WARNING: %s (pid %s) — named by a pidfile but slay did not stop it; run: slay.sh --bead %s\n' \
                "$bead_for_pid" "$apid" "$bead_for_pid"
        else
            printf '  WARNING: pid %s (%s) — no pidfile names it; could not resolve to a bead — inspect /proc/%s/cmdline before killing\n' \
                "$apid" "${aunit:--}" "$apid"
        fi
        stray=$((stray+1))
    done < <(live_aeons)
    [ "$n" = 0 ] && [ "$stray" = 0 ] && echo "  no live aeons"

    mkdir -p "$SPIRA_RUN"
    { date -u '+%Y-%m-%dT%H:%M:%SZ'; printf 'why: %s\n' "${why:-unstated}"; } > "$STAMP"

    # A HALT THAT CANNOT STOP SOMETHING SAYS SO AND EXITS NON-ZERO. Printing STOPPED while a
    # worker is still running was the bug that made this bead necessary: the operator halted,
    # saw the success message, and three gate.sh processes on spira-landing.service went on
    # saturating the tree lock the halt was meant to clear.
    if [ "$svc_failed" = 1 ]; then
        printf 'spira: stop INCOMPLETE — work service(s) could not be stopped (see warnings above)\n' >&2
        exit 1
    fi
    echo "spira: STOPPED. Dolt and the cockpit are untouched. Restart with: world.sh start"
    ;;

start)
    echo "spira: starting the loop"
    for t in "${TIMERS[@]}"; do
        # Derive the subject: strip .timer, then the instance suffix if set.
        # ctrl.sh and systemd both record by subject (e.g. spira-ops), not by
        # unit name (spira-ops-prod.timer).
        _subj="${t%.timer}"
        [ -n "${SPIRA_INSTANCE:-}" ] && _subj="${_subj%-$SPIRA_INSTANCE}"
        # A ctrl-suspended timer: the operator recorded a reason not to run it.
        # start is the one command most likely to run in the middle of a recovery,
        # and silently reversing a recorded decision there is the defect (sp-cqyfy).
        if "$SPIRA_HOME/ctrl.sh" check "$_subj" 2>/dev/null; then
            _reason="$("$SPIRA_HOME/ctrl.sh" reason "$_subj" 2>/dev/null)"
            printf '  skipped %s (suspended%s)\n' "$t" "${_reason:+: $_reason}"
            continue
        fi
        # A disabled timer was removed from the loop deliberately. start restores
        # the loop; a disabled timer was never part of it.
        if [ "$("$SC" --user is-enabled "$t" 2>/dev/null)" = "disabled" ]; then
            printf '  skipped %s (disabled)\n' "$t"
            continue
        fi
        "$SC" --user start "$t" 2>/dev/null && printf '  started %s\n' "$t"
    done

    # START WHAT `stop --hard` STOPPED. The watchers are long-running SERVICES, not
    # timers, so the loop above never touched them and for a long time `start` left
    # them exactly as `stop --hard` had: dead.
    #
    # Restart=always does not cover this and cannot. systemd deliberately does not
    # revive a unit an operator stopped on purpose -- the unit reports
    # Result=success and stays inactive -- so the policy that looks like a safety
    # net here is inert precisely on the path that needs it. watch-refresh.sh skips
    # inactive units for the same reason and says so in a comment, which means
    # nothing in the system was going to bring these back.
    #
    # THE COST IS SILENT AND IT IS THE WORST KIND. The answers watcher is the leg
    # that carries the operator's verdict back out of the cockpit. With it dead the
    # operator answers, the bead closes, and no session ever learns of it: they
    # believe they have replied and the work does not move. Nine answers sat
    # undelivered for eight hours this way. An escalation whose reply reaches nobody
    # is worse than an unanswered one (law-answers-need-a-delivery-path).
    #
    # Queried from systemd rather than from a list here, matching how `stop` finds
    # them, so a watcher added later cannot escape being restarted. Enabled-but-
    # inactive is the state we are repairing; --state is therefore not filtered.
    for u in $(
        {
            "$SC" --user list-unit-files 'spira-watch@*' --no-legend 2>/dev/null
            "$SC" --user list-unit-files \
                  "spira-watch-*${SPIRA_INSTANCE:+-$SPIRA_INSTANCE}.service" \
                  --no-legend 2>/dev/null
        } | awk '{print $1}' | sort -u
    ); do
        # SKIP THE ONESHOTS BY TYPE, NEVER BY NAME. The timer-driven watchers
        # (refresh, notify) are Type=oneshot and their TIMERS are already in the
        # loop above; starting the service directly would fire one pass out of
        # band. Naming them here instead would be a second hand-written list that
        # a watcher added later escapes silently -- which is the shape of the bug
        # this whole block exists to fix.
        [ "$("$SC" --user show "$u" -p Type --value 2>/dev/null)" = oneshot ] && continue
        [ "$("$SC" --user is-active "$u" 2>/dev/null)" = active ] && continue
        "$SC" --user start "$u" 2>/dev/null && printf '  started %s\n' "$u"
    done

    # START WHAT a system halt stopped. spira-mail-deliver is not a watcher unit and is not
    # stopped by `world.sh stop`, but a system halt's SIGTERM leaves it inactive (not failed,
    # because SuccessExitStatus=143 is set in the unit) and nothing else restores it.
    for u in $(
        {
            "$SC" --user list-unit-files \
                  "spira-mail-deliver${SPIRA_INSTANCE:+-$SPIRA_INSTANCE}.service" \
                  --no-legend 2>/dev/null
            "$SC" --user list-unit-files "spira-mail-deliver.service" \
                  --no-legend 2>/dev/null
        } | awk '{print $1}' | sort -u
    ); do
        _subj="${u%.service}"; _subj="${_subj%.timer}"
        [ -n "${SPIRA_INSTANCE:-}" ] && _subj="${_subj%-$SPIRA_INSTANCE}"
        if "$SPIRA_HOME/ctrl.sh" check "$_subj" 2>/dev/null; then
            _reason="$("$SPIRA_HOME/ctrl.sh" reason "$_subj" 2>/dev/null)"
            printf '  skipped %s (suspended%s)\n' "$u" "${_reason:+: $_reason}"
            continue
        fi
        [ "$("$SC" --user is-enabled "$u" 2>/dev/null)" = "disabled" ] && continue
        [ "$("$SC" --user is-active  "$u" 2>/dev/null)" = "active"   ] && continue
        "$SC" --user start "$u" 2>/dev/null && printf '  started %s\n' "$u"
    done

    rm -f "$STAMP"
    echo "spira: RUNNING"
    ;;

# DRAIN IS NOT A SOFTER STOP — IT IS A DIFFERENT SHAPE. `stop` halts the timers, the work
# services and the live aeons, because it exists to make everything quiet. `drain` closes the
# door and lets the room empty: no NEW aeon is summoned, while the loop, landing and reaping
# all keep running so an aeon already working can finish and LAND what it built.
#
# NOTHING IS STOPPED, so there is no SERVICE to forget to restart. THE STAMP IS STILL A THING
# TO FORGET, and it was forgotten: an Ops sweep applying sop-harness-checkout-behind drained
# at 18:02:03 on 2026-09-09, finished its SOP at 18:04:29, and exited without resuming. The
# world stayed gated for 59 minutes with 25 beads ready and zero aeons, and every pass
# reported "pass complete — goal reached" while doing so. The sentinel was behaving correctly
# — a drain is a mode, not a fault — so nothing anywhere treated it as one.
#
# SO THE DRAIN EXPIRES. It carries a deadline and the gate lifts it when that passes, loudly.
# A drain is a held breath: legitimate for the minutes an operation needs, never for an hour,
# and a holder that dies must not take the loop with it. `--for` sets the lifetime; the gate
# in lib.sh enforces it, so every caller is covered by one rule.
#
# NOT THE SAME AS `--timeout`, which bounds how long `drain` WAITS for live aeons to finish
# before returning. That is this command's patience; this is the gate's lifetime after the
# command has gone.
#
# AT THE DEADLINE, `--deadline N` slays every live aeon (--keep-work --reopen) so the fleet
# ends quiet. No attempt is charged — a slay the operator ordered is not evidence the aeon
# failed. `--timeout N` keeps the old warn-and-return for callers that only want to know.
# world.sh stop uses its own slay sweep for the same reason: a stop cannot proceed with
# aeons alive.
#
# The gate is a stamp file that summon_fayth() checks in lib.sh — one place, covering every
# caller. The first attempt at this stopped spira-sentinel.timer instead, which also stopped
# landing, because landing is a leg of the sentinel pass rather than a timer of its own:
# three finished branches sat unlanded for sixteen minutes (2026-09-08).
drain)
    shift; dtimeout=1800; dfor="${SPIRA_DRAIN_TTL:-1800}"; dslay=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --timeout)  dtimeout="${2:-1800}"; shift 2 ;;
            --for)      dfor="${2:-1800}";     shift 2 ;;
            --deadline) dtimeout="${2:-1800}"; dslay=1; shift 2 ;;
            *) break ;;
        esac
    done

    { date '+%Y-%m-%d %H:%M:%S %Z'
      printf 'summons gated in summon_fayth; loop and landing still running. Lift with: %s resume\n' "$0"
      printf 'expires %s\n' "$(( $(date +%s) + dfor ))"
    } > "$DRAIN_STAMP"
    echo "spira: draining — no new aeons; loop, landing and reaping continue"

    waited=0
    while :; do
        n="$(live_aeons | grep -c . || true)"
        [ "$n" -eq 0 ] && break
        if [ "$waited" -ge "$dtimeout" ]; then
            if [ "$dslay" = 1 ]; then
                printf 'spira: drain deadline reached — slaying %s aeon(s)\n' "$n" >&2
                for _pf in "$SPIRA_RUN"/aeon-*.pid; do
                    [ -e "$_pf" ] || continue
                    _pid="$(cat "$_pf" 2>/dev/null)"
                    [ -n "$_pid" ] && [ -d "/proc/$_pid" ] || { rm -f "$_pf"; continue; }
                    _abead="$(basename "$_pf" .pid)"; _abead="${_abead#aeon-}"; _abead="${_abead#*-}"
                    [ -n "$_abead" ] || continue
                    printf '  slaying %s (pid %s)\n' "$_abead" "$_pid"
                    "$SPIRA_HOME/slay.sh" --bead "$_abead" --keep-work --reopen \
                        --why "drain deadline reached" >/dev/null 2>&1 \
                        || printf '    slay.sh could not stop %s — left running\n' "$_abead" >&2
                done
                unset _pf _pid _abead
                echo "spira: DRAINED — aeons slain at deadline. Summons remain gated."
                printf 'spira: resume with: %s resume\n' "$0"
                exit 0
            fi
            printf 'spira: NOT DRAINED — %s aeon(s) still live after %ss\n' "$n" "$dtimeout" >&2
            printf 'spira: summons REMAIN GATED. Lift with: %s resume\n' "$0" >&2
            exit 1
        fi
        [ $(( waited % 60 )) -eq 0 ] && [ "$waited" -gt 0 ] && \
            printf '  %s aeon(s) still working (%ss elapsed)\n' "$n" "$waited"
        sleep 10; waited=$(( waited + 10 ))
    done

    echo "spira: DRAINED — no aeon running, summons gated"
    printf 'spira: resume with: %s resume\n' "$0"
    ;;

resume)
    if [ -f "$DRAIN_STAMP" ]; then
        rm -f "$DRAIN_STAMP"; echo "spira: summons resumed"
    else
        echo "spira: was not draining — nothing to resume"
    fi
    ;;

status)
    # A GATED SUMMON MUST NEVER BE INVISIBLE: a pool held at zero on purpose and a queue with
    # nothing in it look identical from every other surface.
    [ -f "$DRAIN_STAMP" ] && { printf 'spira: DRAINING since %s — summons gated\n' "$(head -1 "$DRAIN_STAMP")"; sed -n 2p "$DRAIN_STAMP"; }
    if [ -f "$STAMP" ]; then
        printf 'spira: HALTED since %s\n' "$(head -1 "$STAMP")"; sed -n 2p "$STAMP"
        _sfx="${SPIRA_INSTANCE:+-$SPIRA_INSTANCE}"
        _ci_watching=()
        for _b in "${CI_WATCHER_BASES[@]}"; do
            for _t in "${_b}${_sfx}.timer" "${_b}.timer"; do
                [ "$("$SC" --user is-active "$_t" 2>/dev/null)" = active ] && { _ci_watching+=("$_t"); break; }
            done
        done
        unset _sfx _b _t
        if [ "${#_ci_watching[@]}" -gt 0 ]; then
            printf 'spira: CI watchers: %s\n' "${_ci_watching[*]}"
        else
            printf 'spira: nobody is watching CI\n'
        fi
    else echo "spira: not halted by world.sh"; fi
    for t in "${TIMERS[@]}"; do
        timer_state="$("$SC" --user is-active "$t" 2>/dev/null)"
        svc="${t%.timer}.service"
        svc_result="$("$SC" --user show "$svc" -p Result --value 2>/dev/null)"
        if [ -n "$svc_result" ] && [ "$svc_result" != "success" ]; then
            printf '  %-26s %s (svc: %s)\n' "$t" "$timer_state" "$svc_result"
        else
            printf '  %-26s %s\n' "$t" "$timer_state"
        fi
    done
    printf '  %-26s %s\n' "dolt-beads.service" "$("$SC" --user is-active dolt-beads.service 2>/dev/null)"
    printf '  %-26s %s\n' "dolt-beads-test.service" "$("$SC" --user is-active dolt-beads-test.service 2>/dev/null)"

    # WORK SERVICES: spira-landing is always checked because it is a transient unit — it only
    # exists while it is running and does not appear in list-unit-files, so `is-active` is the
    # only reliable probe. Any other active spira-*.service (excluding cockpit and watch@) is
    # also reported, so a new work unit cannot hide here while appearing as HALTED above.
    printf '  %-26s %s\n' "spira-landing.service" "$("$SC" --user is-active spira-landing.service 2>/dev/null || echo inactive)"
    while IFS= read -r svc; do
        case "$svc" in spira-landing.service|spira-cockpit*.service|spira-loom*.service) continue ;; esac
        printf '  %-26s %s\n' "$svc" "$("$SC" --user is-active "$svc" 2>/dev/null)"
    done < <("$SC" --user list-units 'spira-*.service' --state=active --no-legend 2>/dev/null | awk '{print $1}' | grep -Ev '^spira-watch@')

    # /proc SCAN: gate.sh and landing.sh processes survive a service stop if the service was
    # killed before they finished. Status reports the count so a running worker cannot hide
    # behind a HALTED header and 0 live aeons.
    wcount="$(live_workers | grep -c . || true)"
    printf '  %-26s %s\n' "live workers (/proc)" "$wcount"

    a="$(live_aeons | grep -c . || true)"; printf '  live aeons: %s\n' "$a"
    ;;
*)  echo "usage: world.sh {stop [--why \"...\"] [--hard] | drain [--timeout SECS | --deadline SECS] | resume | start | status}" >&2; exit 64 ;;
esac
