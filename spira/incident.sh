#!/usr/bin/env bash
#
# incident.sh — turn a production event into a bead Ops can claim.
#
#   incident.sh systemd <unit>           file an incident for a failed systemd user unit
#   incident.sh file <title> [-|<file>]  file one from an arbitrary payload
#   incident.sh drain                    file everything the spool is holding
#   incident.sh list                     open incidents
#   incident.sh backfill-ref-labels      add ref:<hash> to older beads (one-time migration)
#   incident.sh backfill-recur-causes    convert bare sp-recur-N to sp-recur-N-unrecorded
#   incident.sh retire-unsatisfiable-delivers [--dry-run]
#                                        drop delivers: labels naming a path this install
#                                        can never write (one-time migration)
#
# THE INTAKE ALREADY EXISTED IN SHAPE
# -----------------------------------
# `mtgc-alert-<store>@.service` fires from `OnFailure=` on every unit worth hearing about,
# and pushes the failed unit's journal tail to Pushover. That is a notification: it reaches
# the operator's phone and then it is gone. This turns the same event into work with an identity —
# a bead Ops claims with a lease, resolves, and closes with evidence. `install-intake.sh`
# is the wiring; nothing here has to be remembered by a human.
#
# WRITE-AHEAD, THEN FILE
# ----------------------
# The payload is spooled to disk BEFORE the database is touched, and the spool entry is
# removed only once the bead exists. A production event arrives exactly once and cannot be
# re-asked for: if Dolt is down, or the box is mid-reboot, or `bd` takes longer than its
# timeout, the alternative is losing the only record of the failure. `drain` files whatever
# the spool is still holding, and the ops timer runs it every pass.
#
# DEDUPE, BECAUSE A FLAPPING UNIT IS ONE INCIDENT
# -----------------------------------------------
# A timer that fails every five minutes would otherwise file 300 beads a day, and a queue
# with 300 copies of one problem in it is a queue nobody reads — the failure mode already
# measured at 71 unread on the Mayor. A second failure of a unit that already has an open
# incident bumps a recurrence count on the existing bead instead. Past SIN_AT recurrences
# it is a Sin: a class that keeps returning because no SOP has broken the cycle, which is
# escalated to the operator exactly once (law-alerts-must-be-actionable — a second copy of a page
# is what buries the first).
set -uo pipefail
. "$(dirname "$0")/lib.sh"

# WHICH PARTITION AN INTAKE LANDS IN, and therefore who works it. `spira,incident` is Ops's
# and is the default, because the original caller was a crashed unit and Ops is the healer.
# A caller whose finding is a DEFECT rather than an outage sets this to the builder's labels
# instead: a red test suite needs somebody who can change the code, and Ops has eight minutes
# and a runbook. Everything else about the intake — the write-ahead spool, the dedupe on the
# external ref, the recurrence bump, the Sin escalation — is the same either way, and is the
# reason a second implementation of "file a bead, but only once" does not exist.
#
# THE OPEN-BEAD LOOKUP KEYS ON ref:<hash> ALONE. The authoritative dedupe key is
# external_ref, compared client-side; the --label filter is only a cheap prefilter to
# reduce the candidate set before that comparison. Any label the caller chooses —
# including the partition labels in SPIRA_INCIDENT_LABELS — must not appear in that
# prefilter: two filers of the same event that declare different labels would query
# non-overlapping partitions and never find each other's open bead, filing duplicates.
# ref:<hash> is derived from external_ref and cannot vary between filers of one event,
# so it is the only safe prefilter. The unlabeled fallback (sub-path B in _dedup_incident)
# uses no label filter at all, because those beads were filed before the ref: label existed
# and carry no invariant label.
LABELS="${SPIRA_INCIDENT_LABELS:-spira,incident}"

# THE REPOSITORY THIS INCIDENT BELONGS TO. Without a repo dimension a bead is worked in
# the home-repo fallback (brain), which has not contained the harness since sp-9tal.
# Callers declare it via SPIRA_INCIDENT_REPO. The repo dimension is written with
# bd set-state after the bead is created so that single-valuedness is enforced by the
# substrate (a label add can stack duplicates; set-state cannot). Where nothing is declared
# the bead is marked needs-repo-triage and escalated (sp-io5e, law-a-split-repoints-nothing).
_irepo="${SPIRA_INCIDENT_REPO:-}"
if [ -n "$_irepo" ]; then
    INCIDENT_REPO_DECLARED=1
else
    INCIDENT_REPO_DECLARED=0
fi

SPOOL="${SPIRA_SPOOL:-$SPIRA_RUN/incident-spool}"
ILOG="${SPIRA_INCIDENT_LOG:-$SPIRA_RUN/incident.log}"
SIN_AT="${SPIRA_SIN_AT:-5}"
# HOW FAR BACK THE DEDUP LOOKS FOR CLOSED BEADS. A failure whose bead was closed and then
# recurred is a recurrence, not a new event — reopening keeps the count and timeline on one
# record and prevents the close-then-refile loop that produced 38 duplicates in 2 days
# (sp-srgr6). The window is in whole days; the default of 7 days covers the week-scale
# flaps seen in timed-suite failures while leaving clearly old closures as "done".
DEDUP_LOOKBACK_DAYS="${SPIRA_INCIDENT_DEDUP_LOOKBACK:-7}"
# A CALLER MAY DECLARE ITSELF EXEMPT FROM THE SIN ESCALATION. The recurrence counter and the
# notes still increment — the signal stays where it belongs, on the bead and in the ops pane —
# but the counter cannot cross the SIN threshold and page the operator. The watchtower sweep
# is the canonical case: it fires on a ten-minute timer, so N counts intervals in which nobody
# closed a routine health report, not unremediated failures. Fifty minutes of quiet is five
# recurrences and a page, and closing the bead re-arms the cycle. $18/day of aeon cost to
# re-derive "the pipeline is fine" (measured sp-kufh).
SIN_EXEMPT="${SPIRA_SIN_EXEMPT:-0}"
# WATCHER INTERVAL: a close within this many seconds of the next same-ref filing
# is classified as closed-while-live rather than recurrence. Reads from conf.sh
# (SPIRA_WATCHER_INTERVAL_S); env override keeps tests isolated.
WATCHER_INTERVAL_S="${SPIRA_WATCHER_INTERVAL_S:-1800}"
# THE CAUSE CARRIED BY EACH RECURRENCE RUNG. Callers that know the kind of event they are
# filing set SPIRA_INCIDENT_CAUSE to a short slug (suite-red, systemd-fail, etc.). When not
# set, the rung records unrecorded rather than omitting the cause field — a recurrence must
# always say something about why, even if only "we did not capture it" (control: a
# recurrence whose cause genuinely cannot be determined still records, as unrecorded, rather
# than failing to record at all). Sanitised identically to bump_counter in lib.sh: a space
# in a label splits it into two labels, desynchronising the counter ladder.
INCIDENT_CAUSE="${SPIRA_INCIDENT_CAUSE:-unrecorded}"
INCIDENT_CAUSE="$(printf '%s' "$INCIDENT_CAUSE" | tr -c 'a-zA-Z0-9-' '-' | sed 's/-\{2,\}/-/g;s/^-//;s/-$//')"
[ -n "$INCIDENT_CAUSE" ] || INCIDENT_CAUSE="unrecorded"
mkdir -p "$SPOOL" "$(dirname "$ILOG")"

ilog() { printf '%s incident: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" | tee -a "$ILOG"; }
# CAUSE OMISSION IS VISIBLE. A producer that does not set SPIRA_INCIDENT_CAUSE is noticed
# at filing time rather than silently widening the "unrecorded" bucket in census.sh.
[ "$INCIDENT_CAUSE" = "unrecorded" ] && ilog "warning: SPIRA_INCIDENT_CAUSE not set for ${SPIRA_INCIDENT_REF:-(unknown ref)} — recurrence will be recorded as 'unrecorded'; add SPIRA_INCIDENT_CAUSE=<slug> to the producer's env block"

# Deterministic 8-char hex label token for an external_ref string. sha256sum is standard
# on Linux; cut -c1-8 gives 32 bits — collision probability across a typical incident queue
# (hundreds of open beads, not millions) is negligible, and a collision is not a silent
# failure: the dedup path checks external_ref from bd list output, so a collision only
# adds one extra list iteration.
_ref_hash() {   # _ref_hash <ref> -> 8-char hex
    printf '%s' "$1" | sha256sum | cut -c1-8
}

# PROVENANCE: the three facts an operator needs to decide whether an escalated ask is real.
#
# _unit_from_cgroup derives the calling systemd unit from /proc/self/cgroup, whose last
# path component is the unit name (e.g. spira-suites.service). Outputs ? rather than
# guessing when the path carries no recognisable unit suffix — a failed probe must render ?
# and never a plausible-looking name (law-detection-outranks-rejection).
#
# _provenance builds "<unit> on <host>: <path>" from SPIRA_INCIDENT_UNIT (env, fallback
# to cgroup) and SPIRA_INCIDENT_PATH (env, fallback to ?). Callers that know the absolute
# path of the failing suite or command set SPIRA_INCIDENT_PATH; callers that know their
# own unit name set SPIRA_INCIDENT_UNIT. The spool captures both so drain retries carry
# the same provenance the original filing would have shown.
_unit_from_cgroup() {
    local _cg _u
    _cg="$(head -1 /proc/self/cgroup 2>/dev/null)"
    _u="${_cg##*/}"
    case "$_u" in *.service|*.timer|*.scope) printf '%s' "$_u" ;; *) printf '?' ;; esac
}
_provenance() {
    local _u _h _p
    _u="${SPIRA_INCIDENT_UNIT:-$(_unit_from_cgroup)}"
    _h="$(hostname 2>/dev/null)"; [ -n "${_h:-}" ] || _h='?'
    _p="${SPIRA_INCIDENT_PATH:-?}"
    printf '%s on %s: %s' "$_u" "$_h" "$_p"
}

# --------------------------------------------------------------------------------------
# Dedupe key for an incident ref. Prints "open <id> <n>", "closed <id> <n>", or nothing.
# <n> is the current highest sp-recur-N count, extracted from the bead's labels so the
# caller can skip a separate bd call.
# --------------------------------------------------------------------------------------
# TWO PASSES, NOT ONE. Pass 1 queries open/in_progress; pass 2 (only when pass 1 finds
# nothing) queries recently-closed. They cannot be merged into one call because
# --closed-after filters on the closed_at field: open beads carry no closed_at and are
# silently excluded when --closed-after is present, making the combined query always return
# empty for the common recurrence case.
#
# LABEL-KEYED, NOT bd-show-per-candidate. Each bead carries ref:<hash-of-external-ref>
# at filing time. Querying --label ref:<hash> returns at most 1 candidate regardless of
# queue depth; external_ref is present in bd list --json output so no bd show is needed
# to confirm the match. A hash collision (two distinct refs share the 8-char hash) adds
# one extra list iteration, not a bd show per candidate.
#
# FALLBACK FOR UNLABELED BEADS. A bead filed by older code carries no ref: label.
# After the label-keyed path finds nothing, a second query walks all open beads and
# filters client-side on external_ref, skipping beads that carry any ref: label (those
# were already checked and did not match). No label filter is applied: any filer-chosen
# label in the filter would partition the candidate set so cross-label duplicates are
# missed — the same class of defect as the one in sub-path A, on the slower path.
# Once found on this path, an unlabeled bead receives the ref: label so subsequent
# queries take the fast label-keyed path.
#
# ALL SHELL-LEVEL BD CALLS USE bdq, NOT $SPIRA_BD DIRECTLY. bdq is defined in lib.sh
# (sourced above) and adds -C "$SPIRA_DB" so bd finds the right database. Calling $SPIRA_BD
# directly omits -C and makes bd search from the working directory, which finds nothing.
#
# DATE ARITHMETIC IS GNU date(1). The -v flag is a BSD/macOS fallback. An empty since
# skips the closed-bead search rather than scanning with an unbounded window.
# REOPENING IS THE CHOSEN STRATEGY, not linking: a closed bead within the lookback is
# the same incident returning. One bead that says "red 6 times over 2 days" lets Ops
# see the pattern; a chain of six single-occurrence beads does not (sp-srgr6).
# --------------------------------------------------------------------------------------
_dedup_incident() {      # _dedup_incident <ref> -> "open <id> <n>" | "closed <id> <n>" | nothing
    local ref="$1" _ref_label _since _r
    _ref_label="ref:$(_ref_hash "$ref")"

    # PASS 1 — open / in_progress.
    # Sub-path A: label-keyed on ref:<hash> ALONE. No caller-chosen label appears here:
    # two filers with different SPIRA_INCIDENT_LABELS would query non-overlapping label
    # partitions and never find each other's bead. ref:<hash> is derived from external_ref
    # and is invariant across all filers of one event. At most 1 candidate is returned;
    # external_ref is confirmed client-side. bdq adds -C "$SPIRA_DB" so the query reaches
    # the configured database, not the auto-discovered one (law-address-the-store-with-spira-bd).
    _r="$(bdq list --status open,in_progress --limit 0 --label "$_ref_label" --json 2>/dev/null \
      | python3 -c '
import sys, json
target = sys.argv[1]
try:
    for bead in json.load(sys.stdin):
        if bead.get('"'"'external_ref'"'"') == target and bead.get('"'"'status'"'"') in ('"'"'open'"'"', '"'"'in_progress'"'"'):
            print('"'"'open'"'"', bead['"'"'id'"'"']); sys.exit(0)
except: pass
' "$ref" 2>/dev/null)"
    if [ -n "$_r" ]; then printf '%s' "$_r"; return; fi

    # Sub-path B: fallback for beads without the ref: label (filed by older code).
    # No label filter: these beads predate the ref: label and carry no invariant label,
    # so any label filter would partition by filer and miss cross-label duplicates.
    # Skips beads that carry any ref: label — those were already checked in sub-path A.
    # Once found here, file_one adds the label so this path is not needed again.
    _r="$(bdq list --status open,in_progress --limit 0 --json 2>/dev/null \
      | python3 -c '
import sys, json
target = sys.argv[1]
try:
    for bead in json.load(sys.stdin):
        if any(l.startswith('"'"'ref:'"'"') for l in (bead.get('"'"'labels'"'"') or [])): continue
        if bead.get('"'"'external_ref'"'"') == target and bead.get('"'"'status'"'"') in ('"'"'open'"'"', '"'"'in_progress'"'"'):
            print('"'"'open'"'"', bead['"'"'id'"'"']); sys.exit(0)
except: pass
' "$ref" 2>/dev/null)"
    if [ -n "$_r" ]; then printf '%s' "$_r"; return; fi

    # PASS 2 — recently-closed. Only reached when no open bead matched.
    _since="$(date -u -d "-${DEDUP_LOOKBACK_DAYS} days" '+%Y-%m-%d' 2>/dev/null \
           || date -u -v "-${DEDUP_LOOKBACK_DAYS}d" '+%Y-%m-%d' 2>/dev/null || true)"
    [ -z "$_since" ] && return

    # Sub-path A for closed beads: label-keyed on ref:<hash> alone.
    # Prints "closed <id> <closed_at>" so the caller can compute how recently
    # the bead was closed and classify the reopen as closed-while-live vs recurrence.
    _r="$(bdq list --status closed --closed-after "$_since" --limit 0 --label "$_ref_label" --json 2>/dev/null \
      | python3 -c '
import sys, json
target = sys.argv[1]
try:
    for bead in json.load(sys.stdin):
        if bead.get('"'"'external_ref'"'"') == target and bead.get('"'"'status'"'"') == '"'"'closed'"'"':
            print('"'"'closed'"'"', bead['"'"'id'"'"'], bead.get('"'"'closed_at'"'"') or '"'"''"'"'); sys.exit(0)
except: pass
' "$ref" 2>/dev/null)"
    if [ -n "$_r" ]; then printf '%s' "$_r"; return; fi

    # Sub-path B for closed beads: fallback for unlabeled beads. No label filter,
    # same reason as the open-bead fallback above.
    bdq list --status closed --closed-after "$_since" --limit 0 --json 2>/dev/null \
      | python3 -c '
import sys, json
target = sys.argv[1]
try:
    for bead in json.load(sys.stdin):
        if any(l.startswith('"'"'ref:'"'"') for l in (bead.get('"'"'labels'"'"') or [])): continue
        if bead.get('"'"'external_ref'"'"') == target and bead.get('"'"'status'"'"') == '"'"'closed'"'"':
            print('"'"'closed'"'"', bead['"'"'id'"'"'], bead.get('"'"'closed_at'"'"') or '"'"''"'"'); sys.exit(0)
except: pass
' "$ref" 2>/dev/null
}

# --------------------------------------------------------------------------------------
# file_one <ref> <title> <payload-file> — create or dedupe. Prints the bead id.
# Returns non-zero WITHOUT consuming the spool entry if the database could not be reached,
# so a transient failure costs a retry rather than the event.
# --------------------------------------------------------------------------------------
file_one() {
    local ref="$1" title="$2" pf="$3" id n _was_closed _reopen_note _log_suffix _hit _recur_n _prov
    # PROVENANCE: built once per filing so both ask-filing paths see the same string.
    # Derived here, inside file_one, so it reflects the current SPIRA_INCIDENT_UNIT and
    # SPIRA_INCIDENT_PATH — which drain_one shadows per spool entry (backward-compatible).
    _prov="$(_provenance)"

    # DEDUP QUERY — two passes (open first, closed second if needed). If the database is
    # unreachable, _dedup_incident prints nothing; id stays empty and the probe below catches
    # it. The probe is skipped on the recurrence path because a result from _dedup_incident
    # proves the database is reachable. The recurrence count comes from the events trail
    # (sp-lzt removed sp-recur-N label writes; recurs_of queries the events table instead).
    _hit="$(_dedup_incident "$ref")"
    id="" _was_closed=0 _recur_n=0 _closed_at_raw=""
    case "$_hit" in
        "open "*)   _rest="${_hit#open }";   id="${_rest%% *}"; _was_closed=0 ;;
        "closed "*) _rest="${_hit#closed }"; id="${_rest%% *}"
                    _closed_at_raw="${_rest#"$id"}"; _closed_at_raw="${_closed_at_raw# }"
                    _was_closed=1 ;;
    esac
    if [ -n "${id:-}" ]; then
        # THE COUNT COMES FROM THE EVENT HISTORY. sp-recur-N labels stopped being written
        # when recurrences became events (sp-lzt), and the reader did not move with them
        # (law-a-rename-repoints-no-reader): _recur_n was 0 on every recurrence and n was 1
        # every time. recurs_of queries the events table; the larger of the two is taken so
        # a bead filed by older code that still carries sp-recur-N labels counts correctly.
        _ev_n="$(recurs_of "$id" 2>/dev/null)"
        case "$_ev_n" in ''|*[!0-9]*) _ev_n=0 ;; esac
        [ "$_ev_n" -gt "$_recur_n" ] && _recur_n="$_ev_n"
        n=$((_recur_n + 1))
        if [ "$_was_closed" = 1 ]; then
            # CLASSIFY THE REOPEN. A bead closed within one watcher interval of the next
            # same-ref filing was closed while the condition was still live — the aeon
            # resolved it before the watcher confirmed the fix. Record that as its own
            # class so census can distinguish it from genuine recurrences after a real fix.
            _reopen_cause=recurrence
            if [ -n "$_closed_at_raw" ]; then
                _close_ts="$(date -u -d "$_closed_at_raw" +%s 2>/dev/null || true)"
                if [ -n "$_close_ts" ]; then
                    _now_ts="$(date -u +%s)"
                    [ "$(( _now_ts - _close_ts ))" -lt "$WATCHER_INTERVAL_S" ] && _reopen_cause=closed-while-live
                fi
            fi
            bead_reopen "$id" "$_reopen_cause" "Recurrence $n at $(date -u +%Y-%m-%dT%H:%M:%SZ) — same failure fingerprint, dedup within ${DEDUP_LOOKBACK_DAYS}-day window"
        fi
        # sp-recur-N-<cause> labels are no longer written; recurrence count is derived
        # from event history. The note below records the recurrence (sp-lzt).
        # PROMOTE FALLBACK-FOUND BEADS. A bead found via the O(N) fallback path has no
        # ref: label; adding it here ensures the next query takes the fast label-keyed path.
        # bdq label add is idempotent, so this is safe to call even if the label already exists.
        bdq label add "$id" "ref:$(_ref_hash "$ref")" >/dev/null 2>&1
        _reopen_note="" _log_suffix=""
        if [ "$_was_closed" = 1 ]; then
            _reopen_note="
Reopened by dedup — same external ref seen again within ${DEDUP_LOOKBACK_DAYS} days of close."
            _log_suffix=" (reopened from closed)"
        fi
        # Write the full payload only when it differs from the last stored hash — an
        # unchanged base-suite red otherwise grows the bead ~1.3KB per recurrence (sp-obwc).
        _ph="$(sha256sum "$pf" 2>/dev/null | cut -c1-16)"
        _prev_ph="$(bdq label list "$id" 2>/dev/null | grep -o 'payload-hash:[0-9a-f]*' | head -1 || true)"
        if [ -n "$_ph" ] && [ "payload-hash:${_ph}" = "${_prev_ph:-}" ] && [ "$_was_closed" = 0 ]; then
            bdq note "$id" "Recurrence $n at $(date -u +%Y-%m-%dT%H:%M:%SZ).${_reopen_note}" >/dev/null 2>&1
        else
            bdq note "$id" "Recurrence $n at $(date -u +%Y-%m-%dT%H:%M:%SZ).${_reopen_note}
$(head -c 2000 "$pf")" >/dev/null 2>&1
            if [ -n "$_ph" ]; then
                [ -n "${_prev_ph:-}" ] && bdq label remove "$id" "$_prev_ph" >/dev/null 2>&1
                bdq label add "$id" "payload-hash:${_ph}" >/dev/null 2>&1
            fi
        fi
        bump_recur "$id" "$INCIDENT_CAUSE" >/dev/null 2>&1
        ilog "$ref recurred ($n) — $id${_log_suffix}"
        # A Sin: it keeps coming back because nothing has broken the cycle. Escalated once,
        # on the crossing, never again — a second page buries the first.
        # AN EXEMPT REF NEVER REACHES THIS BLOCK. The recurrence counter and the notes have
        # already been written above, so the signal is preserved; what is removed is its ability
        # to raise an ask against the operator. The log still says the threshold was crossed.
        if [ "$SIN_EXEMPT" = 1 ] && [ "$n" -ge "$SIN_AT" ]; then
            ilog "$ref crossed SIN_AT=$SIN_AT ($n recurrences) but is exempt — no escalation"
        elif [ "$n" -ge "$SIN_AT" ] && ! bdq label list "$id" 2>/dev/null | grep -q '\bsin\b'; then
            bdq label add "$id" sin >/dev/null 2>&1
            # THE ASK LEADS WITH PROVENANCE, not the bead title or $ref. $ref is a dedupe
            # slug ("incident:Spira-sweep-----is-the-pipeline-moving-"), which reaches the
            # operator as a mangled identifier with no subject. The title ("test-loom.sh is
            # red") is equally opaque because it omits where the failure came from — four
            # escalations in one hour were rejected with "is this from a test container?"
            # (sp-fzxk9, sp-dmjge). Leading with "$_prov" — the unit, host, and absolute
            # path — answers that question before it can be asked
            # (law-escalations-carry-their-evidence, law-escalations-lead-with-the-bead).
            local age evf first secs
            # Elapsed beats a bare count: "5 times" says nothing about whether that is an
            # hour of noise or a fortnight of it. Guarded, because a date this cannot parse
            # must cost the phrase and not the ask.
            age=""
            first="$(bdq show "$id" --json 2>/dev/null \
                | grep -m1 -oE '"created"[^,]*' \
                | grep -oE '[0-9]{4}-[0-9]{2}-[0-9]{2}[T ][0-9:]+' || true)"
            if [ -n "$first" ]; then
                secs=$(( $(date -u +%s) - $(date -u -d "$first" +%s 2>/dev/null || echo 0) ))
                [ "$secs" -gt 0 ] && age=" over $(( secs / 3600 ))h $(( (secs % 3600) / 60 ))m"
            fi
            # The vital signs are at the HEAD of the payload; trim to 2000 bytes so the mail body stays bounded.
            local _sin_subj="$_prov — recurred $n times$age with no fix holding. Mute it, or keep paging?"
            local _sin_dflt="mute this alert and leave $id open for Ops to work unpaged; keep paging only if you want a decision on every recurrence"
            local _sin_ev; _sin_ev="$(head -c 2000 "$pf" 2>/dev/null || true)"
            if [ -x "$SPIRA_HOME/mail.sh" ]; then
                "$SPIRA_HOME/mail.sh" send operator \
                    --from "Incident <incident@spira>" \
                    --subject "$_sin_subj" \
                    --kind question \
                    --default "$_sin_dflt" <<MAILEOF >/dev/null 2>&1
## Question
$_sin_subj

## Default
$_sin_dflt

$id is "$title". It has fired $n times$age and each recurrence pages you while filing nothing new. Its current vital signs are below — if they show nothing you must act on, muting is the right answer.

$_sin_ev
MAILEOF
            fi
            ilog "$ref is a SIN at $n recurrences — escalated once"
        fi
        printf '%s' "$id"
        return 0
    fi

    # No existing bead found. Distinguish "no open incident" from "database unreachable":
    # _dedup_incident returned empty in both cases, but the right response differs.
    # A bd that cannot answer must not be treated as "no open incident" — that reading is
    # how one outage becomes one bead per alert.
    if ! bdq list --limit 1 >/dev/null 2>&1; then
        ilog "database unreachable — $ref stays spooled"
        return 1
    fi

    # THE TYPE AND THE ACTOR ARE THE CALLER'S, because not every intake is a failure. A
    # crashed unit is a bug and belongs at P1; the watchtower's ten-minute health sweep is
    # routine work, and filing it as a `bug` owned by whoever ran the timer put a chore in
    # the operator's queue looking like a defect he had been assigned. Defaults unchanged, so
    # the systemd path files exactly as it always did.
    id="$(BEADS_ACTOR="${SPIRA_INCIDENT_ACTOR:-${BEADS_ACTOR:-}}" \
          bdq create "$title" --type "${SPIRA_INCIDENT_TYPE:-bug}" \
            --priority "$SPIRA_INCIDENT_PRIORITY" \
            --labels "$LABELS" --external-ref "$ref" \
            --body-file "$pf" --silent 2>/dev/null | tr -d '[:space:]')"
    if [ -z "${id:-}" ]; then
        ilog "create FAILED for $ref — stays spooled"
        return 1
    fi
    ilog "filed $id for $ref"
    # THE REPOSITORY DIMENSION IS WRITTEN WITH SET-STATE, NOT AS A LABEL AT CREATION. Writing
    # it through bd set-state rather than embedding "repo:$name" in the --labels string
    # enforces single-valuedness at the substrate level: a second set-state call removes the
    # first repo: label atomically, so no bead can accumulate two repo: labels (the failure
    # bd label add allowed). Done immediately after creation so the bead never exists without
    # its repo dimension in the open state.
    if [ "${INCIDENT_REPO_DECLARED:-0}" = 1 ] && [ -n "${_irepo:-}" ]; then
        bdq set-state "$id" "repo=${_irepo}" >/dev/null 2>&1
    fi
    # DELIVERS LABEL. When an Ops session commits code naming the bead, the sentinel's
    # commit-naming check accepts the close and this label is never consulted. When no
    # commit is made (the SOP already existed and no file changed), the sentinel checks
    # this label: if `sop.sh applied` or `sop.sh write` ran during the session the
    # applications ledger is updated, its mtime is after started_at, and the close stands.
    # A session that skips both sop.sh calls is re-summoned — enforcing the closing rule
    # mechanically rather than trusting the brief alone.
    #
    # SPIRA_INCIDENT_DELIVERS: a caller that knows at filing time that no SOP and no commit
    # will ever satisfy this bead can name the delivers type explicitly. Only recognised types
    # are written — an unknown type is logged and omitted rather than recorded as a criterion
    # nobody can satisfy. Currently recognised: action (close accepted; close reason is the
    # evidence). When set, the SOP ledger path is skipped entirely, so the two do not combine
    # into a criterion the aeon must satisfy both parts of.
    #
    # SCHEMA ON WRITE: A CRITERION NOBODY CAN SATISFY IS NEVER RECORDED.
    #
    # This label used to be stamped unconditionally, and both ways it could be wrong
    # were live at once. Eleven open beads named $SPIRA_RUN/sop/applied.jsonl, a file
    # under a directory nothing ever created -- so `sop.sh applied` could not write it,
    # the mtime check could never pass, and any close without a commit was reopened
    # forever. Two more named an absolute path under another install's layout, having
    # travelled between machines inside the bead: no session on this box could satisfy
    # those at all.
    #
    # Repairing the labels afterwards leaves the writer free to mint more, so the check
    # belongs here. Two conditions, and the label is written only when both hold:
    #   - the path is under THIS install's run directory, so a session here can write it
    #   - its parent directory exists, which we ensure rather than assume
    #
    # SKIPPING IS LOGGED. A criterion silently omitted is as hard to diagnose as one
    # that cannot be met -- the bead simply closes on the commit rule and nobody knows
    # a second rule was meant to apply.
    if [ -n "${SPIRA_INCIDENT_DELIVERS:-}" ]; then
        case "${SPIRA_INCIDENT_DELIVERS}" in
            action)
                bdq label add "$id" "delivers:${SPIRA_INCIDENT_DELIVERS}" >/dev/null 2>&1
                ilog "delivers: $id: delivers:${SPIRA_INCIDENT_DELIVERS} written (SPIRA_INCIDENT_DELIVERS set by caller)" ;;
            *)
                ilog "delivers: $id: unrecognised SPIRA_INCIDENT_DELIVERS='${SPIRA_INCIDENT_DELIVERS}' — label not written" ;;
        esac
    else
        _sop_ledger="${SPIRA_SOP_LEDGER:-${SPIRA_RUN}/sop/applied.jsonl}"
        _deliverable=1
        case "$_sop_ledger" in
            "${SPIRA_RUN}"/*) ;;
            *) _deliverable=0
               ilog "delivers: $id: ledger $_sop_ledger is outside $SPIRA_RUN — label not written (a session here could never satisfy it)" ;;
        esac
        if [ "$_deliverable" = 1 ] && ! mkdir -p "$(dirname "$_sop_ledger")" 2>/dev/null; then
            _deliverable=0
            ilog "delivers: $id: cannot create $(dirname "$_sop_ledger") — label not written"
        fi
        if [ "$_deliverable" = 1 ]; then
            bdq label add "$id" "delivers:note:${_sop_ledger}" >/dev/null 2>&1
        fi
    fi
    # LABEL THE REF HASH so future dedup queries take the O(1) label-keyed path instead of
    # scanning all open incident beads. Added at creation so every new bead carries it from
    # the start; the backfill-ref-labels subcommand labels beads filed before this was added.
    bdq label add "$id" "ref:$(_ref_hash "$ref")" >/dev/null 2>&1
    # NO bump_recur ON FIRST FILING. The recurrence counter (recurs_of) counts events of
    # type "recurred", which are written ONLY on second-and-later filings. The first filing
    # is creation, not a recurrence. SIN fires at the (SIN_AT+1)-th total filing: first
    # creates the bead, then SIN_AT recurrences accumulate before the escalation triggers.
    # The old behaviour (bump_recur on first filing) was removed because it made recurs_of
    # return 1 for a fresh bead, contradicting the semantic that recurs_of counts
    # recurrences, not total filings (sp-doh5).
    # AN UNDECLARED REPO STAYS VISIBLE. Filed but labelled needs-repo-triage so an aeon
    # that would claim it in the home-repo fallback is stopped by its own confusion rather
    # than silently working in the wrong checkout. Escalated once so the operator can
    # correct the label before any aeon touches it (sp-io5e, law-a-split-repoints-nothing).
    if [ "${INCIDENT_REPO_DECLARED:-1}" = 0 ]; then
        bdq label add "$id" "needs-repo-triage" >/dev/null 2>&1
        bdq note "$id" "Repository not declared — SPIRA_INCIDENT_REPO was not set. An aeon claiming this bead works it in the home-repo fallback, which may be the wrong checkout. Set the repo dimension with: bd set-state $id repo=<name>." >/dev/null 2>&1
        if [ "${SIN_EXEMPT:-0}" != 1 ]; then
            local _ask_subj="undeclared repo: $(printf '%s' "$ref" | cut -c1-72)"
            local _ask_title="$_prov — $_ask_subj"
            local _ask_dflt="add repo:<name> to $id once you know which checkout owns the code this incident is about"
            [ -x "$SPIRA_HOME/mail.sh" ] && "$SPIRA_HOME/mail.sh" send operator \
                --from "Incident <incident@spira>" \
                --subject "$_ask_title" \
                --kind question \
                --default "$_ask_dflt" <<MAILEOF >/dev/null 2>&1
## Question
$_ask_title

## Default
$_ask_dflt

$id was filed without a repo: label. Without one an aeon works it in the home-repo fallback, which may be the wrong checkout.
MAILEOF
        fi
        ilog "$ref labelled needs-repo-triage — repo undeclared"
    fi
    printf '%s' "$id"
}

# spool_write <ref> <title> <<payload on stdin>> -> path of the spool entry
spool_write() {
    local ref="$1" title="$2" safe stamp path _sw_unit _sw_path
    # printf, not a herestring: `<<<` appends a newline, tr turns it into another
    # separator character, and the dedupe key quietly grows a trailing underscore.
    # PROVENANCE and CAUSE captured at spool time so drain retries use the original values.
    # hostname is not stored — it is always readable on the same machine at drain time.
    _sw_unit="${SPIRA_INCIDENT_UNIT:-$(_unit_from_cgroup)}"
    _sw_path="${SPIRA_INCIDENT_PATH:-?}"
    safe="$(printf '%s' "$ref" | tr -c 'A-Za-z0-9._-' '_' | cut -c1-80)"
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    path="$SPOOL/$stamp-$safe-$$"
    { printf 'REF: %s\nTITLE: %s\nUNIT: %s\nINCIDENT_PATH: %s\nCAUSE: %s\n--\n' \
        "$ref" "$title" "$_sw_unit" "$_sw_path" "$INCIDENT_CAUSE"; cat; } > "$path"
    printf '%s' "$path"
}

spool_field() { sed -n "s/^$1: //p" "$2" | head -1; }
spool_body()  { sed -n '/^--$/,$p' "$1" | tail -n +2; }

drain_one() {            # drain_one <spool-path>
    # PROVENANCE and CAUSE VARS are shadowed locally so each spool entry's values are
    # isolated. All default gracefully when the spool entry predates the field (backward-
    # compatible): provenance to ?, cause to unrecorded.
    local SPIRA_INCIDENT_UNIT SPIRA_INCIDENT_PATH INCIDENT_CAUSE
    local sp="$1" ref title body id _d_unit _d_path _d_cause
    ref="$(spool_field REF "$sp")"; title="$(spool_field TITLE "$sp")"
    [ -n "$ref" ] || { ilog "spool entry with no REF: $sp — moved aside"; mv "$sp" "$sp.bad"; return 0; }
    _d_unit="$(spool_field UNIT "$sp")"; SPIRA_INCIDENT_UNIT="${_d_unit:-?}"
    _d_path="$(spool_field INCIDENT_PATH "$sp")"; SPIRA_INCIDENT_PATH="${_d_path:-?}"
    _d_cause="$(spool_field CAUSE "$sp")"
    # BACKWARD COMPAT: entries written by older code carry no CAUSE line — treat as unrecorded.
    INCIDENT_CAUSE="${_d_cause:-unrecorded}"
    INCIDENT_CAUSE="$(printf '%s' "$INCIDENT_CAUSE" | tr -c 'a-zA-Z0-9-' '-' | sed 's/-\{2,\}/-/g;s/^-//;s/-$//')"
    [ -n "$INCIDENT_CAUSE" ] || INCIDENT_CAUSE="unrecorded"
    body="$(mktemp)"; spool_body "$sp" > "$body"
    # An empty payload is a broken probe, not an incident with no detail — and `bd create`
    # refuses an empty --body-file outright, so filing would fail and the event would sit in
    # the spool forever looking like an outage. Say what could not be gathered, in the bead,
    # where whoever picks it up will see it (a failed probe renders `?`, never 0).
    if [ ! -s "$body" ]; then
        printf 'The intake gathered NO payload for %s.\nsystemctl/journalctl produced nothing — suspect the unit name or a journal this user cannot read.\n' \
               "$ref" > "$body"
    fi
    # SERIALISED, BECAUSE THE DEDUPE IS A CHECK FOLLOWED BY AN ACT. file_one asks whether an
    # open incident already carries this ref and creates one if not; two callers that ask
    # before either answers both get "no" and both file. That is not theoretical — the
    # watchtower's timer and a hand-run of the same unit produced sp-aapz and sp-uvfq at
    # 22:30:15 on 2026-09-07, same second, same ref, two beads, and each one costs a separate
    # Ops session working an identical snapshot.
    #
    # ONE LOCK FOR THE WHOLE INTAKE, not one per ref. The critical section is a database read
    # and a write measured in hundreds of milliseconds, incidents do not arrive in floods, and
    # a per-ref lock would leave two DIFFERENT refs racing on `bd create` anyway. The wait is
    # bounded and a timeout leaves the entry spooled — which is the write-ahead behaving
    # exactly as designed rather than a loss.
    mkdir -p "$(dirname "$SPOOL")" 2>/dev/null
    local lock="${SPIRA_INCIDENT_LOCK:-$SPIRA_RUN/incident.lock}"
    exec 8>>"$lock" || { ilog "cannot open the intake lock at $lock — $ref stays spooled"; rm -f "$body"; return 1; }
    if ! flock -w "${SPIRA_INCIDENT_LOCK_WAIT:-30}" 8; then
        ilog "another intake held $lock for 30s — $ref stays spooled, drain will retry"
        exec 8>&-; rm -f "$body"; return 1
    fi
    if id="$(file_one "$ref" "$title" "$body")" && [ -n "$id" ]; then
        exec 8>&-
        rm -f "$sp" "$body"; printf '%s\n' "$id"; return 0
    fi
    exec 8>&-
    rm -f "$body"; return 1
}

case "${1:-}" in

systemd)
    unit="${2:?usage: incident.sh systemd <unit>}"
    ref="incident:$unit"
    # PROVENANCE: the unit name is explicit here; the path is the unit itself since there is
    # no single file path more authoritative than the unit name for a crashed unit.
    SPIRA_INCIDENT_UNIT="$unit"
    # The evidence, gathered while it is still fresh. `systemctl show` first because the
    # exit status and the result reason are what distinguish "the command failed" from
    # "the unit timed out" from "the box killed it", and the journal alone does not always
    # say which. 40 lines of journal, because a bead body nobody can read is a payload
    # nobody reads.
    payload="$( {
        printf 'unit: %s\nhost: %s\nwhen: %s\n\n' \
               "$unit" "$(hostname)" "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        printf '## systemctl\n'
        systemctl --user show "$unit" \
            -p Result -p ExecMainStatus -p ExecMainCode -p NRestarts \
            -p ActiveState -p SubState -p InvocationID -p ExecMainStartTimestamp \
            2>/dev/null || echo '(systemctl unavailable)'
        printf '\n## journal (last 40)\n'
        journalctl --user -u "$unit" -n 40 --no-pager 2>/dev/null || echo '(journal unavailable)'
    } )"
    sp="$(printf '%s' "$payload" | spool_write "$ref" "incident: $unit failed")"
    drain_one "$sp" || { ilog "spooled $ref at $sp"; exit 1; }
    ;;

file)
    title="${2:?usage: incident.sh file <title> [-|<file>]}"
    src="${3:--}"
    # THE REF IS THE DEDUPE KEY AND THE CALLER MAY STATE IT. Derived from the title only
    # because most callers have nothing better; a title is prose, it is truncated at 60
    # characters here, and two genuinely different findings that happen to share a wording
    # would then dedupe into one bead and the second would be lost as a recurrence of the
    # first. A caller that knows the identity of what it found — a suite plus the fingerprint
    # of its failure — passes that instead, and gets the dedupe the comment on open_incident
    # promises: keyed on an identifier, not on words somebody will eventually reword.
    ref="${SPIRA_INCIDENT_REF:-}"
    [ -n "$ref" ] || ref="incident:$(printf '%s' "$title" | tr -c 'A-Za-z0-9._-' '-' | cut -c1-60)"
    case "$src" in
        -) sp="$(spool_write "$ref" "$title")" ;;
        *) sp="$(spool_write "$ref" "$title" < "$src")" ;;
    esac
    drain_one "$sp" || { ilog "spooled $ref at $sp"; exit 1; }
    ;;

drain)
    n=0; stuck=0
    for sp in "$SPOOL"/*; do
        [ -f "$sp" ] || continue
        case "$sp" in *.bad) continue ;; esac
        if drain_one "$sp" >/dev/null; then n=$((n+1)); else stuck=$((stuck+1)); fi
    done
    # Report both numbers, always. A drain that prints only its successes is a probe that
    # renders a broken check as all-clear.
    printf 'drained %d, still spooled %d\n' "$n" "$stuck"
    [ "$stuck" -eq 0 ]
    ;;

list)
    bdq list --status open,in_progress --limit 0 --label "$LABELS" 2>/dev/null \
        | grep -vE '^💡|^warning|^  Fix|^  Or'
    n="$(find "$SPOOL" -maxdepth 1 -type f ! -name '*.bad' 2>/dev/null | wc -l)"
    [ "$n" -gt 0 ] && printf '\n%s event(s) still in the spool — run: incident.sh drain\n' "$n"
    b="$(find "$SPOOL" -maxdepth 1 -type f -name '*.bad' 2>/dev/null | wc -l)"
    [ "$b" -gt 0 ] && printf '%s malformed spool entr(ies) in %s\n' "$b" "$SPOOL"
    exit 0
    ;;

backfill-ref-labels)
    # MIGRATION PASS — adds ref:<hash> labels to open incident beads that lack them.
    # A bead filed before this feature was added carries no ref: label and is found only
    # by the O(N) fallback path. Running this once labels every open bead so all future
    # dedup queries take the fast O(1) label-keyed path. Safe to re-run: bd label add
    # is idempotent, and beads already carrying a ref: label are skipped.
    n=0 s=0 e=0
    while IFS=$'\t' read -r bid eref; do
        [ -n "$bid" ] && [ -n "$eref" ] || continue
        _rl="ref:$(_ref_hash "$eref")"
        if bdq label add "$bid" "$_rl" >/dev/null 2>&1; then
            n=$((n+1))
        else
            e=$((e+1))
        fi
    done < <(bdq list --status open,in_progress --limit 0 --label "$LABELS" --json 2>/dev/null \
      | python3 -c '
import sys, json
try:
    for b in json.load(sys.stdin):
        ls = b.get('"'"'labels'"'"') or []
        if any(l.startswith('"'"'ref:'"'"') for l in ls): continue
        ref = b.get('"'"'external_ref'"'"') or '"'"''"'"'
        if ref:
            print(b.get('"'"'id'"'"','"'"''"'"'), ref, sep='"'"'\t'"'"')
except: pass
')
    printf 'backfilled %d, skipped (already labelled) — rerun %s list to confirm\n' "$n" "$0"
    [ "$e" -eq 0 ] || printf 'errors on %d beads — check %s\n' "$e" "$ILOG"
    ;;

retire-unsatisfiable-delivers)
    # MIGRATION PASS — removes delivers: labels that name a path no session on THIS install
    # could ever write. A delivers: label is a CLOSING CRITERION: the sentinel reopens a
    # bead closed without a commit unless the named path was written during the session. So
    # an unreachable path is not a stale annotation, it is a bead that cannot be closed by
    # the route the label describes.
    #
    # Two ways they got here, both live at once before this ran. Beads filed when the writer
    # stamped the ledger unconditionally named a file under a directory nothing created.
    # Beads that travelled between machines carried an ABSOLUTE path belonging to another
    # install's layout, which no session here can satisfy at all.
    #
    # The writer is fixed (see the delivers block in file_incident) so this does not need to
    # run again; it exists because a closed incident bead is REOPENED by dedup rather than
    # refiled, which would otherwise resurrect the bad label along with the bead.
    #
    # ONLY UNREACHABLE ONES. A delivers: label under this install's run directory is a
    # working criterion and is left exactly alone — the point is to retire what cannot be
    # met, never to make beads look tidy.
    shift
    _dry=0; [ "${1:-}" = "--dry-run" ] && _dry=1
    n=0; k=0
    while IFS=$'\t' read -r bid lab; do
        [ -n "$bid" ] && [ -n "$lab" ] || continue
        _path="${lab#delivers:note:}"
        case "$_path" in
            "${SPIRA_RUN}"/*)
                # Under this install AND its directory already there: a session here can
                # write it, so the criterion works and is left alone.
                #
                # THIS BRANCH ONLY ASKS. Two earlier versions ran mkdir -p here and both
                # were wrong. It made --dry-run mutate, so the dry run created the
                # directories and then reported every label reachable — and the real run
                # then retired nothing while the dry run had promised six, which is the
                # worst thing a migration can do. And the repair itself was a mirage: the
                # sentinel requires the ledger's MTIME to move during the session, so an
                # empty directory satisfies nothing. Creating it belongs in the writer,
                # where a session that runs sop.sh can actually fill it.
                [ -d "$(dirname "$_path")" ] && { k=$((k+1)); continue; } ;;
        esac
        if [ "$_dry" = 1 ]; then
            printf '  would drop %-9s %s\n' "$bid" "$lab"
        else
            bdq label remove "$bid" "$lab" >/dev/null 2>&1 \
                && ilog "delivers: $bid: retired unsatisfiable $lab"
        fi
        n=$((n+1))
    done < <(bdq list --status open,in_progress --limit 0 --json 2>/dev/null \
      | python3 -c '
import sys, json
try:
    for b in json.load(sys.stdin):
        for l in (b.get("labels") or []):
            if l.startswith("delivers:note:"):
                print(b.get("id",""), l, sep="\t")
except Exception:
    pass
')
    if [ "$_dry" = 1 ]; then
        printf 'would retire %d label(s); %d already reachable and left alone\n' "$n" "$k"
    else
        printf 'retired %d unsatisfiable label(s); %d already reachable and left alone\n' "$n" "$k"
    fi
    ;;

backfill-recur-causes)
    # MIGRATION PASS — converts bare sp-recur-N labels to sp-recur-N-unrecorded.
    # Every recurrence rung must carry a cause; older code wrote sp-recur-N without one.
    # The cause cannot be recovered from the label alone, so each bare rung is promoted to
    # sp-recur-N-unrecorded: explicitly unrecoverable rather than silently left blank.
    # Safe to re-run: beads whose every sp-recur-N rung already carries a suffix are
    # reported as skipped. Only open and recently-closed beads are processed — older
    # closed beads carry bare labels that counter_causes already maps to "unrecorded",
    # so the census query groups them correctly without backfill.
    #
    # EXCLUDE repo: FROM THE QUERY LABEL — a bead filed without a repo: label (or with a
    # different one) would be missed if LABELS included repo:. The dedupe key is the
    # non-repo labels; the same strip is applied in _dedup_incident for the same reason.
    _bf_lq="$(printf '%s' "$LABELS" | tr ',' '\n' | grep -v '^repo:' | paste -sd, -)"
    _since="$(date -u -d "-${DEDUP_LOOKBACK_DAYS} days" '+%Y-%m-%d' 2>/dev/null \
           || date -u -v "-${DEDUP_LOOKBACK_DAYS}d" '+%Y-%m-%d' 2>/dev/null || true)"
    n=0 e=0 s=0
    _process_backfill() {
        local bid="$1" rungs_csv="$2"
        local ok=0 err=0
        IFS=',' read -ra rungs <<< "$rungs_csv"
        for rung in "${rungs[@]}"; do
            [ -n "$rung" ] || continue
            # Add the typed label first; only remove the bare one when it succeeded.
            if bdq label add "$bid" "${rung}-unrecorded" >/dev/null 2>&1; then
                bdq label remove "$bid" "$rung" >/dev/null 2>&1 || true
                ok=$((ok+1))
            else
                err=$((err+1))
            fi
        done
        if [ "$err" -gt 0 ]; then e=$((e+1))
        elif [ "$ok" -gt 0 ]; then n=$((n+1))
        else s=$((s+1)); fi
    }
    _bare_recur_beads() {
        python3 -c '
import sys, json, re
try:
    for b in json.load(sys.stdin):
        bare = [l for l in (b.get("labels") or []) if re.match(r"^sp-recur-\d+$", l)]
        if bare:
            print(b["id"], ",".join(bare), sep="\t")
except: pass
'
    }
    while IFS=$'\t' read -r bid rungs_csv; do
        [ -n "$bid" ] || continue
        _process_backfill "$bid" "$rungs_csv"
    done < <(
        bdq list --status open,in_progress --limit 0 --label "$_bf_lq" --json 2>/dev/null \
            | _bare_recur_beads
        [ -n "$_since" ] && \
        bdq list --status closed --closed-after "$_since" --limit 0 --label "$_bf_lq" --json 2>/dev/null \
            | _bare_recur_beads || true
    )
    printf 'backfilled %d bead(s), errors %d, skipped %d (already typed)\n' "$n" "$e" "$s"
    [ "$e" -eq 0 ]
    ;;

*) sed -n '3,9p' "$0" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
