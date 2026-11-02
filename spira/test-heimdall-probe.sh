#!/usr/bin/env bash
#
# test-heimdall-probe.sh — the write probe against a real bd and a write-refusing bd shim.
#
#   ./test-heimdall.sh
#
# The classifier half (`heimdall classify`, the pure function that turns observations into
# firing keys) is test-heimdall-classify.sh, which needs nothing but the heimdall binary and runs
# everywhere. This suite drives a real `bd` against a throwaway Dolt database, because a
# stub for bd has twice made correct callers look broken (law-prefer-the-real-dependency).
# With no server, this suite exits 77 — the automake skip convention, which gate-brain.sh
# names in the gate's output — but ONLY if everything that did run passed. A skip must
# never be able to swallow a failure.
#
# mklog (a synthetic sentinel log in lib.sh's log() format) is needed here too, for the
# wedge()/heal() fixtures below — it is duplicated from test-heimdall-classify.sh rather than
# shared, since testlib.sh suites are sourced standalone with no shared-helper convention.
#
# heimdall.sh is retired into the `heimdall` binary (sp-zpaq0, rewrite wave 5); this suite now
# drives that binary (`command heimdall --home "$SH"`) instead of `bash "$SH/heimdall.sh"`,
# exactly test-dwarf-sweep.sh's own `command dwarf --home ...` pattern for the same reason
# (the wrapper function below is itself named `heimdall`, shadowing the bare binary name).
# Three files share this setup: each heimdall pass costs about a second of real bd round trips,
# so the sections run as test-heimdall.sh, test-heimdall-probe.sh and test-heimdall-restart.sh,
# each on a fixture of its own.
# tier: T2
# covers: heimdall/src/* spira/testlib/heimdall-fixture.sh spira/conf.sh UC-ops-detection-remediation-19
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

command -v heimdall >/dev/null 2>&1 \
    || { echo "test-heimdall: heimdall is not on PATH" >&2; exit 1; }


TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

echo "test-heimdall-probe.sh"

. "$HERE/testdb.sh"
if ! testdb_available; then
    printf 'SKIP test-heimdall: no bd engine available — the reconcile cases need a real bd.\n' \
        >&2
    printf '  embedded: install bd-embedded  server: testenv testdb\n' \
        >&2
    skip "no bd engine available"
fi
testdb_up heimdall_probe || { echo "test-heimdall: could not build a fixture database"; exit 1; }

. "$HERE/testlib/heimdall-fixture.sh"

echo
echo "heimdall.sh — write probe detects a write-only database failure:"

# Build a write-fail shim. The seam SPIRA_BD exists for exactly this: a bd that fails
# in a way the real embedded database cannot be asked to reproduce on demand (the write-
# only failure that happens when a schema cursor is rolled back). The shim delegates
# reads to the real binary; writes return error. This isolates the write path without
# modelling bd's surface — reads still go through the real engine.
WRITE_FAIL_BD="$TMP/write-fail-bd"
{
    printf '#!/usr/bin/env bash\n'
    printf '# Passes reads through to the real embedded binary; fails writes.\n'
    printf '# Simulates a schema-cursor write failure (reads ok, writes refused).\n'
    printf 'case "${3:-}" in\n'
    printf '    list|show|migrate) exec %q "$@" ;;\n' "$TESTDB_BD"
    printf '    *) printf "write-fail-bd: write refused (simulating schema-skew write failure)\\n" >&2; exit 1 ;;\n'
    printf 'esac\n'
} > "$WRITE_FAIL_BD"
chmod +x "$WRITE_FAIL_BD"

# A healthy loop — no conditions firing. With a working database, the write probe
# creates its bead and publishes SP_HEIMDALL_DB_WRITE=ok.
heal; rm -f "$RUN/heimdall.state"   # clear state so probe starts fresh
tl_config SPIRA_BD="$TESTDB_BD"
heimdall >/dev/null
grep -q 'SP_HEIMDALL_DB_WRITE=ok' "$RUN/heimdall.status" \
    && ok "write probe: healthy database shows write ok" \
    || bad "write probe baseline" "SP_HEIMDALL_DB_WRITE is not ok before the fault"
grep -q 'SP_HEIMDALL_DB_READ=ok' "$RUN/heimdall.status" \
    && ok "write probe: read path also ok at baseline" \
    || bad "write probe baseline read" "SP_HEIMDALL_DB_READ is not ok before the fault"
[ ! -e "$RUN/heimdall.alerts.json" ] \
    && ok "write probe: no fallback file when writes are healthy" \
    || bad "write probe baseline fallback" "fallback file exists when it should not"

# Now switch to the write-fail shim. Reads succeed, writes fail.
# The write probe detects the failure and the fallback file appears.
rm -f "$RUN/heimdall.state"   # clear state so probe tries to create (which will fail)
tl_config SPIRA_BD="$WRITE_FAIL_BD"
heimdall >/dev/null
grep -q 'SP_HEIMDALL_DB_READ=ok' "$RUN/heimdall.status" \
    && ok "write probe: read path still shows ok during write-only failure" \
    || bad "write probe read during fault" "SP_HEIMDALL_DB_READ is not ok with writes broken"
grep -q 'SP_HEIMDALL_DB_WRITE=down' "$RUN/heimdall.status" \
    && ok "write probe: SP_HEIMDALL_DB_WRITE=down when writes fail" \
    || bad "write probe fault" "SP_HEIMDALL_DB_WRITE is not down with writes broken"
[ -r "$RUN/heimdall.alerts.json" ] \
    && ok "write probe: fallback file written when write path fails" \
    || bad "write probe fallback" "no fallback file when write probe failed"
# Reads are fine so db_reachable=1; the fallback reflects write failure, not read failure.
fb_write="$(python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
print(d.get("db_reachable"), d.get("db_write_ok"))' "$RUN/heimdall.alerts.json" 2>/dev/null)"
[ "$fb_write" = "True False" ] \
    && ok "write probe: fallback carries db_reachable=True and db_write_ok=False" \
    || bad "write probe fallback content" "got [$fb_write]"

# Restore the healthy database. The fallback file is removed and writes are ok again.
tl_config SPIRA_BD="$TESTDB_BD"
heimdall >/dev/null
grep -q 'SP_HEIMDALL_DB_WRITE=ok' "$RUN/heimdall.status" \
    && ok "write probe: write ok restored after the fault clears" \
    || bad "write probe restore" "SP_HEIMDALL_DB_WRITE is not ok after restoring the database"
[ ! -e "$RUN/heimdall.alerts.json" ] \
    && ok "write probe: fallback file removed once writes succeed again" \
    || bad "write probe fallback removal" "fallback file survived the database recovering"

echo
echo "heimdall.sh — write probe: failed re-derive does not create a bead (Fix 1):"

# A shim that passes the alert-list read through (so db_reachable=1) but fails the
# probe re-derive. Without Fix 1 the code created a new P0 bead every time the re-derive
# failed, because a failed call and an empty result were indistinguishable.
REDERIVE_FAIL_BD="$TMP/rederive-fail-bd"
{
    printf '#!/usr/bin/env bash\n'
    # The alert probe uses --label alert; the re-derive uses --label heimdall:probe.
    # Pass alert reads to the real binary so db_reachable stays 1; fail probe reads.
    printf 'for a in "$@"; do case "$a" in\n'
    printf '    "heimdall:probe") printf "rederive-fail-bd: probe list refused\\n" >&2; exit 1 ;;\n'
    printf 'esac; done\n'
    printf 'exec %q "$@"\n' "$TESTDB_BD"
} > "$REDERIVE_FAIL_BD"
chmod +x "$REDERIVE_FAIL_BD"

heal; rm -f "$RUN/heimdall.state"   # empty state: PROBE_ID empty, re-derive will run
_n_probes_before="$(n_probe_beads)"
tl_config SPIRA_BD="$REDERIVE_FAIL_BD"
heimdall >/dev/null
_n_probes_after="$(n_probe_beads)"
[ "$_n_probes_after" = "$_n_probes_before" ] \
    && ok "re-derive fail: no probe bead created when re-derive fails" \
    || bad "re-derive fail" "probe bead was leaked when re-derive failed (before=$_n_probes_before after=$_n_probes_after)"
unset _n_probes_before _n_probes_after
grep -q 'SP_HEIMDALL_DB_WRITE=down' "$RUN/heimdall.status" \
    && ok "re-derive fail: write reported as down when re-derive fails" \
    || bad "re-derive fail write status" "SP_HEIMDALL_DB_WRITE should be down when re-derive failed"

echo
echo "heimdall.sh — write probe: extras closed on re-derive (Fix 3):"

# Pre-populate three probe beads (simulating the debris from past contention episodes).
# After one run with empty state, all but the first should be closed.
heal; rm -f "$RUN/heimdall.state"
_extras_before="$(n_probe_beads)"   # track pre-existing probes; test counts relative to this
_p1="$(create_probe)"; _p2="$(create_probe)"; _p3="$(create_probe)"
if [ -z "$_p1" ] || [ -z "$_p2" ] || [ -z "$_p3" ]; then
    bad "extras setup" "could not create three probe beads for extras test"
else
    tl_config SPIRA_BD="$TESTDB_BD"
    heimdall >/dev/null
    # Exactly 1 probe bead should be open; all others (pre-existing + 2 of the 3 new ones)
    # should be closed rather than deleted.
    n_open="$(timeout 5 bd -C "$SPIRA_DB" list --limit 0 --label heimdall:probe --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: d = []
print(len(d if isinstance(d, list) else [d]))')"
    is "extras: only one probe bead is open after adoption" 1 "$n_open"
    _expected_total=$(( _extras_before + 3 ))
    [ "$(n_probe_beads)" = "$_expected_total" ] \
        && ok "extras: all beads still exist (closed, not deleted)" \
        || bad "extras count" "expected $_expected_total total, got [$(n_probe_beads)]"
    grep -q 'SP_HEIMDALL_DB_WRITE=ok' "$RUN/heimdall.status" \
        && ok "extras: probe update succeeds after adoption" \
        || bad "extras write" "SP_HEIMDALL_DB_WRITE not ok after adoption"
fi
unset _p1 _p2 _p3 n_open _extras_before _expected_total

echo
echo "heimdall.sh — write probe: lock-timeout reported as saturated, not down (Fix 5):"

# A shim that exits 124 (the timeout exit code) for all calls. This simulates the lock
# contention that drove half of all passes to report DB_READ=down on a healthy database.
TIMEOUT_BD="$TMP/timeout-bd"
{
    printf '#!/usr/bin/env bash\n'
    printf 'exit 124\n'
} > "$TIMEOUT_BD"
chmod +x "$TIMEOUT_BD"

heal; rm -f "$RUN/heimdall.state"
tl_config SPIRA_BD="$TIMEOUT_BD"
HEIMDALL_DB=/nonexistent-spira-db heimdall >/dev/null
grep -q 'SP_HEIMDALL_DB_READ=saturated' "$RUN/heimdall.status" \
    && ok "saturation: timeout on read probe reports DB_READ=saturated" \
    || bad "saturation read" "expected SP_HEIMDALL_DB_READ=saturated, got $(grep SP_HEIMDALL_DB_READ "$RUN/heimdall.status" 2>/dev/null || echo none)"
grep -q 'SP_HEIMDALL_DB_SATURATED=1' "$RUN/heimdall.status" \
    && ok "saturation: SP_HEIMDALL_DB_SATURATED=1 when timed out" \
    || bad "saturation flag" "SP_HEIMDALL_DB_SATURATED not 1"
# A timeout is still unreachable as far as the rest of the pass is concerned, so the
# fallback file should still be written (same as the db-down case).
[ -r "$RUN/heimdall.alerts.json" ] \
    && ok "saturation: fallback file written on timeout" \
    || bad "saturation fallback" "no fallback file on timeout"

# Reset SPIRA_BD back to the real store: it stays declared in the override file (there is
# no implicit per-call env scoping any more) until something changes it again, so every
# bare `heimdall`/`heimdall_restart` call below would otherwise keep hitting the 124-exit stub.
tl_config SPIRA_BD="$TESTDB_BD"

testdb_drop >/dev/null 2>&1
tl_summary
