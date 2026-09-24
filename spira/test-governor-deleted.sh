#!/usr/bin/env bash
#
# test-governor-deleted.sh — governor.sh is gone and nothing tracked still names it.
#
#   ./test-governor-deleted.sh
#
# WHAT THIS CATCHES. governor.sh ran in 'measure' mode only, for its whole life: it logged
# 'WOULD withhold' and never withheld. The admission throttle (CHECK 7 in sentinel.sh) is
# the mechanism that actually gates a summon. A deletion that removes the program but leaves
# a stub call, a dangling budget.env read, or a stale mention in a doc or a dashboard is a
# deletion that looks done and is not — this suite is the T0 the deliverable asks for.
#
# THE SCAN IS TESTED ON A SCRATCH REPOSITORY, NEVER ON THIS WORKTREE
# (law-absence-needs-a-positive-control). Planting a positive control inside the real tree
# would mean either leaving it there (failing the very check it plants for) or mutating this
# worktree's git index from inside a test. Part A builds a throwaway git repo with one file
# in each excluded prefix and one that is not, proves the scan finds the one that should be
# found and skips the three that should not, then Part B points the same scan at this
# worktree read-only.
#
# WHAT IS EXCLUDED, AND WHY.
#   spira/testdata/*.log   — a real captured sentinel log from before this bead landed
#                             (law-prefer-the-real-dependency); editing it would corrupt the
#                             artifact it exists to be.
#   docs/test-plan/*.md    — narrative status pages that record, in the past tense, what a
#                             prior slice of this same epic found still wired in. Rewriting
#                             history to match today reads as a plan, not a record.
#   docs/spikes/**         — a frozen research corpus, captured wholesale from real suite
#                             runs; it is data, not a claim about current code.
#   this file itself       — it names the word to test for it; that is not a live reference.
# Anything else that names governor.sh after this suite is green is a live reference this
# suite exists to catch, not a fifth exception to add.
#
# covers: spira/watchtower.sh spira/sentinel.sh spira/lib.sh spira/auron-classify.py
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }

echo "test-governor-deleted.sh"

command -v git >/dev/null 2>&1 || { echo "  SKIP  git is not on PATH"; exit 77; }
git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1 \
    || { echo "  SKIP  $ROOT is not a git worktree"; exit 77; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# scan <repo-root> -> tracked files, minus the historical/data/self exclusions above, that
# contain 'governor' (case-insensitive). Reads the tree; changes nothing.
scan() {
    local repo="$1"
    git -C "$repo" ls-files -z \
        | grep -zv -E '^spira/testdata/|^docs/test-plan/|^docs/spikes/|^spira/test-governor-deleted\.sh$' \
        | (cd "$repo" && xargs -0 grep -liE 'governor' 2>/dev/null)
}

echo
echo "Part A — the scan finds the one file it should, on a throwaway repository:"

SCRATCH="$TMP/scratch"
mkdir -p "$SCRATCH/spira/testdata" "$SCRATCH/docs/test-plan" "$SCRATCH/docs/spikes"
printf 'the governor used to withhold here\n'      > "$SCRATCH/spira/watchtower.sh"
printf 'governor [measure]: WOULD withhold\n'       > "$SCRATCH/spira/testdata/sentinel-healthy.log"
printf 'governor deletion (sp-8mzsh)\n'             > "$SCRATCH/docs/test-plan/dispatch.md"
printf 'governor\tcorpus row\n'                     > "$SCRATCH/docs/spikes/corpus.tsv"
printf 'nothing to see here\n'                      > "$SCRATCH/spira/lib.sh"
git -C "$SCRATCH" init -q -b main
git -C "$SCRATCH" -c user.email=t@t -c user.name=t add -A
git -C "$SCRATCH" -c user.email=t@t -c user.name=t commit -q -m scratch

hits="$(scan "$SCRATCH" | sort)"
want_hits="$(printf 'spira/watchtower.sh\n')"
if [ "$hits" = "$want_hits" ]; then
    ok "A1: the scan finds the live file and skips the three excluded ones"
else
    bad "A1: the scan finds the live file and skips the three excluded ones" \
        "wanted [$want_hits] got [$hits]"
fi

echo
echo "Part B — this worktree, read-only:"

real_hits="$(scan "$ROOT")"
if [ -z "$real_hits" ]; then
    ok "B1: no tracked, non-historical file names 'governor'"
else
    bad "B1: no tracked, non-historical file names 'governor'" "$(printf '%s' "$real_hits" | tr '\n' ' ')"
fi

for f in spira/governor.sh spira/governor-budget.py spira/test-governor-host-cores.sh; do
    if [ -e "$ROOT/$f" ]; then
        bad "B2: $f no longer exists" "still present"
    else
        ok "B2: $f no longer exists"
    fi
done

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
