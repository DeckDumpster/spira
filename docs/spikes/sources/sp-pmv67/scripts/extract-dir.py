import os,re,sys
OK=re.compile(r'^\s{1,4}ok\s+(?:— )?(.*)$'); FAIL=re.compile(r'^\s{1,4}FAIL\s+(?:— )?(.*?)(?::\s.*)?$')
def norm(s):
    s=re.sub(r'/tmp/[A-Za-z0-9._-]*','/tmp/X',s); s=re.sub(r'/[A-Za-z0-9._/-]*/sptest_[A-Za-z0-9_]*','/X',s)
    s=re.sub(r'\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d[.0-9]*Z?','TIMESTAMP',s); s=re.sub(r'\d\d:\d\d:\d\d','TIME',s); s=re.sub(r'\d{3,}','N',s); return s.strip()
raw = len(sys.argv)>2 and sys.argv[2]=='raw'
d=sys.argv[1]
for f in sorted(os.listdir(d)):
    if not f.endswith('.out'): continue
    for line in open(os.path.join(d,f),errors='replace'):
        line=line.rstrip('\n'); m=OK.match(line); v='ok'
        if not m: m=FAIL.match(line); v='fail'
        if m and m.group(1).strip(): print(f'{f[:-4]}\t{v}\t{m.group(1).strip() if raw else norm(m.group(1))}')
