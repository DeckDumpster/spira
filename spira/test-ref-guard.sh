#!/usr/bin/env bash
#
# test-ref-guard.sh — the reference-transaction hook refuses to delete a bead branch
# outside the chokepoint, and refuses nothing else. Also: worktree-hooks.sh install
# composes a worktree's pre-commit from the canonical fences (exclude.sh, spira-lint's
# scratch-fence rule, branch-guard.sh staged) followed by pre-commit-guard.sh, and each stage is seen to refuse,
# in that order, through the installed hook.
#
# WHAT THIS IS FOR. On 2026-09-18 an Ops dwarf deleted 24 peers' unlanded branches from
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
# CASES 10-14 close gap 3 (docs/test-plan/safety-fences.md): dwarf.sh swallows a failed
# `worktree-hooks.sh install` (`>/dev/null 2>&1 || true`), so a broken install leaves an
# dwarf worktree with no ref guard and no pre-commit fences, silently. Case 9 alone (the
# hook file exists and is executable) cannot tell a working composition from an empty
# shell script with the same permissions — these cases run real commits through it.
#
# Runs against a throwaway repository, never the harness: a suite that exercised branch
# deletion in a real checkout would be the very thing the hook exists to prevent.
#
# tier: T2
# covers: spira/hooks/reference-transaction spira/worktree-hooks.sh spira/hooks/pre-commit spira/hooks/commit-msg spira/pre-commit-guard.sh UC-safety-fences-20 UC-safety-fences-21 UC-safety-fences-35
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/testlib.sh"

HOOK="$HERE/hooks/reference-transaction"
[ -x "$HOOK" ] || bail "$HOOK missing or not executable"

TR="$(mktemp -d)" || exit 1
trap 'rm -rf "$TR"' EXIT
cd "$TR" || exit 1

# commit-msg asks spira-lc whether cited beads exist; this fixture has no store, so a stub
# answers: a message citing sp-phantom is refused (3), anything else passes.
unset SPIRA_WORK_BEAD_ID SPIRA_DWARF BEAD_ID SPIRA_REF_SANCTIONED
STUBBIN="$TR/stubbin"; mkdir -p "$STUBBIN"
cat > "$STUBBIN/spira-lc" <<'STUB'
#!/usr/bin/env bash
if grep -q 'sp-phantom'; then echo "commit-msg stub: sp-phantom is not a bead; reword" >&2; exit 3; fi
exit 0
STUB
chmod +x "$STUBBIN/spira-lc"; export PATH="$STUBBIN:$PATH"

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
#    writes too, and blocking those would stop every dwarf from working at all.
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

# 6a. LARGE STDIN. A single-line stdin fits the pipe buffer so the race is not
#     reliable. A stdin that overflows the buffer (>64 KB) causes SIGPIPE
#     deterministically if the hook exits without draining.
#     SEEN TO FAIL: change the hook's early-exit to a bare "exit 0".
(set -eo pipefail
 printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 0000000000000000000000000000000000000000 refs/heads/spira/sp-x\n%.0s' \
     {1..700}) | "$HOOK" committed >/dev/null 2>&1
is "hook drains large committed-phase stdin without SIGPIPE" "0" "$?"

# 7. THE CASE THE FIRST SIX COULD NOT SEE. Every case above runs in a repository with one
#    hooks directory, so they cannot tell "the hook is armed everywhere" from "the hook is
#    armed in the one place this test looks". dwarf.sh gives each worktree its OWN
#    core.hooksPath, and a per-worktree setting OVERRIDES the repo-level one — so the guard
#    was absent from every dwarf worktree while all six cases above passed. A worktree is
#    the only place the deletion this hook exists for has ever happened (sp-urifb,
#    law-guard-proved-where-the-offender-runs).
#
#    SEEN TO FAIL: with worktree-hooks.sh reduced to what dwarf.sh used to do — copying
#    pre-commit alone into the worktree hooks dir — this case deletes the branch and fails.
git worktree add -q "$TR/wt" -b spira/sp-wt 2>/dev/null
SPIRA_HOME="$HERE" worktree-hooks.sh install "$TR/wt" >/dev/null 2>&1

git -C "$TR/wt" branch spira/sp-wt-victim 2>/dev/null
git -C "$TR/wt" branch -D spira/sp-wt-victim >/dev/null 2>&1
is "unsanctioned delete from INSIDE a worktree is refused" \
   "yes" "$(git -C "$TR/wt" show-ref --verify -q refs/heads/spira/sp-wt-victim && echo yes || echo no)"

# 8. POSITIVE CONTROL for case 7, for the same reason case 2 exists: a worktree whose
#    hooks directory refused everything would pass case 7 and break every dwarf's own reap.
SPIRA_REF_SANCTIONED=1 git -C "$TR/wt" branch -D spira/sp-wt-victim >/dev/null 2>&1
is "sanctioned delete from inside a worktree succeeds" \
   "no" "$(git -C "$TR/wt" show-ref --verify -q refs/heads/spira/sp-wt-victim && echo yes || echo no)"

# 9. THE OTHER HOOK MUST SURVIVE THE COMPOSITION. dwarf.sh needs a per-worktree pre-commit
#    (this session's dirty-files guard) and used to install it by REPLACING the directory,
#    which dropped the canonical pre-commit's own fences too. Both must be present.
WGD="$(git -C "$TR/wt" rev-parse --path-format=absolute --git-dir)"
is "worktree still has a pre-commit hook" \
   "yes" "$([ -x "$WGD/hooks/pre-commit" ] && echo yes || echo no)"

# ---------------------------------------------------------------------------------------
# CASES 10-14 (gap 3). "Present and executable" (case 9) cannot distinguish a working
# composition from an empty shell script with the same permissions. These cases run real
# commits through the installed hook and read what refused, and in what order.
#
# The worktree needs the boundary/gate.sh/lib.sh signature exclude.sh's own scope
# detection requires (spira/exclude.sh), tracked within ITS OWN history — exclude.sh
# resolves scope from `git -C <worktree-toplevel> ls-files`, not from where the script
# itself lives, so without this exclude.sh silently declares "not a harness checkout" and
# never inspects anything staged.
# ---------------------------------------------------------------------------------------
cp "$HERE/boundary" "$HERE/gate.sh" "$HERE/lib.sh" "$TR/wt/"
echo 'fn main() {}' > "$TR/wt/seed.rs"
git -C "$TR/wt" add boundary gate.sh lib.sh seed.rs >/dev/null 2>&1
git -C "$TR/wt" commit -qm "test: seed the harness signature" >/dev/null 2>&1

commit_in_wt() {   # commit_in_wt <message> -> combined stdout+stderr; sets rc via $?
    git -C "$TR/wt" commit -m "$1" 2>&1
}

echo
echo "CASE 10: exclude.sh staged, through the installed worktree hook, refuses beads data:"
printf '{}\n' > "$TR/wt/export.jsonl"
git -C "$TR/wt" add export.jsonl >/dev/null 2>&1
out="$(commit_in_wt 'sp-test: stage beads export')"; rc=$?
is   "case 10: commit is refused" "1" "$rc"
want "case 10: refusal names exclude.sh" "exclude.sh" "$out"
want "case 10: refusal names the offending file" "export.jsonl" "$out"
git -C "$TR/wt" restore --staged export.jsonl >/dev/null 2>&1
rm -f "$TR/wt/export.jsonl"

echo
echo "CASE 11: spira-lint's scratch-fence rule, through the installed worktree hook, refuses a root-level sp-* file:"
printf 'notes\n' > "$TR/wt/sp-xxxx-notes.md"
git -C "$TR/wt" add sp-xxxx-notes.md >/dev/null 2>&1
out="$(commit_in_wt 'sp-test: stage scratch note')"; rc=$?
is   "case 11: commit is refused" "1" "$rc"
want "case 11: refusal names scratch-fence.sh" "scratch-fence" "$out"
want "case 11: refusal names the offending file" "sp-xxxx-notes.md" "$out"
git -C "$TR/wt" restore --staged sp-xxxx-notes.md >/dev/null 2>&1
rm -f "$TR/wt/sp-xxxx-notes.md"

echo
echo "CASE 12: branch-guard.sh staged, through the installed worktree hook, refuses a SPIRA_WORK mismatch:"
printf 'more\n' >> "$TR/wt/seed.txt"
git -C "$TR/wt" add seed.txt >/dev/null 2>&1
out="$(GIT_AUTHOR_NAME=dwarf-t GIT_AUTHOR_EMAIL=dwarf-t@spira.local \
       GIT_COMMITTER_NAME=dwarf-t GIT_COMMITTER_EMAIL=dwarf-t@spira.local \
       SPIRA_WORK="$TR/somewhere-else" \
       commit_in_wt 'sp-test: dwarf commits from the wrong assigned worktree')"; rc=$?
is   "case 12: commit is refused" "1" "$rc"
want "case 12: refusal names branch-guard.sh" "branch-guard.sh" "$out"
git -C "$TR/wt" restore --staged seed.txt >/dev/null 2>&1
git -C "$TR/wt" checkout -q -- seed.txt

echo
echo "CASE 13: the canonical fences run BEFORE pre-commit-guard.sh — an offender both would refuse is stopped by the earlier one, so pre-commit-guard.sh never runs:"
# Write a pre-session dirty snapshot naming the sp-* path (exactly as dwarf.sh does), so
# pre-commit-guard.sh WOULD ALSO refuse this path if it ever got to run.
printf 'sp-both-guards.md\n' > "$WGD/spira-dirty-before"
printf 'planted before the session\n' > "$TR/wt/sp-both-guards.md"
git -C "$TR/wt" add sp-both-guards.md >/dev/null 2>&1
out="$(commit_in_wt 'sp-test: offender for both scratch-fence and the dirty guard')"; rc=$?
is     "case 13: commit is refused" "1" "$rc"
want   "case 13: refusal is scratch-fence.sh's (the earlier stage)" "scratch-fence" "$out"
nowant "case 13: pre-commit-guard.sh's message never appears (it did not run)" \
       "staged paths were dirty" "$out"
git -C "$TR/wt" restore --staged sp-both-guards.md >/dev/null 2>&1
rm -f "$TR/wt/sp-both-guards.md" "$WGD/spira-dirty-before"

echo
echo "CASE 14: with the canonical fences silent, pre-commit-guard.sh still runs and refuses its own offender:"
printf 'own-file.txt\n' > "$WGD/spira-dirty-before"
printf 'planted before the session\n' > "$TR/wt/own-file.txt"
git -C "$TR/wt" add own-file.txt >/dev/null 2>&1
out="$(commit_in_wt 'sp-test: dirty pre-session file, no canonical-fence offence')"; rc=$?
is   "case 14: commit is refused" "1" "$rc"
want "case 14: refusal is pre-commit-guard.sh's" "staged paths were dirty" "$out"
want "case 14: refusal names the offending file" "own-file.txt" "$out"
git -C "$TR/wt" restore --staged own-file.txt >/dev/null 2>&1
rm -f "$TR/wt/own-file.txt" "$WGD/spira-dirty-before"

echo
echo "CASE 15 (positive control for 10-14): a clean, ordinary commit through the same installed hook still succeeds:"
printf 'ordinary\n' > "$TR/wt/ordinary.txt"
git -C "$TR/wt" add ordinary.txt >/dev/null 2>&1
out="$(commit_in_wt 'sp-test: ordinary commit')"; rc=$?
is "case 15: an ordinary commit is not refused" "0" "$rc"
printf 'phantom\n' > "$TR/wt/phantom.txt"
git -C "$TR/wt" add phantom.txt >/dev/null 2>&1
out="$(commit_in_wt 'sp-test: cites sp-phantom')"; rc=$?
is   "case 15: a commit citing a phantom bead is refused by commit-msg" "1" "$rc"
want "case 15: the refusal comes from the store's answer" "sp-phantom" "$out"
git -C "$TR/wt" restore --staged phantom.txt >/dev/null 2>&1
rm -f "$TR/wt/phantom.txt"

echo
echo "CASE 16: THE FIX. git pack-refs --all --prune (what git gc runs) completes on a loose bead branch, and the branch still resolves to the same commit afterward — packing is a representation change, not a deletion:"
git branch spira/sp-packme
before="$(git rev-parse spira/sp-packme)"
git pack-refs --all --prune >/dev/null 2>&1
rc=$?
is "case 16: pack-refs --all --prune exits 0" "0" "$rc"
is "case 16: the branch still resolves after packing" "yes" "$(alive spira/sp-packme)"
is "case 16: the branch still points at the same commit" "$before" "$(git rev-parse spira/sp-packme 2>/dev/null)"

echo
echo "CASE 17: NO OVERREACH IN THE OTHER DIRECTION. A now-packed bead branch is still refused an unsanctioned plain delete — the fix must not widen the hole a git-branch--D sweep walks through:"
echo "SEEN TO FAIL before this fix: case 16 above (pack-refs --all --prune exits 1 and aborts)."
git branch -D spira/sp-packme >/dev/null 2>&1
is "case 17: unsanctioned delete of a now-packed bead branch is still refused" "yes" "$(alive spira/sp-packme)"

echo
echo "CASE 18: a worktree armed under release A still commits after A is deleted and B is current:"
REL="$TR/rels"
mkdir -p "$REL/aaaa" "$REL/bbbb"
cp -r "$HERE/." "$REL/aaaa/spira/" 2>/dev/null || { mkdir -p "$REL/aaaa/spira"; cp -r "$HERE/." "$REL/aaaa/spira/"; }
mkdir -p "$REL/bbbb/spira" && cp -r "$HERE/." "$REL/bbbb/spira/"
ln -s aaaa "$REL/current"
git worktree add -q "$TR/wt18" -b spira/sp-wt18 2>/dev/null
cp "$HERE/boundary" "$HERE/gate.sh" "$HERE/lib.sh" "$TR/wt18/"
echo 'fn main() {}' > "$TR/wt18/seed.rs"
SPIRA_HOME="$REL/aaaa/spira" worktree-hooks.sh install "$TR/wt18" >/dev/null 2>&1
rm -rf "$REL/aaaa"
ln -sfn bbbb "$REL/current"
echo x > "$TR/wt18/f18"
git -C "$TR/wt18" add f18 seed.rs >/dev/null 2>&1
git -C "$TR/wt18" commit -qm "sp-test: after release A is gone" >/dev/null 2>&1
is "case 18: commit succeeds after the release it was armed under is deleted" "0" "$?"

# ---------------------------------------------------------------------------------------
# CASES 19-25: a dwarf (SPIRA_DWARF set) may not put the landing ref under HEAD or move it.
# The fixture's landing ref is the LOCAL branch local/main, registered through the real
# spira-config, so the resolution the hook performs is what runs. The main checkout is armed
# by exclude.sh install, the worktree by worktree-hooks.sh install.
# ---------------------------------------------------------------------------------------
LR="$TR/landrepo"
git init -q "$LR" && git -C "$LR" config user.email guard-suite@example.invalid \
    && git -C "$LR" config user.name "ref guard suite"
mkdir -p "$LR/hooks"
cp "$HERE/boundary" "$HERE/gate.sh" "$HERE/lib.sh" "$LR/"
cp "$HERE/hooks/pre-commit" "$LR/hooks/pre-commit"
printf '#!/usr/bin/env bash\nexec "%s" "$@"\n' "$HOOK" > "$LR/hooks/reference-transaction"
chmod +x "$LR/hooks/reference-transaction" "$LR/hooks/pre-commit"
echo seed > "$LR/seed.txt"
git -C "$LR" add -A >/dev/null 2>&1
git -C "$LR" commit -qm seed >/dev/null 2>&1
git -C "$LR" branch local/main
git -C "$LR" commit -q --allow-empty -m "second" >/dev/null 2>&1
LR_OLD="$(git -C "$LR" rev-parse local/main)"
LR_NEW="$(git -C "$LR" rev-parse HEAD)"
printf '%s | %s | local | local/main | |\n' "landfix" "$LR" > "$TR/landmap"
tl_config SPIRA_REPO_MAP="$TR/landmap" SPIRA_HOME_REPO="landfix"
exclude.sh install "$LR" >/dev/null 2>&1
git -C "$LR" worktree add -q "$TR/landwt" -b spira/sp-mine 2>/dev/null
SPIRA_HOME="$HERE" worktree-hooks.sh install "$TR/landwt" >/dev/null 2>&1
landref_at() { git -C "$LR" rev-parse refs/heads/local/main; }
export SPIRA_DWARF=fixture BEAD_ID=sp-mine

# git before 2.46 does not run the prepared phase for a HEAD symref move (checkout, switch),
# so the hook cannot see a checkout there; the suite asserts it where git reports it.
PROBE="$TR/headprobe"; git init -q "$PROBE"; mkdir "$PROBE/h"
printf '#!/usr/bin/env bash\n[ "$1" = prepared ] && cat | grep -q " HEAD$" && touch "%s/saw-head"\nexit 0\n' "$TR" > "$PROBE/h/reference-transaction"
chmod +x "$PROBE/h/reference-transaction"
git -C "$PROBE" config core.hooksPath h
git -C "$PROBE" config user.email p@example.invalid; git -C "$PROBE" config user.name p
git -C "$PROBE" commit -q --allow-empty -m p >/dev/null 2>&1
git -C "$PROBE" branch probe-b
rm -f "$TR/saw-head"
git -C "$PROBE" checkout -q probe-b >/dev/null 2>&1
if [ -e "$TR/saw-head" ]; then HEAD_SEEN=1; else HEAD_SEEN=0; echo "note: $(git --version) reports no HEAD update to the hook; the checkout cases 19 and 21 are not asserted"; fi

echo
echo "CASE 19: a dwarf's checkout of the landing ref is refused, and HEAD stays on its branch:"
if [ "$HEAD_SEEN" = 1 ]; then
out="$(git -C "$TR/landwt" checkout local/main 2>&1)"; rc=$?
is   "case 19: checkout is refused" "1" "$([ $rc -ne 0 ] && echo 1 || echo 0)"
is   "case 19: HEAD is still the dwarf's branch" "spira/sp-mine" "$(git -C "$TR/landwt" symbolic-ref --short HEAD)"
want "case 19: refusal names the landing ref" "local/main" "$out"
want "case 19: refusal names the exit" "git merge local/main" "$out"
want "case 19: refusal names SPIRA_DWARF" "SPIRA_DWARF" "$out"
want "case 19: refusal names the bead branch" "git switch spira/sp-mine" "$out"
nowant "case 19: refusal does not advertise the override" "SPIRA_REF_SANCTIONED" "$out"
fi

echo
echo "CASE 20: update-ref and branch -f on the landing ref are refused:"
git -C "$TR/landwt" update-ref refs/heads/local/main "$LR_NEW" "$LR_OLD" >/dev/null 2>&1
is "case 20: update-ref leaves local/main" "$LR_OLD" "$(landref_at)"
git -C "$TR/landwt" branch -f local/main HEAD >/dev/null 2>&1
is "case 20: branch -f leaves local/main" "$LR_OLD" "$(landref_at)"

echo
echo "CASE 21: a git -C aimed at the armed main checkout cannot check out and commit on the landing ref:"
out="$(git -C "$LR" checkout local/main 2>&1)"; rc=$?
if [ "$HEAD_SEEN" = 1 ]; then
    is   "case 21: the checkout in the main checkout is refused" "1" "$([ $rc -ne 0 ] && echo 1 || echo 0)"
    want "case 21: refusal names the exit" "git merge local/main" "$out"
    is   "case 21: the main checkout is still on its own branch" "main" "$(git -C "$LR" symbolic-ref --short HEAD)"
fi
git -C "$LR" commit -q --allow-empty -m x >/dev/null 2>&1
git -C "$LR" checkout -q main >/dev/null 2>&1
is "case 21: local/main is unchanged" "$LR_OLD" "$(landref_at)"

echo
echo "CASE 22: positive controls — a dwarf's own work still passes:"
echo "fn main() {}" > "$TR/landwt/mine.rs"
git -C "$TR/landwt" add mine.rs >/dev/null 2>&1
git -C "$TR/landwt" commit -qm "sp-test: own work" >/dev/null 2>&1
is "case 22: commit on the bead branch succeeds" "0" "$?"
git -C "$TR/landwt" merge -q local/main >/dev/null 2>&1
is "case 22: merging the landing ref into the bead branch succeeds" "0" "$?"
timeout 5 git -C "$TR/landwt" fetch -q "$LR" >/dev/null 2>&1
is "case 22: fetch succeeds" "0" "$?"

echo
echo "CASE 23: without SPIRA_DWARF an operator may move the landing ref:"
env -u SPIRA_DWARF git -C "$LR" update-ref refs/heads/local/main "$LR_NEW" "$LR_OLD" >/dev/null 2>&1
is "case 23: operator update-ref succeeds" "$LR_NEW" "$(landref_at)"
env -u SPIRA_DWARF git -C "$LR" update-ref refs/heads/local/main "$LR_OLD" "$LR_NEW" >/dev/null 2>&1

echo
echo "CASE 24: the sanctioned writer moves it even with SPIRA_DWARF set (what RealGit::update_ref runs):"
SPIRA_REF_SANCTIONED=1 git -C "$LR" update-ref refs/heads/local/main "$LR_NEW" "$LR_OLD" >/dev/null 2>&1
is "case 24: sanctioned update-ref succeeds" "$LR_NEW" "$(landref_at)"
SPIRA_REF_SANCTIONED=1 git -C "$LR" update-ref refs/heads/local/main "$LR_OLD" "$LR_NEW" >/dev/null 2>&1

echo
echo "CASE 25: an unresolvable landing ref fails closed for a dwarf, naming the root:"
printf '%s | %s | local | gone/nowhere | |\n' "landfix" "$LR" > "$TR/landmap"
out="$(git -C "$TR/landwt" update-ref refs/heads/local/main "$LR_NEW" "$LR_OLD" 2>&1)"; rc=$?
is   "case 25: the write is refused" "1" "$([ $rc -ne 0 ] && echo 1 || echo 0)"
want "case 25: refusal names the root" "$LR" "$out"
want "case 25: refusal names the failing command" "spira-config repo landref" "$out"
unset SPIRA_DWARF BEAD_ID


tl_summary
