#!/usr/bin/env bash
#
# test-built-elsewhere-reads-run-tree.sh — a test binary compiled in one checkout and run
# from another reads the second checkout's files. Gate build reuse serves binaries built in a
# builder's tree to the gate's tree; one that baked in its build tree's path reads the wrong
# tree, or none.
#
# Takes the units_lint binary testenv has already built and does not build.
#
# tier: T1
# covers: install/tests/units_lint.rs testkit/src/lib.rs gate/src/real.rs
# timeout: 60
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
HARNESS="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"

TEST=exec_lines_carry_no_unescaped_percent_specifier
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

deps="${CARGO_TARGET_DIR:-$HARNESS/target}/debug/deps"
bins=()
for f in "$deps"/units_lint-*; do
    [[ -f "$f" && -x "$f" && "${f##*/}" != *.* ]] && bins+=("$f")
done
if [[ ${#bins[@]} -ne 1 ]]; then
    bad "exactly one built units_lint binary under $deps" "found ${#bins[@]}: ${bins[*]:-}"
    tl_summary
fi
BIN="${bins[0]}"

B="$TMP/b"
mkdir -p "$B/install"
cp -R "$HARNESS/systemd" "$B/systemd"
cp -R "$HARNESS/spira" "$B/spira"

# Env is explicit: nothing ambient decides which tree the binary reads.
run_in() {
    local home="$1"
    (cd "$B/install" && env -i PATH="$PATH" HOME="$TMP" \
        CARGO_MANIFEST_DIR="$B/install" SPIRA_HOME="$home" \
        "$BIN" --exact "$TEST" --test-threads=1 2>&1)
}
run_in_b() { run_in "$B/spira"; }

out="$(run_in_b)"; rc=$?
is "the binary passes against the unplanted second checkout" 0 "$rc"
want "the test ran" "1 passed" "$out"

printf 'ExecStart=/bin/true %%n\n' >"$TMP/line"
unit="$B/systemd/planted-bad-percent.service"
{ printf '[Service]\n'; cat "$TMP/line"; } >"$unit"

out="$(run_in_b)"; rc=$?
wantrc "the binary fails against the second checkout's plant" 101 "$rc"
want "the failure names the planted unit" "planted-bad-percent.service" "$out"

out="$(run_in "$HARNESS/spira")"; rc=$?
is "the same binary follows SPIRA_HOME back to the clean tree" 0 "$rc"

rm -f "$unit"
out="$(run_in_b)"; rc=$?
is "the binary passes again once the plant is removed" 0 "$rc"

tl_summary
