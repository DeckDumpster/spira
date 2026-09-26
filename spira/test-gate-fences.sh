#!/usr/bin/env bash
#
# test-gate-fences.sh — gate_fence_list is the ONE list gate-spira.sh's fence loop reads,
# and this is the one row that checks it (D7).
#
# Before gate-fences.sh existed, test-inventory.sh, test-scratch-fence.sh and
# test-suite-state-fence.sh each grepped gate-spira.sh's source for their own fence's
# name — three independent copies of "is this fence still wired into the gate", each of
# which could go stale without the other two noticing. Read the list gate-spira.sh itself
# reads, and assert it against the set of fences this area actually expects; a fence
# dropped from gate-fences.sh now breaks this row directly, instead of leaving three
# greps to quietly stop matching a renamed string.
#
# The hooks/pre-commit half of the old test-scratch-fence.sh grep (whether the hook itself
# calls scratch-fence.sh) is not reasserted here: UC-safety-fences-22's commit-through-hook
# row (sp-pohf2) replaces it with a real commit through the armed hook, which is the
# behaviour that matters — a grep of the hook's source proves nothing a real commit doesn't
# prove better.
#
# tier: T1
# covers: spira/gate-fences.sh spira/gate-spira.sh UC-safety-fences-24 UC-safety-fences-25 UC-safety-fences-28
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/gate-fences.sh"

echo "test-gate-fences.sh"

EXPECTED="spira/exclude.sh
spira/inventory.sh
spira/scratch-fence.sh
spira/wiki-add-fence.sh
spira/sop.sh
spira/literal-lint.sh
spira/testdb-mode-lint.sh
spira/bd-stdin-lint.sh
spira/gh-intake-lint.sh
spira/incident-cause-lint.sh
spira/suite-state-fence.sh
spira/orphan-test.sh
spira/tmux-scope-fence.sh"

is "gate_fence_list is exactly the expected set" "$EXPECTED" "$(gate_fence_list)"

# POSITIVE CONTROL ON THE WIRING ITSELF: gate-spira.sh must call gate_fence_list, not
# carry a second, hand-written copy of the same names — that second copy is exactly the
# thing D7 existed to delete. A gate-spira.sh with the names pasted back in as a literal
# would still pass the assertion above (gate-fences.sh is unchanged) while silently
# un-wiring the module this suite exists to hold in place.
gate_src="$(cat "$HERE/gate-spira.sh")"
want "gate-spira.sh sources gate-fences.sh" "gate-fences.sh" "$gate_src"
want "and reads its fence list from gate_fence_list, not a second literal" \
     "for fence in \$(gate_fence_list)" "$gate_src"

# Every path gate_fence_list names must be a real, readable file — a stale entry (a fence
# renamed or deleted on one side only) is exactly the drift this module exists to close.
missing=""
for f in $(gate_fence_list); do
    [ -r "$HERE/../$f" ] || missing="$missing $f"
done
is "every fence gate_fence_list names is readable" "" "$missing"

tl_summary
