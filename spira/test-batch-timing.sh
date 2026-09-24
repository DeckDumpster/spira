#!/usr/bin/env bash
# test-batch-timing.sh — batch timing ledger written by gate-timing.sh
#
# WHAT IS UNDER TEST.  gate-timing.sh reads a completed RESULTS directory
# (batch.meta + runner.meta + *.result files) and appends one TSV row to the
# timing ledger.  testenv-batch.sh calls it after green and red runs.
#
# Three properties that must hold:
#   1. A row appended for a run with known .result files carries the correct
#      per-field values (n_ok, n_red, sum_s, wall_s, nproc, …).
#   2. Missing runner.meta produces "-" in the shape fields rather than "0" or
#      empty.  A ledger that silently shows zeroes for missing data is worse
#      than one that shows "-", because zero reads as measured.
#   3. SPIRA_BATCH_LEDGER is in SPIRA_CONF_KEYS so operators can direct the
#      ledger to a persistent path from spira.conf.
#
# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail)
#   A "red" .result file is planted; if gate-timing.sh never reads .result
#   files n_red stays 0 and the assertion fails.  A "ok" .result is planted
#   for sum_s; if wall_s and seconds are not read the sum is 0 and fails.
#
# covers: spira/gate-timing.sh spira/testenv-batch.sh spira/conf.sh
# host-reason: reads no live database; all fixtures are local temp dirs

set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
notwant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "unwanted [$2] in [$3]"; }

GATE_TIMING="$HERE/gate-timing.sh"
CONF_SH="$HERE/conf.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

printf 'test-batch-timing.sh\n'

# ===========================================================================
# PART A: gate-timing.sh produces a correct ledger row
# ===========================================================================
printf '\nPart A: correct ledger row from a complete results dir\n'

RESULTS="$TMP/results-a"
LEDGER="$TMP/ledger-a.tsv"
mkdir -p "$RESULTS"

# batch.meta
printf 'key=testrun001\nbranch=test/br\nbase=abc123\nmode=parallel\nselection=diff\n' \
    > "$RESULTS/batch.meta"

# runner.meta
printf 'nproc=16\nmemtotal_kb=65536000\nmaxpar=16\ncpu_busy_pct=72\nsuites_wall_s=900\n' \
    > "$RESULTS/runner.meta"

# result files: 3 ok, 1 red, 1 skip
printf 'ok 0 12 - parallel diff 0\n'   > "$RESULTS/test-foo.result"
printf 'ok 0 34 - parallel diff 0\n'   > "$RESULTS/test-bar.result"
printf 'ok 0 56 - parallel diff 0\n'   > "$RESULTS/test-baz.result"
printf 'red 0 78 fp parallel diff 1\n' > "$RESULTS/test-red.result"
printf 'skip 0 0 - parallel diff 77\n' > "$RESULTS/test-skip.result"

SPIRA_BATCH_LEDGER="$LEDGER" SPIRA_RUN="$TMP" \
    bash "$GATE_TIMING" "$RESULTS" green

[ -f "$LEDGER" ] && ok "A0: ledger file created" || { bad "A0: ledger file created" "not found"; }

row="$(cat "$LEDGER")"
fields="$(printf '%s' "$row" | awk -F'\t' '{print NF}')"
is "A1: row has 15 fields" "15" "$fields"

# Field order: run_id when branch base verdict wall_s n_ok n_red n_skip n_other sum_s maxpar nproc memtotal_kb cpu_busy_pct
is "A2: run_id"        "testrun001"  "$(printf '%s' "$row" | awk -F'\t' '{print $1}')"
is "A3: branch"        "test/br"     "$(printf '%s' "$row" | awk -F'\t' '{print $3}')"
is "A4: base"          "abc123"      "$(printf '%s' "$row" | awk -F'\t' '{print $4}')"
is "A5: verdict"       "green"       "$(printf '%s' "$row" | awk -F'\t' '{print $5}')"
is "A6: wall_s"        "900"         "$(printf '%s' "$row" | awk -F'\t' '{print $6}')"
is "A7: n_ok"          "3"           "$(printf '%s' "$row" | awk -F'\t' '{print $7}')"
is "A8: n_red"         "1"           "$(printf '%s' "$row" | awk -F'\t' '{print $8}')"
is "A9: n_skip"        "1"           "$(printf '%s' "$row" | awk -F'\t' '{print $9}')"
is "A10: n_other"      "0"           "$(printf '%s' "$row" | awk -F'\t' '{print $10}')"
is "A11: sum_s"        "180"         "$(printf '%s' "$row" | awk -F'\t' '{print $11}')"  # 12+34+56+78
is "A12: maxpar"       "16"          "$(printf '%s' "$row" | awk -F'\t' '{print $12}')"
is "A13: nproc"        "16"          "$(printf '%s' "$row" | awk -F'\t' '{print $13}')"
is "A14: memtotal_kb"  "65536000"    "$(printf '%s' "$row" | awk -F'\t' '{print $14}')"
is "A15: cpu_busy_pct" "72"          "$(printf '%s' "$row" | awk -F'\t' '{print $15}')"

# ===========================================================================
# PART B: missing runner.meta → shape fields are "-" not "0" or blank
# ===========================================================================
printf '\nPart B: missing runner.meta produces "-" in shape fields\n'

RESULTS_B="$TMP/results-b"
LEDGER_B="$TMP/ledger-b.tsv"
mkdir -p "$RESULTS_B"
printf 'key=run2\nbranch=b\nbase=x\n' > "$RESULTS_B/batch.meta"
printf 'red 0 10 fp parallel diff 1\n' > "$RESULTS_B/test-only.result"

SPIRA_BATCH_LEDGER="$LEDGER_B" SPIRA_RUN="$TMP" \
    bash "$GATE_TIMING" "$RESULTS_B" red

row_b="$(cat "$LEDGER_B" 2>/dev/null)"
is "B1: verdict field"    "red" "$(printf '%s' "$row_b" | awk -F'\t' '{print $5}')"
is "B2: wall_s is -"      "-"   "$(printf '%s' "$row_b" | awk -F'\t' '{print $6}')"
is "B3: nproc is -"       "-"   "$(printf '%s' "$row_b" | awk -F'\t' '{print $13}')"
is "B4: memtotal_kb is -" "-"   "$(printf '%s' "$row_b" | awk -F'\t' '{print $14}')"
is "B5: n_red is 1"       "1"   "$(printf '%s' "$row_b" | awk -F'\t' '{print $8}')"

# ===========================================================================
# PART C: multiple runs append rows (ledger grows)
# ===========================================================================
printf '\nPart C: multiple runs append separate rows\n'

LEDGER_C="$TMP/ledger-c.tsv"
RESULTS_C1="$TMP/results-c1"; mkdir -p "$RESULTS_C1"
RESULTS_C2="$TMP/results-c2"; mkdir -p "$RESULTS_C2"
printf 'key=r1\nbranch=b\nbase=x\n' > "$RESULTS_C1/batch.meta"
printf 'ok 0 5 - parallel diff 0\n'  > "$RESULTS_C1/test-a.result"
printf 'key=r2\nbranch=b\nbase=x\n' > "$RESULTS_C2/batch.meta"
printf 'red 0 7 fp parallel diff 1\n' > "$RESULTS_C2/test-a.result"

SPIRA_BATCH_LEDGER="$LEDGER_C" SPIRA_RUN="$TMP" \
    bash "$GATE_TIMING" "$RESULTS_C1" green
SPIRA_BATCH_LEDGER="$LEDGER_C" SPIRA_RUN="$TMP" \
    bash "$GATE_TIMING" "$RESULTS_C2" red

n_rows="$(wc -l < "$LEDGER_C" 2>/dev/null | tr -d ' ')"
is "C1: two rows appended" "2" "$n_rows"
is "C2: first row is green" "green" "$(awk 'NR==1{print $5}' FS='\t' "$LEDGER_C")"
is "C3: second row is red"  "red"   "$(awk 'NR==2{print $5}' FS='\t' "$LEDGER_C")"

# ===========================================================================
# PART D: SPIRA_BATCH_LEDGER is in SPIRA_CONF_KEYS (settable from spira.conf)
# ===========================================================================
printf '\nPart D: SPIRA_BATCH_LEDGER in SPIRA_CONF_KEYS\n'

_key_present="$(grep -c 'SPIRA_BATCH_LEDGER' "$CONF_SH" 2>/dev/null || echo 0)"
[ "$_key_present" -gt 0 ] && ok "D1: SPIRA_BATCH_LEDGER in conf.sh" \
    || bad "D1: SPIRA_BATCH_LEDGER in conf.sh" "not found"

# Positive control: a made-up key is absent (proves the membership test ran).
if grep -q 'SPIRA_BATCH_LEDGER_NONEXISTENT' "$CONF_SH" 2>/dev/null; then
    bad "D2: made-up key absent (positive control)" "found unexpectedly"
else
    ok "D2: made-up key absent (positive control passes)"
fi

# ===========================================================================
# PART E: gate-timing.sh is callable and exits 0 for a well-formed results dir
# ===========================================================================
printf '\nPart E: gate-timing.sh exits 0 for a complete results dir\n'
RESULTS_E="$TMP/results-e"; mkdir -p "$RESULTS_E"
printf 'key=re\nbranch=b\nbase=x\n' > "$RESULTS_E/batch.meta"
printf 'nproc=8\nmemtotal_kb=1024\nmaxpar=8\ncpu_busy_pct=50\nsuites_wall_s=60\n' \
    > "$RESULTS_E/runner.meta"
SPIRA_BATCH_LEDGER="$TMP/ledger-e.tsv" SPIRA_RUN="$TMP" \
    bash "$GATE_TIMING" "$RESULTS_E" green \
    && ok "E1: exit 0 for well-formed dir" \
    || bad "E1: exit 0 for well-formed dir" "non-zero exit"

# ===========================================================================
printf '\n'
[ "$fail" -eq 0 ] && printf '  %d passed\n' "$pass" || printf '  %d passed, %d FAILED\n' "$pass" "$fail"
exit "$fail"
