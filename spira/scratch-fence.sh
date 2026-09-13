#!/usr/bin/env bash
#
# scratch-fence.sh — refuse aeon scratch files tracked at the harness root.
#
#   scratch-fence.sh   scan every tracked file; exit 1 naming each offender
#
# WHAT IT REFUSES. Two patterns at the repository root only (no slash in the path):
#   sp-*    aeon working notes named after a bead
#   *.fixed  hand-patched file artefacts
#
# WHY A PROGRAM AND NOT A PARAGRAPH. The harness is cloned by other operators; their
# agents must not inherit working notes from this installation. These files also cost
# real time: a changed file no suite declares triggers the all-suites fallback in
# coverage selection, so one note at the root costs suite time on every branch that
# follows it. The class of defect is repeatable — ten accumulated before this fence.
#
# OVERRIDE. Set SCRATCH_FENCE_OK=1. Use only for the commit that removes existing
# offenders; name the reason in the commit message. Once the tree is clean the fence
# keeps it so.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)"
[ -n "$ROOT" ] || ROOT="$(cd "$HERE/.." && pwd -P)"

# An empty index or a bad root is indistinguishable from a clean tree — refuse to report
# clean without first confirming git can enumerate files
# (law-absence-needs-a-positive-control).
count="$(git -C "$ROOT" ls-files 2>/dev/null | wc -l)"
if [ "${count:-0}" -lt 1 ]; then
    printf 'scratch-fence: refusing to report clean — no tracked files found (bad root or empty index)\n' >&2
    exit 3
fi

offenders="$(git -C "$ROOT" ls-files 2>/dev/null | grep -E '^(sp-[^/]+|[^/]+\.fixed)$' || true)"

if [ -z "$offenders" ]; then
    printf 'scratch-fence: no aeon scratch files at root\n'
    exit 0
fi

if [ "${SCRATCH_FENCE_OK:-}" = "1" ]; then
    printf 'scratch-fence: SCRATCH_FENCE_OK=1 — override accepted; offenders present:\n' >&2
    printf '%s\n' "$offenders" | sed 's/^/scratch-fence:   /' >&2
    exit 0
fi

printf 'scratch-fence: aeon scratch files must not land in the harness root:\n' >&2
printf '%s\n' "$offenders" | sed 's/^/scratch-fence:   /' >&2
printf 'scratch-fence: delete them; set SCRATCH_FENCE_OK=1 only for the commit that removes them.\n' >&2
exit 1
