#!/usr/bin/env bash
#
# test-testdb-testenv-path.sh — testdb.sh finds testenv without the caller's PATH.
#
# tier: T1
# covers: spira/testdb.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
mkdir -p "$T/rel/bin" "$T/empty"
printf '#!/bin/sh\necho STUB\n' > "$T/rel/bin/testenv"; chmod +x "$T/rel/bin/testenv"

got="$(env -i PATH="$PATH" HOME="$T" SPIRA_RELEASE="$T/rel" "$BASH" -c \
    'unset TESTDB_TESTENV; . "$1"; PATH="$2"; _testdb_testenv' _ "$HERE/testdb.sh" "$T/empty" 2>/dev/null)"
is "release testenv found with testenv absent from PATH" "$T/rel/bin/testenv" "$got"

got="$(env -i PATH="$PATH" HOME="$T" "$BASH" -c \
    'unset TESTDB_TESTENV SPIRA_RELEASE; . "$1"; PATH="$2"; _testdb_testenv' _ "$HERE/testdb.sh" "$T/empty" 2>/dev/null)"
is "no testenv anywhere resolves to nothing, not a bare name" "" "$got"
tl_summary
