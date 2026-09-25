#!/usr/bin/env bash
#
# released-defects.sh — report released defects over a configurable window.
#
#   released-defects.sh [--window <days>] [--graph <file>]
#
# --graph <file>: read the closed-bug discovered-from graph (the bd list --status
# closed --type bug --json shape) from <file> instead of querying bd. The join
# this script does is pure over that list; git stays real either way.
#
# A RELEASED DEFECT is a bug bead (type: bug) with a 'discovered-from' link to
# another bead (the introducing bead), where the introducing bead's commit
# reached the base branch BEFORE the fixing bead's commit. Both commits are
# found by searching commit messages on the base branch for each bead's id.
#
# A defect is CAUGHT (not counted) when the introducing bead has no commit on
# the base branch — it was rejected before it could escape. A defect fixed in
# the same commit as its introduction is not counted (same unit).
#
# To mark that a bug was released from a bead's work:
#   bd link <fix-bead> <introducing-bead> --type discovered-from
#
# OUTPUT: one line per released defect —
#   RELEASED  <fix-bead>  <intro-bead>  <intro-commit>  <fix-commit>
#
# A field that cannot be determined renders ?, never 0 or empty — a probe that
# cannot read a field must not report all-clear (law-alerts-must-be-actionable).
#
# WHAT IS NOT COUNTED:
#   - A caught defect: the introducing bead never reached the base branch
#   - Same unit: both beads name the same commit
#   - A bug bead with no discovered-from link (origin unknown, not tracked)

# covers: spira/released-defects.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

WINDOW_DAYS=""
GRAPH_FILE=""
while [ $# -gt 0 ]; do
    case "$1" in
        --window) WINDOW_DAYS="${2:-}"; shift ;;
        --graph) GRAPH_FILE="${2:-}"; shift ;;
        -h|--help) sed -n '2,32p' "$0"; exit 0 ;;
        -*) printf 'released-defects.sh: unknown flag %s\n' "$1" >&2; exit 2 ;;
    esac
    shift
done

if [ -n "$WINDOW_DAYS" ]; then
    case "$WINDOW_DAYS" in ''|*[!0-9]*)
        printf 'released-defects.sh: --window requires a number of days\n' >&2; exit 2 ;;
    esac
fi

# commit_for <bead-id> <repo> <refs...> -> SHA of the most recent commit naming
# the bead on those refs, or empty. No pipe into head -1 under pipefail: capture
# whole, then trim (law-no-grep-q-under-pipefail).
commit_for() {
    local id="$1" repo="$2"; shift 2
    [ -e "$repo/.git" ] || return 0
    local out
    # --fixed-strings: bead ids contain dots, which are regex wildcards without it.
    # -n limits the number of OUTPUT commits; combined with --grep this still searches
    # all history. For a bead id that appears once the first match is correct.
    # shellcheck disable=SC2086
    out="$(git -C "$repo" log --format='%H' --fixed-strings --grep="$id" \
               -n "${SPIRA_VERDICT_WINDOW:-400}" "$@" 2>/dev/null)" || out=""
    printf '%s' "${out%%$'\n'*}"
}

# Fetch all closed bug beads — from the --graph fixture when given, else real bd.
if [ -n "$GRAPH_FILE" ]; then
    bugs_json="$(cat "$GRAPH_FILE" 2>/dev/null)" || bugs_json=""
else
    bugs_json="$(bdjson list --status closed --type bug --limit 0 2>/dev/null)" || bugs_json=""
fi

# A probe that cannot read should say so, not silently report clean.
if [ -z "$bugs_json" ]; then
    printf 'released-defects.sh: could not read bead database\n' >&2
    exit 1
fi

# Extract (fix_id, intro_id, repo_name) pairs from the bug bead list.
# Dependencies in bd list JSON use the key 'type' (not 'dependency_type') and
# 'depends_on_id' for the upstream bead id.
pairs="$(printf '%s\n' "$bugs_json" | python3 -c '
import sys, json, time

rows = json.load(sys.stdin)
if not isinstance(rows, list): rows = []

window_days = sys.argv[1] if sys.argv[1:] else ""
cutoff = float(time.time() - int(window_days) * 86400) if window_days else 0.0

for bug in rows:
    bug_id = bug.get("id") or ""
    if not bug_id:
        continue

    # Apply window filter on the close time
    if cutoff:
        ts_str = bug.get("closed_at") or bug.get("updated_at") or ""
        if ts_str:
            try:
                from datetime import datetime, timezone
                ts = datetime.fromisoformat(ts_str.replace("Z", "+00:00")).timestamp()
                if ts < cutoff:
                    continue
            except Exception:
                pass  # keep on parse failure; do not silently drop

    # Repo label — "repo:<name>" in the labels list
    labels = bug.get("labels") or []
    repo_name = ""
    for lbl in labels:
        if isinstance(lbl, str) and lbl.startswith("repo:"):
            repo_name = lbl[5:]
            break

    # Find discovered-from dependencies (what bug_id was discovered from)
    for dep in (bug.get("dependencies") or []):
        dep_type = dep.get("type") or dep.get("dependency_type") or ""
        if dep_type != "discovered-from":
            continue
        intro_id = dep.get("depends_on_id") or dep.get("id") or ""
        if not intro_id or intro_id == bug_id:
            continue
        print(bug_id, intro_id, repo_name)
        break  # one discovered-from link per defect is the convention
' "$WINDOW_DAYS" 2>/dev/null)" || pairs=""

if [ -z "$pairs" ]; then
    printf 'no released defects%s\n' "${WINDOW_DAYS:+ in the last ${WINDOW_DAYS} days}"
    exit 0
fi

released=0

while IFS=' ' read -r fix_id intro_id repo_name; do
    [ -n "$fix_id" ] && [ -n "$intro_id" ] || continue

    # Resolve the git checkout and base refs for this repo.
    root=""; refs=""
    if [ -n "$repo_name" ]; then
        root="$(repo_root "$repo_name" 2>/dev/null)" || root=""
    fi
    # Fall back to the home repo when no repo label is present
    [ -n "$root" ] || root="$(repo_root 2>/dev/null)" || root=""

    if [ -n "$root" ]; then
        refs="$(spira_landrefs "$root" 2>/dev/null)" || refs=""
    fi

    # A repo we cannot locate: report both fields as ? (law-alerts-must-be-actionable)
    if [ -z "$root" ] || [ -z "$refs" ]; then
        printf 'RELEASED\t%s\t%s\t?\t?\n' "$fix_id" "$intro_id"
        released=$((released+1))
        continue
    fi

    # Find the introducing commit — the commit on the base branch naming the intro bead.
    # shellcheck disable=SC2086
    ca="$(commit_for "$intro_id" "$root" $refs)"
    [ -n "$ca" ] || ca=""

    # No introducing commit on the base branch → the bug was CAUGHT before release.
    # Do not count it; do not print it (the caller asked for released defects only).
    if [ -z "$ca" ]; then
        continue
    fi

    # Find the fixing commit — the commit naming the fix bead.
    # shellcheck disable=SC2086
    cb="$(commit_for "$fix_id" "$root" $refs)"
    [ -n "$cb" ] || cb=""

    # Fix not yet on the base branch — unresolvable; render ? rather than 0.
    if [ -z "$cb" ]; then
        printf 'RELEASED\t%s\t%s\t%s\t?\n' "$fix_id" "$intro_id" "$ca"
        released=$((released+1))
        continue
    fi

    # Same commit: introduced and fixed in the same landing unit — not counted.
    if [ "$ca" = "$cb" ]; then
        continue
    fi

    # Check ordering: Ca must be an ancestor of Cb (intro before fix).
    if git -C "$root" merge-base --is-ancestor "$ca" "$cb" 2>/dev/null; then
        printf 'RELEASED\t%s\t%s\t%.12s\t%.12s\n' "$fix_id" "$intro_id" "$ca" "$cb"
        released=$((released+1))
    fi
    # If Ca is NOT an ancestor of Cb, the fix came before the introduction — unusual
    # and most likely a data error; skip silently.

done <<< "$pairs"

if [ "$released" -eq 0 ]; then
    printf 'no released defects%s\n' "${WINDOW_DAYS:+ in the last ${WINDOW_DAYS} days}"
else
    printf '%s released defect%s\n' "$released" "$([ "$released" -eq 1 ] && printf '' || printf 's')"
fi
