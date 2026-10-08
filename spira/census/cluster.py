#!/usr/bin/env python3
# census/cluster.py — cluster raw event rows into DISTINCT CAUSAL EVENTS per class.
#
#   python3 cluster.py < census_event_rows_run_sql-output
#
# Reads tabular SQL output (the `bd sql` result format: a leading/trailing separator
# line, a header row, and pipe-delimited data rows) from stdin — one row per event:
# (event_type, new_value, issue_id, unix-timestamp), as emitted by lib.sh's
# _census_event_rows_sql — and prints one "<causal> <victims> <detections> <class>" line
# per class, ranked by causal-event count then victim count then detection count, all
# descending (ties broken by class name for determinism).
#
# THE RULE (Concierge decision, sp-h2hpl/sp-yojbh, implemented by sp-jcd0e): same-class
# events separated by less than SPIRA_CENSUS_CLUSTER_GAP_S (default 300 = 5 minutes) are
# one causal event, not one each. A detector sweep that touches five beads inside one gap
# window is one occurrence with five victims, not five occurrences — counting it as five
# lets a single stuck condition outrank genuinely distinct failures. Victim count (the
# old ranking metric) is retained as a secondary field precisely so nothing is hidden by
# the switch, per the ask's own instruction that it must never decide the rank.
import os, sys, collections
from classmap import class_for

GAP_S = int(os.environ.get('SPIRA_CENSUS_CLUSTER_GAP_S', '300') or '300')

rows = collections.defaultdict(list)   # class -> [(epoch, issue_id), ...]
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
    event_type, new_value, issue_id, ts = parts
    if event_type == 'event_type' or 'COALESCE' in event_type:
        continue
    try:
        ts = int(ts)
    except ValueError:
        continue
    cls = class_for(event_type, new_value)
    if cls is None:
        continue
    rows[cls].append((ts, issue_id))

out = []
for cls, events in rows.items():
    events.sort(key=lambda e: e[0])
    causal = 0
    prev_ts = None
    for ts, _issue_id in events:
        if prev_ts is None or ts - prev_ts >= GAP_S:
            causal += 1
        prev_ts = ts
    victims = len(set(issue_id for _ts, issue_id in events))
    detections = len(events)
    out.append((causal, victims, detections, cls))

for causal, victims, detections, cls in sorted(out, key=lambda x: (-x[0], -x[1], -x[2], x[3])):
    print(causal, victims, detections, cls)
