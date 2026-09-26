#!/usr/bin/env python3
# census/count.py — aggregate failure events into ranked class counts.
#
#   python3 count.py < census_events_run_sql-output
#
# Reads tabular SQL output (the `bd sql` result format: a leading/trailing separator
# line, a header row, and pipe-delimited data rows) from stdin and prints one
# "<distinct-beads> <detections> <class>" line per class, ranked by distinct-bead count
# then detection count, both descending.
#
# Maps event_type + new_value (already folded by the SQL — see lib.sh's
# _census_events_sql) to the class name used throughout the census pipeline:
#   requeued  + <cause>            -> sp-requeue-<cause>
#   recurred  + <cause>            -> sp-recur-<cause>
#   reclaimed + (empty/unrecorded) -> sp-reclaim
#   reclaimed + <named-cause>      -> sp-reclaim-<named-cause>
#   lapsed    + <cause>            -> sp-lapsed-<cause>
#   reopen(ed)+ <cause>            -> sp-reopen-<cause>
#
# A row's new_value column can be empty (NULL cause). Trimming empty fields BY VALUE
# instead of by position shifts a 4-column row to 3, and the 3-column branch below reads
# the event count as the bead count (db-i0jd) — so leading/trailing blanks are trimmed
# positionally, and an empty new_value still counts as its own field.
import sys, collections

bc = collections.Counter()
ec = collections.Counter()
for line in sys.stdin:
    line = line.rstrip('\n').strip()
    if not line or line.startswith('+') or line.startswith('('):
        continue
    parts = [p.strip() for p in line.split('|')]
    while parts and parts[0] == '':
        parts = parts[1:]
    while parts and parts[-1] == '':
        parts = parts[:-1]
    if len(parts) == 4:
        event_type, new_value, n_beads, n_events = parts[0], parts[1], parts[2], parts[3]
    elif len(parts) == 3:
        event_type, new_value, n_beads = parts[0], parts[1], parts[2]
        n_events = n_beads
    else:
        continue
    if event_type == 'event_type' or 'COALESCE' in event_type:
        continue
    try:
        n_beads = int(n_beads)
        n_events = int(n_events)
    except ValueError:
        continue
    if n_beads == 0:
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
    elif event_type == 'reopen':
        cls = 'sp-reopen-' + (cause or 'unrecorded')
    elif event_type == 'reopened':
        cls = 'sp-reopen-' + (cause or 'unrecorded')
    else:
        continue
    bc[cls] += n_beads
    ec[cls] += n_events

for cls, nb in sorted(bc.items(), key=lambda x: (-x[1], -ec.get(x[0], 0))):
    print(nb, ec[cls], cls)
