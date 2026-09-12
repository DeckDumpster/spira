#!/usr/bin/env bash
#
# test-gate-fixture-contamination.sh — gate-spira.sh refuses to land when the production
# database contains test fixture beads.
#
# THE DEFECT SERIES THIS ENDS. Tests that did not isolate their SPIRA_DB wrote beads
# directly into the production store. The markers are unambiguous: external_ref values
# under "fixture-fault:<name>" where <name> is not a real TESTDB_NAME (which always starts
# with "sptest_"), and bodies of exactly "Test payload". Each represents a test whose
# cleanup trap did not run — the live store carries the leak until it is found by hand.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control). The gate-spira.sh fixture
# contamination fence reads the configured SPIRA_DB. Before asserting that a clean database
# passes, we plant an offender in the fixture database, require the gate to name it, and
# only then trust its silence after removal.
#
# DISCRIMINATION: the fence must NOT flag legitimate fixture-fault beads filed by suites.sh.
# Real TESTDB_NAMEs are "sptest_<tag>_<epoch>_<pid>"; the test inserts one and verifies
# the gate does not reject it.
#
# defects: sp-pvoyq, sp-qxxfd, sp-2mo8w, sp-05dvg, sp-ejjkr, sp-ubhqe
# covers: spira/gate-spira.sh spira/gate-fences.sh
# host-reason: testdb provides isolation; gate-spira.sh fixture runs without host systemd.
# scar: the fence cp list (exclude.sh inventory.sh hermetic.sh) omitted `literal-lint.sh` when
# c515322 added it as gate-spira.sh's fourth fence — gate refused with "literal-lint.sh is
# missing" so contamination cases could not be tested; gate_fence_cp derives the fence list
# from gate-spira.sh at call time (sp-i7u). Seen red (6 failures): FAIL gate names the
# contaminating bead id: wanted [sp-...] in [gate: spira/literal-lint.sh is missing — refusing
# to land unchecked]
# shellcheck disable=SC1090
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }

. "$HERE/testdb.sh"
. "$HERE/gate-fences.sh"
testdb_require test-gate-fixture-contamination
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up fixture-contamination || {
    echo "test-gate-fixture-contamination: could not build a fixture database"; exit 1
}

echo "test-gate-fixture-contamination.sh"

B() { bd -C "$SPIRA_DB" "$@"; }
# bead_id_from_ref <external-ref> — find the id of the open bead with the given ref.
bead_id_from_ref() {
    B list --status open,in_progress --limit 0 --json 2>/dev/null \
      | python3 -c "
import sys, json
target = sys.argv[1]
try: d = json.load(sys.stdin)
except Exception: d = []
for b in (d if isinstance(d, list) else []):
    if b.get('external_ref') == target:
        print(b['id']); break
" "$1" 2>/dev/null
}

# THE HARNESS UNDER TEST — a copy of gate-spira.sh and its dependencies in a temp
# directory. gate-spira.sh locates lib.sh, conf.sh etc. from its own path, so a copy
# with those files beside it runs identically to the installed version while drawing on
# the fixture database.
SH="$TMP/spira"
mkdir -p "$SH"
cp "$HERE/gate-spira.sh" "$HERE/suite-covers.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/sop.sh" "$SH/"
gate_fence_cp "$HERE/gate-spira.sh" "$HERE" "$SH"
[ -f "$HERE/inventory-deny" ] && cp "$HERE/inventory-deny" "$SH/"
# A REPO-MAP so _bdq_check_repo_label allows repo:spira if file_budget_bead fires.
printf 'spira | %s\n' "$TMP" > "$SH/repo-map"

# A MINIMAL GIT REPOSITORY that satisfies exclude.sh (which calls `git ls-files`) and
# inventory.sh (which scans tracked files).
git init -q -b main "$TMP" 2>/dev/null || true
printf 'marker\n' > "$TMP/marker.txt"
git -C "$TMP" add marker.txt 2>/dev/null || true
git -C "$TMP" -c user.email=t@t -c user.name=t commit -q -m init 2>/dev/null || true

# A DUMMY SUITE so hermetic.sh finds at least one test-*.sh
# (law-absence-needs-a-positive-control: an empty glob and a clean tree are identical
# from outside). The suite must have a # covers: line or suites.sh reports it as an
# omission.
printf '#!/usr/bin/env bash\n# covers: spira/gate-spira.sh\nset -uo pipefail\nexit 0\n' \
    > "$SH/test-dummy.sh"
chmod +x "$SH/test-dummy.sh"

# A PASSING GATE-SUITES so the gate can complete when no contamination is present.
# The dummy suite above is the only entry; it exits 0, so the gate exits 0.
printf 'spira/test-dummy.sh\n' > "$SH/gate-suites"

# run_gate — invoke gate-spira.sh from $TMP with SPIRA_DB pointing at the fixture
# database. Output is captured; exit status is in gate_rc.
gate_rc=0
GOUT="$TMP/gate-out"
run_gate() {
    (
        unset SPIRA_HOME
        cd "$TMP"
        env -i HOME="$HOME" PATH="$PATH" \
            SPIRA_CONF="/nonexistent.conf" \
            SPIRA_DB="$SPIRA_DB" \
            SPIRA_GATE_BUDGET=9999 \
            bash spira/gate-spira.sh 2>&1
    ) > "$GOUT" 2>&1; gate_rc=$?
}

# --------------------------------------------------------------------------------------
# CASE 1 — POSITIVE CONTROL: gate detects a contaminating bead with a test-only
# external_ref. Plant a bead whose external_ref is "fixture-fault:test_fixture" — the
# literal tag that leaked into production — and require the gate to name it.
# --------------------------------------------------------------------------------------
echo
echo "case 1 — positive control: gate detects fixture-fault external_ref contamination"

B create "test fixture fault" --type task \
    --external-ref "fixture-fault:test_fixture" \
    --body - >/dev/null 2>&1 <<'BODY'
Test payload
BODY
FIXTURE_ID="$(bead_id_from_ref "fixture-fault:test_fixture")"

run_gate; out1="$(cat "$GOUT")"
is   "gate exits 1 with contaminating bead present"              1 "$gate_rc"
want "gate names the contaminating bead id"          "$FIXTURE_ID" "$out1"
want "gate output mentions the external_ref"  "fixture-fault:test_fixture" "$out1"
want "gate explains the finding"              "test beads found"  "$out1"

# --------------------------------------------------------------------------------------
# CASE 2 — BODY MARKER: a bead whose body is exactly "Test payload" is also flagged,
# even when the external_ref gives no hint. Reset the database first so only the body
# bead is present and the assertion is unambiguous.
# --------------------------------------------------------------------------------------
echo
echo "case 2 — body marker: bead with body 'Test payload' is detected"

testdb_reset

B create "innocuous title" --type task \
    --body - >/dev/null 2>&1 <<'BODY'
Test payload
BODY

run_gate; out2="$(cat "$GOUT")"
is   "gate exits 1 with Test-payload body bead present"          1 "$gate_rc"
want "gate mentions body=Test payload"               "body=Test payload" "$out2"

# --------------------------------------------------------------------------------------
# CASE 3 — NEGATIVE CONTROL: with no contaminating beads the gate passes the
# contamination fence. (It may still run suites; the dummy suite exits 0.)
# --------------------------------------------------------------------------------------
echo
echo "case 3 — negative control: clean database passes the contamination fence"

testdb_reset

run_gate; out3="$(cat "$GOUT")"
nowant "gate does not report contamination on a clean database" \
    "test beads found" "$out3"
is "gate exits 0 on a clean database"                           0 "$gate_rc"

# --------------------------------------------------------------------------------------
# CASE 4 — DISCRIMINATION: a legitimate fixture-fault:<sptest_…> bead is NOT flagged.
# Real TESTDB_NAMEs produced by testdb_up start with "sptest_"; the fence must treat
# those as operational data, not contamination.
# --------------------------------------------------------------------------------------
echo
echo "case 4 — discrimination: legitimate sptest_* external_ref not flagged"

B create "shared fixture collapsed — 3 suite(s) could not start" --type task \
    --external-ref "fixture-fault:sptest_mytag_1725100000_99999" \
    --body - >/dev/null 2>&1 <<'BODY'
The shared fixture failed to reset during the timed suite run.
BODY

run_gate; out4="$(cat "$GOUT")"
is   "gate exits 0 with a legitimate sptest_* bead present"     0 "$gate_rc"
nowant "legitimate sptest_* ref not flagged as contamination" \
    "test beads found" "$out4"

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
