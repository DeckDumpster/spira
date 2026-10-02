#!/usr/bin/env bash
# test-covers-seam-closure.sh — a suite that covers the landing-pass seam also covers the
# shell it execs: seam.rs shells out to lib.sh functions, so a change there must select it.
# tier: T1
# covers: spira/test-*.sh landing-pass/src/seam.rs spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
ROOT="$(dirname "$HERE")"

seam_closure_gaps() {  # seam_closure_gaps <suites-dir> -> suites covering the seam but not lib.sh
    local f g covers seam lib
    for f in "$1"/test-*.sh; do
        covers="$(sed -n '/^set -/q;s/^# *covers: *//p' "$f" | head -1)"
        seam=0; lib=0
        set -f
        for g in $covers; do
            [[ "landing-pass/src/seam.rs" == $g ]] && seam=1
            [[ "spira/lib.sh" == $g ]] && lib=1
        done
        set +f
        [ "$seam" = 1 ] && [ "$lib" = 0 ] && basename "$f"
    done
}

grep -q 'spira_git_push' "$ROOT/landing-pass/src/seam.rs" \
    || { echo "  FAIL  seam.rs no longer execs spira_git_push; revisit this lint"; exit 1; }

PLANT="$(mktemp -d)"; trap 'rm -rf "$PLANT"' EXIT INT TERM
printf '#!/usr/bin/env bash\n# covers: landing-pass/*\nset -u\n' > "$PLANT/test-planted.sh"
[ "$(seam_closure_gaps "$PLANT")" = "test-planted.sh" ] \
    && echo "  PASS  planted seam-only suite is flagged" \
    || { echo "  FAIL  matcher missed the planted offender"; exit 1; }

gaps="$(seam_closure_gaps "$HERE")"
if [ -z "$gaps" ]; then echo "  PASS  every seam-covering suite covers spira/lib.sh"
else echo "  FAIL  cover landing-pass but not spira/lib.sh: $gaps"; exit 1; fi
