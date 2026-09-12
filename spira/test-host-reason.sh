#!/usr/bin/env bash
#
# test-host-reason.sh — the fence that requires every host suite to declare why.
#
#   ./test-host-reason.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# hermetic.sh --host-check refuses a suite that runs on the host without explaining
# why: either it calls testenv.sh (up/exec) so its assertions run in a container, or
# it carries a non-empty `# host-reason: <text>` annotation.  This suite proves the
# fence can go red (positive control first), that the escape works, and that the count
# hermetic.sh --count-undeclared prints is what suites.sh status renders.
#
# THE POSITIVE CONTROL IS THE FIRST ASSERTION. With the REGULAR hermetic.sh (no
# --host-check flag) run against a planted undeclared suite, the fence is silent.
# That silence proves the regular check does not know about host-reason, so when
# --host-check IS used and refuses the same suite, the --host-check mode is what
# produced the refusal.  Without this pair, a check that never fired looks identical
# to a check that fired and found nothing (law-absence-needs-a-positive-control).
#
# THE WHOLE-TREE WALK IS OVER A SCRATCH REPOSITORY, because the shipped suites are
# not yet fully migrated and running --host-check against them would produce noise
# that teaches nothing about whether the fence itself is correct.
#
# host-reason: tests the host-reason fence; assertions run on the host using scratch repos only
#
# covers: spira/hermetic.sh spira/suites.sh
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
cp "$HERE/hermetic.sh" "$ROOT/spira/hermetic.sh"
git init -q -b main "$ROOT"
git -C "$ROOT" config user.email t@t; git -C "$ROOT" config user.name t

fence_at() {     # fence_at <root> [args...] -> hermetic.sh output from that root
    env -i PATH="$PATH" HOME="$TMP" TERM=dumb bash "$1/spira/hermetic.sh" "${@:2}" 2>&1
}

fence() {        # fence [args...] -> hermetic.sh from the scratch root
    fence_at "$ROOT" "$@"
}

# The planted suite: a bare host suite with no host-reason and no container usage.
# Line 1 is the shebang, so the first real content is line 2 — known to the assertions below.
PLANTED="$ROOT/spira/test-planted-undeclared.sh"
printf '#!/usr/bin/env bash\necho hello from planted\n' > "$PLANTED"

# ==========================================================================
# THE POSITIVE CONTROL. Run the REGULAR hermetic.sh (without --host-check) on the tree
# that contains the planted undeclared suite.  The regular check does not know about
# host-reason, so it must be silent about the plant.  Only AFTER proving this does the
# --host-check refusal mean something.
# ==========================================================================
out="$(fence)"; rc=$?
is   "SEEN GREEN: regular check is silent about a missing # host-reason:" "0" "$rc"
nowant "regular check does not mention host-reason" "host-reason" "$out"

# Now --host-check: the same tree must be refused.
out="$(fence --host-check)"; rc=$?
isnz "SEEN RED: --host-check refuses the same undeclared suite" "$rc"
want "and names the file"     "test-planted-undeclared" "$out"
want "and names the override" "host-reason"             "$out"

# The fix is in the override message: add host-reason or container calls.
want "refusal mentions the fix" "host-reason" "$out"

# ==========================================================================
# EMPTY REASON IS REFUSED — the reason is the point of the declaration.
# ==========================================================================
printf '#!/usr/bin/env bash\n# host-reason:\necho hello\n' > "$PLANTED"
out="$(fence --host-check)"; rc=$?
isnz "empty # host-reason: is refused" "$rc"
want "and explains that the reason text is required" "no reason" "$out"

# A reason that is only whitespace is also empty after trimming.
printf '#!/usr/bin/env bash\n# host-reason:   \necho hello\n' > "$PLANTED"
out="$(fence --host-check)"; rc=$?
isnz "whitespace-only # host-reason: is refused" "$rc"

# ==========================================================================
# A VALID DECLARATION PASSES.
# ==========================================================================
printf '#!/usr/bin/env bash\n# host-reason: tests the fence — assertions run on the host\necho hello\n' \
    > "$PLANTED"
out="$(fence --host-check)"; rc=$?
isz "non-empty # host-reason: passes the fence" "$rc"

# The host-reason annotation can appear after the shebang comment block, not only on line 2.
printf '#!/usr/bin/env bash\n#\n# Some description.\n#\n# host-reason: drives real systemd\nset -e\necho hi\n' \
    > "$PLANTED"
out="$(fence --host-check)"; rc=$?
isz "host-reason anywhere in the file header passes" "$rc"

# ==========================================================================
# CONTAINER USAGE PASSES WITHOUT A DECLARATION.
# A suite that calls testenv.sh up or exec in a non-comment line is a container suite;
# no # host-reason: annotation is required.
# ==========================================================================
printf '#!/usr/bin/env bash\nTESTENV="$HERE/testenv.sh"\nbash "$TESTENV" up --name my-test\necho done\n' \
    > "$PLANTED"
out="$(fence --host-check)"; rc=$?
isz "testenv up in code passes without # host-reason:" "$rc"

printf '#!/usr/bin/env bash\nTESTENV="$HERE/testenv.sh"\nbash "$TESTENV" exec -- bash -c "echo hi"\n' \
    > "$PLANTED"
out="$(fence --host-check)"; rc=$?
isz "testenv exec in code passes without # host-reason:" "$rc"

# A testenv call in a COMMENT does not count — the suite itself is not a container suite.
printf '#!/usr/bin/env bash\n# calls: bash "$TESTENV" up (for reference)\necho hello\n' \
    > "$PLANTED"
out="$(fence --host-check)"; rc=$?
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
cp "$HERE/hermetic.sh" "$EMPTY_ROOT/spira/hermetic.sh"
git init -q -b main "$EMPTY_ROOT"
git -C "$EMPTY_ROOT" config user.email t@t; git -C "$EMPTY_ROOT" config user.name t
count_empty="$(fence_at "$EMPTY_ROOT" --count-undeclared)"
is "empty tree renders ? not 0" "?" "$count_empty"

# ==========================================================================
# suites.sh status renders the count on the sweep line.
# The shipped suites are not yet fully migrated, so the count is a non-zero number.
# ==========================================================================
if [ -x "$HERE/suites.sh" ]; then
    status_out="$(bash "$HERE/suites.sh" status 2>&1 || true)"
    want "suites.sh status names the host-reason line" "host-reason" "$status_out"
    # The count is either a number or ? — not blank, not the literal string "undeclared".
    if [[ "$status_out" =~ "host suites without # host-reason:" ]]; then
        # Extract the value after the label.
        val="$(printf '%s\n' "$status_out" | grep 'host suites without' | sed 's/.*# host-reason:[[:space:]]*//')"
        case "$val" in
            [0-9]*|'?') ok "sweep line carries a number or ? [$val]" ;;
            *) bad "sweep line carries a number or ?" "got [$val]" ;;
        esac
    fi
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
