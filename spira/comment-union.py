#!/usr/bin/env python3
"""Resolve conflict regions in a file ONLY when every line on both sides is a comment (or
blank) — divergent prose, never code. Union: ours in order, then any theirs line not already
present. Exit 1, leaving the file untouched, if any region contains a non-comment line."""
import os, sys

MARKERS = ('#', '//', '/*', '*', '--')


def is_comment(line):
    t = line.strip()
    return t == '' or t.startswith(MARKERS)


p = sys.argv[1]
s = open(p).read()
out, i, lines = [], 0, s.split('\n')
resolved = 0
while i < len(lines):
    l = lines[i]
    if not l.startswith('<<<<<<< '):
        out.append(l); i += 1; continue
    j = i + 1; ours = []
    while not lines[j].startswith('======='): ours.append(lines[j]); j += 1
    k = j + 1; theirs = []
    while not lines[k].startswith('>>>>>>> '): theirs.append(lines[k]); k += 1
    for x in ours + theirs:
        if not is_comment(x):
            sys.exit("not a comment-only hunk: %r" % x[:80])
    merged = list(ours)
    have = set(x.strip() for x in ours)
    merged.extend(x for x in theirs if x.strip() not in have)
    out.extend(merged); i = k + 1
    resolved += 1
if resolved == 0:
    sys.exit("no conflict markers found")
open(p, 'w').write('\n'.join(out))
print("resolved: comment-union merged", resolved, "hunk(s)")
