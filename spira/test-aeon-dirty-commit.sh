#!/usr/bin/env bash
#
# test-aeon-dirty-commit.sh — pre-existing dirty files must not appear in an aeon's commit.
#
#   ./test-aeon-dirty-commit.sh
#
# THE DEFECT THIS REPRODUCES. An aeon working in a shared checkout ran `git add -A` and
# staged a file written by another process (the archivist). The commit named the bead but
# carried none of the aeon's own work — only the archivist's uncommitted draft. The guard
# installed by aeon.sh prevents this by refusing to commit any path that was dirty
# (modified or untracked) before the aeon's session began.
#
# THREE CASES (law-absence-needs-a-positive-control):
#   1. `git add -A` stages the dirty file → commit is refused and names the offender.
#   2. `git add -- <own-path>` succeeds and the dirty file is absent from the commit.
#   3. SPIRA_ALLOW_DIRTY_STAGE=1 overrides the guard — the fence names its own bypass.
#
# Driven through `worktree-hooks.sh install` — the composed hook aeon.sh actually arms
# (canonical exclude.sh/scratch-fence.sh/branch-guard.sh, then pre-commit-guard.sh) —
# rather than a hand-written wrapper that calls pre-commit-guard.sh alone. A hand-written
# hook proves this guard fires in isolation; it does not prove aeon.sh's real install
# composes it correctly (UC-safety-fences-21 covers that composition directly; this suite
# exercises the dirty-path guard's own behaviour through the same install).
#
# `# requires: testenv` (below, read by testlib.sh) replaces a from-scratch podman
# re-exec that keyed on IN_TESTENV while CI sets SPIRA_IN_TESTENV — the suite silently
# never ran under CI (gap 10, docs/test-plan/safety-fences.md). testenv-batch.sh already
# provides the container and sets SPIRA_IN_TESTENV=1; GIT_CONFIG_GLOBAL/GIT_CONFIG_NOSYSTEM
# below keep ambient host gitconfig out of the real git this suite drives.
#
# requires: testenv
# tier: T2
# covers: spira/aeon.sh spira/pre-commit-guard.sh spira/worktree-hooks.sh UC-safety-fences-17
# defect: sp-rbxr
# scar: an aeon ran `git add -A` and staged a pre-existing dirty file; the commit named the bead but carried another process's uncommitted work instead of the aeon's own.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

# A GUARD ON THE GUARD: the hook must exist; testing with a missing hook would give a false
# pass on every case (no hook = no refusal = commits succeed = looks like "excluded").
[ -f "$HERE/pre-commit-guard.sh" ] || bail "pre-commit-guard.sh not found beside this suite"

# Repo with a remote, matching what aeon.sh creates.
ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null

# Enable per-worktree config, as aeon.sh does — prerequisite for worktree-specific hooks.
git -C "$REPO" config extensions.worktreeConfig true

# Create a worktree (as aeon.sh does for each bead).
WORK="$TMP/work"
git -C "$REPO" worktree add -q -b spira/sp-test "$WORK" origin/main

# Get the worktree-specific git dir (different from the shared .git/).
WGD="$(git -C "$WORK" rev-parse --path-format=absolute --git-dir)"

# PLANT THE DIRTY FILE BEFORE THE SNAPSHOT — this simulates the archivist writing a wiki
# page to the shared checkout while an aeon's worktree is open in the same directory tree.
mkdir -p "$WORK/wiki/notes"
printf 'archivist draft content\n' > "$WORK/wiki/notes/archivist-2026-09-08.md"

# Take the snapshot (exactly as aeon.sh does).
SNAP="$WGD/spira-dirty-before"
{ git -C "$WORK" diff --name-only HEAD 2>/dev/null
  git -C "$WORK" ls-files --others --exclude-standard 2>/dev/null
} | sort -u > "$SNAP"

# COMPOSE THE REAL HOOK, THE WAY aeon.sh DOES. worktree-hooks.sh install arms the
# canonical fences (exclude.sh staged, scratch-fence.sh, branch-guard.sh staged) ahead of
# pre-commit-guard.sh in one generated pre-commit — not a wrapper that calls
# pre-commit-guard.sh alone, which would pass even if aeon.sh's own install were broken.
SPIRA_HOME="$HERE" bash "$HERE/worktree-hooks.sh" install "$WORK" >/dev/null

# Verify setup: the snapshot must contain the archivist file or the test proves nothing.
if grep -qF "wiki/notes/archivist-2026-09-08.md" "$SNAP"; then
    ok "setup: archivist file appears in the pre-session snapshot"
else
    bad "setup: archivist file appears in the pre-session snapshot" \
        "snapshot is empty or missing the planted path"
fi

# ======================================================================================
echo
echo "CASE 1: git add -A stages the dirty file — commit must be refused:"
# ======================================================================================
printf 'aeon output\n' > "$WORK/aeon-output.txt"
git -C "$WORK" add -A 2>/dev/null
commit_out="$(git -C "$WORK" commit -m 'sp-test: work' 2>&1)" && commit_rc=0 || commit_rc=$?
if [ "$commit_rc" -ne 0 ]; then
    ok "commit is refused when a dirty file is staged via git add -A"
else
    bad "commit is refused when a dirty file is staged via git add -A" \
        "commit succeeded; the hook did not fire"
fi
want "the refusal names the offending file" "archivist-2026-09-08.md" "$commit_out"
git -C "$WORK" reset -q HEAD 2>/dev/null  # unstage everything before the next case

# ======================================================================================
echo
echo "CASE 2: git add -- <own-path> only — dirty file absent from commit:"
# ======================================================================================
git -C "$WORK" add -- aeon-output.txt
git -C "$WORK" commit -qm 'sp-test: aeon work only' 2>/dev/null
nowant "dirty file is absent from commit" "archivist-2026-09-08.md" \
    "$(git -C "$WORK" show --name-only HEAD)"
want "aeon's own file is present in the commit" "aeon-output.txt" \
    "$(git -C "$WORK" show --name-only HEAD)"

# ======================================================================================
echo
echo "CASE 3: SPIRA_ALLOW_DIRTY_STAGE=1 overrides the guard:"
# ======================================================================================
# Without override: staging the archivist file directly must still be refused.
git -C "$WORK" add -- "wiki/notes/archivist-2026-09-08.md" 2>/dev/null
commit_out="$(git -C "$WORK" commit -m 'sp-test: should fail' 2>&1)" && commit_rc=0 || commit_rc=$?
if [ "$commit_rc" -ne 0 ]; then
    ok "without override: direct staging of pre-session file is also refused"
else
    bad "without override: direct staging of pre-session file is also refused" \
        "commit succeeded with no override"
fi
git -C "$WORK" reset -q HEAD 2>/dev/null

# With override: the fence must yield.
git -C "$WORK" add -- "wiki/notes/archivist-2026-09-08.md" 2>/dev/null
if SPIRA_ALLOW_DIRTY_STAGE=1 git -C "$WORK" commit -qm 'sp-test: override' 2>/dev/null; then
    ok "with override: commit is allowed despite pre-session file"
else
    bad "with override: commit is allowed despite pre-session file" \
        "commit refused even with SPIRA_ALLOW_DIRTY_STAGE=1"
fi

tl_summary
