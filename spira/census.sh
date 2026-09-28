#!/usr/bin/env bash
#
# census.sh — Maechen census: failure classes ranked by frequency, with open-remedy suppression.
#
#   census.sh [--with-suppressed]
#
# Output: one line per class, "<distinct-beads> <class> (<events> detections)", ranked by
# distinct-bead count. A class with an open remedy bead (carrying labels
# "$SPIRA_MAECHEN_REMEDY_LABEL" AND "covers:<class>") is suppressed: omitted by default,
# annotated "[suppressed]" when --with-suppressed is given.
#
# CLASS EXTRACTION — event_type + new_value map to a class name:
#   recurred  + suite-red    → sp-recur-suite-red
#   recurred  + unrecorded   → sp-recur-unrecorded
#   requeued  + prod-dirty   → sp-requeue-prod-dirty
#   reclaimed + (empty)      → sp-reclaim
#   reopened  + merge-conflict → sp-reopen-merge-conflict
#   reopened  + (empty)      → sp-reopen-unrecorded
#
# The first number on each output line is DISTINCT BEADS — the count of unique incident
# beads that produced events of that class. A single condition re-detected by a timer
# writes many event rows for one bead; those count as one, not many. The detection count
# (total event rows) appears in parentheses and is the right instrument for measuring
# how long a condition went unresolved.
#
# REMEDY SUPPRESSION — a class is suppressed while its covers-label appears on an
# open remedy bead, or a closed remedy bead whose commit is not yet on the base.
# Closed remedies beyond SPIRA_REMEDY_WINDOW days are excluded from the check.
#
# ACTOR FILTER — the ranked block only counts events written by harness or an aeon-*
# session (_census_events_sql in lib.sh). An operator's hand-written events-table row
# (e.g. an attempt-ledger offset) is real state, not a failure class, and is listed
# instead under a trailing "hand-written (not ranked):" line when --with-suppressed
# is given (law-absence-needs-a-positive-control).
#
# WHY THE COVERS LABEL, NOT THE TITLE OR DESCRIPTION
# A label is a machine-readable primary key. A title is human prose and may drift from
# the class name over the bead's lifetime. Grepping a description field requires parsing
# a structured text format that can change. The covers: label is exact, short, and
# queryable without JSON parsing on the receiving side.
#
# covers: spira/census.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/lib.sh"
CENSUS_PY="$HERE/census"

WITH_SUPPRESSED=0
case "${1:-}" in --with-suppressed) WITH_SUPPRESSED=1 ;; esac

REMEDY_LABEL="$SPIRA_MAECHEN_REMEDY_LABEL"

_TMPDIR="$(mktemp -d)"
trap 'rm -rf "$_TMPDIR"' EXIT INT TERM

# Aggregate failure events across the whole store, output <count> <class> ranked.
# census_events_run_sql is defined in lib.sh (sourced above); it queries the events
# table via bd sql (sp-2lk). count.py's mapping and column-shift handling are table-
# tested directly on canned rows in test-census-pipeline.sh.
_census_raw() {
    census_events_run_sql | python3 "$CENSUS_PY/count.py"
}

# READ WATERMARK. $SPIRA_RUN/maechen.watermark holds a Unix epoch integer written by
# maechen-trigger.sh. When present and valid, census ranks by since-watermark count and
# shows both counts: "N class (M all-time)". When absent or unreadable, fall back to
# all-time counts (same format as before) and say so on stderr so the caller knows the
# ranking is all-time rather than since-watermark.
_watermark_ts=0
_watermark_file="${SPIRA_RUN}/maechen.watermark"
if [ -f "$_watermark_file" ]; then
    _wm_raw="$(cat "$_watermark_file" 2>/dev/null | tr -d '[:space:]' || true)"
    case "${_wm_raw:-}" in
        ''|*[!0-9]*)
            printf 'census.sh: watermark at %s is unreadable; falling back to all-time counts\n' \
                "$_watermark_file" >&2 ;;
        *) _watermark_ts="$_wm_raw" ;;
    esac
else
    printf 'census.sh: no watermark file at %s; reporting all-time counts\n' \
        "$_watermark_file" >&2
fi

# CLOCK SKEW GUARD (law-a-control-that-cannot-check-must-refuse). The window above is
# bounded by true UTC, but every event it counts is stamped by the substrate's own
# UTC_TIMESTAMP(). If that clock disagrees with UTC (sp-yyih8: a dolt server with no TZ
# set returns local wall clock labelled UTC), the events within the skew are invisible
# and a windowed count over them is not a ranking — rc=0 with an all-zero table looks
# exactly like a quiet graph. Refuse instead, and refuse the same way when the check
# itself cannot run: a control that cannot check must refuse, not pass silently.
#
# Measured against the host's own UTC clock (date -u), never the substrate's local wall
# clock — that clock is expected to differ from UTC_TIMESTAMP() by the host timezone
# offset on any non-UTC box (sp-9b8py), and that disagreement is not a clock fault.
_skew_tolerance="${SPIRA_CENSUS_CLOCK_SKEW_TOLERANCE_S:-120}"
_host_utc_epoch="${SPIRA_NOW:-$(date -u +%s)}"
_skew_out="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql \
    "SELECT DATE_FORMAT(UTC_TIMESTAMP(), '%Y-%m-%d %H:%i:%s') AS utc_fn" 2>&1)"
_skew_rc=$?
_skew_row="$(printf '%s\n' "$_skew_out" | sed -n '3p')"
_substrate_utc="$(printf '%s\n' "$_skew_row" | awk -F'|' '{print $1}' | sed -E 's/^ +//; s/ +$//')"
_substrate_epoch="$(date -u -d "$_substrate_utc" +%s 2>/dev/null || true)"
if [ "$_skew_rc" -ne 0 ] || [ -z "${_substrate_epoch:-}" ]; then
    printf 'census.sh: cannot verify the substrate clock against UTC — refusing to rank blind\n%s\n' \
        "$_skew_out" >&2
    exit 1
fi
_skew_s=$(( _substrate_epoch - _host_utc_epoch ))
_skew_abs=$(( _skew_s < 0 ? -_skew_s : _skew_s ))
if [ "$_skew_abs" -gt "$_skew_tolerance" ]; then
    printf 'census.sh: substrate clock skew is %ss (tolerance %ss) — %s\n' \
        "$_skew_s" "$_skew_tolerance" \
        "refusing to rank a windowed query over a clock that disagrees with UTC" >&2
    printf 'census.sh: substrate utc_fn=%s host_utc=%s\n' \
        "$_substrate_utc" "$(date -u -d "@$_host_utc_epoch" '+%Y-%m-%d %H:%M:%S')" >&2
    exit 1
fi

# Build the ranked census: since-watermark when watermark is valid, all-time otherwise.
# _census_raw exits non-zero when the events substrate is unreachable; fail closed rather
# than report zero classes — a blind census is indistinguishable from a clean one.
if ! _census_raw > "$_TMPDIR/all_time.txt"; then
    printf 'census.sh: events substrate is unreachable — cannot produce a census\n' >&2
    exit 1
fi
if [ "$_watermark_ts" -gt 0 ] 2>/dev/null; then
    if ! census_events_run_sql "$_watermark_ts" | python3 "$CENSUS_PY/count.py" > "$_TMPDIR/since_wm.txt"; then
        printf 'census.sh: events substrate is unreachable — cannot produce a census\n' >&2
        exit 1
    fi
    _RANKED="$(python3 "$CENSUS_PY/merge.py" "$_TMPDIR/all_time.txt" "$_TMPDIR/since_wm.txt")"
else
    _RANKED="$(awk '{print $1, $3, "(" $2 " detections)"}' "$_TMPDIR/all_time.txt")"
fi

# Build the class fold map so covers: labels using pre-fold names suppress correctly.
_census_class_fold_map > "$_TMPDIR/fold_map.txt"

# Collect classes already covered by an open remedy bead.
_suppressed_classes() {
    bdq list --status open,in_progress,blocked,deferred --label "$REMEDY_LABEL" --json 2>/dev/null \
        | python3 "$CENSUS_PY/covers.py" "$_TMPDIR/fold_map.txt"
}

# Collect classes covered by a closed remedy bead whose commit is not yet on the base.
# Processes closed remedy beads to classify them as in-flight or orphaned.
# Outputs classification markers to avoid buffering issues with file appends in subshells:
#   "suppressed <class>" for in-flight (branch exists)
#   "orphaned <bead_id> <class>" for orphaned (no branch)
_suppressed_closed_classes() {
    local bead_id class rc _repo
    _repo="$(repo_root)"
    bdq list --status closed --label "$REMEDY_LABEL" --json 2>/dev/null \
        | python3 "$CENSUS_PY/covers_closed.py" "$_TMPDIR/fold_map.txt" \
        | while IFS=' ' read -r bead_id class; do
            landed "$bead_id"; rc=$?
            case $rc in
                1)
                    if git -C "$_repo" branch -a --list "*${bead_id}*" 2>/dev/null \
                           | grep -q .; then
                        printf 'suppressed %s\n' "$class"
                    else
                        printf 'orphaned %s %s\n' "$bead_id" "$class"
                    fi
                    ;;
                2) printf 'census.sh: remedy %s: land status unknown, not suppressing %s\n' \
                       "$bead_id" "$class" >&2 ;;
            esac
        done
}

# Build suppressed sets as newline-delimited files for grep -xF membership tests.
_suppressed_classes > "$_TMPDIR/suppressed.txt"
: > "$_TMPDIR/suppressed_closed.txt"
: > "$_TMPDIR/orphaned_closed.txt"

# Process classified remedies, separating into suppressed and orphaned files.
while IFS=' ' read -r _type _id1 _id2; do
    case "$_type" in
        suppressed) printf '%s\n' "$_id1" >> "$_TMPDIR/suppressed_closed.txt" ;;
        orphaned) printf '%s %s\n' "$_id1" "$_id2" >> "$_TMPDIR/orphaned_closed.txt" ;;
    esac
done < <(_suppressed_closed_classes)

# Build class→"id[,id]" lookup for orphan annotations in the ranking loop below.
[ -s "$_TMPDIR/orphaned_closed.txt" ] \
    && awk '{ids[$2]=(ids[$2]?ids[$2]",":"")$1} END{for(c in ids)print c,ids[c]}' \
       "$_TMPDIR/orphaned_closed.txt" > "$_TMPDIR/orphaned_annots.txt"

# Emit the ranked census, suppressing (or annotating) remedy-covered classes.
# IFS=' ' with read -r splits "N class (M all-time)" into: count, class, rest.
while IFS=' ' read -r count class rest; do
    [ -n "$class" ] || continue
    if grep -qxF "$class" "$_TMPDIR/suppressed.txt" 2>/dev/null; then
        if [ "$WITH_SUPPRESSED" -eq 1 ]; then
            printf '%s %s%s [suppressed]\n' "$count" "$class" "${rest:+ $rest}"
        fi
    elif grep -qxF "$class" "$_TMPDIR/suppressed_closed.txt" 2>/dev/null; then
        if [ "$WITH_SUPPRESSED" -eq 1 ]; then
            printf '%s %s%s [suppressed: remedy closed, not landed]\n' \
                "$count" "$class" "${rest:+ $rest}"
        fi
    else
        _orphan=""
        [ -f "$_TMPDIR/orphaned_annots.txt" ] \
            && _orphan="$(awk -v c="$class" '$1==c{print $2;exit}' \
                          "$_TMPDIR/orphaned_annots.txt" 2>/dev/null || true)"
        if [ -n "$_orphan" ]; then
            printf '%s %s%s [orphaned remedy %s: closed, nothing in flight]\n' \
                "$count" "$class" "${rest:+ $rest}" "$_orphan"
        else
            printf '%s %s%s\n' "$count" "$class" "${rest:+ $rest}"
        fi
    fi
done <<< "$_RANKED"

# HAND-WRITTEN LEDGER CORRECTIONS — events an operator inserted directly (actor
# outside harness/aeon-*), excluded from the ranked block above by the actor
# predicate in _census_events_sql. Listed here rather than dropped silently
# (law-absence-needs-a-positive-control): a deliberate ledger correction is not a
# failure class a fix could flatline (law-a-deliberate-state-is-not-a-fault), so it
# must not compete for a Maechen selection, but it must not vanish either.
if [ "$WITH_SUPPRESSED" -eq 1 ]; then
    census_handwritten_run_sql | python3 "$CENSUS_PY/handwritten.py"
fi

exit 0
