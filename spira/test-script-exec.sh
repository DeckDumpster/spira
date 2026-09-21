#!/usr/bin/env bash
# test-script-exec.sh — every operator-runnable script in spira/ carries the execute bit.
#
# Sourced-only files (explicitly "Sourced, never executed" in their headers) are
# legitimately non-executable and excluded. test-*.sh suites are also excluded.
# Everything else must have +x so a newly-added operator command cannot ship silent.
#
# covers: spira/*.sh
# selects-on: added,mode
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-script-exec.sh"

SOURCED_ONLY="conf.sh lib.sh testdb.sh suite-assert.sh suite-covers.sh suite-state.sh select-globs.sh gate-fences.sh"

# find_nonexec <dir> — space-separated basenames of *.sh that are not test-*.sh,
# not in SOURCED_ONLY, and lack the execute bit.
find_nonexec() {
    local dir="$1" nonexec="" b
    for f in "$dir"/*.sh; do
        [ -e "$f" ] || continue
        b="$(basename "$f")"
        case "$b" in test-*) continue ;; esac
        for so in $SOURCED_ONLY; do [ "$b" = "$so" ] && continue 2; done
        [ -x "$f" ] || nonexec="$nonexec $b"
    done
    printf '%s' "${nonexec# }"
}

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# ===========================================================================
echo
echo "POSITIVE CONTROL — scanner finds a missing +x when planted:"
# ===========================================================================
FAKE="$TMP/fake"
mkdir -p "$FAKE"

printf '#!/usr/bin/env bash\ntrue\n' > "$FAKE/canary-operator.sh"

found="$(find_nonexec "$FAKE")"
[ -n "$found" ] \
    && ok  "positive control: found offender [$found]" \
    || bad "positive control: found offender" "found nothing — scanner is broken"

# ===========================================================================
echo
echo "POSITIVE CONTROL — sourced-only is not flagged:"
# ===========================================================================
printf '#!/usr/bin/env bash\ntrue\n' > "$FAKE/conf.sh"
found_so="$(find_nonexec "$FAKE")"
is "sourced-only (conf.sh) excluded from scan" "canary-operator.sh" "$found_so"

# ===========================================================================
echo
echo "POSITIVE CONTROL — scanner goes silent after adding +x:"
# ===========================================================================
chmod +x "$FAKE/canary-operator.sh"
found_after="$(find_nonexec "$FAKE")"
is "silent after +x" "" "$found_after"

# ===========================================================================
echo
echo "REAL TREE — no operator scripts missing execute bit:"
# ===========================================================================
offenders="$(find_nonexec "$HERE")"
is "spira/: no non-executable operator scripts" "" "$offenders"

echo
printf 'ASSERTIONS %d\n' $(( pass + fail ))
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
