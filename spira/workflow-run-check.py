#!/usr/bin/env python3
# workflow-run-check.py — law-a-workflow-lands-on-its-own-run: does BRANCH's close reason
# cite a dispatched GitHub Actions run that actually exercised the workflow files it changed?
#
# Reads SPIRA_REPO, BRANCH, BASE, BEAD_ID, SPIRA_BD, SPIRA_DB, SPIRA_GH_API,
# SPIRA_WORKFLOW_ONLY_PATHS from the environment. Prints one token and sets exit status:
#   (nothing), OK   -> exit 0, nothing to check or the cited run is valid
#   NO_URL | WRONG_BRANCH:<b> | SHA_NOT_ANCESTOR:<sha> | WRONG_WORKFLOW:<path>
#   RUN_NOT_FOUND:<code> | API_FAIL:<msg>                -> exit 1
#
# Called from aeon.sh's close-time teardown fence, and directly by
# test-workflow-run-check.sh — the one script both sides trust, so they cannot disagree
# about which branch/URL pairing counts as evidence.
import json, os, re, subprocess, sys

try:
    import urllib.request, urllib.error
    _have_urllib = True
except ImportError:
    _have_urllib = False

repo   = os.environ.get("SPIRA_REPO", "")
branch = os.environ.get("BRANCH", "")
base   = os.environ.get("BASE", "origin/main")
bid    = os.environ.get("BEAD_ID", "")
bd     = os.environ.get("SPIRA_BD", "bd")
db     = os.environ.get("SPIRA_DB", "")
gh     = os.environ.get("SPIRA_GH_API", "https://api.github.com")
wonly  = os.environ.get("SPIRA_WORKFLOW_ONLY_PATHS", "").split()
if not repo or not branch or not db or not bid:
    sys.exit(0)

try:
    r = subprocess.run(
        ["git", "-C", repo, "diff", "--name-only", f"{base}...refs/heads/{branch}"],
        capture_output=True, text=True, timeout=10)
    changed = set(r.stdout.strip().splitlines()) if r.returncode == 0 else set()
except Exception:
    changed = set()

if not changed:
    sys.exit(0)

wf_changed = [f for f in changed if f.startswith(".github/workflows/")]
only_changed = [f for f in changed if f in wonly]
for wf in wf_changed:
    try:
        r = subprocess.run(["git", "-C", repo, "show", f"refs/heads/{branch}:{wf}"],
                           capture_output=True, text=True, timeout=5)
        if r.returncode == 0:
            for m in re.finditer(r"(?:bash\s+|python3?\s+)(spira/[\w/_-]+\.sh)", r.stdout):
                path = m.group(1)
                if path not in only_changed:
                    only_changed.append(path)
    except Exception:
        pass

if not wf_changed and not only_changed:
    sys.exit(0)

try:
    r = subprocess.run([bd, "-C", db, "show", bid, "--format", "json"],
                       capture_output=True, text=True, timeout=10)
    if r.returncode != 0 or not r.stdout.strip():
        sys.exit(0)
    bead = json.loads(r.stdout)
    bead = bead[0] if isinstance(bead, list) else bead
    reason = bead.get("close_reason") or ""
    notes  = bead.get("notes") or ""
except Exception:
    sys.exit(0)

# Agent-level override: bd note $BID "WORKFLOW_RUN_CONSIDERED: <reason>"
if re.search(r"WORKFLOW_RUN_CONSIDERED\s*:", notes):
    sys.exit(0)

url_pat = re.compile(r"https://github\.com/([^/\s]+)/([^/\s]+)/actions/runs/(\d+)")
url_m = url_pat.search(reason)
if not url_m:
    print("NO_URL")
    sys.exit(1)

if not _have_urllib:
    sys.exit(0)

owner, repo_name, run_id = url_m.groups()
try:
    req = urllib.request.Request(
        f"{gh}/repos/{owner}/{repo_name}/actions/runs/{run_id}",
        headers={"Accept": "application/vnd.github+json",
                 "User-Agent": "spira/1.0",
                 "X-GitHub-Api-Version": "2022-11-28"})
    with urllib.request.urlopen(req, timeout=5) as resp:
        run = json.loads(resp.read())
except urllib.error.HTTPError as e:
    print(f"RUN_NOT_FOUND:{e.code}")
    sys.exit(1)
except Exception as e:
    msg = str(e).replace("\n", " ")[:60]
    print(f"API_FAIL:{msg}")
    sys.exit(1)

run_branch = re.sub(r"^refs/heads/", "", run.get("head_branch", "") or "")
if run_branch != branch:
    print(f"WRONG_BRANCH:{run_branch}")
    sys.exit(1)

head_sha = (run.get("head_sha") or "").strip()
if head_sha:
    try:
        r = subprocess.run(
            ["git", "-C", repo, "merge-base", "--is-ancestor",
             head_sha, f"refs/heads/{branch}"],
            capture_output=True, timeout=5)
        if r.returncode != 0:
            print(f"SHA_NOT_ANCESTOR:{head_sha[:12]}")
            sys.exit(1)
    except Exception:
        pass

run_path = (run.get("path") or "").lstrip("./")
if wf_changed and run_path:
    if run_path not in [f.lstrip("./") for f in wf_changed]:
        print(f"WRONG_WORKFLOW:{run_path}")
        sys.exit(1)

print("OK")
sys.exit(0)
