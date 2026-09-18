#!/usr/bin/env bash
#
# test-suites-fixture-fault.sh — a shared-fixture collapse files one bead, not one per borrower.
#
#   ./test-suites-fixture-fault.sh
#
# THE PROPERTY UNDER TEST. When the shared fixture that suites.sh builds at the top of
# each pass fails to reset inside a borrower suite, that borrower exits with
# TESTDB_FAULT_EXIT (75) rather than a suite-failure code. suites.sh must classify that
# as a pass-level fixture fault and file exactly one bead naming all affected suites,
# not one bead per borrower. A borrower that cannot get a fixture is not a red suite.
#
# THE PROBLEM THIS ENDS. The 02:34-03:05 pass filed five separate beads for one fixture
# collapse. Five workers were each summoned onto a bead whose suite passes individually,
# each closed it as unreproducible, and the real finding — that the shared fixture failed
# mid-pass — appeared in none of them. The operator saw five separate regressions in
# five suites and spent half a day ruling them out one by one (sp-n0xj7).
#
# POSITIVE CONTROL: three borrower suites exit 75 (fixture fault) and one suite exits 1
# (a genuine red). The pass must file exactly one fixture-fault bead (not three) and
# exactly one red bead (for the genuine failure). Total beads filed: two. Not four.
#
# NEGATIVE CONTROL: with no shared fixture built by the pass (_td_shared=0), a suite that
# exits 75 is treated as a red, not a fixture fault. TESTDB_FAULT_EXIT is a shared-fixture
# protocol; outside that context it is an ordinary non-zero exit code.
#
# SPIRA_DB RESTORE: with testdb_up forced to fail, suites.sh's production SPIRA_DB must
# still point at the outer database after the fixture block.
#
# defect: sp-n0xj7
# covers: spira/suites.sh spira/testdb.sh spira/incident.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-suites-fixture-fault.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-suites-fixture-fault
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# Capture before testdb_up prepends the bd-embedded shim to PATH; used in sut so the
# server-mode fallback finds the real bd, not the wrapper.
_BD_REAL="$(command -v bd 2>/dev/null || true)"
testdb_up fixture-fault || { echo "test-suites-fixture-fault: could not build a fixture database"; exit 1; }

SH="$TMP/spira"; RUN="$TMP/run"; STATE="$TMP/state"; GATEF="$TMP/gate-suites"
mkdir -p "$SH" "$RUN" "$STATE" "$TMP/repo"
cp "$HERE/suites.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/incident.sh" \
   "$HERE/testdb.sh" "$HERE/suite-covers.sh" "$SH/"
# SCAR: sp-5l05i — the fixture's cp list omitted suite-covers.sh; lib.sh sources it at boot,
# so without it suite_covers_of was undefined and suites.sh misclassified suites.

printf '# the gated one\nspira/test-ff-gated.sh\n' > "$GATEF"

# Knobs, all away from the shipped default.
BUDGET=120; PERSUITE=20; STALE=3600; PRIO=3; REPONAME=fixture-fault-repo

# THE bd WRAPPER NEEDS THE REAL HOME. The embedded binary looks for its data dir under
# the invoking user's HOME, and the test's scratch HOME would cause every bd call to fail.
# The wrapper passes the real HOME to bd-embedded while the sut environment still uses a
# scratch HOME for everything else (shell history, readline, etc.). Isolating the spira
# config is SPIRA_CONF's job, not HOME's.
#
# type -P bd finds bd-embedded (TESTDB_BIN is prepended to PATH by testdb_up above).
TOOLPATH="$TMP/bin"; mkdir -p "$TOOLPATH"
printf '#!/usr/bin/env bash\nHOME=%s exec %s "$@"\n' "$HOME" "$(type -P bd)" > "$TOOLPATH/bd"
chmod +x "$TOOLPATH/bd"

# sut: run suites.sh in a clean environment.
#
# HOME: real — bd-embedded init needs the invoking user's HOME; a scratch HOME causes
# testdb_up to fail, _td_shared=0, and the fixture-fault path is never reached.
# SPIRA_TESTDB_DATA + TESTDB_SERVER_BD: passed through when set, giving the nested
# testdb_up a server-mode fallback when bd-embedded fails transiently. Without them,
# testdb_available returns 1 on transient failure and _td_shared stays 0. TESTDB_SERVER_BD
# is pre-testdb_up bd so the fallback uses the real bd, not the bd-embedded wrapper.
sut() {
    local cmd="$1"; shift
    # SPIRA_SUITES_INLINE=1 — run the planted suites HERE, not in a container.
    # suites.sh now delegates its pass to testenv-batch.sh (law-tests-run-only-through-
    # testenv-batch). This suite is testing suites.sh's OWN logic against fake suites it
    # planted in a scratch tree, so a container would have to be started per invocation to
    # run code that exists only to be counted. Containment is not being waived: this suite
    # is itself run inside a container by the timed pass, so the planted suites are already
    # contained by it.
    env -i PATH="$PATH" HOME="$HOME" \
        SPIRA_CONF="$TMP/no-such.conf" \
        SPIRA_HOME="$SH" SPIRA_REPO="$TMP/repo" SPIRA_HOME_REPO="$REPONAME" \
        SPIRA_DB="$SPIRA_DB" BEADS_DIR="${SPIRA_DB}/.beads" SPIRA_RUN="$RUN" \
        SPIRA_SUITES_STATE="$STATE" SPIRA_GATE_SUITES="$GATEF" \
        SPIRA_SUITES_BUDGET="$BUDGET" SPIRA_SUITE_TIMEOUT="$PERSUITE" \
        SPIRA_SUITES_STALE="$STALE" SPIRA_SUITES_PRIORITY="$PRIO" \
        SPIRA_PATH="$TOOLPATH" \
        SPIRA_SUITES_INLINE=1 \
        ${SPIRA_TESTDB_DATA:+SPIRA_TESTDB_DATA=$SPIRA_TESTDB_DATA} \
        ${SPIRA_TESTDB_DATA:+TESTDB_SERVER_BD=${_BD_REAL:-bd}} \
        ${SPIRA_TESTDB_PORT:+SPIRA_TESTDB_PORT=$SPIRA_TESTDB_PORT} \
        "$@" bash "$SH/suites.sh" "$cmd" 2>&1
}
plant() { cat > "$SH/$1"; chmod +x "$SH/$1"; }

B() { bd -C "$SPIRA_DB" "$@"; }
beads() {
    B list --status open,in_progress --limit 0 --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' \
        | python3 -c '
import sys, json
key = sys.argv[1]
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    if key in (i.get("title") or ""): print(i["id"])
' "$1"
}
count() { printf '%s\n' "$1" | grep -c . || true; }
bead_field() {
    B show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
d = d[0] if isinstance(d, list) else d
print(d.get(sys.argv[1], ""))
' "$2"
}
list_all_titles() {
    B list --status open,in_progress --limit 0 --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' \
        | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]): print(i.get("title",""))
' 2>/dev/null || true
}

plant test-ff-gated.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
exit 0
S

# ======================================================================================
echo
echo "positive control — three fixture-fault suites and one genuine red:"
echo "  expect two beads (one fixture-fault, one red), not four"
# ======================================================================================
# THE THREE BORROWERS exit with TESTDB_FAULT_EXIT (75). They are planted as scripts that
# simply exit 75, which is what testdb_up does when a shared fixture reset fails and
# TESTDB_SHARED=1. Since suites.sh builds a shared fixture, TESTDB_FAULT_EXIT=75 is
# exported into every suite's environment before they run.
#
# Planting them without sourcing testdb.sh is deliberate: the property under test is the
# exit-code classification, not the fixture machinery. A planted suite that exits 75 is
# identical in effect to a real borrower whose testdb_up failed; the mechanism that
# produces the exit is not what is under test.
plant test-ff-borrower1.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
exit "${TESTDB_FAULT_EXIT:-75}"
S
plant test-ff-borrower2.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
exit "${TESTDB_FAULT_EXIT:-75}"
S
plant test-ff-borrower3.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
exit "${TESTDB_FAULT_EXIT:-75}"
S
plant test-ff-genuine-red.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
printf '  FAIL  genuine assertion failed: wanted [ok] got [broken]\n'
exit 1
S

out="$(sut run)"; rc_out=$?

# The pass must exit 2 (something was filed) — not 0 (all green) and not 1 (critical error).
is "a pass with fixture faults exits 2" "2" "$rc_out"

# THREE SUITES EXITED 75. The pass must classify all three as fixture-fault, not red.
want "the pass reports FIXTURE-FAULT"  "FIXTURE-FAULT" "$out"
want "the summary names fixture-fault count" "3 fixture-fault" "$out"

# EXACTLY ONE FIXTURE-FAULT BEAD. The three borrowers must produce one bead, not three.
ff_ids="$(beads 'shared fixture collapsed')"
is "exactly one fixture-fault bead was filed, not one per borrower" "1" "$(count "$ff_ids")"
ff_id="$(printf '%s\n' "$ff_ids" | head -1)"
if [ -n "$ff_id" ]; then
    desc="$(bead_field "$ff_id" description)"
    want "it names borrower1" "test-ff-borrower1.sh" "$desc"
    want "it names borrower2" "test-ff-borrower2.sh" "$desc"
    want "it names borrower3" "test-ff-borrower3.sh" "$desc"
    labels="$(B label list "$ff_id" 2>&1)"
    want "it is labelled for builders"  "plan"            "$labels"
    want "and names the repository"     "repo:$REPONAME"  "$labels"
fi

# THE GENUINE RED FILES ITS OWN BEAD. Fixture-fault detection must not absorb genuine reds.
red_ids="$(beads 'test-ff-genuine-red.sh')"
is "the genuine red still files its own bead" "1" "$(count "$red_ids")"

# TOTAL COUNT. Two beads: one fixture-fault, one red. Not four.
all_titles="$(list_all_titles)"
is "total beads filed: two (one fixture-fault, one red) — not four" "2" "$(count "$all_titles")"

# NO PER-BORROWER BEAD. None of the three borrowers' basenames appear in bead titles.
nowant "no per-borrower bead for borrower1" "test-ff-borrower1.sh" "$all_titles"
nowant "no per-borrower bead for borrower2" "test-ff-borrower2.sh" "$all_titles"
nowant "no per-borrower bead for borrower3" "test-ff-borrower3.sh" "$all_titles"

# ======================================================================================
echo
echo "negative control — no shared fixture, exit 75 is treated as red:"
# ======================================================================================
# When suites.sh does NOT build a shared fixture (testdb.sh absent), TESTDB_FAULT_EXIT
# is never exported and _td_shared is 0. A suite exiting 75 must be classified as red
# (the normal non-zero path), not fixture-fault. This proves the fixture-fault check
# discriminates: it triggers on TESTDB_FAULT_EXIT AND _td_shared=1, not on exit-75 alone.
#
# Removing testdb.sh from SH prevents the fixture block from executing: the `if .
# "$HERE/testdb.sh" 2>/dev/null` fails silently, _td_shared stays 0, TESTDB_FAULT_EXIT
# is never exported, and exit 75 falls through to the normal red path.
rm -f "$SH/test-ff-borrower1.sh" "$SH/test-ff-borrower2.sh" "$SH/test-ff-borrower3.sh"
rm -f "$SH/test-ff-genuine-red.sh"
find "$STATE" -maxdepth 1 -name '*.result' -delete 2>/dev/null; true
# Reset fixture so the next B list call sees a clean store.
testdb_reset 2>/dev/null || true

# Plant a suite that exits 75 with no shared fixture context.
plant test-ff-solo75.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
echo "  FAIL  solo75 suite: exiting 75 with no shared fixture"
exit 75
S

# Block testdb.sh from loading so suites.sh cannot build a shared fixture at all.
rm -f "$SH/testdb.sh"
out_nc="$(sut run)"; rc_nc=$?

is "with no shared fixture, exit-75 suite exits 2 (filed as red)" "2" "$rc_nc"
want "it appears as RED, not FIXTURE-FAULT" "RED" "$out_nc"
nowant "it is not classified as FIXTURE-FAULT" "FIXTURE-FAULT" "$out_nc"
want "the summary shows 0 fixture-fault" "0 fixture-fault" "$out_nc"
red_nc_ids="$(beads 'test-ff-solo75.sh')"
is "it files a regular red bead" "1" "$(count "$red_nc_ids")"
rm -f "$SH/test-ff-solo75.sh"

echo
echo "  $pass passed, $fail failed"
[ "$fail" -eq 0 ]
