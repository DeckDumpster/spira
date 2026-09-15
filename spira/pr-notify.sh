#!/usr/bin/env bash
#
# pr-notify.sh — report green/unmerged PRs, red PRs, and spira/* branches with no PR.
#
#   pr-notify.sh            scan; append findings to $SPIRA_RUN/watchd/pr-notify.log
#   pr-notify.sh --show     scan; print to stdout only, touch nothing
#
# WHY THIS EXISTS. law-green-prs-merge-themselves required visibility into which managed-repo
# PRs are ready to merge or have broken CI. That visibility was provided by gate-check; when
# gate-check was turned off, it went with it. This script restores it as a pure visibility
# tool: a timer fires every thirty minutes, scans managed repos for PRs that need attention,
# and appends findings to a watcher log the operator can drain. Nothing is filed, nothing is
# merged, no gate is touched.
#
# WHAT IT REPORTS (each line is a watcher log entry)
#
#   ⚠ GREEN #N <title> [<repo>]   open PR whose CI checks all passed; ready to hand-merge
#   FAIL RED #N <title> [<repo>]  open PR with at least one failing check
#   ⚠ BRANCH <ref>: no PR        spira/* branch ahead of base in a pr-mode repo, no open PR
#
# ⚠ and FAIL are both in the default SPIRA_ACTIONABLE set, so `watchd.sh notify` escalates
# findings that sit unread past SPIRA_NOTIFY_AGE. `watchd.sh drain pr-notify` delivers
# them to a session's attention pane.
#
# A PASS THAT FINDS NOTHING EMITS NOTHING. Silence is the healthy state, not a finding.
# Silence and a broken probe are different: a broken probe exits non-zero and says why.
# A pass over repos with no gh access is silent, not an error: the check itself is running
# on behalf of the operator, who knows whether gh is configured.
#
# GREEN VS RED CLASSIFICATION
#
#   GREEN: statusCheckRollup is non-empty, every check is COMPLETED, every conclusion is
#          SUCCESS. No checks at all → skip (state unknown; not yet run or not configured).
#   RED:   at least one conclusion is FAILURE, CANCELLED, TIMED_OUT, STALE, or
#          ACTION_REQUIRED. A check still running → not yet red; wait for it.
#
# MANAGED REPOS, NOT ALL REPOS. Only repos in the repo-map with land=pr participate:
# a push-mode repo merges directly through the gate and never has an unmerged PR; a
# hold-mode repo is the operator's to touch, and a timer that reports everything it holds
# is noise (law-alerts-must-be-actionable).

# covers: spira/pr-notify.sh spira/watchers systemd/spira-pr-notify.service systemd/spira-pr-notify.timer systemd/units.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

case "${1:-}" in
    --show|-s) _show=1 ;;
    "")        _show=0 ;;
    *) echo "usage: pr-notify.sh [--show]" >&2; exit 2 ;;
esac

LOGFILE="$SPIRA_RUN/watchd/pr-notify.log"

# _emit <line> — write to log (or stdout in --show mode).
_emit() {
    if [ "$_show" -eq 1 ]; then
        printf '%s\n' "$1"
    else
        mkdir -p "$(dirname "$LOGFILE")"
        printf '%s\n' "$1" >> "$LOGFILE"
    fi
}

# _PR_PY — Python3 script to classify open PRs from `gh pr list --json` output.
#
# Reads JSON from stdin, writes classified lines to stdout.
# sys.argv[1] is the repo label for the output line.
#
# NOT a here-doc: this variable is passed to `python3 -c` so `|` can supply stdin
# separately. A here-doc redirect (`<<`) and a pipe (`|`) both bind stdin and conflict —
# the here-doc wins and swallows the JSON the script is meant to parse.
_PR_PY='
import json, sys
repo = sys.argv[1]
try:
    prs = json.load(sys.stdin)
except Exception:
    sys.exit(0)
BAD = {"FAILURE", "CANCELLED", "TIMED_OUT", "STALE", "ACTION_REQUIRED"}
for pr in prs:
    n      = pr.get("number", "?")
    title  = (pr.get("title") or "").replace("\n", " ")
    checks = pr.get("statusCheckRollup") or []
    if not checks:
        continue
    conclusions = [c.get("conclusion") or "" for c in checks]
    statuses    = [c.get("status")     or "" for c in checks]
    if any(c in BAD for c in conclusions):
        print(f"FAIL RED #{n} {title} [{repo}]")
    elif all(s == "COMPLETED" for s in statuses) and all(c == "SUCCESS" for c in conclusions):
        print(f"⚠ GREEN #{n} {title} [{repo}]")
'

# _scan_repo <repo-dir> <repo-name> <base-branch>
#
# Query open PRs and emit actionable lines:
#   FAIL RED  — at least one check concluded FAILURE/CANCELLED/TIMED_OUT/STALE
#   ⚠ GREEN   — all checks completed and all succeeded
#   ⚠ BRANCH  — spira/* branch ahead of base, no open PR
#
# Runs from within $repo_dir so `gh` resolves the repo automatically. A gh invocation
# that fails is skipped silently — a transient network error is not an operator finding.
_scan_repo() {
    local repo_dir="$1" repo_name="$2" base="$3"
    local pr_json line

    # ---- open PR classification -----------------------------------------------
    pr_json="$(cd "$repo_dir" && gh pr list --state open \
        --json number,title,headRefName,statusCheckRollup 2>/dev/null)" || pr_json=""

    if [ -n "$pr_json" ]; then
        while IFS= read -r line; do
            [ -n "$line" ] && _emit "$line"
        done < <(printf '%s' "$pr_json" | python3 -c "$_PR_PY" "$repo_name")
    fi

    # ---- spira/* branches with no open PR ------------------------------------
    # A spira/* branch was created by an aeon but might not have a PR yet.
    # Report those ahead of base so the operator knows they exist.
    git -C "$repo_dir" fetch --quiet 2>/dev/null || true
    local ref short ahead pr_n
    while IFS= read -r ref; do
        ref="${ref#  }"
        # Skip remote-tracking metadata (e.g. origin/HEAD -> origin/main)
        case "$ref" in *" -> "*) continue ;; esac
        short="${ref#origin/}"
        [ "$short" = "$ref" ] && continue  # not from origin; skip
        case "$short" in spira/*) ;; *) continue ;; esac

        # Ahead of base?  rev-list A..B: commits in B not in A.
        ahead="$(git -C "$repo_dir" rev-list --count "origin/$base..origin/$short" 2>/dev/null)"
        case "$ahead" in ""|0) continue ;; esac

        # Open PR for this branch?
        pr_n="$(cd "$repo_dir" && gh pr list --head "$short" --state open \
            --json number --jq 'length' 2>/dev/null)" || pr_n=""
        case "$pr_n" in ""|0) _emit "⚠ BRANCH $short: no PR [$repo_name]" ;; esac
    done < <(git -C "$repo_dir" branch -r 2>/dev/null)
}

# Iterate pr-mode repos in the repo-map.
# push-mode and hold-mode repos are not scanned: a push-mode repo merges directly through
# the gate (no PR exists), and hold-mode branches are the operator's own (no timer noise).
while IFS= read -r name; do
    [ "$(repo_land "$name" 2>/dev/null)" = pr ] || continue
    repo="$(repo_root "$name" 2>/dev/null)" || continue
    [ -d "$repo/.git" ] || continue
    base="$(repo_base "$name" 2>/dev/null)"; [ -n "$base" ] || base="main"
    _scan_repo "$repo" "$name" "$base"
done < <(repo_names 2>/dev/null)
