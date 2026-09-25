#!/usr/bin/env bash
#
# test-landing-basefail-select-t1.sh — basefail_fix_decision (landing-lib.sh), the pure
# selection _basefail_fix_check delegates to: a closed branch is a certifiable base-fix
# only when its external_ref names THIS repository's basefail suite and the branch's own
# section of the gate transcript shows that suite green, not red, timed out, or killed.
# Every row here is a function call over a literal external_ref and a literal gate
# transcript — no git, no testdb, no gate.sh trial, no landing pass. What
# test-landing-basefail-fix.sh needs 3+ worktrees and a stub gate script to exercise one
# branch of, this asserts directly.
#
# host-reason: sources landing-lib.sh only; no database, no systemd, no git
# tier: T1
# covers: spira/landing-lib.sh UC-landing-merge-queue-11
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
. "$HERE/landing-lib.sh"

echo "test-landing-basefail-select-t1.sh"

REPO=fixture-repo
SUITE=test-failing.sh

gate_out_red() {   # the branch's own section still shows the suite red
    printf '%s\n' \
        "gate: $REPO's own gate fails against origin/main — this branch did not cause it." \
        "gate: red on origin/main: $SUITE" \
        "--- origin/main's own output ---" \
        "$SUITE RED 3.1s" \
        "--- this branch's output ---" \
        "$SUITE RED 1.8s" \
        "gate: fix the repository, or clear that command from the repo-map."
}

gate_out_green() {   # the branch's own section omits the suite — it is green there
    printf '%s\n' \
        "gate: $REPO's own gate fails against origin/main — this branch did not cause it." \
        "gate: red on origin/main: $SUITE" \
        "--- origin/main's own output ---" \
        "$SUITE RED 3.1s" \
        "--- this branch's output ---" \
        "test-other.sh GREEN 1.2s" \
        "gate: fix the repository, or clear that command from the repo-map."
}

gate_out_timeout() {
    printf '%s\n' \
        "--- origin/main's own output ---" \
        "$SUITE RED 3.1s" \
        "--- this branch's output ---" \
        "$SUITE TIMEOUT 30.0s"
}

gate_out_killed() {
    printf '%s\n' \
        "--- origin/main's own output ---" \
        "$SUITE RED 3.1s" \
        "--- this branch's output ---" \
        "$SUITE was killed"
}

# 1. GREEN ON SUITE: extref names this repo's basefail suite, branch section is green.
basefail_fix_decision "basefail:$REPO:$SUITE" "$REPO" "$(gate_out_green)"
is "basefail_fix_decision(): green branch section -> fix (rc 0)" "0" "$?"

# POSITIVE CONTROL for 1: the SAME extref, but the branch section is still red, is NOT a
# fix — proves the matcher reads the branch's own section, not just the extref shape.
basefail_fix_decision "basefail:$REPO:$SUITE" "$REPO" "$(gate_out_red)"
is "basefail_fix_decision(): red branch section -> not a fix (rc 1)" "1" "$?"

# 2. TIMEOUT counts as red, not green.
basefail_fix_decision "basefail:$REPO:$SUITE" "$REPO" "$(gate_out_timeout)"
is "basefail_fix_decision(): branch section TIMEOUT -> not a fix (rc 1)" "1" "$?"

# 3. "was killed" counts as red, not green.
basefail_fix_decision "basefail:$REPO:$SUITE" "$REPO" "$(gate_out_killed)"
is "basefail_fix_decision(): branch section 'was killed' -> not a fix (rc 1)" "1" "$?"

# 4. WRONG REPO: extref names a different repository's basefail suite — never this
#    repository's fix, even with a green branch section for the same suite name.
basefail_fix_decision "basefail:another-repo:$SUITE" "$REPO" "$(gate_out_green)"
is "basefail_fix_decision(): extref for a different repo -> not a fix (rc 1)" "1" "$?"

# 5. NOT A BASEFAIL REF AT ALL.
basefail_fix_decision "-" "$REPO" "$(gate_out_green)"
is "basefail_fix_decision(): extref '-' -> not a fix (rc 1)" "1" "$?"
basefail_fix_decision "" "$REPO" "$(gate_out_green)"
is "basefail_fix_decision(): empty extref -> not a fix (rc 1)" "1" "$?"

# 6. EMPTY SUITE NAME after the basefail: prefix (malformed ref) -> not a fix.
basefail_fix_decision "basefail:$REPO:" "$REPO" "$(gate_out_green)"
is "basefail_fix_decision(): basefail ref with no suite -> not a fix (rc 1)" "1" "$?"
basefail_fix_decision "basefail:$REPO:-" "$REPO" "$(gate_out_green)"
is "basefail_fix_decision(): basefail ref with '-' suite -> not a fix (rc 1)" "1" "$?"

tl_summary
