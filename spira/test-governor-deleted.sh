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
# PART B RUNS FOR REAL, EVERYWHERE (law-absence-needs-a-positive-control). It used to `skip`
# whenever `$ROOT` was a linked worktree — every testenv run, always (testenv/DESIGN.md
# §4.1: in place, a scratch slot, or a throwaway, never a plain clone) — because the
# sandboxed container bind-mounts only the worktree itself, not the main checkout's .git its
# `.git` file points at, so `git -C "$ROOT"` cannot resolve anything there at all. That skip
# was itself a fail-open: the tree could carry a stray mention and this suite would never
# see it except on GitHub, where `actions/checkout` makes a plain clone with its own .git.
# `scan` below takes a git-free fallback in exactly that situation instead of skipping, so
# the suite runs — and can go red — inside the container too.
#
# Planting a positive control inside the real tree would mean either leaving it there
# (failing the very check it plants for) or mutating this worktree's git index from inside a
# test, so both scan paths are proven on throwaway trees instead: Part A drives the git
# branch (a real, freshly-`git init`-ed repo), Part A2 drives the fallback branch (a plain
# directory, no git at all — `rev-parse --is-inside-work-tree` fails on it exactly as it does
# on a linked worktree with an unreachable commondir), then Part B points the same `scan` at
# this worktree read-only and takes whichever branch this environment actually gets.
#
# WHAT IS EXCLUDED, AND WHY.
#   spira/testdata/*.log      — a real captured sentinel log from before this bead landed
#                                (law-prefer-the-real-dependency); editing it would corrupt
#                                the artifact it exists to be.
#   docs/test-plan/*.md       — narrative status pages that record, in the past tense, what a
#                                prior slice of this same epic found still wired in.
#                                Rewriting history to match today reads as a plan, not a
#                                record.
#   docs/spikes/**            — a frozen research corpus, captured wholesale from real suite
#                                runs; it is data, not a claim about current code.
#   spira/tier-budget-allowlist — the per-suite budget ratchet (spira/tier-budget.sh) lists
#                                every suite slower than its tier's default by filename,
#                                including this one; that row will name "governor" for as
#                                long as this suite keeps its name, and it is bookkeeping
#                                about wall-clock time, not a claim about governor.sh.
#   spira/test-summon-fayth.sh — one dated, bead-cited note (gap row G7) recording that
#                                governor.sh was already deleted (sp-8mzsh) before that suite
#                                was written and there is no hook left to test against —
#                                history, like docs/test-plan/*.md, just not shaped as one of
#                                its files.
#   this file itself          — it names the word to test for it; that is not a live
#                                reference.
# Anything else that names governor.sh after this suite is green is a live reference this
# suite exists to catch, not a seventh exception to add.
#
# tier: T1
# covers: watchtower/src/* sentinel/src/* spira/lib.sh spira/auron-classify.py
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"

echo "test-governor-deleted.sh"

command -v git >/dev/null 2>&1 || skip "git not on PATH"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

EXCLUDE_RE='^spira/testdata/|^docs/test-plan/|^docs/spikes/|^spira/test-governor-deleted\.sh$|^spira/tier-budget-allowlist$|^spira/test-summon-fayth\.sh$'

# scan <repo-root> -> tracked files, minus the historical/data/self exclusions above, that
# contain 'governor' (case-insensitive). Reads the tree; changes nothing.
#
# TWO PATHS, ONE ANSWER. `git -C <repo> ls-files` is tried first — what Part A's real
# throwaway repository takes, and what a plain clone (this suite's own tree on GitHub CI)
# takes too. `git -C <repo> rev-parse --is-inside-work-tree` fails outright (a hard error,
# not "false") when <repo> is a linked worktree whose .git file names a gitdir this
# environment cannot see — see the header. The fallback below is what makes that case run
# instead of skip.
#
# THE FALLBACK WALKS THE DISK. It cannot ask git "tracked", so it asks "the file tree minus
# everything .gitignore already draws the same line at" instead: .git itself, build output
# (target/, the top-level bin/), machine-local state (.runtime/, .beads/, .dolt/), and the
# exact file shapes .gitignore names outright (*.jsonl, *.db*, *.sqlite*, *.pyc). Any of
# those left on disk in a reused slot is ignored-and-present, never tracked-and-absent, so
# this can only ever scan a harmless SUPERSET of what `git ls-files` would — it cannot hide a
# real tracked file the way the old skip could hide the whole scan. In practice the two paths
# see the same files: a worktree is checked out fresh and cleaned before the container boots
# (`git clean -fdq -e target`), and cargo's own target/ is redirected off /workspace entirely
# via CARGO_TARGET_DIR (testenv/DESIGN.md §4.2), so there is usually nothing extra to prune.
scan() {
    local repo="$1"
    if git -C "$repo" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        git -C "$repo" ls-files -z
    else
        find "$repo" \( -name .git -o -name target -o -path "$repo/bin" \
                -o -name .runtime -o -name .beads -o -name .dolt \) -prune \
            -o -type f -print0 \
            | sed -z "s#^$repo/##" \
            | grep -zv -E '\.(jsonl|db|sqlite3?)$|\.db-|\.sqlite3?-|\.pyc$'
    fi \
        | grep -zv -E "$EXCLUDE_RE" \
        | (cd "$repo" && xargs -0 grep -liE 'governor' 2>/dev/null)
}

echo
echo "Part A — the scan (git branch) finds the one file it should, on a throwaway repository:"

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

want_hits="$(printf 'spira/watchtower.sh\n')"
hits="$(scan "$SCRATCH" | sort)"
if [ "$hits" = "$want_hits" ]; then
    ok "A1: the scan finds the live file and skips the three excluded ones"
else
    bad "A1: the scan finds the live file and skips the three excluded ones" \
        "wanted [$want_hits] got [$hits]"
fi

echo
echo "Part A2 — the fallback (no working git) finds the same file and prunes build/runtime state too:"

FALLBACK="$TMP/fallback"
mkdir -p "$FALLBACK/spira/testdata" "$FALLBACK/docs/test-plan" "$FALLBACK/docs/spikes" \
         "$FALLBACK/target" "$FALLBACK/.beads" "$FALLBACK/.runtime"
printf 'the governor used to withhold here\n'      > "$FALLBACK/spira/watchtower.sh"
printf 'governor [measure]: WOULD withhold\n'       > "$FALLBACK/spira/testdata/sentinel-healthy.log"
printf 'governor deletion (sp-8mzsh)\n'             > "$FALLBACK/docs/test-plan/dispatch.md"
printf 'governor\tcorpus row\n'                     > "$FALLBACK/docs/spikes/corpus.tsv"
printf 'nothing to see here\n'                      > "$FALLBACK/spira/lib.sh"
printf 'governor build litter, never tracked\n'     > "$FALLBACK/target/leftover.txt"
printf 'governor beads litter, never tracked\n'     > "$FALLBACK/.beads/leftover.txt"
# No `git init` here. rev-parse --is-inside-work-tree fails on a plain directory exactly as
# it does on a linked worktree with an unreachable commondir, so this exercises the fallback
# branch above with no need to fake a broken gitdir.

hits2="$(scan "$FALLBACK" | sort)"
if [ "$hits2" = "$want_hits" ]; then
    ok "A2: the git-free fallback finds the live file, skips the exclusions, and prunes target/ + .beads/"
else
    bad "A2: the git-free fallback finds the live file, skips the exclusions, and prunes target/ + .beads/" \
        "wanted [$want_hits] got [$hits2]"
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

tl_summary
