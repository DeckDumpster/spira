#!/usr/bin/env bash
#
# test-gate-testdb-diag.sh — gate-spira.sh preserves the testdb_up diagnostic
# when a suite's fixture build fails.
#
# THE DEFECT THIS PREVENTS. When a suite calls testdb_up and it returns
# non-zero (e.g. "fixture lock", "bd init failed"), gate-spira.sh's run()
# must pass the diagnostic through to its own output. A prior version of
# gate-spira.sh suppressed testdb_up's stderr with `>/dev/null 2>&1`, leaving
# the operator to read "see testdb_up's output above" with nothing above it
# (sp-iap9). The fix is the `setsid bash "$s" > "$tmp" 2>&1` capture in run(),
# which collects both streams; this suite proves that capture is live.
#
# POSITIVE CONTROL IS ESSENTIAL (law-absence-needs-a-positive-control).
# Case 1 proves the stub testdb_up actually emits the token before asserting
# gate-spira.sh captures it — a silent or broken stub would produce a false
# all-clear on Case 2.
#
# covers: spira/gate-spira.sh spira/gate-fences.sh
# shellcheck disable=SC1090
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
gone() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] absent from [$3]"; }

command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }

. "$HERE/testdb.sh"
. "$HERE/gate-fences.sh"
testdb_require test-gate-testdb-diag
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up testdbdiag || { echo "  SKIP  could not build fixture database"; exit 77; }

# THE HARNESS UNDER TEST — a copy of gate-spira.sh and its dependencies in a
# temp directory. gate-spira.sh locates lib.sh, conf.sh etc. from its own
# path ($HERE in the script), so a copy with those files beside it runs
# identically to the installed version while drawing on the fixture database.
SH="$TMP/spira"
mkdir -p "$SH"
cp "$HERE/gate-spira.sh" "$HERE/suite-covers.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/sop.sh" \
   "$HERE/schema.sh" "$SH/"
gate_fence_cp "$HERE/gate-spira.sh" "$HERE" "$SH"
[ -f "$HERE/inventory-deny" ] && cp "$HERE/inventory-deny" "$SH/"
printf 'spira | %s\n' "$TMP" > "$SH/repo-map"

# MINIMAL GIT REPO so exclude.sh (git ls-files) and inventory.sh can scan it.
git -C "$TMP" init -q -b main 2>/dev/null || true
printf 'marker\n' > "$TMP/marker.txt"
git -C "$TMP" add marker.txt 2>/dev/null || true
git -C "$TMP" -c user.email=t@t -c user.name=t commit -q -m init 2>/dev/null || true

# DIAGNOSTIC TOKEN — hardcoded in the stub testdb_up below and checked in both
# the positive control and the main assertion. A unique string prevents a match
# against unrelated output.
DIAG_TOKEN="TESTDB_INIT_DIAG_SURVIVES"

# STUB testdb.sh: a minimal stand-in whose testdb_up always fails with the
# known diagnostic. testdb_require is stubbed to never skip (the suite will
# always reach testdb_up); testdb_drop is a no-op. The suite sources this
# from its own directory, not the real testdb.sh.
cat > "$SH/testdb.sh" << 'STUB'
testdb_require() { :; }
testdb_up() {
    printf 'testdb: bd init failed (rc=1): TESTDB_INIT_DIAG_SURVIVES\n' >&2
    return 1
}
testdb_drop() { :; }
STUB

# FAILING SUITE: sources the stub testdb.sh and calls testdb_up, which fails.
# The diagnostic token goes to stderr; gate-spira.sh captures both streams.
cat > "$SH/test-fixture-fail.sh" << 'SUITE'
#!/usr/bin/env bash
# covers: spira/gate-spira.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testdb.sh"
testdb_require test-fixture-fail
testdb_up test-fixture-fail || { printf 'suite: testdb_up returned non-zero\n' >&2; exit 1; }
SUITE
chmod +x "$SH/test-fixture-fail.sh"

# PASSING SUITE for the clean case — no testdb_up, no diagnostic emitted.
cat > "$SH/test-pass.sh" << 'SUITE'
#!/usr/bin/env bash
# covers: spira/gate-spira.sh
set -uo pipefail
exit 0
SUITE
chmod +x "$SH/test-pass.sh"

# HERMETIC.SH POSITIVE CONTROL: at least one test-*.sh with a covers: line so
# hermetic.sh does not silently match an empty glob (law-absence-needs-a-positive-control).
cat > "$SH/test-dummy.sh" << 'SUITE'
#!/usr/bin/env bash
# covers: spira/gate-spira.sh
set -uo pipefail
exit 0
SUITE
chmod +x "$SH/test-dummy.sh"

run_gate() {
    local content="$1"
    printf '%s\n' "$content" > "$SH/gate-suites"
    (
        unset SPIRA_HOME
        cd "$TMP"
        SPIRA_CONF="/nonexistent.conf" SPIRA_DB="$SPIRA_DB" \
            bash spira/gate-spira.sh 2>&1
    )
}

echo "test-gate-testdb-diag.sh — gate-spira.sh preserves testdb_up diagnostic on failure"

# ---- CASE 1: POSITIVE CONTROL -------------------------------------------------------
# The stub must actually emit the diagnostic. A silent or misquoted stub would
# produce a false all-clear on Case 2 — the token would be absent for the wrong reason.
stub_out="$(bash "$SH/test-fixture-fail.sh" 2>&1)"; stub_rc=$?
is   "positive control: suite exits 1 via failing testdb_up"   1              "$stub_rc"
want "positive control: diagnostic token appears in suite output" "$DIAG_TOKEN" "$stub_out"

# ---- CASE 2: THE ASSERTION ----------------------------------------------------------
# gate-spira.sh's run() captures `setsid bash "$s" > "$tmp" 2>&1` and prints $tmp
# on failure. The testdb_up diagnostic (stderr from the suite) must survive that path.
gate_out="$(run_gate "spira/test-fixture-fail.sh")"; gate_rc=$?
is   "gate exits 1 on fixture failure"                          1              "$gate_rc"
want "testdb_up diagnostic appears in gate-spira.sh output"     "$DIAG_TOKEN" "$gate_out"

# ---- CASE 3: CLEAN ------------------------------------------------------------------
# A passing suite must not carry the diagnostic into a passing gate's output.
pass_out="$(run_gate "spira/test-pass.sh")"
gone "diagnostic absent from a passing gate"                    "$DIAG_TOKEN" "$pass_out"

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
