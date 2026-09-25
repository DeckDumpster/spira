#!/usr/bin/env bash
# test-testenv-guard.sh — a suite declaring `# requires: testenv` refuses before any of
# its own code runs when SPIRA_IN_TESTENV is not 1, and proceeds when it is.
#
# WHAT THIS PROVES (sp-nxvjm)
#   A. suite_testenv_unmet (suite-covers.sh): true only for a suite declaring
#      `# requires: testenv` with SPIRA_IN_TESTENV unset or not "1"; false once
#      SPIRA_IN_TESTENV=1, and false for a suite with no such declaration.
#   B. testlib.sh: sourcing it from a fixture suite that declares `# requires: testenv`
#      refuses (non-zero exit, "Bail out!") before the suite's own body runs — proved by
#      a stub systemctl on PATH recording zero invocations — and, with SPIRA_IN_TESTENV=1,
#      runs the suite body, which records exactly one invocation.
#   C. testenv-guard.sh: the same two outcomes for a suite that does not source testlib.sh.
#
# SEEN TO FAIL AGAINST UNFIXED TREE (law-a-regression-test-must-be-seen-to-fail):
#   Before suite_testenv_unmet existed and before testlib.sh/testenv-guard.sh called it,
#   a fixture suite run directly on the host called the stub systemctl (log had 1 line,
#   not 0) and testlib never bailed — this is exactly how sp-1l8ze's suite reached the
#   operator's real user manager.
#
# host-reason: fixture suites are invoked directly with `bash`, never through
# testenv-batch.sh or a container — that IS the scenario under test (run by hand).
# tier: T1
# covers: spira/testlib.sh spira/suite-covers.sh spira/testenv-guard.sh spira/testenv-batch.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/suite-covers.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
# PART A — suite_testenv_unmet predicate, no subprocess needed.
# ---------------------------------------------------------------------------
cat > "$TMP/fx-req-testenv.sh" <<'EOF'
#!/usr/bin/env bash
# requires: testenv
exit 0
EOF
cat > "$TMP/fx-req-other.sh" <<'EOF'
#!/usr/bin/env bash
# requires: bash
exit 0
EOF
cat > "$TMP/fx-no-req.sh" <<'EOF'
#!/usr/bin/env bash
# covers: some/file.sh
exit 0
EOF

# NOT run in a subshell: testlib's pass/fail counters are plain shell globals, and a
# subshell's increments vanish when it exits, letting a "bad" inside it print to stdout
# while tl_summary never sees it — a false green.  SPIRA_IN_TESTENV is toggled in-place
# instead (set to empty rather than unset, since `set -u` is active).
SPIRA_IN_TESTENV=
if suite_testenv_unmet "$TMP/fx-req-testenv.sh"; then
    ok "A1: requires:testenv + no SPIRA_IN_TESTENV -> unmet"
else
    bad "A1: requires:testenv + no SPIRA_IN_TESTENV -> unmet" "returned false"
fi

SPIRA_IN_TESTENV=1
if suite_testenv_unmet "$TMP/fx-req-testenv.sh"; then
    bad "A2: requires:testenv + SPIRA_IN_TESTENV=1 -> met" "returned true"
else
    ok "A2: requires:testenv + SPIRA_IN_TESTENV=1 -> met"
fi

SPIRA_IN_TESTENV=
if suite_testenv_unmet "$TMP/fx-req-other.sh"; then
    bad "A3: requires:bash (no testenv token) -> never unmet" "returned true"
else
    ok "A3: requires:bash (no testenv token) -> never unmet"
fi

if suite_testenv_unmet "$TMP/fx-no-req.sh"; then
    bad "A4: no requires: line -> never unmet" "returned true"
else
    ok "A4: no requires: line -> never unmet"
fi
unset SPIRA_IN_TESTENV

# ---------------------------------------------------------------------------
# Shared fixture plumbing for parts B and C: a stub systemctl on PATH that
# only records invocations — it must never touch a real service manager.
# ---------------------------------------------------------------------------
run_fixture() {  # run_fixture <fixture-file> <stub-log> [SPIRA_IN_TESTENV=1]
    local fx="$1" stublog="$2" in_testenv="${3:-}"
    local bindir="$TMP/bin-$$-$RANDOM"
    mkdir -p "$bindir"
    cat > "$bindir/systemctl" <<EOF2
#!/bin/sh
printf '%s\n' "\$*" >> '$stublog'
exit 0
EOF2
    chmod +x "$bindir/systemctl"
    : > "$stublog"
    local rc=0
    if [ -n "$in_testenv" ]; then
        PATH="$bindir:$PATH" SPIRA_IN_TESTENV=1 bash "$fx" >"$TMP/out.$$" 2>&1 || rc=$?
    else
        PATH="$bindir:$PATH" env -u SPIRA_IN_TESTENV bash "$fx" >"$TMP/out.$$" 2>&1 || rc=$?
    fi
    cat "$TMP/out.$$"
    rm -f "$TMP/out.$$"
    return "$rc"
}

# ---------------------------------------------------------------------------
# PART B — testlib.sh-based suite.
# ---------------------------------------------------------------------------
cat > "$TMP/fx-testlib-suite.sh" <<EOF
#!/usr/bin/env bash
# requires: testenv
# covers: nothing
set -uo pipefail
. "$HERE/testlib.sh"
systemctl --user start spira-fixture.service
ok "fixture ran"
tl_summary
EOF

_b_stublog="$TMP/b-systemctl.log"
_b_out="$(run_fixture "$TMP/fx-testlib-suite.sh" "$_b_stublog" "")"
_b_rc=$?
if [ "$_b_rc" -ne 0 ]; then
    ok "B1: refuses (non-zero exit) without SPIRA_IN_TESTENV"
else
    bad "B1: refuses (non-zero exit) without SPIRA_IN_TESTENV" "exited 0"
fi
want "B1: refusal names Bail out!" "Bail out!" "$_b_out"
want "B1: refusal names testenv-batch.sh" "testenv-batch.sh" "$_b_out"
_b_lines="$(wc -l < "$_b_stublog" | tr -d ' ')"
is "B1: stub systemctl recorded zero invocations before refusal" "0" "$_b_lines"

_b2_out="$(run_fixture "$TMP/fx-testlib-suite.sh" "$_b_stublog" 1)"
_b2_rc=$?
is "B2: proceeds with SPIRA_IN_TESTENV=1 (exit 0)" 0 "$_b2_rc"
want "B2: suite body ran" "fixture ran" "$_b2_out"
_b2_lines="$(wc -l < "$_b_stublog" | tr -d ' ')"
is "B2: stub systemctl recorded exactly one invocation" "1" "$_b2_lines"

# ---------------------------------------------------------------------------
# PART C — testenv-guard.sh-based suite (no testlib.sh).
# ---------------------------------------------------------------------------
cat > "$TMP/fx-prelude-suite.sh" <<EOF
#!/usr/bin/env bash
# requires: testenv
# covers: nothing
set -uo pipefail
HERE2="$HERE"
. "\$HERE2/testenv-guard.sh"
systemctl --user start spira-fixture.service
printf 'fixture ran\n'
exit 0
EOF

_c_stublog="$TMP/c-systemctl.log"
_c_out="$(run_fixture "$TMP/fx-prelude-suite.sh" "$_c_stublog" "")"
_c_rc=$?
if [ "$_c_rc" -ne 0 ]; then
    ok "C1: refuses (non-zero exit) without SPIRA_IN_TESTENV"
else
    bad "C1: refuses (non-zero exit) without SPIRA_IN_TESTENV" "exited 0"
fi
want "C1: refusal names Bail out!" "Bail out!" "$_c_out"
want "C1: refusal names testenv-batch.sh" "testenv-batch.sh" "$_c_out"
_c_lines="$(wc -l < "$_c_stublog" | tr -d ' ')"
is "C1: stub systemctl recorded zero invocations before refusal" "0" "$_c_lines"

_c2_out="$(run_fixture "$TMP/fx-prelude-suite.sh" "$_c_stublog" 1)"
_c2_rc=$?
is "C2: proceeds with SPIRA_IN_TESTENV=1 (exit 0)" 0 "$_c2_rc"
want "C2: suite body ran" "fixture ran" "$_c2_out"
_c2_lines="$(wc -l < "$_c_stublog" | tr -d ' ')"
is "C2: stub systemctl recorded exactly one invocation" "1" "$_c2_lines"

tl_summary
