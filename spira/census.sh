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
# WHY THE COVERS LABEL, NOT THE TITLE OR DESCRIPTION
# A label is a machine-readable primary key. A title is human prose and may drift from
# the class name over the bead's lifetime. Grepping a description field requires parsing
# a structured text format that can change. The covers: label is exact, short, and
# queryable without JSON parsing on the receiving side.
#
# WHY PYTHON TEMP FILES INSTEAD OF python3 - <<'PY'
# A pipe sets up stdin for the reader before the process starts. `python3 - <<'PY'`
# has the heredoc override stdin so Python reads its SCRIPT from the heredoc, not from
# the pipe — leaving the pipe writer with no reader (SIGPIPE). Writing the script to a
# temp file and invoking `python3 "$script"` keeps stdin available for the pipe.
#
# covers: spira/census.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/lib.sh"

WITH_SUPPRESSED=0
case "${1:-}" in --with-suppressed) WITH_SUPPRESSED=1 ;; esac

REMEDY_LABEL="$SPIRA_MAECHEN_REMEDY_LABEL"

_TMPDIR="$(mktemp -d)"
trap 'rm -rf "$_TMPDIR"' EXIT INT TERM

# Python: aggregate failure events → <count> <class> lines, ranked highest first.
# Reads tabular SQL output from census_events_run_sql.
# Maps event_type + new_value to the class name used throughout the census pipeline:
#   requeued  + <cause>           → sp-requeue-<cause>
#   recurred  + <cause>           → sp-recur-<cause>
#   reclaimed + (empty/unrecorded)→ sp-reclaim
#   reclaimed + <named-cause>     → sp-reclaim-<named-cause>
#   lapsed    + <cause>           → sp-lapsed-<cause>
cat > "$_TMPDIR/count.py" <<'EOF'
import sys, collections

beads = collections.defaultdict(set)
ec = collections.Counter()
for line in sys.stdin:
    line = line.rstrip('\n').strip()
    if not line or line.startswith('+') or line.startswith('('):
        continue
    parts = [p.strip() for p in line.split('|')]
    if len(parts) != 3:
        continue
    event_type, new_value, issue_id = parts[0], parts[1], parts[2]
    if event_type == 'event_type' or 'COALESCE' in event_type:
        continue
    if not issue_id:
        continue
    cause = new_value
    if event_type == 'requeued':
        cls = 'sp-requeue-' + (cause or 'unrecorded')
    elif event_type == 'recurred':
        cls = 'sp-recur-' + (cause or 'unrecorded')
    elif event_type == 'reclaimed':
        if cause and cause != 'unrecorded':
            cls = 'sp-reclaim-' + cause
        else:
            cls = 'sp-reclaim'
    elif event_type == 'lapsed':
        cls = 'sp-lapsed-' + (cause or 'unrecorded')
    elif event_type == 'reopened':
        cls = 'sp-reopen'
    else:
        continue
    beads[cls].add(issue_id)
    ec[cls] += 1

bc = {cls: len(ids) for cls, ids in beads.items()}
for cls, nb in sorted(bc.items(), key=lambda x: (-x[1], -ec.get(x[0], 0))):
    print(nb, ec[cls], cls)
EOF

# Python: merge all-time and since-watermark counts, rank by since-watermark.
# Reads two "N class" files; outputs "N class (M all-time)" ranked by N descending.
cat > "$_TMPDIR/merge.py" <<'EOF'
import sys

def read_counts(path):
    beads = {}
    events = {}
    try:
        with open(path) as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                parts = line.split(None, 2)
                if len(parts) == 3:
                    try:
                        beads[parts[2]] = int(parts[0])
                        events[parts[2]] = int(parts[1])
                    except ValueError:
                        pass
    except Exception:
        pass
    return beads, events

all_beads, all_events = read_counts(sys.argv[1])
wm_beads,  wm_events  = read_counts(sys.argv[2])
all_classes = set(all_beads) | set(wm_beads)

ranked = sorted(all_classes, key=lambda c: (-wm_beads.get(c, 0), -wm_events.get(c, 0)))
for cls in ranked:
    b_since = wm_beads.get(cls, 0)
    e_since = wm_events.get(cls, 0)
    b_all   = all_beads.get(cls, 0)
    print('{} {} ({} detections, {} all-time)'.format(b_since, cls, e_since, b_all))
EOF

# Python: extract covered classes from open remedy beads → one class per line.
cat > "$_TMPDIR/covers.py" <<'EOF'
import sys, json
try:
    data = json.load(sys.stdin)
except Exception:
    sys.exit(0)
if not isinstance(data, list):
    data = [data]
for b in data:
    for lbl in (b.get("labels") or []):
        if lbl.startswith("covers:"):
            print(lbl[len("covers:"):])
EOF

# Python: closed remedy beads → "<bead-id> <class>" within SPIRA_REMEDY_WINDOW days.
cat > "$_TMPDIR/covers_closed.py" <<EOF
import sys, json
from datetime import datetime, timezone, timedelta
window = ${SPIRA_REMEDY_WINDOW:-30}
cutoff = datetime.now(timezone.utc) - timedelta(days=window)
try:
    data = json.load(sys.stdin)
except Exception:
    sys.exit(0)
if not isinstance(data, list):
    data = [data]
for b in data:
    closed_at = b.get('closed_at') or ''
    if closed_at:
        try:
            dt = datetime.fromisoformat(closed_at.replace('Z', '+00:00'))
            if dt < cutoff:
                continue
        except Exception:
            pass
    bid = b.get('id') or ''
    if not bid:
        continue
    for lbl in (b.get('labels') or []):
        if lbl.startswith('covers:'):
            print(bid, lbl[len('covers:'):])
EOF

# Aggregate failure events across the whole store, output <count> <class> ranked.
# census_events_run_sql is defined in lib.sh (sourced above); it queries the events
# table via bd sql (sp-2lk).
_census_raw() {
    census_events_run_sql | python3 "$_TMPDIR/count.py"
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

# Build the ranked census: since-watermark when watermark is valid, all-time otherwise.
# _census_raw exits non-zero when the events substrate is unreachable; fail closed rather
# than report zero classes — a blind census is indistinguishable from a clean one.
if ! _census_raw > "$_TMPDIR/all_time.txt"; then
    printf 'census.sh: events substrate is unreachable — cannot produce a census\n' >&2
    exit 1
fi
if [ "$_watermark_ts" -gt 0 ] 2>/dev/null; then
    if ! census_events_run_sql "$_watermark_ts" | python3 "$_TMPDIR/count.py" > "$_TMPDIR/since_wm.txt"; then
        printf 'census.sh: events substrate is unreachable — cannot produce a census\n' >&2
        exit 1
    fi
    _RANKED="$(python3 "$_TMPDIR/merge.py" "$_TMPDIR/all_time.txt" "$_TMPDIR/since_wm.txt")"
else
    _RANKED="$(awk '{print $1, $3, "(" $2 " detections)"}' "$_TMPDIR/all_time.txt")"
fi

# Collect classes already covered by an open remedy bead.
_suppressed_classes() {
    bdq list --status open,in_progress,blocked,deferred --label "$REMEDY_LABEL" --json 2>/dev/null \
        | python3 "$_TMPDIR/covers.py"
}

# Collect classes covered by a closed remedy bead whose commit is not yet on the base.
_suppressed_closed_classes() {
    local bead_id class rc
    bdq list --status closed --label "$REMEDY_LABEL" --json 2>/dev/null \
        | python3 "$_TMPDIR/covers_closed.py" \
        | while IFS=' ' read -r bead_id class; do
            landed "$bead_id"; rc=$?
            case $rc in
                1) printf '%s\n' "$class" ;;
                2) printf 'census.sh: remedy %s: land ref unresolvable, keeping suppression for %s\n' \
                       "$bead_id" "$class" >&2
                   printf '%s\n' "$class" ;;
            esac
        done
}

# Build suppressed sets as newline-delimited files for grep -xF membership tests.
_suppressed_classes > "$_TMPDIR/suppressed.txt"
_suppressed_closed_classes > "$_TMPDIR/suppressed_closed.txt"

# Emit the ranked census, suppressing (or annotating) remedy-covered classes.
# IFS=' ' with read -r splits "N class (M all-time)" into: count, class, rest.
while IFS=' ' read -r count class rest; do
    [ -n "$class" ] || continue
    if grep -qxF "$class" "$_TMPDIR/suppressed.txt" 2>/dev/null; then
        [ "$WITH_SUPPRESSED" -eq 1 ] && printf '%s %s%s [suppressed]\n' \
            "$count" "$class" "${rest:+ $rest}"
    elif grep -qxF "$class" "$_TMPDIR/suppressed_closed.txt" 2>/dev/null; then
        [ "$WITH_SUPPRESSED" -eq 1 ] && printf '%s %s%s [suppressed: remedy closed, not landed]\n' \
            "$count" "$class" "${rest:+ $rest}"
    else
        printf '%s %s%s\n' "$count" "$class" "${rest:+ $rest}"
    fi
done <<< "$_RANKED"
