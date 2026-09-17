#!/usr/bin/env bash
#
# cockpit.sh — gather Spira health into .runtime/spira/cockpit.env for the cockpit pane.
#
#   cockpit.sh          collect once, then attach to the concierge (SPIRA_COCKPIT_NO_ATTACH=1 skips)
#   cockpit.sh once     one pass, write the snapshot, append a history row, exit
#   cockpit.sh loop     forever, every INTERVAL seconds
#   cockpit.sh history  append one history row from the snapshot already on disk
#
# WHY A SNAPSHOT FILE AND NOT DIRECT CALLS
# ----------------------------------------
# The pane repaints every two seconds. A pane that shelled out to `bd` and `git` would
# freeze on every repaint and paint stale frames while it did — the reason the town
# collector exists, arriving here for the same reason. The cost is paid once per pass; the
# pane reads a file.
#
# Flat `KEY='value'` so a renderer can `source` it with no parser. EVERY VALUE IS
# SHELL-QUOTED: the town snapshot once carried an unquoted `SCHED_CAP=direct dispatch
# (scheduler.max_polecats=-1)`, which is a syntax error that aborts the source, so every
# key after it silently read as unset and the whole panel rendered "?" while the collector
# looked healthy. Writes are atomic, so a half-written file cannot wedge a pane.
#
# SEPARATE FROM .runtime/cockpit.env, DELIBERATELY. That file is Gas Town's and is deleted
# with it; this one is Spira's and must survive that.
#
# THE RULE THAT GOVERNS EVERY PROBE: a probe that fails renders `?`, never 0. The town
# collector's first version returned 0 from its exception handler, so a broken parser
# displayed as "no parked beads" — a panel that reports a broken check as all-clear
# displaces the suspicion that would have prompted a look.
set -uo pipefail
. "$(dirname "$0")/lib.sh"
# The analyser is addressed as a SIBLING of this file, not through $SPIRA_HOME. A collector
# running from a worktree while its analyser resolves to the installed copy on main is a
# version skew that shows up as `?` on the panel and as nothing at all in any log.
HERE="$(cd "$(dirname "$0")" && pwd)"
COCK_DIR="$SPIRA_COCKPIT"

SNAP="$SPIRA_RUN/cockpit.env"
# No single $REPO. Every git question here is asked of the repository the BEAD named, or of
# every registered repository — see spira_repos / repo_root in lib.sh.
WINDOW_HOURS="${SPIRA_COCKPIT_WINDOW_HOURS:-24}"
INTERVAL="${SPIRA_COCKPIT_INTERVAL:-60}"

# THE SNAPSHOT HAS EXACTLY ONE WRITER: spira-cockpit.service. A second writer — an aeon
# running the collector from its worktree, a retired brain collector calling a vendored copy,
# a manual `cockpit.sh once` — overwrites the live snapshot with whatever keys its branch
# carries, and the pane reads `?` for every key the interloper lacked. The fence is
# INVOCATION_ID, which systemd sets for exactly one process tree per invocation: if this
# process's INVOCATION_ID matches the service's, it IS the service. SPIRA_COCKPIT_FORCE=1
# names the override (a fence, not a wall).
cockpit_may_write() {
    [ "${SPIRA_COCKPIT_FORCE:-0}" = 1 ] && return 0
    [ -n "${INVOCATION_ID:-}" ] || return 1
    # THE UNIT NAME IS PER-INSTANCE. install.sh renders spira-cockpit-<instance>.service, so
    # comparing against the plain name refused every write from the moment the migration
    # landed: 225 restarts and an hour of frozen snapshot, while the pane showed a STALE
    # reading rather than a fault (2026-09-08). Both names are tried, so this works before a
    # migration and after one, and on any instance.
    local svc_id u
    for u in "spira-cockpit${SPIRA_INSTANCE:+-$SPIRA_INSTANCE}.service" spira-cockpit.service; do
        svc_id="$(systemctl --user show "$u" -p InvocationID --value 2>/dev/null)" || continue
        [ -n "$svc_id" ] && [ "$INVOCATION_ID" = "$svc_id" ] && return 0
    done
    return 1
}

# count <bd-args...> -> number of rows, or `?` if the query or the parse failed.
count() {
    bdq "$@" --json 2>/dev/null | json_only | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: print("?"); raise SystemExit
print(len(d if isinstance(d, list) else [d]))' 2>/dev/null || echo "?"
}

# age_of <path> -> seconds since last write, or `?` if it does not exist.
age_of() {
    local f="$1" m
    m="$(stat -c %Y "$f" 2>/dev/null)" || { printf '?'; return; }
    [ -n "$m" ] || { printf '?'; return; }
    printf '%d' $(( $(date +%s) - m ))
}

# unit_active <unit> -> 1 (active), 0 (not active), or ? (unit unknown to systemd).
# A timer that is not active is why nothing is happening, and it is the first thing to look
# at when every other number has stopped moving. A unit that cannot be found must not be
# reported as 0 — that reads as "not active" about a known unit, which displaces the suspicion
# that it might not exist at all (law-absence-needs-a-positive-control).
unit_active() {
    [ "$1" = '?' ] && { printf '?'; return; }
    [ "$(systemctl --user is-active "$1" 2>/dev/null)" = active ] && printf 1 || printf 0
}


probe() {
    # Backward-compat wrapper: calls the three tier functions sequentially then the
    # existing per-subcommand functions, keeping cockpit.sh once/loop working unchanged.
    # SP_AT FIRST: now_keys emits it as its very first output line.
    local _probe_start; _probe_start=$(date +%s)
    now_keys
    core_detail_keys
    core_counts_keys
    unsent_keys
    queue_keys
    sphere_keys
    repo_label_keys
    strand_keys
    dup_refs_keys
    livelock_keys
    sop_keys
    ratelim_keys
    statute_keys
    # SP_PASS_SECS: captures the total pass duration including all tier functions.
    echo "SP_PASS_SECS=$(( $(date +%s) - _probe_start ))"
}

# Fast-tier keys: filesystem and /proc only. Called as `cockpit.sh now` by collect.sh;
# also called by probe() as part of the backward-compat serial pass.
# Emits SP_AT first so the badge in health.sh bounds the age of the oldest reading.
now_keys() {
    local _probe_start; _probe_start=$(date +%s)
    echo "SP_AT=$_probe_start"
    echo "SP_WINDOW_HOURS=$WINDOW_HOURS"

    # ---- NOW: what each aeon is doing, by name -----------------------------------------
    # The pane repaints every two seconds and must never shell out, so the live picture is
    # assembled here: which named aeon holds which bead, for how long, and the last thing
    # it actually did — read from its stream-json trace, which is the only honest answer to
    # "is it working and on what".
    local i=0
    for pf in "$SPIRA_RUN"/aeon-*.pid; do
        [ -e "$pf" ] || continue
        aeon_alive "$pf" || continue
        local base name bead fay secs
        base="$(basename "$pf" .pid)"          # aeon-<fayth>-<bead>
        fay="$(printf '%s' "$base" | cut -d- -f2)"
        bead="$(printf '%s' "$base" | cut -d- -f3-)"
        name="$(aeon_named "$pf")"
        secs="$(ps -o etimes= -p "$(cat "$pf" 2>/dev/null)" 2>/dev/null | tr -d ' ')"
        # The title, so NOW says what is being worked and not only its id — the same help
        # NEXT gives for queued work.
        # THE PRIORITY COMES BACK WITH THE TITLE, from the one call already being made.
        # NEXT and RECENT both lead with P<n>; NOW did not, so the three sections describing
        # the same beads at three stages of one lifecycle did not line up and could not be
        # compared down the column (the operator: "parallel structure with the NEXT and
        # RECENT ones").
        # PARTITION AND TITLE FROM THE STORE, not from label parsing. bd state reads the
        # fayth dimension written by bd set-state — the single-valued attribute the harness
        # already maintains, so this cannot disagree with the claim view.
        # REPO LABEL FROM THE SAME CALL. spira_landref needs a repo name; reading it from
        # the same bdjson call that already runs here adds no round trip.
        local title pri partition meta repo_name _meta_tmp
        meta="$(bdjson show "$bead" 2>/dev/null | python3 -c '
import sys, json, re
try: d = json.load(sys.stdin); i = (d if isinstance(d, list) else [d])[0]
except Exception: raise SystemExit
repo = next((l[5:] for l in (i.get("labels") or []) if l.startswith("repo:")), "")
print("%s\t%s\t%s" % (i.get("priority"),
    re.sub(r"[^ A-Za-z0-9._/:,()#+-]", " ", (i.get("title") or ""))[:80],
    repo))' 2>/dev/null)"
        pri="${meta%%$'\t'*}"
        _meta_tmp="${meta#*$'\t'}"
        title="${_meta_tmp%%$'\t'*}"
        repo_name="${_meta_tmp#*$'\t'}"
        [ "$pri" = "$meta" ] && { pri=""; title=""; repo_name=""; }
        partition="$(bdq state "$bead" fayth 2>/dev/null)" || partition="?"
        [ -n "$partition" ] || partition="?"
        echo "SP_AEON${i}_PRI=${pri:-?}"
        echo "SP_AEON${i}_PARTITION=${partition:-?}"
        echo "SP_AEON${i}_TITLE=${title:-?}"
        echo "SP_AEON${i}_NAME=${name:-?}"
        echo "SP_AEON${i}_FAYTH=${fay:-?}"
        # FAYTH_MODEL is what this aeon was summoned with — the model the fayth file declared.
        # The trace's own MODEL is what actually ran; the two differ when a provider substitutes
        # silently without error. Reading the chamber file rather than the aeon's environment lets
        # the collector emit it without a second pass over the trace. `?` when the file cannot be
        # read, never empty (law-absence-needs-a-positive-control).
        local _fayth_mdl
        _fayth_mdl="$(sed -n 's/^FAYTH_MODEL=//p' "$HERE/chamber/$fay.fayth" 2>/dev/null | head -1)"
        echo "SP_AEON${i}_FAYTH_MODEL=${_fayth_mdl:-?}"
        echo "SP_AEON${i}_BEAD=${bead:-?}"
        echo "SP_AEON${i}_MIN=$(( ${secs:-0} / 60 ))"
        # LIVENESS FUSE — see aeon_fuse_minutes in lib.sh for the full reasoning.
        # Shared with aeon.sh so the display and the trip always agree on when the fuse burns.
        echo "SP_AEON${i}_FUSE=$(aeon_fuse_minutes "$bead" "$SPIRA_RUN/worktree/$bead" "${repo_name:-}")"
        echo "SP_AEON${i}_WALL=$(( ${secs:-0} / 60 ))"
        # LIVENESS LEASE COUNTDOWN — see aeon_lease_minutes in lib.sh. One file read per
        # aeon per pass; the file is written by the heartbeat subshell and is the single
        # source shared between the killer and this display (law-verify-the-discriminating-fact).
        echo "SP_AEON${i}_LEASE=$(aeon_lease_minutes "$bead" 2>/dev/null || echo ?)"
        # HOW HEALTHY THE SESSION IS, not merely that it exists. TURNS CTX TOOLS FILES QUIET
        # ACT SAID, from ONE streaming read of the aeon's stream-json trace — the only
        # artifact that knows any of it. Held to one read per aeon per pass because the trace
        # is the single thing here that grows without bound; the pane reads what this wrote
        # and never opens the file itself.
        #
        # trace_stats SANITISES ITS OWN VALUES, and it has to: they are arbitrary text from an
        # agent — a shell command, a code fragment, a sentence — and this file is sourced by
        # the pane with no parser. A newline injects extra lines and an "=" makes a bogus key;
        # the pane rendered a tools help page where the ops summary belongs before that was
        # clamped at the source.
        #
        # A KEY IT DOES NOT EMIT STAYS UNSET, and the renderer shows `?` for it. That is the
        # wanted behaviour rather than a gap: a failed read must never arrive as a zero
        # (law-absence-needs-a-positive-control).
        trace_stats "$SPIRA_RUN/$bead.log" 2>/dev/null | sed "s/^/SP_AEON${i}_/"
        # THE TRAILING MOMENTS, from trace_tail. Each line is sanitised by the same
        # allowlist trace_stats uses for ACT: the snapshot is a KEY=value file the pane
        # SOURCES, so a newline injects extra lines and an "=" makes a bogus key. trace_tail
        # has no such clamp — it is multi-line by design and emits $ and -> prefixes —
        # so every line goes through the allowlist before it becomes a value.
        # TWO, AND conf.sh AND THE RENDERER AGREE. A collector emitting fewer keys than
        # the pane reads is a section that silently renders short.
        local tl="${SPIRA_COCKPIT_TRACE_LINES:-2}"
        if [ "$tl" -gt 0 ] 2>/dev/null; then
            trace_tail "$SPIRA_RUN/$bead.log" "$tl" 2>/dev/null | python3 -c '
import sys, re
ALLOW = re.compile(r"[^ A-Za-z0-9._/:,()#+-]")
pfx = sys.argv[1]
n = int(sys.argv[2])
lines = []
for raw in sys.stdin:
    c = re.sub(r"\s+", " ", ALLOW.sub(" ", raw)).strip()[:96]
    if c:
        lines.append(c)
for j, l in enumerate(lines[-n:]):
    print("%sACT%d=%s" % (pfx, j, l))
' "SP_AEON${i}_" "$tl" 2>/dev/null
        fi
        i=$((i+1))
    done
    echo "SP_AEON_N=$i"

    # ---- the harness itself -----------------------------------------------------------
    # strand.sh names this as its own blind spot: a check cannot observe the failure of the
    # thing running it. The collector is not the sentinel, so it can — and this is what
    # makes every number below interpretable, because a stale graph under a dead sentinel
    # looks exactly like a quiet one under a live sentinel.
    echo "SP_SENTINEL_TIMER=$(unit_active "$(spira_unit sentinel timer)")"
    echo "SP_SENTINEL_AGE=$(age_of "$SPIRA_RUN/sentinel.log")"
    echo "SP_OPS_TIMER=$(unit_active "$(spira_unit ops timer)")"
    # SP_OPS_AGE: "ops is inside a session" vs "ops has stopped". ops.log only gets new lines
    # from aeon.sh's own log() calls, which are silent during the 480s claude session itself,
    # so its mtime is frozen while ops is actually working — indistinguishable from a dead
    # collector. A live ops pid file is the authoritative "busy" signal (law-alerts-must-be-actionable).
    _ops_age="$(age_of "$SPIRA_RUN/ops.log")"
    for _ops_pf in "$SPIRA_RUN"/aeon-ops-*.pid; do
        [ -e "$_ops_pf" ] || continue
        if aeon_alive "$_ops_pf"; then _ops_age=0; break; fi
    done
    echo "SP_OPS_AGE=$_ops_age"
    unset _ops_pf _ops_age
    # AURON'S OWN PULSE, and it is here for the same reason the two above are: a check
    # cannot observe the failure of the thing running it. Auron watches the loop, so the
    # one thing IT cannot report is that it has stopped — and a silent watchdog and a
    # healthy system look identical, with the pane rendering the healthy reading
    # (law-absence-needs-a-positive-control).
    #
    # THE HEARTBEAT, NOT THE LOG. auron.log is appended to by systemd on every run
    # including a run that died on its first line; auron.status is written by auron.sh
    # itself, last, only once a whole pass has completed. Only the second one distinguishes
    # "it ran" from "it worked".
    echo "SP_AURON_TIMER=$(unit_active "$(spira_unit auron timer)")"
    echo "SP_AURON_AGE=$(age_of "$SPIRA_RUN/auron.status")"
    # What it is currently saying. A failed read renders `?`, never 0: "no alerts firing"
    # is the reassuring answer and must never be the one a broken probe produces.
    if [ -r "$SPIRA_RUN/auron.status" ]; then
        echo "SP_AURON_FIRING=$(. "$SPIRA_RUN/auron.status" 2>/dev/null; printf '%s' "${SP_AURON_FIRING:-?}")"
        echo "SP_AURON_KEYS='$(. "$SPIRA_RUN/auron.status" 2>/dev/null; printf '%s' "${SP_AURON_KEYS:-}")'"
    else
        echo "SP_AURON_FIRING=?"
        echo "SP_AURON_KEYS=''"
    fi


    # gate-run/ — live gate runs. LIVENESS FROM /proc ON argv, never from directory existence
    # or pgrep -f (law-absence-needs-a-positive-control). Nothing prunes gate-run/, so stale
    # directories are the trap: a pid file that points at a dead process, or one recycled by
    # another program, must never render as a live gate.
    local gate_n=0 gate_live=0
    if [ -d "$SPIRA_RUN/gate-run" ]; then
        for _gd in "$SPIRA_RUN/gate-run"/*/; do
            [ -e "$_gd/pid" ] || continue
            local _g_slug _g_pid _g_started _g_age _g_state _g_cmd _g_phase
            _g_slug="$(basename "$_gd")"
            _g_pid="$(cat "$_gd/pid" 2>/dev/null)"; [ -n "${_g_pid:-}" ] || continue
            _g_started="$(cat "$_gd/started" 2>/dev/null)"

            _g_state=dead
            if [ -d "/proc/$_g_pid" ]; then
                # The same stderr-safe pattern as gate-run.sh itself: the shell's failed
                # redirection to a vanished /proc entry goes to stderr, which pollutes the
                # snapshot if not suppressed.
                { _g_cmd="$(tr '\0' ' ' < "/proc/$_g_pid/cmdline")"; } 2>/dev/null
                # argv decides, not the directory. A recycled pid belongs to a different
                # process whose cmdline will not contain 'gate'.
                [[ "${_g_cmd:-}" == *gate* ]] && _g_state=live
            fi

            [ "$_g_state" = live ] || continue
            gate_live=$((gate_live+1))

            if [ -n "${_g_started:-}" ]; then
                _g_age=$(( $(date +%s) - _g_started ))
            else
                _g_age="?"
            fi

            # Waiting on the tree lock vs running suites: gate.sh produces no output
            # until after flock, so an empty out file means the gate is queued.
            if [ -s "$_gd/out" ]; then
                _g_phase=running
            else
                _g_phase=waiting
            fi

            # Why a run is slow — "selecting all" is the line that explains a 650s run.
            local _g_why=""
            if [ "$_g_phase" = running ] && [ -r "$_gd/out" ]; then
                _g_why="$(grep -m1 'selecting' "$_gd/out" 2>/dev/null)"
                _g_why="$(printf '%s' "${_g_why:-}" | tr -c 'A-Za-z0-9 ._/:,()#+-' ' ' | tr -s ' ')"
                _g_why="${_g_why:0:100}"
            fi

            printf 'SP_GATE%d_SLUG=%s\n' "$gate_n" "$_g_slug"
            printf 'SP_GATE%d_AGE=%s\n' "$gate_n" "${_g_age:-?}"
            printf 'SP_GATE%d_PHASE=%s\n' "$gate_n" "$_g_phase"
            [ -n "$_g_why" ] && printf 'SP_GATE%d_WHY=%s\n' "$gate_n" "$_g_why"
            gate_n=$((gate_n+1))
        done
    fi
    echo "SP_GATE_N=$gate_n"
    echo "SP_GATE_LIVE=$gate_live"

    # ---- live aeons --------------------------------------------------------------------
    # /proc, never a directory count and never `pgrep -f`. `gt polecat list` counting
    # DIRECTORIES is the original scar; `pgrep -f` is the second one, where the pattern
    # matches the searching process's own command line. aeon_alive reads argv of the
    # recorded pid.
    #
    # Read-only: a stale pidfile is left where it is rather than swept, because a collector
    # that mutates the state it reports can race the harness that owns it.
    local n=0 pf
    for pf in "$SPIRA_RUN"/aeon-*.pid; do
        [ -e "$pf" ] || continue
        aeon_alive "$pf" && n=$((n+1))
    done
    echo "SP_AEONS=$n"

    # ---- the account's own capacity -----------------------------------------------------
    # "Nothing is moving" and "nothing is moving because the account is out until 15:00" are
    # the same pixels without this, and the first of those is the reading that prompts
    # somebody to go looking for a fault that does not exist.
    #
    # READ-ONLY, unlike everywhere else this predicate is asked. capacity_paused deletes an
    # expired pause file and announces the reopening; a collector that ran every minute would
    # win that race against the sentinel and swallow the announcement into a snapshot nobody
    # reads. So the epoch is compared here by hand and the file is left for its owner.
    local cap_at cap_now
    cap_at="$(capacity_pause_until)"; cap_now="$(date +%s)"
    if [ "${cap_at:-0}" -gt "$cap_now" ] 2>/dev/null; then
        echo "SP_CAPACITY_PAUSED=1"
        echo "SP_CAPACITY_LEFT=$(( cap_at - cap_now ))"
        echo "SP_CAPACITY_AT=$(date -d "@$cap_at" +%H:%M 2>/dev/null)"
        echo "SP_CAPACITY_WHY=$(capacity_pause_why 2>/dev/null)"
    else
        echo "SP_CAPACITY_PAUSED=0"
        echo "SP_CAPACITY_LEFT=0"
        echo "SP_CAPACITY_AT="
        echo "SP_CAPACITY_WHY="
    fi
}

# Medium-tier fast counts — SP_WAITING. Called as `cockpit.sh core` alongside
# core_detail_keys. SP_READY is owned by core_detail_keys (same population as NEXT);
# emitting it here would produce a second SP_READY=0 after core_detail_keys emits SP_READY=?
# on a failed probe, which is the reassuring answer a broken probe must never produce.
core_counts_keys() {
    # SP_WAITING — open needs-operator beads (same distinguish-refusal pattern).
    local _wait_raw
    _wait_raw="$(bdjson list --status open --limit 0 --label "$SPIRA_ASK_LABEL" 2>/dev/null)"
    if [ -z "$_wait_raw" ]; then
        echo "SP_WAITING=?"
    else
        local _w; _w="$(printf '%s\n' "$_wait_raw" | json_count)"
        echo "SP_WAITING=${_w:-?}"
    fi

}

# Slow-tier core detail: per-partition ready listing, event feed, CI parking,
# throughput sparklines, token meters. Called as `cockpit.sh core_detail`.
core_detail_keys() {
    # PARTITION MAP BUILT DIRECTLY FROM THE CHAMBER — fayth_get resolves every variable the
    # summoner would, so scope labels and other expansions cannot survive unexpanded into a
    # query label (the defect that turned an unreadable chamber into "0 ready").
    # AN UNREADABLE CHAMBER IS A REFUSAL, NOT AN EMPTY QUEUE. See the aggregator below.
    local _f _fname _flabels _frows="" _PART_MAP=""
    for _f in $(spira_fayths 2>/dev/null); do
        _fname="$(fayth_get "$_f" FAYTH_NAME "$_f" 2>/dev/null)"
        _flabels="$(fayth_get "$_f" FAYTH_LABELS 2>/dev/null)"
        [ -n "$_fname" ] && [ -n "$_flabels" ] || continue
        _frows="${_frows}${_flabels}\t${_fname}\n"
    done
    if [ -n "$_frows" ]; then
        _PART_MAP="$(printf '%b' "$_frows" | python3 -c '
import sys, json
result = {}
for line in sys.stdin:
    line = line.rstrip("\n")
    if "\t" not in line: continue
    labels, _, name = line.partition("\t")
    if labels and name: result[labels] = name
print(json.dumps(result))
' 2>/dev/null)" || _PART_MAP=""
    fi

    # ---- NEXT: ready beads from EVERY declared partition, ordered by priority ------------
    # PARTITIONS COME FROM THE CHAMBER. Each .fayth defines its own label predicate; a new
    # persona appears here with no edit because _PART_MAP was already read at the top of
    # probe(). Querying only builder's `spira,plan` left the incident queue invisible: a
    # stalled incident looked identical to an empty one on the pane Ryan reads.
    #
    # CLAIM ORDER IS PARTITION-SPECIFIC, not global. Each partition is drained by its own
    # persona concurrently, so there is no single claim order across them. Interleaved by
    # priority this shows outstanding P0 work everywhere, which is what was asked for; the
    # header now says so rather than promising a single order that does not exist.
    #
    # SP_READY IS EMITTED HERE because it is the same population: the union across partitions
    # that NEXT shows. Counting it again at the old site would be a second query for a number
    # already in hand; the old PLAN_READY_ARGS site is removed along with the hardcoded label.
    {
        # AN UNREADABLE CHAMBER IS A REFUSAL, NOT AN EMPTY QUEUE. With no partitions the loop
        # below never runs, rows stays empty, and the aggregator prints SP_READY=0 — the
        # reassuring answer a broken probe must never produce. Emit the sentinel instead.
        if [ -z "$_PART_MAP" ]; then
            printf '%s\n' '{"_refused": true}'
        else
        python3 -c '
import sys, json
for labels, name in json.loads(sys.argv[1]).items():
    print(name + "\t" + labels)
' "$_PART_MAP" 2>/dev/null
        fi
    } | while IFS=$(printf '\t') read -r _pname _plabels; do
        # The sentinel line has no tab, so _plabels is empty — pass it straight through.
        if [ -z "$_plabels" ]; then printf '%s\n' "$_pname"; continue; fi
        bdjson "${READY_ARGS[@]}" \
            --label "$_plabels" \
            --exclude-label "spira-poison,$SPIRA_ASK_LABEL,$SPIRA_CI_LABEL" \
            2>/dev/null | python3 -c '
import sys, json
name = sys.argv[1]
raw = sys.stdin.read()
if not raw.strip():
    # bdjson produced no output — bd refused (schema mismatch, timeout) rather than returning
    # an empty list. A genuine empty response arrives as "[]" which json_only passes through;
    # empty output means the JSON was never produced. Emit a sentinel so the aggregator can
    # distinguish a refusal from a true zero (law-failed-probe-renders-question).
    print(json.dumps({"_refused": True}))
else:
    try:
        d = json.loads(raw)
        for r in (d if isinstance(d, list) else [d]):
            r["_partition"] = name
            print(json.dumps(r))
    except Exception:
        print(json.dumps({"_refused": True}))
' "$_pname" 2>/dev/null
    done | python3 -c '
import sys, json, re
# THE PARTITION MAP IS PASSED AS argv[1] so that fayth: preferences can be checked at
# render time. A bead carrying fayth:ops on labels that match the builder partition is
# shown by the partition query as "builder" — but builder is excluded by the preference.
# Without the map we cannot compute which persona would actually claim it; with it we
# either name the right persona or mark the bead "unclaimable". A bead named "builder"
# when no builder can claim it is worse than "unclaimable": it displaces the suspicion
# that would have prompted a look. (sp-f8vry: 7 P1 beads shown as "builder" for 15h.)
try:
    part_map = json.loads(sys.argv[1])
except Exception:
    part_map = {}
rows = []
seen = set()
refused = False
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    try:
        r = json.loads(line)
        # _refused sentinel: the inner script detected that bdjson produced no output,
        # meaning bd refused rather than returning an empty list.
        if r.get("_refused"):
            refused = True
            continue
        bid = r.get("id", "")
        if bid and bid not in seen:
            seen.add(bid)
            rows.append(r)
    except Exception:
        pass
# P0 across all partitions, then P1, then P2 — harness order preserved within each priority.
rows.sort(key=lambda r: (r.get("priority") or 9))
# FIVE, AND THE RENDERER AGREES. Matches health.sh MAX_NEXT_ROWS, which is now a HEIGHT
# rather than a ceiling: NEXT is a glance at the head of the queue and `bd ready` is the
# list. A collector cap TIGHTER than the renderer is the one that cannot be seen -- rows
# never emitted look exactly like rows that do not exist -- so the two move together.
for n, i in enumerate(rows[:5]):
    labels = set(i.get("labels") or [])
    pref = {x.split(":", 1)[1] for x in labels if x.startswith("fayth:")}
    if pref:
        # A fayth: preference narrows who can claim this bead. Find the preferred persona
        # whose partition labels are all present on the bead; "unclaimable" if none qualify.
        actual = None
        for lset_str, pname in part_map.items():
            if pname not in pref:
                continue
            if all(l in labels for l in lset_str.split(",")):
                actual = pname
                break
        part = actual if actual else "unclaimable"
    else:
        part = i.get("_partition", "?")
    title = re.sub(r"[^ A-Za-z0-9._/:,()#+-]", " ", (i.get("title") or ""))[:80].replace("=", "-")
    print("SP_NEXT%d=P%s %s %s %s" % (n, i.get("priority") or "?", part, i["id"], title))
# A refused query renders ? — "no work ready" is the reassuring answer that must never be
# the one a broken probe produces (law-failed-probe-renders-question).
if refused:
    print("SP_NEXT_N=?")
    print("SP_READY=?")
else:
    print("SP_NEXT_N=%d" % len(rows))
    print("SP_READY=%d" % len(rows))
' "$_PART_MAP" 2>/dev/null

    # ---- RECENT: TRANSITIONS, not just outcomes ----------------------------------------
    # The id->title map for the rows below, fetched once through the harness chokepoint and
    # passed as a file. An unreadable map leaves the rows bare rather than failing the pass:
    # "what happened" is the load-bearing half and must survive a database that will not
    # answer (law-absence-needs-a-positive-control applies to the TITLE, not to the event).
    #
    # AND WHETHER IT WAS READ AT ALL IS A SEPARATE FACT FROM WHAT IS IN IT. `bdjson` is a
    # pipeline ending in `sed`, so its exit status is sed's and a refusing bd still returns
    # zero; what a refusal actually leaves is an EMPTY file, where an empty store leaves the
    # two bytes `[]`. RECENT can treat the two alike — it loses titles either way and the
    # event is the load-bearing half — but INFLOW cannot: an empty array renders "0 new in
    # the last hour", which is the reassuring reading a broken probe must never produce
    # (law-failed-probe-renders-question). So the distinction is captured here, once, and
    # each reader below decides what to do with it.
    TITLEMAP="$(mktemp)"; trap 'rm -f "$TITLEMAP"' RETURN
    local _store_read=1
    bdjson list --all --limit 0 > "$TITLEMAP" 2>/dev/null || _store_read=0
    [ -s "$TITLEMAP" ] || { _store_read=0; echo '[]' > "$TITLEMAP"; }
    # This listed sentinel ACTs alone — landed, reopened, poisoned, reaped — which are all
    # ENDINGS. A bead being CLAIMED was invisible, and so was a bead being DROPPED: an aeon
    # exited mid-CI believing something would resume it, the lease expired, and the pane
    # showed a 52-minute-old landing while 21 commits sat abandoned. Nothing on screen said
    # a thing had changed hands. Merging the aeon ledger in makes this a lifecycle, so a
    # switch explains itself instead of having to be inferred from a clock.
    {
        grep -E 'ACT (landed|reopened|poisoned|reclaimed [0-9]|announced|reaped)' \
             "$SPIRA_RUN/sentinel.log" 2>/dev/null | tail -80 \
          | sed -E 's/^([^ ]+) spira: ACT /\1 sentinel /'
        # strand.sh's output carries NO timestamp of its own — it is printed inside a pass.
        # Stamping it with now() made a half-hour-old reclaim read "0s ago", which is the
        # dashboard lying about the single event that mattered. Attribute each line to the
        # most recent timestamped line above it, as the sending's counters already are.
        python3 -c '
import sys, re
ts, out = None, []
for line in open(sys.argv[1], errors="replace"):
    m = re.match(r"(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ) ", line)
    if m:
        ts = m.group(1)
        continue
    if line.startswith("RECLAIMED ") and ts:
        parts = line.split()
        if len(parts) > 1:
            out.append("%s sentinel reclaimed %s" % (ts, parts[1]))
print("\n".join(out[-40:]))
' "$SPIRA_RUN/sentinel.log" 2>/dev/null
        # THE BEAD IS FIELD 4, NOT 3. A ledger line is `<ts> <verb> <fayth> <bead>`, so `$3`
        # is "builder" and the predicate `$3 ~ /^sp-/` was never once true — claims have been
        # silently absent from RECENT for the whole life of this section, which is exactly the
        # blindness it was written to remove. The operator, watching an aeon finish a bead and
        # take another: "i don't see anything from the last hour in RECENT."
        #
        # AND ENDINGS TOO. Only `awake` was read, so a turn that ENDED left no trace unless
        # the sentinel also acted — and an aeon that finishes without landing (the common case
        # while the gate is advisory) produced nothing at all. `done` carries the outcome in
        # its status field; a turn that ended with the bead still in progress is the shape
        # worth seeing, because it is the one that repeats.
        awk '$2 == "awake" && $4 ~ /^sp-/ { printf "%s %s claimed %s\n", $1, $3, $4 }
             $2 == "done"  && $4 ~ /^sp-/ {
                 st = "ended"
                 for (i = 5; i <= NF; i++) if ($i ~ /^status=/) { sub(/^status=/, "", $i); st = $i }
                 printf "%s %s %s %s\n", $1, $3, st == "closed" ? "finished" : st, $4
             }' \
            "$SPIRA_RUN/aeon-ledger.log" 2>/dev/null | tail -80
    # EVERY STAGE OF THIS PIPELINE IS A CAP AND THE SMALLEST ONE DECIDES. Widening only the
    # last would still emit four events, because each source is trimmed before the merge —
    # which is why raising the renderer's ceiling to forty meant raising every `tail` above
    # this line too, not just the `head` on it.
    # -u BECAUSE THE SOURCES OVERLAP AND THE LEDGER REPEATS ITSELF. slay.sh can write two
    # `done` lines for one aeon, and a bead that is both claimed and ended in the window
    # arrives from two branches of the merge — so the pane showed the same event twice and
    # spent two of twenty rows saying it once. The dedupe is also why the sources are trimmed
    # LOOSER than the output: forty unique events can need well over forty input lines.
    } | sort -r -u | head -60 | python3 -c '
import sys, json, re, datetime
now = datetime.datetime.now(datetime.timezone.utc)

# THE TITLE MAP IS BUILT BY THE SHELL, through bdjson, and handed here as a file. Calling bd
# from inside this python meant reinventing how the harness invokes it — the binary, -C, the
# PATH conf.sh exports — and every one of those is a way to get an empty map that renders as
# "no titles" rather than as an error. bdjson is the one chokepoint that already knows.
#
# ONE list call for all of them, never one show per event: twenty shows is twenty round trips
# to Dolt on a pass that runs every minute. --limit 0 because bd list truncates at 50 by
# default, and a silently short map leaves later rows bare (law-bd-list-truncates-at-50).
titles = {}
try:
    for i in json.load(open(sys.argv[1])):
        titles[i["id"]] = re.sub(r"[^ A-Za-z0-9._/:,()#+-]", " ", (i.get("title") or ""))
except Exception:
    titles = {}


# DEDUPED ON THE EVENT, NOT THE LINE. `sort -u` above collapses byte-identical rows, which
# is not what repeats here: slay.sh writes two `done` lines a SECOND apart, so the timestamps
# differ, both survive, and the pane spends two of twenty rows saying one thing once. Keyed on
# (actor, verb, bead) and keeping the first seen — input is newest-first, so that is the newest.
seen, rows = set(), []
for line in sys.stdin:
    parts = line.strip().split(" ", 2)
    if len(parts) < 3:
        continue
    ts_str, actor, body = parts[0], parts[1], parts[2]
    toks = body.split()
    bead = toks[-1] if toks else ""
    if "/" in bead:
        bead = bead.rsplit("/", 1)[-1]
    verb = toks[0] if toks else ""
    key = (actor, verb, bead)
    if key in seen:
        continue
    seen.add(key)
    rows.append((ts_str, actor, body, verb, bead))

# FORTY ROWS, AND THE CAP IS APPLIED HERE RATHER THAN ON THE PIPE ABOVE. The head above is
# the MERGE WINDOW and has to stay wider than the cap, because the dedup that follows it
# removes rows: trimming to forty before it would emit thirty-one events and call that the cap.
# Five matches MAX_RECENT_ROWS in health.sh, which is now a HEIGHT rather than a ceiling.
# The tails above stay wide on purpose: they are the MERGE WINDOW the dedup runs over, and
# trimming them to five would emit three events and call that the cap.
for n, (ts_str, actor, body, verb, bead) in enumerate(rows[:5]):
    try:
        t = datetime.datetime.strptime(ts_str, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=datetime.timezone.utc)
        secs = int((now - t).total_seconds())
    except Exception:
        continue
    # NO "ago" SUFFIX. Every value in this column is an age and the word said so on every
    # row, spending three columns per row on a fact the column header already carries.
    if secs < 90: rel = "%ds" % secs
    elif secs < 5400: rel = "%dm" % (secs // 60)
    elif secs < 172800: rel = "%dh" % (secs // 3600)
    else: rel = "%dd" % (secs // 86400)

    # ONE PERSONA COLUMN, AND IT IS THE ACTOR. This row used to carry the persona twice: the
    # actor as `builder/aeon-shiva`, and a partition column derived from the bead labels
    # which, for every row an aeon wrote, resolves to that same fayth. Worse, the actor was
    # cut at twelve columns, so what survived was `builder/aeon` — the redundant half kept
    # and the one distinguishing part, the aeon name, thrown away. The operator, reading it:
    # "the persona is listed twice". So the assignee suffix and the partition column are both
    # gone and the fayth name stands alone. Non-aeon actors (sentinel, overseer, ryan) print
    # as themselves, which is what they always did.
    actor_disp = actor

    # The last word of an event is its bead id; look the title up and append it. A missing
    # title is left absent rather than filled with a placeholder, so the row still says what
    # happened when the map could not be built.
    #
    # AND THE LAST WORD IS SOMETIMES A REF, NOT AN ID. The sentinel logs a landing as
    # `landed spira/sp-mebw`, which is the BRANCH. Looked up whole it matches nothing, so every
    # landed row on the pane rendered with no title at all -- the one event class where the
    # reader most wants to know what landed. Fall back to the ref basename, which is the bead.
    toks = body.split()
    # A LANDING NAMES THE BEAD; A REAP NAMES THE BRANCH. They are different questions. What
    # landed is work, and the branch it arrived on is an implementation detail the reader
    # already knows -- so `spira/` is noise on that row. What was reaped is a REF, in one of
    # seven repositories, and the whole point of the row is which one; there the bare id is
    # the redundant half, because the branch already ends with it.
    if verb == "landed" and toks and "/" in toks[-1]:
        toks[-1] = bead
        body = " ".join(toks)
    elif len(toks) > 2 and toks[-1] != bead and toks[-1].endswith("/" + bead):
        body = " ".join(toks[:-1])
    title = titles.get(bead, "")
    body_display = ("%s %s" % (body, title) if title else body)[:80].replace("=", "-")
    # FIELDS: age, persona, then the body (verb, bead, title) as one unit. The renderer
    # splits the body and prints the verb BEFORE the persona — time, state, persona, bead,
    # description — because the state is what a reader scans this section for.
    print("SP_EVENT%d=%-4s %-8s %s" % (n, rel, actor_disp[:8], body_display))
' "$TITLEMAP"

    # ---- INFLOW: what is being CUT, and the rate it is arriving at ----------------------
    # EVERY OTHER SECTION ON THIS PANE READS THE QUEUE FROM THE CLAIMING END. NOW says what
    # is held, NEXT what is ready, RECENT what moved -- and none of them answers where the
    # work came from. A growing store reads as a busy harness either way, and the two cases
    # are opposite: beads arriving because a design was decomposed is progress, and beads
    # arriving because one broken thing keeps reporting itself is a defect wearing throughput's
    # clothes. The operator, reading an hour of it: "11 of the 12 beads right now are from the
    # timed test run."
    #
    # THE SAME FILE AS THE TITLE MAP, DELIBERATELY. $TITLEMAP is already `bd list --all
    # --limit 0` -- the whole store, unfiltered, closed rows included. A second query here
    # would be a second round trip to Dolt for rows already in hand, and one more way for two
    # readings of the same store to disagree about what is in it.
    #
    # UNFILTERED BY LABEL, WHICH IS THE WHOLE POINT. The throughput probe above reads the
    # `plan` scope; an Ops incident and an operator ask carry no `plan` label and are exactly
    # the inflow worth seeing. `event` beads ARE excluded -- they are bd's own state-change
    # records, 194 of 249 rows on this store, and nobody cut them.
    #
    # THE RATE AND THE ROWS ARE TWO DIFFERENT WINDOWS, on purpose. The count is what arrived
    # inside the window; the rows are the newest beads whenever they arrived. On a quiet hour
    # the rate is 0 and the rows still say what last landed in the store and how long ago, so
    # the section neither goes blank nor implies a burst that is not happening.
    if [ "$_store_read" != 1 ] || ! python3 /dev/fd/3 "$TITLEMAP" "${SPIRA_INFLOW_WINDOW_MIN:-60}" 3<<'PY' 2>/dev/null
import sys, json, re, datetime

try:
    rows = json.load(open(sys.argv[1]))
except Exception:
    raise SystemExit(1)
if not isinstance(rows, list):
    raise SystemExit(1)
try:
    win = int(sys.argv[2])
except Exception:
    win = 60
if win <= 0:
    win = 60

now = datetime.datetime.now(datetime.timezone.utc)

def when(v):
    try: return datetime.datetime.fromisoformat(str(v).replace("Z", "+00:00"))
    except Exception: return None

def rel(secs):
    if secs < 90: return "%ds" % secs
    if secs < 5400: return "%dm" % (secs // 60)
    if secs < 172800: return "%dh" % (secs // 3600)
    return "%dd" % (secs // 86400)

# A ROW WITH NO READABLE created_at IS DROPPED, not dated to now. Sorting it to the top would
# put an unreadable timestamp at the head of a section whose entire claim is "these are the
# newest" (law-absence-needs-a-positive-control).
aged = []
for i in rows:
    kind = (i.get("issue_type") or "task")
    if kind == "event":
        continue
    t = when(i.get("created_at"))
    if t is None:
        continue
    aged.append((t, kind, i))
aged.sort(key=lambda r: r[0], reverse=True)

cut = now - datetime.timedelta(minutes=win)
fresh = [r for r in aged if r[0] >= cut]

# THE KINDS ARE THE HALF THAT DISCRIMINATES. Twelve new beads in an hour is a decomposed
# design or a loop and the count alone cannot tell those apart; "bug 11" can. DEFECT counts
# the same population a second way so the renderer can colour without re-parsing the string.
kinds, defect = {}, 0
for t, kind, i in fresh:
    kinds[kind] = kinds.get(kind, 0) + 1
    if kind == "bug" or "incident" in (i.get("labels") or []):
        defect += 1

print("SP_INFLOW_WIN=%d" % win)
print("SP_INFLOW_N=%d" % len(fresh))
print("SP_INFLOW_DEFECT=%d" % defect)
print("SP_INFLOW_KINDS=%s" % (", ".join("%s %d" % (k, v)
      for k, v in sorted(kinds.items(), key=lambda x: -x[1])[:4]) or "-"))

# FORTY, AND THE RENDERER AGREES -- MAX_INFLOW_ROWS in health.sh. This is the one section
# left on the column that is a CEILING rather than a height: it is where the slack goes when
# no aeon is awake, and how much of it renders is decided per repaint by `share`.
for n, (t, kind, i) in enumerate(aged[:40]):
    title = re.sub(r"[^ A-Za-z0-9._/:,()#+-]", " ", (i.get("title") or ""))[:80].replace("=", "-")
    k = re.sub(r"[^a-z]", "", kind.lower())[:8] or "task"
    pri = i.get("priority")
    print("SP_INFLOW%d=%-4s %-8s P%s %s %s" % (
        n, rel(int((now - t).total_seconds())), k,
        pri if pri is not None else "?", i["id"], title))
PY
    then
        # A FAILED PROBE RENDERS `?`, NEVER 0. "nothing was cut in the last hour" is the
        # reassuring reading, and it is exactly what an unreadable store would otherwise
        # produce (law-failed-probe-renders-question).
        echo "SP_INFLOW_WIN=?"; echo "SP_INFLOW_N=?"
        echo "SP_INFLOW_DEFECT=?"; echo "SP_INFLOW_KINDS=?"
    fi

    # ---- AWAITING CI: open gh:run gates waiting on a CI run ---------------------------
    # A gated bead is not ready — bd gate blocks it at the query level, so no reader has to
    # remember to exclude a label. Without a row of its own, work gated for an hour looks
    # exactly like work nobody started.
    #
    # GATES ARE CLASSIFIED BY AGE ALONE. A gh:run gate only exists for pr-mode repos (the
    # aeon brief refuses to create one for push or hold), so the no-ci verdict of the former
    # label reader is not needed. An unreadable created_at counts as stuck — a gate that
    # cannot be aged surfaces for inspection rather than being silently painted as healthy.
    #
    # OLDEST FIRST. The Python pass sorts and renders age; the bash pass reads that order.
    # The first row is always the oldest gate; no second sort is needed here.
    #
    # A FAILED PROBE RENDERS `?`, NEVER 0. An empty gate list "[]" exits 0 with no rows,
    # so if-true fires and SP_AWAITING_N=0 is correct. A broken query (bd fails, Python
    # raises on empty stdin) exits non-zero, so if-false fires and emits ?.
    local ci_rows ci_id ci_at ci_rel ci_title ci_n=0
    local ci_watch=0 ci_stuck=0 ci_oldest=- ci_age=- ci_stuck_id=-
    local ci_max ci_now ci_t
    ci_max="${SPIRA_CI_PARK_MAX:-5400}"
    case "$ci_max" in ''|*[!0-9]*) ci_max=5400 ;; esac
    ci_now="$(date -u +%s)"
    # bdq, NOT bdjson: bd gate list returns `null` (not `[]`) when no gates exist, and
    # json_only strips `null` because it does not start with `[` or `{`. With bdjson the
    # Python would receive empty stdin, raise on json.load, and exit non-zero — emitting `?`
    # for a healthy empty-queue state. bdq passes the raw output through; the Python handles
    # both null and [] explicitly (law-absence-needs-a-positive-control).
    if ci_rows="$(bdq gate list --json 2>/dev/null | python3 -c '
import sys, json, datetime, re
raw = sys.stdin.read()
try:
    d = json.loads(raw) if raw.strip() else []
    if d is None: d = []
except Exception: raise SystemExit
rows = [i for i in (d if isinstance(d, list) else [d])
        if i.get("status") != "closed" and i.get("await_type") == "gh:run"]
def when(v):
    try: return datetime.datetime.fromisoformat(str(v).replace("Z", "+00:00"))
    except Exception: return None
def rel(secs):
    if secs < 90: return "%ds" % secs
    if secs < 5400: return "%dm" % (secs // 60)
    if secs < 172800: return "%dh" % (secs // 3600)
    return "%dd" % (secs // 86400)
now = datetime.datetime.now(datetime.timezone.utc)
aged = sorted(((when(i.get("created_at")) or now, i) for i in rows), key=lambda r: r[0])
for t, i in aged[:20]:
    # The description is sanitised and the fields are tab-separated, so a description
    # carrying a tab or control character cannot shift the columns the reader below splits on.
    desc = re.sub(r"[^ A-Za-z0-9._/:,()#+-]", " ", (i.get("description") or ""))[:80]
    print("%s\t%s\t%s\t%s" % (i["id"], i.get("created_at") or "",
                               rel(int((now - t).total_seconds())), desc))' 2>/dev/null)"
    then
        while IFS="$(printf '\t')" read -r ci_id ci_at ci_rel ci_title; do
            [ -n "$ci_id" ] || continue
            # OLDEST FIRST — the Python pass emits rows oldest-first, so ci_oldest is set
            # on the first iteration and never overwritten.
            [ "$ci_oldest" = - ] && { ci_oldest="$ci_id"; ci_age="$ci_rel"; }
            # CLASSIFY BY AGE ALONE. An unreadable created_at counts as stuck.
            ci_t="$(date -u -d "$ci_at" +%s 2>/dev/null)" || ci_t=""
            if [ -n "$ci_t" ] && [ "$ci_max" -gt 0 ] && \
                    [ "$(( ci_now - ci_t ))" -le "$ci_max" ]; then
                ci_watch=$(( ci_watch + 1 ))
            else
                ci_stuck=$(( ci_stuck + 1 ))
                [ "$ci_stuck_id" = - ] && ci_stuck_id="$ci_id"
                # THE ROW CARRIES ITS OWN REASON. A count in the summary says how many gates
                # are overdue; only the row says which gate, and a reader looking at one gate
                # should not have to work out which population it fell into.
                ci_title="gate overdue · $ci_title"
            fi
            # ONE KEY PER GATED BEAD, so the CI section has something to expand INTO. The
            # summary line answers "is anything gated"; only the row answers "which of them
            # has been waiting since yesterday".
            printf 'SP_AWAITING%d=%-10s %-4s %s\n' "$ci_n" "$ci_id" "$ci_rel" "$ci_title"
            ci_n=$(( ci_n + 1 ))
        done <<< "$ci_rows"
        echo "SP_AWAITING_N=$ci_watch"
        echo "SP_AWAITING_STUCK=$ci_stuck"
        echo "SP_AWAITING_STUCK_ID=$ci_stuck_id"
        echo "SP_AWAITING_OLDEST=$ci_oldest"
        echo "SP_AWAITING_AGE=$ci_age"
    else
        # A FAILED PROBE RENDERS `?`, NEVER 0. The first version of this pane returned 0 from
        # its exception handler, so a broken query displayed as "no gated beads".
        echo "SP_AWAITING_N=?"; echo "SP_AWAITING_STUCK=?"; echo "SP_AWAITING_STUCK_ID=?"
        echo "SP_AWAITING_OLDEST=?"; echo "SP_AWAITING_AGE=?"
    fi

    # ---- UNANSWERED: operator threads awaiting a reply --------------------------------------
    # unanswered.sh makes one bd list call then one bd comments call per matching bead, so
    # it belongs in the slow tier rather than the medium-tier counts probe.
    local unread
    unread=$("$COCK_DIR/unanswered.sh" --count 2>/dev/null | tail -1)
    echo "SP_UNANSWERED=${unread:-?}"

    # ---- THROUGHPUT: what closed and what opened, by kind, plus sparklines ---------------
    # ONE bd list call for both the summary counts AND the sparkline timestamps.
    # DO NOT ADD COLUMNS TO cockpit-history.csv for these: append_history would rotate the
    # file on any HIST_COLS change, discarding all token history. Sparklines are bucketed
    # from timestamps on disk and need no accumulated series.
    #
    # The landing.log path is passed as argv[1] so the Python block can read it without
    # re-invoking bd. FD 3 carries the script so stdin stays free for the bdjson pipe
    # (law-commit-messages-via-stdin — same shape, different file descriptor).
    bdjson list --all --limit 0 --label "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan" 2>/dev/null | \
    python3 /dev/fd/3 "$SPIRA_RUN/landing.log" 3<<'PY' 2>/dev/null
import sys, json, datetime, re, os

try: d = json.load(sys.stdin)
except Exception: raise SystemExit
rows = d if isinstance(d, list) else [d]
cut = datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(hours=24)
cut_ts = cut.timestamp()

def when(v):
    try: return datetime.datetime.fromisoformat(str(v).replace("Z", "+00:00"))
    except Exception: return None

closed = [i for i in rows if i.get("status") == "closed" and (when(i.get("closed_at") or i.get("updated_at")) or cut) >= cut]
opened = [i for i in rows if (when(i.get("created_at")) or cut - datetime.timedelta(1)) >= cut]
kinds = {}
for i in closed: kinds[i.get("issue_type") or "task"] = kinds.get(i.get("issue_type") or "task", 0) + 1
print("SP_CLOSED_24H=%d" % len(closed))
print("SP_OPENED_24H=%d" % len(opened))
print("SP_CLOSED_KINDS=%s" % (", ".join("%s %d" % (k, v) for k, v in sorted(kinds.items(), key=lambda x: -x[1])[:4]) or "-"))

# Sparklines: bucket the 24h window into BUCKETS equal intervals and count events per bucket.
# EACH SERIES IS SCALED TO ITSELF: the question is "rising or falling" for that series alone.
# ZERO IS A REAL MEASUREMENT: an interval with no events renders ▁, not dropped. Only an
# unreadable source is absent — that distinction is what law-absence-needs-a-positive-control
# requires. Mirroring spark()'s all-equal rule: all-zero → ▁ flat; all-equal non-zero → ▄ flat.
BUCKETS = 8
bucket_secs = 86400 / BUCKETS
blocks = "▁▂▃▄▅▆▇█"

def spark_str(bkts):
    lo, hi = min(bkts), max(bkts)
    if hi == lo:
        return (blocks[0] if hi == 0 else blocks[3]) * len(bkts)
    return "".join(blocks[min(7, int((v - lo) / (hi - lo) * 7.999))] for v in bkts)

opened_bkts = [0] * BUCKETS
for i in rows:
    t = when(i.get("created_at"))
    if t:
        ts = t.timestamp()
        if ts >= cut_ts:
            opened_bkts[min(BUCKETS - 1, int((ts - cut_ts) / bucket_secs))] += 1

closed_bkts = [0] * BUCKETS
for i in rows:
    if i.get("status") == "closed":
        t = when(i.get("closed_at") or i.get("updated_at"))
        if t:
            ts = t.timestamp()
            if ts >= cut_ts:
                closed_bkts[min(BUCKETS - 1, int((ts - cut_ts) / bucket_secs))] += 1

print("SP_BEADS_SPARK_OPENED=%s" % spark_str(opened_bkts))
print("SP_BEADS_SPARK_CLOSED=%s" % spark_str(closed_bkts))

# Landed sparkline: parse landing.log for "spira: landed" lines within the window.
# A missing or unreadable log renders ?, never 0: the reassuring reading must not be the
# one a broken probe produces (law-absence-needs-a-positive-control).
landing_log = sys.argv[1] if len(sys.argv) > 1 else ""
landed_bkts = [0] * BUCKETS
landed_24h = 0
landed_ok = False
if landing_log and os.path.exists(landing_log):
    landed_ok = True
    try:
        with open(landing_log, errors="replace") as f:
            for line in f:
                if "spira: landed" not in line:
                    continue
                m = re.match(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z)", line)
                if not m:
                    continue
                try:
                    ts = datetime.datetime.strptime(
                        m.group(1), "%Y-%m-%dT%H:%M:%SZ"
                    ).replace(tzinfo=datetime.timezone.utc).timestamp()
                    if ts >= cut_ts:
                        landed_24h += 1
                        landed_bkts[min(BUCKETS - 1, int((ts - cut_ts) / bucket_secs))] += 1
                except Exception:
                    pass
    except Exception:
        landed_ok = False

if landed_ok:
    print("SP_BEADS_SPARK_LANDED=%s" % spark_str(landed_bkts))
    print("SP_BEADS_LANDED_24H=%d" % landed_24h)
else:
    print("SP_BEADS_SPARK_LANDED=?")
    print("SP_BEADS_LANDED_24H=?")
PY


    # ---- TOKENS: what the account is spending, and which half is spending it ------------
    # The rate limit is the binding constraint on everything else on this pane — when the
    # account is out of capacity no aeon can be summoned, no bead can move, and every other
    # figure here is frozen for reasons nothing else reports. It was also unattributed: the
    # harness and the interactive session were both plausible culprits and optimising the
    # wrong one is the expensive mistake.
    #
    # `tokens.sh env` READS ONLY FILES TOUCHED INSIDE THE WINDOW, which is why it can run on
    # every pass at all. The full corpus is billions of tokens of history and re-reading it
    # here would make the instrument cost more than the thing it measures. Do not "simplify"
    # that away by calling `report`.
    "$HERE/tokens.sh" env 2>/dev/null \
      || { for k in SP_TOK_WINDOW_H SP_TOK_AEON_WIN SP_TOK_ARC_WIN SP_TOK_SESS_WIN SP_TOK_WIN \
                    SP_TOK_AEON_TURNS SP_TOK_ARC_TURNS SP_TOK_SESS_TURNS \
                    SP_TOK_AEON_CTX SP_TOK_ARC_CTX SP_TOK_SESS_CTX \
                    SP_TOK_AEON_OUT SP_TOK_ARC_OUT SP_TOK_SESS_OUT \
                    SP_TOK_AEON_RECENT SP_TOK_ARC_RECENT SP_TOK_SESS_RECENT; do
               echo "$k=?"
           done; }

    # ---- LIVE CONTEXT: how close the session in front of the operator is to the edge -----
    # A total says what was spent; only the proximity says whether to act now, and acting is
    # what the operator can actually do about it. Measured by ctx-meter.sh — the same program
    # the status line calls, deliberately, so the pane and the status line cannot disagree
    # about how close to a threshold a session is.
    "$HERE/ctx-meter.sh" env 2>/dev/null \
      || { for k in SP_CTX_NOW SP_CTX_TURNS SP_CTX_GROWTH SP_CTX_NEXT SP_CTX_HEADROOM \
                    SP_CTX_TURNS_LEFT SP_CTX_AGE SP_CTX_ARCHIVIST SP_CTX_ARCHIVIST_BEHIND \
                    SP_CTX_SCAN_BYTES SP_CTX_ARCHIVIST_FILED \
                    SP_LIMIT_5H_PCT SP_LIMIT_5H_ETA SP_LIMIT_7D_PCT SP_LIMIT_7D_ETA \
                    SP_LIMIT_AGE; do
               echo "$k=?"
           done; }

    # ---- the four numbers this build got wrong -----------------------------------------
    SPIRA_SELF_WINDOW="${SPIRA_SELF_WINDOW:-60}" \
    python3 "$HERE/cockpit-metrics.py" \
        "$SPIRA_RUN/sentinel.log" "$SPIRA_RUN/aeon-ledger.log" "$WINDOW_HOURS" 2>/dev/null \
      || { for k in SP_PASSES SP_ACTS SP_FALSE_ACTS SP_FALSE_PER_PASS SP_SINCE_JUDGEMENT \
                    SP_AEON_BORN SP_AEON_LIVED SP_AEON_STILLBORN SP_AEON_WORKED SP_AEON_THRASH \
                    SP_SELF_REPEATING_N SP_SELF_STILLBORN_W SP_SELF_STILLBORN_LAST \
                    SP_SELF_STARVED_W SP_SELF_STARVED_LAST; do
               echo "$k=?"
           done; }
}

# Slow-tier keys: git graph walks, landing checks, unbounded queries.
# Called as `cockpit.sh unsent` by collect.sh; also called by probe().
unsent_keys() {
    # ---- the unsent backlog ------------------------------------------------------------
    # Current state, not log history: how many spira/* branches exist right now and how old
    # the oldest is. A branch that keeps ageing is work that landed nowhere, which is how a
    # fiend starts — before sending.sh, one such branch was re-merged and re-pushed every
    # two minutes forever because git would not delete it while a worktree held it.
    # A failed probe renders `?`, never 0: "no unsent work" is the reassuring answer and
    # must never be the one a broken git call produces.
    # ACROSS EVERY REPOSITORY. A bead names the checkout it is worked in, so counting only
    # the home repo's branches would report "no unsent work" while another repository's
    # branches aged forever — the reassuring answer, produced by looking in the wrong place.
    # Refs are a local read, so this costs nothing per repository; no fetch happens here.
    _fail=0; _n=0; _o=""; _done=0; _unadopted=0; _orphan_work=0; _unadopted_names=""
    for _r in $(spira_repos); do
        _p="$(repo_root "$_r")" || continue
        [ -e "$_p/.git" ] || continue
        # Resolve the base ref once per repo. spira_landref reads the repo-map's declared base
        # column first, which is the only authoritative source for repos whose remote is not
        # named `origin` (e.g. a repo with a `gitea` remote). The result is used below to
        # distinguish orphan work from unadopted strays.
        _base="$(spira_landref "$_r" 2>/dev/null)" || _base=""
        # ONE LOOP PER REPOSITORY, fetching both name and timestamp. The bead lookup decides
        # whether a branch counts as unsent work, a true unadopted stray, or orphan work.
        # A ref with no bead may be: (a) a disposable stray whose commits are already on the
        # base — safe to delete, counted as SP_UNADOPTED; or (b) a branch that carries commits
        # absent from the base — unlanded work awaiting human attention, counted as
        # SP_ORPHAN_WORK and never filed as a reapable stray. Conflating the two produced a
        # recurring unadopted-refs incident every 30 minutes for a hold repo whose branch had
        # unlanded commits and could not be deleted by any rite. (sp-doh5)
        if _brs="$(git -C "$_p" for-each-ref --format='%(refname:short) %(committerdate:unix)' 'refs/heads/spira/*' 2>/dev/null)"; then
            while read -r _b _ts; do
                [ -n "$_b" ] || continue
                # BD_TIMEOUT, NOT `timeout 2`. bdjson is a SHELL FUNCTION, and timeout is an
                # external binary that cannot execute one: `timeout 2 bdjson ...` died with
                # "timeout: failed to execute process: No such file or directory" and rc=127
                # on EVERY branch, with the error swallowed by 2>/dev/null. An empty status
                # is the unadopted branch of the test below, so every branch in every
                # repository was counted as a stray: SP_UNSENT and SP_BRANCH_DONE read 0 —
                # the reassuring answer — and SP_UNADOPTED read the total. bdq already wraps
                # bd in `timeout "${BD_TIMEOUT:-180}"`, so BD_TIMEOUT is the seam that gives
                # this loop the short bound it wanted, through the function rather than
                # around it.
                _st="$(BD_TIMEOUT=2 bdjson show "${_b#spira/}" 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin); print((d if isinstance(d, list) else [d])[0].get("status", ""))
except Exception: print("")' 2>/dev/null)"
                if [ -z "$_st" ]; then
                    # No bead for this branch. Only count as an unadopted stray (safe to delete)
                    # when the tip is confirmed to be already on the base branch. If the base is
                    # unknown or the branch has commits absent from base, count as orphan work —
                    # it is safer to over-report work than to misclassify unlanded commits as strays.
                    if [ -n "$_base" ] && git -C "$_p" merge-base --is-ancestor "$_b" "$_base" 2>/dev/null; then
                        _unadopted=$((_unadopted+1))
                        _unadopted_names="${_unadopted_names:+$_unadopted_names }${_b#spira/}"
                    else
                        _orphan_work=$((_orphan_work+1))
                    fi
                    continue
                fi
                [ "$_st" = closed ] && _done=$((_done+1))
                _n=$((_n+1))
                if [ -n "$_ts" ]; then
                    if [ -z "$_o" ] || [ "$_ts" -lt "$_o" ]; then _o="$_ts"; fi
                fi
            done <<< "$_brs"
        else
            _fail=1
        fi
    done
    echo "SP_BRANCH_DONE=$_done"
    echo "SP_UNADOPTED=$_unadopted"
    echo "SP_UNADOPTED_NAMES='$_unadopted_names'"
    echo "SP_ORPHAN_WORK=$_orphan_work"
    if [ "$_fail" = 1 ]; then
        echo "SP_UNSENT=?"
        echo "SP_UNSENT_OLDEST_H=?"
    else
        echo "SP_UNSENT=$_n"
        if [ "$_n" -gt 0 ]; then
            echo "SP_UNSENT_OLDEST_H=$(( ( $(date +%s) - $_o ) / 3600 ))"
        else
            echo "SP_UNSENT_OLDEST_H=0"
        fi
    fi

    # ---- what the landing gate is worth ------------------------------------------------
    # THE PANE ALREADY SAYS WHAT THE HARNESS COSTS; this is the one number that says whether
    # the check between work and its landings is buying anything. The metric selection rule
    # is the operator's — instrument the numbers that surprised us — and a gate that ran for
    # weeks, cost seventeen minutes a branch and caught nothing is the largest such surprise
    # this system has produced (law-gate-earns-its-place).
    #
    # yield.sh does the reading and renders `?` for anything it could not read; this passes
    # that through untouched, because a yield meter that reported a broken check as "no
    # faults" is the same failure as reporting a stalled queue as idle. SPIRA_RUN is passed
    # explicitly — conf.sh does not export it, and a child re-deriving it from a config file
    # would read a different directory and report a confident zero.
    # EVERY KEY IS WRITTEN OUT IN FULL rather than renamed by a pattern. The pane reads these
    # by name, and the way a pair like this drifts is that one side is edited and the other
    # is not — which is findable by grep only if both sides spell the key.
    _y_reds="?"; _y_def="?"; _y_fault="?"; _y_unk="?"; _y_solo="?"; _y_conc="?"; _y_worst="?"
    if [ -r "$HERE/yield.sh" ]; then
        while IFS='=' read -r _k _v; do
            case "$_k" in
                YIELD_REDS)     _y_reds="$_v" ;;
                YIELD_DEFECT)   _y_def="$_v" ;;
                YIELD_FAULT)    _y_fault="$_v" ;;
                YIELD_UNKNOWN)  _y_unk="$_v" ;;
                YIELD_SOLO_MED) _y_solo="$_v" ;;
                YIELD_CONC_MED) _y_conc="$_v" ;;
                YIELD_TOP_FAULT) _y_worst="$_v" ;;
            esac
        done < <(SPIRA_RUN="$SPIRA_RUN" bash "$HERE/yield.sh" report 2>/dev/null)
    fi
    echo "SP_YIELD_REDS=$_y_reds"
    echo "SP_YIELD_DEFECT=$_y_def"
    echo "SP_YIELD_FAULT=$_y_fault"
    echo "SP_YIELD_UNKNOWN=$_y_unk"
    echo "SP_YIELD_SOLO_MED=$_y_solo"
    echo "SP_YIELD_CONC_MED=$_y_conc"
    echo "SP_YIELD_TOP_FAULT=$_y_worst"

    # law-closed-is-not-landed as a 24h figure matching the header it sits under. The
    # population is beads an aeon worked AND closed within the last 24 hours, so the row's
    # window agrees with the throughput row above it. Previously this was all-time, which
    # produced nonsense when compared against the 24h closed/opened counts on the same block.
    #
    # A `<id>.log` in the run directory is the evidence that a session ran. The 24h filter
    # uses the bead's closed_at timestamp.
    #
    # THREE STATES, NOT TWO. A closed bead with no commit naming it is not necessarily lost:
    # if its branch still exists it is awaiting landing — a healthy, normally non-zero state.
    # Only a bead with no commit AND no branch is irrecoverable (law-alerts-must-be-actionable).
    #
    # ONE `git log` plus ONE `for-each-ref` per repo, then membership tests per id.
    local closed_ids subjects branches
    # THE BEAD'S REPOSITORY COMES OUT OF THE SAME QUERY AS ITS ID, because reading the wrong
    # repository's commit graph is wrong confidently in both directions: another repository's bead
    # that landed perfectly reads as unlanded in brain, and a panel that reports finished
    # work as lost is the same false alert as one that reports lost work as finished.
    # TAB-SEPARATED: id, repo, priority, closed_at, title. The extra fields feed the PEND
    # section below without a second walk of the database. The first two fields serve the
    # existing landing check; the rest serve the unlanded-queue detail rows.
    #
    # bdjson is a pipeline ending in sed (json_only), so it exits 0 even when bd refuses and
    # produces nothing. Call bdq directly and gate on emptiness before parsing: an empty
    # response means the probe refused, which must render ? not 0 (law-detection-outranks-rejection).
    local _closed_raw _closed_read=1
    _closed_raw="$(bdq list --status closed --limit 0 --label "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan" --json 2>/dev/null)"
    [ -n "$_closed_raw" ] || _closed_read=0
    if [ "$_closed_read" = 0 ]; then
        echo "SP_CLOSED=?"; echo "SP_LANDED=?"; echo "SP_AWAITING_LAND=?"; echo "SP_UNLANDED=?"
        echo "SP_PEND_N=?"; echo "SP_PEND_OLDEST=?"
    else
    closed_pairs="$(printf '%s\n' "$_closed_raw" | json_only | python3 -c '
import sys, json, os, re, datetime
run, home = sys.argv[1], sys.argv[2]
try: d = json.load(sys.stdin)
except Exception: raise SystemExit
cut = datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(hours=24)
def when(v):
    try: return datetime.datetime.fromisoformat(str(v).replace("Z", "+00:00"))
    except Exception: return None
for i in (d if isinstance(d, list) else [d]):
    if os.path.exists(os.path.join(run, i["id"] + ".log")):
        ts = when(i.get("closed_at") or i.get("updated_at"))
        if ts and ts >= cut:
            repo = next((l[5:] for l in (i.get("labels") or []) if l.startswith("repo:")), home)
            pri = i.get("priority", "")
            cat = i.get("closed_at") or i.get("updated_at") or ""
            title = re.sub(r"[^ A-Za-z0-9._/:,()#+-]", " ", (i.get("title") or ""))[:80]
            print("%s\t%s\t%s\t%s\t%s" % (i["id"], repo, pri, cat, title))' "$SPIRA_RUN" "$(spira_home_repo)" 2>/dev/null)"
    closed_ids="$(printf '%s\n' "$closed_pairs" | awk -F'\t' 'NF{print $1}')"
    if [ -z "$closed_ids" ]; then
        echo "SP_CLOSED=0"; echo "SP_LANDED=0"; echo "SP_AWAITING_LAND=0"; echo "SP_UNLANDED=0"
        echo "SP_PEND_N=0"; echo "SP_PEND_OLDEST=0"
    else
    # ONE FETCH PER REPOSITORY THAT ACTUALLY HAS A CLOSED BEAD IN IT, and none at all for the
    # rest. This runs on the collector loop; fetching every registered repository each pass
    # would be thousands of round trips a day to answer a question about repositories with no
    # Spira work in them.
    #
    # The remote-tracking ref, never the local branch alone: nothing in this harness advances
    # the shared checkout's default branch — the sentinel pushes the landing branch from its
    # own worktree — so the local ref is however stale the last human left it.
    #
    # AND THE REF IS RESOLVED, NOT `main`. This appended a literal `main` to the ref list,
    # which in a `master`-based repository names nothing: `git log <ref> main` fails outright on
    # an unknown revision, so BOTH repositories contributed no subjects at all and every
    # closed bead in them counted as unlanded on the panel the operator reads.
    subjects=""
    branches=""
    for _r in $(printf '%s\n' "$closed_pairs" | awk -F'\t' 'NF{print $2}' | sort -u); do
        _p="$(repo_root "$_r")" || continue
        [ -e "$_p/.git" ] || continue
        _refs="$(spira_landrefs "$_p")" || continue
        if _rem="$(ref_remote "${_refs%% *}")"; then git -C "$_p" fetch -q "$_rem" 2>/dev/null; fi
        # shellcheck disable=SC2086
        subjects="$subjects
$(git -C "$_p" log --format='%s%n%b' -n 2000 $_refs 2>/dev/null)"
        branches="$branches
$(git -C "$_p" for-each-ref --format='%(refname:short)' 'refs/heads/spira/' 'refs/remotes/*/spira/' 2>/dev/null)"
    done
    if [ -z "$subjects" ] && [ -z "$branches" ]; then
        echo "SP_CLOSED=?"; echo "SP_LANDED=?"; echo "SP_AWAITING_LAND=?"; echo "SP_UNLANDED=?"
        echo "SP_PEND_N=?"; echo "SP_PEND_OLDEST=?"
    else
        # Subjects on stdin, branches as argv[2], closed_pairs metadata as argv[3].
        # A `---` line in a commit body would split a stdin separator, and commit messages
        # do contain freeform text.
        #
        # _AWAITING lines carry the metadata the PEND section needs, sorted oldest-first by
        # closed_at so the shell loop below emits rows in age order with no second sort.
        local _land_out
        _land_out="$(printf '%s' "$subjects" | python3 -c '
import sys, re, datetime
ids = [i for i in sys.argv[1].split() if i]
br_lines = sys.argv[2].split("\n") if len(sys.argv) > 2 and sys.argv[2] else []
meta = {}
if len(sys.argv) > 3:
    for line in sys.argv[3].split("\n"):
        parts = line.strip().split("\t")
        if len(parts) >= 5:
            meta[parts[0]] = {"pri": parts[2], "cat": parts[3], "title": parts[4]}
text = sys.stdin.read()
landed_set = set(); awaiting_ids = []; never = 0
for i in ids:
    if re.search(r"(?<![\w-])%s(?![\w-])" % re.escape(i), text):
        landed_set.add(i)
    elif any(b.rstrip().endswith("/" + i) for b in br_lines if b.strip()):
        awaiting_ids.append(i)
    else:
        never += 1
def cat_key(bid):
    m = meta.get(bid, {})
    try: return datetime.datetime.fromisoformat(m.get("cat", "").replace("Z", "+00:00"))
    except Exception: return datetime.datetime.max.replace(tzinfo=datetime.timezone.utc)
awaiting_ids.sort(key=cat_key)
print("SP_CLOSED=%d"        % len(ids))
print("SP_LANDED=%d"        % len(landed_set))
print("SP_AWAITING_LAND=%d" % len(awaiting_ids))
print("SP_UNLANDED=%d"      % never)
for i in awaiting_ids:
    m = meta.get(i, {})
    print("_AWAITING=%s\t%s\t%s\t%s" % (i, m.get("pri", ""), m.get("cat", ""), m.get("title", "")))
' "$closed_ids" "$branches" "$closed_pairs" 2>/dev/null)"
        if [ -n "$_land_out" ]; then
            printf '%s\n' "$_land_out" | grep '^SP_'

            # ---- PENDING LANDING: individual beads with a branch, not yet on the base ----
            local _pend_n=0 _pend_oldest_age="" _pend_now
            _pend_now="$(date +%s)"
            # Build bead→PR map from open batch files so BATCHED entries show their PR.
            local _batched_pr_map=""
            local _bmf _bpr _bmems _bmm _bqdir
            _bqdir="${SPIRA_QUEUE_DIR:-$SPIRA_RUN/queue}"
            for _bmf in "$_bqdir"/*/open; do
                [ -f "$_bmf" ] || continue
                _bpr="$(grep '^pr=' "$_bmf" 2>/dev/null | cut -d= -f2-)" || continue
                _bmems="$(grep '^members=' "$_bmf" 2>/dev/null | cut -d= -f2-)" || continue
                [ -n "${_bpr:-}" ] || continue
                for _bmm in $_bmems; do
                    _batched_pr_map="$_batched_pr_map ${_bmm%%:*}:${_bpr}"
                done
            done
            while IFS=$'\t' read -r _ul_id _ul_pri _ul_cat _ul_title; do
                [ -n "$_ul_id" ] || continue
                local _ul_age="?"
                if [ -n "$_ul_cat" ]; then
                    local _ul_epoch
                    _ul_epoch="$(date -d "$_ul_cat" +%s 2>/dev/null)" || _ul_epoch=""
                    if [ -n "$_ul_epoch" ]; then
                        local _ul_secs=$(( _pend_now - _ul_epoch ))
                        if [ "$_ul_secs" -lt 90 ]; then _ul_age="${_ul_secs}s"
                        elif [ "$_ul_secs" -lt 5400 ]; then _ul_age="$(( _ul_secs / 60 ))m"
                        elif [ "$_ul_secs" -lt 172800 ]; then _ul_age="$(( _ul_secs / 3600 ))h"
                        else _ul_age="$(( _ul_secs / 86400 ))d"; fi
                        [ -z "$_pend_oldest_age" ] && _pend_oldest_age="$_ul_age"
                    fi
                fi
                local _ul_state="" _ul_tag=""
                { read -r _ul_state _ < "$SPIRA_RUN/landstate/$_ul_id"; } 2>/dev/null || true
                case "${_ul_state:-}" in
                    CERTIFIED) _ul_tag="[queued] " ;;
                    BATCHED)
                        local _ul_bpr="" _ul_bme
                        for _ul_bme in $_batched_pr_map; do
                            case "$_ul_bme" in "${_ul_id}:"*) _ul_bpr="${_ul_bme#*:}"; break ;; esac
                        done
                        _ul_tag="[batch${_ul_bpr:+ #${_ul_bpr}}] "
                        ;;
                    EJECTED) _ul_tag="[ejected] " ;;
                esac
                printf 'SP_PEND%d=P%s %s %s %s%s\n' "$_pend_n" "${_ul_pri:-?}" "$_ul_id" "$_ul_age" "$_ul_tag" "${_ul_title:--}"
                _pend_n=$((_pend_n + 1))
                [ "$_pend_n" -ge 20 ] && break
            done < <(printf '%s\n' "$_land_out" | sed -n 's/^_AWAITING=//p')
            echo "SP_PEND_N=$_pend_n"
            echo "SP_PEND_OLDEST=${_pend_oldest_age:-0}"
        else
            echo "SP_CLOSED=?"; echo "SP_LANDED=?"; echo "SP_AWAITING_LAND=?"; echo "SP_UNLANDED=?"
            echo "SP_PEND_N=?"; echo "SP_PEND_OLDEST=?"
        fi
    fi
    fi
    fi

    # ---- GATE: what is happening between DONE and LANDED --------------------------------
    # The operator (2026-09-07): "there's currently a lot that happens between 'DONE' and
    # 'LANDED' and the ops dashboard shows none of it."
    #
    # landing.status IS ALREADY SP_ KEY=VALUE FORM, written by the landing pass. Concatenate
    # it into the snapshot rather than recomputing any of it — it is the landing pass's own
    # word, and a collector that re-derived it would disagree exactly when it matters.
    if [ -r "$SPIRA_RUN/landing.status" ]; then
        cat "$SPIRA_RUN/landing.status"
    else
        for _k in SP_LAND_AT SP_LAND_RC SP_LAND_BRANCHES SP_LAND_MOVED; do
            echo "$_k=?"
        done
    fi

    # landing.progress — per-branch outcomes of the pass in flight, e.g.
    #   "landed spira/sp-35pl"
    #   "reopened sp-dvlq -- does not rebase onto origin/main"
    # Sanitised for the snapshot: the content is freeform prose from the sentinel, and the
    # snapshot is a KEY=value file the pane sources.
    local lp_n=0
    if [ -r "$SPIRA_RUN/landing.progress" ]; then
        while IFS= read -r _lp; do
            [ -n "$_lp" ] || continue
            _lp="$(printf '%s' "$_lp" | tr -c 'A-Za-z0-9 ._/:,()#+-' ' ' | tr -s ' ')"
            printf 'SP_LANDPROG%d=%s\n' "$lp_n" "${_lp:0:120}"
            lp_n=$((lp_n+1))
        done < "$SPIRA_RUN/landing.progress"
    fi
    echo "SP_LANDPROG_N=$lp_n"
}

# Queue-state summary keys: queue depth, open batch, quarantined suites.
queue_keys() {
    local depth=0 _f _st
    if [ -d "$SPIRA_RUN/landstate" ]; then
        for _f in "$SPIRA_RUN/landstate/"*; do
            [ -f "$_f" ] || continue
            { read -r _st _ < "$_f"; } 2>/dev/null || continue
            [ "$_st" = "CERTIFIED" ] && depth=$(( depth + 1 ))
        done
    fi
    echo "SP_QUEUE_DEPTH=$depth"

    local batch_pr=0 batch_age="0"
    local _bf _pr _opened _age_secs _now _qdir
    _now="$(date +%s)"
    _qdir="${SPIRA_QUEUE_DIR:-$SPIRA_RUN/queue}"
    for _bf in "$_qdir"/*/open; do
        [ -f "$_bf" ] || continue
        _pr="$(grep '^pr=' "$_bf" 2>/dev/null | cut -d= -f2-)" || continue
        _opened="$(grep '^opened=' "$_bf" 2>/dev/null | cut -d= -f2-)" || continue
        [ -n "${_pr:-}" ] || continue
        batch_pr="$_pr"
        _age_secs=$(( _now - ${_opened:-_now} ))
        if [ "$_age_secs" -lt 90 ]; then batch_age="${_age_secs}s"
        elif [ "$_age_secs" -lt 5400 ]; then batch_age="$(( _age_secs / 60 ))m"
        elif [ "$_age_secs" -lt 172800 ]; then batch_age="$(( _age_secs / 3600 ))h"
        else batch_age="$(( _age_secs / 86400 ))d"; fi
        break
    done
    echo "SP_QUEUE_BATCH_PR=$batch_pr"
    echo "SP_QUEUE_BATCH_AGE=$batch_age"

    local quarantine_n=0 _rname _rp _sf _n
    for _rname in $(spira_repos 2>/dev/null); do
        _rp="$(repo_root "$_rname" 2>/dev/null)" || continue
        _sf="$_rp/${SPIRA_SUITE_STATE:-spira/suite-state}"
        [ -f "$_sf" ] || continue
        _n="$(grep -c ' | quarantined |' "$_sf" 2>/dev/null)" || _n=0
        quarantine_n=$(( quarantine_n + _n ))
    done
    echo "SP_QUEUE_QUARANTINE_N=$quarantine_n"
}

# The sphere-grid keys: plan-bead counts (open, in-progress, needs-op) and the poison count.
#
# SP_POISON IS A SEPARATE QUERY, independent of the spira,plan scoping. Poison is applied
# only by aeon.sh's closing rule when SOP_REQUIRED=1, which fires for ops and qa personas
# whose beads are labelled `incident`, not `plan`. Counting SP_POISON inside the spira,plan
# filter therefore produced a counter that was structurally always zero: of eleven open
# poisoned beads measured on 2026-09-08, not one carried the plan label, so the filter and
# the population were disjoint by construction (defect sp-b3ub).
#
# The remaining three keys (OPEN, INPROG, NEEDSOP) remain scoped to spira,plan, which is
# correct: law-spira-is-a-replica-until-cutover is a good reason for that scope and only
# SP_POISON does not belong inside it.
#
# Broken out as a function so the test suite can drive the exact code the collector runs —
# the same reason strand_keys and sop_keys are functions, not inlined.
sphere_keys() {
    # SP_POISON: non-closed beads carrying spira-poison, across ALL partitions.
    # A failed probe renders ?, never 0 (law-failed-probe-renders-question).
    local _poison_raw
    _poison_raw="$(bdjson list --all --limit 0 --label spira-poison 2>/dev/null)"
    if [ -z "$_poison_raw" ]; then
        echo "SP_POISON=?"
    else
        printf '%s\n' "$_poison_raw" | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: print("SP_POISON=?"); raise SystemExit
d = d if isinstance(d, list) else [d]
print("SP_POISON=%d" % sum(1 for i in d if i.get("status") != "closed"))' 2>/dev/null \
            || echo "SP_POISON=?"
    fi

    bdjson list --limit 0 --label "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan" 2>/dev/null | python3 -c '
import os, sys, json
# The escalation label is one configured key, read from the environment rather than written
# in: five literals in five files is how the panel, the gate and the predicates come to
# disagree about which beads are waiting on anyone.
ASK = os.environ["SPIRA_ASK_LABEL"]
try: d = json.load(sys.stdin)
except Exception:
    for k in ("OPEN", "INPROG", "NEEDSOP"): print("SP_%s=?" % k)
    raise SystemExit
d = d if isinstance(d, list) else [d]
def has(i, lab): return lab in (i.get("labels") or [])
# _is_work is the generated column from schema-apply. When present it is authoritative;
# when absent (schema not yet applied) fall back to SCHEMA_WORK_TYPES so the count is
# correct on both schema versions. Epics are work items, counted here.
_WORK = {"task","bug","feature","epic","chore","spike"}
work = [i for i in d if i.get("_is_work") == 1 or (i.get("_is_work") is None and i.get("issue_type") in _WORK)]
print("SP_OPEN=%d"      % sum(1 for i in work if i.get("status") != "closed"))
print("SP_INPROG=%d"    % sum(1 for i in work if i.get("status") == "in_progress"))
print("SP_NEEDSOP=%d"  % sum(1 for i in work if i.get("status") != "closed" and has(i, ASK)))
' 2>/dev/null || { echo "SP_OPEN=?"; echo "SP_INPROG=?"; echo "SP_NEEDSOP=?"; }
}

# The repo-label keys: non-closed beads whose repo: label is either absent from the
# repo-map or missing entirely. Both count as failures: a bead with `repo:bogus` parks an
# aeon at unmapped-repo just as a bead with no label does — they fail closed identically.
#
# A SEPARATE QUERY FROM THE SPHERE GRID. The sphere grid is scoped to spira,plan; this
# check spans every non-closed bead in the database, because a mis-labelled bead outside
# that scope parks an aeon on unmapped-repo the same way.
#
# Broken out as a function so the test suite can drive the exact code the collector runs —
# the same reason strand_keys and sphere_keys are functions and not inlined.
repo_label_keys() {
    # SP_REPO_UNMAPPED: non-closed beads with a repo: label absent from the map.
    # SP_REPO_ABSENT:   non-closed beads carrying no repo: label at all.
    # Both render ? when the repo-map is unreadable — an unreadable map and a clean map are
    # indistinguishable from outside; the ? is the only honest answer
    # (law-absence-needs-a-positive-control).
    if [ ! -r "${SPIRA_REPO_MAP:-}" ]; then
        echo "SP_REPO_UNMAPPED=?"
        echo "SP_REPO_ABSENT=?"
        return
    fi
    local valid_names _raw
    valid_names="$(awk 'BEGIN{FS="|"} /^[ \t]*#/{next}
        {n=$1; gsub(/^[ \t]+|[ \t]+$/,"",n); if(n!=""&&NF>1) print n}' \
        "$SPIRA_REPO_MAP" 2>/dev/null)"
    # bdjson list --limit 0 returns non-closed beads only (default filter). An empty result
    # means bd refused (schema mismatch, misconfigured path) — distinguish from a real empty
    # list, which arrives as "[]" that json_only passes through unchanged.
    _raw="$(bdjson list --limit 0 2>/dev/null)"
    if [ -z "$_raw" ]; then
        echo "SP_REPO_UNMAPPED=?"
        echo "SP_REPO_ABSENT=?"
        return
    fi
    printf '%s\n' "$_raw" | VALID_NAMES="$valid_names" python3 -c '
import os, sys, json
try: d = json.load(sys.stdin)
except Exception:
    print("SP_REPO_UNMAPPED=?"); print("SP_REPO_ABSENT=?"); raise SystemExit
d = d if isinstance(d, list) else [d]
# Names come in newline-separated; split() handles any whitespace including newlines.
valid = set(os.environ.get("VALID_NAMES", "").split())
unmapped = absent = 0
for i in d:
    repo_labels = [l[5:] for l in (i.get("labels") or []) if l.startswith("repo:")]
    if not repo_labels:
        absent += 1
    elif not any(r in valid for r in repo_labels):
        unmapped += 1
print("SP_REPO_UNMAPPED=%d" % unmapped)
print("SP_REPO_ABSENT=%d" % absent)
' 2>/dev/null || { echo "SP_REPO_UNMAPPED=?"; echo "SP_REPO_ABSENT=?"; }
}

# The duplicate-ref meter: how many external_ref values appear on more than one bead.
#
# SP_DUP_REFS:  count of distinct external_ref values carried by more than one bead within
#               the lookback window (open beads always included; closed beads within
#               SPIRA_INCIDENT_DEDUP_LOOKBACK days, matching incident.sh's own window).
# SP_DUP_BEADS: total surplus beads — sum of (count - 1) per duplicated ref.
# SP_DUP_ROW0..N: the worst offenders, for the watchtower's incident body.
#
# WHY THIS EXISTS (law-dedup-must-be-measured). Every filer trusts incident.sh's dedup; this
# is the check on the dedup itself. A nonzero SP_DUP_REFS means two or more beads exist for
# the same event, which is the looping-incident pattern that produced 38 surplus beads over
# two days and was found by the operator looking, not by any instrument. When dedup is working
# the meter reads 0; when it breaks the meter says so before anyone has to notice the queue
# filling up.
#
# A FAILED PROBE RENDERS ?, NEVER 0. A dedup meter showing 0 because it could not query is the
# exact failure it exists to catch (law-absence-needs-a-positive-control). The ? convention
# tells Ops to read the code rather than trust the reassuring zero.
#
# SCOPED TO spira,incident — the label pair incident.sh sets on every bead it files. This
# keeps the query set small and matches the dedup candidate pool incident.sh itself searches.
#
# Broken out as a function so the test suite can drive the exact code the collector runs,
# the same reason strand_keys and livelock_keys are functions and not inlined.
dup_refs_keys() {
    local _since _raw
    _since="$(date -u -d "-${SPIRA_INCIDENT_DEDUP_LOOKBACK:-7} days" '+%Y-%m-%d' 2>/dev/null)"
    if [ -z "$_since" ]; then
        echo "SP_DUP_REFS=?"; echo "SP_DUP_BEADS=?"; echo "SP_DUP_N=0"; return
    fi
    # --all: include closed beads (within-lookback filter is done in Python below).
    # Empty output means bdjson failed (bd unreachable, schema mismatch); a real empty
    # result arrives as "[]" which json_only passes through, so the variable is never
    # empty when the query succeeded.
    _raw="$(bdjson list --all --limit 0 --label spira,incident 2>/dev/null)"
    if [ -z "$_raw" ]; then
        echo "SP_DUP_REFS=?"; echo "SP_DUP_BEADS=?"; echo "SP_DUP_N=0"; return
    fi
    printf '%s\n' "$_raw" | DUP_SINCE="$_since" python3 -c '
import sys, json, os
from collections import defaultdict

cutoff = os.environ.get("DUP_SINCE", "")
try: d = json.load(sys.stdin)
except Exception:
    print("SP_DUP_REFS=?"); print("SP_DUP_BEADS=?"); print("SP_DUP_N=0")
    raise SystemExit
d = d if isinstance(d, list) else [d]

by_ref = defaultdict(list)
for i in d:
    ref = i.get("external_ref") or ""
    if not ref:
        continue
    # Include non-closed beads always; include closed beads only within the lookback.
    if i.get("status") == "closed" and cutoff:
        closed_at = (i.get("closed_at") or "")[:10]
        if closed_at < cutoff:
            continue
    by_ref[ref].append(i["id"])

dup = {r: ids for r, ids in by_ref.items() if len(ids) > 1}
print("SP_DUP_REFS=%d" % len(dup))
print("SP_DUP_BEADS=%d" % sum(len(ids) - 1 for ids in dup.values()))
rows = sorted(dup.items(), key=lambda x: -len(x[1]))[:5]
for n, (ref, ids) in enumerate(rows):
    line = "%s +%d %s" % (ref, len(ids) - 1, " ".join(ids[:5]))
    print("SP_DUP_ROW%d=%s" % (n, line[:120]))
print("SP_DUP_N=%d" % len(rows))
' 2>/dev/null || { echo "SP_DUP_REFS=?"; echo "SP_DUP_BEADS=?"; echo "SP_DUP_N=0"; }
}

# THE SERIES, because a gauge cannot answer "over time". The question the token meter exists
# for is what has been contributing to the rate limit across a day, and no instant answers it.
#
# ITS OWN FILE, under $SPIRA_RUN, for the same reason the snapshot is: the predecessor
# harness's collector owns the other cockpit-history.csv and is deleted along with it. A
# series appended by a service that is being retired stops without anyone noticing, and a
# flat line reads as calm rather than as absent.
#
# EVERY TOKEN COLUMN IS A ROLLING WINDOW TOTAL, not a counter — successive rows overlap and
# must never be summed. That is what makes it readable against the limit, which is itself a
# rolling window: the column IS the thing the account is judged on.
#
# A `?` OR `-` IS WRITTEN THROUGH VERBATIM. A probe that failed and a probe that measured zero
# are different facts, and collapsing them here would put the difference beyond recovery for
# every reader downstream.
HIST="$SPIRA_RUN/cockpit-history.csv"
HISTORY_MAX="${SPIRA_COCKPIT_HISTORY_MAX:-20160}"   # 14 days at the default 60s cadence
HIST_COLS="ts,tok_win,tok_aeon_win,tok_sess_win,tok_aeon_turns,tok_sess_turns,ctx_now,ratelim_5h,ratelim_7d"

append_history() {
    # Read back the file just written rather than the probe's own output: the snapshot is what
    # every other reader sees, so a row disagreeing with it would be a third opinion.
    #
    # IN A SUBSHELL, WHICH IS LOAD-BEARING IN `loop` MODE. Sourcing the snapshot into this
    # process leaves its keys set, so the NEXT pass — with a probe that had failed and written
    # no key at all — would quietly reuse the previous pass's figure instead of `?`. A stale
    # number presented as current is precisely the confident wrong reading the `?` rule exists
    # to prevent, and it would be indistinguishable from a healthy flat line.
    local row
    row="$(
        set +u
        # shellcheck disable=SC1090
        . "$SNAP" 2>/dev/null
        printf '%s,%s,%s,%s,%s,%s,%s,%s,%s' \
            "${SP_AT:-$(date +%s)}" "${SP_TOK_WIN:-?}" "${SP_TOK_AEON_WIN:-?}" \
            "${SP_TOK_SESS_WIN:-?}" "${SP_TOK_AEON_TURNS:-?}" "${SP_TOK_SESS_TURNS:-?}" \
            "${SP_CTX_NOW:-?}" "${SP_RATELIM_5H:-?}" "${SP_RATELIM_7D:-?}"
    )"
    # A CHANGED COLUMN SET ROTATES THE FILE RATHER THAN APPENDING A SECOND HEADER. Every reader
    # takes line one as the header, so a header written into the middle is parsed as data and
    # every column after it is read under the wrong name — a series that is quietly wrong is
    # worse than one that is quietly short. The old rows are moved aside, not deleted: they are
    # the only record of what came before, and nothing here is worth destroying to keep the
    # shape tidy.
    if [ ! -s "$HIST" ]; then
        echo "$HIST_COLS" > "$HIST"
    elif [ "$(head -1 "$HIST")" != "$HIST_COLS" ]; then
        mv -f "$HIST" "$HIST.$(date +%s)" 2>/dev/null
        echo "$HIST_COLS" > "$HIST"
    fi
    printf '%s\n' "$row" >> "$HIST"

    local lines; lines=$(wc -l < "$HIST" 2>/dev/null || echo 0)
    if [ "${lines:-0}" -gt $(( HISTORY_MAX + 1 )) ] 2>/dev/null; then
        { head -1 "$HIST"; tail -n "$HISTORY_MAX" "$HIST"; } > "$HIST.tmp" \
            && mv -f "$HIST.tmp" "$HIST"
    fi
}

# The two rate-limit windows, read from the newest rate_limit_event across live aeon traces.
#
# WHY TRACES AND NOT THE HOOK. ctx-meter.sh maintains limits.samples from the status-line
# hook, which fires only when the operator's interactive session is speaking. Aeons run
# throughout the day and their traces record rate_limit_event on every API call — so even
# with nobody at the keyboard, the most recent utilisation reading is in whatever trace was
# written last. That reading is what says "the account is approaching full while the operator
# is offline." On 2026-09-06 the five-hour window hit 1.0 three times with nothing on the
# dashboard to warn of it.
#
# THE READING IS FROM THE LAST ~128KB OF THE NEWEST LOG. rate_limit_events fire on every
# API call and are dense in any active session, so the tail reliably holds a recent one
# without reading the whole file. At 30 files maximum, with most already in the VFS cache,
# this costs one seek-and-read per file.
#
# THE SLOPE IS FROM THE HISTORY CSV, not from consecutive trace events. A slope from two
# adjacent events is the per-turn rate on those two turns — dominated by quantisation noise.
# The collector's history at its 60s interval is already smoothed, and a slope from an hour
# of that history is the signal that says "the account will be full in N minutes".
#
# A MISSING OR UNREADABLE TRACE RENDERS `?`, NEVER 0. A window shown at 0% is the best
# possible news; a failed probe must never produce it (law-absence-needs-a-positive-control).
#
# Emitted as its own seam so a suite can drive the same code the collector runs.
ratelim_keys() {
    # THE SCRIPT ARRIVES ON FD 3, NOT STDIN. `python3 - <<PY` reads the heredoc as data too:
    # the redirect wins, the args are discarded, and the script then fails to parse its inputs.
    # `python3 /dev/fd/3 arg1 arg2 3<<PY` keeps stdin free and passes args normally.
    python3 /dev/fd/3 "$SPIRA_RUN" "$HIST" 3<<'PY' 2>/dev/null && return 0
import sys, json, os, glob, time

run, hist = sys.argv[1], sys.argv[2]
now = int(time.time())

KEYS = ["SP_RATELIM_5H", "SP_RATELIM_7D", "SP_RATELIM_5H_PCT", "SP_RATELIM_7D_PCT",
        "SP_RATELIM_5H_MIN", "SP_RATELIM_7D_MIN", "SP_RATELIM_5H_ETA", "SP_RATELIM_7D_ETA",
        "SP_RATELIM_AGE"]

TAIL = 131072  # 128 KB — enough for many rate_limit_event occurrences

# NEWEST LOG FIRST. rate_limit_event data in a log that was written last is the most current
# reading of the account-wide windows; earlier logs may predate a window reset and their
# resetsAt would be in the past.
try:
    logs = sorted(glob.glob(os.path.join(run, "*.log")),
                  key=lambda f: os.path.getmtime(f), reverse=True)
except Exception:
    for k in KEYS: print(f"{k}=?")
    raise SystemExit(1)

five_h = seven_d = None
file_mtime = None

for lf in logs[:30]:
    try:
        mtime = int(os.path.getmtime(lf))
    except OSError:
        continue
    try:
        with open(lf, "rb") as fh:
            size = fh.seek(0, 2)
            fh.seek(max(0, size - TAIL))
            chunk = fh.read()
    except OSError:
        continue
    # LAST OCCURRENCE IN THE FILE IS THE NEWEST. Scanning forward and keeping `last` gives
    # the most recent rate_limit_event in the tail without reversing a potentially large list.
    last = None
    for line in chunk.decode("utf-8", errors="replace").splitlines():
        if '"rate_limit_event"' not in line:
            continue
        try:
            d = json.loads(line)
        except Exception:
            continue
        if d.get("type") != "rate_limit_event":
            continue
        rl = d.get("rate_limit_info") or {}
        uw = rl.get("unifiedWindows") or {}
        if uw.get("five_hour") or uw.get("seven_day"):
            last = (uw.get("five_hour"), uw.get("seven_day"), mtime)
    if last:
        five_h, seven_d, file_mtime = last
        break

if five_h is None and seven_d is None:
    for k in KEYS: print(f"{k}=?")
    raise SystemExit(0)

# Extract validated utilization (0.0–1.0) and resetsAt (epoch seconds) from one window dict.
def win_vals(w):
    if not isinstance(w, dict): return None, None
    u = w.get("utilization")
    r = w.get("resetsAt")
    if isinstance(u, bool) or not isinstance(u, (int, float)): return None, None
    if isinstance(r, bool) or not isinstance(r, (int, float)): return None, None
    if not 0 <= float(u) <= 1: return None, None
    return float(u), int(r)

util_5h, reset_5h = win_vals(five_h) if five_h else (None, None)
util_7d, reset_7d = win_vals(seven_d) if seven_d else (None, None)

def mins_to(epoch):
    if epoch is None: return "?"
    return str(max(0, (epoch - now) // 60))

# ETA from the last hour of history. Returns seconds-to-full as a string, '-' (resets first),
# '?' (no slope), or '0' (already full). TWO FILE OPENS, not one: DictReader reads the
# header in the first pass; we look up the column index and re-read only the data rows we
# need. This avoids loading the whole file at once when the series is long.
def eta_from_history(col_name, current_util, reset_epoch):
    if current_util is None: return "?"
    if current_util >= 1.0: return "0"
    try:
        with open(hist) as f:
            header = f.readline().strip().split(",")
        try:
            col_idx = header.index(col_name)
        except ValueError:
            return "?"  # column does not exist yet; happens on the first pass after upgrade
        rows = []
        hour_ago = now - 3600
        with open(hist) as f:
            f.readline()  # skip header
            for line in f:
                parts = line.strip().split(",")
                if len(parts) <= col_idx: continue
                try:
                    ts = int(parts[0])
                    val_s = parts[col_idx]
                    if val_s in ("?", "-", ""): continue
                    val = float(val_s)
                    if ts >= hour_ago: rows.append((ts, val))
                except Exception: continue
    except Exception:
        return "?"
    if len(rows) < 2: return "?"
    rows.sort()
    span = rows[-1][0] - rows[0][0]
    move = rows[-1][1] - rows[0][1]
    # MINIMUM SPAN AND MINIMUM MOVE, matching ctx-meter.sh's LIM_MIN_SPAN / LIM_MIN_MOVE.
    # Below either floor the slope is noise: a single quantisation step of 0.01 across 600s
    # is already a 0.06%/h rate, and one below 0.02 total move in an hour is unmeasurable.
    if span < 600 or move < 0.02: return "?"
    slope = move / span  # utilization per second
    if slope <= 0: return "?"
    eta_secs = int((1.0 - current_util) / slope)
    # THE RESET WINS. A window that fills in 2h but resets in 45m needs no remediation;
    # showing a fill ETA for it would be misleading.
    if reset_epoch is not None:
        secs_to_reset = max(0, reset_epoch - now)
        if secs_to_reset < eta_secs: return "-"
    return str(eta_secs)

eta_5h = eta_from_history("ratelim_5h", util_5h, reset_5h)
eta_7d = eta_from_history("ratelim_7d", util_7d, reset_7d)

def fmt_util(u): return "%.3f" % u if u is not None else "?"
def fmt_pct(u):
    if u is None: return "?"
    return str(int(round(u * 100)))

print(f"SP_RATELIM_5H={fmt_util(util_5h)}")
print(f"SP_RATELIM_7D={fmt_util(util_7d)}")
print(f"SP_RATELIM_5H_PCT={fmt_pct(util_5h)}")
print(f"SP_RATELIM_7D_PCT={fmt_pct(util_7d)}")
print(f"SP_RATELIM_5H_MIN={mins_to(reset_5h)}")
print(f"SP_RATELIM_7D_MIN={mins_to(reset_7d)}")
print(f"SP_RATELIM_5H_ETA={eta_5h}")
print(f"SP_RATELIM_7D_ETA={eta_7d}")
print(f"SP_RATELIM_AGE={now - file_mtime if file_mtime is not None else '?'}")
PY
    for _k in SP_RATELIM_5H SP_RATELIM_7D SP_RATELIM_5H_PCT SP_RATELIM_7D_PCT \
              SP_RATELIM_5H_MIN SP_RATELIM_7D_MIN SP_RATELIM_5H_ETA SP_RATELIM_7D_ETA \
              SP_RATELIM_AGE; do
        echo "$_k=?"
    done
}

# Livelocked and invalid-closed bead counts, broken out as their own function so the
# test suite can drive the exact code the collector runs — the same reason strand_keys and
# sop_keys are functions and not inlined.
#
# SP_LIVELOCKED: count of open beads no mechanism will ever resolve — structural, not timing.
# SP_INVALID_CLOSED: count of closed beads whose close reason admits the work is unfinished
#   (statute phrases: "PERMANENT FIX NEEDED", "temporary", "mitigated-only", "TODO").
# SP_UNFILED_FOLLOW: count of closed beads whose close reason implies follow-on work but
#   names no bead id — the follow-on was observed and not filed.
# All three render ? when the underlying query fails; ? and 0 must not look the same
# (law-absence-needs-a-positive-control).
#
# INDIVIDUAL ROWS are emitted alongside the counts so the panel can show which beads are
# affected and why. SP_LIVELOCK_N, SP_INVCLSD_N and SP_UNFLFLW_N carry the row counts;
# individual rows are SP_LIVELOCK0..N-1, SP_INVCLSD0..N-1 and SP_UNFLFLW0..N-1.
# All cap at 20 rows to stay pane-friendly.
livelock_keys() {
    local _ll_out _ic_out _n
    _ll_out="$(detect_livelocked 2>/dev/null)"
    _ic_out="$(detect_invalid_closed 2>/dev/null)"

    # LIVELOCKED count and rows
    if [ -z "$_ll_out" ] && ! bdjson list --limit 1 >/dev/null 2>&1; then
        # A genuinely empty database and a failed query both produce empty output.
        # Distinguish by probing the database; a failure renders ?.
        echo "SP_LIVELOCKED=?"
        echo "SP_LIVELOCK_N=?"
    else
        _n=0
        if [ -n "$_ll_out" ]; then
            while IFS= read -r _line; do
                [ -n "$_line" ] || continue
                case "$_line" in LIVELOCK\ *)
                    _line="$(printf '%s' "$_line" | tr -c 'A-Za-z0-9 ._/:,()#+-' ' ' | tr -s ' ')"
                    printf 'SP_LIVELOCK%d=%s\n' "$_n" "${_line:0:120}"
                    _n=$((_n+1))
                    [ "$_n" -ge 20 ] && break
                ;; esac
            done <<< "$_ll_out"
        fi
        echo "SP_LIVELOCKED=$_n"
        echo "SP_LIVELOCK_N=$_n"
    fi

    # INVALID-CLOSED and UNFILED-FOLLOW counts and rows (both come from detect_invalid_closed).
    if [ -z "$_ic_out" ] && ! bdjson list --status closed --limit 1 >/dev/null 2>&1; then
        echo "SP_INVALID_CLOSED=?"
        echo "SP_INVCLSD_N=?"
        echo "SP_UNFILED_FOLLOW=?"
        echo "SP_UNFLFLW_N=?"
    else
        local _ic_n=0 _uf_n=0
        if [ -n "$_ic_out" ]; then
            while IFS= read -r _line; do
                [ -n "$_line" ] || continue
                case "$_line" in
                INVALID-CLOSED\ *)
                    _line="$(printf '%s' "$_line" | tr -c 'A-Za-z0-9 ._/:,()#+-' ' ' | tr -s ' ')"
                    printf 'SP_INVCLSD%d=%s\n' "$_ic_n" "${_line:0:120}"
                    _ic_n=$((_ic_n+1))
                    [ "$_ic_n" -ge 20 ] && true
                    ;;
                UNFILED-FOLLOW\ *)
                    _line="$(printf '%s' "$_line" | tr -c 'A-Za-z0-9 ._/:,()#+-' ' ' | tr -s ' ')"
                    printf 'SP_UNFLFLW%d=%s\n' "$_uf_n" "${_line:0:120}"
                    _uf_n=$((_uf_n+1))
                    [ "$_uf_n" -ge 20 ] && true
                    ;;
                esac
            done <<< "$_ic_out"
        fi
        echo "SP_INVALID_CLOSED=$_ic_n"
        echo "SP_INVCLSD_N=$_ic_n"
        echo "SP_UNFILED_FOLLOW=$_uf_n"
        echo "SP_UNFLFLW_N=$_uf_n"
    fi
}

# The strand ledger, BROKEN OUT BY KIND. strands.json holds every disposition strand.sh
# classifies — ghost, empty, starved, stuck, cycle and the rest — and only `ghost` is the
# labelled failure of a claimed bead whose holder is gone. Reporting its SIZE under that
# one member's name is law-alerts-must-be-actionable in miniature: a childless epic and a
# dead worker counted identically, so a sweep spent four commands hunting for a holder that
# had never existed.
#
# THE KEY IS SPLIT FROM THE RIGHT. It is `<partition>:<kind>:<id>`, and a partition is a
# label list that may itself contain a colon; an id may not. Splitting from the left reads
# the partition as the kind on exactly the entries that are hardest to reason about.
#
# Emitted as its own seam so a suite can drive the classifier the collector actually runs
# rather than a copy of it, the same way `history` exposes append_history.
strand_keys() {
    local f="$SPIRA_RUN/strands.json"
    # No file is not the same claim as no strands: strand.sh writes it on its first pass,
    # so its absence means the detector has not run, which is a different fault.
    if [ -f "$f" ]; then
        python3 -c '
import sys, json
KEYS = ("SP_STRANDS", "SP_STRANDS_ESCALATED", "SP_STRAND_GHOST", "SP_STRAND_OTHER")
try: d = json.load(open(sys.argv[1]))
except Exception:
    for k in KEYS: print("%s=?" % k)
    raise SystemExit
counts, bad = {}, 0
for k in d:
    part = k.rsplit(":", 2)
    if len(part) != 3 or not part[1]:
        bad += 1
        continue
    counts[part[1]] = counts.get(part[1], 0) + 1
print("SP_STRANDS=%d" % len(d))
print("SP_STRANDS_ESCALATED=%d" % sum(1 for v in d.values() if v.get("escalated")))
# AN UNREADABLE KEY MAKES THE GHOST COUNT UNKNOWN, NEVER ZERO (law-absence-needs-a-positive-
# control). The entry that could not be classified may well be a ghost, and a confident 0
# there is precisely the reading that stops anybody looking.
print("SP_STRAND_GHOST=%s" % ("?" if bad else counts.get("ghost", 0)))
rest = ["%s=%d" % (k, counts[k]) for k in sorted(counts) if k != "ghost"]
if bad: rest.append("unclassified=%d" % bad)
# COMMA-SEPARATED, NEVER SPACES. A renderer `source`s the snapshot, and while write_snapshot
# quotes what it writes, this same function is read directly by the suite and by anything
# that evals a bare line.
print("SP_STRAND_OTHER=%s" % (",".join(rest) or "none"))
' "$f" 2>/dev/null && return 0
    fi
    echo "SP_STRANDS=?"; echo "SP_STRANDS_ESCALATED=?"
    echo "SP_STRAND_GHOST=?"; echo "SP_STRAND_OTHER=?"
}

# The SOP shelf metrics, broken out so a suite can drive the exact code the collector
# runs — the same reason strand_keys is a function and not inlined.
#
# SP_SOP_NEVER_FIRED: SOPs on the shelf with no entry at all in the applications ledger.
# Candidates for retirement, but only after a human has checked: an SOP for a rare disaster
# is exactly the one that never fires and is the costliest to lose. REPORTED ONLY; never
# auto-retired here.
#
# SP_SOP_RECURRED: distinct SOP slugs where check=pass and held=no inside the window — the
# runbook applied and its fix did NOT hold. This is the single most informative signal on the
# shelf, and it is the one no session has ever seen before because nothing recorded it.
#
# A FAILED PROBE RENDERS `?`, NEVER 0 (law-absence-needs-a-positive-control). An unreadable
# shelf is indistinguishable from an empty one from the outside; conflating them here would
# make a database outage look like a shelf with nothing on it — and render 0 never-fired
# while every SOP on the shelf has in fact never been exercised.
sop_keys() {
    local _sop_ledger="${SPIRA_SOP_LEDGER:-$SPIRA_RUN/sop/applied.jsonl}"
    local _sop_raw
    # The empty STRING means the query FAILED, not that the shelf is bare: `bd memories --json`
    # prints nothing on failure and `{}` on an honest empty shelf. Conflating them would let a
    # database outage erase the never-fired count and make a broken probe read as all-clear.
    _sop_raw="$(bdjson memories 2>/dev/null)"
    if [ -z "${_sop_raw//[[:space:]]/}" ]; then
        echo "SP_SOP_NEVER_FIRED=?"
        echo "SP_SOP_RECURRED=?"
        echo "SP_SWEEP_AGE=?"
        return
    fi
    printf '%s\n' "$_sop_raw" | python3 -c '
import sys, json, os, time
from datetime import datetime, timedelta, timezone

ledger_path = sys.argv[1]
window_h = float(sys.argv[2]) if len(sys.argv) > 2 else 24.0
since = datetime.now(timezone.utc) - timedelta(hours=window_h)
now_epoch = int(time.time())

try:
    shelf = json.load(sys.stdin)
    sop_keys = {k for k, v in shelf.items() if isinstance(v, str) and k.startswith("sop-")}
except Exception:
    print("SP_SOP_NEVER_FIRED=?")
    print("SP_SOP_RECURRED=?")
    print("SP_SWEEP_AGE=?")
    raise SystemExit

ledger_sops = set()
recurred_sops = set()
# NEWEST ENTRY IN THE LEDGER, by epoch field. An empty ledger means the sweep has never run
# and renders ?, not 0 — a stopped sweep and a quiet one must be distinguishable
# (law-arm-before-you-retire, law-absence-needs-a-positive-control).
newest_epoch = None

# A MISSING LEDGER IS A VALID STATE, NOT AN ERROR: no SOP has ever been applied, so every
# SOP on the shelf is never-fired. An UNREADABLE ledger (present but cannot be opened)
# is the failure case and renders `?`.
if os.path.exists(ledger_path):
    try:
        with open(ledger_path, encoding="utf-8", errors="replace") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    r = json.loads(line)
                except Exception:
                    continue
                k = r.get("sop", "")
                if not k:
                    continue
                ledger_sops.add(k)
                if r.get("check") == "pass" and r.get("held") == "no":
                    try:
                        ts = datetime.strptime(
                            r.get("ts", ""), "%Y-%m-%dT%H:%M:%SZ"
                        ).replace(tzinfo=timezone.utc)
                        if ts >= since:
                            recurred_sops.add(k)
                    except Exception:
                        pass
                ep = r.get("epoch")
                if isinstance(ep, (int, float)) and ep > 0:
                    ep = int(ep)
                    if newest_epoch is None or ep > newest_epoch:
                        newest_epoch = ep
    except Exception:
        print("SP_SOP_NEVER_FIRED=?")
        print("SP_SOP_RECURRED=?")
        print("SP_SWEEP_AGE=?")
        raise SystemExit

print("SP_SOP_NEVER_FIRED=%d" % len(sop_keys - ledger_sops))
print("SP_SOP_RECURRED=%d" % len(recurred_sops))
if newest_epoch is not None:
    print("SP_SWEEP_AGE=%d" % max(0, now_epoch - newest_epoch))
else:
    print("SP_SWEEP_AGE=?")
' "$_sop_ledger" "$WINDOW_HOURS" 2>/dev/null || {
        echo "SP_SOP_NEVER_FIRED=?"
        echo "SP_SOP_RECURRED=?"
        echo "SP_SWEEP_AGE=?"
    }
}

# The statute projection integrity check. Compares how many law- statutes are in the live
# database against how many ### headings the last-committed wiki/notes/common-law.md carries.
#
# WHY THIS EXISTS. On 2026-09-11 law-synth.sh was called against a near-empty store and
# overwrote 112 statutes with 3 on disk. It was never committed, but law-cron.sh detected the
# failure in a log file nobody reads. A log line that reaches nowhere is the same as no check
# (law-alerts-must-be-actionable). This key puts the skew where the cockpit panel sees it so
# the operator does not have to look for the log (sp-p0xyt).
#
# THREE KEYS:
#   SP_STATUTE_DB_N   — law- count in the live database (? if db unreadable)
#   SP_STATUTE_PAGE_N — ### heading count in HEAD:wiki/notes/common-law.md (? if git unreadable)
#   SP_STATUTE_SKEW   — OK if counts are reasonably aligned; MISMATCH:DB=N,PAGE=M if the
#                       database looks far below what the committed page carries (possible wrong
#                       store); ? if either side could not be read.
#
# A FAILED PROBE RENDERS `?`, NEVER 0 (law-absence-needs-a-positive-control).
statute_keys() {
    local _wiki="${SPIRA_WIKI:-}"
    if [ -z "$_wiki" ]; then
        echo "SP_STATUTE_DB_N=?"
        echo "SP_STATUTE_PAGE_N=?"
        echo "SP_STATUTE_SKEW=?"
        return
    fi

    local _db_n _page_n
    _db_n="$(bdjson memories 2>/dev/null | python3 -c '
import json, sys
raw = sys.stdin.read().strip()
if not raw:
    print("?"); raise SystemExit(1)
try:
    book = json.loads(raw)
    n = sum(1 for k, v in book.items() if isinstance(v, str) and k.startswith("law-"))
    print(n)
except Exception:
    print("?"); raise SystemExit(1)
' 2>/dev/null)" || _db_n="?"

    _page_n="$(git -C "$_wiki" show "HEAD:wiki/notes/common-law.md" 2>/dev/null \
        | grep -c '^### ')" || _page_n="?"

    echo "SP_STATUTE_DB_N=${_db_n}"
    echo "SP_STATUTE_PAGE_N=${_page_n}"

    # Skew: if either side unreadable → ?. If db_count < page_count/2 → MISMATCH.
    # db_count >= page_count/2 is OK — the page may be a few commits behind, that's normal.
    if [ "$_db_n" = "?" ] || [ "$_page_n" = "?" ]; then
        echo "SP_STATUTE_SKEW=?"
    else
        python3 -c "
db, page = int('$_db_n'), int('$_page_n')
if db == 0:
    print('SP_STATUTE_SKEW=MISMATCH:DB=0,PAGE=%d' % page)
elif page > 0 and db * 2 < page:
    print('SP_STATUTE_SKEW=MISMATCH:DB=%d,PAGE=%d' % (db, page))
else:
    print('SP_STATUTE_SKEW=OK')
" 2>/dev/null || echo "SP_STATUTE_SKEW=?"
    fi
}

# The snapshot is written to a temp and renamed, so a reader can never see a half-file.
# The temp is a SCRIPT-LEVEL variable with its trap installed once, not a local re-armed on
# every pass: in loop mode write_snapshot runs forever, and the pass that gets killed is
# exactly the one that would have been running unarmed. Probing is the slow part and a kill
# lands in it, so every unclean exit used to leave a temp behind — 303 of them accumulated
# in one hour of a restart loop, and a directory that grows a file per unclean exit also
# hides how often unclean exits happen, because nothing counts them.
SNAP_TMP=""
clear_snap_tmp() {
    [ -n "$SNAP_TMP" ] && rm -f -- "$SNAP_TMP"
    SNAP_TMP=""
    return 0
}
trap clear_snap_tmp EXIT
# Each signal re-raises itself after cleaning up rather than exiting with a made-up status,
# so the caller — systemd on a restart, a shell on Ctrl-C — still sees the death it sent.
for _sig in INT TERM HUP; do
    trap "clear_snap_tmp; trap - $_sig; kill -s $_sig \$\$" "$_sig"
done
unset _sig

write_snapshot() {
    # mktemp, not `.cockpit.$$`: a recycled pid collides with a leaked temp from a previous
    # one, and the redirect below would then silently reuse that file.
    SNAP_TMP="$(mktemp "$SPIRA_RUN/.cockpit.XXXXXXXX")" || return 1
    local writer_unit="${INVOCATION_ID:+spira-cockpit.service}"
    { probe 2>/dev/null; echo "SP_WRITER=$$:${writer_unit:-force}"; } | python3 -c '
import sys
seen = set()
for line in sys.stdin:
    line = line.rstrip("\n")
    if "=" not in line:
        continue
    k, _, v = line.partition("=")
    k = k.strip()
    if not k or not (k[0].isalpha() or k[0] == "_"):
        continue
    # First wins. A key is emitted twice only when a probe printed rows and its fallback
    # then fired as well, and the first of those two is the one that actually measured
    # something — the fallback is a shape guarantee, not a reading.
    if k in seen:
        continue
    seen.add(k)
    print("%s=%s" % (k, "\x27" + v.replace("\x27", "\x27\\\x27\x27") + "\x27"))
' > "$SNAP_TMP"
    # The rename consumes the temp; clearing the variable is what keeps the EXIT trap from
    # chasing a name that is now the snapshot's.
    mv -f "$SNAP_TMP" "$SNAP" && SNAP_TMP=""
    append_history
}

# Remove any .cockpit.* temps left by a previous unclean exit. At startup the previous
# process is gone, so every .cockpit.* in SPIRA_RUN is an orphan: the live snapshot is
# cockpit.env, not .cockpit.anything, and no reader knows the temp's name.
sweep_stale_tmps() {
    local _f _n=0
    for _f in "$SPIRA_RUN"/.cockpit.*; do
        [ -e "$_f" ] || continue
        rm -f -- "$_f" && _n=$((_n+1))
    done
    [ "$_n" -gt 0 ] && echo "cockpit.sh: swept $_n stale temp(s) from $SPIRA_RUN" >&2
    return 0
}

case "${1:-}" in
""|once)
    if cockpit_may_write; then
        sweep_stale_tmps
        write_snapshot
        echo "spira cockpit: $SNAP ($(wc -l < "$SNAP") keys)"
    else
        probe 2>/dev/null
        echo "spira cockpit: keys printed to stdout (not the supervised process)" >&2
    fi
    # NO-ARGS INVOCATION: attach the operator to the concierge after collecting.
    # The timer and collect.sh always pass a subcommand, so this path is for the operator.
    # SPIRA_COCKPIT_NO_ATTACH=1 skips the attach for scripted callers.
    # A nested attach from inside a tmux pane would leave the operator outside the outer
    # session — refuse with a clear message rather than silently breaking the session tree.
    if [ -z "${1:-}" ] && [ -z "${SPIRA_COCKPIT_NO_ATTACH:-}" ]; then
        if [ -n "${TMUX:-}" ]; then
            printf 'cockpit: cannot attach from inside a tmux pane — run from a plain terminal\n' >&2
            exit 1
        fi
        _concierge="${SPIRA_REPO}/concierge.sh"
        if [ -x "$_concierge" ]; then
            "$_concierge" start || exit 1
            exec "$_concierge" attach
        fi
    fi
    ;;
# Appends from the snapshot ALREADY ON DISK, taking no fresh reading. It is how the series is
# repaired without disturbing the live snapshot, and it is the seam the suite drives — the
# same function the loop calls, so what is tested is what runs.
history)
    append_history
    echo "spira cockpit: $HIST ($(( $(wc -l < "$HIST") - 1 )) rows)"
    ;;
loop)
    cockpit_may_write || {
        echo "cockpit.sh loop: write refused — not the supervised process. Set SPIRA_COCKPIT_FORCE=1 to override." >&2
        exit 1
    }
    sweep_stale_tmps
    while :; do write_snapshot; sleep "$INTERVAL"; done
    ;;
# The strand ledger's keys alone, taking no other reading. This is the seam the suite drives:
# it is the same function probe calls, so what is tested is what runs.
strands)
    strand_keys
    ;;
# The SOP shelf keys alone, taking no other reading. This is the seam the suite drives:
# it is the same function probe calls, so what is tested is what runs.
sops)
    sop_keys
    ;;
# The rate-limit window keys alone, taking no other reading. This is the seam the suite
# drives: it is the same function probe calls, so what is tested is what runs.
ratelim)
    ratelim_keys
    ;;
# The sphere-grid keys alone, taking no other reading. This is the seam the suite drives:
# it is the same function probe calls, so what is tested is what runs.
sphere)
    sphere_keys
    ;;
# The repo-label keys alone, taking no other reading. This is the seam the suite drives:
# it is the same function probe calls, so what is tested is what runs.
repo_labels)
    repo_label_keys
    ;;
# The livelocked/invalid-closed keys alone, taking no other reading. This is the seam
# the suite drives: it is the same function probe calls, so what is tested is what runs.
livelock)
    livelock_keys
    ;;
# The duplicate-ref meter keys alone, taking no other reading. This is the seam the suite
# drives: it is the same function probe calls, so what is tested is what runs.
dup_refs)
    dup_refs_keys
    ;;
# Fast-tier keys only — the seam collect.sh drives on every 5s tick.
now)
    now_keys
    ;;
# Medium-tier keys only — counts that need to stay fresh every 60s.
core)
    core_detail_keys
    core_counts_keys
    ;;
# Slow-tier core detail — SP_NEXT* listing, event feed, sparklines, token meters.
core_detail)
    core_detail_keys
    ;;
# Slow-tier keys only — the seam collect.sh drives on each 600s tick.
unsent)
    unsent_keys
    ;;
# The statute projection keys alone, taking no other reading. This is the seam the suite drives:
# it is the same function probe calls, so what is tested is what runs.
statute)
    statute_keys
    ;;
*) echo "usage: cockpit.sh [once|loop|history|now|core|core_detail|unsent|strands|sops|ratelim|sphere|repo_labels|livelock|dup_refs|statute]" >&2
   echo "  (no args: collect once then attach to the concierge; SPIRA_COCKPIT_NO_ATTACH=1 skips the attach)" >&2
   exit 1 ;;
esac
