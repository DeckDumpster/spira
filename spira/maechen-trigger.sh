#!/usr/bin/env bash
#
# maechen-trigger.sh — evaluate the two Maechen trigger conditions and file a sweep
# bead when either fires, with idempotent deduplication.
#
# Two conditions, one watermark. Both are measured from the same watermark so two
# triggers that fire within seconds of each other produce one bead, not two.
#
#   LANDING TRIGGER: counts commits on every managed repo's base branch whose subject
#     names a bead id, since the watermark timestamp. Fires when the count reaches
#     SPIRA_MAECHEN_LANDING_INTERVAL. A landing is a commit whose SUBJECT names a bead
#     id (law-landed-is-content). Bead status is never consulted.
#
#   TIME TRIGGER: fires when more than SPIRA_MAECHEN_MAX_GAP_SECONDS have elapsed since
#     the watermark, even if the landing volume threshold has not been reached.
#
# DEDUP. At most one open trigger bead at a time. maechen.fayth has
# FAYTH_MAX_CONCURRENT=1; a second open trigger bead would wait forever behind the
# first, building an ever-growing backlog of no-op passes. This guard enforces one open
# trigger as the correct steady state.
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

log() { printf '%s maechen-trigger: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >&2; }

# PARTITION LABELS. The trigger bead carries SPIRA_SCOPE_LABEL (if non-empty) and
# SPIRA_MAECHEN_LABEL so maechen.fayth's FAYTH_LABELS predicate selects it. The same
# expansion is used here for the dedup query so the two never disagree.
if [ -n "${SPIRA_SCOPE_LABEL:-}" ]; then
    LABELS="${SPIRA_SCOPE_LABEL},${SPIRA_MAECHEN_LABEL}"
else
    LABELS="${SPIRA_MAECHEN_LABEL}"
fi

# DEDUP — at most one open trigger bead at a time. Query uses the same labels as
# maechen.fayth's predicate; a bead present here is one Maechen will claim.
open_count=0
open_json="$("$BD" -C "$DB" list --status open --label "$LABELS" --json 2>/dev/null)" || open_json="[]"
[ -z "$open_json" ] && open_json="[]"
open_count="$(printf '%s\n' "$open_json" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d))' 2>/dev/null)" \
    || open_count=0

if [ "${open_count:-0}" -gt 0 ] 2>/dev/null; then
    log "trigger already open (${open_count} bead(s) with labels [$LABELS]) — skipping"
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

now_ts="$(date +%s)"
elapsed=$(( now_ts - watermark_ts ))

# TIME TRIGGER. Fire when more than SPIRA_MAECHEN_MAX_GAP_SECONDS have elapsed.
time_trigger=0
if [ "$elapsed" -ge "${SPIRA_MAECHEN_MAX_GAP_SECONDS:-10800}" ]; then
    time_trigger=1
    log "time trigger: ${elapsed}s elapsed since watermark (threshold: ${SPIRA_MAECHEN_MAX_GAP_SECONDS:-10800}s)"
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

_count_landings() {   # _count_landings <repo_path> <since_ts>
    local rp="$1" ts="$2" base_ref="" n=0 subject
    # Resolve the remote-tracking base ref via spira_landref — handles any remote name.
    # The old inline implementation hardcoded 'origin', silently zeroing counts for repos
    # whose remote has a different name (sp-3ljk). spira_landref resolves the remote name
    # dynamically: declared base in the repo-map, then the remote's own symbolic HEAD, then
    # by asking the remote once and caching the answer.
    base_ref="$(spira_landref "$rp" 2>/dev/null)" || base_ref=""
    if [ -z "$base_ref" ]; then
        log "landing count: cannot resolve base ref for $rp — skipped"
        printf '0'; return 0
    fi
    while IFS= read -r subject; do
        case "$subject" in "${id_prefix}-"*) n=$(( n + 1 )) ;; esac
    done < <(git -C "$rp" log --format='%s' --after="@${ts}" "$base_ref" 2>/dev/null || true)
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
if [ -d "${SPIRA_REPO:-}" ]; then
    _n="$(_count_landings "$SPIRA_REPO" "$watermark_ts")"
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
        [ "$_rp" = "${SPIRA_REPO:-}" ] && continue   # already counted
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
    log "no trigger: ${landing_count} landings (threshold: ${SPIRA_MAECHEN_LANDING_INTERVAL:-25}), ${elapsed}s elapsed (threshold: ${SPIRA_MAECHEN_MAX_GAP_SECONDS:-10800}s)"
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
    --description "Scheduled trigger: the Maechen persona will claim this bead, run a retrospective pass over the failure distribution, identify recurring failure classes, and cut at most ${SPIRA_MAECHEN_MAX_BEADS:-3} remedy beads. Trigger: ${trigger_reason} since watermark (ts=${watermark_ts}). See spira/chamber/maechen.md for the pass procedure." \
; then
    log "Maechen trigger bead filed (labels: $LABELS,delivers:note:${SPIRA_RUN}/maechen.log, reason: ${trigger_reason})"
else
    log "ERROR: failed to file Maechen trigger bead"
    exit 1
fi
