#!/usr/bin/env bash
#
# aeons.sh — how many aeons may run at once, and how many are running.
#
#   aeons.sh                  what the ceiling is, what is live, what the real ceiling is
#   aeons.sh set <n>          allow at most <n> aeons at once, across every persona and lane
#   aeons.sh unset            remove the fleet ceiling (back to pool + one per lane)
#   aeons.sh pool <n>         set the TASK pool (SPIRA_MAX_AEONS) — the older, narrower knob
#
# WHY THIS IS A TOOL AND NOT A LINE IN A FILE
# -------------------------------------------
# "Run fewer aeons" is a routine operational wish and it had no lever, so it was answered by
# hand-editing spira.conf — and answered WRONG twice, because the obvious key is not the one
# that governs. SPIRA_MAX_AEONS is the TASK pool; ops, qa and groomer are lanes and draw
# outside it by design, so a host set to a pool of 1 summoned a builder and an ops aeon in
# the same second (2026-09-09 21:46:47) while its own log read `pool: 1 slot(s)`. Anybody
# reaching for "one aeon" has to know that lanes exist, find their caps, and do arithmetic.
# That is a rung-4 problem: a program that refuses to be misread beats a paragraph that
# explains the trap (law-bake-rules-into-tools).
#
# So `status` always prints BOTH numbers and the sum they imply, and `set` writes the key
# that actually binds.
#
# THE CEILING TAKES EFFECT ON THE NEXT PASS, WITH NO RESTART. conf.sh is sourced per pass by
# the sentinel, so the new value is read within ~2 minutes. Nothing needs reloading, and a
# running aeon is never touched — this changes what may be SUMMONED, not what is alive
# (the harness does not halt live work to satisfy a lowered ceiling; see slay.sh for that).
#
# IT DOES NOT EDIT A CONFIG FILE IT DID NOT FIND. The path comes from conf.sh's own
# resolution ($SPIRA_CONF_FILE), never a hardcoded ~/.config path, so this writes to the
# file the harness actually reads — including on a host that keeps it beside the checkout
# or in /etc (law-every-path-comes-from-conf).
set -uo pipefail
. "$(dirname "$0")/conf.sh"

CONF="${SPIRA_CONF_FILE:-}"

die() { printf 'aeons.sh: %s\n' "$*" >&2; exit 1; }

# live_now -> aeons alive this instant. lib.sh owns the counting, including the reason it
# reads systemd units rather than pid files; duplicating that logic here is how the two
# would drift.
live_now() {
    . "$SPIRA_HOME/lib.sh" >/dev/null 2>&1 || { printf '?'; return; }
    aeons_live_total 2>/dev/null || printf '?'
}

# lane_caps -> "<total> <detail>" for every persona that draws outside the task pool.
lane_caps() {
    . "$SPIRA_HOME/lib.sh" >/dev/null 2>&1 || { printf '? ?'; return; }
    local f n total=0 detail=""
    for f in $(spira_lane_fayths 2>/dev/null); do
        n="$(fayth_get "$f" FAYTH_MAX_CONCURRENT 1)"; n="${n:-1}"
        total=$(( total + n ))
        detail="${detail:+$detail, }$f=$n"
    done
    printf '%s %s' "$total" "${detail:-none}"
}

# conf_set <KEY> <value> <comment...> — write a key into the config file in force.
#
# WRITTEN BESIDE AND MOVED, never edited in place: a sentinel pass may be sourcing this file
# at the moment of the write, and a half-written config is a harness that reads garbage for
# one pass (law-replace-running-files-atomically). An existing key is replaced in place so
# the comment above it — which is usually the REASON, and the only record of it — survives;
# a missing key is appended with the comment this tool was given.
conf_set() {
    local key="$1" val="$2"; shift 2
    local comment="$*" tmp
    [ -n "$CONF" ] || die "no config file in force — conf.sh resolved none. Set \$SPIRA_CONF."
    [ -w "$CONF" ] || die "$CONF is not writable"
    tmp="$(mktemp "${CONF}.XXXXXX")" || die "cannot write beside $CONF"
    if grep -qE "^[[:space:]]*${key}[[:space:]]*=" "$CONF"; then
        awk -v k="$key" -v v="$val" '
            $0 ~ "^[[:space:]]*"k"[[:space:]]*=" { printf "%-20s = %s\n", k, v; next }
            { print }
        ' "$CONF" > "$tmp" || die "rewrite failed"
    else
        cat "$CONF" > "$tmp" || die "copy failed"
        {
            printf '\n'
            [ -n "$comment" ] && printf '# %s\n' "$comment"
            printf '%-20s = %s\n' "$key" "$val"
        } >> "$tmp"
    fi
    chmod --reference="$CONF" "$tmp" 2>/dev/null || true
    mv -f "$tmp" "$CONF" || die "install failed"
}

status() {
    local ceiling="${SPIRA_MAX_LIVE_AEONS:-}" pool="${SPIRA_MAX_AEONS:-?}"
    local lanes_cap="${SPIRA_LANES_MAX_LIVE:-}"
    local lt ld live; read -r lt ld <<< "$(lane_caps)"; live="$(live_now)"
    printf '  live now          %s\n' "$live"
    if [ -n "$ceiling" ]; then
        printf '  fleet ceiling     %s   (SPIRA_MAX_LIVE_AEONS — counts every aeon)\n' "$ceiling"
    else
        printf '  fleet ceiling     none  (SPIRA_MAX_LIVE_AEONS unset)\n'
    fi
    printf '  task pool         %s   (SPIRA_MAX_AEONS — builders and other task fayths)\n' "$pool"
    printf '  lanes             %s   (%s — each draws OUTSIDE the pool)\n' "$lt" "$ld"
    if [ -n "$lanes_cap" ]; then
        printf '  lane cap          %s   (SPIRA_LANES_MAX_LIVE — lanes share this many fleet slots)\n' "$lanes_cap"
        local task_min; task_min=$(( ${pool:-0} > ${lanes_cap:-0} ? ${pool:-0} - ${lanes_cap:-0} : 0 ))
        printf '  effective split   builders %s-%s, lanes 0-%s\n' "$task_min" "${pool:-0}" "$lanes_cap"
    fi
    # THE SUM IS THE POINT. Without a fleet ceiling the real limit is pool + lanes, and that
    # number appears nowhere in the config — which is exactly why "set the pool to 1" reads
    # as "run one aeon" and is not.
    if [ -n "$ceiling" ]; then
        printf '  ---\n  at most %s aeon(s) at once. Without the ceiling it would be %s (%s pool + %s lanes).\n' \
            "$ceiling" "$(( ${pool:-0} + ${lt:-0} ))" "$pool" "$lt"
    else
        printf '  ---\n  at most %s aeon(s) at once (%s pool + %s lanes). `aeons.sh set <n>` to cap the total.\n' \
            "$(( ${pool:-0} + ${lt:-0} ))" "$pool" "$lt"
    fi
    [ -n "$CONF" ] && printf '  config            %s\n' "$CONF"
}

CMD="${1:-status}"
case "$CMD" in
status)
    status
    ;;
set)
    N="${2:-}"
    case "$N" in ''|*[!0-9]*) die "usage: aeons.sh set <n>   (a whole number; 0 stops summoning entirely)" ;; esac
    conf_set SPIRA_MAX_LIVE_AEONS "$N" \
        "THE WHOLE-FLEET CEILING — at most N aeons at once, counting lane fayths (ops, qa, groomer) that draw outside SPIRA_MAX_AEONS. Set by aeons.sh on $(TZ=America/Los_Angeles date '+%Y-%m-%d')."
    printf 'fleet ceiling set to %s — in force on the next sentinel pass (<=2 min), no restart needed.\n' "$N"
    [ "$N" = 0 ] && printf 'NOTE: 0 stops every summon. `aeons.sh unset` or `set <n>` to resume; live aeons are untouched.\n'
    # RE-READ IN A CLEAN PROCESS, and this is not fussiness. conf.sh returns early when
    # SPIRA_CONF_LOADED is set and an inherited env value outranks the file, so re-sourcing
    # it in a subshell prints the value this command just REPLACED — a confirmation that
    # confidently shows the old number. Scrub both and re-exec, so what is printed back is
    # what the next sentinel pass will actually read.
    env -u SPIRA_CONF_LOADED -u SPIRA_MAX_LIVE_AEONS "$0" status 2>/dev/null || true
    ;;
unset)
    conf_set SPIRA_MAX_LIVE_AEONS "" ""
    printf 'fleet ceiling removed — the limit is again the task pool plus one per lane.\n'
    ;;
pool)
    N="${2:-}"
    case "$N" in ''|*[!0-9]*) die "usage: aeons.sh pool <n>" ;; esac
    conf_set SPIRA_MAX_AEONS "$N" "How many TASK aeons may run at once. Lanes draw outside this; see aeons.sh."
    printf 'task pool set to %s. NOTE: lanes draw outside it — `aeons.sh set <n>` is the total.\n' "$N"
    ;;
-h|--help|help)
    sed -n '3,8p' "$0" | sed 's/^# \{0,1\}//'
    ;;
*)
    die "unknown command '$CMD' — try: status | set <n> | unset | pool <n>"
    ;;
esac
