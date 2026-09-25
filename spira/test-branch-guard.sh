#!/usr/bin/env bash
#
# test-branch-guard.sh — branch-guard.sh refuses aeon commits to the base branch, and aeon
# commits outside SPIRA_WORK; it does not refuse operator commits, aeon commits on
# non-base branches, or aeon commits inside their own worktree. Also exercises the real,
# tracked hooks/pre-commit (exclude.sh + scratch-fence.sh + branch-guard.sh staged)
# end-to-end through a clone armed by `exclude.sh install`.
#
# Positive controls throughout (the guard MUST fire on the bad cases), negative controls
# (the guard MUST NOT fire on the good cases), and two check-mode cases (the audit detects
# an aeon tip and a checkout ahead of remote, then clears on both counts after cleanup).
#
# Absorbs test-aeon-worktree-guard.sh (D3, docs/test-plan/safety-fences.md): both suites
# drove the identical `branch-guard.sh staged` identity rules against their own tmp git
# fixture, so they are one suite now.
#
# defect: sp-poou
# tier: T2
# covers: spira/branch-guard.sh spira/hooks/pre-commit spira/aeon.sh UC-safety-fences-18 UC-safety-fences-19 UC-safety-fences-22
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# Minimal harness copy the guard resolves relative to its own location.
SH="$TMP/spira"
mkdir -p "$SH/hooks"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/branch-guard.sh" "$SH/"
cp "$HERE/hooks/pre-commit" "$SH/hooks/"

# A bare remote plus a working checkout with main as its base branch.
REMOTE="$TMP/remote.git"
REPO="$TMP/repo"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
printf 'initial content\n' > "$REPO/f.txt"
GIT_AUTHOR_NAME=op GIT_AUTHOR_EMAIL="op@example.com" \
GIT_COMMITTER_NAME=op GIT_COMMITTER_EMAIL="op@example.com" \
git -C "$REPO" add -A
GIT_AUTHOR_NAME=op GIT_AUTHOR_EMAIL="op@example.com" \
GIT_COMMITTER_NAME=op GIT_COMMITTER_EMAIL="op@example.com" \
git -C "$REPO" commit -q -m "initial"
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
# Cache origin/HEAD so spira_landref finds the base without a network call.
git -C "$REPO" symbolic-ref refs/remotes/origin/HEAD refs/remotes/origin/main

GIT_BIN="$(dirname "$(command -v git)")"
mkdir -p "$TMP/run"

# run_guard <email> <repo> -> exit code of branch-guard.sh staged
run_guard() {
    local email="$1" root="$2"
    # RUN IN AN EXPLICIT MINIMAL ENVIRONMENT. Ambient conf is the thing that silently decides
    # verdicts in a suite that inherits it. SPIRA_CONF points at a nonexistent file so no
    # config file is read; SPIRA_REPO_MAP likewise so no map is consulted.
    env -i HOME="$TMP" PATH="$GIT_BIN:/usr/bin:/bin" \
        GIT_COMMITTER_NAME="test" GIT_COMMITTER_EMAIL="$email" \
        SPIRA_CONF="$TMP/none.conf" SPIRA_REPO="$root" \
        SPIRA_REPO_MAP="$TMP/none.map" SPIRA_DB="$TMP/none.db" \
        SPIRA_RUN="$TMP/run" \
        bash "$SH/branch-guard.sh" staged "$root" >/dev/null 2>&1
}

echo "test-branch-guard.sh — staged: refuse aeon on base branch; pass otherwise"

# Stage a change so git commit would have something to commit.
printf 'line\n' >> "$REPO/f.txt"
git -C "$REPO" add -A

# ---------------------------------------------------------------------------------------
# POSITIVE CONTROL — the guard MUST fire when it should. "A check that finds nothing must
# first prove it could have found something" (law-absence-needs-a-positive-control).
# Plant the bad case and require the guard to say so, then believe it when it is silent.
# ---------------------------------------------------------------------------------------
out="$(env -i HOME="$TMP" PATH="$GIT_BIN:/usr/bin:/bin" \
    GIT_COMMITTER_NAME="aeon-shiva" GIT_COMMITTER_EMAIL="aeon-shiva@spira.local" \
    SPIRA_CONF="$TMP/none.conf" SPIRA_REPO="$REPO" \
    SPIRA_REPO_MAP="$TMP/none.map" SPIRA_DB="$TMP/none.db" \
    SPIRA_RUN="$TMP/run" \
    bash "$SH/branch-guard.sh" staged "$REPO" 2>&1)"; guard_rc=$?
is   "aeon on base branch: guard exits 1" 1 "$guard_rc"
want "aeon on base branch: names the committer email" "aeon-shiva@spira.local" "$out"
want "aeon on base branch: names the branch" "main" "$out"
want "aeon on base branch: names the override" "--no-verify" "$out"

# ---------------------------------------------------------------------------------------
# NEGATIVE CONTROL 1 — operator identity on base branch must be allowed through.
# ---------------------------------------------------------------------------------------
rc=0; run_guard "op@example.com" "$REPO" || rc=$?
is "operator on base branch: guard exits 0" 0 "$rc"

# ---------------------------------------------------------------------------------------
# NEGATIVE CONTROL 2 — aeon identity on a non-base branch must be allowed through.
# ---------------------------------------------------------------------------------------
git -C "$REPO" checkout -q -b "spira/sp-test" 2>/dev/null
rc=0; run_guard "aeon-shiva@spira.local" "$REPO" || rc=$?
is "aeon on non-base branch: guard exits 0" 0 "$rc"
git -C "$REPO" checkout -q main 2>/dev/null

echo ""
echo "test-branch-guard.sh — check: detect aeon tip and checkout ahead of remote"

# ---------------------------------------------------------------------------------------
# SET UP: plant an aeon commit directly on main (reproducing the defect), WITHOUT pushing.
# This exercises both anomalies: (1) aeon commit at the base branch tip, and (2) shared
# checkout is ahead of its remote.
# ---------------------------------------------------------------------------------------
printf 'aeon direct commit\n' >> "$REPO/f.txt"
GIT_AUTHOR_NAME="aeon-shiva" GIT_AUTHOR_EMAIL="aeon-shiva@spira.local" \
GIT_COMMITTER_NAME="aeon-shiva" GIT_COMMITTER_EMAIL="aeon-shiva@spira.local" \
git -C "$REPO" add -A
GIT_AUTHOR_NAME="aeon-shiva" GIT_AUTHOR_EMAIL="aeon-shiva@spira.local" \
GIT_COMMITTER_NAME="aeon-shiva" GIT_COMMITTER_EMAIL="aeon-shiva@spira.local" \
git -C "$REPO" commit -q -m "sp-test: aeon commit planted directly on main"

# The check is run with SPIRA_REPO=$REPO so spira_repos returns the home repo name and
# repo_root resolves to $REPO. SPIRA_REPO_MAP is absent so the map contributes nothing.
run_check() {
    env -i HOME="$TMP" PATH="$GIT_BIN:/usr/bin:/bin" \
        SPIRA_CONF="$TMP/none.conf" SPIRA_REPO="$REPO" \
        SPIRA_REPO_MAP="$TMP/none.map" SPIRA_DB="$TMP/none.db" \
        SPIRA_RUN="$TMP/run" \
        bash "$SH/branch-guard.sh" check 2>&1
}

check_out="$(run_check)"; check_rc=$?
is   "check: exits 1 when aeon tip and ahead-of-remote" 1 "$check_rc"
want "check: reports AEON COMMIT ON" "AEON COMMIT ON" "$check_out"
want "check: reports SHARED CHECKOUT AHEAD" "SHARED CHECKOUT AHEAD" "$check_out"

# ---------------------------------------------------------------------------------------
# After pushing, the checkout is no longer ahead. The aeon commit is still the tip on
# both sides, so AEON COMMIT is still reported but SHARED CHECKOUT AHEAD is not.
# ---------------------------------------------------------------------------------------
git -C "$REPO" push -q origin main

check_out="$(run_check)"; check_rc=$?
is   "check: still exits 1 (aeon is still the tip)" 1 "$check_rc"
want "check: still reports AEON COMMIT ON" "AEON COMMIT ON" "$check_out"
nowant "check: SHARED CHECKOUT AHEAD gone after push" "SHARED CHECKOUT AHEAD" "$check_out"

# ---------------------------------------------------------------------------------------
# After an operator commit on top, the aeon is no longer the tip. Check should be clean.
# ---------------------------------------------------------------------------------------
printf 'operator restores order\n' >> "$REPO/f.txt"
GIT_AUTHOR_NAME=op GIT_AUTHOR_EMAIL="op@example.com" \
GIT_COMMITTER_NAME=op GIT_COMMITTER_EMAIL="op@example.com" \
git -C "$REPO" add -A
GIT_AUTHOR_NAME=op GIT_AUTHOR_EMAIL="op@example.com" \
GIT_COMMITTER_NAME=op GIT_COMMITTER_EMAIL="op@example.com" \
git -C "$REPO" commit -q -m "chore: operator commit restoring a clean tip"
git -C "$REPO" push -q origin main

check_out="$(run_check)"; check_rc=$?
is   "check: exits 0 after operator commit" 0 "$check_rc"
want "check: reports clean" "clean" "$check_out"

echo ""
echo "test-branch-guard.sh — worktree: aeon commits refused outside SPIRA_WORK (absorbed from test-aeon-worktree-guard.sh)"

# ---------------------------------------------------------------------------------------
# FOUR CASES (law-absence-needs-a-positive-control):
#   1. Aeon commits in SPIRA_WORK (the assigned worktree) → allowed.
#   2. Aeon commits in REPO (the production checkout) with SPIRA_WORK set → REFUSED.
#   3. Aeon commits in REPO without SPIRA_WORK (no assigned worktree, e.g. a sweep) → allowed.
#   4. Operator commits in REPO with SPIRA_WORK set → allowed (not an aeon identity).
#
# Case 2 is the positive control for the guard: if it does not fire on that case, the test
# proves nothing (law-absence-needs-a-positive-control).
#
# Driven against branch-guard.sh directly (via a pre-commit hook), without a full aeon.sh
# invocation. The hook is what enforces the rule; testing via aeon.sh would test that the
# harness invoked a model, not that the guard fires.
# ---------------------------------------------------------------------------------------

# Enable per-worktree config (prerequisite for per-worktree hooks in the worktree).
git -C "$REPO" config extensions.worktreeConfig true

# Create a worktree at a path that matches the sanctioned root pattern.
WORK="$TMP/run/worktree/sp-test"
mkdir -p "$(dirname "$WORK")"
git -C "$REPO" worktree add -q -b spira/sp-wt-test "$WORK" origin/main

# Install the hook in REPO: points core.hooksPath at a directory containing a pre-commit
# script that calls branch-guard.sh staged. This mirrors what exclude.sh install does in
# production.
WT_HOOK_DIR="$TMP/wt-hooks"
mkdir -p "$WT_HOOK_DIR"
cat > "$WT_HOOK_DIR/pre-commit" << HOOK
#!/usr/bin/env bash
exec bash "$SH/branch-guard.sh" staged
HOOK
chmod +x "$WT_HOOK_DIR/pre-commit"
git -C "$REPO" config core.hooksPath "$WT_HOOK_DIR"
# The worktree also needs the hook. Use the same hook dir — the guard is the same.
git -C "$WORK" config --worktree core.hooksPath "$WT_HOOK_DIR"

git -C "$REPO" checkout -q main

echo "CASE 1: aeon commits in the assigned worktree (SPIRA_WORK == current tree) — ALLOWED:"
# The guard must NOT fire when the commit happens in the worktree it was assigned.
printf 'aeon work v1\n' > "$WORK/g"
git -C "$WORK" add g
GIT_AUTHOR_NAME="aeon-t" GIT_AUTHOR_EMAIL="aeon-t@spira.local" \
GIT_COMMITTER_NAME="aeon-t" GIT_COMMITTER_EMAIL="aeon-t@spira.local" \
SPIRA_WORK="$WORK" \
git -C "$WORK" commit -m "sp-test: work in worktree" >/dev/null 2>&1; rc=$?
is "case 1: commit in worktree is allowed" "0" "$rc"

echo "CASE 2 (positive control): aeon commits in REPO (production checkout) with SPIRA_WORK set — REFUSED:"
# This is the exact defect: committing in the production checkout while the worktree path
# is set to something else. The guard must fire and name both paths.
printf 'production change\n' >> "$REPO/f.txt"
git -C "$REPO" add f.txt
out="$(GIT_AUTHOR_NAME="aeon-t" GIT_AUTHOR_EMAIL="aeon-t@spira.local" \
       GIT_COMMITTER_NAME="aeon-t" GIT_COMMITTER_EMAIL="aeon-t@spira.local" \
       SPIRA_WORK="$WORK" \
       git -C "$REPO" commit -m "sp-test: wrong checkout" 2>&1)"; rc=$?
is "case 2: commit in production checkout is refused" "1" "$rc"
want "case 2: refusal names the production checkout path" "$REPO" "$out"
want "case 2: refusal names SPIRA_WORK" "$WORK" "$out"
want "case 2: refusal message identifies branch-guard.sh" "branch-guard.sh" "$out"
# Unstage so later cases start clean.
git -C "$REPO" restore --staged f.txt 2>/dev/null || git -C "$REPO" reset -q HEAD f.txt 2>/dev/null || true

echo "CASE 3: aeon commits in REPO without SPIRA_WORK set (no assigned worktree) — ALLOWED:"
# A sweep or other beadless session has no SPIRA_WORK. The guard must not fire.
# Use a feature branch (not main) so the existing base-branch check does not also fire —
# the case under test is specifically the SPIRA_WORK check, not the base-branch check.
git -C "$REPO" checkout -q -b spira/sp-sweep 2>/dev/null || git -C "$REPO" checkout -q spira/sp-sweep
printf 'unassigned aeon change\n' >> "$REPO/f.txt"
git -C "$REPO" add f.txt
GIT_AUTHOR_NAME="aeon-t" GIT_AUTHOR_EMAIL="aeon-t@spira.local" \
GIT_COMMITTER_NAME="aeon-t" GIT_COMMITTER_EMAIL="aeon-t@spira.local" \
git -C "$REPO" commit -m "sp-test: sweep commit" >/dev/null 2>&1; rc=$?
is "case 3: commit with no SPIRA_WORK is allowed" "0" "$rc"

echo "CASE 4: operator commits in REPO with SPIRA_WORK set — ALLOWED (not an aeon identity):"
# The guard must not affect operator commits, which have a normal email address.
printf 'operator change\n' >> "$REPO/f.txt"
git -C "$REPO" add f.txt
GIT_AUTHOR_NAME="op" GIT_AUTHOR_EMAIL="op@example.com" \
GIT_COMMITTER_NAME="op" GIT_COMMITTER_EMAIL="op@example.com" \
SPIRA_WORK="$WORK" \
git -C "$REPO" commit -m "operator direct commit" >/dev/null 2>&1; rc=$?
is "case 4: operator commit in production checkout is allowed" "0" "$rc"

# Restore REPO's hooksPath for the next section (undo the worktree-section override).
git -C "$REPO" config --unset core.hooksPath 2>/dev/null || true

echo ""
echo "test-branch-guard.sh — hooks/pre-commit: the tracked hook runs exclude.sh, scratch-fence.sh and branch-guard.sh staged, each seen to refuse through the REAL hook (UC-safety-fences-22, gap 5)"

# ---------------------------------------------------------------------------------------
# A clone that LOOKS like the harness (carries the boundary/gate.sh/lib.sh signature
# exclude.sh's own scope detection requires) so `exclude.sh install` arms the real,
# tracked hooks/pre-commit rather than a copy of it — the gap this row closes: test-
# branch-guard used to copy hooks/pre-commit into a fixture and never run it, and test-
# scratch-fence only grepped it for the string "scratch-fence".
# ---------------------------------------------------------------------------------------
HREPO="$TMP/hrepo"
mkdir -p "$HREPO/hooks"
git init -q -b main "$HREPO"
cp "$HERE/boundary" "$HERE/gate.sh" "$HERE/lib.sh" "$HERE/conf.sh" \
   "$HERE/exclude.sh" "$HERE/scratch-fence.sh" "$HERE/branch-guard.sh" "$HREPO/"
cp "$HERE/hooks/pre-commit" "$HREPO/hooks/pre-commit"
chmod +x "$HREPO/exclude.sh" "$HREPO/scratch-fence.sh" "$HREPO/branch-guard.sh" "$HREPO/hooks/pre-commit"
GIT_AUTHOR_NAME=op GIT_AUTHOR_EMAIL="op@example.com" \
GIT_COMMITTER_NAME=op GIT_COMMITTER_EMAIL="op@example.com" \
git -C "$HREPO" add -A
GIT_AUTHOR_NAME=op GIT_AUTHOR_EMAIL="op@example.com" \
GIT_COMMITTER_NAME=op GIT_COMMITTER_EMAIL="op@example.com" \
git -C "$HREPO" commit -q -m "seed: harness-shaped clone"
# Cache origin/HEAD so branch-guard.sh's spira_landref resolves without a network remote.
HREMOTE="$TMP/hrepo-remote.git"
git init -q --bare -b main "$HREMOTE"
git -C "$HREPO" remote add origin "$HREMOTE"
git -C "$HREPO" push -q origin main
git -C "$HREPO" symbolic-ref refs/remotes/origin/HEAD refs/remotes/origin/main

install_out="$(bash "$HREPO/exclude.sh" install "$HREPO" 2>&1)"; install_rc=$?
is   "exclude.sh install: succeeds on a harness-shaped clone" "0" "$install_rc"
want "exclude.sh install: points core.hooksPath at hooks" "hooks" "$install_out"
is   "exclude.sh install: core.hooksPath is set to hooks" "hooks" \
     "$(git -C "$HREPO" config core.hooksPath)"

commit_through_hook() {   # commit_through_hook <email> <name> <message> -> stdout+stderr, sets rc
    GIT_AUTHOR_NAME="$2" GIT_AUTHOR_EMAIL="$1" \
    GIT_COMMITTER_NAME="$2" GIT_COMMITTER_EMAIL="$1" \
    git -C "$HREPO" commit -m "$3" 2>&1
}

echo "SEEN RED: exclude.sh, through the real hook, refuses beads data:"
printf '{}\n' > "$HREPO/export.jsonl"
git -C "$HREPO" add export.jsonl
out="$(commit_through_hook op@example.com op "sp-test: stage beads export")"; rc=$?
is   "hook run: exclude.sh refuses a staged .jsonl export" "1" "$rc"
want "hook run: refusal names exclude.sh" "exclude.sh" "$out"
want "hook run: refusal names the offending file" "export.jsonl" "$out"
git -C "$HREPO" restore --staged export.jsonl 2>/dev/null; rm -f "$HREPO/export.jsonl"

echo "SEEN RED: scratch-fence.sh, through the real hook, refuses a root-level sp-* file:"
printf 'notes\n' > "$HREPO/sp-xxxx-notes.md"
git -C "$HREPO" add sp-xxxx-notes.md
out="$(commit_through_hook op@example.com op "sp-test: stage scratch note")"; rc=$?
is   "hook run: scratch-fence.sh refuses a root-level sp-* file" "1" "$rc"
want "hook run: refusal names scratch-fence.sh" "scratch-fence" "$out"
want "hook run: refusal names the offending file" "sp-xxxx-notes.md" "$out"
git -C "$HREPO" restore --staged sp-xxxx-notes.md 2>/dev/null; rm -f "$HREPO/sp-xxxx-notes.md"

echo "SEEN RED: branch-guard.sh, through the real hook, refuses an aeon-identity commit on the base branch:"
printf 'more\n' >> "$HREPO/lib.sh"
git -C "$HREPO" add lib.sh
out="$(commit_through_hook aeon-shiva@spira.local aeon-shiva "sp-test: aeon commits directly to main")"; rc=$?
is   "hook run: branch-guard.sh refuses an aeon identity on the base branch" "1" "$rc"
want "hook run: refusal names branch-guard.sh" "branch-guard.sh" "$out"
git -C "$HREPO" restore --staged lib.sh 2>/dev/null
git -C "$HREPO" checkout -q -- lib.sh 2>/dev/null || true

echo "SEEN GREEN (positive control): an ordinary operator commit through the same hook succeeds:"
printf 'ordinary change\n' >> "$HREPO/gate.sh"
git -C "$HREPO" add gate.sh
out="$(commit_through_hook op@example.com op "chore: ordinary operator commit")"; rc=$?
is "hook run: an ordinary operator commit is not refused" "0" "$rc"

tl_summary
