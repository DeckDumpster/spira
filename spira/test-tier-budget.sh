#!/usr/bin/env bash
#
# test-tier-budget.sh — per-tier suite wall-time budgets and the shrink-only allowlist
# (sp-5m133): spira/tier-budget.sh's check/check-batch/lint-allowlist against synthetic
# run/tsd/ suite-timing rows (tsd-write, DuckDB), plus the testenv-batch.sh wiring.
#
# WHAT THIS SUITE CHECKS.
#   A. cmd_check against tier budgets: T0-T3 defaults, untagged counts as T1, a suite under
#      budget passes and one over it fails naming tier/budget/measured (the bead's own
#      acceptance case).
#   B. An allowlisted suite is judged against its recorded time (+ margin) instead of its
#      tier budget — passes at its recorded time, fails once it drifts past the margin
#      (the bead's second acceptance case).
#   C. cmd_check_batch judges every suite in a testenv-batch.sh-shaped suite-times.tsv in
#      one pass, naming only the violator.
#   D. lint-allowlist: unchanged passes; a new entry or a raised time fails (the bead's third
#      acceptance case); a removed entry or a lowered time passes (the ratchet's whole point).
#   E. testenv-batch.sh's _tsd_suite_timing (extracted verbatim, not reimplemented) now
#      writes a tier field — MUST FAIL against the pre-sp-5m133 function, which did not.
#      testenv-batch.sh is wired to call tier-budget.sh check-batch (structural).
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control): every refusal case here (over
# budget, new/raised allowlist entry, bad suite name) is paired with the accepting case
# right beside it.
#
# covers: spira/tier-budget.sh spira/testenv-batch.sh spira/tier-budget-allowlist spira/conf.sh
# tier: T2
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL — %s\n' "$1"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1: wanted [$2] in [$3]"; }
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1: did not want [$2] in [$3]"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1: wanted [$2] got [$3]"; }

printf 'test-tier-budget.sh\n'

DUCKDB_BIN="$(command -v duckdb 2>/dev/null || true)"
if [ -z "$DUCKDB_BIN" ]; then
    echo "SKIP test-tier-budget: duckdb not found on PATH"
    exit 77
fi

CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
[ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ] && CARGO_BIN="$HOME/.cargo/bin/cargo"
if [ -z "$CARGO_BIN" ]; then
    echo "SKIP test-tier-budget: cargo not found — tsd-write cannot be built"
    exit 77
fi

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM

TSD_ROOT="$HERE/../tsd"
TSD_BIN="$TSD_ROOT/target/release/tsd-write"
if [ ! -x "$TSD_BIN" ]; then
    cp -r "$TSD_ROOT/." "$T/tsd-src"
    CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$T/tsd-target" \
        "$CARGO_BIN" build --release --manifest-path "$T/tsd-src/Cargo.toml" >/dev/null 2>&1
    TSD_BIN="$T/tsd-target/release/tsd-write"
fi
if [ ! -x "$TSD_BIN" ]; then
    printf 'tsd-write binary not found at %s\n' "$TSD_BIN" >&2
    printf '0 passed, 1 failed\n'
    exit 1
fi

# ── fixture suite dir: one file per tier, plus one untagged ────────────────────────────────
SUITE_DIR="$T/suites"; mkdir -p "$SUITE_DIR"
write_suite() {  # write_suite <name> [<tier-line>]
    {
        printf '#!/usr/bin/env bash\n'
        [ -n "${2:-}" ] && printf '# tier: %s\n' "$2"
        printf 'set -uo pipefail\n'
    } > "$SUITE_DIR/$1"
}
write_suite fixture-t0.sh   T0
write_suite fixture-t1.sh   T1
write_suite fixture-t2.sh   T2
write_suite fixture-t3.sh   T3
write_suite fixture-untagged.sh

row() {  # row <run-dir> <suite> <wall-secs> [<ts>]
    "$TSD_BIN" --family suite-timing --root "$1" --host h1 --ts "${4:-2026-09-25T00:00:00Z}" \
        --field-str "suite=$2" --field "wall_secs=$3" --field rc=0 >/dev/null 2>&1
}

TBS() { bash "$HERE/tier-budget.sh" "$@"; }  # under test, PATH/env otherwise inherited

# ============================================================================================
printf '\n%s\n' "A. cmd_check against tier budgets"
# ============================================================================================
RUNA="$T/runA"; mkdir -p "$RUNA"
row "$RUNA" fixture-t0.sh 0.05
row "$RUNA" fixture-t1.sh 0.05
row "$RUNA" fixture-t2.sh 5
row "$RUNA" fixture-t3.sh 45
row "$RUNA" fixture-untagged.sh 0.05

for s in fixture-t0.sh fixture-t1.sh fixture-t2.sh fixture-t3.sh fixture-untagged.sh; do
    out="$(TBS check "$s" --suite-dir "$SUITE_DIR" --root "$RUNA" 2>&1)"; rc=$?
    is "under budget: $s passes" "0" "$rc"
done

# The acceptance case: a T1 fixture suite that measures 2s fails, naming the budget.
row "$RUNA" fixture-t1-slow.sh 2
write_suite fixture-t1-slow.sh T1
out="$(TBS check fixture-t1-slow.sh --suite-dir "$SUITE_DIR" --root "$RUNA" 2>&1)"; rc=$?
is  "T1 suite measuring 2s fails" "1" "$rc"
want "failure names the tier"    "tier=T1"        "$out"
want "failure names the budget"  "budget=1.000s"  "$out"
want "failure names the measured time" "measured=2" "$out"

# Untagged counts as T1: same fixture with no # tier: line, same violation.
row "$RUNA" fixture-untagged-slow.sh 2
write_suite fixture-untagged-slow.sh
out="$(TBS check fixture-untagged-slow.sh --suite-dir "$SUITE_DIR" --root "$RUNA" 2>&1)"; rc=$?
is  "untagged suite measuring 2s fails as T1" "1" "$rc"
want "failure names tier=T1 for the untagged suite" "tier=T1" "$out"

# T2/T3 over their own (larger) budgets also fail — not just T0/T1.
row "$RUNA" fixture-t2-slow.sh 15
write_suite fixture-t2-slow.sh T2
out="$(TBS check fixture-t2-slow.sh --suite-dir "$SUITE_DIR" --root "$RUNA" 2>&1)"; rc=$?
is "T2 suite over its 10s budget fails" "1" "$rc"
want "T2 failure names its own budget" "budget=10.000s" "$out"

row "$RUNA" fixture-t3-slow.sh 75
write_suite fixture-t3-slow.sh T3
out="$(TBS check fixture-t3-slow.sh --suite-dir "$SUITE_DIR" --root "$RUNA" 2>&1)"; rc=$?
is "T3 suite over its 60s budget fails" "1" "$rc"
want "T3 failure names its own budget" "budget=60.000s" "$out"

# Bad suite name is refused outright (positive control: fixture-t1.sh above already passed).
out="$(TBS check '../escape' --suite-dir "$SUITE_DIR" --root "$RUNA" 2>&1)"; rc=$?
is "a path-shaped suite name is refused" "2" "$rc"

# ============================================================================================
printf '\n%s\n' "B. allowlisted suites are judged against their recorded time, not the tier budget"
# ============================================================================================
RUNB="$T/runB"; mkdir -p "$RUNB"
AL_B="$T/allowlist-b"
printf 'fixture-t1-slow.sh\tT1\t2.0\n' > "$AL_B"

row "$RUNB" fixture-t1-slow.sh 2
out="$(SPIRA_TIER_ALLOWLIST="$AL_B" TBS check fixture-t1-slow.sh --suite-dir "$SUITE_DIR" --root "$RUNB" 2>&1)"; rc=$?
is "allowlisted at its own recorded time (2s) passes" "0" "$rc"

# Positive control the allowlist margin still bites: past recorded*1.2 = 2.4s it fails again.
row "$RUNB" fixture-t1-slow2.sh 2.5 2026-09-25T00:00:01Z
printf 'fixture-t1-slow2.sh\tT1\t2.0\n' >> "$AL_B"
write_suite fixture-t1-slow2.sh T1
out="$(SPIRA_TIER_ALLOWLIST="$AL_B" TBS check fixture-t1-slow2.sh --suite-dir "$SUITE_DIR" --root "$RUNB" 2>&1)"; rc=$?
is  "allowlisted suite past recorded+20% still fails" "1" "$rc"
want "failure names the allowlisted budget (2.0 * 1.2)" "budget=2.400s" "$out"

# ============================================================================================
printf '\n%s\n' "C. cmd_check_batch judges every suite in one pass, naming only the violator"
# ============================================================================================
RUNC="$T/runC"; mkdir -p "$RUNC"
row "$RUNC" fixture-t1.sh 0.05
row "$RUNC" fixture-t1-slow.sh 2
TSV_C="$T/suite-times-c.tsv"
printf 'run1\tspira/sp-test\tfixture-t1.sh\t0\t0.05\t0\t0\tparallel\n' > "$TSV_C"
printf 'run1\tspira/sp-test\tfixture-t1-slow.sh\t0\t2\t0\t0\tparallel\n' >> "$TSV_C"
printf 'run1\tspira/sp-test\t__batch__\t0\t3\t0\t0\tparallel\n' >> "$TSV_C"
out="$(TBS check-batch --suite-dir "$SUITE_DIR" --root "$RUNC" --tsv "$TSV_C" 2>&1)"; rc=$?
is  "batch with one violator fails" "1" "$rc"
want "the violator is named"      "fixture-t1-slow.sh" "$out"
lack "the clean suite is not named"      "tier-budget: fixture-t1.sh " "$out"
lack "the __batch__ row is never judged" "__batch__"          "$out"

RUNC2="$T/runC2"; mkdir -p "$RUNC2"
row "$RUNC2" fixture-t1.sh 0.05
TSV_C2="$T/suite-times-c2.tsv"
printf 'run1\tspira/sp-test\tfixture-t1.sh\t0\t0.05\t0\t0\tparallel\n' > "$TSV_C2"
out="$(TBS check-batch --suite-dir "$SUITE_DIR" --root "$RUNC2" --tsv "$TSV_C2" 2>&1)"; rc=$?
is "a clean batch passes" "0" "$rc"

# ============================================================================================
printf '\n%s\n' "D. lint-allowlist: shrink-only"
# ============================================================================================
CUR_D="$T/allowlist-cur"
PRIOR_D="$T/allowlist-prior"
printf 'test-a.sh\tT1\t2.0\ntest-b.sh\tT2\t12.0\n' > "$PRIOR_D"

# D1: unchanged -> passes.
cp "$PRIOR_D" "$CUR_D"
out="$(SPIRA_TIER_ALLOWLIST="$CUR_D" TBS lint-allowlist --prior "$PRIOR_D" 2>&1)"; rc=$?
is "an unchanged allowlist passes the lint" "0" "$rc"

# D2: a lowered time (shrink) -> passes.
printf 'test-a.sh\tT1\t1.5\ntest-b.sh\tT2\t12.0\n' > "$CUR_D"
out="$(SPIRA_TIER_ALLOWLIST="$CUR_D" TBS lint-allowlist --prior "$PRIOR_D" 2>&1)"; rc=$?
is "a lowered recorded time passes the lint" "0" "$rc"

# D3: a removed entry (shrink) -> passes.
printf 'test-a.sh\tT1\t2.0\n' > "$CUR_D"
out="$(SPIRA_TIER_ALLOWLIST="$CUR_D" TBS lint-allowlist --prior "$PRIOR_D" 2>&1)"; rc=$?
is "a removed entry passes the lint" "0" "$rc"

# D4: the acceptance case — a raised time fails the lint.
printf 'test-a.sh\tT1\t2.5\ntest-b.sh\tT2\t12.0\n' > "$CUR_D"
out="$(SPIRA_TIER_ALLOWLIST="$CUR_D" TBS lint-allowlist --prior "$PRIOR_D" 2>&1)"; rc=$?
is  "a raised recorded time fails the lint" "1" "$rc"
want "the failure names the suite and both times" "test-a.sh raised 2.0s -> 2.5s" "$out"

# D5: a new entry not in the prior state fails the lint.
printf 'test-a.sh\tT1\t2.0\ntest-b.sh\tT2\t12.0\ntest-c.sh\tT1\t3.0\n' > "$CUR_D"
out="$(SPIRA_TIER_ALLOWLIST="$CUR_D" TBS lint-allowlist --prior "$PRIOR_D" 2>&1)"; rc=$?
is  "a new entry fails the lint" "1" "$rc"
want "the failure names the new entry" "new entry test-c.sh" "$out"

# D6: an empty prior state (this file's own introducing commit has no prior at all) reports
# every current entry as new — a real violation (rc=1) against that base, by design; a
# caller seeding the allowlist for the first time compares against --base/--prior that
# resolves to nothing at all (see cmd_lint_allowlist), not an empty file, and gets rc=0.
EMPTY_D="$T/allowlist-empty"; : > "$EMPTY_D"
out="$(SPIRA_TIER_ALLOWLIST="$CUR_D" TBS lint-allowlist --prior "$EMPTY_D" 2>&1)"; rc=$?
is "every entry against an empty (but existing) prior file is reported as new" "1" "$rc"

# D7: no prior resolves at all (the allowlist's own introducing commit) — passes outright,
# never iterated as though every line were new.
out="$(SPIRA_TIER_ALLOWLIST="$CUR_D" TBS lint-allowlist --prior "$T/no-such-prior-file" 2>&1)"; rc=$?
is "no prior found at all passes as an introducing commit" "0" "$rc"

# ============================================================================================
printf '\n%s\n' "E. testenv-batch.sh wiring"
# ============================================================================================
# E1: _tsd_suite_timing (extracted verbatim) now writes a tier field — MUST FAIL against the
# pre-sp-5m133 function, which had no --field-str tier=.
FUNCS="$T/funcs.sh"
sed -n '/^_tsd_suite_timing() {/,/^}/p' "$HERE/testenv-batch.sh" > "$FUNCS"
[ -s "$FUNCS" ] || bad "could not extract _tsd_suite_timing from testenv-batch.sh"
RUNE="$T/runE"; mkdir -p "$RUNE"
(
    export SPIRA_RUN="$RUNE" SPIRA_TSD_BIN="$TSD_BIN" SUITE_DIR="$SUITE_DIR"
    BR="spira/sp-test"; _BATCH_RUN_ID="runE"
    . "$HERE/suite-covers.sh"
    . "$FUNCS"
    _tsd_suite_timing "fixture-t1.sh" "0" "1" "0" "0" "parallel"
)
FAME="$RUNE/tsd/suite-timing.jsonl"
[ -f "$FAME" ] && ok "suite-timing row appended by the extracted function" \
                || bad "MUST-FAIL CHECK: no suite-timing row"
if [ -f "$FAME" ]; then
    tier_written="$(python3 -c '
import json
print(json.loads(open("'"$FAME"'").readlines()[0]).get("tier"))
')"
    is "MUST-FAIL CHECK: the row carries tier=T1 (absent in the pre-sp-5m133 function)" \
       "T1" "$tier_written"
fi

# E2: testenv-batch.sh is wired to invoke tier-budget.sh check-batch (structural).
want "testenv-batch.sh calls tier-budget.sh check-batch" \
     "tier-budget.sh\" check-batch" "$(cat "$HERE/testenv-batch.sh")"

printf '\ntest-tier-budget.sh: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
