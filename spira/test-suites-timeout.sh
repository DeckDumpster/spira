#!/usr/bin/env bash
#
# test-suites-timeout.sh — spira-suites.service has a timeout guard, and suites.sh recovers
# when a suite hangs past its per-suite limit.
#
#   ./test-suites-timeout.sh
#
# WHAT THIS GUARDS (sp-3cb0). The service timed out at 05:44, 06:44, 07:44 — a suite hung
# and the runner wedged on it. Every hourly pass died mid-run. Two properties must hold to
# prevent the recurrence:
#
#   1. spira-suites.service has TimeoutStartSec — a unit-level kill that ends a pass that
#      has completely stalled. This is the last line of defense.
#
#   2. suites.sh wraps each suite in `timeout $PER_SUITE` — a suite that hangs is killed
#      (rc=124), recorded as `timeout`, filed as a bead, and the runner continues to the
#      suite that follows. A runner that wedges on one hung suite reproduces the defect.
#
# POSITIVE CONTROL IS FIRST (law-absence-needs-a-positive-control). Before trusting "the
# next suite ran", prove the hung suite was actually killed: its result file says `timeout`,
# not `ok`. A pass that never killed the suite would produce the same "next suite ran"
# appearance only after the suite finally exited on its own.
#
# THE INTAKE IS THE REAL ONE on a throwaway database (law-prefer-the-real-dependency).
# The dedupe and filing are what matter; a stub would reproduce whichever half the author
# remembered.
#
# EVERY CONFIGURED VALUE IS PINNED TO A NON-DEFAULT so a literal in the code cannot pass
# this (law-gates-run-in-a-clean-environment).
#
# defect: sp-3cb0
# covers: systemd/spira-suites.service spira/suites.sh spira/incident.sh
# timeout: 60
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
UNIT_DIR="$HERE/../systemd"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-suites-timeout.sh"

# ======================================================================================
echo
echo "structural: spira-suites.service has a timeout guard:"
# ======================================================================================
UNIT="$UNIT_DIR/spira-suites.service"

if [ ! -r "$UNIT" ]; then
    bad "spira-suites.service is readable" "not found: $UNIT"
else
    # POSITIVE CONTROL. Parse the directive before trusting the absence of problems.
    timeout_val="$(grep -m1 '^TimeoutStartSec=' "$UNIT" 2>/dev/null | cut -d= -f2- | tr -d '[:space:]')"
    if [ -z "$timeout_val" ]; then
        bad "spira-suites.service has TimeoutStartSec" "directive not found"
    else
        ok "spira-suites.service has TimeoutStartSec=$timeout_val"
        case "$timeout_val" in
            ''|*[!0-9]*) bad "TimeoutStartSec is numeric" "got [$timeout_val]" ;;
            *) [ "$timeout_val" -gt 0 ] && ok "TimeoutStartSec is positive ($timeout_val)" \
               || bad "TimeoutStartSec is positive" "got 0" ;;
        esac
    fi

    # THE UNIT INJECTS SPIRA_SUITES_MAXSEC so the script can cap its own budget under the
    # unit's kill deadline. test-budget-drift.sh verifies the values match; this verifies
    # the injection exists at all, because a missing Environment= line means the cap never
    # fires regardless of what suites.sh does with it.
    maxsec="$(grep -m1 '^Environment=SPIRA_SUITES_MAXSEC=' "$UNIT" 2>/dev/null \
              | sed 's/^Environment=SPIRA_SUITES_MAXSEC=//' | tr -d '[:space:]')"
    if [ -n "$maxsec" ]; then
        ok "unit injects SPIRA_SUITES_MAXSEC=$maxsec"
    else
        bad "unit injects SPIRA_SUITES_MAXSEC" "Environment= line not found in $UNIT"
    fi

    # suites.sh run exits 2 for routine reds; SuccessExitStatus=2 keeps the unit out of
    # the failed state on a normal red day, while exit 1 (critical errors) still fails it.
    success_exit="$(grep -m1 '^SuccessExitStatus=' "$UNIT" 2>/dev/null \
                   | cut -d= -f2- | tr -d '[:space:]')"
    if [ "$success_exit" = "2" ]; then
        ok "unit has SuccessExitStatus=2 (routine reds do not mark the unit failed)"
    else
        bad "unit has SuccessExitStatus=2" \
            "got [${success_exit:-MISSING}] — routine reds will mark the unit failed"
    fi
fi

# ======================================================================================
echo
echo "structural: suites.sh has a per-suite kill guard:"
# ======================================================================================
# THE MECHANISM THAT MAKES THE GUARD WORK AT THE SUITE LEVEL. A per-suite kill mechanism
# is what prevents a single hung suite from blocking the whole pass.
# Without this, the only guard is the unit's TimeoutStartSec — the pass would hang on
# the first hung suite until systemd killed the entire session, which is what sp-3cb0 saw.
#
# PROVE THE KILL GUARD EXISTS IN THE RUN PATH. The mechanism: each suite is run in its own
# process group (setsid), a watchdog kills the group when the slice expires, and any
# rc >= 128 is remapped to 124 (the timeout convention). All three parts must exist and
# be uncommented to constitute a working guard.
watchdog="$(grep -n 'sleep.*kill.*suite_pid\|kill.*-.*suite_pid.*sleep' "$HERE/suites.sh" \
            | grep -v '^\s*#' | head -1)"
setsid_call="$(grep -n 'setsid.*bash.*HERE.*\$s\|setsid bash' "$HERE/suites.sh" \
               | grep -v '^\s*#' | head -1)"
rc_remap="$(grep -n 'rc.*124\|124.*rc' "$HERE/suites.sh" | grep -v '^\s*#' | head -1)"
if [ -n "$setsid_call" ] && [ -n "$watchdog" ]; then
    ok "suites.sh runs each suite in its own process group with a watchdog kill"
else
    bad "suites.sh has a per-suite kill guard" \
        "setsid=[${setsid_call:-MISSING}] watchdog=[${watchdog:-MISSING}]"
fi
if [ -n "$rc_remap" ]; then
    ok "suites.sh remaps signal exits to rc=124 (timeout convention)"
else
    bad "suites.sh remaps signal exits to rc=124" "remap line not found"
fi

# ======================================================================================
echo
echo "behavioral: a hung suite is killed, filed, and the runner continues:"
# ======================================================================================
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-suites-timeout
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up suites-timeout || { echo "test-suites-timeout: could not build a fixture database"; exit 1; }
# shellcheck disable=SC1090
. "$HERE/lib.sh"

SH="$TMP/spira"; RUN="$TMP/run"; STATE="$TMP/state"; GATEF="$TMP/gate-suites"
mkdir -p "$SH" "$RUN" "$STATE" "$TMP/home" "$TMP/repo"
cp "$HERE/suites.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/incident.sh" "$SH/"

# Knobs, every one pinned away from the shipped default.
BUDGET=60; PERSUITE=3; STALE=3600; PRIO=3; REPONAME=timeout-fixture

# THE REPO-MAP. bdq refuses a create whose repo: label is not in the map, and the fixture
# runs under SPIRA_HOME=$SH so conf.sh resolves the map at $SH/repo-map. Without this,
# incident.sh's `bdq create` is refused before bd is reached, and no bead is filed.
printf '%s | %s | push | main | : | :\n' "$REPONAME" "$TMP/repo" > "$SH/repo-map"

# ASK STUB for escalation calls from incident.sh.
printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$*" >> "%s"\n' "$TMP/ask.log" > "$SH/ask.sh"
chmod +x "$SH/ask.sh"

# sut — the runner in an explicit minimal environment.
#
# SPIRA_PATH comes from testdb_up: TESTDB_BIN is prepended and contains a `bd` symlink
# pointing to the embedded binary. conf.sh rebuilds PATH from SPIRA_PATH, so TESTDB_BIN
# must be in SPIRA_PATH for the embedded binary to survive conf.sh's PATH replacement.
#
# SPIRA_BD is intentionally NOT passed. conf.sh resolves SPIRA_BD from the first `bd` on
# the assembled PATH (i.e., $TESTDB_BIN/bd → bd-embedded). Passing SPIRA_BD="bd-embedded"
# would prevent that and cause conf.sh to try running `bd-embedded` directly, which is not
# on the PATH that sut() constructs (TESTDB_BIN only has `bd`, not `bd-embedded`).
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

# B is called from outside sut(), so use the bd binary testdb_up resolved.
# conf.sh inside sut() resolves its own SPIRA_BD from SPIRA_PATH; here we use the
# command-v resolution at the time testdb_up ran (same binary, different entry point).
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

# The gate file names nothing — every planted suite goes to the timed pass.
printf '# nothing in the gate for this fixture\n' > "$GATEF"

# Plant a suite that sleeps forever — the hung suite under test.
plant test-fx-hung.sh <<'S'
#!/usr/bin/env bash
# covers: spira/suites.sh
# This suite intentionally hangs to test the per-suite timeout guard.
sleep 300
echo "FAIL  hung suite woke up — the timeout did not fire"
exit 1
S

# Plant a suite that exits quickly — its result proves the runner recovered after the hang.
plant test-fx-after-hang.sh <<'S'
#!/usr/bin/env bash
# covers: spira/suites.sh
echo "  ok    suite after the hung one ran"
S

# POSITIVE CONTROL: show the runner can actually run a suite before testing recovery.
out_pre="$(sut run)"
want "positive control: the after-hang suite ran on a normal pass" "test-fx-after-hang.sh" "$out_pre"

# Now reset and run again with the hung suite present; it should be killed, filed, and
# the runner should continue to test-fx-after-hang.sh.
find "$STATE" -maxdepth 1 -name '*.result' -delete 2>/dev/null; true

t0="$(date +%s)"
out="$(sut run)"
elapsed=$(( $(date +%s) - t0 ))

# POSITIVE CONTROL: the hung suite was actually killed, not merely slow.
# If it ran to completion it would have printed "hung suite woke up" — which it cannot
# do within PERSUITE seconds. So the result file must say `timeout`, not `ok`.
hung_st="$( { read -r s _ < "$STATE/test-fx-hung.sh.result"; printf '%s' "${s:-MISSING}"; } 2>/dev/null )"
is "the hung suite's result says timeout, not ok" "timeout" "$hung_st"

# THE RUNNER REPORTED THE TIMEOUT.
want "the pass output names the hung suite as TIMEOUT" "TIMEOUT" "$out"
want "and names the suite"                             "test-fx-hung.sh" "$out"
# FILING WAS ATTEMPTED AND DID NOT SILENTLY FAIL. file_red prints "${id:-not filed}" on
# the TIMEOUT line; "not filed" means incident.sh ran but returned no bead id — the
# original defect (sp-hk7bt): bead count was 0 while nothing in the pass output said why.
nowant "the timeout line does not say 'not filed'" "not filed" "$out"

# THE RUNNER CONTINUED AFTER THE TIMEOUT.
after_st="$( { read -r s _ < "$STATE/test-fx-after-hang.sh.result"; printf '%s' "${s:-MISSING}"; } 2>/dev/null )"
is "the suite after the hung one ran (runner recovered)" "ok" "$after_st"
want "and the pass output names it" "test-fx-after-hang.sh" "$out"

# THE TIMEOUT WAS FILED AS A BEAD. A timeout that is silent — killed and forgotten — gives
# no signal that a suite is broken, which is the recurrence of the original defect.
timeout_beads="$(beads 'test-fx-hung.sh')"
is "a bead was filed for the timed-out suite" "1" "$(count "$timeout_beads")"
bid="$(printf '%s\n' "$timeout_beads" | head -1)"
if [ -n "$bid" ]; then
    shown="$(B show "$bid" 2>&1)"
    want "the bead body names the suite"           "test-fx-hung.sh" "$shown"
    want "and says the suite timed out"            "timeout" "$shown"
    want "and names the per-suite limit"           "${PERSUITE}s" "$shown"
fi

# SANITY: the pass completed in roughly PERSUITE seconds per hung suite, not 300 (sleep).
# This is not a strict timing assertion, just a guard that the suite did not actually wait
# for the sleep. Give 30 seconds of slack for CI overhead.
[ "$elapsed" -lt $(( PERSUITE * 10 + 30 )) ] \
    && ok "pass elapsed ~${elapsed}s, not the 300s the hung suite would need" \
    || bad "pass elapsed ~${elapsed}s — did the hung suite actually get killed?" \
          "wanted < $(( PERSUITE * 10 + 30 ))s"

# ======================================================================================
echo
echo "declared timeout: # timeout: N skips the suite when budget < N:"
# ======================================================================================
# A SUITE THAT DECLARES A TIMEOUT LARGER THAN THE AVAILABLE BUDGET IS SKIPPED, not killed.
# The distinction matters: killed produces rc=124 and a filed bead (a signal that the suite
# is broken); skipped produces an `unreached` record and a "budget spent" line in the output
# (a signal that the suite deferred to a future pass). Declaring a minimum budget is how a
# suite that drives the real harness lifecycle — 8 aeon invocations, ~80s each — avoids
# being filed as broken every time it lands at the tail of a 7-minute budget.
#
# POSITIVE CONTROL FIRST. Without it, "the suite was skipped" could mean the selector is
# broken and no suite with a declared timeout ever runs. Plant the suite with a declared
# timeout larger than the pass budget, then verify it was skipped — not killed — and that
# without the declaration the same suite body would have run (law-absence-needs-a-positive-control).

# Remove the hung/after-hang suites so they don't consume budget.
rm -f "$SH/test-fx-hung.sh" "$SH/test-fx-after-hang.sh"

# A suite that declares a 60s timeout, but completes in under 1s if the runner starts it.
# This separates "skipped by the declared-timeout check" from "killed by the watchdog".
plant test-fx-declared-timeout.sh <<'S'
#!/usr/bin/env bash
# covers: spira/suites.sh
# timeout: 60
echo "  ok    declared-timeout suite ran — budget was sufficient"
S

# POSITIVE CONTROL: with BUDGET=90 (> declared 60), the suite runs.
find "$STATE" -maxdepth 1 -name '*.result' -delete 2>/dev/null; true
out_pos="$(BUDGET=90 sut run)"
pos_st="$( { read -r s _ < "$STATE/test-fx-declared-timeout.sh.result"; printf '%s' "${s:-MISSING}"; } 2>/dev/null )"
is "positive control: declared-timeout suite runs when budget >= declared" "ok" "$pos_st"
want "positive control: pass output shows the suite passed" "test-fx-declared-timeout.sh" "$out_pos"

# SKIP: with BUDGET=30 (< declared 60), the suite should be deferred, not killed.
find "$STATE" -maxdepth 1 -name '*.result' -delete 2>/dev/null; true
find "$STATE" -maxdepth 1 -name '*.unreached' -delete 2>/dev/null; true
out_skip="$(BUDGET=30 sut run)"
# The suite was deferred: it must have a .unreached file (sp-u1g: unreached no longer
# writes to .result, so checking .result for "unreached" would show MISSING).
[ -f "$STATE/test-fx-declared-timeout.sh.unreached" ] \
    && ok "suite is deferred (has .unreached file) when budget < declared timeout" \
    || bad "suite is deferred (has .unreached file) when budget < declared timeout" \
          "file not found: $STATE/test-fx-declared-timeout.sh.unreached"
# The pass output names the deferred suite rather than silently dropping it.
want "deferred suite appears in pass output" "test-fx-declared-timeout.sh" "$out_skip"
# Crucially: NOT killed (which would produce a filed bead and look like a broken suite).
nowant "deferred suite was not killed with rc=124" "TIMEOUT" "$out_skip"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
