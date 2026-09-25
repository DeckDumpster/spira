#!/usr/bin/env bash
# test-gate-base-evidence.sh — a red base names its own red suites and carries its own output,
# and excuses only the suites that are red on it too.
# tier: T1
# covers: spira/gate.sh UC-gate-verdict-10 UC-gate-verdict-14
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
. "$HERE/testlib/gate-fixture.sh"
command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
gate_fixture_init "$TMP"
BR=spira/sp-v1
gate_fixture_branch "$BR" f1.txt one

# gate_with <printed at the branch> <printed at the base>: both trials exit 1. Every case
# below is a fresh call: a red verdict removes the worktree (UC-gate-verdict-18), so the one
# fixture from gate_fixture_init is safe to reuse across cases rather than rebuilding a
# remote-plus-clone per case (docs/test-plan/gate-verdict.md section 4 cluster 2).
gate_with() {
    printf '%s\n' "$1" > "$TMP/at-branch"; printf '%s\n' "$2" > "$TMP/at-base"
    printf 'repo | %s | push | origin/main |  | %s\n' "$REPO" \
        "if [ \"\$SPIRA_GATE_BRANCH\" = \"$BR\" ]; then cat $TMP/at-branch; else cat $TMP/at-base; fi; exit 1" > "$MAP"
    gate_fixture_run "$BR" repo SPIRA_VERDICT_TTL=600
}

echo "test-gate-base-evidence.sh"

echo "the same suite red on both: charged to the base, named, with the base's own output"
out="$(gate_with '  test-shared.sh                   RED     rc=1 after 2s' \
                 $'  test-shared.sh                   RED     rc=1 after 2s\nBASE-ONLY-EVIDENCE')"
want   "base-red verdict"                 "reason=base-red"       "$out"
want   "the suite is named, not -"        "suite=test-shared.sh"  "$out"
want   "the base's own output is carried" "BASE-ONLY-EVIDENCE"    "$out"

echo "a base trial with only timeouts is NO_VERDICT, not BASE_FAIL"
out="$(gate_with '  test-slow.sh                     TIMEOUT after 600s' \
                 '  test-slow.sh                     TIMEOUT after 600s')"
want   "base-timeout verdict"             "reason=base-timeout"   "$out"
want   "VERDICT is NO_VERDICT"            "VERDICT=NO_VERDICT"    "$out"
want   "timed suite is named"             "suite=test-slow.sh"    "$out"
nowant "not charged as BASE_FAIL"         "VERDICT=BASE_FAIL"     "$out"

echo "a base with both genuine failures and timeouts is still BASE_FAIL"
out="$(gate_with $'  test-real.sh                     RED     rc=1 after 2s\n  test-slow.sh                     TIMEOUT after 600s' \
                 $'  test-real.sh                     RED     rc=1 after 2s\n  test-slow.sh                     TIMEOUT after 600s')"
want   "base-red verdict retained"        "reason=base-red"       "$out"
want   "VERDICT is BASE_FAIL"             "VERDICT=BASE_FAIL"     "$out"

echo "a suite red only on the branch is the branch's, even while the base is red elsewhere"
out="$(gate_with $'  test-shared.sh                   RED     rc=1 after 2s\n  test-mine.sh                     RED     rc=1 after 2s' \
                 '  test-shared.sh                   RED     rc=1 after 2s')"
want   "branch-red verdict"               "reason=branch-red"     "$out"
nowant "not excused as base-red"          "reason=base-red"       "$out"
want   "the branch's own suite is named"  "suite=test-mine.sh"    "$out"

echo "neither trial names a suite: still the base's, with -"
out="$(gate_with 'lint said no' 'lint said no')"
want   "unnamed base red is base-red"     "reason=base-red"       "$out"
want   "suite is -"                       "suite=-"               "$out"

echo "a base trial returning NV (exit 75) is NO_VERDICT, not BASE_FAIL"
# The batch converts a repeat-refused run (exit 2) to exit 75 via the gate command's
# case clause. A base trial that returns NV means we cannot confirm the base is broken;
# it must not charge the branch or hold the repository as BASE_FAIL.
# POSITIVE CONTROL: the case above (both exit 1, unnamed) still produces BASE_FAIL, so
# this silence is not from a guard that never fires.
printf 'repo | %s | push | origin/main |  | %s\n' "$REPO" \
    "if [ \"\$SPIRA_GATE_BRANCH\" = \"$BR\" ]; then exit 1; else exit 75; fi" > "$MAP"
out_nv="$(gate_fixture_run "$BR" repo SPIRA_VERDICT_TTL=600)"
want   "base returning NV gives NO_VERDICT"  "VERDICT=NO_VERDICT" "$out_nv"
nowant "and is not charged as BASE_FAIL"     "VERDICT=BASE_FAIL"  "$out_nv"

# --------------------------------------------------------------------------------------
# UC-gate-verdict-10 — THE ENV CONTRACT REACHES BOTH TRIALS. gate.sh runs the repository's
# own command twice — once on the branch, once on the base — and both must see the same
# shape of env: SPIRA_GATE_SELECT_HEAD names the BRANCH on both calls (it is what a
# repository's own selection logic should key on; SPIRA_GATE_BRANCH itself changes per
# trial and cannot be used for that), SPIRA_GATE_HOST_CORES is the host's real core count
# (not the cgroup-limited nproc a CI preflight would otherwise see, folded in here per
# docs/test-plan/gate-verdict.md section 4 cluster 3 — test-gate-unit.sh already proves
# host_cores() itself is nproc-immune, so this row only has to prove the value reaches CMD).
# --------------------------------------------------------------------------------------
echo "the env contract reaches both trials, unchanged in shape"
ENV_BR="$TMP/env-branch"; ENV_BASE="$TMP/env-base"
_dump="echo BRANCH=\$SPIRA_GATE_BRANCH; echo BASE=\$SPIRA_GATE_BASE;"
_dump="$_dump echo SELECT_HEAD=\$SPIRA_GATE_SELECT_HEAD;"
_dump="$_dump echo HOST_CORES=\$SPIRA_GATE_HOST_CORES; echo EJECTED=\$SPIRA_GATE_EJECTED_SUITES;"
_dump="$_dump echo SUITES=\$SPIRA_GATE_SUITES; cat \"\$SPIRA_GATE_FILES\""
_cmd="if [ \"\$SPIRA_GATE_BRANCH\" = \"$BR\" ]; then f=\"$ENV_BR\"; else f=\"$ENV_BASE\"; fi"
_cmd="$_cmd; { $_dump; } > \"\$f\"; exit 1"
printf 'repo | %s | push | origin/main |  | %s\n' "$REPO" "$_cmd" > "$MAP"
gate_fixture_run "$BR" repo >/dev/null 2>&1
real_cores="$(getconf _NPROCESSORS_ONLN 2>/dev/null)"
branch_env="$(cat "$ENV_BR" 2>/dev/null)"; base_env="$(cat "$ENV_BASE" 2>/dev/null)"
want "branch trial sees its own branch"           "BRANCH=$BR"           "$branch_env"
want "base trial sees the base"                   "BASE=origin/main"     "$base_env"
want "SELECT_HEAD names the branch on the branch trial" "SELECT_HEAD=$BR" "$branch_env"
want "SELECT_HEAD names the branch on the base trial too" "SELECT_HEAD=$BR" "$base_env"
want "the branch trial sees the changed file"      "f1.txt"               "$branch_env"
want "HOST_CORES is the real host count, not a cgroup-limited one" "HOST_CORES=$real_cores" "$branch_env"
want "SUITES defaults to conf.sh's gate-suites path" "SUITES=/" "$branch_env"
want "and names gate-suites"                         "gate-suites"          "$branch_env"

# --------------------------------------------------------------------------------------
# UC-gate-verdict-14 — THE FULL OUTPUT SURVIVES, NOT A `tail -20` WINDOW (folds in
# test-gate-fixture-diag.sh). A diagnostic near the start of a red branch's own output must
# not be pushed out by 25 later "ok" lines — gate.sh embeds the branch's own output whole in
# a branch-red verdict. And a PASSING gate prints none of the command's own output at all:
# verdict() is called as `verdict 0 pass ""`, an empty message, on the one path that never
# looks at what the command printed.
# --------------------------------------------------------------------------------------
echo "the full output survives a red gate, and none of it leaks through a pass"
DIAG_TOKEN="FIXTURE_BUILD_DIAG_SURVIVED"
_noisy="printf 'DIAG: $DIAG_TOKEN\n' >&2; i=1; while [ \$i -le 25 ]; do printf 'suite-%s.sh ok\n' \$i >&2; i=\$((i+1)); done; exit 1"
_cmd="if [ \"\$SPIRA_GATE_BRANCH\" = \"$BR\" ]; then $_noisy; else exit 0; fi"
printf 'repo | %s | push | origin/main |  | %s\n' "$REPO" "$_cmd" > "$MAP"
out="$(gate_fixture_run "$BR" repo)"
want "a diagnostic near the start survives 25 lines of later noise" "$DIAG_TOKEN" "$out"

_cmd2="printf 'DIAG: $DIAG_TOKEN\n' >&2; exit 0"
printf 'repo | %s | push | origin/main |  | %s\n' "$REPO" "$_cmd2" > "$MAP"
pass_out="$(gate_fixture_run "$BR" repo)"
nowant "a passing gate prints none of the command's own output" "$DIAG_TOKEN" "$pass_out"

echo
tl_summary
