#!/usr/bin/env python3
"""
ready-bucket.py — bucket one ready-set fetch into a per-fayth count (lib.sh's
bulk_ready_by_fayth, for sentinel.sh --summon-only), mirroring fayth_ready's own predicate
without paying one `bd ready` call per persona.

Input: the ready set (bd --json), list or single object, on stdin. PARTS:
"<fayth>|<inc-labels-csv>|<exc-labels-csv>" lines, one per active fayth (spira_fayths, so
every fayth is its own row even when two share a partition — each may carry its own
FAYTH_EXCLUDE_LABELS). Output: "<fayth> <count>" lines, one per PARTS row.

THE PREDICATE MIRRORS fayth_exclude/ready_count exactly: a bead counts for a fayth iff its
labels are a superset of FAYTH_LABELS, disjoint from FAYTH_EXCLUDE_LABELS and from the
shared QUEUE_WAIT/SUBMITTED exclusion, and — when the bead carries fayth:<name> — this
fayth is among the named preferences (fayth_exclude's "every OTHER persona's claim").
"""
import json, os, sys

try:
    d = json.load(sys.stdin)
except Exception:
    d = []
beads = d if isinstance(d, list) else [d]

parts = []
for line in os.environ.get("PARTS", "").splitlines():
    line = line.strip()
    if not line:
        continue
    name, inc_str, exc_str = line.split("|", 2)
    parts.append((name, set(filter(None, inc_str.split(","))),
                        set(filter(None, exc_str.split(",")))))

shared_exclude = set(filter(None, [
    os.environ.get("SPIRA_QUEUE_WAIT_LABEL", ""),
    os.environ.get("SPIRA_SUBMITTED_LABEL", ""),
]))

counts = {name: 0 for name, _, _ in parts}
for bead in beads:
    L = set(bead.get("labels") or [])
    if L & shared_exclude:
        continue
    pref = {x.split(":", 1)[1] for x in L if x.startswith("fayth:")}
    for name, inc, exc in parts:
        if pref and name not in pref:
            continue
        if not inc <= L:
            continue
        if L & exc:
            continue
        counts[name] += 1

for name, _, _ in parts:
    print("%s %d" % (name, counts[name]))
