#!/usr/bin/env python3
# census/covers_closed.py — closed remedy beads within the remedy window.
#
#   SPIRA_REMEDY_WINDOW=30 python3 covers_closed.py <fold-map-file> < bd-list---json-output
#
# Reads a JSON bead list (or single bead object) from stdin and prints "<bead-id>
# <class>" for each closed bead's "covers:<class>" label, skipping beads closed before
# SPIRA_REMEDY_WINDOW days ago (default 30). The fold-map file resolves covers: aliases
# to canonical class names, same as covers.py.
import os, sys, json
from datetime import datetime, timezone, timedelta

window = int(os.environ.get('SPIRA_REMEDY_WINDOW', '30'))
cutoff = datetime.now(timezone.utc) - timedelta(days=window)

fold = {}
try:
    with open(sys.argv[1]) as f:
        for line in f:
            parts = line.split()
            if len(parts) == 2:
                fold[parts[0]] = parts[1]
except Exception:
    pass

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
            cls = lbl[len('covers:'):]
            print(bid, fold.get(cls, cls))
