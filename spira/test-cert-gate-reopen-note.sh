#!/usr/bin/env bash
#
# test-cert-gate-reopen-note.sh — reopen note for literal-lint failure carries the offender.
#
# literal-lint.sh prints offending file:line first, then an 18-line explanation block.
# The gate appends 2 trailer lines. With 21 total lines, tail -20 drops line 1 — the
# offender — leaving a note that says "the lines above contain a configured label name"
# with no lines above it.
#
# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail)
# Run against the unfixed literal-lint.sh (before sp-ejdiu):
#   FAIL — offender in tail-20 window: expected [1] got [0]
#
# tier: T1
# covers: spira/literal-lint.sh UC-gate-diag-01
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-cert-gate-reopen-note.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# Build a scratch repo with exactly one offending file so literal-lint produces one
# offender line followed by its full explanation block.
ROOT="$TMP/root"
mkdir -p "$ROOT/spira"
cp "$HERE/literal-lint.sh" "$ROOT/spira/literal-lint.sh"
git init -q -b main "$ROOT"
git -C "$ROOT" config user.email t@t
git -C "$ROOT" config user.name t
printf '#!/usr/bin/env bash\nSOME="${SPIRA_CI_LABEL:-awaiting-ci}"\n' \
    > "$ROOT/spira/one-offender.sh"
git -C "$ROOT" add .
git -C "$ROOT" commit -q -m "one offender"

# Capture stdout+stderr — the same way gate-spira.sh does it.
lint_out="$(env -i PATH="$PATH" HOME="$TMP" TERM=dumb \
    bash "$ROOT/spira/literal-lint.sh" 2>&1 || true)"

# Gate-spira appends two trailer lines after the fence output.
gate_out="$(printf '%s\ngate: the same command passes against origin/main\ngate: VERDICT=FAIL reason=literal-lint suite=-\n' "$lint_out")"

# The reopen note's window comes from landing.sh's own reopen-note line — a bare
# `$(printf '%s' "$gate_out" | tail -N)"` at column 1, distinct from the `tail -3`
# breadcrumb lines nearby — not a copy of N: a change to the window there must change
# what this test asserts against.
window="$(grep -oE '^\$\(printf .%s. "\$gate_out" \| tail -[0-9]+\)"$' "$HERE/landing.sh" \
    | head -1 | grep -oE '[0-9]+')"
[ -n "$window" ] || { echo "test-cert-gate-reopen-note.sh: no reopen-note tail -N found in landing.sh" >&2; exit 1; }
note="$(printf '%s' "$gate_out" | tail -"$window")"

# The offender line matches ^[^ ]+:[0-9]+: — must appear in the note.
count="$(printf '%s' "$note" | grep -cE '^[^ ]+:[0-9]+: ' || true)"
is "offender in tail-20 window" "1" "$count"

tl_summary
