#!/usr/bin/env bash
#
# test-ref-guard.sh — the reference-transaction hook refuses to delete a bead branch
# outside the chokepoint, and refuses nothing else.
#
# WHAT THIS IS FOR. On 2026-09-18 an Ops aeon deleted 24 peers' unlanded branches from
# inside its own worktree, because a worktree shares one ref namespace with every other
# worktree and lib.sh's "nothing outside this section may call git branch -D" was prose
# rather than a program (sp-q27cp). The hook is the program. This suite is what keeps it
# one.
#
# THE POSITIVE CONTROL IS THE POINT. A hook that refuses EVERYTHING passes any test that
# only checks the attack is blocked, and would break every reap and every batch in the
# fleet. Cases 2, 4 and 5 are what tell the two apart
# (law-absence-needs-a-positive-control).
#
# Runs against a throwaway repository, never the harness: a suite that exercised branch
# deletion in a real checkout would be the very thing the hook exists to prevent.

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

pass=0; fail=0
ok()   { printf '  ok    %s\n' "$1"; pass=$((pass+1)); }
bad()  { printf '  FAIL  %s\n' "$1"; fail=$((fail+1)); }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1: wanted [$2] got [$3]"; }

HOOK="$HERE/hooks/reference-transaction"
[ -x "$HOOK" ] || { printf 'test-ref-guard: %s missing or not executable\n' "$HOOK" >&2; exit 1; }

TR="$(mktemp -d)" || exit 1
trap 'rm -rf "$TR"' EXIT
cd "$TR" || exit 1

git init -q repo && cd repo || exit 1
git config user.email guard-suite@example.invalid
git config user.name  "ref guard suite"
mkdir -p hooks && cp "$HOOK" hooks/ && chmod +x hooks/reference-transaction
echo seed > seed.txt
git add seed.txt >/dev/null 2>&1
git commit -qm "seed" >/dev/null 2>&1
git config core.hooksPath hooks

git branch spira/sp-victim
git branch spira/queue/20260918T000000Z
git branch feature/unrelated

alive() { git show-ref --verify -q "refs/heads/$1" && echo yes || echo no; }

# 1. THE ATTACK. An unsanctioned delete of a bead branch must fail AND leave the ref.
#    Both halves matter: a hook that printed a refusal and let the delete through would
#    pass on exit status alone.
git branch -D spira/sp-victim >/dev/null 2>&1
is "unsanctioned delete of a bead branch is refused" "yes" "$(alive spira/sp-victim)"

# 2. POSITIVE CONTROL. The sanctioned path must still delete, or every reap breaks.
SPIRA_REF_SANCTIONED=1 git branch -D spira/sp-victim >/dev/null 2>&1
is "sanctioned delete succeeds" "no" "$(alive spira/sp-victim)"

# 3. Queue branches live under the same namespace and carry batch work; same rule.
git branch -D spira/queue/20260918T000000Z >/dev/null 2>&1
is "unsanctioned delete of a queue branch is refused" "yes" "$(alive spira/queue/20260918T000000Z)"

# 4. NO OVERREACH. A ref outside refs/heads/spira/ is none of the hook's business.
git branch -D feature/unrelated >/dev/null 2>&1
is "delete outside the spira namespace is untouched" "no" "$(alive feature/unrelated)"

# 5. NO OVERREACH, the common path. Creating a branch and committing on it are ref
#    writes too, and blocking those would stop every aeon from working at all.
git checkout -q -b spira/sp-mine 2>/dev/null
echo work > work.txt
git add work.txt >/dev/null 2>&1
git commit -qm "ordinary work" >/dev/null 2>&1
is "branch creation and commit are untouched" "yes" "$(alive spira/sp-mine)"

# 6. THE HOOK MUST ONLY ABORT IN THE PHASE THAT CAN ABORT. git ignores a non-zero exit
#    from "committed"/"aborted", so a hook that refused there would report success while
#    permitting the deletion — the most expensive kind of broken guard.
printf '%s %s %s\n' "$(git rev-parse HEAD)" "0000000000000000000000000000000000000000" "refs/heads/spira/sp-x" \
    | "$HOOK" committed >/dev/null 2>&1
is "hook exits 0 in the committed phase" "0" "$?"

# 7. THE CASE THE FIRST SIX COULD NOT SEE. Every case above runs in a repository with one
#    hooks directory, so they cannot tell "the hook is armed everywhere" from "the hook is
#    armed in the one place this test looks". aeon.sh gives each worktree its OWN
#    core.hooksPath, and a per-worktree setting OVERRIDES the repo-level one — so the guard
#    was absent from every aeon worktree while all six cases above passed. A worktree is
#    the only place the deletion this hook exists for has ever happened (sp-urifb,
#    law-guard-proved-where-the-offender-runs).
#
#    SEEN TO FAIL: with worktree-hooks.sh reduced to what aeon.sh used to do — copying
#    pre-commit alone into the worktree hooks dir — this case deletes the branch and fails.
git worktree add -q "$TR/wt" -b spira/sp-wt 2>/dev/null
SPIRA_HOME="$HERE" bash "$HERE/worktree-hooks.sh" install "$TR/wt" >/dev/null 2>&1

git -C "$TR/wt" branch spira/sp-wt-victim 2>/dev/null
git -C "$TR/wt" branch -D spira/sp-wt-victim >/dev/null 2>&1
is "unsanctioned delete from INSIDE a worktree is refused" \
   "yes" "$(git -C "$TR/wt" show-ref --verify -q refs/heads/spira/sp-wt-victim && echo yes || echo no)"

# 8. POSITIVE CONTROL for case 7, for the same reason case 2 exists: a worktree whose
#    hooks directory refused everything would pass case 7 and break every aeon's own reap.
SPIRA_REF_SANCTIONED=1 git -C "$TR/wt" branch -D spira/sp-wt-victim >/dev/null 2>&1
is "sanctioned delete from inside a worktree succeeds" \
   "no" "$(git -C "$TR/wt" show-ref --verify -q refs/heads/spira/sp-wt-victim && echo yes || echo no)"

# 9. THE OTHER HOOK MUST SURVIVE THE COMPOSITION. aeon.sh needs a per-worktree pre-commit
#    (this session's dirty-files guard) and used to install it by REPLACING the directory,
#    which dropped the canonical pre-commit's own fences too. Both must be present.
is "worktree still has a pre-commit hook" \
   "yes" "$([ -x "$(git -C "$TR/wt" rev-parse --path-format=absolute --git-dir)/hooks/pre-commit" ] && echo yes || echo no)"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
