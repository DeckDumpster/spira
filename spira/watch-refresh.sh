#!/usr/bin/env bash
#
# watch-refresh.sh — restart a watcher whose code is newer than the process running it.
#
#   watch-refresh.sh            one pass; restart what is stale and say so
#   watch-refresh.sh --dry-run  say what it would restart and change nothing
#
# THE FAILURE THIS EXISTS FOR. A long-lived process pins the configuration it read at
# startup, and nothing about it looks wrong afterwards: a watcher started before a database
# cutover went on addressing the retired databases for days, its state holding not one id
# from the live store, while every process listing showed it healthy and every pass it ran
# was silent. A watcher's silence and its blindness are indistinguishable from outside
# (law-long-lived-processes-pin-their-config), so the repair cannot wait for someone to
# notice — it has to be mechanical, and it has to be on a timer.
#
# This is `cockpit-ensure` for daemons rather than for panes, and the two stay separate
# because a pane and a daemon fail for different reasons.
#
# WHAT COUNTS AS "THE CODE" OF A WATCHER. Not simply the unit's ExecStart, because every
# watcher unit's ExecStart is the DISPATCHER — `watchd.sh exec <name>` — and the program a
# watcher actually runs is the row's target. So a pass compares the unit's start against the
# newest of:
#
#   the unit's own ExecStart program        a stale install pointing at another harness copy
#   the row's target program                the watcher itself
#   the shell and python files BESIDE it    the libraries it sources; the measured failure
#                                           was a rewritten library, not a rewritten target
#   watchd.sh                               the dispatcher that resolves the row
#   the manifest                            what the row says the target IS
#   conf.sh and the config file in force    every path and label the watcher reads
#
# The sibling sweep is deliberately restricted to `*.sh` and `*.py`. A watcher's directory
# also holds state it writes while running, and a check that stat'ed all of it would restart
# the watcher on its own output, every pass, forever. Code files are not written at runtime.
#
# MTIME, NOT A CONTENT HASH. A checkout that rewrites mtime without changing content costs
# one harmless restart, and that is the cheaper mistake. The meter that says when this stops
# being adequate is the per-watcher restart counter `watchd.sh` keeps — rendered as the
# RESTARTS column of `watchd.sh status` — and a number climbing without an edit behind it is
# the evidence that would justify hashing (law-take-the-simple-fix-with-a-meter). Which is
# why the restart below is issued through `cmd_restart` and not through `systemctl`: that
# command is where a restart is counted, so a restart around it is one the meter never sees.
#
# IT MUST BE CHEAP. It runs every minute on a box that may be running production on very few
# cores, and an unbounded loop here is the thing the fencing law is named for. So a steady
# pass costs exactly TWO execs — one `systemctl show` for every unit at once, one `stat` for
# every path at once — and runs no other program at all. It never opens the database: a
# staleness check that queried the store would be the very failure it exists to detect.
# Everything else is done in-process, which is why the manifest is parsed by sourcing
# `watchd.sh` rather than by running it.
#
# A PASS THAT CANNOT SEE FAILS LOUDLY AND RESTARTS NOTHING. A malformed manifest, or a
# `systemctl` that will not answer, exits non-zero with the reason — never a quiet pass,
# which is what a broken checker and a healthy fleet have in common
# (law-absence-needs-a-positive-control).
set -uo pipefail
_wr_here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$_wr_here/conf.sh"
# Sourced, not run: the manifest parser is a function, so reading the manifest costs no fork
# and no exec. watchd.sh runs nothing when it is sourced.
. "$_wr_here/watchd.sh"
unset _wr_here

# The properties one call asks for. `--timestamp=unix` is what makes the answer a bare epoch
# instead of a localised date somebody would have to shell out to `date` to parse, once per
# unit. `Id=` is asked for as well and the answer is parsed by it rather than by position:
# `systemctl show` returns the properties in ITS order, not the order requested, and with
# `--value` there are no keys and no unit names at all — so blocks could only be told apart
# by counting, which is exactly the assumption that breaks the day one unit is unknown.
WR_PROPS="Id,ActiveState,ActiveEnterTimestamp,ExecStart"

wr_stamp() {
    local t
    [ -n "${SPIRA_TZ:-}" ] && export TZ="$SPIRA_TZ"
    printf -v t '%(%Y-%m-%d %H:%M:%S)T' -1
    printf '%s' "$t"
}

# wr_pass [dry] -> 0 having restarted whatever was stale, or 1 having said why it could not
# look. Everything is in one function so a test can call it with the environment it means
# and count what it execs.
wr_pass() {
    local dry="${1:-}"
    local rows name kind target health
    local -a names=()
    local -A files=()          # watcher name -> newline-separated paths that are its code
    local -A want=()           # every path to stat, deduplicated

    # A MALFORMED MANIFEST RESTARTS NOTHING. watchd_rows has already named each fault on
    # stderr; acting on the rows that happened to parse would restart some watchers and
    # silently leave the rest pinned, which is worse than doing nothing loudly.
    rows="$(watchd_rows)" || return 1

    # What every watcher's staleness is measured against, whoever it is: the dispatcher that
    # resolves its row, the manifest that holds the row, and the two files that carry every
    # path and label it will read.
    local -a common=("$SPIRA_HOME/watchd.sh" "$SPIRA_HOME/conf.sh" "$SPIRA_WATCHERS")
    [ -n "${SPIRA_CONF_FILE:-}" ] && common+=("$SPIRA_CONF_FILE")

    local -a argv sibs
    local dir joined
    while IFS='|' read -r name kind target health; do
        [ -n "$name" ] || continue
        # A `log` row names a file something else writes. There is no process of ours to
        # restart, and nothing here may touch whatever is producing it.
        [ "$kind" = daemon ] || continue
        names+=("$name")
        read -r -a argv <<< "$target"
        # Parameter expansion rather than `dirname`, and `printf -v` rather than a command
        # substitution: both would be a fork or an exec per row, in the one function whose
        # contract is that a pass costs two. A target is guaranteed absolute by the manifest
        # parser, so it always has a `/` to cut at; a program directly in the root leaves
        # nothing behind it, which is the root itself.
        dir="${argv[0]%/*}"; [ -n "$dir" ] || dir=/
        sibs=("$dir"/*.sh "$dir"/*.py)
        printf -v joined '%s\n' "${argv[0]}" "${common[@]}" "${sibs[@]}"
        files["$name"]="$joined"
    done <<< "$rows"

    if [ "${#names[@]}" -eq 0 ]; then
        # Not an error: an installation may own no daemon watchers. Said out loud all the
        # same, because "nothing to refresh" and "the manifest is somewhere else" read the
        # same in an empty pass.
        echo "watch-refresh: no daemon watchers in $SPIRA_WATCHERS" >&2
        return 0
    fi

    local -a units=()
    for name in "${names[@]}"; do units+=("$(watch_unit_name "$name")"); done

    # ---- exec 1 of 2 -------------------------------------------------------------------
    local show rc
    show="$(systemctl --user show --timestamp=unix --property="$WR_PROPS" "${units[@]}" 2>&1)"
    rc=$?
    if [ "$rc" != 0 ]; then
        printf 'watch-refresh: systemctl show failed (%s), restarting nothing:\n%s\n' \
            "$rc" "$show" >&2
        return 1
    fi

    # Blocks are separated by a blank line and identified by their own Id=, never by order.
    local -A state=() start=() estart=()
    local id="" line v
    while IFS= read -r line; do
        case "$line" in
            Id=*)          id="${line#Id=}" ;;
            ActiveState=*) [ -n "$id" ] && state["$id"]="${line#ActiveState=}" ;;
            ActiveEnterTimestamp=*)
                v="${line#ActiveEnterTimestamp=}"
                [ -n "$id" ] && start["$id"]="$v" ;;
            ExecStart=*)
                # `ExecStart={ path=/x ; argv[]=/x a b ; ... }`. The first one wins: a unit
                # may carry several, and it is the first that this template renders.
                v="${line#ExecStart=}"
                if [ -n "$id" ] && [ -z "${estart[$id]:-}" ]; then
                    case "$v" in *path=*) v="${v#*path=}"; estart["$id"]="${v%% *}" ;; esac
                fi ;;
        esac
    done <<< "$show"

    # Fold the ExecStart programs in, then reduce to the set of paths that actually exist.
    # The `-e` test is a builtin, so filtering here costs nothing and buys the one thing that
    # matters: `stat` is then expected to succeed, and a `stat` that fails is a fault to
    # report rather than a missing file to shrug at. Without it a broken `stat` would report
    # every watcher as fresh — a checker's silence wearing an all-clear.
    local i n u f
    for i in "${!names[@]}"; do
        name="${names[$i]}"; u="${units[$i]}"
        [ -n "${estart[$u]:-}" ] && files["$name"]="${files[$name]}
${estart[$u]}"
        while IFS= read -r f; do
            [ -n "$f" ] && [ -e "$f" ] && want["$f"]=1
        done <<< "${files[$name]}"
    done

    # ---- exec 2 of 2 -------------------------------------------------------------------
    local -A mt=()
    if [ "${#want[@]}" -gt 0 ]; then
        local statout
        statout="$(stat -c '%Y %n' -- "${!want[@]}" 2>&1)"
        rc=$?
        if [ "$rc" != 0 ]; then
            printf 'watch-refresh: stat failed (%s), restarting nothing:\n%s\n' \
                "$rc" "$statout" >&2
            return 1
        fi
        while read -r n f; do
            [ -n "$f" ] && mt["$f"]="$n"
        done <<< "$statout"
    fi

    local newest culprit s stale=0
    for i in "${!names[@]}"; do
        name="${names[$i]}"; u="${units[$i]}"
        # Only a running watcher pins anything. An inactive or activating unit has no process
        # holding stale configuration, and `Restart=always` is what brings it back — a
        # restart aimed at it here would fight systemd's own backoff.
        [ "${state[$u]:-}" = active ] || continue
        s="${start[$u]:-}"
        case "$s" in
            @[0-9]*) s="${s#@}" ;;
            # Anything else means the timestamp did not come back as an epoch — an older
            # systemd that does not know `--timestamp=unix`, or a unit that never started.
            # Guessing at a format here is how a check silently starts answering about
            # nothing, so it says so and leaves this watcher alone.
            *) printf 'watch-refresh: %s: no usable start time (%s), leaving it alone\n' \
                   "$u" "${s:-empty}" >&2; continue ;;
        esac

        newest=0; culprit=""
        while IFS= read -r f; do
            [ -n "$f" ] || continue
            n="${mt[$f]:-0}"
            [ "$n" -gt "$newest" ] && { newest="$n"; culprit="$f"; }
        done <<< "${files[$name]}"

        # `>=`, not `>`. A unit's start time is whole seconds and so is a file's mtime, so an
        # edit made in the same second the process started would otherwise be invisible
        # forever — the silent direction. The cost of the other direction is one extra
        # restart in a case that terminates immediately, because the restart moves the start
        # time forward.
        [ "$newest" -ge "$s" ] || continue
        stale=$((stale+1))
        if [ -n "$dry" ]; then
            printf '%s would restart %s: %s is newer than the process (%s >= %s)\n' \
                "$(wr_stamp)" "$u" "$culprit" "$newest" "$s"
            continue
        fi
        # NAMED BEFORE IT IS DONE, and the file that caused it is the whole message. A
        # restart counter that climbs with no line saying what moved is a meter nobody can
        # act on.
        printf '%s restarting %s: %s is newer than the process (%s >= %s)\n' \
            "$(wr_stamp)" "$u" "$culprit" "$newest" "$s"
        # THROUGH `cmd_restart`, NEVER PAST IT. That is the one verb that restarts a watcher,
        # and it is where the meter is bumped once systemd has agreed. Issuing the restart
        # straight to `systemctl` from here would still restart the watcher — and silently
        # cost the counter its meaning, which is the number the mtime heuristic is answerable
        # for. It is called by name rather than bare, which would restart every watcher.
        if ! cmd_restart "$name"; then
            printf '%s watch-refresh: restarting %s FAILED\n' "$(wr_stamp)" "$u" >&2
        fi
    done
    return 0
}

# wr_sigterm <pid> — factored out so tests can redefine it without shimming a builtin.
wr_sigterm() { kill -TERM "$1" 2>/dev/null || true; }

# Where to look for process information. Tests override this to a scratch tree.
WR_PROC_ROOT="${WR_PROC_ROOT:-/proc}"

# wr_reap_orphans [dry] — terminate any watch-answers.sh or `watchd.sh tail` process whose
# cgroup is not under spira-watch@.
#
# WHY /proc, NOT pgrep -f. pgrep -f matches the CALLER's command line: a script whose body
# contains the pattern it searches finds itself among the results, and the signal it sends
# ends the sweep. That trap fired live during the 2026-09-08 hand sweep and reported one
# survivor that was the enumerating shell itself. Reading /proc/$pid/cmdline is the kernel's
# own argv; skipping $$ is the only remaining self-match to guard against.
#
# WHY CGROUP, NOT FIRST-ARRIVAL. An flock (sp-21hk) would have given the lock to whichever
# process arrived first — an 8-hour-old orphan takes it and shuts the supervised unit out.
# Cgroup membership selects on who OWNS the process, not who arrived first: a process inside
# spira-watch@ is there because systemd put it there, and this path cannot touch it.
#
# COST. One grep across all /proc/*/cmdline files, then one tr and one cgroup read per
# candidate. Candidates are usually zero or one. Kept separate from wr_pass so the staleness
# check's two-exec invariant stays exact and measurable independently.
wr_reap_orphans() {
    local dry="${1:-}"

    # One scan across every cmdline in proc — the grep exec is the price of this whole pass
    # in the common case of no candidates.
    local -a candidates=()
    while IFS= read -r f; do
        candidates+=("${f%/cmdline}")
    done < <(grep -ral 'watch-answers\.sh\|watchd\.sh' \
                 "$WR_PROC_ROOT"/[0-9]*/cmdline 2>/dev/null)

    [ "${#candidates[@]}" -gt 0 ] || return 0

    local dir pid argv
    for dir in "${candidates[@]}"; do
        pid="${dir##*/}"
        [ "$pid" = "$$" ] && continue   # never signal ourselves

        # Confirm the verb. For watchd.sh, `tail` is a reader that a session opens via
        # Monitor — killing it severs the channel the SessionStart hook just told the session
        # to open (law-bind-the-actor). Only `exec` (the supervised daemon verb) should ever
        # be a reap target; in practice exec processes are inside spira-watch@ anyway, so
        # the cgroup guard below would protect them too — this is belt-and-braces.
        argv="$(tr '\0' ' ' 2>/dev/null < "$dir/cmdline")" || continue
        case "$argv" in
            *watch-answers.sh*) : ;;
            *watchd.sh*)
                case "$argv" in *" tail "*) continue ;; esac ;;
            *) continue ;;
        esac

        # THE GUARD. A process inside any spira-watch unit is supervised; this path must never
        # touch it. Cgroup membership is what systemd writes and nothing else can write it.
        # The pattern is `spira-watch`, not `spira-watch@`: a non-template unit such as
        # `spira-watch-answers-prod.service` is equally supervised and its cgroup carries the
        # unit name without an `@` — the narrower pattern matched template instances only and
        # SIGTERMd the non-template unit on every pass.
        grep -q 'spira-watch' "$dir/cgroup" 2>/dev/null && continue

        if [ -n "$dry" ]; then
            printf '%s would reap orphan pid %s: %s\n' "$(wr_stamp)" "$pid" "$argv"
            continue
        fi
        printf '%s reaping orphan pid %s: %s\n' "$(wr_stamp)" "$pid" "$argv"
        wr_sigterm "$pid"
    done
    return 0
}

# Sourceable. When this file is sourced — by a test, or by anything that wants the pass as a
# function — it defines and does not act.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    case "${1:-run}" in
        run)
            wr_pass; _wr_rc=$?
            wr_reap_orphans
            exit $_wr_rc ;;
        --dry-run|-n)
            wr_pass dry; _wr_rc=$?
            wr_reap_orphans dry
            exit $_wr_rc ;;
        *) echo "usage: watch-refresh.sh [--dry-run]" >&2; exit 2 ;;
    esac
fi
