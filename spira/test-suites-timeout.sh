#!/usr/bin/env bash
#
# test-suites-timeout.sh — the per-suite watchdog: a hung suite is killed and classified as
# timeout — not red, even when it traps TERM and exits 1 (sp-prhs2) — filed once through the
# runner's intake contract, and the runner continues past it (sp-3cb0). Real budget
# exhaustion defers a later suite to unreached, rather than starting and killing it (sp-04bd).
#
# ABSORBS test-suites-watchdog-classify.sh (the TERM-trap classification row) and
# test-suites-result-files.sh (the real, budget-exhausted unreached row): both drove this
# same watchdog through a copy of this same fixture harness to prove properties this suite
# already covers. Fingerprint stability and the pure classify()/record_write() decisions are
# now a table over fake inputs in test-suites-classify.sh — cheaper and no longer bound to a
# real kill; only the wiring — the runner making the kill/defer decision against a real
# clock and a real process group — needs a real process here.
#
# POSITIVE CONTROL IS FIRST throughout (law-absence-needs-a-positive-control): a suite is
# shown to hang, or to fit the budget, before its killed or deferred shape is trusted.
#
# THE FILER IS A STUB, not incident.sh. This suite proves the RUNNER's decision — which
# suite got killed, at what limit, and that filing was attempted — not the bead body or
# dedupe rules incident.sh itself implements, which test-suites-filing.sh already covers
# against the same stub contract, and test-suites.sh covers once against a real bd.
#
# NO DATABASE. SPIRA_SUITES_SKIP_TESTDB=1 and a SPIRA_DB that resolves to no `.beads` dir
# skip conf.sh's bd migration probe entirely — nothing here ever calls bd.
#
# EVERY CONFIGURED VALUE IS PINNED TO A NON-DEFAULT (law-gates-run-in-a-clean-environment).
#
# defect: sp-3cb0
# tier: T2
# covers: spira/suites.sh systemd/spira-suites.service UC-test-infrastructure-25 UC-test-infrastructure-29 UC-test-infrastructure-30
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
UNIT_DIR="$HERE/../systemd"

# ======================================================================================
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
    # unit's kill deadline. test-suites-unit-lint.sh(-ish) verifies the values match; this
    # verifies the injection exists at all, because a missing Environment= line means the
    # cap never fires regardless of what suites.sh does with it.
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
echo "structural: suites.sh has a per-suite kill guard:"
# ======================================================================================
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
echo "fixture: a scratch runner with a stub filer, no database, no container:"
# ======================================================================================
# suites.sh now delegates its pass to testenv-batch.sh (law-tests-run-only-through-
# testenv-batch). This suite tests suites.sh's OWN watchdog/classification logic against
# fake suites it plants in a scratch tree — running that inside a second, nested container
# would start a container to run code that exists only to be counted. Containment is not
# waived: this suite is itself run inside a container by the timed pass, so the planted
# suites are already contained by it.
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
SH="$TMP/spira"; RUN="$TMP/run"; STATE="$TMP/state"; GATEF="$TMP/gate-suites"
mkdir -p "$SH" "$RUN" "$STATE" "$TMP/home"
cp "$HERE/suites.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/suite-state.sh" \
   "$HERE/suite-covers.sh" "$SH/"
printf '# nothing in the gate for this fixture\n' > "$GATEF"

# THE STUB FILER. Records argv, every SPIRA_INCIDENT_*/SPIRA_SIN_EXEMPT env var and stdin
# to a file named after the ref (so concurrent refs never collide), then answers with a
# fake id — the one contract file_red actually parses (its last stdout line).
CAP="$TMP/captures"; mkdir -p "$CAP"
cat > "$SH/incident-stub.sh" <<'STUB'
#!/usr/bin/env bash
_ref="${SPIRA_INCIDENT_REF:-noref}"
_safe="$(printf '%s' "$_ref" | tr -c 'A-Za-z0-9_.-' '_')"
{
    printf 'ARGV: %s\n' "$*"
    env | grep -E '^SPIRA_(INCIDENT|SIN)_' | sort
    printf -- '--- stdin ---\n'
    cat
} > "$SPIRA_TEST_CAP_DIR/$_safe"
printf 'sp-stubfake1\n'
STUB
chmod +x "$SH/incident-stub.sh"
cap_of() { cat "$CAP/$(printf '%s' "$1" | tr -c 'A-Za-z0-9_.-' '_')" 2>/dev/null || true; }

# Knobs. BUDGET/PERSUITE are overridden per section below (bash applies a command-prefix
# assignment to a shell function for the duration of that one call, same as a builtin).
BUDGET=20; PERSUITE=1; STALE=3600; PRIO=4

sut() {
    local cmd="$1"; shift
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF="$TMP/no-such.conf" \
        SPIRA_HOME="$SH" SPIRA_REPO="$TMP" SPIRA_HOME_REPO=timeout-fixture \
        SPIRA_DB="$TMP/no-such-db" SPIRA_RUN="$RUN" \
        SPIRA_SUITES_STATE="$STATE" SPIRA_GATE_SUITES="$GATEF" \
        SPIRA_SUITES_BUDGET="$BUDGET" SPIRA_SUITE_TIMEOUT="$PERSUITE" \
        SPIRA_SUITES_STALE="$STALE" SPIRA_SUITES_PRIORITY="$PRIO" \
        SPIRA_INCIDENT="$SH/incident-stub.sh" SPIRA_TEST_CAP_DIR="$CAP" \
        SPIRA_SUITES_INLINE=1 SPIRA_SUITES_SKIP_TESTDB=1 \
        "$@" bash "$SH/suites.sh" "$cmd" 2>&1
}
plant() { cat > "$SH/$1"; chmod +x "$SH/$1"; }
result_status() {
    local f="$STATE/$1.result"
    [ -r "$f" ] || { printf 'MISSING'; return; }
    read -r s _ < "$f" 2>/dev/null && printf '%s' "${s:-MISSING}" || printf 'MISSING'
}
clear_state() {
    find "$STATE" -maxdepth 1 \( -name '*.result' -o -name '*.unreached' \) -delete 2>/dev/null
    true
}

# ======================================================================================
echo "behavioral: a hung suite is killed, filed, and the runner continues:"
# ======================================================================================
plant test-fx-hung.sh <<'S'
#!/usr/bin/env bash
# covers: spira/suites.sh
sleep 300
echo "FAIL  hung suite woke up — the timeout did not fire"
exit 1
S
# Sorts after test-fx-hung.sh, so "the pass reached the suite after the hung one" is a
# claim about run order, not just about the runner eventually returning.
plant test-fx-zzz-after.sh <<'S'
#!/usr/bin/env bash
# covers: spira/suites.sh
echo "  ok    suite after the hung one ran"
S

t0="$(date +%s)"
out="$(sut run)"
elapsed=$(( $(date +%s) - t0 ))

# POSITIVE CONTROL: the hung suite was actually killed, not merely slow. If it ran to
# completion it would have printed "hung suite woke up", which it cannot do within
# PERSUITE seconds — so the result file must say timeout, not ok.
is "the hung suite's result says timeout, not ok" "timeout" "$(result_status test-fx-hung.sh)"
want "the pass output names the hung suite as TIMEOUT" "TIMEOUT" "$out"
want "and names the suite"                             "test-fx-hung.sh" "$out"
# file_red prints "${id:-not filed}" on the TIMEOUT line; "not filed" means filing was
# attempted and failed silently — the original defect shape (sp-hk7bt).
nowant "the timeout line does not say 'not filed'" "not filed" "$out"

# THE RUNNER REACHED THE SUITE AFTER THE HUNG ONE.
is "the suite after the hung one ran (runner recovered)" "ok" "$(result_status test-fx-zzz-after.sh)"
want "and the pass output names it" "test-fx-zzz-after.sh" "$out"

# THE TIMEOUT WAS FILED THROUGH THE INTAKE CONTRACT, naming the suite and the per-suite
# limit that killed it — not merely "something failed".
_c="$(cap_of suite:test-fx-hung.sh)"
want "a filing was attempted for the timed-out suite" "ARGV:" "$_c"
want "the filing names the suite as timeout"      "is timeout in the timed suite run" "$_c"
want "the filing is sin-exempt"                   "SPIRA_SIN_EXEMPT=1" "$_c"
want "the body carries a reproduce line naming the suite" "bash spira/test-fx-hung.sh" "$_c"
want "the body names the per-suite limit that killed it"  "killed at ${PERSUITE}s" "$_c"

# SANITY: the pass completed in roughly PERSUITE seconds, not the 300s the hung suite's
# sleep would need. Not a strict timing assertion, just a guard against the runner having
# actually waited for it. Slack is generous for CI overhead.
[ "$elapsed" -lt $(( PERSUITE * 10 + 30 )) ] \
    && ok "pass elapsed ~${elapsed}s, not the 300s the hung suite would need" \
    || bad "pass elapsed ~${elapsed}s — did the hung suite actually get killed?" \
          "wanted < $(( PERSUITE * 10 + 30 ))s"

rm -f "$SH/test-fx-hung.sh" "$SH/test-fx-zzz-after.sh"

# ======================================================================================
echo "behavioral: a TERM-trapping suite is classified timeout, never red (sp-prhs2):"
# ======================================================================================
# 43 of 160 suites in this tree trap TERM to run cleanup. When the watchdog SIGTERMs one,
# its trap fires, execution resumes against a deleted scratch tree, and it exits 1 — not
# 143 — so an rc>=128 check alone misses the kill and files a false red. The runner must
# use the watchdog's own record of having fired, not the exit code convention.
clear_state
plant test-fx-termtrap.sh <<'S'
#!/usr/bin/env bash
set -uo pipefail
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
sleep 300
echo "Terminated"
exit 1
S

# POSITIVE CONTROL: the fixture does not exit on its own inside PERSUITE — it needs the
# watchdog. Without this, "classified timeout" and "exited timeout-shaped by coincidence"
# look the same from outside.
timeout "$((PERSUITE * 2))" bash "$SH/test-fx-termtrap.sh" >/dev/null 2>&1 \
    && bad "positive control: the fixture does not exit naturally within PERSUITE" "it exited 0" \
    || ok "positive control: the fixture does not exit naturally within ${PERSUITE}s"

out="$(sut run)"
is "the TERM-trapping suite is classified as timeout, not red" \
    "timeout" "$(result_status test-fx-termtrap.sh)"
want "the pass output says TIMEOUT for it" "TIMEOUT" "$out"
nowant "the pass output does not say RED for it" "RED" "$out"
_c="$(cap_of suite:test-fx-termtrap.sh)"
want "it was filed as timeout, not as red" "is timeout in the timed suite run" "$_c"
nowant "no bead was filed titled '<suite> is red'" "is red in the timed suite run" "$_c"

rm -f "$SH/test-fx-termtrap.sh"

# ======================================================================================
echo "behavioral: budget exhaustion defers a suite to unreached, not killed (sp-04bd):"
# ======================================================================================
# A suite the runner never gets to start because the pass ran out of time must still get
# a record (an .unreached file), and must NOT be started only to be killed — that would
# file it as a broken suite rather than show it as deferred to the next pass.
clear_state
plant test-fx-uaaa.sh <<'S'
#!/usr/bin/env bash
echo "  ok    passes immediately"
S
plant test-fx-uslow.sh <<'S'
#!/usr/bin/env bash
sleep 300
S
plant test-fx-uzzz.sh <<'S'
#!/usr/bin/env bash
echo "  ok    would pass immediately, if reached"
S

# POSITIVE CONTROL: before any run, no result or unreached files exist for these fixtures.
[ -e "$STATE/test-fx-uzzz.sh.result" ] || [ -e "$STATE/test-fx-uzzz.sh.unreached" ] \
    && bad "no pre-existing record for test-fx-uzzz.sh" "a file already present" \
    || ok "no pre-existing record for test-fx-uzzz.sh"

# BUDGET=8, PERSUITE=5: test-fx-uaaa.sh runs (left ~8, well above the 5s UNREACHED_MIN
# floor), test-fx-uslow.sh is killed at its 5s slice (left ~3 afterward, at or below the
# floor), and test-fx-uzzz.sh — never started, no prior recorded runtime — is deferred
# rather than begun with almost no budget left.
out="$(BUDGET=8 PERSUITE=5 sut run)"

is "positive control: test-fx-uaaa.sh (fits easily) has an ok result" \
    "ok" "$(result_status test-fx-uaaa.sh)"
is "test-fx-uslow.sh (hung, killed by watchdog) has a timeout result" \
    "timeout" "$(result_status test-fx-uslow.sh)"
[ -f "$STATE/test-fx-uzzz.sh.unreached" ] \
    && ok "test-fx-uzzz.sh (budget exhausted) has an .unreached record" \
    || bad "test-fx-uzzz.sh (budget exhausted) has an .unreached record" \
          "file not found: $STATE/test-fx-uzzz.sh.unreached"
[ -f "$STATE/test-fx-uzzz.sh.result" ] \
    && bad "test-fx-uzzz.sh was never started (no .result)" "a .result file exists — it ran" \
    || ok "test-fx-uzzz.sh was never started (no .result)"
want "the pass output names it as budget spent" "test-fx-uzzz.sh" "$out"

rm -f "$SH/test-fx-uaaa.sh" "$SH/test-fx-uslow.sh" "$SH/test-fx-uzzz.sh"

# ======================================================================================
echo "declared timeout: # timeout: N skips the suite when budget < N:"
# ======================================================================================
# A declared timeout larger than the available budget defers the suite (unreached), rather
# than starting and killing it. This lets a suite that drives the real harness lifecycle —
# many aeon invocations, tens of seconds each — declare its own minimum without being filed
# as broken every time it lands at the tail of a tight budget.
clear_state
plant test-fx-declared-timeout.sh <<'S'
#!/usr/bin/env bash
# timeout: 60
echo "  ok    declared-timeout suite ran — budget was sufficient"
S

# POSITIVE CONTROL: with budget above the declared minimum, the suite runs.
out_pos="$(BUDGET=90 sut run)"
is "positive control: declared-timeout suite runs when budget >= declared" \
    "ok" "$(result_status test-fx-declared-timeout.sh)"
want "positive control: pass output shows the suite passed" \
    "test-fx-declared-timeout.sh" "$out_pos"

# SKIP: with budget below the declared minimum, the suite is deferred, not killed.
clear_state
out_skip="$(BUDGET=30 sut run)"
[ -f "$STATE/test-fx-declared-timeout.sh.unreached" ] \
    && ok "suite is deferred (.unreached) when budget < declared timeout" \
    || bad "suite is deferred (.unreached) when budget < declared timeout" \
          "file not found: $STATE/test-fx-declared-timeout.sh.unreached"
want "deferred suite appears in pass output" "test-fx-declared-timeout.sh" "$out_skip"
nowant "deferred suite was not killed with rc=124" "TIMEOUT" "$out_skip"

tl_summary
