#!/usr/bin/env bash
#
# cadence.sh — change how often an INSTALLED timer fires, without editing the template.
#
#   cadence.sh list                     every installed timer, its template cadence, its
#                                       effective cadence, and whether an override is in force
#   cadence.sh show <unit>              one timer in full, including its next elapse
#   cadence.sh set <unit> <interval>    override the cadence; verify it actually took
#   cadence.sh clear <unit>             drop the override; back to the template
#   cadence.sh verify [unit...]         every timer (or the named ones) has a real next elapse
#
# <unit> may be given bare (`suites`), as a service name (`spira-suites`), or fully
# (`spira-suites-prod.timer`). The instance suffix comes from SPIRA_INSTANCE.
#
# <interval> is either a systemd time span (`6h`, `90min`, `2d`) or an OnCalendar expression
# (anything containing a `:` or `*`, e.g. `*-*-* 00/6:00:00`, `Mon *-*-* 09:00:00`).
#
# ---------------------------------------------------------------------------------------
# WHY THIS EXISTS, AND WHY IT VERIFIES RATHER THAN ASSUMES
# ---------------------------------------------------------------------------------------
# Changing a cadence by hand is four steps — find which of 157 installed units is the one,
# write a drop-in with the empty-assignment trick, daemon-reload, restart — and three of the
# four fail silently.
#
# THE SCAR THIS IS BUILT FROM (2026-09-12). Asked to slow the suite runner from hourly to
# 6h, a hand-written drop-in cleared the template's `OnUnitActiveSec` and set `6h`. Both
# `daemon-reload` and `restart` exited 0. The timer was left with
# `NextElapseUSecMonotonic=infinity` — it would never have fired again. `OnUnitActiveSec`
# measures from the unit's last activation, and restarting the timer discards that
# reference; the template's `OnBootSec` would normally re-arm it, but the box had been up 27
# days, so that trigger was long past. "Slowed to 6h" and "switched off" are the same exit
# status, the same silence, and the same green `systemctl status`.
#
# So this program does three things a person doing it by hand reliably does not:
#
#   1. IT PREFERS WALL CLOCK. A bare span like `6h` is converted to an OnCalendar expression
#      wherever one exists that means the same thing, because a monotonic-only timer on a
#      long-lived host can be disarmed by the very command that installs it. A span with no
#      clean calendar equivalent keeps OnUnitActiveSec but is paired with an OnCalendar
#      backstop so there is always a next elapse.
#
#   2. IT READS THE TIMER BACK. After reloading it asserts `NextElapseUSecRealtime` is a real
#      time. If it is not, the override is REMOVED and the previous state restored, because a
#      disarmed QA timer is worse than a fast one — it reports absence as success
#      (law-absence-needs-a-positive-control).
#
#   3. IT WRITES DOWN WHY. Every drop-in carries the reason, the date, the operator, and the
#      revert line. A cadence that differs from the template with no note is indistinguishable
#      from drift, and six months on nobody can tell a deliberate slowdown from a mistake.
#
# WHY A DROP-IN AND NOT THE TEMPLATE. The templates in systemd/ are the committed defaults and
# carry their own reasoning; a cadence changed for an operational reason ("while we clean this
# up") is not a new default and must not be landed as one. A drop-in also survives a re-run of
# install.sh, which rewrites the `.timer` but not the `.timer.d/` beside it — and leaves the
# rendered unit byte-identical to the template, so `skew.sh units` stays clean.
#
# WHAT THIS DOES NOT DO. It does not enable, disable, start or stop anything. A timer that is
# off stays off and is reported as off; changing the cadence of a stopped timer is a legitimate
# thing to do before starting it, and silently starting one would be this tool deciding
# something the operator did not ask for.
# ---------------------------------------------------------------------------------------
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/lib.sh"

DROPIN_DIR="${SPIRA_SYSTEMD_USER_DIR:-$HOME/.config/systemd/user}"
DROPIN_FILE="cadence.conf"        # ours alone; other drop-ins in the same .d/ are left be
SYSTEMCTL="${SPIRA_SYSTEMCTL:-systemctl} --user"

die() { printf 'cadence: %s\n' "$*" >&2; exit 1; }
say() { printf 'cadence: %s\n' "$*" >&2; }

# ---------------------------------------------------------------------------------------
# NAMING. One unit may be written four ways; resolve to exactly one installed .timer or
# refuse. Refusing on an ambiguous prefix matters more than convenience: `spira-watch` is a
# prefix of two real units, and picking one would change a cadence the operator did not name.
# ---------------------------------------------------------------------------------------
# THE UNIT SUFFIX IS NOT conf.sh's PATH SUFFIX. conf.sh gives prod no suffix when deriving
# paths; install.sh names units the other way — "Units whose names start with 'spira-' get
# the instance suffix", prod included, so the installed unit is spira-suites-prod.timer while
# the template is systemd/spira-suites.timer. Units that do NOT start with spira- (cockpit-
# ensure, concierge, beads-push, dolt-beads) are shared across instances and carry no suffix
# at all. Reading conf.sh's rule here made every spira- template resolve to `?`.
unit_suffix() { printf -- '-%s' "${SPIRA_INSTANCE:-prod}"; }

installed_timers() {
    local f
    for f in "$DROPIN_DIR"/*.timer; do [ -f "$f" ] || continue; basename "$f"; done | sort
}

resolve_unit() {         # resolve_unit <name> -> full installed unit name, or fail
    local want="$1" sfx all hit n
    sfx="$(unit_suffix)"
    all="$(installed_timers)"
    [ -n "$all" ] || { say "no timer units installed under $DROPIN_DIR"; return 1; }

    # Exact, then with the instance suffix, then with the spira- prefix and suffix. Each is
    # tried as a WHOLE name; no prefix matching until every exact form has failed.
    local cand
    for cand in "$want" "${want}.timer" "${want}${sfx}.timer" \
                "spira-${want}${sfx}.timer" "spira-${want}.timer"; do
        case $'\n'"$all"$'\n' in *$'\n'"$cand"$'\n'*) printf '%s' "$cand"; return 0 ;; esac
    done

    # Last resort: a substring match, and ONLY when it is unique.
    hit="$(printf '%s\n' "$all" | grep -F -- "$want" || true)"
    n="$(printf '%s' "$hit" | grep -c . || true)"
    if [ "${n:-0}" -eq 1 ]; then printf '%s' "$hit"; return 0; fi
    if [ "${n:-0}" -gt 1 ]; then
        say "'$want' names more than one installed timer — say which:"
        printf '%s\n' "$hit" | sed 's/^/  /' >&2
        return 1
    fi
    say "no installed timer matches '$want'. Try: cadence.sh list"
    return 1
}

dropin_path() { printf '%s/%s.d/%s' "$DROPIN_DIR" "$1" "$DROPIN_FILE"; }

# ---------------------------------------------------------------------------------------
# THE INTERVAL. A span becomes wall clock wherever that is exactly equivalent, because a
# monotonic-only timer is the failure this tool exists to prevent.
#
# An hour count that divides 24 maps to `*-*-* 0/N:00:00` — every N hours from midnight,
# which is stable across restarts and reboots and always has a next elapse. Anything else
# keeps its span and gets a daily OnCalendar backstop, so the timer can never be left with
# no trigger at all even if its monotonic reference is discarded.
# ---------------------------------------------------------------------------------------
render_schedule() {      # render_schedule <interval> -> the [Timer] lines, on stdout
    local iv="$1" n
    # Already an OnCalendar expression: pass it through untouched.
    case "$iv" in
        *:*|*'*'*|Mon*|Tue*|Wed*|Thu*|Fri*|Sat*|Sun*|daily|hourly|weekly|monthly)
            printf 'OnUnitActiveSec=\nOnBootSec=\nOnCalendar=%s\n' "$iv"; return 0 ;;
    esac
    # A whole number of hours that divides the day.
    case "$iv" in
        *h) n="${iv%h}"
            case "$n" in ''|*[!0-9]*) ;; *)
                if [ "$n" -gt 0 ] && [ "$n" -le 24 ] && [ $(( 24 % n )) -eq 0 ]; then
                    if [ "$n" -eq 24 ]; then
                        printf 'OnUnitActiveSec=\nOnBootSec=\nOnCalendar=*-*-* 00:00:00\n'
                    else
                        printf 'OnUnitActiveSec=\nOnBootSec=\nOnCalendar=*-*-* 0/%s:00:00\n' "$n"
                    fi
                    return 0
                fi ;;
            esac ;;
    esac
    # A whole number of minutes that divides the hour.
    case "$iv" in
        *min) n="${iv%min}"
            case "$n" in ''|*[!0-9]*) ;; *)
                if [ "$n" -gt 0 ] && [ "$n" -lt 60 ] && [ $(( 60 % n )) -eq 0 ]; then
                    printf 'OnUnitActiveSec=\nOnBootSec=\nOnCalendar=*-*-* *:0/%s:00\n' "$n"
                    return 0
                fi ;;
            esac ;;
    esac
    # No exact calendar equivalent. Keep the span, but never leave it as the only trigger:
    # a daily backstop guarantees a next elapse even if the monotonic reference is lost.
    printf 'OnUnitActiveSec=\nOnBootSec=\nOnUnitActiveSec=%s\nOnCalendar=*-*-* 00:00:00\n' "$iv"
}

# ---------------------------------------------------------------------------------------
# IS IT ARMED. systemd reports the next elapse in ONE OF TWO properties and leaves the other
# empty or zero, so reading either alone misreports half the fleet:
#
#   calendar timer   NextElapseUSecRealtime=Sat 2026-09-12 18:00:00 UTC   Monotonic=0
#   monotonic timer  NextElapseUSecRealtime=                              Monotonic=3w 6d 17h
#   DISARMED         NextElapseUSecRealtime=                              Monotonic=infinity
#
# The first version of this file read only the realtime property and reported
# cockpit-ensure.timer — which fires every minute — as NEVER. A checker that calls a healthy
# timer dead is the same defect as one that calls a dead timer healthy, run backwards; both
# come from asking one source a question it cannot answer (law-absence-needs-a-positive-control).
# ---------------------------------------------------------------------------------------
next_realtime() { $SYSTEMCTL show "$1" -p NextElapseUSecRealtime --value 2>/dev/null; }
next_monotonic() { $SYSTEMCTL show "$1" -p NextElapseUSecMonotonic --value 2>/dev/null; }

is_armed() {             # is_armed <unit> -> 0 when systemd holds a next elapse for it
    local rt mono
    rt="$(next_realtime "$1")"
    case "$rt" in ''|n/a) ;; *) return 0 ;; esac
    mono="$(next_monotonic "$1")"
    case "$mono" in ''|n/a|infinity|0) return 1 ;; *) return 0 ;; esac
}

next_elapse() {          # next_elapse <unit> -> a printable next elapse, or empty
    local rt mono
    rt="$(next_realtime "$1")"
    case "$rt" in ''|n/a) ;; *) printf '%s' "$rt"; return 0 ;; esac
    mono="$(next_monotonic "$1")"
    case "$mono" in ''|n/a|infinity|0) printf '' ;; *) printf 'boot+%s' "${mono%%.*}" ;; esac
}

# _dropin_for <unit> <interval> <why> -> the full cadence.conf content, on stdout. Split out
# of cmd_set so the drop-in text (comment header + render_schedule's [Timer] lines) can be
# tested without systemd: its only non-pure input is the wall clock and $SPIRA_OPERATOR.
_dropin_for() {
    local u="$1" iv="$2" why="${3:-}"
    printf '# Written by cadence.sh on %s by %s.\n' \
        "$(TZ="${SPIRA_TZ:-UTC}" date '+%Y-%m-%d %H:%M %Z')" "${SPIRA_OPERATOR:-unknown}"
    printf '#\n# Requested cadence: %s\n' "$iv"
    [ -n "$why" ] && printf '# Why: %s\n' "$why"
    printf '#\n# This is an OVERRIDE, not a new default. The shipped template asks for:\n'
    printf '#   %s\n' "$(template_cadence "$u")"
    printf '# Change the template only if this cadence is meant to be permanent.\n'
    printf '#\n# REVERT: %s/cadence.sh clear %s\n' "$HERE" "$u"
    printf '#\n[Timer]\n'
    render_schedule "$iv"
}

timer_active() { $SYSTEMCTL is-active --quiet "$1" 2>/dev/null; }

# service_running <timer> -> 0 when the unit this timer triggers is active or activating.
# The triggered unit is read from the timer's own Unit= property rather than guessed by
# swapping the extension, because a timer may name a service with an unrelated name.
service_running() {
    local svc st
    svc="$($SYSTEMCTL show "$1" -p Unit --value 2>/dev/null)"
    [ -n "$svc" ] || return 1
    st="$($SYSTEMCTL show "$svc" -p ActiveState --value 2>/dev/null)"
    case "$st" in active|activating|reloading) return 0 ;; *) return 1 ;; esac
}

template_cadence() {     # template_cadence <unit> -> the cadence the shipped template asks for
    local u="$1" base f
    base="${u%.timer}"
    # Try the suffixed form first (spira-*), then the bare name (shared units). A unit whose
    # own name happens to end in the instance word is still found, because the bare name is
    # only reached when the stripped one does not resolve.
    f="$HERE/../systemd/${base%"$(unit_suffix)"}.timer"
    [ -r "$f" ] || f="$HERE/../systemd/${base}.timer"
    [ -r "$f" ] || { printf '?'; return; }
    local out; out="$(grep -E '^(OnCalendar|OnUnitActiveSec|OnBootSec)=' "$f" 2>/dev/null \
        | grep -v '=$' | cut -d= -f2- | paste -sd' ' - || true)"
    printf '%s' "${out:-?}"
}

effective_cadence() {    # effective_cadence <unit> -> what systemd will actually do
    local u="$1" cal mono
    cal="$($SYSTEMCTL show "$u" -p TimersCalendar --value 2>/dev/null \
        | sed -n 's/.*OnCalendar=\([^ ]*\( [^ ]*\)\?\) .*/\1/p' | paste -sd' ' - || true)"
    mono="$($SYSTEMCTL show "$u" -p TimersMonotonic --value 2>/dev/null \
        | tr ' ' '\n' | grep -E '^On(UnitActive|Boot)USec=' | paste -sd' ' - || true)"
    printf '%s' "${cal:+cal:$cal }${mono:+mono:$mono}"
}

# ---------------------------------------------------------------------------------------
cmd_list() {
    local u ov
    printf '%-34s %-8s %-7s %-26s %s\n' UNIT ACTIVE OVERRIDE NEXT TEMPLATE
    for u in $(installed_timers); do
        ov=no; [ -r "$(dropin_path "$u")" ] && ov=YES
        printf '%-34s %-8s %-7s %-26s %s\n' \
            "$u" "$(timer_active "$u" && echo active || echo inactive)" "$ov" \
            "$(next_elapse "$u" | sed 's/^$/NEVER/')" "$(template_cadence "$u")"
    done
    printf '\n%s\n' "OVERRIDE=YES means a cadence.sh drop-in is in force; see: cadence.sh show <unit>" >&2
}

cmd_show() {
    local u; u="$(resolve_unit "${1:?usage: cadence.sh show <unit>}")" || exit 1
    printf 'unit        %s\n' "$u"
    printf 'active      %s\n' "$(timer_active "$u" && echo yes || echo no)"
    printf 'template    %s\n' "$(template_cadence "$u")"
    printf 'effective   %s\n' "$(effective_cadence "$u")"
    printf 'next        %s\n' "$(next_elapse "$u" | sed 's/^$/NEVER — this timer will not fire/')"
    local d; d="$(dropin_path "$u")"
    if [ -r "$d" ]; then
        printf 'override    %s\n\n' "$d"
        sed 's/^/  /' "$d"
    else
        printf 'override    none — running the shipped template\n'
    fi
}

cmd_set() {
    local name="${1:?usage: cadence.sh set <unit> <interval> [--why TEXT]}"
    local iv="${2:?usage: cadence.sh set <unit> <interval> [--why TEXT]}"
    shift 2
    local why=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --why) why="${2:-}"; shift 2 ;;
            *) die "unknown option '$1'" ;;
        esac
    done
    local u; u="$(resolve_unit "$name")" || exit 1
    local d; d="$(dropin_path "$u")"

    # REMEMBER THE PREVIOUS STATE so a failed verify can put it back exactly, including the
    # case where there was no override at all.
    local prev="" had_prev=0
    if [ -r "$d" ]; then prev="$(cat "$d")"; had_prev=1; fi
    local was_armed=0; is_armed "$u" && was_armed=1

    mkdir -p "$(dirname "$d")" 2>/dev/null || die "cannot create $(dirname "$d")"
    _dropin_for "$u" "$iv" "$why" > "$d" || die "could not write $d"

    $SYSTEMCTL daemon-reload 2>/dev/null || die "daemon-reload failed"
    # Only restart a timer that is running. Restarting a stopped timer would start it, which
    # is a decision the operator did not ask this command to make.
    if timer_active "$u"; then
        $SYSTEMCTL restart "$u" 2>/dev/null || true
    fi

    # ---------------------------------------------------------------------------------
    # THE VERIFY. This is the whole point of the program. A timer that WAS armed and is now
    # unarmed has been switched off by this command, and that must not be reported as a
    # cadence change.
    # ---------------------------------------------------------------------------------
    if timer_active "$u" && [ "$was_armed" -eq 1 ] && ! is_armed "$u"; then
        say "REFUSED — '$iv' left $u with no next elapse; it would never fire again."
        say "reverting to the previous configuration."
        if [ "$had_prev" -eq 1 ]; then printf '%s\n' "$prev" > "$d"; else rm -f "$d"; rmdir "$(dirname "$d")" 2>/dev/null || true; fi
        $SYSTEMCTL daemon-reload 2>/dev/null || true
        timer_active "$u" && $SYSTEMCTL restart "$u" 2>/dev/null || true
        is_armed "$u" && say "restored — next elapse $(next_elapse "$u")" \
                      || say "WARNING: $u is still unarmed after the revert — look at it by hand"
        exit 1
    fi

    printf 'cadence: %s -> %s\n' "$u" "$iv" >&2
    printf 'cadence: next elapse %s\n' "$(next_elapse "$u" | sed 's/^$/NEVER (timer is stopped)/')" >&2
    printf 'cadence: revert with  %s/cadence.sh clear %s\n' "$HERE" "$u" >&2
}

cmd_clear() {
    local u; u="$(resolve_unit "${1:?usage: cadence.sh clear <unit>}")" || exit 1
    local d; d="$(dropin_path "$u")"
    [ -r "$d" ] || { say "$u has no cadence.sh override — nothing to clear"; return 0; }
    rm -f "$d" || die "could not remove $d"
    rmdir "$(dirname "$d")" 2>/dev/null || true     # only if we left it empty
    $SYSTEMCTL daemon-reload 2>/dev/null || die "daemon-reload failed"
    timer_active "$u" && $SYSTEMCTL restart "$u" 2>/dev/null || true
    printf 'cadence: %s restored to its template: %s\n' "$u" "$(template_cadence "$u")" >&2
    if timer_active "$u" && ! is_armed "$u"; then
        say "WARNING: $u is active but has no next elapse even on the template — look at it"
        exit 1
    fi
    printf 'cadence: next elapse %s\n' "$(next_elapse "$u" | sed 's/^$/n\/a (timer is stopped)/')" >&2
}

# ---------------------------------------------------------------------------------------
# THE FLEET CHECK. Not about cadence at all: it answers "is any timer on this box silently
# disarmed", which is the class of fault this tool was built from and which nothing else
# reports. An active timer with no next elapse looks healthy in `systemctl status`.
# ---------------------------------------------------------------------------------------
cmd_verify() {
    local u list bad=0 checked=0
    if [ $# -gt 0 ]; then
        list=""; for u in "$@"; do list="$list $(resolve_unit "$u")" || exit 1; done
    else
        list="$(installed_timers)"
    fi
    for u in $list; do
        timer_active "$u" || continue          # a stopped timer is not a fault
        # A MONOTONIC TIMER WHOSE SERVICE IS RUNNING REPORTS infinity, AND IT IS FINE.
        # OnUnitActiveSec is measured from the unit's activation, so while the service is
        # active or activating there is no next elapse to report; it re-arms when the run
        # finishes. Reported as a fault, this fires every time a five-minute job takes longer
        # than the poll — which is precisely when an operator should not be handed a false
        # alarm (law-alerts-must-be-actionable). Caught on this check's first run against
        # spira-archivist-prod.timer, mid-pass.
        if ! is_armed "$u" && service_running "$u"; then continue; fi
        checked=$(( checked + 1 ))
        if ! is_armed "$u"; then
            printf '  %-34s ACTIVE BUT NEVER FIRES (last trigger %s)\n' \
                "$u" "$($SYSTEMCTL show "$u" -p LastTriggerUSec --value 2>/dev/null | sed 's/^$/never/')"
            bad=$(( bad + 1 ))
        fi
    done
    # AN EMPTY SWEEP IS A FAILURE, NOT A CLEAN BILL. Zero active timers means this could not
    # have found anything, and reporting "all armed" there is the reassuring reading of a
    # check pointed at nothing (law-absence-needs-a-positive-control).
    if [ "$checked" -eq 0 ]; then
        say "no ACTIVE timers found under $DROPIN_DIR — refusing to report all-clear on nothing"
        return 1
    fi
    if [ "$bad" -eq 0 ]; then
        printf 'cadence: %s active timer(s), all armed with a next elapse\n' "$checked" >&2
        return 0
    fi
    printf 'cadence: %s of %s active timer(s) will never fire\n' "$bad" "$checked" >&2
    return 2
}

# Guarded so a test can source this file to reach render_schedule, resolve_unit and
# _dropin_for directly, without also running the CLI dispatch against the sourcing shell's
# own positional params.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    case "${1:-list}" in
        list)   cmd_list ;;
        show)   shift; cmd_show "$@" ;;
        set)    shift; cmd_set "$@" ;;
        clear)  shift; cmd_clear "$@" ;;
        verify) shift; cmd_verify "$@" ;;
        *) printf 'usage: cadence.sh [list|show <unit>|set <unit> <interval> [--why TEXT]|clear <unit>|verify [unit...]]\n' >&2; exit 2 ;;
    esac
fi
