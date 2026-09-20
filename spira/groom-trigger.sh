#!/usr/bin/env bash
#
# groom-trigger.sh — file a trigger bead for the groomer persona.
#
# The groomer persona is a lane fayth: it draws from its own FAYTH_MAX_CONCURRENT
# slot and is woken by trigger beads carrying SPIRA_GROOMER_LABEL. This script
# files one such bead on a cadence (driven by spira-groom.timer), deduplicating
# so at most one open trigger exists at a time.
#
# DEDUP. An open trigger bead means a groom pass is either waiting to be claimed
# or actively running. Filing a second one while the first is still open would
# queue a redundant pass; the groomer's FAYTH_MAX_CONCURRENT=1 makes such a queue
# permanent — a lane with one slot and two trigger beads means the second trigger
# waits forever after the first, building an ever-growing backlog of noop passes.
# One open trigger is the correct steady state; this guard enforces it.
#
# SCOPE. The trigger bead carries SPIRA_SCOPE_LABEL + SPIRA_GROOMER_LABEL so the
# groomer's FAYTH_LABELS predicate selects it. One definition of the label keeps
# the fayth, the trigger, and the dedup query all consistent — changing the label
# in conf.sh changes all three.
#
# EXIT:
#   0  trigger bead filed, or an open trigger already exists (dedup) — either is
#      the correct outcome; the groomer will run when the sentinel next checks.
#   1  error filing the bead
#
# covers: spira/groom-trigger.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"

BD="${SPIRA_BD:-bd}"
DB="${SPIRA_DB:-.}"

log() { printf '%s groom-trigger: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"; }

# PARTITION LABELS. SPIRA_SCOPE_LABEL defaults to "spira"; empty means no scope
# restriction. The groomer's FAYTH_LABELS uses the same expansion so the trigger
# lands in exactly the partition the groomer queries.
if [ -n "${SPIRA_SCOPE_LABEL:-}" ]; then
    LABELS="${SPIRA_SCOPE_LABEL},${SPIRA_GROOMER_LABEL}"
else
    LABELS="${SPIRA_GROOMER_LABEL}"
fi

# DEDUP — at most one open trigger bead at a time. The query uses the same labels
# the groomer's predicate uses; a bead present here is one the groomer will claim.
# bd list --json returns a JSON array; [] means nothing open, [...] means at least
# one. The python3 count is borrowed from lib.sh's ready_count rather than
# reimplemented in a way that might drift.
open_count=0
open_json="$("$BD" -C "$DB" list --status open --label "$LABELS" --json 2>/dev/null)" || open_json="[]"
# An empty string means bd succeeded but returned nothing — treat as no results.
[ -z "$open_json" ] && open_json="[]"
open_count="$(printf '%s\n' "$open_json" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d))' 2>/dev/null)" || open_count=0

if [ "${open_count:-0}" -gt 0 ] 2>/dev/null; then
    log "trigger already open ($open_count bead(s) with labels [$LABELS]) — skipping"
    exit 0
fi

# LANE CHECK. Skip when no repository admits the groom lane — on a consuming install
# this prevents trigger beads from accumulating for work nobody can do.
_gr_lane="${SPIRA_GROOMER_LABEL:-groom}"  # literal-ok: bash fallback; SPIRA_GROOMER_LABEL set by conf.sh
_lane_admitted=0
_hr="$(spira_home_repo 2>/dev/null)" || _hr=""
if [ -n "$_hr" ]; then
    _hl="$(spira_repo_lanes "$_hr" 2>/dev/null)" || _hl=""
    case " $_hl " in *" $_gr_lane "*) _lane_admitted=1 ;; esac
fi
if [ "$_lane_admitted" = 0 ] && [ -f "${SPIRA_REPO_MAP:-}" ]; then
    while IFS='|' read -r _rn _rest; do
        _rn="${_rn#"${_rn%%[![:space:]]*}"}"; _rn="${_rn%"${_rn##*[![:space:]]}"}"
        case "${_rn:-}" in ''|'#'*) continue ;; esac
        _rl="$(spira_repo_lanes "$_rn" 2>/dev/null)" || continue
        case " $_rl " in *" $_gr_lane "*) _lane_admitted=1; break ;; esac
    done < "$SPIRA_REPO_MAP"
fi
if [ "$_lane_admitted" = 0 ]; then
    log "no repository admits lane ${_gr_lane} — skipping trigger"
    exit 0
fi

# SHORT-CIRCUIT PREDICATE. Skip when the graph has not changed enough since the last
# pass to be worth examining. Score = total open bead count + landings since the last
# groom pass. Below SPIRA_GROOM_THRESHOLD the pass would cost a context to print
# "Actions: none". A missing lastpass file yields ts=0 (never ran) so the trigger
# always fires on first run.
_lp_file="${SPIRA_RUN}/groom.lastpass"
_lp_ts=0
if [ -f "$_lp_file" ]; then
    _lp_raw="$(cat "$_lp_file" 2>/dev/null | tr -d '[:space:]' || true)"
    case "${_lp_raw:-}" in
        ''|*[!0-9]*) _lp_ts=0 ;;
        *) _lp_ts="$_lp_raw" ;;
    esac
fi

_total_json="$("$BD" -C "$DB" list --status open,in_progress --json 2>/dev/null)" || _total_json="[]"
[ -z "$_total_json" ] && _total_json="[]"
_total_open="$(printf '%s\n' "$_total_json" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d))' 2>/dev/null)" || _total_open=0

_land_count=0
_hr="$(spira_home_repo 2>/dev/null)" || _hr=""
_hr_path="$(repo_root "$_hr" 2>/dev/null)" || _hr_path=""
if [ -d "${_hr_path:-}" ]; then
    _base_ref="$(spira_landref "$_hr" 2>/dev/null)" || _base_ref=""
    if [ -n "$_base_ref" ]; then
        _land_count="$(git -C "$_hr_path" log --format='%s' --after="@${_lp_ts}" "$_base_ref" 2>/dev/null \
            | awk '
                /^spira: land / { rest=substr($0,13); if (match(rest,/^[a-z0-9]+-[a-z0-9]+/)) { id=substr(rest,RSTART,RLENGTH); if (!seen[id]++) print id }; next }
                /spira\// { if (match($0,/spira\/[a-z0-9]+-[a-z0-9]+/)) { id=substr($0,RSTART+6,RLENGTH-6); if (!seen[id]++) print id }; next }
                /^[a-z0-9]+-[a-z0-9]+:/ { if (match($0,/^[a-z0-9]+-[a-z0-9]+/)) { id=substr($0,RSTART,RLENGTH); if (!seen[id]++) print id } }
            ' \
            | wc -l | tr -d '[:space:]')" || _land_count=0
    fi
fi

_score=$(( ${_total_open:-0} + ${_land_count:-0} ))
_threshold="${SPIRA_GROOM_THRESHOLD:-5}"
if [ "$_score" -lt "$_threshold" ] 2>/dev/null; then
    log "no-pass: score ${_score} (open ${_total_open}, landings ${_land_count} since ts=${_lp_ts}) below threshold ${_threshold} — skipping"
    exit 0
fi

# FILE THE TRIGGER BEAD. Type task (not decision) — this is work the groomer does,
# not a question Ryan answers. Priority 3: hygiene work, not urgent, but important
# enough to run on schedule. No repo: label — the groomer reads the whole graph,
# not one repository.
if "$BD" -C "$DB" create \
    "Groomer pass — scheduled graph hygiene" \
    --type task \
    --label "$LABELS,delivers:note:${SPIRA_RUN}/groom.log" \
    --priority 3 \
    --description "Scheduled trigger: the groomer persona will claim this bead, run a hygiene pass over the open bead graph (splitting unsplittable beads, merging duplicates, closing stale premises, correcting mislabelled lanes), and close this bead when finished. See spira/chamber/groomer.md for the pass procedure." \
; then
    log "groomer trigger bead filed (labels: $LABELS,delivers:note:${SPIRA_RUN}/groom.log)"
else
    log "ERROR: failed to file groomer trigger bead"
    exit 1
fi
