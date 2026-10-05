#!/usr/bin/env bash
# script-running-guard.sh — PreToolUse hook: refuse an in-place write to a shell script
# that a live process is executing. bash reads a script as it runs, so rewriting the same
# inode shifts the text under it. The exit is a sibling temp file and mv over the path.
#
# Override: SCRIPT_RUNNING_EDIT_CONSIDERED=1 (in the hook's environment).
# Exit 2 BLOCKS the tool call and feeds stderr back to the model. Exit 0 allows.
set -uo pipefail

payload="$(cat)"
[ -z "${SCRIPT_RUNNING_EDIT_CONSIDERED:-}" ] || exit 0

GUARD_PAYLOAD="$payload" PROC_ROOT="${SPIRA_PROC_ROOT:-/proc}" python3 -c '
import json, os, re, shlex, sys

try:
    d = json.loads(os.environ.get("GUARD_PAYLOAD", ""))
except Exception:
    sys.exit(0)
proc = os.environ["PROC_ROOT"]
tool = d.get("tool_name", "")
ti = d.get("tool_input") or {}
cwd = d.get("cwd") or os.getcwd()

def resolve(p, base=cwd):
    return os.path.realpath(os.path.join(base, os.path.expanduser(p)))

cands = []
if tool in ("Write", "Edit", "MultiEdit", "NotebookEdit"):
    p = ti.get("file_path") or ti.get("notebook_path") or ""
    if p:
        cands.append(resolve(p))
elif tool == "Bash":
    cmd = ti.get("command", "")
    toks = []
    try:
        toks = shlex.split(cmd, posix=True)
    except ValueError:
        toks = cmd.split()
    shpath = lambda t: t.endswith(".sh")
    for i, t in enumerate(toks):
        m = re.match(r"^\d*>>?\|?(.*)$", t)
        if m and not t.startswith(">&"):
            tgt = m.group(1) or (toks[i + 1] if i + 1 < len(toks) else "")
            if shpath(tgt):
                cands.append(resolve(tgt))
    for m in re.finditer(r"(?:^|[\s;&|(])\d*>>?\|?\s*[\"\x27]?([^\s\"\x27;&|)<>]+\.sh)(?=$|[\s\"\x27;&|)])", cmd):
        cands.append(resolve(m.group(1)))
    verbs = {"tee", "cp", "dd", "truncate", "sed", "perl", "install"}
    for i, t in enumerate(toks):
        if os.path.basename(t) in verbs:
            rest = toks[i + 1:]
            end = len(rest)
            for j, r in enumerate(rest):
                if r in ("&&", "||", ";", "|"):
                    end = j
                    break
            seg = rest[:end]
            if os.path.basename(t) == "sed" and not any(re.match(r"^-[a-zA-Z]*i", a) or a.startswith("--in-place") for a in seg):
                continue
            if os.path.basename(t) == "perl" and not any(re.match(r"^-[a-zA-Z]*i", a) for a in seg):
                continue
            if os.path.basename(t) in ("cp", "install"):
                seg = seg[-1:]
            for a in seg:
                a = a[3:] if a.startswith("of=") else a
                if shpath(a):
                    cands.append(resolve(a))
    if re.search(r"open\(", cmd) and re.search(r"[\"\x27](?:w|a|r\+|wb|ab|w\+|a\+)[\"\x27]", cmd):
        for m in re.finditer(r"[\"\x27]([^\"\x27\s]+\.sh)[\"\x27]", cmd):
            cands.append(resolve(m.group(1)))
        for m in re.finditer(r"(?:^|[\s=(])([^\s\"\x27()=]+\.sh)(?=$|[\s\"\x27;&|)])", cmd):
            cands.append(resolve(m.group(1)))

cands = [c for c in dict.fromkeys(cands) if os.path.isfile(c)]
if not cands:
    sys.exit(0)

SHELLS = {"bash", "sh", "dash", "zsh", "ksh", "ash"}
me = str(os.getpid())
hits = {c: [] for c in cands}
for pid in os.listdir(proc):
    if not pid.isdigit() or pid == me:
        continue
    pd = os.path.join(proc, pid)
    try:
        argv = open(os.path.join(pd, "cmdline"), "rb").read().split(b"\0")
        argv = [a.decode("utf-8", "replace") for a in argv if a]
    except OSError:
        continue
    try:
        pcwd = os.readlink(os.path.join(pd, "cwd"))
    except OSError:
        pcwd = "/"
    found = set()
    if argv and os.path.basename(argv[0]).lstrip("-") in SHELLS:
        for a in argv[1:]:
            if a.startswith("-") or not a:
                continue
            r = resolve(a, pcwd)
            if r in hits:
                found.add(r)
    try:
        for fd in os.listdir(os.path.join(pd, "fd")):
            try:
                r = os.path.realpath(os.readlink(os.path.join(pd, "fd", fd)))
            except OSError:
                continue
            if r in hits:
                found.add(r)
    except OSError:
        pass
    for r in found:
        hits[r].append(pid)

bad = {c: p for c, p in hits.items() if p}
if not bad:
    sys.exit(0)
for c, p in bad.items():
    sys.stderr.write(
        "script-running-guard: BLOCKED %s: %s is being executed by live process(es) %s. "
        "A shell reads its script as it runs, so an in-place write shifts the text under it. "
        "Write a sibling temp file and mv it over the path (atomic replace; the running shell "
        "keeps the old inode), or set SCRIPT_RUNNING_EDIT_CONSIDERED=1.\n" % (tool, c, ",".join(p)))
sys.exit(2)
'
