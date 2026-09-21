#!/usr/bin/env bash
# bd-close-workflow-run-guard.sh — PreToolUse hook: refuse bd close on a bead
# whose branch touches a CI workflow or workflow-only script unless the close
# reason cites a dispatched run URL for that workflow on that branch where the
# changed step executed (law-a-workflow-lands-on-its-own-run).
#
# The forge is not available to aeons. The cited URL is read via the public
# GitHub API (no credential) or SPIRA_GH_API if overridden. A 403 or 404 fails
# closed; a network failure fails closed. The check requires: head_branch equals
# the bead's branch, head_sha is an ancestor-or-equal of HEAD, and the
# workflow_path matches one of the changed workflow files.
#
# Override (operator-level bypass, logged):
#   SPIRA_WORKFLOW_RUN_CONSIDERED=<reason>
# Inline bypass (agent; guard only, teardown still checks):
#   SPIRA_WORKFLOW_RUN_CONSIDERED=<reason> bd close ...
#
# Exit 2 BLOCKS the tool call and feeds stderr back to the model. Exit 0 allows.
set -uo pipefail

PAYLOAD="$(cat)"

[ -n "${SPIRA_AEON:-}" ] || exit 0

BID="${BEAD_ID:-}"; DB="${SPIRA_DB:-}"; BD="${SPIRA_BD:-bd}"
WORK="${SPIRA_WORK:-}"
[ -n "$BID" ] && [ -n "$DB" ] && [ -n "$WORK" ] || exit 0

HIT="$(GUARD_PAYLOAD="$PAYLOAD" BEAD_ID="$BID" SPIRA_BD="$BD" SPIRA_DB="$DB" \
       SPIRA_WORK="$WORK" \
       SPIRA_GH_API="${SPIRA_GH_API:-https://api.github.com}" \
       SPIRA_WORKFLOW_ONLY_PATHS="${SPIRA_WORKFLOW_ONLY_PATHS:-spira/acceptance-ci.sh spira/acceptance-run.sh spira/acceptance-agent.sh spira/build-tarball.sh}" \
       python3 -c '
import json, os, re, subprocess, sys

try:
    import urllib.request, urllib.error
    _have_urllib = True
except ImportError:
    _have_urllib = False

payload = os.environ.get("GUARD_PAYLOAD", "")
try:
    d = json.loads(payload)
except Exception:
    sys.exit(0)

if d.get("tool_name") != "Bash":
    sys.exit(0)

cmd = (d.get("tool_input") or {}).get("command", "")
if not cmd:
    sys.exit(0)

cmd_nosq = re.sub(r"\x27[^\x27]*\x27", " ", cmd)
segs = re.split(r"\|\||\&\&|[;|\n]", cmd_nosq)
found_close = False
for seg in segs:
    if re.search(r"(?:^|\s)(?:\w+=\S+\s+)*(?:[-\w./]*/)?bd\b[^\n]*\bclose\b", seg):
        found_close = True
        break
if not found_close:
    sys.exit(0)

bid   = os.environ.get("BEAD_ID", "")
bd    = os.environ.get("SPIRA_BD", "bd")
db    = os.environ.get("SPIRA_DB", "")
work  = os.environ.get("SPIRA_WORK", "")
gh    = os.environ.get("SPIRA_GH_API", "https://api.github.com")
wonly = os.environ.get("SPIRA_WORKFLOW_ONLY_PATHS", "").split()
if not bid or not db or not work:
    sys.exit(0)

try:
    r = subprocess.run([bd, "-C", db, "state", bid, "branch"],
                       capture_output=True, text=True, timeout=5)
    branch = r.stdout.strip() if r.returncode == 0 else ""
    if not branch or branch.startswith("("):
        branch = f"spira/{bid}"
except Exception:
    branch = f"spira/{bid}"

try:
    r = subprocess.run(
        ["git", "-C", work, "diff", "--name-only", "origin/main...HEAD"],
        capture_output=True, text=True, timeout=5)
    changed = set(r.stdout.strip().splitlines()) if r.returncode == 0 else set()
except Exception:
    changed = set()

if not changed:
    sys.exit(0)

wf_changed = [f for f in changed if f.startswith(".github/workflows/")]
only_changed = [f for f in changed if f in wonly]

# Detect scripts referenced from changed workflow files via "run:" blocks.
for wf in wf_changed:
    try:
        r = subprocess.run(["git", "-C", work, "show", f"HEAD:{wf}"],
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

# Branch touches workflow files. Extract the close reason from the command.
reason = ""
# Heredoc: <<'"'"'WORD'"'"'\n...\nWORD (or <<"WORD">)
hd = re.search(r"<<['\"]?(\w+)['\"]?\n(.*?)\n\1\s*(?:$|#)", cmd, re.DOTALL | re.MULTILINE)
if hd:
    reason = hd.group(2)
else:
    rm = re.search(r"--reason\s+\"([^\"]+)\"", cmd)
    if rm:
        reason = rm.group(1)
    else:
        rm = re.search(r"--reason\s+(\S+)", cmd)
        if rm and rm.group(1) not in ("--", "-"):
            reason = rm.group(1)

# If reason is via a file path we cannot read, allow and let teardown judge.
if not reason:
    if re.search(r"--reason-file\s+(?!-(?:\s|$))", cmd):
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
                 "User-Agent": "spira-guard/1.0",
                 "X-GitHub-Api-Version": "2022-11-28"})
    with urllib.request.urlopen(req, timeout=3) as resp:
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
            ["git", "-C", work, "merge-base", "--is-ancestor", head_sha, "HEAD"],
            capture_output=True, timeout=3)
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
' 2>/dev/null)"

[ -n "$HIT" ] || exit 0

if [ -n "${SPIRA_WORKFLOW_RUN_CONSIDERED:-}" ]; then
    printf '%s spira: bd-close-workflow-run-guard: %s: override active (%s)\n' \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$BID" "${SPIRA_WORKFLOW_RUN_CONSIDERED}" >&2
    exit 0
fi
if printf '%s' "$PAYLOAD" | python3 -c '
import json,sys
d=json.load(sys.stdin)
c=(d.get("tool_input") or {}).get("command","")
sys.exit(0 if "SPIRA_WORKFLOW_RUN_CONSIDERED=" in c else 1)' 2>/dev/null; then exit 0; fi

case "$HIT" in
    NO_URL)
        printf '\nBLOCKED by bd-close-workflow-run-guard: this branch touches CI workflow files but the close reason contains no GitHub Actions run URL (law-a-workflow-lands-on-its-own-run).\n\n' >&2
        printf 'The close reason must include a URL of the form:\n' >&2
        printf '  https://github.com/<owner>/<repo>/actions/runs/<id>\n\n' >&2
        printf 'The run must be on branch "%s" and must have executed a step from the changed workflow.\n\n' "$BID" >&2
        ;;
    WRONG_BRANCH:*)
        printf '\nBLOCKED by bd-close-workflow-run-guard: the cited run is on branch "%s", not "%s" (law-a-workflow-lands-on-its-own-run).\n\n' \
            "${HIT#WRONG_BRANCH:}" "$BID" >&2
        ;;
    SHA_NOT_ANCESTOR:*)
        printf '\nBLOCKED by bd-close-workflow-run-guard: the cited run is at commit %s which is not an ancestor of the current tip — the run tested a stale tree (law-a-workflow-lands-on-its-own-run).\n\n' \
            "${HIT#SHA_NOT_ANCESTOR:}" >&2
        printf 'Ask the concierge to re-dispatch after pushing the current tip.\n\n' >&2
        ;;
    WRONG_WORKFLOW:*)
        printf '\nBLOCKED by bd-close-workflow-run-guard: the cited run is for workflow "%s" but the changed workflow files are not that file (law-a-workflow-lands-on-its-own-run).\n\n' \
            "${HIT#WRONG_WORKFLOW:}" >&2
        ;;
    RUN_NOT_FOUND:*)
        printf '\nBLOCKED by bd-close-workflow-run-guard: the cited run URL returned HTTP %s — run not found or not accessible.\n\n' \
            "${HIT#RUN_NOT_FOUND:}" >&2
        ;;
    API_FAIL:*)
        printf '\nBLOCKED by bd-close-workflow-run-guard: could not read the run URL (%s). Use SPIRA_WORKFLOW_RUN_CONSIDERED=<reason> if the check cannot be satisfied.\n\n' \
            "${HIT#API_FAIL:}" >&2
        ;;
    OK) exit 0 ;;
    *) printf '\nBLOCKED by bd-close-workflow-run-guard: %s\n\n' "$HIT" >&2 ;;
esac

printf 'To request a dispatch, mail the concierge:\n' >&2
printf '  spira/mail.sh send operator --from "Builder <builder@spira>" --kind question --subject "dispatch <workflow-file> on <branch>"\n\n' >&2
printf 'The concierge will push the branch and dispatch the workflow, then reply with the run URL.\n\n' >&2
printf 'Override (logs the reason):\n  SPIRA_WORKFLOW_RUN_CONSIDERED=<reason>\n' >&2
exit 2
