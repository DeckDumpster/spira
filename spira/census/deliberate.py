#!/usr/bin/env python3
# census/deliberate.py — list deliberate-cause reopen events excluded from the ranked
# census by the deliberate_fold predicate in lib.sh's _census_events_sql, so a cause that
# is the system working (law-a-deliberate-state-is-not-a-fault) stays visible instead of
# vanishing along with the ranking it was excluded to stop polluting (sp-eiatd).
#
#   python3 deliberate.py < census_deliberate_run_sql-output
#
# Reads tabular SQL output (event_type|new_value|beads|events) and prints one
# "deliberate, not ranked: <class> <beads> beads (<events> events)" line per class,
# ranked by beads descending.
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
    if len(parts) != 4:
        continue
    event_type, new_value, n_beads, n_events = parts
    if event_type == 'event_type' or 'COALESCE' in event_type:
        continue
    try:
        n_beads = int(n_beads)
        n_events = int(n_events)
    except ValueError:
        continue
    if n_beads == 0:
        continue
    cls = class_for(event_type, new_value)
    if cls is None:
        continue
    rows.append((n_beads, n_events, cls))

for n_beads, n_events, cls in sorted(rows, key=lambda r: (-r[0], r[2])):
    print('deliberate, not ranked: {} {} beads ({} events)'.format(cls, n_beads, n_events))
