#!/usr/bin/env python3
"""assertion-edits.py <repo> <rev-range>

For every non-merge commit in the range that changes an assertion line (a call of
ok/bad/is/want/nowant/isnt with a quoted label) in a spira/test-*.sh, classify the
(commit, suite) pair by whether the same commit also changed a file matched by that
suite's # covers: globs (read at the commit), excluding the suite itself.
"""
import subprocess, sys, re, fnmatch, collections
repo, rng = sys.argv[1], sys.argv[2]
def git(*a): return subprocess.run(['git','-C',repo,*a],capture_output=True,text=True,errors='replace').stdout
ASSERT = re.compile(r'^[-+]\s*.*\b(ok|bad|is|want|nowant|isnt)\s+"')
commits = git('rev-list','--no-merges',rng,'--','spira/').split()
cls = collections.Counter(); per_commit = []
for c in commits:
    files = [f for f in git('diff-tree','--no-commit-id','--name-only','-r',c).split()]
    suites = [f for f in files if re.match(r'spira/test-.*\.sh$', f)]
    if not suites: continue
    for s in suites:
        diff = git('show','--format=','-U0',c,'--',s)
        adds = [l for l in diff.splitlines() if ASSERT.match(l) and not l.startswith(('+++','---'))]
        if not adds: continue
        new_file = 'new file mode' in git('show','--format=','--summary',c,'--',s) or any(l.startswith('@@ -0,0') for l in diff.splitlines()[:6])
        if new_file: cls['suite created']+=1; continue
        cov = ''
        for l in git('show', f'{c}:{s}').splitlines():
            if l.startswith('# covers:'): cov = l[len('# covers:'):].split(); break
        other = [f for f in files if f != s]
        touched = any(fnmatch.fnmatch(f, g) for f in other for g in cov) if cov else None
        rem = sum(1 for l in adds if l.startswith('-')); add = sum(1 for l in adds if l.startswith('+'))
        k = ('covered code also changed' if touched else ('no covers line' if touched is None else 'ONLY the suite (no covered file)'))
        cls[k]+=1
        if k.startswith('ONLY'): per_commit.append((c[:8], s, rem, add))
tot = sum(v for k,v in cls.items() if k!='suite created')
print(f'commits scanned {len(commits)}; (commit,suite) pairs changing an existing suite\'s assertion lines: {tot}')
for k,v in cls.most_common(): print(f'  {v:5d}  {k}' + (f'  ({100*v/tot:.0f}% of edits to existing suites)' if k!='suite created' else ''))
net_removed = sum(1 for _,_,r,a in per_commit if r>a)
print(f'  of the suite-only edits, {net_removed} removed more assertion lines than they added')
