#!/usr/bin/env bash
# test-testenv-batch-branch.sh — testenv-batch.sh reads suite scripts from
# the branch under test, not from the production checkout.
#
# WHAT THIS PROVES
#   The positive control for sp-2f51e: running testenv-batch.sh against a
#   branch that has a FAILING suite returns rc=1, while running it against
#   a branch with a PASSING suite returns rc=0 — even when the working tree
#   ($FIXTURE) is on a different branch.
#
#   Before the fix both returned 0: the container mounted the production
#   checkout regardless of the branch argument, and the production checkout
#   was always green.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control)
#   D1: running against the FAILING branch returns rc=1 (branch read).
#   D2: running against the PASSING branch returns rc=0 (same fixture repo,
#       different branch).
#   D1 and D2 would both return 0 against origin/main before the fix, because
#   the container mounted origin/main regardless of the branch argument.
#
# host-reason: requires podman
# tier: T0
# covers: spira/testenv-batch.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

iszero()  { [ "$2" = 0 ]    && ok "$1" || bad "$1" "expected 0, got $2"; }
isexit1() { [ "$2" = 1 ]    && ok "$1" || bad "$1" "expected 1, got $2"; }

BATCH="$HERE/testenv-batch.sh"
TESTENV="$HERE/testenv.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "test-testenv-batch-branch.sh"

# ---------------------------------------------------------------------------
# PRE-FLIGHT — skip if podman or user systemd is unavailable.
# ---------------------------------------------------------------------------
command -v podman >/dev/null 2>&1 || {
    printf 'SKIP test-testenv-batch-branch.sh: podman not on PATH\n' >&2
    exit 77
}

PRE_CNAME="spira-batch-brpre-$$"
bash "$TESTENV" up --name "$PRE_CNAME" >&2 || {
    printf 'SKIP test-testenv-batch-branch.sh: container did not start\n' >&2
    exit 77
}
if ! bash "$TESTENV" probe --name "$PRE_CNAME" 2>/dev/null; then
    bash "$TESTENV" down --name "$PRE_CNAME" >/dev/null 2>&1 || true
    printf 'SKIP test-testenv-batch-branch.sh: user systemd not available\n' >&2
    exit 77
fi
bash "$TESTENV" down --name "$PRE_CNAME" >/dev/null 2>&1 || true

# ---------------------------------------------------------------------------
# FIXTURE REPO — two branches with opposite suite content.
#
# main:      spira/test-target.sh exits 0 (passing)
# breaks-it: spira/test-target.sh exits 1 (failing)
#
# The fixture working tree is left on breaks-it. Before the fix the container
# mounted the working tree and both branch runs returned rc=1. After the fix
# each run creates a worktree for its branch and returns the branch's result.
# ---------------------------------------------------------------------------
REMOTE="$TMP/remote"
FIXTURE="$TMP/fixture"

git init -q --initial-branch=main "$REMOTE"
git -C "$REMOTE" config user.email "test@spira.local"
git -C "$REMOTE" config user.name "Spira Test"
touch "$REMOTE/placeholder"
git -C "$REMOTE" add placeholder
git -C "$REMOTE" commit -q -m "initial"

git clone -q --local "$REMOTE" "$FIXTURE"
git -C "$FIXTURE" config user.email "test@spira.local"
git -C "$FIXTURE" config user.name "Spira Test"

# main: passing suite
mkdir -p "$FIXTURE/spira"
cat > "$FIXTURE/spira/test-target.sh" << 'EOF'
#!/usr/bin/env bash
printf '  ok    test-target: passes on main\n'; exit 0
EOF
chmod +x "$FIXTURE/spira/test-target.sh"
git -C "$FIXTURE" add spira/
git -C "$FIXTURE" commit -q -m "main: target suite passes"

# breaks-it: same suite name, exits 1
git -C "$FIXTURE" checkout -q -b breaks-it
cat > "$FIXTURE/spira/test-target.sh" << 'EOF'
#!/usr/bin/env bash
printf '  FAIL  test-target: fails on breaks-it\n'; exit 1
EOF
git -C "$FIXTURE" add spira/
git -C "$FIXTURE" commit -q -m "breaks-it: target suite fails"
# Fixture working tree is now on breaks-it (with the failing suite).

echo
echo "D1: running against breaks-it → rc=1 (branch has failing suite)"

RESULTS_ROOT_D1="$TMP/results-D1"
rc_d1=0
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_D1" \
SPIRA_BATCH_INSTANCE="d1-$$" \
SPIRA_VERDICT_TTL=0 \
    bash "$BATCH" --suites test-target.sh breaks-it "$FIXTURE" || rc_d1=$?

isexit1 "D1: running against breaks-it gives rc=1 (branch suite is red)" "$rc_d1"

echo
echo "D2: running against main → rc=0 (branch has passing suite)"
echo "    (positive control: fixture working tree is still on breaks-it with failing suite)"
echo "    (before the fix both D1 and D2 returned rc=1 — the working tree, not the branch)"

RESULTS_ROOT_D2="$TMP/results-D2"
rc_d2=0
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_D2" \
SPIRA_BATCH_INSTANCE="d2-$$" \
SPIRA_VERDICT_TTL=0 \
    bash "$BATCH" --suites test-target.sh main "$FIXTURE" || rc_d2=$?

iszero "D2: running against main gives rc=0 even though working tree is on breaks-it" "$rc_d2"

echo
tl_summary
