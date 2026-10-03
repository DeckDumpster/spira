#!/usr/bin/env python3
# census/count.py — aggregate failure events into ranked class counts.
#
#   python3 count.py < census_events_run_sql-output
#
# Reads tabular SQL output (the `bd sql` result format: a leading/trailing separator
# line, a header row, and pipe-delimited data rows) from stdin and prints one
# "<occurrences> <detections> <class> [<distinct-beads>]" line per class, ranked by
# occurrence count then detection count, both descending. An occurrence is a burst of
# events (the SQL's 5th column); a row without one counts each distinct bead as its own.
# The distinct-bead count trails only when it differs from the occurrence count.
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
from classmap import class_for

oc = collections.Counter()
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
    n_occ = None
    if len(parts) == 5:
        event_type, new_value, n_beads, n_events, n_occ = parts
    elif len(parts) == 4:
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
        n_occ = n_beads if n_occ is None else int(n_occ)
    except ValueError:
        continue
    if n_beads == 0:
        continue
    cls = class_for(event_type, new_value)
    if cls is None:
        continue
    oc[cls] += n_occ
    bc[cls] += n_beads
    ec[cls] += n_events

for cls, no in sorted(oc.items(), key=lambda x: (-x[1], -ec.get(x[0], 0))):
    print(no, ec[cls], cls, *([bc[cls]] if bc[cls] != no else []))
