#!/usr/bin/env bash
#
# test-governor-deleted.sh — the governor is gone, and nothing in the tree still names it.
#
#   ./test-governor-deleted.sh
#
# WHY A GREP AND NOT A STRUCTURAL CHECK. governor.sh, governor-budget.py and their budget.env
# plumbing were deleted outright — the admission throttle (sentinel CHECK 7) is now the only
# summon gate, and watchtower.sh carries disk and memory as vital signs instead. A dangling
# mention left behind — a comment, a stub name in a test's fixture list, a config example —
# teaches the next reader a mechanism that no longer runs, and sometimes to go looking for it.
#
# THE POSITIVE CONTROL COMES FIRST (law-a-regression-test-must-be-seen-to-fail). A scan that
# always reports clean proves nothing; this plants an offender and requires the same grep to
# find it before trusting a clean result over the real tree.
#
# TWO PATHS ARE EXEMPT, and both for the same reason: they are a historical record, not a
# live reference. Rewriting them to erase "governor" would falsify the record rather than
# reflect that the mechanism is gone.
#   spira/testdata/sentinel-healthy.log        — ~90 real sentinel passes captured while the
#                                                 governor was running, used by test-auron.sh
#                                                 as the noise-floor fixture.
#   docs/spikes/sources/sp-pmv67/data/*.tsv    — a captured corpus of real suite-run history,
#                                                 including runs of the (also deleted)
#                                                 test-governor-host-cores.sh.
#
# covers: spira/governor.sh spira/sentinel.sh spira/lib.sh spira/watchtower.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)"
[ -n "$ROOT" ] || ROOT="$(cd "$HERE/.." && pwd -P)"

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }

echo "test-governor-deleted.sh"

# ---------------------------------------------------------------------------------------
# POSITIVE CONTROL — the matcher can find an offender before it is trusted to find none.
# ---------------------------------------------------------------------------------------
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
printf '# the governor withholds here\n' > "$TMP/planted.sh"
if grep -qil "governor" "$TMP/planted.sh"; then
    ok "the matcher finds a planted mention (positive control)"
else
    bad "the matcher finds a planted mention (positive control)" "grep -qil found nothing"
    printf '\n%d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi

# ---------------------------------------------------------------------------------------
# THE REAL SCAN. git ls-files (tracked) plus --others --exclude-standard (staged-but-new)
# — the same two-list union inventory.sh uses — so a file not yet committed still counts.
#
# A CONTAINERISED WORKTREE HAS NO RESOLVABLE .git (law-tests-run-only-through-testenv-batch:
# testenv-batch.sh bind-mounts the worktree alone, and a worktree's .git file points at a
# gitdir path on the host that does not exist inside the container). Falling back to `find`
# there scans the same tree by content instead of by git's index — a wider list, never a
# narrower one, so nothing the git path would have caught goes unseen.
# ---------------------------------------------------------------------------------------
EXEMPT='^spira/testdata/sentinel-healthy\.log$|^docs/spikes/sources/sp-pmv67/data/.*\.tsv$|^spira/test-governor-deleted\.sh$'

offenders=0
if git -C "$ROOT" rev-parse --git-dir >/dev/null 2>&1; then
    files="$( { git -C "$ROOT" ls-files; git -C "$ROOT" ls-files --others --exclude-standard; } | sort -u )"
else
    files="$(cd "$ROOT" && find . \( -name .git -o -name target -o -name .runtime -o -name __pycache__ \) -prune -o -type f -print | sed 's#^\./##' | sort -u)"
fi

while IFS= read -r f; do
    [ -n "$f" ] || continue
    [[ "$f" =~ $EXEMPT ]] && continue
    [ -f "$ROOT/$f" ] || continue
    hit="$(grep -il "governor" "$ROOT/$f" 2>/dev/null || true)"
    [ -n "$hit" ] || continue
    offenders=$((offenders+1))
    bad "clean tree" "$f still names the governor"
done <<<"$files"

[ "$offenders" -eq 0 ] && ok "no tracked or new file names the governor"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
