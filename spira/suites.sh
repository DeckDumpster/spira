#!/usr/bin/env bash
#
# suites.sh — enumerate every spira/test-*.sh, report what the gate does not cover, and
# manage quarantine.
#
#   suites.sh list           every spira/test-*.sh, where it runs, and its last result
#   suites.sh names          the suites the gate does not run, one per line
#   suites.sh corpus         every non-disabled suite, for queue-PR CI
#   suites.sh status         the compact block the watchtower puts on the sweep
#   suites.sh hygiene        quarantine reactivation and max-age mail
#   suites.sh observe-flake  called by the gate/verdict on a red suite; files past threshold
#   suites.sh quarantine|disable|activate   suite-state transitions, run by hand
#
# The background timed sweep that once ran every gate-uncovered suite on an hourly timer
# (`suites.sh run`, spira-suites.timer) is retired outright (sp-b99nj) — not repaired. It
# spent most of a day filing incidents misdiagnosed as unrelated faults before anyone noticed
# the actual cause was a stray untracked suite, and the mechanism was already suspended
# in run/control when that happened. What remains here has other live callers (the gate
# calls `observe-flake` on every red; the watchtower reads `status`) and stays.
#
# DISCOVERY IS A GLOB, NEVER A LIST. `spira/test-*.sh` is the whole population. A list is the
# thing that just failed, so a new suite is found by existing, and a deleted one drops out
# with no edit anywhere. The ONE hand-written list is the gate's, in `gate-suites`, and `list`/
# `names`/`corpus` report its complement — so a suite dropped from the gate becomes visible
# here as uncovered rather than invisible.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/lib.sh"
. "$HERE/suite-state.sh"

STATE="${SPIRA_SUITES_STATE:-$SPIRA_RUN/suites}"
STALE="${SPIRA_SUITES_STALE:-21600}"
PRIORITY="${SPIRA_SUITES_PRIORITY:-2}"
GATE_LIST="${SPIRA_GATE_SUITES:-$HERE/gate-suites}"
INC="${SPIRA_INCIDENT:-$HERE/incident.sh}"

# --------------------------------------------------------------------------------------
# THE POPULATION, AND THE PARTITION OF IT.
# --------------------------------------------------------------------------------------
# all_suites -> every suite in the tree, basenames, sorted. The glob is the definition.
#
# `printf '%s\n' "$HERE"/test-*.sh` on a directory with no match yields the PATTERN itself,
# which would be reported as one suite named `test-*.sh` that cannot be read — so the
# existence of each is tested rather than assumed.
all_suites() {
    local f
    for f in "$HERE"/test-*.sh; do
        [ -f "$f" ] || continue
        basename "$f"
    done | sort
}

# gated_suites -> the basenames gate-suites names. Empty when the file is unreadable, and
# the caller must treat that as "unknown", never as "the gate runs nothing" — reading it as
# nothing would put every gated suite into the timed run and double the cost of a pass while
# reporting that it had found more work to do.
gated_suites() {
    local line
    [ -r "$GATE_LIST" ] || return 1
    while IFS= read -r line || [ -n "$line" ]; do
        line="${line%%#*}"
        line="${line#"${line%%[![:space:]]*}"}"; line="${line%"${line##*[![:space:]]}"}"
        [ -n "$line" ] || continue
        basename "$line"
    done < "$GATE_LIST" | sort
}

# is_gated <basename> -> 0 when the gate already runs it
is_gated() { case " $GATED " in *" $1 "*) return 0 ;; *) return 1 ;; esac; }


# priority_of <basename> -> the priority a red from this suite is filed at.
#
# THE SUITE DECLARES IT, with `# priority: N` beside its `# covers:` line, and the default is
# routine. How urgent a failure in some code is, is a claim about that code, and the only
# place that claim can live without becoming a second list to keep in step with the glob is
# in the suite itself. A malformed value is ignored rather than passed to `bd`, which would
# fail the create and lose the finding to a typo.
priority_of() {
    local p; p="$(sed -n 's/^# *priority: *//p' "$HERE/$1" 2>/dev/null | head -1 | tr -d '[:space:]')"
    case "$p" in [0-4]) printf '%s' "$p" ;; *) printf '%s' "$PRIORITY" ;; esac
}

# --------------------------------------------------------------------------------------
# THE RECORD. One file per suite: `<status> <epoch> <seconds> <fingerprint>`, left behind
# by a suite's last real run (through the gate, or by hand). `status`/`list` report a record
# older than SPIRA_SUITES_STALE as unrun rather than as green — which is what lets the pane
# render `?` instead of a zero.
# --------------------------------------------------------------------------------------
record_read() {          # record_read <basename> -> `<status> <epoch> <seconds> <fp>` or empty
    local f="$STATE/$1.result" st at secs fp
    [ -r "$f" ] || return 1
    # The trailing-status of `read` is ignored for the same reason the watchtower ignores it:
    # a file without a trailing newline populates every variable and then reports failure at
    # EOF. The guards below judge the CONTENT, which a truncated file cannot satisfy.
    read -r st at secs fp < "$f" 2>/dev/null || true
    [ -n "${st:-}" ] || return 1
    case "${at:-}" in ''|*[!0-9]*) return 1 ;; esac
    printf '%s %s %s %s' "$st" "$at" "${secs:-?}" "${fp:--}"
}

# --------------------------------------------------------------------------------------
# WHAT RUNS WHERE, AND WHAT IT LAST SAID. This is the answer to the question nobody could
# ask before: which suites in this tree are executed by nothing.
# --------------------------------------------------------------------------------------
cmd_list() {
    local s where rec st at secs age reach gated_ok=1 _sts_file _lifecycle
    GATED="$(gated_suites)" || gated_ok=0
    GATED=" $(echo ${GATED:-}) "
    _sts_file="$(suite_state_file "$(cd "$HERE/.." && pwd -P)")"
    printf '%-26s %-12s %-7s %-9s %-8s %-7s %s\n' SUITE STATE RUNS LAST AGE REACH COVERS
    for s in $(all_suites); do
        if [ "$gated_ok" = 0 ]; then where='?'
        elif is_gated "$s"; then where=gate
        else where=timed; fi
        _lifecycle="$(suite_state_of "$_sts_file" "$s")"
        rec="$(record_read "$s" || true)"
        if [ -n "$rec" ]; then
            read -r st at secs _ <<< "$rec"
            age="$(( ( $(date +%s) - at ) / 60 ))m"
        else
            st=-; age=-
        fi
        if [ "$where" = gate ]; then
            st=-; age=-; reach=-
        elif [ -f "$STATE/$s.unreached" ]; then
            reach="—"
        elif [ -n "$rec" ]; then
            reach=ok
        else
            reach=-
        fi
        printf '%-26s %-12s %-7s %-9s %-8s %-7s %s\n' "$s" "$_lifecycle" "$where" "$st" "$age" "$reach" \
            "$(suite_covers_of "$HERE/$s")"
    done
    [ "$gated_ok" = 1 ] || printf '\n%s is unreadable — which suites the gate runs is unknown\n' "$GATE_LIST"
}

# --------------------------------------------------------------------------------------
# THE BLOCK THE SWEEP CARRIES. Cheap by construction: a glob, a read per suite, no database
# and no subprocess that can hang, because the watchtower embeds it and the watchtower may
# not be another thing that is down during an outage.
#
# EVERY FIELD RENDERS `?` WHEN IT COULD NOT BE READ, never 0.
# --------------------------------------------------------------------------------------
cmd_status() {
    local s rec st at total=0 gate_n=0 timed_n=0 never=0 stale=0 red=0 skip=0 setup_fault=0 not_reached=0 oldest="" oldest_s="" now
    now="$(date +%s)"
    if ! GATED="$(gated_suites)"; then
        printf 'suites          ?   %s is unreadable — the gated set is unknown\n' "$GATE_LIST"
        return 0
    fi
    GATED=" $(echo $GATED) "
    for s in $(all_suites); do
        total=$(( total + 1 ))
        if is_gated "$s"; then gate_n=$(( gate_n + 1 )); continue; fi
        timed_n=$(( timed_n + 1 ))
        rec="$(record_read "$s" || true)"
        if [ -z "$rec" ]; then never=$(( never + 1 )); fi
        # A suite's last verdict survives an unreached pass — count red regardless of reach.
        if [ -n "$rec" ]; then
            read -r st at _ _ <<< "$rec"
            case "$st" in red|timeout|red-unconfirmed) red=$(( red + 1 )) ;; skip) skip=$(( skip + 1 )) ;; setup-fault|fixture-fault) setup_fault=$(( setup_fault + 1 )) ;; esac
            if [ "$(( now - at ))" -gt "$STALE" ]; then stale=$(( stale + 1 )); fi
            if [ -z "$oldest" ] || [ "$at" -lt "$oldest" ]; then oldest="$at"; oldest_s="$s"; fi
        fi
        # The .unreached file is distinct from a missing record (law-absence-needs-a-positive-control).
        [ -f "$STATE/$s.unreached" ] && not_reached=$(( not_reached + 1 ))
    done
    # Count suites without a # host-reason: or container calls — migration progress.
    # host-check.sh --count-undeclared does the walk; ? when it is unreadable or the glob
    # matches nothing (indistinguishable from a wrong path), never a silent 0.
    local undeclared="?" copying="?"
    if [ -x "$HERE/host-check.sh" ]; then
        undeclared="$(bash "$HERE/host-check.sh" --count-undeclared 2>/dev/null)" || undeclared="?"
        [ -n "$undeclared" ] || undeclared="?"
        # Wave-2 migration backlog: suites still copying harness files or creating inline stubs.
        # Falls monotonically as sp-841s child beads close; ? if unreadable.
        copying="$(bash "$HERE/host-check.sh" --count-copying 2>/dev/null)" || copying="?"
        [ -n "$copying" ] || copying="?"
    fi
    printf '  %-36s%s\n' "suites in the tree" "$total   ($gate_n gated, $timed_n timed)"
    printf '  %-36s%s\n' "host suites without # host-reason:" "$undeclared"
    printf '  %-36s%s\n' "suites still copying/stubbing (wave 2)" "$copying"
    printf '  %-36s%s\n' "timed suites with no result yet" "$never"
    printf '  %-36s%s\n' "timed results older than $(( STALE / 3600 ))h" "$stale"
    printf '  %-36s%s\n' "timed suites red at last run" "$red"
    printf '  %-36s%s\n' "timed suites setup/fixture fault at last run" "$setup_fault"
    printf '  %-36s%s\n' "timed suites skipped at last run" "$skip"
    printf '  %-36s%s\n' "timed suites not reached last pass" "$not_reached"
    if [ -n "$oldest" ]; then
        printf '  %-36s%sm   %s\n' "oldest timed result" "$(( (now - oldest) / 60 ))" "$oldest_s"
    else
        printf '  %-36s%s\n' "oldest timed result" "?   (nothing has run)"
    fi
}

# --------------------------------------------------------------------------------------
# cmd_names — THE SELECTOR. One suite name per line on stdout and nothing else, so it can be
# piped straight into a runner that takes a list (law-a-runner-takes-a-list):
#
#     suites.sh names | testenv-batch.sh --suites - <branch>
#
# Deciding WHICH suites run is a producer; RUNNING them is a runner. Keeping the two in one
# program is what left no way to name a single suite and made fixing one red cost 65 minutes.
#
# Empty output with exit 0 means "nothing to run" and is not an error.
# --------------------------------------------------------------------------------------
cmd_names() {
    local GATED s
    if ! GATED="$(gated_suites)"; then
        log "suites: $GATE_LIST is unreadable — refusing to guess which suites the gate runs"
        return 1
    fi
    GATED=" $(echo $GATED) "
    for s in $(all_suites); do
        is_gated "$s" && continue
        printf '%s\n' "$s"
    done
}

# cmd_corpus — every suite that is not disabled (active + quarantined), for queue-PR CI.
# Quarantined suites still run so a flake does not permanently escape the corpus.
# Disabled suites are excluded: they were deliberately removed from all automated runs.
cmd_corpus() {
    local sf s st
    sf="$(suite_state_file "$HERE/..")"
    for s in $(all_suites); do
        st="$(suite_state_of "$sf" "$s" 2>/dev/null)"
        [ "${st:-active}" = "disabled" ] && continue
        printf '%s\n' "$s"
    done
}

# --------------------------------------------------------------------------------------
# FLAKE OBSERVATIONS, CLEAN-RUN COUNTERS, AND QUARANTINE HYGIENE.
#
# Flake observations: per-suite files at $STATE/<suite>.flakeobs, one "<epoch> <run_id>"
# per line. observe-flake deduplicates on (suite, run_id); the count is distinct run_ids
# within SPIRA_FLAKE_WINDOW. Files a finding when count reaches SPIRA_FLAKE_QUARANTINE_AT —
# it never quarantines the suite itself; only `suites.sh quarantine`, run by an operator or
# Ops session with a bead in hand, writes suite-state.
#
# Clean-run counters and quarantine hygiene below apply to whatever a human quarantined by
# hand. Clean-run counters: $STATE/<suite>.clean-runs, meant to be incremented each time a
# quarantined suite runs green and reset on red — nothing currently calls cleanruns_inc()
# (its producer was the retired timed sweep; see sp-b99nj). cmd_hygiene still reactivates
# when the bead is LANDED and the count reaches SPIRA_QUARANTINE_CLEAN_RUNS, but until a
# new producer calls cleanruns_inc(), that count never advances on its own.
#
# Max-age mail: $STATE/<suite>.maxage-mailed is written after the first mail; its
# presence suppresses further mails for the same quarantine period.
# --------------------------------------------------------------------------------------
flakeobs_file()      { printf '%s/%s.flakeobs'      "$STATE" "$1"; }
cleanruns_file()     { printf '%s/%s.clean-runs'    "$STATE" "$1"; }
maxage_mailed_file() { printf '%s/%s.maxage-mailed' "$STATE" "$1"; }

flakeobs_record() {    # flakeobs_record <suite> <run_id>
    local suite="$1" run_id="$2"
    mkdir -p "$STATE" 2>/dev/null
    local f; f="$(flakeobs_file "$suite")"
    if [ -r "$f" ]; then
        local _ts _rid
        while IFS=' ' read -r _ts _rid || [ -n "$_ts" ]; do
            [ "${_rid:-}" = "$run_id" ] && return 0
        done < "$f"
    fi
    printf '%s %s\n' "$(date +%s)" "$run_id" >> "$f"
}

flakeobs_in_window() {  # flakeobs_in_window <suite> -> count of distinct runs in window
    local f; f="$(flakeobs_file "$1")"
    [ -r "$f" ] || { printf '0'; return 0; }
    local cutoff; cutoff=$(( $(date +%s) - ${SPIRA_FLAKE_WINDOW:-604800} ))
    local _ts _rid seen="" count=0
    while IFS=' ' read -r _ts _rid || [ -n "$_ts" ]; do
        case "${_ts:-}" in ''|*[!0-9]*) continue ;; esac
        [ "$_ts" -ge "$cutoff" ] || continue
        [ -n "${_rid:-}" ] || continue
        case ":${seen}:" in *":${_rid}:"*) continue ;; esac
        seen="${seen}:${_rid}"
        count=$(( count + 1 ))
    done < "$f"
    printf '%d' "$count"
}

cleanruns_get() {    # cleanruns_get <suite> -> count
    local n; n="$(cat "$(cleanruns_file "$1")" 2>/dev/null || echo 0)"
    case "$n" in ''|*[!0-9]*) n=0 ;; esac
    printf '%d' "$n"
}
cleanruns_inc()   { printf '%d\n' $(( $(cleanruns_get "$1") + 1 )) > "$(cleanruns_file "$1")"; }
cleanruns_reset() { mkdir -p "$STATE" 2>/dev/null; printf '0\n' > "$(cleanruns_file "$1")"; }

# file_flake <suite> <count> <window_s>
# Files a finding once a suite's flake observations cross the threshold, through incident.sh
# (same dedupe/recurrence/Sin machinery incident.sh gives every filer). Writes no suite-state,
# creates no branch, submits nothing to the queue: every automatic quarantine on record was
# either a real defect the quarantine hid or never examined at all
# (docs/spikes/test-identity-lifecycle-and-trust.md §4.2), so a flaky suite is reported and
# attributed, never silently taken out of the corpus.
file_flake() {    # file_flake <basename> <count> <window_s>
    local s="$1" count="$2" window="$3" id=""
    if [ ! -r "$INC" ]; then
        log "suites: no intake at $INC — $s flake finding reaches nobody"
        return 1
    fi
    local out_inc rc_inc
    out_inc="$(SPIRA_INCIDENT_TYPE=bug \
          SPIRA_INCIDENT_PRIORITY="$(priority_of "$s")" \
          SPIRA_INCIDENT_ACTOR=suites \
          SPIRA_INCIDENT_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan" \
          SPIRA_INCIDENT_REPO="$SPIRA_HOME_REPO" \
          SPIRA_INCIDENT_REF="flake:$s" \
          SPIRA_SIN_EXEMPT=1 \
          SPIRA_INCIDENT_PATH="$HERE/$s" \
          SPIRA_INCIDENT_CAUSE=suite-flaky \
          SPIRA_DB="$SPIRA_DB" \
          bash "$INC" file "why does $s fail intermittently" - <<PAYLOAD
$s has $count flake observation(s) within the ${window}s window. It is filed and nothing is
quarantined: the suite keeps running in every batch, so a repeat failure still reaches the
gate instead of being hidden.

  suite            $s
  observations     $count in ${window}s
  reproduce        bash spira/$s

The dedupe ref is flake:$s — a later observation bumps recurrence on this bead rather than
filing another.
PAYLOAD
)"; rc_inc=$?
    if [ "$rc_inc" -ne 0 ]; then
        log "suites: the intake could not file flake finding for $s — it stays spooled and drain will retry"
        return 1
    fi
    id="$(printf '%s\n' "$out_inc" | tail -n 1 | tr -d '[:space:]')"
    case "${id:-}" in
        ''|*[!A-Za-z0-9-]*|-*|*-) log "suites: the intake returned no id for $s flake finding"; return 1 ;;
    esac
    printf '%s' "$id"
}

# cmd_observe_flake — record one flake observation; report once threshold is reached.
cmd_observe_flake() {
    local suite="${1:-}" run_id="${2:-}"
    [ -n "$suite" ]  || { printf 'suites observe-flake: suite name required\n' >&2; return 2; }
    [ -n "$run_id" ] || { printf 'suites observe-flake: run id required\n' >&2; return 2; }
    [ -r "$HERE/$suite" ] || {
        printf 'suites observe-flake: no such suite: %s\n' "$suite" >&2
        return 2
    }
    flakeobs_record "$suite" "$run_id"
    local count threshold window
    count="$(flakeobs_in_window "$suite")"
    threshold="${SPIRA_FLAKE_QUARANTINE_AT:-2}"
    window="${SPIRA_FLAKE_WINDOW:-604800}"
    printf 'observe-flake: %s: %s observation(s) in window (threshold %s)\n' \
        "$suite" "$count" "$threshold"
    [ "$count" -ge "$threshold" ] || return 0
    local id
    id="$(file_flake "$suite" "$count" "$window")" && \
        printf 'observe-flake: %s reported (bead: %s)\n' "$suite" "$id" || \
        printf 'observe-flake: %s crossed threshold but the finding could not be filed\n' "$suite"
}

# cmd_hygiene — reactivation and max-age checks for all quarantined suites.
# Reactivates when: bead is LANDED and clean-run count >= SPIRA_QUARANTINE_CLEAN_RUNS.
# Mails once when: quarantine age >= SPIRA_QUARANTINE_MAX_AGE.
cmd_hygiene() {
    local repo; repo="$(cd "$HERE/.." && pwd -P)"
    local statefile; statefile="$(suite_state_file "$repo")"
    local BDCMD="${SPIRA_BD:-bd}"
    local clean_threshold="${SPIRA_QUARANTINE_CLEAN_RUNS:-10}"
    local max_age="${SPIRA_QUARANTINE_MAX_AGE:-604800}"
    local now; now="$(date +%s)"
    local activated=0 mailed=0
    while IFS=$'\t' read -r s st since bead _reason; do
        [ "$st" = "quarantined" ] || continue
        # -- reactivation --
        if [ -n "$bead" ]; then
            local land_state=""
            land_state="$("$BDCMD" -C "$SPIRA_DB" show "$bead" --json 2>/dev/null \
                | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
    d = d[0] if isinstance(d, list) else d
    lbls = d.get("labels") or []
    if "land_state:LANDED" in lbls:
        print("LANDED")
except Exception:
    pass
' 2>/dev/null)" || land_state=""
            if [ "${land_state:-}" = "LANDED" ]; then
                local cr; cr="$(cleanruns_get "$s")"
                if [ "$cr" -ge "$clean_threshold" ]; then
                    suite_state_clear "$statefile" "$s"
                    rm -f "$(cleanruns_file "$s")" "$(maxage_mailed_file "$s")" 2>/dev/null || true
                    printf 'hygiene: %s reactivated (bead %s LANDED, %d clean runs)\n' \
                        "$s" "$bead" "$cr"
                    printf '## Note\n%s was reactivated after LANDING with %d consecutive clean runs.\n' \
                        "$s" "$cr" \
                    | bash "$HERE/mail.sh" send operator \
                        --from "Suite hygiene <hygiene@spira>" \
                        --subject "$s reactivated" \
                        --bead "$bead" \
                        2>/dev/null || true
                    activated=$(( activated + 1 ))
                    continue
                fi
            fi
        fi
        # -- max-age --
        local since_epoch="" since_age mailed_flag
        mailed_flag="$(maxage_mailed_file "$s")"
        since_epoch="$(date -u -d "$since" +%s 2>/dev/null || true)"
        [ -n "${since_epoch:-}" ] || continue
        since_age=$(( now - since_epoch ))
        if [ "$since_age" -ge "$max_age" ] && [ ! -f "$mailed_flag" ]; then
            local _mail_args=(--from "Suite hygiene <hygiene@spira>" \
                --subject "$s quarantine exceeds $(( max_age / 86400 ))d")
            [ -n "${bead:-}" ] && _mail_args+=(--bead "$bead")
            printf 'quarantine for %s exceeds %d days.\n\nSince: %s\n' \
                "$s" $(( max_age / 86400 )) "$since" \
                | bash "$HERE/mail.sh" send operator "${_mail_args[@]}" \
                    2>/dev/null && touch "$mailed_flag" 2>/dev/null || true
            if [ -f "$mailed_flag" ]; then
                printf 'hygiene: mailed operator about %s (age %ds)\n' "$s" "$since_age"
                mailed=$(( mailed + 1 ))
            fi
        fi
    done < <(suite_state_parse "$statefile" 2>/dev/null)
    printf 'hygiene: %d reactivated, %d max-age mailed\n' "$activated" "$mailed"
}

# --------------------------------------------------------------------------------------
# LIFECYCLE TRANSITIONS. Each command creates a branch, writes the state file,
# commits, and prints the branch name. Pushing is handled by queue.sh submit
# (a later bead). All three validate the suite exists and the reason is given.
# --------------------------------------------------------------------------------------
_sts_transition() {  # _sts_transition <state> <suite> [<bead>] [<reason>]
    local state="$1" suite="${2:-}" bead="${3:-}" reason="${4:-}"
    [ -z "${SPIRA_AEON:-}" ] || {
        printf 'suites %s: aeons may not write suite-state transitions; submit a branch from an operator or Ops session\n' "$state" >&2
        return 1
    }
    local repo; repo="$(cd "$HERE/.." && pwd -P)"
    local statefile; statefile="$(suite_state_file "$repo")"
    [ -n "$suite" ] || { printf 'suites %s: suite name required\n' "$state" >&2; return 2; }
    [ -r "$HERE/$suite" ] || { printf 'suites %s: no such suite: %s\n' "$state" "$suite" >&2; return 2; }
    if [ "$state" = "quarantined" ]; then
        [ -n "$bead" ] || { printf 'suites quarantine: bead id required\n' >&2; return 2; }
    fi
    if [ "$state" != "active" ]; then
        [ -n "$reason" ] || { printf 'suites %s: reason required\n' "$state" >&2; return 2; }
    fi
    local stamp; stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    local branch="spira-suite-state/${suite%.sh}-${stamp}"
    git -C "$repo" checkout -b "$branch" >/dev/null 2>&1 || {
        printf 'suites %s: cannot create branch %s\n' "$state" "$branch" >&2; return 1
    }
    if [ "$state" = "active" ]; then
        suite_state_clear "$statefile" "$suite" || {
            git -C "$repo" checkout - >/dev/null 2>&1 || true
            printf 'suites activate: failed to update state file\n' >&2; return 1
        }
    else
        suite_state_write "$statefile" "$suite" "$state" "$bead" "$reason" || {
            git -C "$repo" checkout - >/dev/null 2>&1 || true
            printf 'suites %s: failed to update state file\n' "$state" >&2; return 1
        }
    fi
    git -C "$repo" add "${SPIRA_SUITE_STATE_FILE:-spira/suite-state}" >/dev/null 2>&1
    git -C "$repo" -c "user.name=${SPIRA_GIT_NAME:-spira}" -c "user.email=${SPIRA_GIT_EMAIL:-spira@spira.invalid}" commit -m "suite-state: $suite -> $state  sp-emvlk" \
        --no-gpg-sign >/dev/null 2>&1 || {
        git -C "$repo" checkout - >/dev/null 2>&1 || true
        printf 'suites %s: commit failed\n' "$state" >&2; return 1
    }
    # Submit to the queue so the change lands with no further step.
    bash "$HERE/queue.sh" submit "$branch" >&2 || {
        printf 'suites %s: queue.sh submit failed for %s — branch exists but is not certified\n' \
            "$state" "$branch" >&2
        return 1
    }
    printf '%s\n' "$branch"
}

cmd_quarantine() { _sts_transition quarantined "$@"; }
cmd_disable()    { _sts_transition disabled    "$1" "" "${2:-}"; }
cmd_activate()   { _sts_transition active      "$1"; }

# Sourced by a T1 suite to unit-test the pure functions above without paying for a container
# or a real intake — the dispatcher below must not fire in that case, or sourcing this file
# would run whatever command the sourcing script's own $1 happens to hold.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
case "${1:-list}" in
    names)          cmd_names ;;
    corpus)         cmd_corpus ;;
    list)           cmd_list ;;
    status)         cmd_status ;;
    hygiene)        cmd_hygiene ;;
    observe-flake)  shift; cmd_observe_flake "$@" ;;
    quarantine)     shift; cmd_quarantine "$@" ;;
    disable)        shift; cmd_disable    "$@" ;;
    activate)       shift; cmd_activate   "$@" ;;
    *) printf 'usage: suites.sh [list|names|corpus|status|hygiene|observe-flake|quarantine|disable|activate]\n' >&2; exit 2 ;;
esac
fi
