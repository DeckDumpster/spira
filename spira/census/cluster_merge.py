#!/usr/bin/env python3
# census/cluster_merge.py — merge all-time and since-watermark cluster.py output, ranked
# by since-watermark causal-event count.
#
#   python3 cluster_merge.py <all-time-file> <since-watermark-file>
#
# Each input is cluster.py's output format: "<causal> <victims> <detections> <class>"
# lines. Prints "<causal-since> <class> (<victims-since> victims, <events-since>
# detections, <causal-all-time> all-time)" per class, ranked by since-watermark causal
# count then victim count then detection count, all descending — a class recently active
# outranks one with a larger historical count but no recent activity. Mirrors merge.py's
# shape exactly, extended with the victim field the ranking rule now carries alongside
# (never as the rank itself — sp-jcd0e).
import sys

def read_counts(path):
    causal, victims, detections = {}, {}, {}
    try:
        with open(path) as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                parts = line.split(None, 3)
                if len(parts) == 4:
                    try:
                        causal[parts[3]] = int(parts[0])
                        victims[parts[3]] = int(parts[1])
                        detections[parts[3]] = int(parts[2])
                    except ValueError:
                        pass
    except Exception:
        pass
    return causal, victims, detections

all_causal, all_victims, all_detections = read_counts(sys.argv[1])
wm_causal, wm_victims, wm_detections = read_counts(sys.argv[2])
all_classes = set(all_causal) | set(wm_causal)

ranked = sorted(
    all_classes,
    key=lambda c: (-wm_causal.get(c, 0), -wm_victims.get(c, 0), -wm_detections.get(c, 0), c),
)
for cls in ranked:
    c_since = wm_causal.get(cls, 0)
    v_since = wm_victims.get(cls, 0)
    d_since = wm_detections.get(cls, 0)
    c_all = all_causal.get(cls, 0)
    print('{} {} ({} victims, {} detections, {} all-time)'.format(c_since, cls, v_since, d_since, c_all))
