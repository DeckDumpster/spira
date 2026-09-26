#!/usr/bin/env python3
# census/covers.py — extract covered classes from open remedy beads.
#
#   python3 covers.py <fold-map-file> < bd-list---json-output
#
# Reads a JSON bead list (or single bead object) from stdin and prints one class per
# line for each "covers:<class>" label found. The fold-map file (lines of
# "<alias> <canonical>") resolves a covers: label naming a folded-away class — see
# lib.sh's _census_class_fold_map — to the class name census.sh actually emits, so a
# remedy written against the pre-fold name still suppresses.
import sys, json

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
    for lbl in (b.get("labels") or []):
        if lbl.startswith("covers:"):
            cls = lbl[len("covers:"):]
            print(fold.get(cls, cls))
