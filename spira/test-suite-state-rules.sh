#!/usr/bin/env bash
# test-suite-state-lint.sh — `testenv suites lint` and the fence over a throwaway suite-state
# file: each structural violation is refused, bad lines never reach the rows, and an empty or
# absent file is clean.
# tier: T1
# requires: testenv
# covers: spira/suite-state-fence.sh spira/suite-state testenv/src/suites/* UC-test-infrastructure-32
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

command -v testenv >/dev/null 2>&1 || bail "testenv is not on PATH"

SD="$(mktemp -d)"
trap 'rm -rf "$SD"' EXIT INT TERM
mkdir -p "$SD/spira"
cp "$HERE/suite-state-fence.sh" "$SD/spira/"
printf '#!/usr/bin/env bash\nexit 0\n' > "$SD/spira/test-fake-suite.sh"
chmod +x "$SD/spira/test-fake-suite.sh"
STATE="$SD/spira/suite-state"

lint() { SPIRA_TESTENV_HARNESS="$SD" testenv suites lint 2>"$SD/err"; }
row() { printf '%s\n' "$1" > "$STATE"; }

rm -f "$STATE"
out="$(lint)"; rc=$?
wantrc "absent file: exits 0"                         0 "$rc"
is     "absent file: no rows"                         "" "$out"

: > "$STATE"
out="$(lint)"; rc=$?
wantrc "empty file: exits 0"                          0 "$rc"
is     "empty file: no rows"                          "" "$out"
fence="$(SPIRA_TESTENV_HARNESS="$SD" bash "$SD/spira/suite-state-fence.sh" 2>&1)"; rc=$?
wantrc "empty file: the fence passes"                 0 "$rc"

row 'test-fake-suite.sh | disabled | 2026-01-01T00:00:00Z | | too slow'
out="$(lint)"; rc=$?
wantrc "well-formed row: exits 0"                     0 "$rc"
is     "well-formed row: one tab-separated row"       "$(printf 'test-fake-suite.sh\tdisabled\t2026-01-01T00:00:00Z\t\ttoo slow')" "$out"

row 'test-fake-suite.sh | quarantined | 2026-01-01T00:00:00Z | | flaky'
lint >/dev/null; rc=$?
wantrc "quarantine with no bead: exits 1"             1 "$rc"
want   "quarantine with no bead: says so"             "has no bead id" "$(cat "$SD/err")"

row 'test-fake-suite.sh | disabled | 2026-01-01T00:00:00Z | | '
lint >/dev/null; rc=$?
wantrc "row with no reason: exits 1"                  1 "$rc"
want   "row with no reason: says so"                  "missing reason" "$(cat "$SD/err")"

row 'test-missing-suite.sh | disabled | 2026-01-01T00:00:00Z | | gone'
lint >/dev/null; rc=$?
wantrc "row for a missing suite: exits 1"             1 "$rc"
want   "row for a missing suite: names it"            "test-missing-suite.sh" "$(cat "$SD/err")"
fence="$(SPIRA_TESTENV_HARNESS="$SD" bash "$SD/spira/suite-state-fence.sh" 2>&1)"; rc=$?
wantrc "the fence refuses the same file"              1 "$rc"

printf 'garbage line\ntest-fake-suite.sh | weird | 2026-01-01T00:00:00Z | | r\n' > "$STATE"
out="$(lint)"; rc=$?
wantrc "bad lines: exits 1"                           1 "$rc"
want   "unparseable line is reported"                 "not parseable" "$(cat "$SD/err")"
want   "unknown state is reported"                    "unknown state weird" "$(cat "$SD/err")"
is     "bad lines are ignored in the rows"            "" "$out"

tl_summary
