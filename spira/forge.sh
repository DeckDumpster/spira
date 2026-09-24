#!/usr/bin/env bash
# forge.sh — forge seam: open and query pull requests; set branch protection.
# SPIRA_FORGE may point to a substitute when a fixture replaces the real forge.
#
# pr-create <repo-dir> <head> <base> <title>   body on stdin; prints PR number
# pr-number <repo-dir> <head>                  prints the open PR number, or empty
# pr-list-queue <repo-dir>                     prints PR numbers with head spira/queue/*, one per line
# pr-mergeability <repo-dir> <pr-number>       prints: DIRTY | CLEAN | UNKNOWN
# check-status <repo-dir> <pr-number>          prints: pending | green | red | harness_fault | provision_fault
#                                              then "flaky: <suite>" for each flaky annotation, and
#                                              "build-error: <line>" per line of a failed build job's error
# run-id <repo-dir> <branch>                   prints the latest CI run ID for the branch
# runs-active <repo-dir>                       prints the count of queued+in_progress PR runs, or ? if unknown
# run-metadata <repo-dir> <run-id>             prints: started-at: <epoch>; last-activity: <epoch>
# run-cancel <repo-dir> <run-id>               cancels an in-progress run
# workflow-rerun <repo-dir> <run-id>           re-queues a failed workflow run
# pr-close <repo-dir> <pr-number>              closes the PR without merging
# pr-comment <repo-dir> <pr-number> <body>     posts a comment to the PR
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
    pr-mergeability)
        pr_n="${1:-}"
        result="$( cd "$repo" && ghq pr view "$pr_n" \
            --json mergeable,mergeStateStatus 2>/dev/null )" || result=""
        printf '%s\n' "${result:-}" | python3 -c "
import json, sys
try:
    d = json.load(sys.stdin)
    m  = (d.get('mergeable') or '').upper()
    ms = (d.get('mergeStateStatus') or '').upper()
    if m == 'CONFLICTING' or ms == 'DIRTY':
        print('DIRTY')
    elif m == 'MERGEABLE':
        print('CLEAN')
    else:
        print('UNKNOWN')
except Exception:
    print('UNKNOWN')
" 2>/dev/null || printf 'UNKNOWN\n'
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
    elif c in ('SKIPPED','NEUTRAL','STALE','CANCELLED'):
        print('harness_fault')
    else:
        print('red')
except Exception:
    print('pending')
" 2>/dev/null)"
        status="${status:-pending}"
        head_sha="$(printf '%s\n' "${rollup_json:-"{}"}" | python3 -c "
import json, sys
try: print(json.load(sys.stdin).get('headRefOid', ''))
except: pass
" 2>/dev/null)"
        run_id="" jobs_json="" build_err=""
        if [ "$status" = "green" ] || [ "$status" = "red" ]; then
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
            if [ -n "${run_id:-}" ]; then
                jobs_json="$( cd "$repo" && ghq api \
                    "repos/{owner}/{repo}/actions/runs/$run_id/jobs" 2>/dev/null )" \
                    || jobs_json="{}"
                # When red, check whether provision failed — gate exit 75 means the
                # branch was never tested; report provision_fault so verdict.sh treats
                # it as infrastructure rather than charging the members with a test red.
                if [ "$status" = "red" ]; then
                    _prov_failed="$(printf '%s\n' "${jobs_json:-"{}"}" | python3 -c "
import json, sys
try:
    for j in json.load(sys.stdin).get('jobs', []):
        if j.get('name') == 'provision' and (j.get('conclusion') or '').lower() not in ('success', ''):
            print('yes'); sys.exit()
except Exception:
    pass
" 2>/dev/null)"
                    [ "${_prov_failed:-}" = "yes" ] && status="provision_fault"
                fi
                # A failed build job precedes suites entirely, so a red here carries no
                # suite annotations — the queue bisects it (queue_bisect_split, lib.sh)
                # rather than attributing to a suite that never ran. Reading the build
                # job's own error names the failing crate/manifest on the ejected bead
                # instead of leaving the operator to open the run by hand.
                if [ "$status" = "red" ]; then
                    _build_job_id="$(printf '%s\n' "${jobs_json:-"{}"}" | python3 -c "
import json, sys
try:
    for j in json.load(sys.stdin).get('jobs', []):
        if j.get('name') == 'build' and (j.get('conclusion') or '').lower() not in ('success', ''):
            print(j.get('id', '')); sys.exit()
except Exception:
    pass
" 2>/dev/null)"
                    if [ -n "${_build_job_id:-}" ]; then
                        build_err="$( cd "$repo" && ghq api \
                            "repos/{owner}/{repo}/actions/jobs/$_build_job_id/logs" 2>/dev/null \
                            | sed -E 's/^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z //' \
                            | grep -E '^error(\[E[0-9]+\])?:|^error: could not compile|failed to load manifest for (workspace member|package)' \
                            | awk '!seen[$0]++' | head -5 )"
                    fi
                fi
            fi
        fi
        # When red, read the full suite list from the batch-results artifact.
        # GitHub caps per-step annotations at 10; the artifact carries the whole list.
        _artifact_out=""
        if [ "$status" = "red" ] && [ -n "${run_id:-}" ]; then
            _art_id="$( cd "$repo" && ghq api \
                "repos/{owner}/{repo}/actions/runs/$run_id/artifacts" 2>/dev/null \
                | python3 -c "
import json, sys
try:
    for a in json.load(sys.stdin).get('artifacts', []):
        if (a.get('name') or '').startswith('batch-results-'):
            print(a['id']); break
except: pass
" 2>/dev/null )" || _art_id=""
            if [ -n "${_art_id:-}" ]; then
                _art_tmp="$(mktemp -d)"
                if ( cd "$repo" && ghq api \
                    "repos/{owner}/{repo}/actions/artifacts/$_art_id/zip" 2>/dev/null ) \
                    > "$_art_tmp/b.zip" 2>/dev/null && [ -s "$_art_tmp/b.zip" ]; then
                    _artifact_out="$(python3 - "$_art_tmp/b.zip" <<'PYEOF' 2>/dev/null
import zipfile, json, sys
try:
    with zipfile.ZipFile(sys.argv[1]) as z:
        rs = next((n for n in z.namelist() if n.endswith('red-suites.json')), None)
        if rs is None:
            sys.exit(0)
        d = json.loads(z.read(rs))
        red = d.get('red', [])
        flaky = d.get('flaky', [])
        red_count = int(d.get('red_count', len(red)))
        if len(red) < red_count:
            print('artifact-truncated: ' + str(len(red)) + '/' + str(red_count))
        else:
            for s in red:
                print('red-suite: ' + s)
            for s in flaky:
                print('flaky: ' + s)
except Exception:
    pass
PYEOF
)"
                fi
                rm -rf "$_art_tmp" 2>/dev/null || true
            fi
            case "${_artifact_out:-}" in
                "artifact-truncated:"*)
                    # Artifact incomplete — fail closed so attribution retries rather
                    # than ejecting against a partial list.
                    status="harness_fault"
                    _artifact_out=""
                    ;;
            esac
        fi
        printf '%s\n' "$status"
        [ -n "${head_sha:-}" ] && printf 'head-sha: %s\n' "$head_sha"
        if [ -n "${build_err:-}" ]; then
            while IFS= read -r _beline; do
                [ -n "$_beline" ] && printf 'build-error: %s\n' "$_beline"
            done <<< "$build_err"
        fi
        [ "$status" = "green" ] || [ "$status" = "red" ] || exit 0
        [ -n "${run_id:-}" ] || exit 0
        if [ -n "${_artifact_out:-}" ]; then
            printf '%s\n' "$_artifact_out"
        else
            # Fall back to per-job annotations (capped at 10 per step by GitHub).
            printf '%s\n' "${jobs_json:-"{}"}" | python3 -c "
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
        fi
        ;;
    run-id)
        branch="${1:-}"
        run_id="$( cd "$repo" && ghq run list --branch "$branch" \
            --json databaseId --limit 1 -q '.[0].databaseId' 2>/dev/null )" || run_id=""
        printf '%s\n' "${run_id:-}"
        ;;
    batch-ci-status)
        # batch-ci-status <repo-dir> <branch> → lines describing the current CI run:
        #   run-id: <id>
        #   run-conclusion: <conclusion>       (when run is completed)
        #   run-completed-at: <epoch>          (when run is completed; from updatedAt)
        #   queued-since: <epoch>              (when any job is in queued status)
        # Uses 2 gh API calls: run list (run_id + conclusion) and jobs (queued-since).
        # Both ci-stalled and ci-red detectors in czar.sh --pass share this output.
        branch="${1:-}"
        run_list="$( cd "$repo" && ghq run list --branch "$branch" \
            --json databaseId,conclusion,status,updatedAt --limit 1 2>/dev/null )" || run_list="[]"
        run_id="$(printf '%s\n' "$run_list" | python3 -c "
import json, sys
try:
    d = json.load(sys.stdin)
    if d: print(d[0].get('databaseId', ''))
except: pass
" 2>/dev/null)"
        [ -n "$run_id" ] || exit 0
        printf 'run-id: %s\n' "$run_id"
        printf '%s\n' "$run_list" | python3 -c "
import json, sys, calendar, datetime
def epoch(t):
    if not t: return 0
    try:
        dt = datetime.datetime.strptime(t.rstrip('Z'), '%Y-%m-%dT%H:%M:%S')
        return calendar.timegm(dt.timetuple())
    except: return 0
try:
    d = json.load(sys.stdin)
    if d and d[0].get('conclusion'):
        print('run-conclusion: ' + d[0]['conclusion'])
        e = epoch(d[0].get('updatedAt') or '')
        if e: print('run-completed-at: ' + str(e))
except: pass
" 2>/dev/null
        jobs_json="$( cd "$repo" && ghq api \
            "repos/{owner}/{repo}/actions/runs/$run_id/jobs" 2>/dev/null )" || exit 0
        printf '%s\n' "$jobs_json" | python3 -c "
import json, sys, calendar, datetime
def epoch(t):
    if not t: return 0
    try:
        dt = datetime.datetime.strptime(t.rstrip('Z'), '%Y-%m-%dT%H:%M:%S')
        return calendar.timegm(dt.timetuple())
    except: return 0
try:
    earliest = 0
    for j in json.load(sys.stdin).get('jobs', []):
        if j.get('status') == 'queued':
            e = epoch(j.get('created_at') or '')
            if e and (earliest == 0 or e < earliest):
                earliest = e
    if earliest: print('queued-since: ' + str(earliest))
except Exception: pass
" 2>/dev/null
        ;;
    queued-since)
        # queued-since <repo-dir> <branch> → epoch seconds when the earliest queued job was
        # created, or empty if no jobs are in queued status. A queued job has no runner
        # assigned and will never start on its own; cancel the run and re-dispatch.
        branch="${1:-}"
        run_id="$( cd "$repo" && ghq run list --branch "$branch" \
            --json databaseId --limit 1 -q '.[0].databaseId' 2>/dev/null )" || run_id=""
        [ -n "$run_id" ] || exit 0
        jobs_json="$( cd "$repo" && ghq api \
            "repos/{owner}/{repo}/actions/runs/$run_id/jobs" 2>/dev/null )" || exit 0
        printf '%s\n' "$jobs_json" | python3 -c "
import json, sys, calendar, datetime
def epoch(t):
    if not t: return 0
    try:
        dt = datetime.datetime.strptime(t.rstrip('Z'), '%Y-%m-%dT%H:%M:%S')
        return calendar.timegm(dt.timetuple())
    except: return 0
try:
    earliest = 0
    for j in json.load(sys.stdin).get('jobs', []):
        if j.get('status') == 'queued':
            e = epoch(j.get('created_at') or '')
            if e and (earliest == 0 or e < earliest):
                earliest = e
    if earliest: print(earliest)
except Exception: pass
" 2>/dev/null
        ;;
    runs-active)
        # runs-active <repo-dir> → how many PULL-REQUEST workflow runs are queued or in
        # progress. Used by batch.sh to answer "is CI idle right now"; idle means waiting for
        # company buys nothing, so cut the batch at once.
        #
        # ONLY pull_request RUNS COUNT. A main-branch gate (push), a release, or a dispatched
        # acceptance run is not competing with a batch for anything the wait could save: runners
        # are cloned per run and the hypervisor has room. Counting them held two certified beads
        # for twenty minutes on 2026-09-23 with no PR open anywhere, while the only runs in
        # flight were a main gate and an acceptance dispatch (per Ryan: "that's not what i
        # would consider 'CI isn't idle'. there are no open PRs").
        #
        # PRINTS ? WHEN IT CANNOT TELL, NEVER 0. A failed API call, an unparseable payload and
        # a genuinely empty queue are three different answers, and only the third one means
        # idle. A caller that read a network failure as "nothing in CI" would cut a batch on
        # every pass while CI was busy, which is the opposite of what this exists for
        # (law-absence-needs-a-positive-control).
        runs_json="$( cd "$repo" && ghq api \
            "repos/{owner}/{repo}/actions/runs?per_page=100&exclude_pull_requests=true" \
            2>/dev/null )" || { printf '?\n'; exit 0; }
        [ -n "${runs_json:-}" ] || { printf '?\n'; exit 0; }
        printf '%s\n' "$runs_json" | python3 -c "
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    print('?'); sys.exit(0)
runs = d.get('workflow_runs')
if runs is None:
    print('?'); sys.exit(0)
print(sum(1 for r in runs if r.get('event') == 'pull_request' and r.get('status') in ('queued', 'in_progress', 'waiting', 'requested', 'pending')))
" 2>/dev/null || printf '?\n'
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
    # run.updated_at advances while CI runs; job/step timestamps stall for the
    # duration of a single-step job until it completes.
    u = epoch(d.get('updated_at') or '')
    if u: print('last-activity: ' + str(u))
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
    run-cancel)
        run_id="${1:-}"
        ( cd "$repo" && ghq run cancel "$run_id" ) 2>/dev/null
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
    pr-comment)
        pr_n="${1:-}" body="${2:-}"
        ( cd "$repo" && ghq pr comment "$pr_n" --body "$body" ) 2>/dev/null
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
