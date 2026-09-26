#!/usr/bin/env python3
# census/merge.py — merge all-time and since-watermark class counts, ranked by since.
#
#   python3 merge.py <all-time-file> <since-watermark-file>
#
# Each input is count.py's output format: "<beads> <events> <class>" lines. Prints
# "<beads-since> <class> (<events-since> detections, <beads-all-time> all-time)" per
# class, ranked by since-watermark bead count then event count, both descending — a
# class recently active outranks one with a larger historical count but no recent
# activity.
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
