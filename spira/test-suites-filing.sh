#!/usr/bin/env bash
# tier: T1
# covers: spira/suites.sh UC-test-infrastructure-27 UC-test-infrastructure-43
# host-reason: sources suites.sh's filing functions and points INC at a recording stub;
#   no container, no real bd, no incident.sh.
#
# WHAT THIS REPLACES. file_red/file_setup_fault/file_fixture_fault/file_skip's bead body and
# ref construction were previously observable only by filing real beads against an embedded
# Dolt database (test-suites-red-dedup.sh alone spent 32s on eight real filings). INC is
# already a variable (`bash "$INC" file ...`); pointing it at a stub that records its own
# environment and stdin, instead of a real incident.sh, proves the same contract — the ref,
# the labels, the sin-exemption and the body text — without a database.
#
# GAP G3 (docs/test-plan/test-infrastructure.md): before this suite, nothing mentioned
# `suite-skip:`, so file_skip's ref and sin-exemption were asserted nowhere.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
export SPIRA_SUITES_STATE="$TMP/state"
export SPIRA_GATE_SUITES="$TMP/gate-suites-empty"
: > "$SPIRA_GATE_SUITES"

# --- the recording stub ------------------------------------------------------------------
# Captures argv, every SPIRA_INCIDENT_*/SPIRA_SIN_EXEMPT env var and stdin to a side file
# (named after the ref, so concurrent calls in this suite never collide), then answers with
# a fake id on the last line of stdout — the one contract file_red et al. actually parse.
CAP="$TMP/captures"; mkdir -p "$CAP"
cat > "$TMP/incident-stub.sh" <<'STUB'
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
chmod +x "$TMP/incident-stub.sh"
export SPIRA_TEST_CAP_DIR="$CAP"
export SPIRA_INCIDENT="$TMP/incident-stub.sh"

# Sourcing (not executing) suites.sh: BASH_SOURCE[0] != $0 here, so its dispatcher never
# fires and INC picks up the stub set above (INC is derived from SPIRA_INCIDENT at source
# time, so the export must happen before this line).
. "$HERE/suites.sh"

cap_of() { cat "$CAP/$1" 2>/dev/null || true; }
safe_ref() { printf '%s' "$1" | tr -c 'A-Za-z0-9_.-' '_'; }

# --- file_red ------------------------------------------------------------------------------
_id="$(file_red test-fake-red.sh red 1 12 abc123fp 'FAIL: something broke')"
is "file_red: returns the stub's id" sp-stubfake1 "$_id"
_c="$(cap_of "$(safe_ref suite:test-fake-red.sh)")"
want "file_red: ref is suite:<name>" "SPIRA_INCIDENT_REF=suite:test-fake-red.sh" "$_c"
want "file_red: sin-exempt (persistent reds dedupe, never escalate on recurrence alone)" \
    "SPIRA_SIN_EXEMPT=1" "$_c"
# SPIRA_INCIDENT_LABELS also carries SPIRA_SCOPE_LABEL, a repo-derived default this test
# does not control — only that the "plan" label file_red itself appends is present.
want "file_red: labelled plan" "plan" "$(printf '%s' "$_c" | grep '^SPIRA_INCIDENT_LABELS=')"
want "file_red: cause is suite-red" "SPIRA_INCIDENT_CAUSE=suite-red" "$_c"
want "file_red: body carries the fingerprint" "abc123fp" "$_c"
want "file_red: body carries a reproduce line naming the suite" "bash spira/test-fake-red.sh" "$_c"

# --- file_setup_fault ------------------------------------------------------------------------
_id="$(file_setup_fault test-fake-setup.sh 1 4 'starting up
ASSERTIONS 0')"
is "file_setup_fault: returns the stub's id" sp-stubfake1 "$_id"
_c="$(cap_of "$(safe_ref setup-fault:test-fake-setup.sh)")"
want "file_setup_fault: ref is setup-fault:<name>" "SPIRA_INCIDENT_REF=setup-fault:test-fake-setup.sh" "$_c"
want "file_setup_fault: cause is setup-fault" "SPIRA_INCIDENT_CAUSE=setup-fault" "$_c"
nowant "file_setup_fault: NOT sin-exempt (unlike a red, this may escalate on recurrence)" \
    "SPIRA_SIN_EXEMPT=1" "$_c"
want "file_setup_fault: body says the covered code is NOT at fault" "is NOT at" "$_c"

# --- file_fixture_fault ----------------------------------------------------------------------
_id="$(file_fixture_fault 3 'test-a.sh test-b.sh test-c.sh' sptest_baseline_xyz)"
is "file_fixture_fault: returns the stub's id" sp-stubfake1 "$_id"
_c="$(cap_of "$(safe_ref fixture-fault:sptest_baseline_xyz)")"
want "file_fixture_fault: ref names the fixture, not a suite" \
    "SPIRA_INCIDENT_REF=fixture-fault:sptest_baseline_xyz" "$_c"
want "file_fixture_fault: one bead names every affected suite" "test-a.sh test-b.sh test-c.sh" "$_c"
want "file_fixture_fault: body says these are not red suites" "not red suites" "$_c"

# --- file_flake (sp-n2ax8: reported, never quarantined) --------------------------------------
_id="$(file_flake test-fake-flaky.sh 2 604800)"
is "file_flake: returns the stub's id" sp-stubfake1 "$_id"
_c="$(cap_of "$(safe_ref flake:test-fake-flaky.sh)")"
want "file_flake: ref is flake:<name>" "SPIRA_INCIDENT_REF=flake:test-fake-flaky.sh" "$_c"
want "file_flake: sin-exempt (recurring flakes dedupe, never escalate on recurrence alone)" \
    "SPIRA_SIN_EXEMPT=1" "$_c"
want "file_flake: cause is suite-flaky" "SPIRA_INCIDENT_CAUSE=suite-flaky" "$_c"
want "file_flake: title is a question, not a quarantine notice" \
    "why does test-fake-flaky.sh fail intermittently" "$_c"
want "file_flake: body carries the observation count and window" "2 flake observation(s) within the 604800s window" "$_c"
want "file_flake: body carries a reproduce line naming the suite" "bash spira/test-fake-flaky.sh" "$_c"
want "file_flake: body states nothing is quarantined" "nothing is" "$_c"

# --- file_skip (GAP G3) ----------------------------------------------------------------------
_id="$(file_skip test-fake-skip.sh 2 'exit 77 — no display available')"
is "file_skip: returns the stub's id" sp-stubfake1 "$_id"
_c="$(cap_of "$(safe_ref suite-skip:test-fake-skip.sh)")"
want "file_skip: ref is suite-skip:<name>" "SPIRA_INCIDENT_REF=suite-skip:test-fake-skip.sh" "$_c"
want "file_skip: sin-exempt" "SPIRA_SIN_EXEMPT=1" "$_c"
want "file_skip: body states the rule a check that cannot run is not a check that passed" \
    "not a check that passed" "$_c"
want "file_skip: body carries the reproduce line" "bash spira/test-fake-skip.sh" "$_c"

# POSITIVE CONTROL: when INC is unreadable, every filer refuses rather than losing the
# finding silently — proven last so an earlier bug in the stub wiring above would have
# already failed a case, not been masked by this one always passing.
export SPIRA_INCIDENT="$TMP/does-not-exist.sh"
INC="$SPIRA_INCIDENT"
wantrc "file_red: refuses (rc=1) when INC is unreadable" 1 \
    "$(file_red test-x.sh red 1 1 fp out >/dev/null 2>&1; echo $?)"
wantrc "file_skip: refuses (rc=1) when INC is unreadable" 1 \
    "$(file_skip test-x.sh 1 out >/dev/null 2>&1; echo $?)"
wantrc "file_flake: refuses (rc=1) when INC is unreadable" 1 \
    "$(file_flake test-x.sh 2 604800 >/dev/null 2>&1; echo $?)"

tl_summary
