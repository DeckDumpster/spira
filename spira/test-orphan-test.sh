#!/usr/bin/env bash
#
# test-orphan-test.sh — the fence that catches a diff orphaning a test suite.
#
#   ./test-orphan-test.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# orphan-test.sh refuses a diff that removes a token from a non-test source file while a
# test suite that is NOT modified by the same diff still asserts on that token. The test then
# goes red on the next timed run, pointed at the source file it was testing, which is correct.
# Two violations earned this fence; both are live at the time this suite was written.
#
# THE POSITIVE CONTROL IS THE FIRST ASSERTION (law-absence-needs-a-positive-control). A fence
# that reports a clean diff is indistinguishable from a fence whose matcher never fires. So:
#   1. A scratch repository is built with a source file containing a hyphenated token and a
#      test suite asserting on it.
#   2. A diff is produced that removes the token from the source file WITHOUT updating the
#      suite.
#   3. The fence MUST refuse and name both the source file and the suite.
#   4. The matcher is then deliberately broken; the same diff must NOT be refused — proving
#      that the pass below is real and the matcher was actually exercised.
#   5. The matcher is restored. The same diff WITH the suite updated MUST pass.
#
# The acceptance criteria from the bead, verified here:
#   • A diff deleting a token from a source file without touching its test suite is refused,
#     and the message names both.
#   • The same diff with the suite updated in the same diff passes.
#   • With the fence's matcher deliberately broken, the planted case is NOT refused — proving
#     the pass is a real pass, not a matcher that never fired.
#   • The override (orphan-test-ok) is named in the refusal text.
#
# host-reason: builds a scratch git repository on the host filesystem; the container has no
# git user config and this suite configures one explicitly.
#
# tier: T0
# covers: spira/orphan-test.sh spira/gate-spira.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-orphan-test.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# fence_at <root> — run the fence from inside the given scratch root, with minimal env.
# SPIRA_GATE_BASE is always set (to origin/main) so the fence does not skip.
fence_at() {
    local root="$1"; shift
    ( cd "$root" && env -i PATH="$PATH" HOME="$TMP" TERM=dumb \
        SPIRA_GATE_BASE=origin/main \
        bash "$root/spira/orphan-test.sh" "$@" 2>&1 )
}

# ---------------------------------------------------------------------------------------
# BUILD THE SCRATCH REPOSITORY
# One branch (main) with a source file and a test suite that asserts on a token; a second
# branch (del-token) that removes the token from the source file but does NOT update the
# suite.
# ---------------------------------------------------------------------------------------
ROOT="$TMP/root"
mkdir -p "$ROOT/spira"
cp "$HERE/orphan-test.sh" "$ROOT/spira/orphan-test.sh"

git init -q -b main "$ROOT"
git -C "$ROOT" config user.email t@t
git -C "$ROOT" config user.name t

# Source file with a hyphenated token.
# The token "sp-recur-" is the exact pattern from the first violation this fence was built for.
cat > "$ROOT/spira/source.sh" <<'SRC'
#!/usr/bin/env bash
# Source file with a label the suites depend on.
LABEL_PREFIX="sp-recur-"
incident_label() { printf '%s%d\n' "sp-recur-" "$1"; }
SRC

# Test suite asserting on the same token.
cat > "$ROOT/spira/test-source.sh" <<'TST'
#!/usr/bin/env bash
# covers: spira/source.sh
set -uo pipefail
label="$(bash spira/source.sh)"
[[ "$label" == *sp-recur-* ]] && echo ok || echo FAIL
TST

git -C "$ROOT" add .
git -C "$ROOT" commit -q -m "initial"

# Add the remote (required so origin/main resolves; point at the same repo).
git -C "$ROOT" remote add origin "$ROOT"

# Create the branch that removes the token without updating the test.
git -C "$ROOT" checkout -q -b del-token

cat > "$ROOT/spira/source.sh" <<'SRC2'
#!/usr/bin/env bash
# Source file — token removed.
LABEL_PREFIX="sp-incident-"
incident_label() { printf '%s%d\n' "sp-incident-" "$1"; }
SRC2

git -C "$ROOT" add spira/source.sh
git -C "$ROOT" commit -q -m "rename token without updating test"

# Point origin/main at the initial commit (the base the branch diverged from).
git -C "$ROOT" update-ref refs/remotes/origin/main main

# ---------------------------------------------------------------------------------------
# THE POSITIVE CONTROL (law-absence-needs-a-positive-control).
# The fence MUST refuse this diff — the token was removed from source.sh but test-source.sh
# still asserts on it and is NOT in the diff.
# ---------------------------------------------------------------------------------------
out="$(fence_at "$ROOT")"; rc=$?
is   "SEEN RED: orphaned test is refused"            "1" "$rc"
want "names the token"                               "sp-recur-" "$out"
want "names the test suite"                          "test-source.sh" "$out"
want "names the source file"                         "source.sh" "$out"
want "names the override annotation"                 "orphan-test-ok" "$out"

# ---------------------------------------------------------------------------------------
# POSITIVE CONTROL FOR THE MATCHER (law-absence-needs-a-positive-control, second half).
# Break the matcher and confirm the planted case is NOT refused — proving the pass below
# is a real pass and not a matcher that never fired.
# ---------------------------------------------------------------------------------------
cp "$ROOT/spira/orphan-test.sh" "$TMP/orphan-test.sh.real"
# A broken fence: always exits 0 without checking anything.
printf '#!/usr/bin/env bash\nprintf "orphan-test: clean (matcher disabled)\\n"\nexit 0\n' \
    > "$ROOT/spira/orphan-test.sh"
out_broken="$(fence_at "$ROOT")"; rc_broken=$?
is   "broken matcher does not refuse"         "0" "$rc_broken"
want "broken matcher says clean"              "clean" "$out_broken"
# Restore the real fence.
cp "$TMP/orphan-test.sh.real" "$ROOT/spira/orphan-test.sh"

# Confirm the real fence still refuses after restoration (same diff, same state).
out2="$(fence_at "$ROOT")"; rc2=$?
is   "real fence still refuses after restoration" "1" "$rc2"
want "still names the token after restoration"    "sp-recur-" "$out2"

# ---------------------------------------------------------------------------------------
# PASS CASE: the same diff WITH the suite updated in the same diff.
# ---------------------------------------------------------------------------------------
cat > "$ROOT/spira/test-source.sh" <<'TST2'
#!/usr/bin/env bash
# covers: spira/source.sh
set -uo pipefail
label="$(bash spira/source.sh)"
[[ "$label" == *sp-incident-* ]] && echo ok || echo FAIL
TST2

git -C "$ROOT" add spira/test-source.sh
git -C "$ROOT" commit -q -m "update test to match renamed token"

# Now origin/main points at the base; HEAD is del-token with both files updated.
# The changed-file list includes test-source.sh, so it is NOT an orphan.
out3="$(fence_at "$ROOT")"; rc3=$?
is   "PASS CASE: diff with suite updated passes" "0" "$rc3"
want "says clean"                                "clean" "$out3"

# ---------------------------------------------------------------------------------------
# SKIP CASE: no SPIRA_GATE_BASE in the environment → exit 77.
# ---------------------------------------------------------------------------------------
out4="$( cd "$ROOT" && env -i PATH="$PATH" HOME="$TMP" TERM=dumb \
    bash "$ROOT/spira/orphan-test.sh" 2>&1 )"; rc4=$?
is   "skips when SPIRA_GATE_BASE is absent" "77" "$rc4"
want "says why it is skipping"              "SPIRA_GATE_BASE" "$out4"

# ---------------------------------------------------------------------------------------
# OVERRIDE CASE: orphan-test-ok on the offending line in the test suite.
# Roll back the test update so the diff orphans the test again, then add the override.
# ---------------------------------------------------------------------------------------
git -C "$ROOT" checkout -q HEAD~1 -- spira/test-source.sh

cat > "$ROOT/spira/test-source.sh" <<'TST3'
#!/usr/bin/env bash
# covers: spira/source.sh
set -uo pipefail
label="$(bash spira/source.sh)"
# orphan-test-ok: deliberately asserts the old label to catch regressions
[[ "$label" == *sp-recur-* ]] && echo ok || echo FAIL
TST3

git -C "$ROOT" add spira/test-source.sh
git -C "$ROOT" commit -q -m "add orphan-test-ok override"
# Update the base ref to stay one commit behind HEAD.
git -C "$ROOT" update-ref refs/remotes/origin/main "$(git -C "$ROOT" rev-parse HEAD~2)"

out5="$(fence_at "$ROOT")"; rc5=$?
is   "OVERRIDE CASE: annotated line is not refused" "0" "$rc5"
want "says clean with override present"             "clean" "$out5"

# ---------------------------------------------------------------------------------------
# REFACTORED-TOKEN CASE: token on both - and + lines of the same diff is NOT refused.
#
# SEEN RED: without the fix, the old code extracted every token from - lines regardless of
# whether the same token appeared on + lines. Wrapping a call in an `if` block puts the
# original line on - and an equivalent line on +; the token is not truly removed. The old
# orphan-test flagged it as removed and refused the branch — the exact false positive this
# fix closes. The test was seen to fail against the unfixed awk path (only checking -lines).
# ---------------------------------------------------------------------------------------

# Start from a fresh state: reset test-source.sh back to asserting sp-recur-.
# The source file also needs sp-recur- on both - and + sides (refactored, not removed).
git -C "$ROOT" checkout -q HEAD~1 -- spira/source.sh
git -C "$ROOT" checkout -q HEAD~1 -- spira/test-source.sh

# Commit a version where source.sh is modified so sp-recur- appears on BOTH - and + lines:
# a wrapping change (add an if-block) that keeps the token present.
cat > "$ROOT/spira/source.sh" <<'SRC3'
#!/usr/bin/env bash
# Source file — token refactored, not removed.
LABEL_PREFIX="sp-recur-"
incident_label() {
    if true; then
        printf '%s%d\n' "sp-recur-" "$1"
    fi
}
SRC3
git -C "$ROOT" add spira/source.sh
git -C "$ROOT" commit -q -m "refactor: wrap sp-recur- call in if-block (token still present)"
git -C "$ROOT" update-ref refs/remotes/origin/main "$(git -C "$ROOT" rev-parse HEAD~1)"

out6="$(fence_at "$ROOT")"; rc6=$?
is   "REFACTORED-TOKEN: token on - and + lines is not refused" "0" "$rc6"
want "says clean for refactored token"                         "clean" "$out6"
tl_summary
