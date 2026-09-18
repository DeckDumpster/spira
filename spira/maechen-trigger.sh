#!/usr/bin/env bash
#
# maechen-trigger.sh — evaluate the two Maechen trigger conditions and file a sweep
# bead when either fires, with idempotent deduplication.
#
#   LANDING TRIGGER: counts commits on every managed repo's base branch whose subject
#     names a bead id, since the watermark timestamp. Fires when the count reaches
#     SPIRA_MAECHEN_LANDING_INTERVAL. A landing is a commit whose SUBJECT names a bead
#     id (law-landed-is-content). Bead status is never consulted.
#
#   TIME TRIGGER: fires when more than SPIRA_MAECHEN_MAX_GAP_SECONDS have elapsed since
#     the last completed pass ($SPIRA_RUN/maechen.lastpass), even if the landing volume
#     threshold has not been reached.
#
# TWO CLOCKS, NOT ONE. The watermark ($SPIRA_RUN/maechen.watermark) bounds the census
# query window — it tells census.sh how far back to look, and must not advance when a
# pass cannot read the substrate (db-93g). The time trigger cadence must be governed
# by when a pass last ran, not by whether the census could be read: if the substrate is
# temporarily unreadable every pass holds the watermark, elapsed since watermark grows
# without bound, and the fire condition becomes permanently true. maechen.lastpass is
# written by the Maechen pass on every completion (success or failure), so the time
# trigger gates on the pass cadence, not the census window.
#
# DEDUP. At most one open-or-in-progress trigger bead at a time. maechen.fayth has
# FAYTH_MAX_CONCURRENT=1; a second trigger bead would wait forever behind the first,
# building an ever-growing backlog of no-op passes. This guard enforces one open or
# in-progress trigger as the correct steady state.
#
# WATERMARK. $SPIRA_RUN/maechen.watermark holds a single integer: the Unix epoch
# timestamp of the last window the Maechen pass examined. A missing or empty file means
# epoch 0 (never fired), which guarantees the time trigger fires on the very first run.
# The trigger does NOT advance the watermark — the Maechen PASS advances it in its
# final step, after running census.sh. This ordering is load-bearing: census.sh reads
# the watermark to bound its since-watermark query, so an advance before the census
# would cause census to examine a window starting after the events that fired the
# trigger — reporting 0 classes while the trigger window held the offenders. The dedup
# guard (above) already covers the crash-between-trigger-and-bead case: a crash leaves
# the watermark unchanged and no bead open; the next run re-fires and re-files.
#
# LANDING COUNT. Counts commits on the remote-tracking base ref of each managed repo
# whose SUBJECT begins with the bead id prefix (SPIRA_ID_PREFIX, default "sp"). The
# home repo is always counted; additional repos are read from SPIRA_REPO_MAP. A repo
# whose base ref cannot be resolved is skipped with a log line.
#
# EXIT:
#   0  bead filed, or an open trigger already exists (dedup) — either is correct
#   1  error writing watermark or filing the bead
#
# covers: spira/maechen-trigger.sh spira/conf.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"

BD="${SPIRA_BD:-bd}"
DB="${SPIRA_DB:-.}"
WATERMARK_FILE="${SPIRA_RUN}/maechen.watermark"
LASTPASS_FILE="${SPIRA_RUN}/maechen.lastpass"

log() { printf '%s maechen-trigger: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >&2; }

# PARTITION LABELS. The trigger bead carries SPIRA_SCOPE_LABEL (if non-empty) and
# SPIRA_MAECHEN_LABEL so maechen.fayth's FAYTH_LABELS predicate selects it. The same
# expansion is used here for the dedup query so the two never disagree.
if [ -n "${SPIRA_SCOPE_LABEL:-}" ]; then
    LABELS="${SPIRA_SCOPE_LABEL},${SPIRA_MAECHEN_LABEL}"
else
    LABELS="${SPIRA_MAECHEN_LABEL}"
fi

# DEDUP — at most one open-or-in-progress trigger bead at a time. Query uses the same
# labels as maechen.fayth's predicate. in_progress is included because a claimed bead
# leaves --status open and the guard would file a duplicate on the next tick (sp-mp9s).
open_count=0
open_json="$("$BD" -C "$DB" list --status open,in_progress --label "$LABELS" --json 2>/dev/null)" || open_json="[]"
[ -z "$open_json" ] && open_json="[]"
open_count="$(printf '%s\n' "$open_json" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d))' 2>/dev/null)" \
    || open_count=0

if [ "${open_count:-0}" -gt 0 ] 2>/dev/null; then
    log "trigger already open or in_progress (${open_count} bead(s) with labels [$LABELS]) — skipping"
    exit 0
fi

# READ THE WATERMARK. A missing or empty file yields epoch 0 (trigger fires immediately).
watermark_ts=0
if [ -f "$WATERMARK_FILE" ]; then
    _raw="$(cat "$WATERMARK_FILE" 2>/dev/null | tr -d '[:space:]' || true)"
    case "${_raw:-}" in
        ''|*[!0-9]*) watermark_ts=0 ;;
        *) watermark_ts="$_raw" ;;
    esac
fi

# READ THE LAST-PASS STAMP. A missing or empty file yields epoch 0 (trigger fires
# immediately on first run or after a pass has never completed). The pass writes this
# file on every completion — success or failure — so the time trigger measures how long
# ago a pass ran, independently of whether the census could read the substrate.
lastpass_ts=0
if [ -f "$LASTPASS_FILE" ]; then
    _lp_raw="$(cat "$LASTPASS_FILE" 2>/dev/null | tr -d '[:space:]' || true)"
    case "${_lp_raw:-}" in
        ''|*[!0-9]*) lastpass_ts=0 ;;
        *) lastpass_ts="$_lp_raw" ;;
    esac
fi

now_ts="$(date +%s)"
elapsed=$(( now_ts - lastpass_ts ))

# TIME TRIGGER. Fire when more than SPIRA_MAECHEN_MAX_GAP_SECONDS have elapsed since
# the last pass. Reads lastpass, not the watermark — the watermark bounds census.sh's
# query window and must not advance when the substrate is unreadable, but that must not
# stop the time trigger from knowing that a pass happened.
time_trigger=0
if [ "$elapsed" -ge "${SPIRA_MAECHEN_MAX_GAP_SECONDS:-10800}" ]; then
    time_trigger=1
    log "time trigger: ${elapsed}s elapsed since last pass (threshold: ${SPIRA_MAECHEN_MAX_GAP_SECONDS:-10800}s)"
fi

# LANDING TRIGGER. Count commits naming a bead id since the watermark.
#
# PATTERN: subjects beginning with ${SPIRA_ID_PREFIX}-[a-z0-9]. Anchored at the start
# of the subject so a description like "add sp-notation" does not count as a landing.
#
# PER-REPO: home repo is always included. Additional repos are read from the repo-map.
# Process substitution (<(...)) avoids pipefail propagating grep's exit 1 on no matches;
# the bash `case` pattern match always exits 0.
landing_count=0
id_prefix="${SPIRA_ID_PREFIX:-sp}"

_count_landings() {   # _count_landings <repo_path_or_name> <since_ts>
    local rp="$1" ts="$2" base_ref="" rpath="" n=0 subject
    base_ref="$(spira_landref "$rp" 2>/dev/null)" || base_ref=""
    if [ -z "$base_ref" ]; then
        log "landing count: cannot resolve base ref for $rp — skipped"
        printf '0'; return 0
    fi
    case "$rp" in
        */*) rpath="$rp" ;;
        *)   rpath="$(repo_root "$rp" 2>/dev/null)" || rpath="" ;;
    esac
    while IFS= read -r subject; do
        case "$subject" in "${id_prefix}-"*) n=$(( n + 1 )) ;; esac
    done < <(git -C "$rpath" log --format='%s' --after="@${ts}" "$base_ref" 2>/dev/null || true)
    printf '%d' "$n"
}

# _add_landings <n> — guard against a non-numeric _n before touching landing_count.
# A future log() stdout leak would otherwise abort the enclosing loop; with this guard it
# degrades to an undercount rather than a loop exit.
_add_landings() {
    local _n="$1"
    case "$_n" in *[!0-9]*) log "WARNING: non-numeric landing count [${_n}] — skipping"; return 0 ;; esac
    landing_count=$(( landing_count + _n ))
}

# Home repo — always present.
_home_repo="$(spira_home_repo)"
_home_path="$(repo_root "$_home_repo" 2>/dev/null)" || _home_path=""
if [ -d "${_home_path:-}" ]; then
    _n="$(_count_landings "$_home_repo" "$watermark_ts")"
    _add_landings "$_n"
fi

# Additional repos from the repo-map, skipping the home repo to avoid double-counting.
if [ -f "${SPIRA_REPO_MAP:-}" ]; then
    while IFS='|' read -r _nm _rp _rest; do
        # Strip surrounding whitespace from name and path.
        _nm="${_nm#"${_nm%%[![:space:]]*}"}"; _nm="${_nm%"${_nm##*[![:space:]]}"}"
        _rp="${_rp#"${_rp%%[![:space:]]*}"}"; _rp="${_rp%"${_rp##*[![:space:]]}"}"
        case "${_nm:-}" in ''|'#'*) continue ;; esac
        [ -n "$_rp" ] || continue
        [ "$_nm" = "$_home_repo" ] && continue   # already counted
        [ -d "$_rp" ] || continue
        _n="$(_count_landings "$_rp" "$watermark_ts")"
        _add_landings "$_n"
    done < "$SPIRA_REPO_MAP"
fi

landing_trigger=0
if [ "$landing_count" -ge "${SPIRA_MAECHEN_LANDING_INTERVAL:-25}" ]; then
    landing_trigger=1
    log "landing trigger: ${landing_count} landings since watermark (threshold: ${SPIRA_MAECHEN_LANDING_INTERVAL:-25})"
fi

# NEITHER TRIGGER — nothing to do.
if [ "$time_trigger" = 0 ] && [ "$landing_trigger" = 0 ]; then
    log "no trigger: ${landing_count} landings (threshold: ${SPIRA_MAECHEN_LANDING_INTERVAL:-25}), ${elapsed}s since last pass (threshold: ${SPIRA_MAECHEN_MAX_GAP_SECONDS:-10800}s)"
    exit 0
fi

# BUILD THE TRIGGER REASON for the bead title and description.
trigger_reason=""
[ "$time_trigger"    = 1 ] && trigger_reason="${trigger_reason}${trigger_reason:+, }${elapsed}s elapsed"
[ "$landing_trigger" = 1 ] && trigger_reason="${trigger_reason}${trigger_reason:+, }${landing_count} landings"

if "$BD" -C "$DB" create \
    "Maechen pass — ${trigger_reason}" \
    --type task \
    --label "$LABELS,delivers:note:${SPIRA_RUN}/maechen.log" \
    --priority 3 \
    --description "Scheduled trigger: the Maechen persona will claim this bead, run a retrospective pass over the failure distribution, identify recurring failure classes, and cut at most ${SPIRA_MAECHEN_MAX_BEADS:-3} remedy beads. Trigger: ${trigger_reason} (lastpass ts=${lastpass_ts}, watermark ts=${watermark_ts}). See spira/chamber/maechen.md for the pass procedure." \
; then
    log "Maechen trigger bead filed (labels: $LABELS,delivers:note:${SPIRA_RUN}/maechen.log, reason: ${trigger_reason})"
else
    log "ERROR: failed to file Maechen trigger bead"
    exit 1
fi
