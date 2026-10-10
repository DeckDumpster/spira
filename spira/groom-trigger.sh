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

# MUTUAL EXCLUSION (gap G10). Dedup below is list-then-create: two overlapping callers
# (the timer plus a manual run) can both read zero open triggers before either files one,
# each filing its own. strand.sh check and incident.sh needed the same flock for the same
# reason (sp-uq55c, sp-io5e); a second caller that cannot take it simply skips this tick —
# the timer runs again shortly, so losing one tick to contention is free.
exec 9>"${SPIRA_RUN:-.}/groom-trigger.lock"
if ! flock --nonblock 9; then
    log "another instance holds the lock — skipping to avoid a duplicate trigger"
    exit 0
fi

# DEDUP — at most one open-or-in_progress trigger bead at a time. The query uses the
# same labels the groomer's predicate uses; a bead present here is one the groomer will
# claim. Shared with maechen-trigger.sh via spira_open_trigger_count (duplicate cluster
# D14), which is also where in_progress got included (sp-mp9s): a claimed trigger bead
# leaves --status open, and a query scoped to open alone would file a duplicate on the
# very next tick.
if ! open_count="$(spira_open_trigger_count "$LABELS")"; then
    log "cannot count open triggers (spira-lc or bd unreadable) — refusing to file, retrying next pass"
    exit 0
fi
if [ "${open_count:-0}" -gt 0 ] 2>/dev/null; then
    log "trigger already open ($open_count bead(s) with labels [$LABELS]) — skipping"
    exit 0
fi

# LANE CHECK. Skip when no repository admits the groom lane — on a consuming install
# this prevents trigger beads from accumulating for work nobody can do. Shared with
# maechen-trigger.sh via spira_lane_admitted (duplicate cluster D14).
_gr_lane="${SPIRA_GROOMER_LABEL:-groom}"  # literal-ok: bash fallback; SPIRA_GROOMER_LABEL set by conf.sh
if ! spira_lane_admitted "$_gr_lane"; then
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

# The open count is the lifecycle machine's: every work bead whose row still owes builder
# work (READY, WORKING, REWORK — what bd's open,in_progress meant). bd status is inert for a
# work bead (sp-mve9i, design §3.4); the dedup above reads the same rows. A machine that
# cannot answer counts 0, so only landings can lift the score (never a spurious trigger).
_total_json="$("${SPIRA_LC_BIN:-spira-lc}" list --live 2>/dev/null)" || _total_json="[]"
[ -z "$_total_json" ] && _total_json="[]"
_total_open="$(printf '%s\n' "$_total_json" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print(sum(1 for r in d if r.get("state") in ("READY", "WORKING", "REWORK")))' 2>/dev/null)" || _total_open=0

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
if SPIRA_DB="$DB" SPIRA_BD="$BD" bdq create \
    "Groomer pass — scheduled graph hygiene" \
    --type task \
    --label "$LABELS,groom-trigger,delivers:note:${SPIRA_RUN}/groom.log" \
    --priority 3 \
    --description "Scheduled trigger: the groomer persona will claim this bead, run a hygiene pass over the open bead graph (splitting unsplittable beads, merging duplicates, closing stale premises, correcting mislabelled lanes), and close this bead when finished. See spira/chamber/groomer.md for the pass procedure." \
; then
    log "groomer trigger bead filed (labels: $LABELS,delivers:note:${SPIRA_RUN}/groom.log)"
else
    log "ERROR: failed to file groomer trigger bead"
    exit 1
fi
