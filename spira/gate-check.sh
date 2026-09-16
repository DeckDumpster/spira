#!/usr/bin/env bash
#
# gate-check.sh — evaluate open gh:run gates and resolve those whose CI has passed.
#
#   gate-check.sh
#
# THE TWO STEPS, IN ORDER.
#
# 1. DISCOVER: for every pr-mode repository in the repo-map, call `bd gate discover`
#    from within that repository's directory. discover calls `gh run list` using the
#    repository's own remote. One discover call per branch stored in open gates'
#    metadata.branch: without an explicit --branch, discover defaults to the repository's
#    current branch (usually the base), and a deployment run on the base branch can
#    satisfy a gate via time proximity alone. Passing the gate's own branch keeps the
#    query scope to that branch's CI runs only.
#
#    A push- or hold-mode repository never opens a pull request, so no CI run exists to
#    discover for it. Calling discover there wastes a gh round-trip and risks matching a
#    coincidentally named branch in the harness repo.
#
# 2. CHECK: `bd gate check --type=gh:run` evaluates every open gh:run gate. For each
#    gate that has an await_id, it calls `gh run view <id> --repo <metadata.repo>` —
#    the --repo flag comes from the gate's own metadata, so a run from the wrong
#    repository cannot resolve a gate for the right one.
#
# CALLED FROM A TIMER, NOT THE SENTINEL. The sentinel's bespoke awaiting-ci sweep has
# been removed; this script is its replacement. It runs on spira-gate-check.timer at
# the same two-minute cadence, independently of a sentinel pass.

# covers: spira/gate-check.sh spira/sentinel.sh spira/aeon.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"
spira_conf

bdq() { "${SPIRA_BD:-bd}" -C "$SPIRA_DB" "$@"; }

# STEP 1: DISCOVER — iterate pr-mode repositories, then call discover once per branch
# recorded in open unbound gates.
#
# ONLY PR-MODE REPOS. push merges directly and hold leaves the branch for a human; in
# both cases the landing gate is the only gate and no CI run exists to discover.
while IFS= read -r name; do
    [ "$(repo_land "$name")" = pr ] || continue
    repo="$(repo_root "$name" 2>/dev/null)" || continue
    [ -d "$repo/.git" ] || continue
    while IFS= read -r branch; do
        [ -n "$branch" ] || continue
        ( cd "$repo" && bdq gate discover --branch "$branch" ) 2>/dev/null || true
    done < <(bdq gate list --json 2>/dev/null | python3 -c '
import json, sys
try:
    gates = json.load(sys.stdin)
    if not isinstance(gates, list): gates = [] if gates is None else [gates]
    seen = set()
    for g in (gates or []):
        if g and g.get("await_type") == "gh:run" and not g.get("await_id"):
            b = (g.get("metadata") or {}).get("branch", "")
            if b and b not in seen:
                seen.add(b)
                print(b)
except Exception:
    pass
' 2>/dev/null)
done < <(repo_names)

# STEP 2: CHECK — evaluate all open gh:run gates.
#
# bd gate check uses metadata.repo on each gate to call `gh run view <id> --repo <org/repo>`,
# so this call is correct regardless of the current directory.
#
# CAPTURE THE OUTPUT TO DETECT CI FAILURES. When a run concludes with failure or cancellation,
# bd gate check prints "⚠ <gate-id>: ESCALATE" but leaves the gate open — the bead stays
# blocked and cannot be re-claimed. Resolving the gate here unblocks the bead so the next
# aeon can pick it up. The ESCALATE line is the only per-gate signal bd gate check produces
# for failures; --json gives only aggregate counts.
check_out="$(bdq gate check --type=gh:run 2>&1 || true)"
printf '%s\n' "$check_out"

# COUNT STUCK GATES — those with no await_id that bd cannot resolve. These are not
# errors by bd's reckoning (they produce no ESCALATE line), but they will never progress,
# so they must be counted separately. Without a distinct count, "0 resolved" is ambiguous
# between "nothing to do" and "every gate is permanently wedged".
stuck=0
while IFS= read -r _line; do
    case "$_line" in *"no run ID specified"*) stuck=$((stuck+1)) ;; esac
done < <(printf '%s\n' "$check_out")
[ "$stuck" -gt 0 ] && printf '%d stuck: no await_id — cannot resolve\n' "$stuck"

# FOR EACH FAILED CI RUN: extract the gate id, find the blocked bead, resolve the gate
# so the bead re-enters the queue, and emit ci.failed so the event feed records it.
#
# GATE_ID EXTRACTION: parse the id from the ESCALATE line format "⚠ GATE_ID: ESCALATE ..."
# without assuming any prefix — a prefix literal here breaks any installation whose
# SPIRA_ID_PREFIX is not the shipped default (law-never-derive-an-id-from-output).
while IFS= read -r esc; do
    gate_id="$(printf '%s' "$esc" | grep -oE '[a-z][a-z0-9]*-[a-z0-9][a-z0-9]*' | head -1)" || continue
    [ -n "${gate_id:-}" ] || continue
    blocked="$(bdq show "$gate_id" --json 2>/dev/null | python3 -c '
import json, sys, re
try:
    d = json.load(sys.stdin)
    d = d if isinstance(d, list) else [d]
    m = re.search(r"blocking ([a-z]+-\w+)", d[0].get("description", ""))
    print(m.group(1) if m else "")
except Exception:
    pass
' 2>/dev/null)" || true
    [ -n "${blocked:-}" ] || continue
    bdq gate resolve "$gate_id" >/dev/null 2>&1 || true
    spira_event ci.failed "$blocked" "CI red — $blocked returned to queue" \
        "$(printf '%s' "$esc")" || true
done < <(printf '%s\n' "$check_out" | grep 'ESCALATE' 2>/dev/null || true)

# STEP 3: FLAKY SUITE BEADS — scan recently completed runs in SPIRA_FLAKY_GH_REPO for
# "flaky suite" annotations; file a P2 bead per suite, deduplicated on the open bead
# for that suite. A suite stays quiet once a bead is open: no second bead until that
# one closes.
#
# Two gh calls per run: one for job ids, one per job for annotations. Both silently
# no-op on network error (|| true) so a missing credential never fails the timer.
if [ -n "${SPIRA_FLAKY_GH_REPO:-}" ] && command -v gh >/dev/null 2>&1; then
    while IFS= read -r _run_id; do
        [ -n "$_run_id" ] || continue
        while IFS= read -r _job_id; do
            [ -n "$_job_id" ] || continue
            while IFS= read -r _suite; do
                [ -n "$_suite" ] || continue
                # Skip if an open bead already exists for this suite.
                _exists="$(bdq list --json 2>/dev/null | python3 -c "
import json, sys
try:
    beads = json.load(sys.stdin)
    if not isinstance(beads, list): beads = []
    title = 'flaky suite: ' + sys.argv[1]
    found = any(b.get('status') in ('open', 'in_progress') and b.get('title') == title
                for b in beads)
    print('yes' if found else '')
except Exception:
    pass
" "$_suite" 2>/dev/null)" || true
                [ -z "${_exists:-}" ] || continue
                printf 'Flaky suite in run %s.\n\n%s was red on a parallel run then green on a serial re-run.\n' \
                    "$_run_id" "$_suite" \
                    | bash "$HERE/bead.sh" file "flaky suite: $_suite" \
                        --for builder --repo "$(spira_home_repo)" \
                        -p 2 --body-file - 2>/dev/null || true
                bash "$HERE/suites.sh" observe-flake "$_suite" 2>/dev/null || true
            done < <(gh api "repos/$SPIRA_FLAKY_GH_REPO/check-runs/$_job_id/annotations" \
                2>/dev/null | python3 -c '
import json, sys
try:
    for a in json.load(sys.stdin):
        if a.get("annotation_level") == "warning" and a.get("title") == "flaky suite":
            msg = a.get("message", "")
            idx = msg.find(" was red")
            if idx > 0:
                print(msg[:idx])
except Exception:
    pass
' 2>/dev/null || true)
        done < <(gh api "repos/$SPIRA_FLAKY_GH_REPO/actions/runs/$_run_id/jobs" \
            2>/dev/null | python3 -c '
import json, sys
try:
    for j in json.load(sys.stdin).get("jobs", []):
        print(j["id"])
except Exception:
    pass
' 2>/dev/null || true)
    done < <(gh run list --repo "$SPIRA_FLAKY_GH_REPO" --status completed --limit 20 \
        --json databaseId --jq '.[].databaseId' 2>/dev/null || true)
fi

# STEP 4: RED-TWICE SUITE BEADS — scan recently completed FAILED main-branch runs for
# "red-twice suite" annotations (filed by gate-retry.sh); file ONE P1 bead per broken
# suite through bead.sh. Deduplicated on the suite while a bead for it is open: a
# pre-existing open bead at lower priority is raised to P1 with evidence added rather
# than duplicated.
#
# Only push-to-main runs carry the full corpus; the "red twice" verdict only appears there.
# Limiting to --branch main keeps this from reacting to PR runs that share the repo.
if [ -n "${SPIRA_FLAKY_GH_REPO:-}" ] && command -v gh >/dev/null 2>&1; then
    _rt_repo="$SPIRA_FLAKY_GH_REPO"
    _rt_home_repo="$(spira_home_repo 2>/dev/null || true)"
    _rt_git_root="$(repo_root "$_rt_home_repo" 2>/dev/null || true)"
    _rt_seen=""
    _rt_last_green="$(gh run list --repo "$_rt_repo" --branch main --status success \
        --limit 1 --json headSha --jq '.[0].headSha' 2>/dev/null || true)"
    while IFS=$'\t' read -r _rt_run_id _rt_sha; do
        [ -n "$_rt_run_id" ] || continue
        while IFS= read -r _rt_job_id; do
            [ -n "$_rt_job_id" ] || continue
            while IFS= read -r _rt_suite; do
                [ -n "$_rt_suite" ] || continue
                case " $_rt_seen " in *" $_rt_suite "*) continue ;; esac
                _rt_seen="$_rt_seen $_rt_suite"
                _rt_existing="$(bdq list --json 2>/dev/null | python3 -c "
import json, sys
try:
    beads = json.load(sys.stdin)
    if not isinstance(beads, list): beads = []
    title = 'suite red on main: ' + sys.argv[1]
    for b in beads:
        if b.get('status') in ('open', 'in_progress') and b.get('title') == title:
            print(b['id'] + '\t' + str(b.get('priority', 2)))
            break
except Exception:
    pass
" "$_rt_suite" 2>/dev/null)" || true
                if [ -n "${_rt_existing:-}" ]; then
                    _rt_bid="${_rt_existing%%	*}"
                    _rt_bpri="${_rt_existing##*	}"
                    [ "${_rt_bpri:-2}" -le 1 ] && continue
                    bdq priority "$_rt_bid" 1 >/dev/null 2>&1 || true
                    bdq note "$_rt_bid" \
                        "Raised to P1: $_rt_suite still red on main (run $_rt_run_id, commit ${_rt_sha:-?})" \
                        >/dev/null 2>&1 || true
                    continue
                fi
                _rt_fail_lines="$(gh run view "$_rt_run_id" --repo "$_rt_repo" --log-failed \
                    2>/dev/null | grep 'FAIL' | sed 's/^[^\t]*\t[^\t]*\t//' | head -10 || true)"
                _rt_commits=""
                if [ -n "${_rt_git_root:-}" ] && [ -n "${_rt_last_green:-}" ] \
                   && [ -n "${_rt_sha:-}" ] && [ "$_rt_last_green" != "$_rt_sha" ]; then
                    _rt_commits="$(git -C "$_rt_git_root" log --oneline \
                        "${_rt_last_green}..${_rt_sha}" 2>/dev/null || true)"
                fi
                printf '%s is red on main and blocks the release.\n\nRun: %s\nFirst red commit: %s\nLast green commit: %s\n\nFailing assertions:\n%s\n\nCommits in range:\n%s\n' \
                    "$_rt_suite" "$_rt_run_id" "${_rt_sha:-?}" \
                    "${_rt_last_green:-(unknown)}" \
                    "${_rt_fail_lines:-(none captured)}" \
                    "${_rt_commits:-(range unknown)}" \
                    | bash "$HERE/bead.sh" file "suite red on main: $_rt_suite" \
                        --for builder --repo "$_rt_home_repo" \
                        -p 1 --body-file - 2>/dev/null || true
            done < <(gh api "repos/$_rt_repo/check-runs/$_rt_job_id/annotations" \
                2>/dev/null | python3 -c '
import json, sys
try:
    for a in json.load(sys.stdin):
        if a.get("annotation_level") == "failure" and a.get("title") == "red-twice suite":
            print(a.get("message", ""))
except Exception:
    pass
' 2>/dev/null || true)
        done < <(gh api "repos/$_rt_repo/actions/runs/$_rt_run_id/jobs" \
            2>/dev/null | python3 -c '
import json, sys
try:
    for j in json.load(sys.stdin).get("jobs", []):
        print(j["id"])
except Exception:
    pass
' 2>/dev/null || true)
    done < <(gh run list --repo "$_rt_repo" --branch main --status failure --limit 10 \
        --json databaseId,headSha \
        --jq '.[] | [(.databaseId|tostring), .headSha] | @tsv' 2>/dev/null || true)
fi
