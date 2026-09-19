#!/usr/bin/env python3
"""harvest-corpus.py <batch-results-root> -> TSV on stdout, one row per assertion line.

Columns: epoch  batch  mode  branch  suite  suite_status  verdict  label
verdict is ok|fail from the assertion line; suite_status is the .result status word.
Labels go through the same normalisation suites.sh fingerprint() applies.
A suite whose .out holds no assertion line gets one row with verdict=no-assertions.
"""
import os, re, sys
root = sys.argv[1]
OK = re.compile(r'^\s{1,4}ok\s+(?:— )?(.*)$')
FAIL = re.compile(r'^\s{1,4}FAIL\s+(?:— )?(.*?)(?::\s.*)?$')
def norm(s):
    s = re.sub(r'/tmp/[A-Za-z0-9._-]*', '/tmp/X', s)
    s = re.sub(r'/[A-Za-z0-9._/-]*/sptest_[A-Za-z0-9_]*', '/X', s)
    s = re.sub(r'\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d[.0-9]*Z?', 'TIMESTAMP', s)
    s = re.sub(r'\d\d:\d\d:\d\d', 'TIME', s)
    s = re.sub(r'\d{3,}', 'N', s)
    return s.strip()
for b in sorted(os.listdir(root)):
    d = os.path.join(root, b)
    if not os.path.isdir(d): continue
    meta = {}
    try:
        for l in open(os.path.join(d, 'batch.meta')):
            if '=' in l: k, v = l.rstrip('\n').split('=', 1); meta[k] = v
    except OSError: pass
    for f in os.listdir(d):
        if not f.endswith('.result'): continue
        suite = f[:-len('.result')]
        try: r = open(os.path.join(d, f)).read().split()
        except OSError: continue
        status = r[0] if r else '?'; epoch = r[1] if len(r) > 1 else '0'
        mode = r[4] if len(r) > 4 else meta.get('mode', '?')
        n = 0
        try: out = open(os.path.join(d, suite + '.out'), errors='replace').read().splitlines()
        except OSError: out = []
        for line in out:
            m = OK.match(line); v = 'ok'
            if not m:
                m = FAIL.match(line); v = 'fail'
            if not m: continue
            lab = norm(m.group(1))
            if not lab: continue
            n += 1
            print('\t'.join([epoch, b[:12], mode, meta.get('branch', '?'), suite, status, v, lab]))
        if n == 0:
            print('\t'.join([epoch, b[:12], mode, meta.get('branch', '?'), suite, status, 'no-assertions', '-']))
