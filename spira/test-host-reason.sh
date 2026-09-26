#!/usr/bin/env bash
#
# test-host-reason.sh — the fence that requires every host suite to declare why.
#
#   ./test-host-reason.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# host-check.sh refuses a suite that runs on the host without explaining
# why: either it calls testenv.sh (up/exec) so its assertions run in a container, or
# it carries a non-empty `# host-reason: <text>` annotation.  This suite proves the
# fence can go red (positive control first), that the escape works, and that the count
# host-check.sh --count-undeclared prints is what suites.sh status renders.
#
# THE POSITIVE CONTROL IS THE FIRST ASSERTION (law-absence-needs-a-positive-control).
# A fence that never fires and a fence that fires and finds nothing are the same silence
# from outside.  Plant an undeclared suite, require the fence to name it (SEEN RED),
# then withdraw it and require a clean pass (SEEN GREEN).  Only after this pair does
# the count's silence mean anything.
#
# This fence is the replacement for two older gate checks that were both detecting suites
# reaching the running system (hermetic.sh's main scan and the fixture-contamination fence
# in gate-spira.sh).  Both became moot once every suite runs in a container — commands
# that were unroutable on the host are legitimate inside a real install, and the container
# provides the isolation the two fences were compensating for.  This fence is what remains:
# it ensures any host-running exception is visible and intentional, so a suite that would
# contaminate the production store (no container isolation, no declaration) cannot be added
# silently.
#
# THE WHOLE-TREE WALK IS OVER A SCRATCH REPOSITORY, because the shipped suites are
# not yet fully migrated and running host-check.sh against them would produce noise
# that teaches nothing about whether the fence itself is correct.
#
# The `suites.sh status` sweep line that renders these counts is a test-infrastructure
# concern, not this fence's: it lives in test-suites-classify.sh now (UC-safety-fences-27,
# gap 12), asserted unconditionally rather than inside an `if` that could silently skip when
# the line text was absent — the exact shape gap 12 names.
#
# host-reason: tests the host-reason fence; assertions run on the host using scratch repos only
#
# tier: T1
# covers: spira/host-check.sh UC-safety-fences-27
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
isz()    { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0 got $2"; }
isnz()   { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit got 0"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-host-reason.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# ---- scratch repository shared by the whole-tree tests ----
ROOT="$TMP/root"; mkdir -p "$ROOT/spira"
cp "$HERE/host-check.sh" "$ROOT/spira/host-check.sh"
git init -q -b main "$ROOT"
git -C "$ROOT" config user.email t@t; git -C "$ROOT" config user.name t

fence_at() {     # fence_at <root> [args...] -> host-check.sh output from that root
    env -i PATH="$PATH" HOME="$TMP" TERM=dumb bash "$1/spira/host-check.sh" "${@:2}" 2>&1
}

fence() {        # fence [args...] -> host-check.sh from the scratch root
    fence_at "$ROOT" "$@"
}

# The planted suite: a bare host suite with no host-reason and no container usage.
# Line 1 is the shebang, so the first real content is line 2 — known to the assertions below.
PLANTED="$ROOT/spira/test-planted-undeclared.sh"

# A clean suite that stays in the tree so withdrawal does not produce an empty tree.
# An empty tree returns exit 3 (refusing to report clean on nothing), not exit 0; the
# positive control's SEEN GREEN must be a real pass, not an absence-of-suites.
CLEAN="$ROOT/spira/test-planted-clean.sh"
printf '#!/usr/bin/env bash\n# host-reason: clean placeholder for positive control\necho clean\n' > "$CLEAN"

printf '#!/usr/bin/env bash\necho hello from planted\n' > "$PLANTED"

# ==========================================================================
# THE POSITIVE CONTROL. Plant an undeclared host suite; require the fence to
# refuse it (SEEN RED), then withdraw it and require a clean pass (SEEN GREEN).
# This is the pair that makes the subsequent silence over the shipped tree mean
# something (law-absence-needs-a-positive-control).
# ==========================================================================
out="$(fence)"; rc=$?
isnz "SEEN RED: host-check.sh refuses the undeclared planted suite" "$rc"
want "and names the file"     "test-planted-undeclared" "$out"
want "and names the override" "host-reason"             "$out"

# The fix is in the override message: add host-reason or container calls.
want "refusal mentions the fix" "host-reason" "$out"

# Withdrawn — the clean suite remains, so the tree is non-empty and exit 0 is a real pass.
rm "$PLANTED"
out="$(fence)"; rc=$?
isz "SEEN GREEN: after removing the planted suite the fence clears" "$rc"

# ==========================================================================
# EMPTY REASON IS REFUSED — the reason is the point of the declaration.
# ==========================================================================
printf '#!/usr/bin/env bash\n# host-reason:\necho hello\n' > "$PLANTED"
out="$(fence)"; rc=$?
isnz "empty # host-reason: is refused" "$rc"
want "and explains that the reason text is required" "no reason" "$out"

# A reason that is only whitespace is also empty after trimming.
printf '#!/usr/bin/env bash\n# host-reason:   \necho hello\n' > "$PLANTED"
out="$(fence)"; rc=$?
isnz "whitespace-only # host-reason: is refused" "$rc"

# ==========================================================================
# A VALID DECLARATION PASSES.
# ==========================================================================
printf '#!/usr/bin/env bash\n# host-reason: tests the fence — assertions run on the host\necho hello\n' \
    > "$PLANTED"
out="$(fence)"; rc=$?
isz "non-empty # host-reason: passes the fence" "$rc"

# The host-reason annotation can appear after the shebang comment block, not only on line 2.
printf '#!/usr/bin/env bash\n#\n# Some description.\n#\n# host-reason: drives real systemd\nset -e\necho hi\n' \
    > "$PLANTED"
out="$(fence)"; rc=$?
isz "host-reason anywhere in the file header passes" "$rc"

# ==========================================================================
# CONTAINER USAGE PASSES WITHOUT A DECLARATION.
# A suite that calls testenv.sh up or exec in a non-comment line is a container suite;
# no # host-reason: annotation is required.
# ==========================================================================
printf '#!/usr/bin/env bash\nTESTENV="$HERE/testenv.sh"\nbash "$TESTENV" up --name my-test\necho done\n' \
    > "$PLANTED"
out="$(fence)"; rc=$?
isz "testenv up in code passes without # host-reason:" "$rc"

printf '#!/usr/bin/env bash\nTESTENV="$HERE/testenv.sh"\nbash "$TESTENV" exec -- bash -c "echo hi"\n' \
    > "$PLANTED"
out="$(fence)"; rc=$?
isz "testenv exec in code passes without # host-reason:" "$rc"

# A testenv call in a COMMENT does not count — the suite itself is not a container suite.
printf '#!/usr/bin/env bash\n# calls: bash "$TESTENV" up (for reference)\necho hello\n' \
    > "$PLANTED"
out="$(fence)"; rc=$?
isnz "testenv up in a comment does not count as container usage" "$rc"

# ==========================================================================
# --count-undeclared: number or ? when the glob matches nothing.
# ==========================================================================
# Plant the undeclared suite and verify the count is > 0 (it is at least 1).
printf '#!/usr/bin/env bash\necho hello\n' > "$PLANTED"
count="$(fence --count-undeclared)"
# The count is a number; it must be at least 1 (the planted suite).
case "$count" in
    ''|*[!0-9]*) bad "count is a number when there is an undeclared suite" "got [$count]" ;;
    0) bad "count is non-zero when a planted undeclared suite exists" "got 0" ;;
    *) ok "count is a non-zero number when an undeclared suite exists ($count)" ;;
esac

# With the planted suite declared, the count decreases.
printf '#!/usr/bin/env bash\n# host-reason: placeholder reason for counting test\necho hello\n' \
    > "$PLANTED"
count2="$(fence --count-undeclared)"
case "$count2" in
    ''|*[!0-9]*) bad "count is a number after the suite is declared" "got [$count2]" ;;
    *) [ "$count2" -lt "$count" ] \
        && ok "count decreases when the undeclared suite gains a # host-reason: ($count → $count2)" \
        || bad "count decreases when suite is declared" "was $count, now $count2" ;;
esac

# An empty tree (no suites) renders ? rather than 0 (law-absence-needs-a-positive-control).
EMPTY_ROOT="$TMP/empty"; mkdir -p "$EMPTY_ROOT/spira"
cp "$HERE/host-check.sh" "$EMPTY_ROOT/spira/host-check.sh"
git init -q -b main "$EMPTY_ROOT"
git -C "$EMPTY_ROOT" config user.email t@t; git -C "$EMPTY_ROOT" config user.name t
count_empty="$(fence_at "$EMPTY_ROOT" --count-undeclared)"
is "empty tree renders ? not 0" "?" "$count_empty"

# ==========================================================================
# --count-copying: wave-2 migration backlog. Counts suites still copying harness
# files or creating inline stubs. Positive control first, then host-reason exclusion.
# ==========================================================================
COPY_ROOT="$TMP/copy-root"; mkdir -p "$COPY_ROOT/spira"
cp "$HERE/host-check.sh" "$COPY_ROOT/spira/host-check.sh"
git init -q -b main "$COPY_ROOT"
git -C "$COPY_ROOT" config user.email t@t; git -C "$COPY_ROOT" config user.name t

# Empty tree -> ?
is "count-copying: empty tree renders ? not 0" "?" \
   "$(fence_at "$COPY_ROOT" --count-copying)"

# Plant a suite that copies a harness file. The count must report 1.
COPY_SUITE="$COPY_ROOT/spira/test-copy-planted.sh"
printf '#!/usr/bin/env bash\nset -e\ncp "$HERE/lib.sh" "$TMP/"\n' > "$COPY_SUITE"
is "POSITIVE CONTROL: count-copying sees a planted cp suite" "1" \
   "$(fence_at "$COPY_ROOT" --count-copying)"

# A host-reason suite with cp is NOT counted — it is a declared host suite, not wave-2 work.
HR_SUITE="$COPY_ROOT/spira/test-hr-copy.sh"
printf '#!/usr/bin/env bash\n# host-reason: needs host systemd\ncp "$HERE/lib.sh" "$TMP/"\n' > "$HR_SUITE"
is "host-reason suite is excluded from count-copying" "1" \
   "$(fence_at "$COPY_ROOT" --count-copying)"

# Remove the copying suite — count falls to 0 (migration complete for that tree).
rm "$COPY_SUITE"
is "count-copying falls when the suite is migrated" "0" \
   "$(fence_at "$COPY_ROOT" --count-copying)"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
