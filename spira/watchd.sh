#!/usr/bin/env bash
#
# watchd.sh — the watcher manifest, and the face over the logs and cursors systemd fills.
#
#   watchd.sh manifest              every valid row, expanded: name|kind|target|health
#   watchd.sh units                 the unit name of every `daemon` row, one per line
#   watchd.sh keys                  the placeholders a row may name, one per line
#   watchd.sh exec <name>           become that watcher; this is what ExecStart calls
#   watchd.sh status                a table, one line per watcher, then a DEGRADED block
#                                   naming each unwell watcher and why — see below
#   watchd.sh drain [name] [--all]  print what nobody has read, and mark it read
#   watchd.sh peek [name] [--all] [--limit N]
#                                   the same, capped, and marking NOTHING read
#   watchd.sh tail <name> [--all] [--takeover]
#                                   replay from the cursor, then stream; for a Monitor.
#                                   ONE AT A TIME per watcher — a second refuses unless
#                                   --takeover is given, which ends the incumbent first
#   watchd.sh tailers               name|pid|since for every watcher being tailed now
#   watchd.sh restart [name]        restart the unit behind a watcher
#   watchd.sh notify                escalate events nobody has drained, and watchers that
#                                   have been unwell too long; for a timer
#   watchd.sh health-ids <file>     assert a state file names at least one of our own beads
#   watchd.sh health-view <prog> <session>
#                                   assert the view a follower steers matches the one it wants
#
# `status` EMITS TWO SECTIONS AND A PARSER MUST KNOW IT: a header, one row per watcher, then
# — only when a health probe failed — a blank line, the word DEGRADED alone, and one indented
# `<name>: <why>` per afflicted watcher. Field one of a ROW is a watcher name; the lines below
# the blank are not rows and field one of them is not a name. A consumer that reads "everything
# after the header" as the table therefore generates commands naming watchers that do not
# exist, which is what the session hook did until it was made to stop at the blank line.
#
# WHAT A READER LATCHES ONTO is two files per watcher and nothing else: `<name>.log`,
# newline-delimited and append-only, and `<name>.cursor`, an integer counting the lines
# already delivered. Every command below is arithmetic over those two, which is what keeps
# the contract agent-agnostic — `tail -n +$((cursor+1)) -F <log>` is a conforming client, and
# a session that cannot run this script loses nothing but ergonomics.
#
# SYSTEMD OWNS THE PROCESS AND THIS OWNS NOTHING. `status` asks systemd what is running and
# `restart` asks systemd to restart it; nothing here launches a process, detaches one, writes
# a pid file, or decides a watcher is alive by signalling it. There is deliberately no second
# supervision scheme beside systemd's, because a second one is how the defect that prompted
# all of this survived: a watcher started by hand went on reading a database that had been
# retired underneath it for three days, looking perfectly healthy in every process listing,
# while nothing outside that session even knew it was supposed to exist.
#
# WHY A DISPATCHER RATHER THAN A PATH IN THE UNIT. `spira-watch@.service` is ONE template
# taking the watcher's name as its instance, so adding a watcher is a row in the manifest
# plus an install run. Its ExecStart names this script and the instance and nothing else, so
# the manifest stays the only source of truth and no unit encodes a path — a unit with a path
# baked into it runs on exactly one box, and systemd's failure for a wrong one is a bare "No
# such file or directory" in a journal nobody is watching.
#
# WHY A MALFORMED MANIFEST REFUSES THE WHOLE FILE. Every fault is named with its line number
# and then nothing is emitted at all. A parser that skipped the bad row and returned the good
# ones would let `install.sh` enable a partial set and report success — and a watcher that was
# never started looks exactly like a watcher with nothing to say. Refusing is loud; skipping
# is a silence somebody has to notice (law-absence-needs-a-positive-control).
set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"

# The keys a target may name. An allowlist rather than "any variable", because this file is
# read by the process systemd starts as the operator, and because an unrecognised placeholder
# is a typo that must be reported rather than left standing in a path.
#
# WRITTEN OVER SEVERAL LINES AND THEN FLATTENED, because the membership test below is a `case`
# on " $WATCHD_KEYS ": a key followed by a NEWLINE rather than a space does not match, so
# wrapping the list refuses valid keys — and since one bad placeholder refuses the whole
# manifest, adding a key on a second line took every watcher on the box down. The same trap
# was paid for once already in conf.sh's own allowlist; the collapse is what stops it costing
# anything the next time the list outgrows a line.
WATCHD_KEYS="SPIRA_HOME SPIRA_REPO SPIRA_RUN SPIRA_COCKPIT SPIRA_DB SPIRA_WORKSPACES
SPIRA_TOWN SPIRA_WIKI SPIRA_ANSWER_STATE SPIRA_VIEW SPIRA_VIEW_SESSION"
WATCHD_KEYS="$(echo $WATCHD_KEYS)"

# Where the two files a reader latches onto live. Under SPIRA_RUN, which is gitignored: a
# watcher's log carries whatever it was watching.
watchd_dir() { printf '%s/watchd' "$SPIRA_RUN"; }

# _wd_logfile <name> <kind> <target> — where this watcher's events actually are.
#
# A `daemon` row's log is ours: the unit's `StandardOutput=append:` writes it. A `log` row's
# log is the target itself, because something else already writes that file and the row exists
# only to say so. The CURSOR is ours either way — how much of a log a reader has consumed is
# never a fact the log's writer knows.
_wd_logfile() {
    case "$2" in
        # An `off` row names a watcher this installation does not have, so nothing writes a
        # log for it and there is no file to point a reader at.
        off) return 0 ;;
        log) printf '%s' "$3" ;;
        *)   printf '%s/%s.log' "$(watchd_dir)" "$1" ;;
    esac
}
_wd_cursorfile() { printf '%s/%s.cursor' "$(watchd_dir)" "$1"; }

# _wd_tail_lockfile <name> — the file whose exclusive flock IS the right to tail this watcher.
#
# ONE READER PER WATCHER IS THE CONTRACT, NOT A CONVENIENCE. A watcher has exactly one cursor
# and the cursor is what makes a re-latch replay only what was missed; two concurrent tails
# share it, so each one marks lines read on the other's behalf and both deliver every line.
# There is no coherent reading of two readers over one cursor, so the cardinality is enforced
# here rather than left as advice.
#
# THE ADVICE COULD NOT HAVE WORKED. The session hook told a fresh context to "run ListAgents
# first and attach only the streams not already listed there" — but ListAgents enumerates
# agents and sessions, never Monitors, and no tool enumerates a session's own Monitors at all.
# So the check always reported nothing attached, and a session that had been cleared four
# times held four tails on `answers`, delivering Ryan's verdicts in quadruplicate into the one
# place they cost the most. An instruction that cannot be followed is not a control.
#
# flock, SO THERE IS NO SUCH THING AS A STALE LOCK. The kernel drops it when the holder's last
# fd closes, which covers the crash, the kill and the session that simply went away — the
# three cases a pid file gets wrong, and gets wrong by locking everyone out forever.
_wd_tail_lockfile() { printf '%s/%s.tail.lock' "$(watchd_dir)" "$1"; }

# _wd_tail_holder <name> — the pid holding this watcher's tail, or nothing if it is free.
#
# ASKED OF THE KERNEL, NOT OF THE FILE'S CONTENTS. `flock -n` on a copy of the descriptor is
# the only question whose answer cannot be stale; the pid recorded inside the file is a
# courtesy for the message and is never what decides. It is also read through /proc rather
# than trusted, because a pid written by a process that has since died names whatever the
# kernel handed out next (law-pgrep-is-not-identity).
_wd_tail_holder() {
    local lk pid
    lk="$(_wd_tail_lockfile "$1")"
    [ -f "$lk" ] || return 1
    # A lock we can take is a lock nobody holds; taking and dropping it in a subshell answers
    # the question without disturbing anything.
    if ( exec 9>>"$lk" && flock -n 9 ) 2>/dev/null; then
        return 1
    fi
    pid="$(head -n1 "$lk" 2>/dev/null)"
    case "$pid" in ''|*[!0-9]*) pid="" ;; esac
    [ -n "$pid" ] && [ -r "/proc/$pid/cmdline" ] || pid="?"
    printf '%s' "$pid"
    return 0
}

# _wd_total <logfile> — lines in the log, and 0 for a log that is not there yet.
#
# THE ABSENT FILE IS TESTED FOR, not redirected into. `wc -l < missing` fails in the SHELL,
# before `wc` runs, so `2>/dev/null` on the command does not suppress it — and a watcher
# enabled but not yet started is the ordinary case, so `status` printed one shell error per
# such watcher on every pass, into the same stream a session hook reads.
_wd_total() {
    local n
    [ -r "$1" ] || { printf '0'; return 0; }
    n="$(wc -l < "$1" 2>/dev/null)" || n=""
    n="${n#"${n%%[![:space:]]*}"}"
    case "$n" in ''|*[!0-9]*) n=0 ;; esac
    printf '%s' "$n"
}

# _wd_pos <name> <total> — the cursor, forced into [0, total].
#
# THE CLAMP IS THE WHOLE OF THE ARITHMETIC and it is not defensive padding. Three ordinary
# situations put the cursor past the end of the log: an operator rotates or truncates it, a
# watcher is mid-write so the final line carries no newline and `wc -l` is one short of what
# a reader counted, and a hand-edited cursor file. Unclamped, each of those yields a NEGATIVE
# unread count that renders as a negative number and makes `tail -n +N` start before the
# beginning. Clamped, they read as "nothing unread", which under-reports rather than replaying
# a whole log into a fresh context window — the conservative direction for a value whose
# consumer is a session's first screen.
#
# A cursor that is missing, empty, negative or not a number at all reads as 0. Zero is the
# only safe reading: it replays, where a crash or a silent skip would lose events.
_wd_pos() {
    local p
    p="$(cat "$(_wd_cursorfile "$1")" 2>/dev/null)" || p=""
    case "$p" in ''|*[!0-9]*) p=0 ;; esac
    [ "$p" -gt "$2" ] && p="$2"
    printf '%s' "$p"
}

_wd_setpos() {
    mkdir -p "$(watchd_dir)" || return 1
    printf '%s\n' "$2" > "$(_wd_cursorfile "$1")"
}

# THE RESTART METER. `spira.conf` and a watcher's own script are compared by MTIME rather than
# by content, which is the cheap choice and the right one — but mtime moves for edits that
# changed nothing, and a `git checkout` rewrites it wholesale. So the simple mechanism ships
# with the number that will say when it stops being adequate: if this column climbs while
# nothing was actually edited, that is the evidence for a content hash
# (law-take-the-simple-fix-with-a-meter).
#
# IT COUNTS THE RESTARTS THIS HARNESS ISSUED, and says so, because that is the quantity the
# mtime heuristic is answerable for. systemd's own `Restart=always` recoveries are a different
# fact and are not in this number; a watcher dying and being revived shows in UNIT and in
# LAST-EVENT, which is where it belongs.
_wd_restartfile() { printf '%s/%s.restarts' "$(watchd_dir)" "$1"; }
# How long this watcher has been unwell. Keyed on DEGRADED itself and not on the reason: a
# watcher flapping between two faults is unwell throughout, and restarting the clock on each
# new reason is how such a thing escalates never.
_wd_unhealthyfile() { printf '%s/%s.unhealthy' "$(watchd_dir)" "$1"; }

# NEITHER OF THESE MAY RUN A PROGRAM. `_wd_restarts` is called once per watcher by `status`,
# and `_wd_bump` sits on the path a timer takes every minute, whose whole contract is a fixed
# number of execs however large the manifest is (law-fence-loops-on-shared-hardware). Reading
# with `read` and testing the directory before creating it keeps both to builtins — which also
# means neither depends on anything being on PATH, and a caller may hand them a PATH holding
# only the programs it is willing to reach for.
_wd_restarts() {
    local f n=""
    f="$(_wd_restartfile "$1")"
    [ -r "$f" ] && read -r n < "$f"
    case "$n" in ''|*[!0-9]*) n=0 ;; esac
    printf '%s' "$n"
}

_wd_bump() {
    local d; d="$(watchd_dir)"
    [ -d "$d" ] || mkdir -p "$d" || return 0
    printf '%s\n' "$(( $(_wd_restarts "$1") + 1 ))" > "$(_wd_restartfile "$1")"
}

# THE BACKLOG CLOCK. `<name>.pending` holds `<line-number> <epoch>`: the absolute position of
# the OLDEST ACTIONABLE UNREAD line, and when this harness first saw it standing there. It is
# written and read by `notify` alone, and it is a third file rather than a column in the
# cursor because the cursor is the reader's and this is ours — a reader that rewrote its
# cursor would otherwise destroy the timing evidence at the moment it mattered.
#
# WHY THE POSITION AND NOT THE UNREAD COUNT. Lines arriving BEHIND a standing event must not
# restart the clock: the oldest unread event is still the oldest unread event, and a counter
# that moved with the log would let a chatty watcher hold off its own escalation forever. The
# position of the oldest actionable line changes only when a reader has actually taken it.
_wd_pendfile() { printf '%s/%s.pending' "$(watchd_dir)" "$1"; }

# _wd_age <seconds> — a whitespace-free age, coarsest unit that is not zero.
#
# NO SPACE IN THE VALUE, ANYWHERE. `status` renders a whitespace-delimited table and its
# readers — a session hook, a pane, an `awk` one-liner — address columns by index. "4m ago"
# splits into two fields and silently shifts every column after it.
#
# A negative age is clamped to zero rather than rendered. It means the log's mtime is in the
# future, which is a clock that moved, not a watcher that is unusually fresh.
_wd_age() {
    local s="$1"
    [ "$s" -ge 0 ] 2>/dev/null || s=0
    if   [ "$s" -lt 60 ];    then printf '%ds' "$s"
    elif [ "$s" -lt 3600 ];  then printf '%dm' "$(( s / 60 ))"
    elif [ "$s" -lt 86400 ]; then printf '%dh' "$(( s / 3600 ))"
    else                          printf '%dd' "$(( s / 86400 ))"
    fi
}

# _wd_probe <command> — run one health assertion, and set _wd_hstate and _wd_hwhy.
#
# THE VERDICT IS THE EXIT CODE, AND ANYTHING BUT ZERO IS DEGRADED. Not found, killed, timed
# out, crashed — every one of them means the same thing, which is that nothing here PROVED the
# watcher can still see. A probe whose own failure rendered as OK would be a broken check
# reported as an all-clear, which displaces the suspicion that would have prompted a look
# (law-alerts-must-be-actionable). An empty command renders `-`: no assertion was made, which
# is a third fact and not a pass.
#
# A HEALTH COMMAND IS SHELL, AND A TARGET IS NOT. The asymmetry is deliberate: a target is
# what systemd starts, so it must be a program and its arguments and nothing that needs
# interpreting. A health assertion is a question asked once, and the useful ones are
# pipelines — which is also why the field is last, so it may contain `|`.
#
# BOUNDED, BECAUSE `status` IS WHAT A SESSION HOOK RUNS. An operator's probe that hangs would
# hang the opening of a context window, and a hook that never returns is a worse failure than
# any watcher it was reporting on.
#
# The probe's stdout is discarded and its stderr is captured — never passed through. It must
# not be able to write a line into a table whose columns something else is parsing.
_wd_probe() {
    _wd_hstate="-"; _wd_hwhy=""
    [ -n "$1" ] || return 0
    local out rc tmo
    tmo="$(command -v timeout 2>/dev/null)" || tmo=""
    if [ -n "$tmo" ]; then
        out="$("$tmo" "$SPIRA_HEALTH_TIMEOUT" bash -c "$1" 2>&1 >/dev/null)"; rc=$?
    else
        # Said once, on stderr, and then the probe is still run: an unbounded assertion is
        # worse than a bounded one and better than none, but it is not the same thing and a
        # silent substitution would leave the operator believing in a fence that is not there.
        echo "watchd: no 'timeout' on PATH — health commands run unbounded" >&2
        out="$(bash -c "$1" 2>&1 >/dev/null)"; rc=$?
    fi
    [ "$rc" = 0 ] && { _wd_hstate="OK"; return 0; }
    _wd_hstate="DEGRADED"
    out="${out%%$'\n'*}"
    case "$rc" in
        124) _wd_hwhy="${out:-the probe was still running} (timed out after ${SPIRA_HEALTH_TIMEOUT}s)" ;;
        *)   _wd_hwhy="${out:-the probe said nothing} (exit $rc)" ;;
    esac
    [ "${#_wd_hwhy}" -le 200 ] || _wd_hwhy="${_wd_hwhy:0:197}..."
    return 0
}

# _wd_filter — the expression deciding which lines a reader is shown by default.
#
# WHY THERE IS A DEFAULT FILTER AT ALL. Every line surfaced to a session becomes something it
# reacts to, and a watcher's log is mostly progress: state changes that are already visible
# elsewhere and recoverable from the log itself. Surfacing all of it turns a watcher into a
# siren, and an alert nobody can afford to read is the same as no alert
# (law-alerts-must-be-actionable). ACTIONABLE means something is stuck, something broke,
# something needs a human choice, or a thing being waited on finished.
#
# It is CONFIGURATION and not a literal because an operator's watchers speak their own
# vocabulary, and because the same expression has to serve `drain` and `tail` — the two used
# to disagree, with one filtering and the other not, and the session hook advertised the
# unfiltered one.
#
# AN EMPTY EXPRESSION IS REFUSED. As a regular expression it matches every line, so a
# mistyped or blanked key would turn the filter off with nothing at all to see; `--all` is
# how you ask for that, and it says so at the call site.
_wd_filter() {
    if [ -z "${SPIRA_ACTIONABLE:-}" ]; then
        echo "watchd: SPIRA_ACTIONABLE is empty — that would match every line; pass --all to ask for that deliberately" >&2
        return 1
    fi
    printf '%s' "$SPIRA_ACTIONABLE"
}

_wd_trim() { local s="$1"; s="${s#"${s%%[![:space:]]*}"}"; printf '%s' "${s%"${s##*[![:space:]]}"}"; }

# _wd_stream_awk -> the awk invocation that LINE-BUFFERS its stdout on this box, in _WD_AWK.
#
# THE DEFECT THIS EXISTS FOR. `tail -F <log> | awk '{ print; fflush() }'` delivers nothing at
# all under mawk, which is the default `awk` on Debian and Ubuntu. Measured on mawk 1.3.4:
# neither fflush() nor fflush("") flushes stdout when it is a pipe or a file — only the
# -W interactive flag, which selects line buffering, does. The stream therefore stayed silent
# forever: `watchd.sh tail` took its reader lock, reported itself as the holder, and
# delivered not one line, which is the reassuring shape — a watcher that is running and blind.
# Every check above the stream passed, because they all look at the lock rather than the data.
#
# stdbuf -oL does NOT fix it: mawk buffers internally rather than through libc, so the
# LD_PRELOAD stdbuf relies on never sees the writes. Verified.
#
# gawk is preferred where present because fflush() is reliable there and the flag is not
# portable; mawk gets -W interactive, which gawk would reject as an unknown option.
_WD_AWK=()
_wd_stream_awk() {
    [ "${#_WD_AWK[@]}" -gt 0 ] && return 0
    if command -v gawk >/dev/null 2>&1; then
        _WD_AWK=(gawk)
    elif awk -W interactive 'BEGIN { exit 0 }' </dev/null >/dev/null 2>&1; then
        _WD_AWK=(awk -W interactive)
    else
        # Neither: fall back to plain awk and rely on fflush(). If this box's awk is a third
        # implementation that also ignores fflush, the stream is silent and the operator has
        # no way to tell — so say so once, on stderr, rather than let it look healthy.
        _WD_AWK=(awk)
        echo "watchd: this box's awk is neither gawk nor mawk; if the stream stays silent," >&2
        echo "  its output buffering is the first thing to check." >&2
    fi
}

# _wd_expand <string> -> 0 and the expansion in _wd_out, or 1 and the reason in _wd_err.
#
# The iteration cap is not paranoia about a hostile file: a value that itself contains an
# `@KEY@` would otherwise spin forever, and a manifest parse that hangs is indistinguishable
# from an install that is merely slow.
_wd_expand() {
    local s="$1" key val i=0
    # _wd_empty names the key that was empty, and is set for that fault ALONE. An optional row
    # turns on this one distinction and on nothing else: "the operator does not have this" is a
    # different fact from "this row has a typo in it", and a marker that swallowed both would
    # make `?` a way to stop the parser complaining about anything.
    _wd_out=""; _wd_err=""; _wd_empty=""
    while [[ "$s" =~ @([A-Z_]+)@ ]]; do
        i=$((i+1)); [ "$i" -gt 20 ] && { _wd_err="placeholders nest more than 20 deep"; return 1; }
        key="${BASH_REMATCH[1]}"
        case " $WATCHD_KEYS " in
            *" $key "*) ;;
            *) _wd_err="unknown placeholder @$key@ (known: $WATCHD_KEYS)"; return 1 ;;
        esac
        val="${!key-}"
        # AN OPTIONAL KEY NOBODY SET IS A FAULT, NOT AN EMPTY STRING. SPIRA_TOWN and its kind
        # default to empty on purpose, and `@SPIRA_TOWN@/watch.sh` would expand to
        # `/watch.sh` — a path that exists on somebody's box and is nobody's watcher.
        [ -n "$val" ] || { _wd_empty="$key"
            _wd_err="@$key@ is empty — set it in ${SPIRA_CONF_FILE:-spira.conf} or drop the row"; return 1; }
        s="${s//@$key@/$val}"
    done
    _wd_out="$s"; return 0
}

# watchd_rows -> every valid row on stdout as name|kind|target|health, or 1 having named
# every fault on stderr and printed nothing.
watchd_rows() {
    local file="$SPIRA_WATCHERS"
    if [ ! -f "$file" ]; then
        echo "watchd: no watcher manifest at $file" >&2
        return 1
    fi
    local n=0 faults=0 rows="" seen=" " line trimmed nf name kind target health i optional
    local -a f
    while IFS= read -r line || [ -n "$line" ]; do
        n=$((n+1))
        line="${line%$'\r'}"
        trimmed="$(_wd_trim "$line")"
        [ -z "$trimmed" ] && continue
        case "$trimmed" in '#'*) continue ;; esac

        IFS='|' read -r -a f <<< "$trimmed"
        nf="${#f[@]}"
        if [ "$nf" -lt 3 ]; then
            echo "watchd: $file:$n: expected name|kind|target[|health], got $nf field(s): $trimmed" >&2
            faults=1; continue
        fi
        name="$(_wd_trim "${f[0]}")"; kind="$(_wd_trim "${f[1]}")"; target="$(_wd_trim "${f[2]}")"
        # A LEADING `?` MARKS A ROW AS OPTIONAL: it watches something not every installation
        # has, so a key it names being unset drops the row instead of refusing the file. It is
        # what lets a manifest ship a row for a program that is the operator's own — without
        # it, a shipped row naming an optional key takes down every OTHER watcher on a box
        # that has not got it, because a malformed manifest installs none of itself.
        optional=""
        case "$name" in '?'*) optional=1; name="${name#\?}" ;; esac
        # The health command is field four ONWARDS, rejoined: it is a command line and may
        # legitimately contain a pipe, and it is last precisely so that it can.
        health=""
        for ((i=3; i<nf; i++)); do health="$health${health:+|}${f[$i]}"; done
        health="$(_wd_trim "$health")"

        # A NAME IS A SYSTEMD INSTANCE NAME. Restricting it here is what lets every caller
        # write `spira-watch@$name.service` without `systemd-escape` — and an unescaped `/`
        # in an instance name is not an error, it is a DIFFERENT unit.
        if ! [[ "$name" =~ ^[A-Za-z0-9][A-Za-z0-9_-]*$ ]]; then
            echo "watchd: $file:$n: '$name' is not a usable watcher name (letters, digits, _ and - only)" >&2
            faults=1; continue
        fi
        case "$seen" in *" $name "*)
            echo "watchd: $file:$n: '$name' is already defined above — a duplicate would render one unit for two rows" >&2
            faults=1; continue ;;
        esac
        case "$kind" in
            daemon|log) ;;
            *) echo "watchd: $file:$n: '$kind' is not a kind (daemon: we run it; log: something else writes it)" >&2
               faults=1; continue ;;
        esac
        if [ -z "$target" ]; then
            echo "watchd: $file:$n: '$name' has no target" >&2
            faults=1; continue
        fi
        if ! _wd_expand "$target"; then
            # AN OPTIONAL ROW WHOSE KEY IS UNSET IS RENDERED, NOT DROPPED. It comes back as
            # kind `off`, carrying the placeholder that is empty, so `status` prints a line
            # saying this watcher exists and is not installed — which is a third fact, and
            # neither "running" nor "gone". Dropping it silently would leave the manifest
            # claiming to be the source of truth about a watcher it had stopped mentioning,
            # and a stderr note instead would print on every parse until it was tuned out
            # (law-absence-needs-a-positive-control, law-alerts-must-be-actionable).
            if [ -n "$optional" ] && [ -n "$_wd_empty" ]; then
                seen="$seen$name "
                rows="$rows$name|off|@$_wd_empty@|
"
                continue
            fi
            echo "watchd: $file:$n: '$name': $_wd_err" >&2
            faults=1; continue
        fi
        target="$_wd_out"
        # ABSOLUTE, ALWAYS. A unit is started with no working directory worth the name, so a
        # relative target resolves against `/` — which either fails at the worst moment or
        # finds something else entirely.
        case "$target" in /*) ;;
            *) echo "watchd: $file:$n: '$name': target must be an absolute path, got '$target'" >&2
               faults=1; continue ;;
        esac
        if [ -n "$health" ] && ! _wd_expand "$health"; then
            if [ -n "$optional" ] && [ -n "$_wd_empty" ]; then
                seen="$seen$name "
                rows="$rows$name|off|@$_wd_empty@|
"
                continue
            fi
            echo "watchd: $file:$n: '$name' health: $_wd_err" >&2
            faults=1; continue
        fi
        [ -n "$health" ] && health="$_wd_out"

        seen="$seen$name "
        rows="$rows$name|$kind|$target|$health
"
    done < "$file"

    if [ "$faults" != 0 ]; then
        echo "watchd: $file is malformed — refusing to answer for any of it" >&2
        return 1
    fi
    # An empty manifest is a legitimate answer — an installation may own no watchers — but it
    # is said out loud, because "no watchers" and "the manifest is somewhere else" read the
    # same in an empty stdout.
    [ -n "$rows" ] || echo "watchd: $file defines no watchers" >&2
    printf '%s' "$rows"
    return 0
}

cmd_manifest() { watchd_rows; }

# The allowlist, answered rather than read out of the source. A row writes `@KEY@` for any of
# these; anything else is a typo and refuses the file. It is a command because the list is the
# thing a test must be able to enumerate — a suite that scraped it out of the assignment would
# go on passing after the assignment moved, which is the failure mode of every check that reads
# a program instead of asking it.
cmd_keys() { printf '%s\n' $WATCHD_KEYS; }

cmd_units() {
    local rows name kind rest
    rows="$(watchd_rows)" || return 1
    while IFS='|' read -r name kind rest; do
        [ -n "$name" ] || continue
        [ "$kind" = daemon ] || continue
        printf 'spira-watch@%s.service\n' "$name"
    done <<< "$rows"
}

# BECOME THE WATCHER. `exec`, so the process systemd supervises IS the watcher and not a
# shell holding it — a wrapper in between makes `Restart=always` restart the wrapper and
# every signal land on the wrong process.
cmd_exec() {
    local want="${1:-}"
    [ -n "$want" ] || { echo "usage: watchd.sh exec <name>" >&2; return 2; }
    local rows name kind target health found=""
    rows="$(watchd_rows)" || return 1
    while IFS='|' read -r name kind target health; do
        [ "$name" = "$want" ] || continue
        found=1; break
    done <<< "$rows"
    [ -n "$found" ] || { echo "watchd: no watcher named '$want' in $SPIRA_WATCHERS" >&2; return 2; }
    if [ "$kind" = off ]; then
        echo "watchd: '$want' is optional and $target is not set in ${SPIRA_CONF_FILE:-spira.conf} — there is nothing to run" >&2
        return 2
    fi
    if [ "$kind" != daemon ]; then
        echo "watchd: '$want' is a $kind row — $target is written by something else and there is nothing here to run" >&2
        return 2
    fi
    # The log the unit appends to lives here, and systemd opens it BEFORE ExecStart runs, so
    # this mkdir is for a hand-run watcher and for the cursor beside it. install.sh makes the
    # directory for the units.
    mkdir -p "$(watchd_dir)"
    # SPLIT ON WHITESPACE, NOT SHELL. Nothing in a target is expanded or substituted; a row
    # that needs a pipeline points at a script that is one.
    local -a argv; read -r -a argv <<< "$target"
    [ -x "${argv[0]}" ] || echo "watchd: $want: ${argv[0]} is not executable — starting it anyway so the failure is systemd's to report" >&2
    exec "${argv[@]}"
}

# _wd_args <argv...> — the one option parser the reading commands share, so that `--all`
# cannot mean one thing to `drain` and another to `tail`. Sets _wd_name, _wd_all, _wd_peek
# and _wd_limit.
#
# An unrecognised option is refused rather than taken as a watcher name: `--al` would
# otherwise be looked up as a watcher, found missing, and reported as though the manifest
# were at fault.
#
# `--limit` WITHOUT `--peek` IS REFUSED, and that is the whole reason the two are one parser.
# A capped read that also marks what it capped as read destroys the lines it did not print,
# and it does so precisely when there are most of them — which is when losing them matters
# most. Capping is therefore only available to the reader that consumes nothing.
_wd_args() {
    _wd_name=""; _wd_all=""; _wd_peek=""; _wd_limit=0; _wd_takeover=""
    local v
    while [ $# -gt 0 ]; do
        case "$1" in
            --all)  _wd_all=1 ;;
            --peek) _wd_peek=1 ;;
            --takeover) _wd_takeover=1 ;;
            --limit|--limit=*)
                    if [ "$1" = --limit ]; then shift; v="${1-}"; else v="${1#--limit=}"; fi
                    case "$v" in ''|*[!0-9]*)
                        echo "watchd: --limit needs a whole number of lines, got '${v}'" >&2; return 2 ;;
                    esac
                    _wd_limit="$v" ;;
            -*)     echo "watchd: unknown option '$1'" >&2; return 2 ;;
            *)      if [ -n "$_wd_name" ]; then
                        echo "watchd: one watcher at a time, got '$_wd_name' and '$1'" >&2; return 2
                    fi
                    _wd_name="$1" ;;
        esac
        shift
    done
    if [ "$_wd_limit" != 0 ] && [ -z "$_wd_peek" ]; then
        echo "watchd: --limit only applies to 'peek' — a capped read that marks the capped lines read would lose them" >&2
        return 2
    fi
    # `--takeover` ENDS ANOTHER READER, so it is refused anywhere it would not: `drain` and
    # `peek` hold no lock, and silently accepting it there would teach the flag as a general
    # incantation to be sprinkled on any watchd command.
    if [ -n "$_wd_takeover" ] && [ -n "$_wd_peek" ]; then
        echo "watchd: --takeover only applies to 'tail' — nothing else holds a reader lock" >&2
        return 2
    fi
    return 0
}

# _wd_find <rows> <name> — sets _wd_kind and _wd_target for one row, or names what is wrong.
_wd_find() {
    local name kind target health
    while IFS='|' read -r name kind target health; do
        [ "$name" = "$2" ] || continue
        _wd_kind="$kind"; _wd_target="$target"; return 0
    done <<< "$1"
    echo "watchd: no watcher named '$2' in $SPIRA_WATCHERS" >&2
    return 2
}

# cmd_status — the one place blindness is made legible.
#
# WHY THERE IS A HEALTH COLUMN AT ALL. A watcher reading a database that was retired
# underneath it and a watcher with nothing to report are both SILENT, and silence is what a
# process listing, a unit state and an unread count all agree on. One here looked healthy in
# every one of those for three days while seeing nothing, and the answer given in the meantime
# reached nobody. So each row may carry an assertion the watcher must be able to satisfy, and
# it is run HERE and nowhere else — `status` is not on any timer, so a probe costs nothing in
# the steady state and the fence in law-fence-loops-on-shared-hardware is not engaged.
#
# THREE COLUMNS, THREE DIFFERENT WAYS TO BE WRONG, and none of them subsumes another:
#
#   HEALTH      the watcher cannot see what it is supposed to be watching
#   LAST-EVENT  it can see, and has stopped producing — active, healthy and mute
#   RESTARTS    it is being restarted, which is how the mtime staleness check is metered
#
# EVERY VALUE IS WHITESPACE-FREE and every unknown is `-`, never `0` and never blank. This is
# a table addressed by column index, and it is read by things that will act on it.
cmd_status() {
    local rows; rows="$(watchd_rows)" || return 1
    local name kind target health
    local -a wname=() wkind=() wtarget=() whealth=() wlog=() units=()
    while IFS='|' read -r name kind target health; do
        [ -n "$name" ] || continue
        wname+=("$name"); wkind+=("$kind"); wtarget+=("$target"); whealth+=("$health")
        wlog+=("$(_wd_logfile "$name" "$kind" "$target")")
        [ "$kind" = daemon ] && units+=("$(watch_unit_name "$name")")
    done <<< "$rows"

    # ONE EXEC FOR EVERY UNIT. `systemctl is-active` takes any number of units and answers one
    # line each, in the order given, so the cost of `status` does not grow with the manifest.
    # A call per watcher on a box that may be running production on the same cores is exactly
    # what law-fence-loops-on-shared-hardware is about, and `status` is the command a session
    # hook runs at every start.
    local -a states=()
    if [ "${#units[@]}" -gt 0 ]; then
        mapfile -t states < <(systemctl --user is-active "${units[@]}" 2>/dev/null)
    fi

    # AND ONE EXEC FOR EVERY MODIFICATION TIME, for the same reason. `stat` takes any number of
    # files, and `%n` echoes each path back, so the answers are matched by NAME rather than by
    # position — a file that vanished between the loop above and this call would otherwise
    # shift every age after it onto the wrong watcher.
    local f ts path
    local -a present=()
    for f in "${wlog[@]}"; do [ -r "$f" ] && present+=("$f"); done
    local -A mtime=()
    if [ "${#present[@]}" -gt 0 ]; then
        while read -r ts path; do
            [ -n "$path" ] && mtime["$path"]="$ts"
        done < <(stat -c '%Y %n' -- "${present[@]}" 2>/dev/null)
    fi
    local now; printf -v now '%(%s)T' -1

    printf '%-14s %-10s %-8s %7s %10s %8s  %s\n' NAME UNIT HEALTH UNREAD LAST-EVENT RESTARTS LOG
    local i u=0 lf total pos state age restarts
    local -a degraded=() uninstalled=()
    for ((i=0; i<${#wname[@]}; i++)); do
        # A WATCHER THIS INSTALLATION HAS NOT GOT IS SAID, NOT OMITTED. Every column is `-`,
        # because none of them has an answer about a process that was never meant to start —
        # and `off` is a third state, neither a watcher that is running nor a row that quietly
        # went missing from the manifest. The key that would turn it on is named in the block
        # below rather than in the table, so the last column stays a path.
        if [ "${wkind[$i]}" = off ]; then
            printf '%-14s %-10s %-8s %7s %10s %8s  %s\n' "${wname[$i]}" off - - - - -
            uninstalled+=("${wname[$i]}: ${wtarget[$i]} is not set in ${SPIRA_CONF_FILE:-spira.conf}")
            continue
        fi
        lf="${wlog[$i]}"
        total="$(_wd_total "$lf")"
        pos="$(_wd_pos "${wname[$i]}" "$total")"
        if [ "${wkind[$i]}" = daemon ]; then
            # AN ANSWER WE DID NOT GET RENDERS `?`, NEVER `inactive`. systemd may not be
            # running at all — a container, a box without a user manager — and a failed probe
            # displayed as a state is a broken check reported as a finding about the watcher
            # (law-absence-needs-a-positive-control).
            state="${states[$u]:-?}"; [ -n "$state" ] || state="?"
            u=$((u+1))
            restarts="$(_wd_restarts "${wname[$i]}")"
        else
            # Nothing here owns it, so there is no unit to ask about and none to restart. The
            # unread count and the health assertion are still ours, and a `log` row can be
            # blind in exactly the way a `daemon` row can.
            state="external"
            restarts="-"
        fi

        # A LOG THAT HAS NEVER BEEN WRITTEN HAS NO AGE, and `-` is what that is. Rendering it
        # as `0s` would make a watcher that has never once emitted look like the freshest one
        # in the table.
        if [ -n "${mtime["$lf"]-}" ]; then age="$(_wd_age "$(( now - ${mtime["$lf"]} ))")"; else age="-"; fi

        # THE PROBE IS SKIPPED WHEN THE UNIT IS DOWN: a dead watcher's stale files would
        # return false OK (law-absence-needs-a-positive-control). HALTED when the world was
        # deliberately stopped, DEGRADED otherwise — the two must be distinguishable.
        if [ "${wkind[$i]}" = daemon ] && [ "$state" != "active" ]; then
            if [ -f "$SPIRA_RUN/world.halted" ]; then
                _wd_hstate="HALTED"; _wd_hwhy=""
            else
                _wd_hstate="DEGRADED"; _wd_hwhy="unit is $state — no writer"
            fi
        else
            _wd_probe "${whealth[$i]}"
        fi
        [ "$_wd_hstate" = DEGRADED ] && degraded+=("${wname[$i]}: $_wd_hwhy")

        printf '%-14s %-10s %-8s %7s %10s %8s  %s\n' \
            "${wname[$i]}" "$state" "$_wd_hstate" "$(( total - pos ))" "$age" "$restarts" "$lf"
    done

    # WHY A REASON AND NOT JUST THE WORD. `DEGRADED` on its own says a check failed and not
    # which, so the next step is to go and re-run the probe by hand — which is the work this
    # command exists to have already done. It goes to STDOUT because it is part of the report a
    # session hook prints, and stderr belongs to faults in `status` itself.
    if [ "${#degraded[@]}" -gt 0 ]; then
        printf '\nDEGRADED\n'
        for i in "${!degraded[@]}"; do printf '  %s\n' "${degraded[$i]}"; done
    fi
    # AND THE ROWS THAT ARE DELIBERATELY NOT RUNNING, with the key that would start each. This
    # is not a fault and is kept apart from the faults for that reason — but it is printed, so
    # an operator who meant to configure one and did not can see that from here rather than
    # from the absence of events they were expecting.
    if [ "${#uninstalled[@]}" -gt 0 ]; then
        printf '\nNOT INSTALLED\n'
        for i in "${!uninstalled[@]}"; do printf '  %s\n' "${uninstalled[$i]}"; done
    fi
    return 0
}

# cmd_drain [name] [--all] — hand over what nobody has read, and record that it was handed over.
# cmd_drain --peek [--limit N] — the same reading, capped, recording nothing.
#
# ONE FUNCTION FOR BOTH, because they are one piece of arithmetic and two policies. Two
# implementations of "which lines has nobody read" is how `drain` and `tail` came to disagree
# about the filter, with the session hook advertising the wrong one.
#
# THE DEFAULT IS FILTERED, AND THAT IS THE POINT OF THIS COMMAND. `drain` is what a session
# hook advertises to a context window that has just opened, so an unfiltered drain puts the
# entire backlog into the most expensive place it could go: one real session was offered a
# replay of 283 raw lines as the first thing in it. `--all` is the escape hatch, and it has to
# be asked for.
#
# THE HEADER CARRIES BOTH NUMBERS — actionable and new — because the suppressed lines are the
# cost of the filter, and a filter whose cost is invisible is one nobody can tell has gone
# wrong. It prints even when nothing survived the filter, so "47 events, none of them needed
# you" and "no events" stay distinguishable.
cmd_drain() {
    _wd_args "$@" || return 2
    local re=""
    [ -n "$_wd_all" ] || { re="$(_wd_filter)" || return 2; }
    local rows; rows="$(watchd_rows)" || return 1

    local name kind target health lf total pos new chunk shown k found=""
    while IFS='|' read -r name kind target health; do
        [ -n "$name" ] || continue
        if [ -n "$_wd_name" ]; then [ "$_wd_name" = "$name" ] || continue; fi
        found=1
        # Nothing writes a log for a watcher that is not installed, so there is nothing to
        # hand over. Said out loud only when this row is the one that was asked for: in bulk
        # it is a line on every pass, which is the noise that makes a real one unreadable.
        if [ "$kind" = off ]; then
            [ -n "$_wd_name" ] && echo "watchd: '$name' is optional and $target is not set in ${SPIRA_CONF_FILE:-spira.conf} — it has never run" >&2
            continue
        fi
        lf="$(_wd_logfile "$name" "$kind" "$target")"
        total="$(_wd_total "$lf")"
        pos="$(_wd_pos "$name" "$total")"
        new=$(( total - pos ))
        [ "$new" -gt 0 ] || continue

        # AN EXACT RANGE, NOT A TAIL PIPED INTO A HEAD. The log is being appended to while
        # this runs, so `tail -n +N` would emit lines written after the count was taken —
        # printed here, but still ahead of the cursor set below, and therefore printed again
        # by the next drain. Naming both ends binds what is shown to what is marked read.
        chunk="$(sed -n "$(( pos + 1 )),${total}p" "$lf" 2>/dev/null)"
        if [ -n "$_wd_all" ]; then
            shown="$chunk"
            k=0; [ -n "$shown" ] && k="$(printf '%s\n' "$shown" | wc -l)"
            printf '=== %s (%d new) ===\n' "$name" "$new"
        else
            shown="$(printf '%s\n' "$chunk" | grep -E -- "$re")"
            k=0; [ -n "$shown" ] && k="$(printf '%s\n' "$shown" | wc -l)"
            printf '=== %s (%d actionable of %d new) ===\n' "$name" "$k" "$new"
        fi

        # THE CAP KEEPS THE MOST RECENT LINES, and says how many it dropped. A reader whose
        # budget is a session's first screen wants the newest state, not the oldest; and a
        # truncation that is silent is a filter whose cost is invisible, which is the defect
        # the header's two numbers exist to avoid.
        if [ "$_wd_limit" != 0 ] && [ "$k" -gt "$_wd_limit" ]; then
            printf '%s\n' "$shown" | tail -n "$_wd_limit"
            printf '    ... %d earlier actionable line(s) withheld; `watchd.sh tail %s` has all of them\n' \
                   "$(( k - _wd_limit ))" "$name"
        elif [ -n "$shown" ]; then
            printf '%s\n' "$shown"
        fi

        # THE CURSOR ADVANCES BY WHAT WAS READ, NEVER BY WHAT WAS PRINTED. A filtered line has
        # been considered and rejected, not missed; leaving it unread would make every
        # subsequent drain re-examine it and would keep the hook reporting a backlog that no
        # amount of draining could clear.
        #
        # AND `peek` ADVANCES IT NOT AT ALL. That is the entire difference between the two
        # verbs: `drain` is delivery and `peek` is a look. The session hook peeks, because it
        # prints a SUMMARY under a line budget — if it consumed what it summarised, every line
        # it had no room for would be marked delivered and the latch that follows would replay
        # nothing.
        [ -n "$_wd_peek" ] || _wd_setpos "$name" "$total"
    done <<< "$rows"

    if [ -n "$_wd_name" ] && [ -z "$found" ]; then
        echo "watchd: no watcher named '$_wd_name' in $SPIRA_WATCHERS" >&2
        return 2
    fi
    return 0
}

# cmd_tail <name> [--all] — replay from the cursor, then stream. This is the Monitor command.
#
# It never exits on its own, and it advances the cursor AS IT READS rather than at the end.
# Both properties are what make re-latching after a context reset correct: attaching replays
# exactly what was written while nobody was attached, and re-attaching after that replays
# nothing, because the position was written down line by line. Advancing only on exit would
# make every re-attach re-fire the whole history — which is the noise the filter exists to
# remove, arriving by a different route.
cmd_tail() {
    _wd_args "$@" || return 2
    [ -n "$_wd_name" ] || { echo "usage: watchd.sh tail <name> [--all]" >&2; return 2; }
    local re=""
    [ -n "$_wd_all" ] || { re="$(_wd_filter)" || return 2; }
    local rows; rows="$(watchd_rows)" || return 1
    _wd_find "$rows" "$_wd_name" || return 2
    # REFUSED RATHER THAN WAITED ON. `tail -F` retries by name and would sit forever on a log
    # nothing is ever going to write, which from a Monitor is indistinguishable from a watcher
    # that is running and quiet.
    if [ "$_wd_kind" = off ]; then
        echo "watchd: '$_wd_name' is optional and $_wd_target is not set in ${SPIRA_CONF_FILE:-spira.conf} — there is nothing to tail" >&2
        return 2
    fi

    local lf cf pos
    lf="$(_wd_logfile "$_wd_name" "$_wd_kind" "$_wd_target")"
    cf="$(_wd_cursorfile "$_wd_name")"
    mkdir -p "$(watchd_dir)" || return 1

    # THE READER LOCK, TAKEN BEFORE THE CURSOR IS READ. Two tails that both read the position
    # and then both stream is the duplicate-delivery bug itself; the lock has to close that
    # window, so it is held before `_wd_pos` rather than merely before `tail`.
    #
    # A FENCE, NOT A WALL. It refuses an accidental second reader — the fresh context obeying
    # a latch instruction it has no way to check — and names its own override in the refusal.
    # `--takeover` is the deliberate form: this session wants the stream and the incumbent may
    # go. Nothing here decides that on its own, because the incumbent may be the session the
    # operator is actually reading.
    local lkf lkpid
    lkf="$(_wd_tail_lockfile "$_wd_name")"
    exec {_wd_lkfd}>>"$lkf" || { echo "watchd: cannot open $lkf" >&2; return 1; }
    if ! flock -n "$_wd_lkfd"; then
        lkpid="$(_wd_tail_holder "$_wd_name")" || lkpid="?"
        if [ -z "$_wd_takeover" ]; then
            echo "watchd: '$_wd_name' is already being tailed by pid $lkpid — not attaching a second reader." >&2
            echo "  Its events are still being delivered to whoever holds it; a second tail would deliver" >&2
            echo "  every line twice and corrupt the shared cursor. If this session should have the stream" >&2
            echo "  instead, re-run with --takeover." >&2
            return 3
        fi
        # THE INCUMBENT IS ADDRESSED BY PID FROM ITS OWN LOCK, never by a pattern over command
        # lines: `pkill -f 'watchd.sh tail'` matches the shell that ran it, including this one
        # (law-pgrep-is-not-identity). Then the lock is WAITED for rather than re-polled, so a
        # takeover cannot race a slow exit into a third reader.
        case "$lkpid" in
            ''|'?'|*[!0-9]*) : ;;
            *) kill -TERM "$lkpid" 2>/dev/null || : ;;
        esac
        # WAITED FOR, because the incumbent's `tail` and `awk` hold the same descriptor and
        # therefore the same lock. The wrapper dying is not the incumbent letting go; the last
        # of its processes closing the fd is. Ten seconds is far longer than that teardown
        # takes and short enough to fail loudly rather than hang a Monitor forever.
        if ! flock -w 10 "$_wd_lkfd"; then
            echo "watchd: '$_wd_name' is still held by pid $lkpid ten seconds after TERM — refusing rather than running beside it" >&2
            return 3
        fi
    fi
    # Recorded for the message another reader will print, and for `tailers`. The kernel, not
    # this line, is what holds the claim.
    printf '%s\n' "$$" >"$lkf" 2>/dev/null || :

    pos="$(_wd_pos "$_wd_name" "$(_wd_total "$lf")")"
    # A LOG THAT IS NOT THERE YET IS SAID OUT LOUD, then waited for. `-F` retries by name, so
    # attaching before the unit has started is legitimate and works; what is not acceptable is
    # doing it silently, because a watcher whose log never appears and a watcher with nothing
    # to say would then look identical from here.
    [ -f "$lf" ] || echo "watchd: $lf does not exist yet — waiting for it" >&2

    # awk holds the position, because only awk knows how far the stream has got. It writes the
    # ABSOLUTE line number after every line and closes the file each time, so a reader killed
    # mid-stream loses at most the line it was on. `print` is flushed for the same reason a
    # Monitor exists at all: a line buffered is a line not delivered.
    # THE STREAM RUNS IN THE BACKGROUND AND THE SCRIPT WAITS ON IT, which looks like a
    # detour and is the only arrangement that can be stopped. bash runs a trap only between
    # commands, so a script sitting in a FOREGROUND `tail -F | awk` never handles the SIGTERM
    # at all — the signal is noted and the handler waits for a command that by construction
    # never returns. `wait` is interruptible, so backgrounding the pipeline is what makes the
    # trap below reachable.
    #
    # AND THE TRAP IS NOT HYGIENE. `tail -F` and `awk` are separate processes that survive
    # their parent, so killing this script leaves them streaming into a stdout nobody is
    # reading, still holding the reader lock through the inherited descriptor. That is a
    # takeover that reports success and changes nothing: the incumbent's wrapper dies, its
    # pipeline goes on delivering, and the challenger has to be refused a second time by a
    # lock whose recorded pid now names a process that no longer exists.
    local pidf; pidf="$(watchd_dir)/$_wd_name.tail.pid"
    : > "$pidf" 2>/dev/null || pidf=""
    # shellcheck disable=SC2064
    trap "_wd_tail_stop '$pidf'" TERM INT HUP EXIT

    _wd_stream_awk
    if [ -n "$_wd_all" ]; then
        { tail -n +$(( pos + 1 )) -F "$lf" & [ -n "$pidf" ] && echo $! > "$pidf"; wait; } \
            | "${_WD_AWK[@]}" -v c="$cf" -v p="$pos" '{ n=p+NR; print; fflush(); print n > c; close(c) }' &
    else
        { tail -n +$(( pos + 1 )) -F "$lf" & [ -n "$pidf" ] && echo $! > "$pidf"; wait; } \
            | "${_WD_AWK[@]}" -v c="$cf" -v p="$pos" -v re="$re" \
                '{ n=p+NR; if ($0 ~ re) { print; fflush() } print n > c; close(c) }' &
    fi
    _wd_tail_pipe=$!
    wait "$_wd_tail_pipe"
}

# _wd_tail_stop <pidfile> — end the stream this process started, children first.
#
# THE `tail` IS KILLED AND THE `awk` IS NOT. Closing the pipe is what makes awk exit, and it
# exits having finished the line it was on — so the cursor it is mid-write on is completed
# rather than truncated. Signalling awk directly would save nothing and could lose the
# position of the last line delivered, which is the one a re-latch must not replay.
#
# ADDRESSED BY THE PID IT WROTE DOWN ITSELF, never by a pattern: `pkill -f 'tail -F'` on this
# box matches every other watcher's stream and the shell that invoked it
# (law-pgrep-is-not-identity).
_wd_tail_stop() {
    local pidf="${1:-}" tp
    trap - TERM INT HUP EXIT
    if [ -n "$pidf" ] && [ -r "$pidf" ]; then
        tp="$(head -n1 "$pidf" 2>/dev/null)"
        case "$tp" in ''|*[!0-9]*) tp="" ;; esac
        [ -n "$tp" ] && kill -TERM "$tp" 2>/dev/null
        rm -f "$pidf" 2>/dev/null
    fi
    [ -n "${_wd_tail_pipe:-}" ] && wait "$_wd_tail_pipe" 2>/dev/null
    return 0
}

# cmd_tailers — `name|pid|since` for every watcher a live `tail` currently holds.
#
# THIS IS THE ANSWER THE SESSION HOOK NEEDED AND COULD NOT GET. A fresh context cannot
# enumerate its own Monitors, so it cannot tell a stream it is already receiving from one it
# is not, and the hook that told it to check with ListAgents was naming a tool that lists
# neither. The lock is the fact, so the lock is what is reported.
#
# IT IS DELIBERATELY NOT PER-SESSION. What matters to a reader is whether the events are being
# delivered SOMEWHERE, because the cardinality is one and a second reader is wrong regardless
# of who owns the first. A pid is printed so that a holder which is not this session can be
# identified — and taken over, on purpose — rather than guessed at.
cmd_tailers() {
    local rows; rows="$(watchd_rows)" || return 1
    local name kind target health pid since
    while IFS='|' read -r name kind target health; do
        [ -n "$name" ] || continue
        [ "$kind" = off ] && continue
        pid="$(_wd_tail_holder "$name")" || continue
        since=""
        [ "$pid" != "?" ] && since="$(stat -c %y "/proc/$pid" 2>/dev/null | cut -d. -f1)"
        printf '%s|%s|%s\n' "$name" "$pid" "$since"
    done <<< "$rows"
    return 0
}

# cmd_restart [name] — hand the restart to systemd, which is the only thing that owns one.
#
# All of them by default and in ONE call, for the reason `status` batches: the manifest decides
# how many units there are, and a command whose cost grows with it gets slower on exactly the
# installation that needs it most.
cmd_restart() {
    local only="${1:-}"
    local rows; rows="$(watchd_rows)" || return 1
    local name kind target health found=""
    local -a units=() names=()
    while IFS='|' read -r name kind target health; do
        [ -n "$name" ] || continue
        if [ -n "$only" ]; then [ "$only" = "$name" ] || continue; fi
        found=1
        if [ "$kind" = off ]; then
            if [ -n "$only" ]; then
                echo "watchd: '$name' is optional and $target is not set in ${SPIRA_CONF_FILE:-spira.conf} — there is no unit to restart" >&2
                return 2
            fi
            continue
        fi
        if [ "$kind" != daemon ]; then
            # There is no unit, so there is nothing to restart — and restarting whatever writes
            # that log is not this harness's business.
            #
            # NAMED ON ITS OWN THIS IS A REFUSAL; IN BULK IT IS A SILENT SKIP. The caller who
            # named it asked for something that cannot happen, and a zero exit would tell them
            # it did. The caller who asked for all of them asked about the units, and a line
            # of explanation on every pass is the noise that makes a real one unreadable.
            if [ -n "$only" ]; then
                echo "watchd: $name is a $kind row — $target is written by something else, so there is no unit to restart" >&2
                return 2
            fi
            continue
        fi
        units+=("$(watch_unit_name "$name")"); names+=("$name")
    done <<< "$rows"

    if [ -n "$only" ] && [ -z "$found" ]; then
        echo "watchd: no watcher named '$only' in $SPIRA_WATCHERS" >&2
        return 2
    fi
    [ "${#units[@]}" -gt 0 ] || return 0
    systemctl --user restart "${units[@]}" || {
        echo "watchd: systemd refused the restart — ask it why with 'systemctl --user status ${units[0]}'" >&2
        return 1
    }
    # THE METER IS BUMPED HERE BECAUSE THIS IS THE ONE VERB, and it is bumped only once systemd
    # has agreed — a restart that was refused did not happen, and counting it would put the
    # blame for a broken unit on the staleness heuristic this number exists to judge. Anything
    # that restarts a watcher goes through this command for exactly that reason: reaching past
    # it to `systemctl` still restarts the watcher, and silently costs the meter its meaning.
    local n
    for n in "${names[@]}"; do _wd_bump "$n"; done
    printf 'restarted: %s\n' "${units[*]}"
}

# cmd_health_ids <file> — the assertion that catches a watcher reading the wrong database.
#
# It is a health command like any other: a manifest row names it, `status` runs it, a non-zero
# exit renders DEGRADED. It is here rather than in a script of its own because every watcher
# that keeps a state file wants the same question asked of it.
#
# THE TEST IS THE ABSENCE OF OUR OWN IDS, NOT THE PRESENCE OF FOREIGN ONES, and getting this
# backwards is the whole trap. A database here legitimately holds beads imported under other
# prefixes — measured once at 145 of 1825 rows carrying the local one — so an assertion reading
# "this state names a prefix that is not ours" is TRUE of a perfectly healthy watcher, and was
# therefore true of the blind one too. It would have passed. What no healthy watcher can do is
# go a whole state file without naming a single local bead.
#
# A MISSING FILE IS DEGRADED, NOT AN ERROR TO BE SWALLOWED. A watcher that has never written
# its state has never completed a pass, which is the same blindness arriving earlier; the
# reason names the path, because the ordinary cause is that the file moved and the assertion
# is now pointed at nothing (law-absence-needs-a-positive-control).
cmd_health_ids() {
    local f="${1:-}"
    [ -n "$f" ] || { echo "usage: watchd.sh health-ids <file>" >&2; return 2; }
    # DERIVE THE PREFIX FROM THE DATABASE, not from the goal bead. The witness is a list of
    # ids that came out of bd; the database is what issues them and is the only source that
    # cannot drift from the ids actually in the file. SPIRA_ID_PREFIX is the fallback for
    # when the database is not reachable — explicit config wins over derivation.
    local p=""
    if [ -n "${SPIRA_BD:-}" ] && [ -n "${SPIRA_DB:-}" ]; then
        p="$("$SPIRA_BD" -C "$SPIRA_DB" config get issue_prefix 2>/dev/null)" || p=""
    fi
    [ -n "$p" ] || p="${SPIRA_ID_PREFIX:-}"
    # REFUSED RATHER THAN GUESSED. An unusable prefix cannot be turned into a verdict either
    # way, and a health check that answers OK when it could not run is the failure this
    # command was written to end. Exit 2 so it is distinguishable from a real DEGRADED.
    case "$p" in
        ''|*[!A-Za-z0-9]*)
            echo "watchd: database prefix is '$p' — set SPIRA_BD and SPIRA_DB, or SPIRA_ID_PREFIX in ${SPIRA_CONF_FILE:-spira.conf}" >&2
            return 2 ;;
    esac
    if [ ! -f "$f" ]; then
        echo "$f does not exist — this watcher has never written its state" >&2
        return 1
    fi
    # A BOUNDARY BEFORE THE PREFIX, so `sp-` is not found inside `wsp-1`. No pipe into `grep`,
    # deliberately: under `pipefail` a `grep` that stops at its first match closes the pipe, the
    # writer dies of SIGPIPE, and the check fails exactly when it succeeds
    # (law-no-grep-q-under-pipefail).
    local hit
    hit="$(grep -Eom1 "(^|[^A-Za-z0-9_-])$p-[A-Za-z0-9]" "$f" 2>/dev/null)" || hit=""
    if [ -z "$hit" ]; then
        echo "$f names no $p- id at all — it is tracking some other database" >&2
        return 1
    fi
    return 0
}

# cmd_health_view <program> <session> — the assertion that catches a correct state machine
# with nothing enacting it.
#
# THE WATCHER THIS JUDGES DECIDES WHICH WINDOW THE OPERATOR IS LOOKING AT, which makes it the
# highest-consequence one here and the one whose failure is hardest to see: it was found
# stopped with its state exactly right — a review was active, it knew the review window was
# wanted — and the operator went on reading the other one. Nothing in a unit state, an unread
# count or a process listing says that, because the follower flips ON TRANSITIONS ONLY. A
# process that is not running has no transitions to miss, so it is silent in precisely the way
# a healthy idle one is.
#
# SO THE ASSERTION COMPARES INTENT AGAINST THE WORLD, not one report against itself. The
# program answers what SHOULD be visible (`want: <session>`); the multiplexer is asked what IS
# visible; DEGRADED is the two disagreeing. That is the whole discriminating fact, and it is
# available to anything that can run two commands.
#
# WHY WINDOW IDS AND NOT THE NAME THE PROGRAM PRINTS. A window is named for the program running
# in it, so the visible window's NAME is the same string whether the right window is showing or
# the wrong one — the observed failure and the healthy state rendered identically. Ids do not
# collide, and the follower's own mechanism is a window linked into two sessions, which is one
# window object and therefore one id.
#
# EVERY WAY THIS CAN FAIL TO ANSWER IS DEGRADED OR REFUSED, NEVER OK. A missing multiplexer, a
# session that is not there, a program that prints no `want:` line — none of them PROVED the
# view is being steered, and a probe that answered OK when it could not run is the broken check
# reported as an all-clear that this column exists to end (law-absence-needs-a-positive-control).
cmd_health_view() {
    local prog="${1:-}" sess="${2:-}"
    if [ -z "$prog" ] || [ -z "$sess" ]; then
        echo "usage: watchd.sh health-view <program> <session>" >&2; return 2
    fi
    # Exit 2, not 1: this is the check being unable to run, which is a different fact from the
    # view being wrong, and only one of the two is about the watcher.
    [ -x "$prog" ] || { echo "$prog is not executable — nothing here can say what should be visible" >&2; return 2; }
    command -v tmux >/dev/null 2>&1 || { echo "no multiplexer on PATH — what is visible cannot be read" >&2; return 2; }

    # `<prog> status` on stdout, parsed in the shell. No pipe into anything that stops early:
    # under `pipefail` a reader closing the pipe kills the writer with SIGPIPE and the check
    # then fails exactly when it succeeds (law-no-grep-q-under-pipefail).
    #
    # THE `want:` LINE IS TAKEN WHEREVER IT APPEARS, exit code or no exit code. The contract is
    # the line, not the status of the process that printed it, and the answer is checked
    # against the world immediately afterwards — so a follower that stumbled on its way out
    # still gets judged on what it said rather than reported blind for an unrelated fault. The
    # exit code is kept only to say what went wrong when the line never came.
    local out rc line want=""
    out="$("$prog" status 2>/dev/null)"; rc=$?
    while IFS= read -r line; do
        case "$line" in want:*) want="$(_wd_trim "${line#want:}")"; break ;; esac
    done <<< "$out"
    if [ -z "$want" ]; then
        echo "$prog status printed no 'want:' line (exit $rc) — it cannot say which view it is steering to" >&2
        return 1
    fi
    # One word, because it is a session name about to be used as a target. A `status` that
    # printed a sentence there would otherwise be spliced into the query.
    case "$want" in *[[:space:]]*|'') echo "$prog status said 'want: $want', which is not a session name" >&2; return 1 ;; esac

    # `list-windows` and not `display-message`: asked for a session that does not exist,
    # display-message prints nothing and exits 0, so an absent session reads as an empty id and
    # two absent sessions would compare EQUAL and pass. list-windows exits 1 and says which.
    local showing wanted
    showing="$(tmux list-windows -t "=$sess" -F '#{window_id}' -f '#{window_active}' 2>/dev/null)" || showing=""
    if [ -z "$showing" ]; then
        echo "there is no '$sess' session — the surface this steers is not there" >&2
        return 1
    fi
    # The follower links window 0 of the wanted session into the surface, so that is the window
    # that should be showing.
    wanted="$(tmux list-windows -t "=$want" -F '#{window_id}' -f '#{==:#{window_index},0}' 2>/dev/null)" || wanted=""
    if [ -z "$wanted" ]; then
        echo "want: $want, but there is no '$want' session to show" >&2
        return 1
    fi
    if [ "$showing" != "$wanted" ]; then
        echo "want: $want ($wanted) but '$sess' is showing $showing — the state is right and nothing is enacting it" >&2
        return 1
    fi
    return 0
}

# =======================================================================================
# cmd_notify — delivery that does not require a reader to exist.
#
# A SESSION HOOK CANNOT CLOSE THIS HOLE, and that is the whole reason this command exists.
# A hook fires at a SESSION BOUNDARY, which is a property of one client: an event produced
# while nothing is running waits for the next session to open, and for a headless agent that
# is never. So a timer asks the question a boundary cannot — has anything actionable been
# sitting here with nobody to take it — and escalates through the channel that needs no
# session at all.
#
# IT DOES NOT ADVANCE THE CURSOR, and that is not an omission. Escalating is an extra copy of
# the event, never a substitute for it: the lines stay unread, so the next reader to latch
# still gets them. A notify that drained what it reported would make the ask the ONLY delivery
# and would silently clear the condition it was reporting on.
#
# ONLY ACTIONABLE LINES COUNT. A watcher's log is mostly progress, and paging somebody because
# a watcher was busy is the false alarm that teaches them to scroll past the real one
# (law-alerts-must-be-actionable). It is the same expression `drain` and `tail` filter with,
# for the same reason it is a key: a second opinion about what matters is how two commands
# come to disagree.
#
# EXIT  0  nothing has been waiting long enough
#       1  something has, and it has been escalated
#       3  could not check, or could not deliver — never a silent pass
#             (law-absence-needs-a-positive-control)
# =======================================================================================

# How many actionable lines of one watcher go into an ask. The evidence is read in a pane, so
# a backlog of three hundred would bury the decision it is evidence for; the count is stated
# in full and the drain command is named, so nothing is hidden, only deferred.
WD_NOTIFY_MAX=12

# _wd_notify_health — escalate a watcher that has been DEGRADED for longer than the threshold.
#
# `notify`'s other half reports events that WERE produced, so it is blind by construction to
# the failure that stops production. The RESTARTS column is no help either: it counts only
# the restarts watch-refresh makes for a code change, so a unit systemd is respawning in a
# loop shows a still number there. systemd's own NRestarts is the fact that discriminates.
_wd_notify_health() {
    local rows; rows="$(watchd_rows)" || return 3
    local now; printf -v now '%(%s)T' -1

    local name kind target health
    local -a units=() unames=() utarget=() uhealth=()
    while IFS='|' read -r name kind target health; do
        [ -n "$name" ] || continue
        [ "$kind" = daemon ] || continue
        unames+=("$name"); units+=("$(watch_unit_name "$name")")
        utarget+=("$target"); uhealth+=("$health")
    done <<< "$rows"
    [ "${#units[@]}" -gt 0 ] || return 0

    # One exec for every unit (law-fence-loops-on-shared-hardware). `show` answers in blocks
    # keyed by Id, so a unit that has gone cannot shift the answers onto its neighbour.
    local -A ustate=() urestarts=()
    local _id="" _k _v
    while IFS='=' read -r _k _v; do
        case "$_k" in
            Id)          _id="$_v" ;;
            ActiveState) [ -n "$_id" ] && ustate["$_id"]="$_v" ;;
            NRestarts)   [ -n "$_id" ] && urestarts["$_id"]="$_v" ;;
        esac
    done < <(systemctl --user show "${units[@]}" -p Id -p ActiveState -p NRestarts --no-pager 2>/dev/null)

    local i n u state nr lf uf prev_at age last report="" key="" stale=0
    for ((i=0; i<${#unames[@]}; i++)); do
        n="${unames[$i]}"; u="${units[$i]}"
        # NO ANSWER IS NOT A FAULT. systemd may not be running here at all, and reporting that
        # as an unwell watcher pages somebody about the probe rather than about the watcher
        # (law-absence-needs-a-positive-control).
        state="${ustate[$u]:-}"
        [ -n "$state" ] || continue

        if [ "$state" != active ]; then
            if [ -f "$SPIRA_RUN/world.halted" ]; then
                rm -f "$(_wd_unhealthyfile "$n")" 2>/dev/null; continue
            fi
            _wd_hstate=DEGRADED; _wd_hwhy="unit is $state — no writer"
        else
            _wd_probe "${uhealth[$i]}"
        fi

        uf="$(_wd_unhealthyfile "$n")"
        if [ "$_wd_hstate" != DEGRADED ]; then rm -f "$uf" 2>/dev/null; continue; fi

        prev_at=""
        [ -r "$uf" ] && read -r prev_at < "$uf" 2>/dev/null
        case "${prev_at:-}" in ''|*[!0-9]*) prev_at="" ;; esac
        if [ -z "$prev_at" ]; then
            mkdir -p "$(watchd_dir)" 2>/dev/null
            printf '%s\n' "$now" > "$uf"
            continue
        fi
        age=$(( now - prev_at )); [ "$age" -ge 0 ] || age=0
        [ "$age" -ge "$SPIRA_NOTIFY_AGE" ] || continue

        stale=$(( stale + 1 ))
        nr="${urestarts[$u]:-?}"
        lf="$(_wd_logfile "$n" daemon "${utarget[$i]}")"
        last="$(tail -n 1 "$lf" 2>/dev/null)"
        key="$key$n|$_wd_hwhy
"
        report="$report
$n — DEGRADED for $(_wd_age "$age"): $_wd_hwhy
  $u, restarted $nr time(s) by systemd
  last line written: ${last:-(nothing)}
  $lf
"
    done

    if [ "$stale" = 0 ]; then
        rm -f "$(watchd_dir)/notify-health.escalated" 2>/dev/null
        return 0
    fi
    printf '%s\n' "$report"
    _wd_ask notify-health.escalated "$key" \
        "A watcher has stopped producing events" \
        "restart it with \`watchd.sh restart <name>\`, then read the last line above. A unit that restarts without ever becoming active is usually a second copy started by hand holding its lock — retiring that copy is the fix, and \`Restart=always\` respawns on a clean exit too, so the loop never ends on its own. If the watcher is meant to be stopped, take its row out of the manifest instead of leaving a unit systemd will respawn forever." \
        "a watcher that is not running produces no events, and a watcher producing no events is indistinguishable from one with nothing to say. Nothing else escalates this: the other half of \`notify\` reports events that WERE produced, so the failure that stops production is exactly the one it cannot see." \
        "$report" || return 3
    return 1
}

cmd_notify() {
    [ $# -eq 0 ] || { echo "usage: watchd.sh notify" >&2; return 3; }
    # A THRESHOLD THAT CANNOT BE READ IS REFUSED. Left to `[ x -ge junk ]` it would fail every
    # comparison and turn into "never escalate", which is this command doing nothing while
    # reporting success — the exact failure it was written to end.
    case "${SPIRA_NOTIFY_AGE:-}" in
        ''|*[!0-9]*)
            echo "watchd: SPIRA_NOTIFY_AGE is '${SPIRA_NOTIFY_AGE:-}' — it must be a whole number of seconds" >&2
            return 3 ;;
    esac
    local re; re="$(_wd_filter)" || return 3
    local rows; rows="$(watchd_rows)" || return 3
    local now; printf -v now '%(%s)T' -1

    local name kind target health lf total pos chunk hit off line apos pend
    local stamp_age oldest stamp_at
    local prev_pos prev_at age shown k report="" key="" stale=0
    while IFS='|' read -r name kind target health; do
        [ -n "$name" ] || continue
        # A WATCHER THIS INSTALLATION HAS NOT GOT CANNOT HAVE A BACKLOG. Nothing writes a log
        # for an `off` row, so there is nothing standing unread and nobody to wake about it.
        # Refused here rather than left to fall through: an unnamed log makes `_wd_total`
        # answer 0, so the row would reach the same verdict by accident, and a silence that
        # depends on an unrelated helper's handling of an empty path is indistinguishable
        # from the silence of a check that has stopped looking
        # (law-absence-needs-a-positive-control).
        [ "$kind" = off ] && continue
        lf="$(_wd_logfile "$name" "$kind" "$target")"
        total="$(_wd_total "$lf")"
        pos="$(_wd_pos "$name" "$total")"
        pend="$(_wd_pendfile "$name")"

        apos=0; line=""; shown=""
        if [ "$total" -gt "$pos" ]; then
            # THE SAME EXACT RANGE `drain` READS, and for the same reason: the log is being
            # appended to while this runs, so a bare `tail -n +N` would count lines written
            # after `total` was taken and put a position in the clock that no reader has.
            chunk="$(sed -n "$(( pos + 1 )),${total}p" "$lf" 2>/dev/null)"
            # NO PIPE INTO `grep -m1`. It stops at the first match and closes the pipe, the
            # writer dies of SIGPIPE, and `pipefail` turns a successful search into 141
            # (law-no-grep-q-under-pipefail).
            hit="$(grep -nE -m1 -- "$re" <<< "$chunk")" || hit=""
            if [ -n "$hit" ]; then
                off="${hit%%:*}"; line="${hit#*:}"
                apos=$(( pos + off ))
                shown="$(grep -E -- "$re" <<< "$chunk")" || shown=""
            fi
        fi

        # NOTHING A READER MUST ACT ON IS WAITING, so the clock is over. Deleting it here is
        # what makes draining clear the condition rather than merely pause it, and it is why
        # the same backlog re-escalates if it comes back: the next standing event starts a new
        # clock rather than inheriting a matured one.
        if [ "$apos" = 0 ]; then rm -f "$pend" 2>/dev/null; continue; fi

        # The producer's own clock, when it declared one: a `[<ISO8601>]` prefix. Oldest
        # wins, and lexicographic order is chronological because the format is fixed-width
        # UTC. Unstamped watchers fall through to first-sighting below.
        stamp_age=""
        oldest="$(printf '%s\n' "$shown" \
                  | sed -n 's/^\[\([0-9][0-9-]\{9\}T[0-9:]\{8\}Z\)\].*/\1/p' | sort | head -1)"
        if [ -n "$oldest" ]; then
            stamp_at="$(date -u -d "$oldest" +%s 2>/dev/null)" || stamp_at=""
            case "${stamp_at:-}" in ''|*[!0-9]*) stamp_at="" ;; esac
            [ -n "$stamp_at" ] && stamp_age=$(( now - stamp_at ))
        fi

        prev_pos=""; prev_at=""
        [ -r "$pend" ] && read -r prev_pos prev_at < "$pend" 2>/dev/null
        case "${prev_at:-}" in ''|*[!0-9]*) prev_at="" ;; esac

        # A BACKLOG THIS PASS HAS NOT SEEN BEFORE STARTS ITS CLOCK NOW, and is not stale yet.
        # That under-reports by up to one threshold for an event written while this timer was
        # not running — the clock dates from when the harness first SAW the event standing,
        # not from when it was written, because a line carries no timestamp this can trust.
        # Under-reporting is the conservative direction for something whose failure mode is
        # waking somebody who did not need waking.
        if [ -z "$prev_at" ] || [ "${prev_pos:-}" != "$apos" ]; then
            mkdir -p "$(watchd_dir)" 2>/dev/null
            printf '%s %s\n' "$apos" "$now" > "$pend"
            # A stamped backlog is not granted a grace period it has already spent, so the
            # first sighting can fire. An unstamped one has no age yet and must wait.
            [ -n "$stamp_age" ] || continue
        fi

        if [ -n "$stamp_age" ]; then age="$stamp_age"; else age=$(( now - prev_at )); fi
        # A clock in the future is a clock that moved, not an event that is unusually old.
        [ "$age" -ge 0 ] || age=0
        [ "$age" -ge "$SPIRA_NOTIFY_AGE" ] || continue

        stale=$(( stale + 1 ))
        k="$(printf '%s\n' "$shown" | wc -l)"
        # THE KEY CARRIES NOTHING THAT CHANGES ON ITS OWN — not the age, not the count, not
        # the log's length. It is the identity of the backlog, and the escalation below is
        # suppressed while it holds, so anything volatile in here would make a standing
        # condition ask again on every pass, which is the false-alarm generator this is
        # required not to be.
        key="$key$name|$apos|$line
"
        report="$report
$name — $k actionable event(s) with no reader, the oldest for $(_wd_age "$age")
  $lf
$(printf '%s\n' "$shown" | head -"$WD_NOTIFY_MAX" | sed 's/^/    /')"
        [ "$k" -gt "$WD_NOTIFY_MAX" ] && report="$report
    ... and $(( k - WD_NOTIFY_MAX )) more — all of them: watchd.sh drain $name"
        report="$report
"
    done <<< "$rows"

    local found=0
    if [ "$stale" = 0 ]; then
        # THE CONDITION IS OVER, SO THE SUPPRESSION IS TOO. Without this a backlog that was
        # escalated, drained, and then recurred identically would be silently swallowed —
        # the fingerprint would still match, and the second occurrence would reach nobody.
        rm -f "$(watchd_dir)/notify.escalated" 2>/dev/null
    else
        printf '%s\n' "$report"
        _wd_escalate "$key" "$report" || return 3
        found=1
    fi

    # BOTH HALVES RUN, WHATEVER THE FIRST FOUND. An unread backlog and a dead watcher are
    # opposite symptoms — one watcher producing into nobody's hands, another producing
    # nothing at all — and returning early on the first would let a noisy watcher hide a
    # dead one for as long as it kept talking.
    _wd_notify_health; local hrc=$?
    [ "$hrc" = 3 ] && return 3
    [ "$hrc" = 1 ] && found=1
    bash "$SPIRA_HOME/mail-health.sh"; local mhrc=$?
    [ "$mhrc" = 3 ] && return 3
    [ "$mhrc" = 1 ] && found=1
    [ "$found" = 1 ] && return 1
    return 0
}

# _wd_escalate <key> <report> — raise the ask once per distinct backlog, never once per pass.
#
# Keyed on a fingerprint of the backlog rather than on a clock, because the condition persists
# until somebody acts on it and an escalation repeated every pass is the noise that teaches
# the operator to scroll past the one that matters. A CHANGE in the backlog is new information
# and does ask again.
#
# THAT KEYING ALSO MAKES THIS LOOP-SAFE, which is not incidental: raising an ask writes a bead,
# a watcher may well emit a line about that bead, and that line lands BEHIND the standing
# event. The key names the OLDEST actionable unread line, so it does not move, and the second
# pass raises nothing.
#
# THE FINGERPRINT IS WRITTEN ONLY AFTER THE ASK WAS ACCEPTED. Stamping first would mean a
# broken escalation path silently consumed the one notification this backlog will ever
# produce: the finding would be marked delivered, and the retry that would have carried it
# once the path was repaired never happens.
# _wd_ask <stamp-name> <key> <title> <default> <why> <evidence>
#
# Suppressed while <key> is unchanged, so a standing condition asks once rather than once per
# pass. Each caller names its own stamp: one shared stamp would let whichever kind escalated
# last erase the other's fingerprint and ask again.
_wd_ask() {
    local stamp fp prev=""
    stamp="$(watchd_dir)/$1"
    fp="$(printf '%s' "$2" | cksum | tr -d ' ')"
    [ -f "$stamp" ] && prev="$(cat "$stamp" 2>/dev/null)"
    [ "$fp" = "$prev" ] && return 0

    if [ ! -x "${SPIRA_NOTIFY:-}" ]; then
        echo "watchd: no escalation path at ${SPIRA_NOTIFY:-<unset>} — the findings above reach nobody" >&2
        return 1
    fi
    "$SPIRA_NOTIFY" add "$3" --default "$4" --why "$5" --evidence "$6" >/dev/null 2>&1 || {
        echo "watchd: the escalation path refused the ask — the findings above reach nobody" >&2
        return 1
    }
    mkdir -p "$(watchd_dir)" 2>/dev/null
    printf '%s' "$fp" > "$stamp"
    return 0
}

_wd_escalate() {
    _wd_ask notify.escalated "$1" \
        "Events a watcher produced have reached no reader" \
        "read them below and act on them here — nothing has been marked read, so the next session to latch still gets them; if a line of this kind is never worth waking anyone for, narrow SPIRA_ACTIONABLE rather than lengthening SPIRA_NOTIFY_AGE" \
        "delivery of a watcher's events otherwise depends on a session existing to drain them, and a session hook fires at a session boundary — so an event produced while nothing is running waits for the next session to open, which for a headless agent never comes. Nothing else will surface these." \
        "$2"
}

# SOURCEABLE, AND SILENT WHEN IT IS. `watch-refresh.sh` reads the manifest and restarts a
# unit through the functions above rather than by running this script, because its whole
# contract is that a steady pass costs two execs and a fork to parse a file it could parse
# in-process would be a third. Without this guard, sourcing runs the dispatcher against the
# CALLER's arguments — so a library read would have executed whatever the caller's first
# argument happened to name, and the default is `status`, which probes every watcher.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    case "${1:-status}" in
        manifest) cmd_manifest ;;
        units)    cmd_units ;;
        keys)     cmd_keys ;;
        exec)     shift; cmd_exec "${1:-}" ;;
        status)   cmd_status ;;
        drain)    shift; cmd_drain "$@" ;;
        peek)     shift; cmd_drain --peek "$@" ;;
        tail)     shift; cmd_tail "$@" ;;
        tailers)  cmd_tailers ;;
        restart)  shift; cmd_restart "${1:-}" ;;
        notify)   shift; cmd_notify "$@" ;;
        health-ids) shift; cmd_health_ids "${1:-}" ;;
        health-view) shift; cmd_health_view "${1:-}" "${2:-}" ;;
        *) echo "usage: watchd.sh manifest|units|keys|exec <name>|status|drain [name] [--all]|peek [name] [--all] [--limit N]|tail <name> [--all] [--takeover]|tailers|restart [name]|notify|health-ids <file>|health-view <program> <session>" >&2
           exit 2 ;;
    esac
fi
