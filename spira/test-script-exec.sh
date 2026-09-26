#!/usr/bin/env bash
# test-script-exec.sh — every operator-runnable script in spira/ carries the execute bit.
#
# A file whose header declares "Sourced, never executed" is legitimately
# non-executable and excluded (law-fail-closed-at-the-source: the header is the
# declaration, so no separate list can drift from it). test-*.sh suites are also
# excluded. Everything else must have +x so a newly-added operator command cannot
# ship silent.
#
# tier: T1
# covers: spira/*.sh
# selects-on: added,mode
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-script-exec.sh"

# declares_sourced_only <file> — true if the header (first 10 lines) declares
# "Sourced, never executed".
declares_sourced_only() {
    head -10 "$1" 2>/dev/null | grep -qF 'Sourced, never executed'
}

# find_nonexec <dir> — space-separated basenames of *.sh that are not test-*.sh,
# do not declare themselves sourced-only, and lack the execute bit.
find_nonexec() {
    local dir="$1" nonexec="" b
    for f in "$dir"/*.sh; do
        [ -e "$f" ] || continue
        b="$(basename "$f")"
        case "$b" in test-*) continue ;; esac
        declares_sourced_only "$f" && continue
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
printf '# fake-lib.sh — a helper.\n# Sourced, never executed.\ntrue\n' > "$FAKE/fake-lib.sh"
found_so="$(find_nonexec "$FAKE")"
is "sourced-only header (fake-lib.sh) excluded from scan" "canary-operator.sh" "$found_so"

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
tl_summary
