#!/usr/bin/env bash
#
# test-gate-budget.sh — the gate declares a time budget; exceeding it files a bead against
# the harness and does NOT fail the branch.
#
# WHY THIS SUITE EXISTS. The previous 43-suite gate grew to 17 minutes not because anyone
# decided to have one that long, but because each suite was added one at a time and the total
# was never measured against an explicit limit. This bead adds SPIRA_GATE_BUDGET and makes
# gate-spira.sh time itself. The suite proves three properties:
#
#   1. The per-check cost and total appear in the output on every run, so the timed run and
#      the cockpit pane can both read them (positive control — a silent cost line reads
#      identically whether it was never computed or just never shown).
#   2. A budget overrun files a bead against the harness and exits with the SUITES' verdict,
#      not with a budget-specific code — the branch did not cause the overrun.
#   3. A second overrun does not file a second bead (deduplication).
#
# defect: sp-cv0i
# covers: spira/gate-spira.sh spira/conf.sh spira/gate-fences.sh
# host-reason: testdb provides isolation; gate-spira.sh fixture runs without host systemd.
# scar: the fence cp list (exclude.sh inventory.sh hermetic.sh) omitted `literal-lint.sh` when
# c515322 added it as gate-spira.sh's fourth fence — gate refused with "literal-lint.sh is
# missing" on every invocation; gate_fence_cp derives the fence list from gate-spira.sh at call
# time (sp-i7u). Seen red: FAIL gate exits 0 when suites pass and budget is not exceeded:
# wanted [0] got [1] — gate: spira/literal-lint.sh is missing — refusing to land unchecked
# shellcheck disable=SC1090
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }

. "$HERE/testdb.sh"
. "$HERE/gate-fences.sh"
testdb_require test-gate-budget
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up budgetgate || { echo "test-gate-budget: could not build fixture database"; exit 1; }

# THE HARNESS UNDER TEST — a copy of gate-spira.sh and its dependencies in a temp directory.
# gate-spira.sh locates lib.sh, conf.sh etc. from its own path ($HERE in the script), so a
# copy with those files beside it runs identically to the installed version while drawing on
# the fixture database and a controllable budget.
SH="$TMP/spira"
mkdir -p "$SH"
cp "$HERE/gate-spira.sh" "$HERE/suite-covers.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/sop.sh" "$SH/"
gate_fence_cp "$HERE/gate-spira.sh" "$HERE" "$SH"
[ -f "$HERE/inventory-deny" ] && cp "$HERE/inventory-deny" "$SH/"
# A REPO-MAP so _bdq_check_repo_label allows repo:spira when file_budget_bead fires. The
# check only tests that "spira" is a known name — the path is not used by the check.
printf 'spira | %s\n' "$TMP" > "$SH/repo-map"

# A MINIMAL GIT REPOSITORY that satisfies exclude.sh (which calls `git ls-files`) and
# inventory.sh (which scans tracked files). One harmless file committed.
git init -q -b main "$TMP" 2>/dev/null || true
printf 'marker\n' > "$TMP/marker.txt"
git -C "$TMP" add marker.txt 2>/dev/null || true
git -C "$TMP" -c user.email=t@t -c user.name=t commit -q -m init 2>/dev/null || true

# A DUMMY SUITE so hermetic.sh finds at least one test-*.sh (law-absence-needs-a-positive-
# control: an empty glob and a clean tree are the same silence from outside).
printf '#!/usr/bin/env bash\n# covers: spira/gate-spira.sh\nset -uo pipefail\nexit 0\n' \
    > "$SH/test-dummy.sh"; chmod +x "$SH/test-dummy.sh"

# THE SUITES UNDER TEST. fast-suite.sh passes immediately; slow-suite.sh sleeps so the gate
# total always exceeds a budget of 0; fail-suite.sh exits 1 to prove a budget overrun does
# not suppress a real failure.
FAST="$SH/fast-suite.sh"
printf '#!/usr/bin/env bash\n# covers: spira/gate-spira.sh\nexit 0\n' > "$FAST"
chmod +x "$FAST"

SLOW="$SH/slow-suite.sh"
printf '#!/usr/bin/env bash\n# covers: spira/gate-spira.sh\nsleep 1; exit 0\n' > "$SLOW"
chmod +x "$SLOW"

FAIL_S="$SH/fail-suite.sh"
printf '#!/usr/bin/env bash\n# covers: spira/gate-spira.sh\nsleep 1; exit 1\n' > "$FAIL_S"
chmod +x "$FAIL_S"

# run_gate <gate-suites content> <budget> — runs gate-spira.sh, captures output to $GOUT,
# sets gate_rc to the exit status. Called directly (not in $()), so gate_rc propagates to
# the outer shell. Callers read $GOUT for content checks.
gate_rc=0
GOUT="$TMP/gate-out"
run_gate() {
    local content="$1" budget="$2"
    printf '%s\n' "$content" > "$SH/gate-suites"
    (
        unset SPIRA_HOME
        cd "$TMP"
        SPIRA_CONF="/nonexistent.conf" SPIRA_DB="$SPIRA_DB" SPIRA_GATE_BUDGET="$budget" \
            bash spira/gate-spira.sh 2>&1
    ) > "$GOUT" 2>&1; gate_rc=$?
}

# Count open beads with the budget external-ref in the fixture database.
# --external-ref is not a bd list flag — filter by the JSON field instead.
beads_open() {
    "${SPIRA_BD:-bd}" -C "$SPIRA_DB" list --status open --limit 0 --json 2>/dev/null \
        | python3 -c 'import sys,json
d=json.load(sys.stdin)
match=[x for x in (d if isinstance(d,list) else []) if x.get("external_ref")=="gate:budget"]
print(len(match))
' 2>/dev/null || echo 0
}

echo "test-gate-budget.sh — timing, budget enforcement, bead filing"

# --------------------------------------------------------------------------------------
# POSITIVE CONTROL: per-check cost and total appear in the output on every run.
# A cost line that is silently suppressed reads identically to one that was never computed.
# --------------------------------------------------------------------------------------
run_gate "spira/fast-suite.sh" 9999; out="$(cat "$GOUT")"
is   "gate exits 0 when suites pass and budget is not exceeded"     0    "$gate_rc"
want "per-check cost appears in output"          "cost="                 "$out"
want "total cost appears in output"              "cost total="           "$out"
want "the budget appears in the total line"      "budget=9999s"          "$out"

# --------------------------------------------------------------------------------------
# SUITES PASS, BUDGET EXCEEDED. Exit 0 — the branch is not at fault.
# Planting a slow suite proves the timing is real: a budget of 0 seconds is exceeded by
# any run that takes at least one second.
# --------------------------------------------------------------------------------------
testdb_reset
run_gate "spira/slow-suite.sh" 0; out="$(cat "$GOUT")"
is   "exit is 0 when suites pass though budget is exceeded"         0    "$gate_rc"
want "output names the overrun"                  "EXCEEDED"              "$out"
want "output names the budget"                   "budget=0s"             "$out"
want "output says this is a gate fault"          "gate fault"            "$out"
is   "a bead is filed against the harness"       1                       "$(beads_open)"

# --------------------------------------------------------------------------------------
# SUITES FAIL, BUDGET EXCEEDED. Exit 1 — the branch fault and the gate fault are
# independent signals; the overrun must not suppress the failure.
# --------------------------------------------------------------------------------------
testdb_reset
run_gate "spira/fail-suite.sh" 0; out="$(cat "$GOUT")"
is   "exit is 1 when suites fail (budget overrun does not mask it)" 1    "$gate_rc"
is   "bead is still filed even when suites fail"                    1    "$(beads_open)"

# --------------------------------------------------------------------------------------
# DEDUPLICATE. A second overrun against a database that already holds one open bead for
# this ref must not file a second one.
# --------------------------------------------------------------------------------------
run_gate "spira/slow-suite.sh" 0; out="$(cat "$GOUT")"
is   "a second overrun does not file a second bead"                 1    "$(beads_open)"

# --------------------------------------------------------------------------------------
# WITHIN BUDGET. No bead is filed; the total line still appears (positive control).
# --------------------------------------------------------------------------------------
testdb_reset
run_gate "spira/fast-suite.sh" 9999; out="$(cat "$GOUT")"
is   "no bead filed when gate finishes under budget"                0    "$(beads_open)"
want "total line still appears when within budget"   "cost total="       "$out"

# --------------------------------------------------------------------------------------
# REGRESSION sp-rgao1: run_gate must unset SPIRA_HOME before invoking gate-spira.sh.
# When SPIRA_HOME points elsewhere, conf.sh's _spira_conf_home_env branch tries
# $SPIRA_HOME/repo-map first — bypassing $SH/repo-map — so _bdq_check_repo_label
# refuses repo:spira and bdq create returns 1, silently dropping the budget bead.
# --------------------------------------------------------------------------------------
testdb_reset
_shome_bak="${SPIRA_HOME:-}"; _shome_set="${SPIRA_HOME+1}"
export SPIRA_HOME="/nonexistent-home"
run_gate "spira/slow-suite.sh" 0
[ -n "$_shome_set" ] && export SPIRA_HOME="$_shome_bak" || unset SPIRA_HOME
is   "bead filed even when outer SPIRA_HOME points elsewhere (regression sp-rgao1)"  1  "$(beads_open)"

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
