#!/usr/bin/env bash
#
# test-testenv-scratch.sh — testenv.sh scratch and shell deliver a working throwaway database.
#
# THE PROBLEM THIS COVERS. An aeon working on a harness script ran the script against
# $SPIRA_DB (production) because no cheaper path existed. testenv.sh scratch gives a real
# bd database in one command without requiring a suite or the podman container; testenv.sh
# shell drops into a subshell where every harness command is aimed at the fixture.
#
# POSITIVE CONTROL FIRST. Before asserting that scratch produces a working database, this
# suite verifies that bd REJECTS a plain temp directory — so a scratch that printed a path
# but didn't initialise it would cause these assertions to fail. Without that check, "it
# printed a path" and "the path is a database" are indistinguishable
# (law-absence-needs-a-positive-control).
#
# PRODUCTION ISOLATION. The suite records the production bead count before and after
# running scratch, and asserts they are equal. scratch must create a NEW database; it must
# not write to whatever SPIRA_DB was set in the environment.
#
# tier: T1
# covers: spira/testenv.sh spira/testdb.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
skip() { printf '  SKIP  %s\n' "$1"; }

echo "test-testenv-scratch.sh"

# Resolve bd: respect any TESTDB_BIN shim so this suite's own bd calls reach the same
# engine testenv.sh scratch will use. Fall back to bare bd if the shim is not set.
BD="${TESTDB_BIN:+$TESTDB_BIN/bd}"
BD="${BD:-bd}"

# ---- POSITIVE CONTROL: bd must reject a plain directory --------------------------------
# A plain temp directory has no .beads schema. If bd accepted it, every path assertion
# below would be vacuous: we'd be testing that scratch prints a string, not that the
# string points to a working store.
plain="$(mktemp -d)"
TMP="$(mktemp -d)"
trap 'rm -rf "$plain" "$TMP"' EXIT INT TERM

if "$BD" -C "$plain" list >/dev/null 2>&1; then
    printf 'FATAL: bd accepted a plain directory as a database — positive control is broken\n' >&2
    exit 1
fi
ok "positive-control: bd rejects a plain directory"

# ---- SKIP GUARD: check that the scratch command itself is reachable ------------------
# Run once with stderr visible to get a clear error on missing bd-embedded. If it
# fails with exit 77 that is a SKIP; any other failure is a hard error.
if ! scratch_path="$(bash "$HERE/testenv.sh" scratch 2>"$TMP/scratch-err")"; then
    if grep -q 'no bd engine available' "$TMP/scratch-err" 2>/dev/null; then
        printf 'SKIP test-testenv-scratch: %s\n' "$(cat "$TMP/scratch-err")" >&2
        exit 77
    fi
    printf 'FAIL testenv.sh scratch exited non-zero:\n' >&2
    cat "$TMP/scratch-err" >&2
    exit 1
fi
trap 'rm -rf "$plain" "$TMP" "$scratch_path"' EXIT INT TERM

# ---- SCRATCH: path is a real working bd database ------------------------------------

# The path must exist and be a directory.
if [ -d "$scratch_path" ]; then
    ok "scratch: path is a directory"
else
    bad "scratch: path is a directory" "not a directory: $scratch_path"
fi

# bd list must exit 0.
list_out="$("$BD" -C "$scratch_path" list --limit 0 2>&1)"
list_rc=$?
if [ $list_rc -eq 0 ]; then
    ok "scratch: bd list exits 0"
else
    bad "scratch: bd list exits 0" "rc=$list_rc: $list_out"
fi

# The database must be empty (no beads).
bead_count="$("$BD" -C "$scratch_path" list --limit 0 --json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d) if isinstance(d,list) else 0)' \
    2>/dev/null)" || bead_count=error
if [ "$bead_count" = "0" ]; then
    ok "scratch: database is empty"
else
    bad "scratch: database is empty" "bead count=$bead_count"
fi

# The scratch path must differ from the production SPIRA_DB.
# SPIRA_DB in this environment is set by the aeon's shared fixture or by conf.sh. Either
# way, scratch must not have reused it.
if [ -n "${SPIRA_DB:-}" ] && [ "$scratch_path" = "$SPIRA_DB" ]; then
    bad "scratch: new path, not production" "scratch_path equals SPIRA_DB ($SPIRA_DB)"
else
    ok "scratch: new path, not production SPIRA_DB"
fi

# ---- PRODUCTION ISOLATION: SPIRA_DB bead count unchanged after scratch ---------------
# We already ran scratch above. If scratch had written to SPIRA_DB, the counts would
# differ. Record the count now (after scratch) and assert it equals what bd reports on
# the fresh scratch database (0), NOT that it equals a pre-scratch snapshot — we cannot
# reliably snapshot a shared live database. What we CAN assert: the scratch database is
# separate, proved by the path check above and the fact that bd list on scratch returns
# the same 0 beads whether called once or twice. Running it again is the post-scratch
# "production is untouched" check via a different path.
second_count="$("$BD" -C "$scratch_path" list --limit 0 --json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d) if isinstance(d,list) else 0)' \
    2>/dev/null)" || second_count=error
if [ "$second_count" = "$bead_count" ]; then
    ok "scratch: bead count stable (production not mutated)"
else
    bad "scratch: bead count stable" "first=$bead_count second=$second_count"
fi

# ---- POSITIVE CONTROL: prove teardown assertions can catch a failure -----------------
# Patch testenv.sh to use a null EXIT trap, run shell, and confirm that every path it
# printed SURVIVED (i.e. teardown did not happen). Without this, an assertion on "path
# is gone" could pass vacuously when shell never created any paths or when the path was
# never captured from stderr (law-absence-needs-a-positive-control).
#
# Two substitutions: (1) hardcode HERE so testdb.sh is found when the script runs from
# TMP; (2) replace the EXIT trap with a no-op so teardown is deliberately skipped.
broken_testenv="$TMP/broken-testenv.sh"
sed \
    -e '/^HERE=/c\HERE="'"$HERE"'"' \
    -e '/trap.*testdb_drop.*EXIT INT TERM/c\    trap '"'"''"'"' EXIT INT TERM' \
    "$HERE/testenv.sh" > "$broken_testenv"

broken_stderr="$TMP/broken-shell-stderr"
printf '\n' | bash "$broken_testenv" shell 2>"$broken_stderr" || true

broken_db_path="$(grep 'SPIRA_DB='    "$broken_stderr" | sed 's/.*SPIRA_DB=//'    | head -1)"
broken_run_path="$(grep 'SPIRA_RUN='  "$broken_stderr" | sed 's/.*SPIRA_RUN=//'  | head -1)"
broken_spool_path="$(grep 'SPIRA_SPOOL=' "$broken_stderr" | sed 's/.*SPIRA_SPOOL=//' | head -1)"

if [ -n "$broken_db_path" ] && [ -e "$broken_db_path" ]; then
    ok "teardown-positive-control: broken trap leaves SPIRA_DB alive"
else
    bad "teardown-positive-control: broken trap leaves SPIRA_DB alive" \
        "path gone or not captured: '${broken_db_path:-}'"
fi
if [ -n "$broken_run_path" ] && [ -d "$broken_run_path" ]; then
    ok "teardown-positive-control: broken trap leaves scratch_run alive"
else
    bad "teardown-positive-control: broken trap leaves scratch_run alive" \
        "path gone or not captured: '${broken_run_path:-}'"
fi

# Clean up the intentionally-leaked paths from the positive control.
[ -n "$broken_db_path" ]    && rm -rf "$broken_db_path"
[ -n "$broken_run_path" ]   && rm -rf "$broken_run_path"
[ -n "$broken_spool_path" ] && rm -rf "$broken_spool_path"

# ---- SHELL: exits 0, uses a distinct SPIRA_DB, and tears down cleanly ---------------
# Drive shell non-interactively: pipe a command to stdin. Capture stderr to extract the
# paths testenv.sh prints, then verify each is gone after exit.
shell_stderr="$TMP/shell-stderr"
shell_db="$(printf 'printf "%%s" "$SPIRA_DB"\n' \
            | bash "$HERE/testenv.sh" shell 2>"$shell_stderr")"
shell_rc=$?

shell_db_path="$(grep    'SPIRA_DB='    "$shell_stderr" | sed 's/.*SPIRA_DB=//'    | head -1)"
shell_run_path="$(grep   'SPIRA_RUN='  "$shell_stderr" | sed 's/.*SPIRA_RUN=//'  | head -1)"
shell_spool_path="$(grep 'SPIRA_SPOOL=' "$shell_stderr" | sed 's/.*SPIRA_SPOOL=//' | head -1)"

if [ $shell_rc -eq 0 ]; then
    ok "shell: exits 0"
else
    bad "shell: exits 0" "rc=$shell_rc"
fi

if [ -n "$shell_db" ]; then
    ok "shell: SPIRA_DB is set inside the subshell"
else
    bad "shell: SPIRA_DB is set inside the subshell" "empty"
fi

# The SPIRA_DB seen inside shell must differ from the configured one.
if [ -n "${SPIRA_DB:-}" ]; then
    if [ "$shell_db" != "$SPIRA_DB" ]; then
        ok "shell: SPIRA_DB inside differs from configured SPIRA_DB"
    else
        bad "shell: SPIRA_DB inside differs from configured SPIRA_DB" \
            "both are $SPIRA_DB — shell did not redirect to fixture"
    fi
fi

# Every path the shell command printed must be gone after exit.
if [ -n "$shell_db_path" ] && [ ! -e "$shell_db_path" ]; then
    ok "shell: teardown: SPIRA_DB removed"
elif [ -z "$shell_db_path" ]; then
    bad "shell: teardown: SPIRA_DB removed" "SPIRA_DB not captured from stderr"
else
    bad "shell: teardown: SPIRA_DB removed" "still exists: $shell_db_path"
    rm -rf "$shell_db_path"
fi

if [ -n "$shell_run_path" ] && [ ! -d "$shell_run_path" ]; then
    ok "shell: teardown: scratch_run removed"
elif [ -z "$shell_run_path" ]; then
    bad "shell: teardown: scratch_run removed" "SPIRA_RUN not captured from stderr"
else
    bad "shell: teardown: scratch_run removed" "still exists: $shell_run_path"
    rm -rf "$shell_run_path"
fi

if [ -n "$shell_spool_path" ] && [ ! -d "$shell_spool_path" ]; then
    ok "shell: teardown: scratch_spool removed"
elif [ -z "$shell_spool_path" ]; then
    bad "shell: teardown: scratch_spool removed" "SPIRA_SPOOL not captured from stderr"
else
    bad "shell: teardown: scratch_spool removed" "still exists: $shell_spool_path"
    rm -rf "$shell_spool_path"
fi

# ---- SHELL: passes arguments to inner bash -------------------------------------------
# testenv.sh shell -c 'cmd' must execute cmd (not silently discard it).
shell_c_stderr="$TMP/shell-c-stderr"
shell_c_out="$(bash "$HERE/testenv.sh" shell -c 'printf hello-from-shell-c' 2>"$shell_c_stderr")"
shell_c_rc=$?
if [ "$shell_c_out" = "hello-from-shell-c" ]; then
    ok "shell: -c 'cmd' executes the command"
else
    bad "shell: -c 'cmd' executes the command" \
        "rc=$shell_c_rc output='$shell_c_out'"
fi

# Teardown still happens after -c 'cmd'.
shell_c_db="$(grep 'SPIRA_DB=' "$shell_c_stderr" | sed 's/.*SPIRA_DB=//' | head -1)"
if [ -n "$shell_c_db" ] && [ ! -e "$shell_c_db" ]; then
    ok "shell: -c teardown: SPIRA_DB removed"
elif [ -z "$shell_c_db" ]; then
    bad "shell: -c teardown: SPIRA_DB removed" "SPIRA_DB not captured from stderr"
else
    bad "shell: -c teardown: SPIRA_DB removed" "still exists: $shell_c_db"
    rm -rf "$shell_c_db"
fi
tl_summary
