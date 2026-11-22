#!/usr/bin/env bash
# tier: T2
# covers: spira-lc/src/work.rs spira/bead.sh spira/runes/pilgrim.md
#
# A pilgrim's file-followup, against a real beads store: the defect is filed, exit 0, and the
# pilgrimage waits on it. The store refuses a bead blocked by its own descendant, so a model of
# it cannot stand in. The Rust test cannot start a store inside cargo's sandbox, so this runs
# its binary outside it.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
BIN="$(cd "$HERE/.." && cargo test -p spira-lc --features integration --no-run --message-format=json 2>/dev/null \
    | jq -r 'select(.profile.test == true and .target.name == "spira-lc" and (.target.kind | index("bin"))) | .executable' | tail -1)"
[ -x "$BIN" ] || { echo "no spira-lc test binary"; exit 1; }

. "$HERE/testlib.sh"
. "$HERE/testdb.sh"
testdb_require pilgrim-followup
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT; trap 'exit 143' INT TERM
testdb_up pilgrim-followup || { echo "could not build a fixture database"; exit 1; }
tl_repo_map "spira | $HERE/.. | push | origin/main | |"
tl_config SPIRA_RUNES_DIR="$HERE/runes"
lc_fix_init "$TMP/lc"
export SPIRA_LC_BIN PATH="$LC_FIX:$HERE:$PATH"

TEST=a_pilgrims_file_followup_files_an_unparented_defect_the_pilgrimage_waits_on
out="$("$BIN" --exact "work::store_tests::$TEST::tglei5n" 2>&1)"; rc=$?
echo "$out" | tail -15
echo "$out" | grep -q "1 passed" || rc=1
exit $rc
