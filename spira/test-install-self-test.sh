#!/usr/bin/env bash
#
# test-install-self-test.sh — conf.sh derives SPIRA_SELF_TEST from .git presence.
#
#   ./test-install-self-test.sh
#
# WHAT THIS SUITE PROVES
# ----------------------
# SPIRA_SELF_TEST=0 is the right default for consumer installations (from a release
# tarball with no .git directory). Filing self-test beads into the operator's work
# queue is the defect: sp-uqod.
#
# conf.sh derives SPIRA_SELF_TEST from SPIRA_REPO_DERIVED (the git root of SPIRA_HOME).
# A SPIRA_HOME outside any git checkout gets SPIRA_SELF_TEST=0; SPIRA_HOME inside a git
# checkout gets SPIRA_SELF_TEST=1. An explicit SPIRA_SELF_TEST=0 in spira.conf overrides
# the detection.
#
# The UNITS/OPTIONAL/ENABLE gating that SPIRA_SELF_TEST controls (does the suites timer
# get installed) moved to test-units-optional.sh, cluster 7 in docs/test-plan/
# instance-lifecycle.md: that is one instance of a shape shared with the loom/broker/
# mail-deliver gates, not a property of conf.sh's own detection, which is what this
# file is left to prove.
#
# defect: sp-uqod
# covers: spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-install-self-test.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ==========================================================================
echo
echo "AUTO-DETECT — conf.sh derives SPIRA_SELF_TEST from .git presence:"
# ==========================================================================
# The detection uses SPIRA_REPO_DERIVED, which is computed from
# `git -C "$SPIRA_HOME" rev-parse --show-toplevel`. To test both outcomes,
# we need SPIRA_HOME to point at (a) a non-git directory and (b) a git checkout.
#
# For (a), we create a stub harness under $TMP, which is /tmp — outside any
# git checkout in the testenv container. git will walk upward from there and
# find nothing, leaving SPIRA_REPO_DERIVED empty (fallback to $TMP's parent),
# which has no .git, so SPIRA_SELF_TEST defaults to 0.
NO_GIT="$TMP/nogit"
mkdir -p "$NO_GIT/spira"
cp "$HERE/conf.sh" "$NO_GIT/spira/conf.sh"

detect() {      # detect <spira-home-dir> -> SPIRA_SELF_TEST value or UNSET
    env -i \
        "PATH=$PATH" \
        "HOME=$TMP/detect-home" \
        SPIRA_CONF=/nonexistent \
        "SPIRA_RUN=$TMP/detect-run" \
        "SPIRA_HOME=$1" \
        bash -c '. "$SPIRA_HOME/conf.sh" 2>/dev/null
                 printf "%s" "${SPIRA_SELF_TEST:-UNSET}"'
}
mkdir -p "$TMP/detect-home" "$TMP/detect-run"

# C1: SPIRA_HOME points into /tmp (no git checkout there) → SPIRA_SELF_TEST=0.
nogit_val="$(detect "$NO_GIT/spira")"
is "C1: no .git above SPIRA_HOME → SPIRA_SELF_TEST=0" "0" "$nogit_val"

# C2: SPIRA_HOME points at the real installed harness (inside a git checkout) → 1.
git_val="$(detect "$HERE")"
is "C2: .git present above SPIRA_HOME → SPIRA_SELF_TEST=1" "1" "$git_val"

# C3: SPIRA_SELF_TEST=0 in spira.conf overrides detection even on a git checkout.
OVERRIDE_CONF="$TMP/override.conf"
printf 'SPIRA_SELF_TEST=0\n' > "$OVERRIDE_CONF"
override_val="$(
    env -i \
        "PATH=$PATH" \
        "HOME=$TMP/detect-home" \
        "SPIRA_CONF=$OVERRIDE_CONF" \
        "SPIRA_RUN=$TMP/detect-run" \
        "SPIRA_HOME=$HERE" \
        bash -c '. "$SPIRA_HOME/conf.sh" 2>/dev/null
                 printf "%s" "${SPIRA_SELF_TEST:-UNSET}"'
)"
is "C3: SPIRA_SELF_TEST=0 in spira.conf overrides .git detection" "0" "$override_val"

# ==========================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
