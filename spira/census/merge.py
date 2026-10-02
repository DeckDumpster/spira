#!/usr/bin/env python3
# census/merge.py — merge all-time and since-watermark class counts, ranked by since.
#
#   python3 merge.py <all-time-file> <since-watermark-file>
#
# Each input is count.py's output format: "<occurrences> <events> <class> [<beads>]"
# lines. Prints "<occ-since> <class> (<events-since> detections[, <beads-since> beads],
# <occ-all-time> all-time)" per class, ranked by since-watermark occurrence count then
# event count, both descending — a
# class recently active outranks one with a larger historical count but no recent
# activity.
import sys

def read_counts(path):
    beads = {}
    events = {}
    distinct = {}
    try:
        with open(path) as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                parts = line.split(None, 3)
                if len(parts) >= 3:
                    try:
                        beads[parts[2]] = int(parts[0])
                        events[parts[2]] = int(parts[1])
                        if len(parts) == 4:
                            distinct[parts[2]] = int(parts[3])
                    except ValueError:
                        pass
    except Exception:
        pass
    return beads, events, distinct

all_beads, all_events, _ = read_counts(sys.argv[1])
wm_beads,  wm_events, wm_distinct = read_counts(sys.argv[2])
all_classes = set(all_beads) | set(wm_beads)

ranked = sorted(all_classes, key=lambda c: (-wm_beads.get(c, 0), -wm_events.get(c, 0), c))
for cls in ranked:
    b_since = wm_beads.get(cls, 0)
    e_since = wm_events.get(cls, 0)
    b_all   = all_beads.get(cls, 0)
    d_since = wm_distinct.get(cls)
    beads_part = '' if d_since is None else ', {} beads'.format(d_since)
    print('{} {} ({} detections{}, {} all-time)'.format(b_since, cls, e_since, beads_part, b_all))
