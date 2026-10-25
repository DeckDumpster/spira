import os, re, subprocess, sys

root = sys.argv[1]
LINK = re.compile(r'(?<!\!)\[[^\]]*\]\(([^)\s]+)\)')


def slug(h):
    h = re.sub(r'[`*_]', '', h.strip().lower())
    h = re.sub(r'[^\w\- ]', '', h)
    return h.replace(' ', '-')


def anchors(path):
    out, fence = set(), False
    for l in open(path, encoding='utf-8'):
        if l.startswith('```'):
            fence = not fence
        elif not fence and re.match(r'#{1,6} ', l):
            out.add(slug(l.lstrip('#')))
    return out


def md_files():
    out = subprocess.run(['git', '-C', root, 'ls-files', 'README.md', 'docs'], capture_output=True, text=True).stdout.split()
    return [f for f in out if f.endswith('.md') and '-evidence/' not in f and '/sources/' not in f]


def check(src, target):
    if re.match(r'[a-z]+:', target):
        return None
    path, _, frag = target.partition('#')
    dest = os.path.normpath(os.path.join(os.path.dirname(os.path.join(root, src)), path)) if path else os.path.join(root, src)
    if not os.path.exists(dest):
        return f'{src}: {target}: no such file'
    if frag and dest.endswith('.md') and frag not in anchors(dest):
        return f'{src}: {target}: no such anchor'
    return None


bad = []
for f in md_files():
    fence = False
    for l in open(os.path.join(root, f), encoding='utf-8'):
        if l.startswith('```'):
            fence = not fence
        if fence:
            continue
        for t in LINK.findall(l):
            e = check(f, t)
            if e:
                bad.append(e)

refs = subprocess.run(['git', '-C', root, 'grep', '-hoI', r'README\.md#[A-Za-z0-9_-]*'], capture_output=True, text=True).stdout.split()
for r in sorted(set(refs)):
    frag = r.split('#', 1)[1]
    if frag not in anchors(os.path.join(root, 'README.md')):
        bad.append(f'{r}: dangling README anchor')

n = len(open(os.path.join(root, 'README.md')).read().splitlines())
if n >= 300:
    bad.append(f'README.md is {n} lines, limit 300')
print('\n'.join(bad))
sys.exit(1 if bad else 0)
