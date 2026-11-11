#!/usr/bin/env bash
#
# test-testdb-testenv-path.sh — testdb_up reaches testenv when the caller's PATH lacks it.
#
# tier: T1
# covers: spira/testdb.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
mkdir -p "$T/rel/bin" "$T/empty" "$T/fx"
# The stub fixture has what a server-mode fixture has: its port file and database name, which
# testdb_up reads to create the read-only SQL user (through TESTDB_DOLT, stubbed below).
mkdir -p "$T/fx/.beads"
echo 1 > "$T/fx/.beads/dolt-server.port"
printf '{"dolt_database": "stub"}\n' > "$T/fx/.beads/metadata.json"
cat > "$T/rel/bin/testenv" <<EOF
#!/bin/sh
echo "\$*" >> "$T/calls"
printf 'TESTDB_NAME=stub\nTESTDB_DIR=$T/fx\nTESTDB_FIXTURE=$T/fx\n'
EOF
chmod +x "$T/rel/bin/testenv"

mkdir -p "$T/tools"
for t in sed head cat tr timeout spira-config; do ln -s "$(command -v $t)" "$T/tools/$t"; done

run_up() {   # run_up <release> — testdb_up under a PATH holding no testenv
    (
        . "$HERE/testdb.sh"
        unset TESTDB_TESTENV TESTDB_SHARED TESTDB_MODE SPIRA_TESTDB_MODE
        PATH="$T/tools"; SPIRA_RELEASE="$1"
        TESTDB_SERVER_BD=/bin/true TESTDB_DOLT=/bin/true TESTDB_OWNER_PID=0 testdb_up tag
    ) 2>&1
}

out="$(run_up "$T/rel")"; rc=$?
is "testdb_up succeeds with testenv only in the release" 0 "$rc"
want "the release testenv was the one called" "testdb " "$(cat "$T/calls" 2>/dev/null)"

: > "$T/calls"
out="$(run_up "$T/nowhere")"; rc=$?
is "testdb_up refuses when no testenv can be resolved" 1 "$rc"
want "the refusal names testenv" "no testenv binary" "$out"
is "nothing was called" "" "$(cat "$T/calls")"

cp "$T/rel/bin/testenv" "$T/tools/testenv"
out="$(run_up "$T/nowhere")"; rc=$?
is "a testenv on the caller's PATH is not used" 1 "$rc"
is "the PATH testenv was not called" "" "$(cat "$T/calls")"
tl_summary
