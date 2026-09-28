#!/usr/bin/env bash
# testlib-migration-lib.sh — the exception list and offender pattern testlib-fence.sh
# (queue.sh submit's static, added/modified-only check) reads. The exception list mirrors
# spira-lint/testlib-migrated-allow, which the corpus-wide spira-lint rule reads.
#
# Sourced, never executed.
set -u

# testlib_migration_offender_pattern -> an ERE matching a self-rolled assertion primitive:
# testlib.sh's own function names redefined, or a bare pass=/fail= counter (test-reconciler-
# flow.sh's own shape before sp-rh0x3: `pass=0; fail=0` with no ok()/bad() at all).
testlib_migration_offender_pattern() {
    printf '%s' '^(ok|bad|fail|is|want|nowant|notwant|wantrc|pass)\(\)|(^|; )(pass|fail)=[0-9]+'
}

# testlib_migration_sources_testlib <file> -> true iff the file sources testlib.sh the
# conventional way. A file that merely mentions the string (a comment, a grep pattern) has
# not reached the real contract, so this matches the sourcing statement itself, not the name.
testlib_migration_sources_testlib() {
    grep -qE '^[[:space:]]*(\.|source)[[:space:]]+.*testlib\.sh' "$1" 2>/dev/null
}

# testlib_migration_exceptions -> one basename per line: suites granted a grandfather clause
# by a real decision, so an unrelated edit to one of them is not re-flagged as a new offense.
# Grow this only from a decision recorded on the bead that grants it; keep it equal to
# spira-lint/testlib-migrated-allow.
testlib_migration_exceptions() {
    printf '%s\n' test-install-refusal.sh
}
