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
