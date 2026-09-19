#!/usr/bin/env python3
"""label-stability.py <repo> <new-rev> <old-rev>...

Static extraction of literal assertion labels (first double-quoted argument of an
ok/bad/is/want/nowant/isnt call with no shell expansion in it) from spira/test-*.sh at
each revision, and the fraction of an old revision's keys absent at the new one.
An absent key is classed as a likely RENAME when the same suite at the new revision has a
label not present at the old one with difflib ratio >= 0.6, otherwise as a DELETION.
"""
import subprocess, re, sys, difflib, collections
repo, new, olds = sys.argv[1], sys.argv[2], sys.argv[3:]
CALL = re.compile(r'(?:^|[;&|]|\bthen |\belse |\bdo |\|\| |&& )\s*(ok|bad|is|want|nowant|isnt|has|lacks)\s+"([^"$`\\]*)"')
def git(*a): return subprocess.run(['git','-C',repo,*a],capture_output=True,text=True).stdout
def labels(rev):
    files=[f for f in git('ls-tree','--name-only',rev,'spira/').split() if re.match(r'spira/test-.*\.sh$',f)]
    out={}
    for f in files:
        txt=git('show',f'{rev}:{f}')
        s=set()
        for line in txt.splitlines():
            for m in CALL.finditer(line):
                if m.group(2).strip(): s.add(m.group(2).strip())
        out[f.split('/')[-1]]=s
    return out
N=labels(new)
print(f'new {new[:9]}: {len(N)} suites, {sum(len(v) for v in N.values())} literal labels')
for old in olds:
    O=labels(old)
    common=[s for s in O if s in N]
    ok=sum(len(O[s]) for s in common)
    gone=collections.Counter(); ren=0; dele=0; changed_suites=0
    for s in common:
        miss=O[s]-N[s]; added=N[s]-O[s]
        if miss: changed_suites+=1
        for l in miss:
            best=max((difflib.SequenceMatcher(None,l,a).ratio() for a in added),default=0)
            if best>=0.6: ren+=1
            else: dele+=1
    date=git('log','-1','--format=%ad','--date=short',old).strip()
    tot_missing=ren+dele
    print(f'old {old[:9]} ({date}): suites {len(O)}, in both {len(common)}, deleted suites {len(O)-len(common)}; '
          f'labels in common suites {ok}; absent at new {tot_missing} ({100*tot_missing/max(ok,1):.1f}%): '
          f'likely-rename {ren} ({100*ren/max(ok,1):.1f}%), deletion {dele} ({100*dele/max(ok,1):.1f}%); '
          f'suites with any absent label {changed_suites}')
