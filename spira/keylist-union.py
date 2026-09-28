#!/usr/bin/env python3
"""Resolve conflict regions in a file ONLY when both sides are pure config-key lists
(whitespace-separated SPIRA_*/COCKPIT_* tokens). Union: ours in order, then theirs' new
tokens appended to ours' last line. Exit 1, leaving the file untouched, if any region
is anything else."""
import re, sys
p = sys.argv[1]
s = open(p).read()
KEY = re.compile(r'^[ \t]*((SPIRA|COCKPIT)_[A-Z0-9_]+[ \t]*)+$')
out, i, lines = [], 0, s.split('\n')
while i < len(lines):
    l = lines[i]
    if not l.startswith('<<<<<<< '):
        out.append(l); i += 1; continue
    j = i + 1; ours = []
    while not lines[j].startswith('======='): ours.append(lines[j]); j += 1
    k = j + 1; theirs = []
    while not lines[k].startswith('>>>>>>> '): theirs.append(lines[k]); k += 1
    for x in ours + theirs:
        if x.strip() and not KEY.match(x):
            sys.exit("not a pure key list: %r" % x[:80])
    have = set(t for x in ours for t in x.split())
    new = [t for x in theirs for t in x.split() if t not in have]
    seen = set(); new = [t for t in new if not (t in seen or seen.add(t))]
    ours = list(ours)
    if new:
        if ours: ours[-1] = ours[-1].rstrip() + ' ' + ' '.join(new)
        else: ours = [' '.join(new)]
    out.extend(ours); i = k + 1
open(p, 'w').write('\n'.join(out))
print("resolved: union adds", len(new) if 'new' in dir() else 0)
