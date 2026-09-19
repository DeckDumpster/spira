#!/usr/bin/env bash
#
# test-release-workflow.sh — .github/workflows/release.yml structural checks:
# the workflow file exists, triggers on spira-release-* tags, pins the
# toolchain to a specific semver rather than a floating alias, and includes
# an assertion step that verifies the installed version.
#
# A GitHub Actions workflow cannot be executed locally, so this suite validates
# structural properties of the YAML file rather than its runtime behaviour.
# These properties are the ones the bead's acceptance criteria name explicitly:
# the trigger pattern, the pinned toolchain, and the assertion step.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control)
# --------------------------------------------------------
# Case 1 proves the file is present before any "not found in file" result can
# be read as meaningful. A missing file and a check whose pattern never matches
# look the same from outside — the positive control distinguishes them.
#
# host-reason: structural grep checks on a YAML file; no container or database dependency
#
# CASES
#   1. POSITIVE CONTROL: workflow file is present.
#   2. Trigger is a push on tags matching spira-release-*.
#   3. Toolchain is pinned to a semver (not "stable", "nightly", or "beta").
#   4. An assertion step verifies the installed Rust version at runtime.
#   5. No build step silences failure with continue-on-error: true.
#
# covers: .github/workflows/release.yml
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
# $HERE is always the spira/ directory, one level below the repo root.
# git rev-parse --show-toplevel fails inside the gate's container because the
# worktree .git file references the parent repo path, which is not mounted there.
REPO_ROOT="$(cd "$HERE/.." && pwd -P)"
WORKFLOW="$REPO_ROOT/.github/workflows/release.yml"

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { grep -qF -- "$2" "$WORKFLOW" 2>/dev/null && ok "$1" || bad "$1" "not found in workflow: $2"; }
nowant() { grep -qF -- "$2" "$WORKFLOW" 2>/dev/null && bad "$1" "found in workflow (should not be): $2" || ok "$1"; }

echo "test-release-workflow.sh"

# ============================================================================
echo
echo "1. POSITIVE CONTROL — workflow file is present"
# ============================================================================

if [ -f "$WORKFLOW" ]; then
    ok "release.yml exists at .github/workflows/release.yml"
else
    bad "release.yml exists at .github/workflows/release.yml" "not found; skipping remaining cases"
    printf '\n%d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi

# ============================================================================
echo
echo "2. Trigger — push on tags matching spira-release-*"
# ============================================================================

want "on: push" "push:" "$WORKFLOW"
want "tag pattern spira-release-*" "spira-release-*"

# ============================================================================
echo
echo "3. Toolchain — pinned to a semver, not a floating alias"
# ============================================================================

# A floating alias ("stable", "nightly", "beta") produces unreproducible builds
# and hides version drift. The pinned semver is what the assertion step checks
# against — they must agree, and both must be explicit.
nowant "not toolchain: stable" "toolchain: stable"
nowant "not toolchain: nightly" "toolchain: nightly"
nowant "not toolchain: beta" "toolchain: beta"

# A pinned semver matches X.Y.Z.
if grep -qE "['\"]?1\.[0-9]+\.[0-9]+['\"]?" "$WORKFLOW" 2>/dev/null; then
    ok "semver X.Y.Z pinned in workflow"
else
    bad "semver X.Y.Z pinned in workflow" "no 1.x.y version string found"
fi

# ============================================================================
echo
echo "4. Assertion step — rustc --version checked at runtime"
# ============================================================================

# The assertion step runs after toolchain installation and exits non-zero if
# the installed version does not match the pinned value. Without it, a runner
# that ships the wrong Rust version produces a binary without any notice.
want "rustc --version checked" "rustc --version"

# ============================================================================
echo
echo "5. Build steps do not suppress failure"
# ============================================================================

# A workflow that publishes despite a build failure is worse than one that
# publishes nothing — the published artifact appears good. continue-on-error:
# true on any step suppresses the failure the job would otherwise propagate.
nowant "no continue-on-error: true" "continue-on-error: true"

# ============================================================================
echo
echo "6. build-tarball.sh is called with --name to stamp once per release"
# ============================================================================

# The tarball stem must match the release tag stem so deploy.sh and skew.sh
# can correlate them without a separate asset lookup or timestamp fallback.
# Without --name, build-tarball.sh generates its own timestamp independently
# of the tag, producing the mismatch that issue #51 describes.
want "build-tarball.sh uses --name" "--name"

# ============================================================================
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
