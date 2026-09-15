#!/usr/bin/env bash
#
# test-suites-watchdog-classify.sh — a suite killed by the watchdog must be classified as
# timeout, not red, even when the suite traps TERM and exits 1 instead of 143.
#
# WHY THIS SUITE EXISTS (sp-prhs2). The watchdog sends SIGTERM to the suite's process group.
# 43 of 160 suites in this tree trap TERM to run cleanup; their cleanup fires, execution
# resumes against a deleted scratch tree, and the suite exits 1.  The rc>=128 check that
# remaps to 124 (timeout) never fires for rc=1, so the kill is filed as a red test defect.
# The runner knows the watchdog fired; the classification must use that fact, not the exit
# code convention GNU timeout uses but this runner does not.
#
# POSITIVE CONTROL IS FIRST (law-absence-needs-a-positive-control). The fixture suite must
# demonstrably be TERM-trapping and must be seen entering the watchdog path; the output
# "Terminated" the suite prints proves the trap ran.
#
# THE INTAKE IS THE REAL ONE on a throwaway database (law-prefer-the-real-dependency).
#
# EVERY CONFIGURED VALUE IS PINNED TO A NON-DEFAULT (law-gates-run-in-a-clean-environment).
#
# defect: sp-prhs2
# covers: spira/suites.sh
# timeout: 60
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-suites-watchdog-classify.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-suites-watchdog-classify
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up watchdog-classify || { echo "test-suites-watchdog-classify: could not build fixture database"; exit 1; }
# shellcheck disable=SC1090
. "$HERE/lib.sh"

SH="$TMP/spira"; RUN="$TMP/run"; STATE="$TMP/state"; GATEF="$TMP/gate-suites"
mkdir -p "$SH" "$RUN" "$STATE" "$TMP/home" "$TMP/repo"
cp "$HERE/suites.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/incident.sh" "$SH/"

# Knobs, pinned away from shipped defaults.
BUDGET=60; PERSUITE=3; STALE=3600; PRIO=3; REPONAME=watchdog-classify-fixture

# Repo-map so bdq allows repo:$REPONAME labels.
printf '%s | %s | push | main | : | :\n' "$REPONAME" "$TMP/repo" > "$SH/repo-map"

# Ask stub for escalation calls from incident.sh.
printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$*" >> "%s"\n' "$TMP/ask.log" > "$SH/ask.sh"
chmod +x "$SH/ask.sh"

sut() {
    local cmd="$1"; shift
    # SPIRA_SUITES_INLINE=1 — run the planted suites HERE, not in a container.
    # suites.sh now delegates its pass to testenv-batch.sh (law-tests-run-only-through-
    # testenv-batch). This suite is testing suites.sh's OWN logic against fake suites it
    # planted in a scratch tree, so a container would have to be started per invocation to
    # run code that exists only to be counted. Containment is not being waived: this suite
    # is itself run inside a container by the timed pass, so the planted suites are already
    # contained by it.
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF="$TMP/no-such.conf" \
        SPIRA_HOME="$SH" SPIRA_REPO="$TMP/repo" SPIRA_HOME_REPO="$REPONAME" \
        SPIRA_DB="$SPIRA_DB" SPIRA_RUN="$RUN" \
        SPIRA_SUITES_STATE="$STATE" SPIRA_GATE_SUITES="$GATEF" \
        SPIRA_SUITES_BUDGET="$BUDGET" SPIRA_SUITE_TIMEOUT="$PERSUITE" \
        SPIRA_SUITES_STALE="$STALE" SPIRA_SUITES_PRIORITY="$PRIO" \
        SPIRA_NOTIFY="$SH/ask.sh" \
        SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_SUITES_INLINE=1 \
        "$@" bash "$SH/suites.sh" "$cmd" 2>&1
}
plant() { cat > "$SH/$1"; chmod +x "$SH/$1"; }

B() { bd -C "$SPIRA_DB" "$@"; }
beads_titled() {
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

# The gate file is empty — every planted suite goes to the timed pass.
printf '# nothing in the gate for this fixture\n' > "$GATEF"

# THE FIXTURE SUITE. It traps TERM — the pattern 43 of 160 suites in this tree use.
# When the watchdog SIGTERMs it, the trap runs cleanup (removing the scratch tree),
# execution resumes, and the suite fails its own assertions, exiting 1 not 143.
# This is the exact shape that produced the false "is red" filings (sp-prhs2).
plant test-fx-termtrap.sh <<'S'
#!/usr/bin/env bash
# covers: spira/suites.sh
set -uo pipefail
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
printf 'data\n' > "$TMP/file"
# Sleep past the per-suite budget so the watchdog fires.
sleep 60
# If we wake up here, our scratch was cleaned by the TERM trap but execution resumed.
echo "Terminated"
if [ -f "$TMP/file" ]; then
    echo "  ok    file survived (watchdog did not fire)"
else
    echo "  FAIL  file gone (TERM trap ran, execution resumed against deleted scratch)"
fi
exit 1
S

# POSITIVE CONTROL: show the suite hangs (does not exit on its own within PERSUITE seconds).
# Without this, "it was killed" and "it exited fast on its own" look the same from outside.
timeout "$((PERSUITE * 2))" bash "$SH/test-fx-termtrap.sh" >/dev/null 2>&1 && \
    bad "positive control: the fixture suite does not exit naturally within PERSUITE" "it exited 0" ||
    ok "positive control: the fixture suite does not exit naturally within ${PERSUITE}s (will need the watchdog)"

# Run the timed pass with the TERM-trapping fixture suite.
out="$(sut run)"

# THE RUNNER MUST CLASSIFY IT AS TIMEOUT, NOT RED.
result_st="$( { read -r s _ < "$STATE/test-fx-termtrap.sh.result"; printf '%s' "${s:-MISSING}"; } 2>/dev/null )"
is "the TERM-trapping suite is classified as timeout, not red" "timeout" "$result_st"

# THE PASS OUTPUT MUST SAY TIMEOUT.
want "the pass output says TIMEOUT for the fixture suite" "TIMEOUT" "$out"
nowant "the pass output does not say RED for the fixture suite" "RED" "$out"

# THE FINGERPRINT MUST BE STABLE ACROSS RUNS, NOT OVER THE DEBRIS OUTPUT.
# A fingerprint over the debris (cat errors, assertion cascades) differs each run because
# the kill lands at a different point, so the FAIL set changes and produces a new bead per
# cycle — dedup is useless.  The stable key ("killed at Ns") hashes to the same checksum
# every time the suite is killed at the same slice.  Verify by running a second pass and
# confirming the fingerprint does not change.
fp1="$( { read -r _ _ _ fp < "$STATE/test-fx-termtrap.sh.result"; printf '%s' "${fp:-MISSING}"; } 2>/dev/null )"

find "$STATE" -maxdepth 1 -name '*.result' -delete 2>/dev/null; true
sut run > /dev/null 2>&1 || true
fp2="$( { read -r _ _ _ fp < "$STATE/test-fx-termtrap.sh.result"; printf '%s' "${fp:-MISSING}"; } 2>/dev/null )"

is "the fingerprint is identical across two watchdog kills (stable key)" "$fp1" "$fp2"

# A BEAD MUST NOT BE FILED TITLED '<suite> IS RED'.
red_beads="$(beads_titled 'test-fx-termtrap.sh is red')"
is "no bead is filed titled '<suite> is red'" "0" "$(count "$red_beads")"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
