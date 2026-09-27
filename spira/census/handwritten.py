#!/usr/bin/env python3
# census/handwritten.py — list events excluded from the ranked census by the actor
# predicate in lib.sh's _census_events_sql, so a hand-written ledger correction stays
# visible (law-absence-needs-a-positive-control) instead of vanishing along with the
# failure classes it was excluded to stop polluting.
#
#   python3 handwritten.py < census_handwritten_run_sql-output
#
# Reads tabular SQL output (event_type|new_value|actor|beads|events) and prints one
# "hand-written (not ranked): <class> <beads> beads (actor <actor>)" line per
# (class, actor) pair, ranked by beads descending.
import sys
from classmap import class_for

rows = []
for line in sys.stdin:
    line = line.rstrip('\n').strip()
    if not line or line.startswith('+') or line.startswith('('):
        continue
    parts = [p.strip() for p in line.split('|')]
    while parts and parts[0] == '':
        parts = parts[1:]
    while parts and parts[-1] == '':
        parts = parts[:-1]
    if len(parts) != 5:
        continue
    event_type, new_value, actor, n_beads, _n_events = parts
    if event_type == 'event_type' or 'COALESCE' in event_type:
        continue
    try:
        n_beads = int(n_beads)
    except ValueError:
        continue
    if n_beads == 0:
        continue
    cls = class_for(event_type, new_value)
    if cls is None:
        continue
    rows.append((n_beads, cls, actor))

for n_beads, cls, actor in sorted(rows, key=lambda r: (-r[0], r[1], r[2])):
    print('hand-written (not ranked): {} {} beads (actor {})'.format(cls, n_beads, actor))
