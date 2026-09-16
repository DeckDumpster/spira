#!/usr/bin/env bash
# forge.sh — forge seam: open and query pull requests.
# SPIRA_FORGE may point to a substitute when a fixture replaces the real forge.
#
# pr-create <repo-dir> <head> <base> <title>   body on stdin; prints PR number
# pr-number <repo-dir> <head>                  prints the open PR number, or empty
# check-status <repo-dir> <pr-number>          prints: pending | green | red | harness_fault
#                                              then "flaky: <suite>" for each flaky annotation
# run-id <repo-dir> <branch>                   prints the latest CI run ID for the branch
# workflow-rerun <repo-dir> <run-id>           re-queues a failed workflow run
# pr-close <repo-dir> <pr-number>              closes the PR without merging

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
                 --title "$title" --body-file - ) 2>/dev/null; then
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
    check-status)
        pr_n="${1:-}"
        rollup_json="$( cd "$repo" && ghq pr view "$pr_n" \
            --json statusCheckRollup 2>/dev/null )" || rollup_json="{}"
        status="$(printf '%s\n' "${rollup_json:-{}}" | python3 -c "
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
        [ "$status" = "green" ] || [ "$status" = "red" ] || exit 0
        # Green or red: extract the CI run id to fetch annotations.
        run_id="$(printf '%s\n' "${rollup_json:-{}}" | python3 -c "
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
        if lvl == 'warning' and title == 'flaky suite':
            idx = msg.find(' was red')
            if idx > 0:
                print('flaky: ' + msg[:idx])
        elif lvl == 'error' and title == 'red-twice suite':
            print('red-suite: ' + msg)
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
    workflow-rerun)
        run_id="${1:-}"
        ( cd "$repo" && ghq run rerun "$run_id" --failed ) 2>/dev/null
        ;;
    pr-close)
        pr_n="${1:-}"
        ( cd "$repo" && ghq pr close "$pr_n" ) 2>/dev/null
        ;;
    *)
        printf 'forge.sh: unknown command: %s\n' "$cmd" >&2
        exit 1
        ;;
esac
