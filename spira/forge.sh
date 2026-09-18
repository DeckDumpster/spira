#!/usr/bin/env bash
# forge.sh — forge seam: open and query pull requests; set branch protection.
# SPIRA_FORGE may point to a substitute when a fixture replaces the real forge.
#
# pr-create <repo-dir> <head> <base> <title>   body on stdin; prints PR number
# pr-number <repo-dir> <head>                  prints the open PR number, or empty
# pr-list-queue <repo-dir>                     prints PR numbers with head spira/queue/*, one per line
# check-status <repo-dir> <pr-number>          prints: pending | green | red | harness_fault
#                                              then "flaky: <suite>" for each flaky annotation
# run-id <repo-dir> <branch>                   prints the latest CI run ID for the branch
# workflow-rerun <repo-dir> <run-id>           re-queues a failed workflow run
# pr-close <repo-dir> <pr-number>              closes the PR without merging
# branch-protect <repo-dir> <branch>           set: required gate check, no force-push, no delete
# branch-protection-status <repo-dir> <branch> prints: protected | unprotected

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"

cmd="${1:-}"; shift
repo="${1:-}"; shift

case "$cmd" in
    pr-create)
        head="${1:-}" base="${2:-}" title="${3:-}"
        if ! ( cd "$repo" && ghq pr create --head "$head" --base "$base" \
                 --title "$title" --body-file - ) >/dev/null 2>&1; then
            exit 1
        fi
        n="$( cd "$repo" && ghq pr view "$head" --json number -q .number 2>/dev/null )"
        printf '%s\n' "${n:-}"
        ;;
    pr-number)
        head="${1:-}"
        n="$( cd "$repo" && ghq pr view "$head" --json number -q .number 2>/dev/null )"
        printf '%s\n' "${n:-}"
        ;;
    pr-list-queue)
        ( cd "$repo" && ghq pr list --state open \
            --json number,headRefName 2>/dev/null ) | python3 -c "
import json, sys
try:
    for pr in json.load(sys.stdin):
        h = pr.get('headRefName', '')
        if h.startswith('spira/queue/'):
            print(pr['number'])
except Exception:
    pass
" 2>/dev/null
        ;;
    check-status)
        pr_n="${1:-}"
        rollup_json="$( cd "$repo" && ghq pr view "$pr_n" \
            --json statusCheckRollup,headRefOid 2>/dev/null )" || rollup_json="{}"
        status="$(printf '%s\n' "${rollup_json:-"{}"}" | python3 -c "
import json, sys
try:
    checks = (json.load(sys.stdin).get('statusCheckRollup') or [])
    gate = next((c for c in checks if c.get('name') == 'gate'), None)
    if not gate:
        print('pending'); sys.exit()
    st = (gate.get('status') or '').upper()
    c  = (gate.get('conclusion') or '').upper()
    if st in ('QUEUED','IN_PROGRESS','WAITING','REQUESTED','PENDING',''):
        print('pending')
    elif c == 'SUCCESS':
        print('green')
    elif c in ('SKIPPED','NEUTRAL','STALE'):
        print('harness_fault')
    else:
        print('red')
except Exception:
    print('pending')
" 2>/dev/null)"
        status="${status:-pending}"
        printf '%s\n' "$status"
        head_sha="$(printf '%s\n' "${rollup_json:-"{}"}" | python3 -c "
import json, sys
try: print(json.load(sys.stdin).get('headRefOid', ''))
except: pass
" 2>/dev/null)"
        [ -n "${head_sha:-}" ] && printf 'head-sha: %s\n' "$head_sha"
        [ "$status" = "green" ] || [ "$status" = "red" ] || exit 0
        # Green or red: extract the CI run id to fetch annotations.
        run_id="$(printf '%s\n' "${rollup_json:-"{}"}" | python3 -c "
import json, sys, re
try:
    checks = (json.load(sys.stdin).get('statusCheckRollup') or [])
    gate = next((c for c in checks if c.get('name') == 'gate'), None)
    if gate:
        url = gate.get('detailsUrl') or ''
        m = re.search(r'/runs/(\d+)', url)
        if m: print(m.group(1))
except Exception:
    pass
" 2>/dev/null)"
        [ -n "${run_id:-}" ] || exit 0
        ( cd "$repo" && ghq api "repos/{owner}/{repo}/actions/runs/$run_id/jobs" 2>/dev/null ) \
        | python3 -c "
import json, sys
try:
    for j in json.load(sys.stdin).get('jobs', []):
        print(j['id'])
except Exception:
    pass
" 2>/dev/null \
        | while IFS= read -r _jid; do
            ( cd "$repo" && ghq api \
                "repos/{owner}/{repo}/check-runs/$_jid/annotations" 2>/dev/null ) \
            | python3 -c "
import json, sys
try:
    for a in json.load(sys.stdin):
        lvl = a.get('annotation_level', '')
        title = a.get('title', '')
        msg = a.get('message', '')
        path = a.get('path', '')
        if lvl == 'warning' and title == 'flaky suite':
            idx = msg.find(' was red')
            if idx > 0:
                print('flaky: ' + msg[:idx])
        elif lvl == 'error' and title == 'red-twice suite':
            print('red-suite: ' + msg)
        elif lvl == 'failure' and path.startswith('spira/test-') and path.endswith('.sh'):
            print('red-suite: ' + path.split('/')[-1])
except Exception:
    pass
" 2>/dev/null || true
        done
        ;;
    run-id)
        branch="${1:-}"
        run_id="$( cd "$repo" && ghq run list --branch "$branch" \
            --json databaseId --limit 1 -q '.[0].databaseId' 2>/dev/null )" || run_id=""
        printf '%s\n' "${run_id:-}"
        ;;
    run-metadata)
        run_id="${1:-}"
        [ -n "$run_id" ] || exit 1
        run_json="$( cd "$repo" && ghq api "repos/{owner}/{repo}/actions/runs/$run_id" \
            2>/dev/null )" || run_json="{}"
        printf '%s\n' "$run_json" | python3 -c "
import json, sys, calendar, datetime
def epoch(t):
    if not t: return 0
    try:
        dt = datetime.datetime.strptime(t.rstrip('Z'), '%Y-%m-%dT%H:%M:%S')
        return calendar.timegm(dt.timetuple())
    except: return 0
try:
    d = json.load(sys.stdin)
    e = epoch(d.get('run_started_at') or d.get('created_at') or '')
    if e: print('started-at: ' + str(e))
except Exception: pass
" 2>/dev/null
        jobs_json="$( cd "$repo" && ghq api \
            "repos/{owner}/{repo}/actions/runs/$run_id/jobs" 2>/dev/null )" || jobs_json="{}"
        printf '%s\n' "$jobs_json" | python3 -c "
import json, sys, calendar, datetime
def epoch(t):
    if not t: return 0
    try:
        dt = datetime.datetime.strptime(t.rstrip('Z'), '%Y-%m-%dT%H:%M:%S')
        return calendar.timegm(dt.timetuple())
    except: return 0
try:
    latest = 0
    for j in json.load(sys.stdin).get('jobs', []):
        for f in ('started_at', 'completed_at'):
            e = epoch(j.get(f) or ''); latest = max(latest, e)
        for s in j.get('steps', []):
            for f in ('started_at', 'completed_at'):
                e = epoch(s.get(f) or ''); latest = max(latest, e)
    if latest: print('last-activity: ' + str(latest))
except Exception: pass
" 2>/dev/null
        ;;
    workflow-rerun)
        run_id="${1:-}"
        st="$( cd "$repo" && ghq run view "$run_id" --json status -q .status 2>/dev/null )" || st=""
        case "${st:-}" in
            in_progress|queued)
                ( cd "$repo" && ghq run cancel "$run_id" ) 2>/dev/null || true
                ;;
        esac
        ( cd "$repo" && ghq run rerun "$run_id" )
        ;;
    pr-close)
        pr_n="${1:-}"
        ( cd "$repo" && ghq pr close "$pr_n" ) 2>/dev/null
        ;;
    branch-protect)
        base="${1:-}"
        [ -n "$base" ] || { printf 'forge.sh: branch-protect: base branch required\n' >&2; exit 1; }
        # enforce_admins=true so an admin token cannot push an unchecked SHA as a bypass.
        # app_id pins the source to the Actions app; -1 or omission admits a hand-posted status.
        # No required_pull_request_reviews: the fast-forward push in queue mode is not a PR.
        printf '{"required_status_checks":{"strict":false,"checks":[{"context":"gate","app_id":%d}]},"enforce_admins":true,"required_pull_request_reviews":null,"restrictions":null,"allow_force_pushes":false,"allow_deletions":false,"block_creations":false}\n' \
            "${SPIRA_QUEUE_ACTIONS_APP_ID:-15368}" \
        | ( cd "$repo" && ghq api "repos/{owner}/{repo}/branches/$base/protection" \
              --method PUT --input - ) 2>&1
        ;;
    branch-protection-status)
        base="${1:-}"
        [ -n "$base" ] || { printf 'unprotected\n'; exit 0; }
        result="$( cd "$repo" && ghq api "repos/{owner}/{repo}/branches/$base" 2>/dev/null \
            | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
    print("protected" if d.get("protected") else "unprotected")
except Exception:
    print("unprotected")
' 2>/dev/null )" || result="unprotected"
        printf '%s\n' "${result:-unprotected}"
        ;;
    *)
        printf 'forge.sh: unknown command: %s\n' "$cmd" >&2
        exit 1
        ;;
esac
