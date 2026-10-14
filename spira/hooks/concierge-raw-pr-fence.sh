#!/usr/bin/env bash
# concierge-raw-pr-fence.sh — PreToolUse hook for the Concierge session: refuse `gh pr create`
# and `gh pr merge` against a repository registered with this installation. Rounds are opened
# and merged by the batcher's queue tools; a raw PR beside a batch PR is a second writer.
#
# Binds the Concierge's own session only (SPIRA_CONCIERGE=1). The target is -R/--repo, else
# GH_REPO, else the working directory. A registry that cannot be read refuses.
# Override: CONCIERGE_RAW_PR_CONSIDERED="<why>", set in the hook's env or as a prefix on the
# command itself. Unregistered repositories are unaffected. Exit 2 blocks.
set -uo pipefail

PAYLOAD="$(cat 2>/dev/null || true)"
[ "${SPIRA_CONCIERGE:-}" = 1 ] || exit 0
[ -z "${CONCIERGE_RAW_PR_CONSIDERED:-}" ] || exit 0

GUARD_PAYLOAD="$PAYLOAD" GUARD_PWD="$PWD" GUARD_CONFIG="${SPIRA_CONFIG_BIN:-spira-config}" python3 -I -c '
import json, os, re, shlex, subprocess, sys

try:
    d = json.loads(os.environ["GUARD_PAYLOAD"])
except Exception:
    sys.exit(0)
if d.get("tool_name") != "Bash":
    sys.exit(0)
cmd = (d.get("tool_input") or {}).get("command", "")
cwd = d.get("cwd") or os.environ["GUARD_PWD"]
cfg = os.environ["GUARD_CONFIG"]

def run(*a):
    r = subprocess.run([cfg, "repo", *a], capture_output=True, text=True, timeout=20)
    if r.returncode != 0:
        raise RuntimeError(r.stderr.strip() or "exit %d" % r.returncode)
    return r.stdout.strip()

def steps():
    for seg in re.split(r"\|\||&&|[;|\n]", cmd):
        try:
            toks = shlex.split(seg)
        except ValueError:
            toks = seg.split()
        env = {}
        while toks and re.match(r"^\w+=", toks[0]):
            k, _, v = toks.pop(0).partition("=")
            env[k] = v
        if toks and toks[0] == "cd" and len(toks) > 1:
            yield ("cd", toks[1], "", env)
            continue
        if not toks or os.path.basename(toks[0]) != "gh":
            continue
        pos, target, i, rest = [], env.get("GH_REPO", ""), 0, toks[1:]
        while i < len(rest):
            t = rest[i]
            if t in ("-R", "--repo") and i + 1 < len(rest):
                target = rest[i + 1]
                i += 2
                continue
            if t.startswith("--repo="):
                target = t.split("=", 1)[1]
            elif t.startswith("-R") and len(t) > 2:
                target = t[2:]
            elif not t.startswith("-"):
                pos.append(t)
            i += 1
        if pos[:1] == ["pr"] and pos[1:2] in (["create"], ["merge"]):
            yield ("gh", "pr " + pos[1], target, env)

err, roots = "", {}
try:
    for n in run("names").splitlines():
        if not n:
            continue
        try:
            roots[n] = os.path.realpath(run("field", n, "path"))
        except RuntimeError:
            pass
except Exception as e:
    err = str(e)

def remote_slug(path):
    r = subprocess.run(["git", "-C", path, "remote", "get-url", "origin"], capture_output=True, text=True)
    m = re.search(r"[:/]([^/:]+/[^/]+?)(?:\.git)?$", r.stdout.strip())
    return m.group(1).lower() if m else ""

def registered(target, here):
    if target:
        slug = re.sub(r"\.git$", "", target.split("github.com/")[-1].strip("/")).lower()
        repo = slug.rsplit("/", 1)[-1]
        for n, p in roots.items():
            if n.lower() == repo or remote_slug(p) == slug:
                return n
        return ""
    h = os.path.realpath(here)
    for n, p in roots.items():
        if h == p or h.startswith(p + os.sep):
            return n
    return ""

here = cwd
for kind, a, target, env in steps():
    if kind == "cd":
        here = a if os.path.isabs(a) else os.path.join(here, a)
        continue
    if env.get("CONCIERGE_RAW_PR_CONSIDERED"):
        continue
    if err:
        sys.stderr.write("BLOCKED by concierge-raw-pr-fence: gh %s, and the repository registry could not be read (%s). "
                         "Override: CONCIERGE_RAW_PR_CONSIDERED=\"<why>\".\n" % (a, err))
        sys.exit(2)
    name = registered(target, here)
    if name:
        sys.stderr.write(
            "BLOCKED by concierge-raw-pr-fence: gh %s against %s, a repository registered with this "
            "installation. Open and merge rounds with `queue open-batch` and the batcher\x27s own tools; "
            "a raw PR sits beside the batch PR as a second writer.\n"
            "If a raw PR is warranted, prefix the command with CONCIERGE_RAW_PR_CONSIDERED=\"<why>\".\n" % (a, name))
        sys.exit(2)
sys.exit(0)
'
