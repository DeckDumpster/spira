#!/usr/bin/env bash
#
# auron.sh — the watchdog over the loop. Its only power is speech.
#
#   auron.sh            one pass (this is what the timer runs)
#   auron.sh --report   classify and print, write nothing
#
# (per the operator, 2026-09-06: "a true livelock situation needs a higher-level sentinel
# whose responsibility is ONLY to escalate the situation.")
#
# WHY IT EXISTS. law-file-it-and-let-the-loop-fix-it says a harness defect is filed and
# fixed by the loop, never by hand — including a fault that stops the loop. That is only
# honest if something notices the loop has stopped and says so. Without this, the statute's
# one hard case resolves to silence.
#
# THE SOLE POWER IS SPEECH, AND THE RESTRICTION IS THE DESIGN. It escalates. It does not
# repair, restart, reclaim, land, kill or summon anything, and it must not grow the ability
# to. A watchdog that can act is a second controller with no supervisor of its own, and the
# first thing it will do wrong is fight the sentinel over the same resource. Auron in FFX is
# the guardian who tells the truth about the pilgrimage and never walks it in the pilgrim's
# place; the name is the reminder.
#
# IT MUST NOT BE ABLE TO LIVELOCK, and that constraint dictates everything below. No
# inference. No git. No worktrees, no repo checkouts, no gate, no test run, no fetch. It
# reads timestamps and counters out of files, asks the database one question, and compares
# what it found to thresholds. Every external call carries a short timeout, and the unit
# carries a RuntimeMaxSec: a pass that cannot finish is killed, and a killed pass stops
# writing the heartbeat, which is the one symptom that must never be silent.
#
# ITS OWN DEATH IS VISIBLE, BY CONSTRUCTION. A silent escalator and a healthy system look
# identical, and the pane would render the healthy reading, which is the failure
# law-absence-needs-a-positive-control is named for. So the heartbeat is written on EVERY
# run including a failing one, and the ops pane shows its age and marks it stale rather
# than omitting it (cockpit.sh SP_AURON_*, health.sh).
#
# WHERE THE ALERT GOES, AND IN WHICH ORDER
# ----------------------------------------
# (corrected per the operator, 2026-09-06: "auron should write to beads — a dolt issue is
# not the only thing that could cause a livelock. if necessary, it may also write to a
# special sentinel file ... but that should be secondary.")
#
#   1. BEADS IS THE RECORD. An alert is one bead per CAUSE, type `event`, labelled
#      `alert`, `overseer` and `alert:<key>`. It gets identity, status, a description that
#      is regenerated whole, and a lifecycle the attention pane can read.
#   2. THE FALLBACK FILE IS SECONDARY, written if and only if the beads path failed —
#      which is exactly the case where beads itself is the casualty. Its EXISTENCE is
#      therefore meaningful: it says the database could not be reached. It is removed the
#      moment beads answers again, so there is never a second source of truth standing
#      beside a working first one.
#
# An earlier draft had this backwards, reasoning from one failure mode (Dolt down) and
# making the rare case the default path — which would have put every ordinary alert
# through a channel with no history, no id and no lifecycle. Most livelocks (a wedged
# pass, summon starvation, an unreclaimed lease) leave the database perfectly healthy.
#
# NOT `needs-ryan`, DELIBERATELY, for two reasons. That label means a decision only the
# operator can make and is excluded from every fayth's predicate; an alert is a CONDITION,
# not a decision. And brain-guard requires a needs-ryan bead to be closed through
# resolve.sh, which would stop Auron clearing its own alert when the condition passes.
#
# THE LABELS ARE A CONTRACT WITH THE ATTENTION PANE, not decoration. The ALERTS tab selects
# on `alert` AND `overseer`, reads the flap count out of `flaps:<n>`, and owns two labels
# this program must respect rather than write:
#
#   acked          the operator has SEEN it. It says nothing about the condition, which is
#                  why an acked alert stays on the tab. Auron removes it when the condition
#                  RETURNS, because a fresh occurrence is not one anybody has looked at.
#   silent-until:  hidden until an instant carried in the label itself, so the silence
#                  expires with no process running. Auron never writes it and never removes
#                  it: it is the pane's affordance and fighting over it would make a
#                  silence the operator asked for evaporate on the next pass.
#
# Nothing in the pane may CLOSE an alert — retracting the statement belongs to whatever
# asserted it — but a close from a terminal is still possible, and is handled below as an
# acknowledgement rather than argued with.
#
# AURON CLOSES ITS OWN ALERTS. That is still only speech — retracting a statement, not
# acting on the system — and it is what makes the tab self-clearing. It does NOT reopen an
# alert the operator closed by hand while the condition still holds: that close is an
# acknowledgement, and re-raising it would be a machine arguing with a human.
#
# NOISE IS THE FAILURE MODE. A watchdog that cries during every ordinary long landing gets
# ignored, and then it is worse than nothing (law-alerts-must-be-actionable). Three
# defences, in order of how much they matter: thresholds measured from the real pass log
# rather than guessed (see auron-classify.py for the numbers); a condition must hold for
# CONFIRM consecutive runs before it fires, and clear for CLEAR consecutive runs before it
# is retracted, so nothing flaps on one sample; and one bead per cause forever, reopened
# and flap-counted, because ten beads for one cause is how a pane becomes wallpaper.
set -uo pipefail
# THE CLASSIFIER IS A SIBLING FILE, so it is addressed from where THIS file sits rather
# than through SPIRA_HOME, which the environment may point at a different copy of the
# harness. strand.sh resolves strand-classify.py the same way and for the same reason.
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/lib.sh"

REPORT=0; [ "${1:-}" = "--report" ] && REPORT=1

# THE DATABASE GETS TEN SECONDS, NOT THREE MINUTES. bdq's default BD_TIMEOUT is 180s,
# which is right for the sentinel and wrong here: a bd that has not answered in ten
# seconds IS "the database is unreachable" as far as a watchdog is concerned, and waiting
# out the default would make Auron itself the thing that hangs.
BD_TIMEOUT="${SPIRA_AURON_BD_TIMEOUT:-10}"

STATE="$SPIRA_RUN/auron.state"
STATUS="$SPIRA_RUN/auron.status"
FALLBACK="${SPIRA_AURON_FALLBACK:-$SPIRA_RUN/auron.alerts.json}"
SENTINEL_LOG="${SPIRA_AURON_SENTINEL_LOG:-$SPIRA_RUN/sentinel.log}"
STRANDS="${SPIRA_AURON_STRANDS:-$SPIRA_RUN/strands.json}"
# The mirror this harness writes, derived exactly as export-beads.sh derives it so the two
# cannot disagree. Empty SPIRA_EXPORTER means no exporter is configured on this box and
# the mirror rule is skipped entirely — a colleague who does not mirror has nothing here
# to be stale.
MIRROR="${SPIRA_AURON_MIRROR:-$SPIRA_REPO/raw/spira-beads/spira.jsonl}"
# How much of the sentinel log to read. Bounded, because an unbounded read of a file that
# grows forever is the one way a program this simple could still become slow. Measured
# 2026-09-06: ~1.1KB per pass, so this covers ~230 of them against a rule that needs 5.
TAIL_BYTES="${SPIRA_AURON_TAIL_BYTES:-262144}"
CONFIRM="${SPIRA_AURON_CONFIRM:-2}"        # runs a condition must hold before it fires
CLEAR="${SPIRA_AURON_CLEAR:-2}"            # runs it must be absent before it is retracted
REFRESH="${SPIRA_AURON_REFRESH:-3600}"     # how often a standing alert's evidence is rewritten
SYSTEMCTL="${SPIRA_SYSTEMCTL:-systemctl}"

NOW="$(date +%s)"

# ======================================================================================
# STATE. TSV, not JSON, because nothing else reads it and bash can parse it without a
# subprocess: one line per condition, plus a `#first_run` header.
#
# EVERY FIELD HERE EXCEPT `flaps` IS A CACHE. The bead id is re-derived from the database
# by label on every run, so a lost state file costs the flap count and nothing else — the
# alert is still found, still updated, still closed. A flap count cannot be recomputed
# from the graph, which is the one reason this file exists at all rather than being
# derived like strand.sh's episode state.
# ======================================================================================
declare -A S_STATE S_SEEN S_UNSEEN S_SINCE S_FIRST S_FLAPS S_BEAD S_REFRESHED
# Per-unit restart baseline: R_BASELINE[unit]=count at window start,
# R_BASELINE_AT[unit]=epoch of window start, R_LAST[unit]=count from previous pass.
declare -A R_BASELINE R_BASELINE_AT R_LAST
FIRST_RUN=0
PROBE_ID=""    # id of the write-probe bead; re-derived if missing, persisted in state
if [ -r "$STATE" ]; then
    while IFS=$'\t' read -r k a b c d e f g h; do
        case "$k" in
            '#first_run') FIRST_RUN="${a:-0}"; continue ;;
            '#probe_id')  PROBE_ID="${a:-}"; continue ;;
            '#restart')   R_BASELINE[$a]="${b:-0}"; R_BASELINE_AT[$a]="${c:-$NOW}"; R_LAST[$a]="${d:-0}"; continue ;;
            ''|'#'*)      continue ;;
        esac
        S_STATE[$k]="${a:-clear}";  S_SEEN[$k]="${b:-0}";      S_UNSEEN[$k]="${c:-0}"
        S_SINCE[$k]="${d:-0}";      S_FIRST[$k]="${e:-0}";     S_FLAPS[$k]="${f:-0}"
        S_BEAD[$k]="${g:-}";        S_REFRESHED[$k]="${h:-0}"
    done < "$STATE"
fi
# THE FIRST-RUN FLOOR. Auron must not fire about a stall that predates its own existence:
# on a box where it is installed after the fact, the log's history is not evidence it was
# there to observe. The floor is written once and never again.
[ "$FIRST_RUN" = 0 ] && FIRST_RUN="$NOW"

state_save() {
    local tmp="$STATE.tmp.$$" k
    { printf '#first_run\t%s\n' "$FIRST_RUN"
      printf '#probe_id\t%s\n' "$PROBE_ID"
      for k in "${!S_STATE[@]}"; do
          printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$k" \
              "${S_STATE[$k]}" "${S_SEEN[$k]}" "${S_UNSEEN[$k]}" "${S_SINCE[$k]}" \
              "${S_FIRST[$k]}" "${S_FLAPS[$k]}" "${S_BEAD[$k]}" "${S_REFRESHED[$k]}"
      done
      for k in "${!R_LAST[@]}"; do
          printf '#restart\t%s\t%s\t%s\t%s\n' "$k" \
              "${R_BASELINE[$k]:-0}" "${R_BASELINE_AT[$k]:-$NOW}" "${R_LAST[$k]:-0}"
      done
    } > "$tmp" 2>/dev/null && mv -f "$tmp" "$STATE"
}

# ======================================================================================
# GATHER. File reads and one database question, and nothing else. Every value that could
# not be read is reported AS unreadable rather than as a benign default, because a probe
# that fails quietly is indistinguishable from a healthy reading.
# ======================================================================================
LOG_TAIL="$SPIRA_RUN/.auron-logtail.$$"
trap 'rm -f "$LOG_TAIL" "$LOG_TAIL.obs" "$LOG_TAIL.body"' EXIT

log_readable=1; log_error=""; log_mtime=0
if [ -r "$SENTINEL_LOG" ]; then
    log_mtime="$(stat -c %Y "$SENTINEL_LOG" 2>/dev/null || echo 0)"
    tail -c "$TAIL_BYTES" "$SENTINEL_LOG" > "$LOG_TAIL" 2>/dev/null || {
        log_readable=0; log_error="tail failed"; }
else
    log_readable=0; : > "$LOG_TAIL"
    [ -e "$SENTINEL_LOG" ] && log_error="exists but is not readable" || log_error="no such file"
fi

_sentinel_unit="$(spira_unit sentinel timer)"
if [ "$_sentinel_unit" = '?' ]; then
    sentinel_timer=unknown
else
    sentinel_timer="$("$SYSTEMCTL" --user is-active "$_sentinel_unit" 2>/dev/null)"
    [ -n "$sentinel_timer" ] || sentinel_timer=unknown
fi
unset _sentinel_unit

mirror_configured=0; mirror_exists=0; mirror_mtime=0
if [ -n "${SPIRA_EXPORTER:-}" ]; then
    mirror_configured=1
    if [ -r "$MIRROR" ]; then
        mirror_exists=1; mirror_mtime="$(stat -c %Y "$MIRROR" 2>/dev/null || echo 0)"
    fi
fi

# UNIT RESTART COUNTS. One `systemctl show` call reads NRestarts for every loaded Spira
# service. No systemd answer is not a fault — a stub, a timeout, or a non-systemd box all
# produce empty output and the check is skipped. Short timeout: a systemd that does not
# answer in 5s is not available as far as a watchdog pass is concerned.
RESTARTS_THRESHOLD="${SPIRA_AURON_RESTARTS:-5}"
RESTART_WINDOW_S="${SPIRA_AURON_RESTART_WINDOW:-3600}"
declare -A CUR_RESTARTS
_sc_unit_list=""
_raw_sc_units="$(timeout 5 "$SYSTEMCTL" --user list-units \
    --type=service --all --plain --no-legend 2>/dev/null || true)"
if [ -n "$_raw_sc_units" ]; then
    _sc_unit_list="$(printf '%s' "$_raw_sc_units" \
        | awk '$1 ~ /^spira-/ {print $1}' | tr '\n' ' ')"
    _sc_unit_list="${_sc_unit_list% }"
fi
if [ -n "$_sc_unit_list" ]; then
    _raw_sc_show="$(timeout 5 "$SYSTEMCTL" --user show \
        --property=Id,NRestarts -- $_sc_unit_list 2>/dev/null || true)"
    if [ -n "$_raw_sc_show" ]; then
        _rid=""
        while IFS= read -r _rline; do
            case "$_rline" in
                Id=*)        _rid="${_rline#Id=}" ;;
                NRestarts=*) [ -n "$_rid" ] && CUR_RESTARTS["$_rid"]="${_rline#NRestarts=}"; _rid="" ;;
                '')          _rid="" ;;
            esac
        done <<< "$_raw_sc_show"
    fi
    unset _raw_sc_show
fi
unset _sc_unit_list _raw_sc_units _rid _rline

# Compute deltas, update baselines, and build the restart alerts JSON (one call per pass).
# A count that went DOWN (daemon-reload, reinstall) resets the baseline — not an alert.
# A window that expired slides forward so stale data does not keep a unit in alert.
_ra_tsv=""
for _u in "${!CUR_RESTARTS[@]}"; do
    _cur="${CUR_RESTARTS[$_u]}"
    _base="${R_BASELINE[$_u]:-$_cur}"
    _base_at="${R_BASELINE_AT[$_u]:-$NOW}"
    _last="${R_LAST[$_u]:-$_cur}"

    if [ "$_cur" -lt "$_last" ] 2>/dev/null; then
        _base="$_cur"; _base_at="$NOW"
    elif [ "$(( NOW - _base_at ))" -ge "$RESTART_WINDOW_S" ]; then
        _base="$_cur"; _base_at="$NOW"
    fi
    R_BASELINE[$_u]="$_base"; R_BASELINE_AT[$_u]="$_base_at"; R_LAST[$_u]="$_cur"

    _rdelta="$(( _cur - _base ))"
    if [ "$_rdelta" -gt "$RESTARTS_THRESHOLD" ] 2>/dev/null; then
        _rjb64="$(timeout 5 journalctl --user --no-pager -n 10 --unit="$_u" 2>/dev/null \
            | python3 -c 'import sys,base64; print(base64.b64encode(sys.stdin.buffer.read()).decode())' \
            || true)"
        _ra_tsv="${_ra_tsv}${_u}	${_cur}	${_base}	${_rdelta}	${RESTART_WINDOW_S}	${_rjb64}
"
    fi
done
unset _u _cur _base _base_at _last _rdelta _rjb64

_restart_alerts_json="$(printf '%s' "$_ra_tsv" | python3 -c '
import sys, json, base64
alerts = []
for line in sys.stdin:
    line = line.rstrip("\n")
    if not line:
        continue
    parts = line.split("\t", 5)
    if len(parts) < 5:
        continue
    u, cur, base, delta, window = parts[0], int(parts[1]), int(parts[2]), int(parts[3]), int(parts[4])
    jb64 = parts[5] if len(parts) > 5 else ""
    try:
        journal = base64.b64decode(jb64).decode(errors="replace")
    except Exception:
        journal = ""
    alerts.append({"unit": u, "current": cur, "baseline": base, "delta": delta,
                   "window": window, "journal": journal})
print(json.dumps(alerts))' 2>/dev/null || printf '[]')"
unset _ra_tsv

# THE ALERT QUERY IS ALSO THE READ PROBE, and deliberately so. Auron needs the open and
# closed alert beads on every run anyway — that is how one bead per cause is enforced
# across a lost state file — so asking a second, separate "is the database readable"
# question would be a second thing to get out of step with the first. An empty list is
# trustworthy only because this same call is what proved the database could answer.
#
# THE EXIT STATUS DISTINGUISHES CONTENTION FROM FAILURE. A call that times out (exit 124
# from `timeout`) means another dolt process held the lock longer than BD_TIMEOUT; a call
# that errors means the database is actually unreachable. Conflating them made half of all
# passes classify the database as DOWN when it was merely busy.
db_saturated=0    # 1 if any bd call this pass timed out (lock contention, not unavailability)
_read_rc=0
_alerts_json="$(bdq list --all --limit 0 --label alert --json 2>/dev/null)" || _read_rc=$?
alerts_raw="$(printf '%s' "$_alerts_json" | json_only)"
unset _alerts_json
db_reachable=1; db_error=""
if [ -z "$alerts_raw" ]; then
    db_reachable=0
    if [ "$_read_rc" = 124 ]; then
        db_saturated=1
        db_error="bd list --label alert timed out after ${BD_TIMEOUT}s (lock contention)"
    else
        db_error="bd list --label alert returned nothing parseable within ${BD_TIMEOUT}s"
    fi
fi
unset _read_rc

# THE WRITE PROBE. db_reachable proves the READ path is up, not the write path. When
# writes fail but reads do not — for example when a schema-cursor rollback leaves the
# binary seeing pending migrations that a remote-backed database will not auto-apply —
# bd list returns normally while every alert_write silently fails. That failure was
# invisible until something was already firing (law-measure-the-outcome applied to the
# watchdog itself). This probe exercises the write path unconditionally on every pass,
# so a broken write path is seen on the very next cycle rather than only at alert time.
#
# The probe bead is updated, not created-and-deleted, because creation churns the graph
# and the database is mirrored to git. A bead Auron already owns costs one row update
# per pass and leaves no debris. PROBE_ID is persisted in the state file; a failed
# update clears it so the next pass re-derives and retries via create.
#
# FIVE PROPERTIES THE PROBE MUST HOLD:
#   1. A failed re-derive does not create. The failure that clears PROBE_ID (a failed
#      update) and the failure that makes re-derive return empty (lock contention) are
#      strongly correlated, so the pair reliably produced a leaked P0 bead per episode.
#   2. Create is idempotent: the probe re-queries before creating and adopts any existing
#      bead rather than adding a duplicate.
#   3. Extras are closed on adoption: one probe bead is the invariant; when re-derive or
#      the pre-create check finds multiples, all but the first are closed immediately.
#   4. A create that follows a failed update is a recovery, not a healthy write. The pass
#      that created a replacement bead reported db_write_ok=1 and hid the fault.
#   5. A timeout is contention, not unavailability. Exit code 124 from `timeout` is
#      reported as saturated rather than down so the two failure modes are distinguishable.
db_write_ok="?"    # unknown until attempted; ? published in the heartbeat if read failed
if [ "$db_reachable" = 1 ]; then
    _probe_had="${PROBE_ID:-}"   # non-empty means an update succeeded before; a create now
                                  # is a recovery (Fix 4), not a first-run healthy write
    # Re-derive PROBE_ID if the state lost it (first pass, state cleared, prior failure).
    if [ -z "$PROBE_ID" ]; then
        _probe_rc=0
        _probe_json="$(bdq list --all --limit 0 --label auron:probe --json 2>/dev/null)" \
            || _probe_rc=$?
        if [ "$_probe_rc" = 0 ]; then
            PROBE_ID="$(printf '%s' "$_probe_json" | json_only | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: d = []
items = d if isinstance(d, list) else ([d] if d else [])
print((items[0] if items else {}).get("id", ""))' 2>/dev/null)"
            # CLOSE EXTRAS. One probe bead is the invariant; close all but the first
            # immediately rather than waiting for the groomer, whose cadence is hours.
            _probe_extras="$(printf '%s' "$_probe_json" | json_only | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: d = []
items = d if isinstance(d, list) else ([d] if d else [])
for it in items[1:]: print(it.get("id", ""))' 2>/dev/null)"
            for _eid in $_probe_extras; do
                [ -n "$_eid" ] || continue
                bdq close "$_eid" --reason-file - <<'REASON' >/dev/null 2>&1 || true
Duplicate Auron write probe bead closed automatically.
The probe is a one-bead invariant; this bead was created during a transient contention
episode and superseded by an earlier probe that was re-derived on recovery.
REASON
            done
            unset _probe_extras _eid
        else
            # Failed re-derive: do not create. The failure that cleared PROBE_ID and the
            # failure that makes re-derive empty are strongly correlated (both are lock
            # contention); creating unconditionally here produced a leaked P0 per episode.
            [ "$_probe_rc" = 124 ] && db_saturated=1
            db_write_ok=0
        fi
        unset _probe_json _probe_rc
    fi

    # IDEMPOTENT BACKSTOP: re-query immediately before creating and adopt any existing probe
    # rather than adding a duplicate. Guards the narrow window between a successful empty
    # re-derive and the create, and cleans up any extras found here as well.
    if [ -z "$PROBE_ID" ] && [ "$db_write_ok" = "?" ]; then
        _pre_rc=0
        _pre_json="$(bdq list --all --limit 0 --label auron:probe --json 2>/dev/null)" \
            || _pre_rc=$?
        if [ "$_pre_rc" = 0 ]; then
            _adopt="$(printf '%s' "$_pre_json" | json_only | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: d = []
items = d if isinstance(d, list) else ([d] if d else [])
print((items[0] if items else {}).get("id", ""))' 2>/dev/null)"
            if [ -n "$_adopt" ]; then
                PROBE_ID="$_adopt"
                _pre_extras="$(printf '%s' "$_pre_json" | json_only | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: d = []
items = d if isinstance(d, list) else ([d] if d else [])
for it in items[1:]: print(it.get("id", ""))' 2>/dev/null)"
                for _eid in $_pre_extras; do
                    [ -n "$_eid" ] || continue
                    bdq close "$_eid" --reason-file - <<'REASON' >/dev/null 2>&1 || true
Duplicate Auron write probe bead closed automatically.
The probe is a one-bead invariant; this bead was created during a transient contention
episode and superseded by an earlier probe that was re-derived on recovery.
REASON
                done
                unset _pre_extras _eid
            fi
        fi
        unset _pre_json _pre_rc _adopt
    fi

    if [ -z "$PROBE_ID" ] && [ "$db_write_ok" = "?" ]; then
        # No probe bead found after re-derive and idempotent check: CREATE it.
        # The create is itself the write probe for this pass.
        probe_out="$(bdq create --title "Auron write probe" --type event -p 0 \
                       --labels "auron:probe,overseer" \
                       --body "write-path probe — updated on every Auron pass" \
                       --json 2>&1)"
        PROBE_ID="$(printf '%s' "$probe_out" | python3 -c '
import sys, json
t = sys.stdin.read(); i = t.find("{")
if i >= 0:
    try: print(json.loads(t[i:])["id"])
    except Exception: pass' 2>/dev/null)"
        if [ -n "$PROBE_ID" ]; then
            # A fresh install create is healthy; a create after a failed update (_probe_had
            # was non-empty) is a recovery — the write path was broken last pass.
            [ -n "$_probe_had" ] && db_write_ok=0 || db_write_ok=1
        else
            db_write_ok=0
        fi
    fi

    if [ -n "$PROBE_ID" ] && [ "$db_write_ok" = "?" ]; then
        # Probe bead exists: UPDATE its body with the current timestamp.
        _upd_rc=0
        bdq update "$PROBE_ID" --body "$(date +%s)" >/dev/null 2>&1 || _upd_rc=$?
        if [ "$_upd_rc" = 0 ]; then
            db_write_ok=1
        else
            # Clear PROBE_ID. A failed update could mean the bead was deleted or writes
            # are broken; the next pass will re-derive and discover which.
            [ "$_upd_rc" = 124 ] && db_saturated=1
            PROBE_ID=""; db_write_ok=0
        fi
        unset _upd_rc
    fi
    unset _probe_had
fi

# key -> id and key -> status, from the labels. Never from a title and never from a grep
# over human output (law-never-derive-an-id-from-output).
declare -A B_ID B_STATUS B_LABELS
if [ "$db_reachable" = 1 ]; then
    while IFS=$'\t' read -r k id st labels; do
        [ -n "$k" ] || continue
        B_ID[$k]="$id"; B_STATUS[$k]="$st"; B_LABELS[$k]="$labels"
    done < <(printf '%s' "$alerts_raw" | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: raise SystemExit
for i in (d if isinstance(d, list) else [d]):
    labels = i.get("labels") or []
    for l in labels:
        if l.startswith("alert:"):
            print("%s\t%s\t%s\t%s" % (l[6:], i["id"], i.get("status") or "", " ".join(labels)))
            break' 2>/dev/null)
fi

# ======================================================================================
# CLASSIFY. Everything gathered above, handed to a pure function that opens nothing.
# ======================================================================================
LOG_TAIL="$LOG_TAIL" NOW="$NOW" FIRST_RUN="$FIRST_RUN" \
LOG_READABLE="$log_readable" LOG_ERROR="$log_error" LOG_MTIME="$log_mtime" \
SENTINEL_LOG="$SENTINEL_LOG" SENTINEL_TIMER="$sentinel_timer" \
DB_REACHABLE="$db_reachable" DB_ERROR="$db_error" DB_PATH="$SPIRA_DB" FALLBACK="$FALLBACK" \
MIRROR="$MIRROR" MIRROR_CONFIGURED="$mirror_configured" MIRROR_EXISTS="$mirror_exists" \
MIRROR_MTIME="$mirror_mtime" EXPORTER="${SPIRA_EXPORTER:-}" STRANDS="$STRANDS" \
T_PASS="${SPIRA_AURON_PASS_STALE:-600}" T_STARVE="${SPIRA_AURON_STARVE_PASSES:-5}" \
T_MIRROR="${SPIRA_AURON_MIRROR_STALE:-90000}" T_GHOST="${SPIRA_AURON_GHOST_STALE:-1800}" \
RESTART_ALERTS="$_restart_alerts_json" \
T_RESTARTS="$RESTARTS_THRESHOLD" T_RESTART_WINDOW="$RESTART_WINDOW_S" \
python3 - > "$LOG_TAIL.obs" <<'PY'
import json, os, sys
def n(k, d=0):
    try: return int(os.environ.get(k) or d)
    except ValueError: return d
try:
    with open(os.environ["STRANDS"]) as fh: strands = json.load(fh)
except Exception:
    strands = {}
if not isinstance(strands, dict): strands = {}
try:
    with open(os.environ["LOG_TAIL"], errors="replace") as fh: text = fh.read()
except Exception:
    text = ""
try:
    restart_alerts = json.loads(os.environ.get("RESTART_ALERTS") or "[]")
    if not isinstance(restart_alerts, list): restart_alerts = []
except Exception:
    restart_alerts = []
json.dump({
    "now": n("NOW"), "auron_first": n("FIRST_RUN"),
    "sentinel_log": text,
    "sentinel_log_readable": os.environ.get("LOG_READABLE") == "1",
    "sentinel_log_error": os.environ.get("LOG_ERROR") or "",
    "sentinel_log_mtime": n("LOG_MTIME"),
    "sentinel_log_path": os.environ.get("SENTINEL_LOG") or "",
    "sentinel_timer": os.environ.get("SENTINEL_TIMER") or "unknown",
    "db_reachable": os.environ.get("DB_REACHABLE") == "1",
    "db_error": os.environ.get("DB_ERROR") or "",
    "db_path": os.environ.get("DB_PATH") or "",
    "fallback_path": os.environ.get("FALLBACK") or "",
    "mirror": {"configured": os.environ.get("MIRROR_CONFIGURED") == "1",
               "exists": os.environ.get("MIRROR_EXISTS") == "1",
               "mtime": n("MIRROR_MTIME"),
               "path": os.environ.get("MIRROR") or "",
               "exporter": os.environ.get("EXPORTER") or ""},
    "strands": strands,
    "restart_alerts": restart_alerts,
    "thresholds": {"pass_stale": n("T_PASS", 600), "starve_passes": n("T_STARVE", 5),
                   "mirror_stale": n("T_MIRROR", 90000), "ghost_stale": n("T_GHOST", 1800),
                   "restart_threshold": n("T_RESTARTS", 5),
                   "restart_window": n("T_RESTART_WINDOW", 3600)},
}, sys.stdout)
PY

firing_json="$(python3 "$HERE/auron-classify.py" < "$LOG_TAIL.obs" 2>/dev/null)"

declare -A F_TITLE
firing_keys=""
while IFS=$'\t' read -r k title; do
    [ -n "$k" ] || continue
    firing_keys="$firing_keys $k"
    F_TITLE[$k]="$title"
done < <(printf '%s' "$firing_json" | python3 -c '
import sys, json
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    try: a = json.loads(line)
    except Exception: continue
    print("%s\t%s" % (a["key"], a["title"].replace("\t", " ")))' 2>/dev/null)

evidence_of() {   # evidence_of <key> -> its evidence block on stdout
    printf '%s' "$firing_json" | KEY="$1" python3 -c '
import sys, os, json
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    try: a = json.loads(line)
    except Exception: continue
    if a["key"] == os.environ["KEY"]:
        sys.stdout.write(a["evidence"]); break'
}

if [ "$REPORT" = 1 ]; then
    if [ -z "${firing_keys// /}" ]; then
        printf 'auron: nothing firing (sentinel timer %s, db read: %s write: %s)\n' \
            "$sentinel_timer" \
            "$([ "$db_reachable" = 1 ] && echo ok || echo UNREACHABLE)" \
            "$(case "$db_write_ok" in 1) echo ok ;; 0) echo FAILED ;; *) echo unknown ;; esac)"
    else
        for k in $firing_keys; do
            printf '\n=== %s ===\n%s\n\n%s\n' "$k" "${F_TITLE[$k]}" "$(evidence_of "$k")"
        done
    fi
    exit 0
fi

# ======================================================================================
# RECONCILE. The only writes this program makes.
# ======================================================================================
iso() { TZ="${SPIRA_TZ:-UTC}" date -d "@$1" '+%Y-%m-%d %H:%M %Z' 2>/dev/null || echo "@$1"; }

# The description is REGENERATED WHOLE on every write, never patched
# (law-regenerate-derived-summaries). A patched alert body accumulates the readings that
# were true at each past firing, and the reader cannot tell which one is now.
body_of() {   # body_of <key>
    local k="$1"
    printf '**Condition:** `%s`\n' "$k"
    printf '**First seen:** %s  ·  **Fired:** %s time(s)\n\n' \
        "$(iso "${S_FIRST[$k]:-$NOW}")" "${S_FLAPS[$k]:-1}"
    printf '```\n%s\n```\n\n' "$(evidence_of "$k")"
    # NO MARKDOWN IN THE TRAILER. It is read in a tmux pane and in `bd show`, neither of
    # which renders emphasis; a paragraph wrapped in underscores and backticks arrives as
    # underscores and backticks, broken across the wrap.
    printf 'Raised by Auron (spira/auron.sh), which watches the loop and only\n'
    printf 'speaks - it repairs nothing. It closes this itself when the condition passes,\n'
    printf 'so nothing is owed. Closing it by hand acknowledges it: Auron will not raise\n'
    printf 'it again until the condition has cleared and returned.\n'
}

beads_ok=1        # every bead write this run succeeded
acted=0

alert_write() {   # alert_write <key> — create or update the bead. 0 on success.
    local k="$1" id="${B_ID[$k]:-}" st="${B_STATUS[$k]:-}" out l
    body_of "$k" > "$LOG_TAIL.body"
    if [ -z "$id" ]; then
        # THE ID COMES FROM `--json`. `bd create` prepends an advisory that echoes the
        # title before the id, so the first id-shaped token in human output is whatever
        # the title happens to contain (law-never-derive-an-id-from-output).
        out="$(bdq create --title "${F_TITLE[$k]}" --type event -p 1 \
                 --labels "alert,overseer,alert:$k,flaps:${S_FLAPS[$k]}" \
                 --body-file "$LOG_TAIL.body" --json 2>&1)"
        id="$(printf '%s' "$out" | python3 -c '
import sys, json
t = sys.stdin.read(); i = t.find("{")
if i >= 0:
    try: print(json.loads(t[i:])["id"])
    except Exception: pass' 2>/dev/null)"
        [ -n "$id" ] || { log "AURON could not create the alert bead for $k"; return 1; }
        B_ID[$k]="$id"; S_BEAD[$k]="$id"
        log "AURON raised $k as $id"
        return 0
    fi
    if [ "$st" = closed ]; then
        bead_reopen "$id" alert-recur "the condition returned"
    fi
    bdq update "$id" --title "${F_TITLE[$k]}" --body-file "$LOG_TAIL.body" >/dev/null 2>&1 || return 1
    # THE FLAP COUNT IS A LABEL, and a count is a fact with exactly one current value — so
    # every stale `flaps:` is removed rather than a new one added beside it. `--add-label`
    # alone would leave `flaps:1 flaps:2 flaps:3` and the pane reads the FIRST it finds.
    for l in ${B_LABELS[$k]:-}; do
        case "$l" in flaps:*) [ "$l" = "flaps:${S_FLAPS[$k]}" ] || bdq label remove "$id" "$l" >/dev/null 2>&1 ;; esac
    done
    bdq label add "$id" "flaps:${S_FLAPS[$k]}" >/dev/null 2>&1
    # A RETURNING CONDITION IS NOT ONE ANYBODY HAS SEEN. `acked` means the operator looked
    # at the last occurrence; carrying it into a new one hides the very thing the flap count
    # exists to make visible.
    case " ${B_LABELS[$k]:-} " in *" acked "*) bdq label remove "$id" acked >/dev/null 2>&1 ;; esac
    S_BEAD[$k]="$id"
    log "AURON re-raised $k on $id"
    return 0
}

alert_clear() {   # alert_clear <key> — close the bead with the retraction as its reason.
    local k="$1" id="${B_ID[$k]:-${S_BEAD[$k]:-}}"
    [ -n "$id" ] || return 0
    [ "${B_STATUS[$k]:-open}" = closed ] && return 0
    bdq close "$id" --reason-file - <<REASON >/dev/null 2>&1 || return 1
Cleared by Auron: the condition stopped holding for $CLEAR consecutive checks.
Raised $(iso "${S_FIRST[$k]:-$NOW}"), fired ${S_FLAPS[$k]:-1} time(s).
Nothing was repaired by Auron — it only reports. Either the loop recovered on its own
or something else fixed it.
REASON
    log "AURON cleared $k ($id)"
    return 0
}

# Every key that matters this run: what is firing, plus what we already had an opinion
# about. A key that has never fired and is not firing is not mentioned anywhere.
all_keys="$(printf '%s\n%s\n' "${firing_keys// /$'\n'}" "$(printf '%s\n' "${!S_STATE[@]}")" \
            | sed '/^$/d' | sort -u)"

for k in $all_keys; do
    is_firing=0
    case " $firing_keys " in *" $k "*) is_firing=1 ;; esac
    : "${S_STATE[$k]:=clear}"; : "${S_SEEN[$k]:=0}"; : "${S_UNSEEN[$k]:=0}"
    : "${S_SINCE[$k]:=$NOW}"; : "${S_FIRST[$k]:=0}"; : "${S_FLAPS[$k]:=0}"
    : "${S_BEAD[$k]:=}";      : "${S_REFRESHED[$k]:=0}"

    if [ "$is_firing" = 1 ]; then
        S_SEEN[$k]=$(( S_SEEN[$k] + 1 )); S_UNSEEN[$k]=0
    else
        S_UNSEEN[$k]=$(( S_UNSEEN[$k] + 1 )); S_SEEN[$k]=0
    fi

    # THE DATABASE IS THE CHANNEL, so with it down nothing below can be attempted. The
    # counters above still advance, so the moment it answers again the transition happens
    # on the next run rather than starting over.
    [ "$db_reachable" = 1 ] || continue

    if [ "${S_STATE[$k]}" != firing ] && [ "$is_firing" = 1 ] && [ "${S_SEEN[$k]}" -ge "$CONFIRM" ]; then
        S_FIRST[$k]=$NOW; S_FLAPS[$k]=$(( S_FLAPS[$k] + 1 ))
        if alert_write "$k"; then
            S_STATE[$k]=firing; S_SINCE[$k]=$NOW; S_REFRESHED[$k]=$NOW; acted=$((acted+1))
        else
            # The transition is NOT recorded, so the next run retries it. Recording a
            # firing state with no bead behind it would be an alert that exists only in a
            # file nobody reads, reported as delivered.
            S_FLAPS[$k]=$(( S_FLAPS[$k] - 1 )); beads_ok=0
        fi
    elif [ "${S_STATE[$k]}" = firing ] && [ "$is_firing" = 0 ] && [ "${S_UNSEEN[$k]}" -ge "$CLEAR" ]; then
        if alert_clear "$k"; then
            S_STATE[$k]=clear; S_SINCE[$k]=$NOW; acted=$((acted+1))
        else
            beads_ok=0
        fi
    elif [ "${S_STATE[$k]}" = firing ] && [ "$is_firing" = 1 ] \
         && [ $(( NOW - S_REFRESHED[$k] )) -ge "$REFRESH" ]; then
        # A STANDING ALERT'S NUMBERS GO STALE, and a body still reading "612s" four hours
        # in is worse than no number at all. Refreshed at most once an hour, so a wedged
        # loop costs one write per hour rather than one per run.
        #
        # NOT IF IT WAS CLOSED BY HAND. That close is an acknowledgement; reopening it
        # would be a machine arguing with the person it is reporting to. It re-raises
        # only after the condition has genuinely cleared and come back.
        if [ "${B_STATUS[$k]:-open}" = closed ]; then
            S_REFRESHED[$k]=$NOW
        elif alert_write "$k"; then
            S_REFRESHED[$k]=$NOW
        else
            beads_ok=0
        fi
    fi
done

# ======================================================================================
# THE FALLBACK FILE. Written if and only if the beads path failed, and REMOVED when it
# succeeded — so its existence is itself the statement "beads could not be reached", and
# there is never a stale second source of truth standing beside a working first one.
# ======================================================================================
fallback=0
if [ "$db_reachable" = 0 ] || [ "$beads_ok" = 0 ] || [ "$db_write_ok" = 0 ]; then
    fallback=1
    printf '%s' "$firing_json" | NOW="$NOW" DBOK="$db_reachable" WROK="$db_write_ok" python3 -c '
import sys, os, json
alerts = []
for line in sys.stdin:
    line = line.strip()
    if line:
        try: alerts.append(json.loads(line))
        except Exception: pass
db_read = os.environ["DBOK"] == "1"
db_write_raw = os.environ["WROK"]
db_write = True if db_write_raw == "1" else (False if db_write_raw == "0" else None)
json.dump({"at": int(os.environ["NOW"]), "db_reachable": db_read,
           "db_write_ok": db_write,
           "why": "beads could not be written; this file is Auron'"'"'s secondary channel",
           "alerts": alerts}, sys.stdout, indent=1, sort_keys=True)
sys.stdout.write("\n")' > "$FALLBACK.tmp.$$" 2>/dev/null \
        && mv -f "$FALLBACK.tmp.$$" "$FALLBACK" \
        || rm -f "$FALLBACK.tmp.$$"
    log "AURON wrote the fallback channel — db_read=$([ "$db_reachable" = 1 ] && echo ok || echo DOWN) db_write=$db_write_ok"
else
    rm -f "$FALLBACK" 2>/dev/null
fi

state_save

# ======================================================================================
# THE HEARTBEAT, written on every run including a failing one. A watchdog that stops
# without saying so is indistinguishable from a healthy system; the ops pane reads the age
# of this file and marks it stale rather than omitting it.
# ======================================================================================
n_firing=0; for k in $firing_keys; do n_firing=$((n_firing+1)); done
# SP_AURON_DB_WRITE uses the probe result: ok, down, saturated, or ? when unprobed.
# `saturated` means a bd call timed out (lock contention), not a true write failure —
# distinguishable from `down` so the cockpit can render the right diagnosis.
# A probe that could not run renders ? — never ok (which would hide a failure).
db_write_status="$(case "$db_write_ok" in 1) echo ok ;; 0) echo down ;; *) echo '?' ;; esac)"
[ "$db_saturated" = 1 ] && [ "$db_write_status" = down ] && db_write_status=saturated
db_read_status="$([ "$db_reachable" = 1 ] && echo ok || { [ "$db_saturated" = 1 ] && echo saturated || echo down; })"
{
    printf 'SP_AURON_AT=%s\n'          "$NOW"
    printf 'SP_AURON_FIRING=%s\n'      "$n_firing"
    printf "SP_AURON_KEYS='%s'\n"      "$(printf '%s' "${firing_keys# }" | tr ' ' ',')"
    printf 'SP_AURON_DB_READ=%s\n'     "$db_read_status"
    printf 'SP_AURON_DB_WRITE=%s\n'    "$db_write_status"
    printf 'SP_AURON_DB_SATURATED=%s\n' "$db_saturated"
    printf 'SP_AURON_FALLBACK=%s\n'    "$fallback"
    printf 'SP_AURON_ACTED=%s\n'       "$acted"
} > "$STATUS.tmp.$$" 2>/dev/null && mv -f "$STATUS.tmp.$$" "$STATUS"

log "auron: $n_firing firing [${firing_keys# }], $acted change(s), db_read=$db_read_status db_write=$db_write_status"
