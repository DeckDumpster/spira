#!/usr/bin/env bash
#
# test-tsd.sh — the run/tsd/ time-series layer (sp-sbc6o): the tsd-write row format and IO
# seam, its wiring into land_mark (landing-event) and testenv-batch.sh's suite-times ledger
# (suite-timing), and tsd-query.sh's DuckDB queries.
#
# WHAT THIS SUITE CHECKS.
#   1. tsd-write builds and appends a well-formed row: envelope (ts, host, family) merged
#      with the caller's fields.
#   2. --field JSON-sniffs (numbers, bools); --field-str never does — a git SHA that happens
#      to be all digits stays a string.
#   3. A field colliding with an envelope key (ts/host/family) is refused, and refused before
#      any line is appended.
#   4. An invalid family name (uppercase, space, path separator) is refused, no file created.
#   5. Neither --root nor SPIRA_RUN set is refused with a clear message.
#   6. Concurrent writers to the same family never interleave a line (flock).
#   7. land_mark (lib.sh) appends a landing-event row alongside its existing landstate file —
#      MUST FAIL against the previous lib.sh, which had no such row.
#   8. land_mark still succeeds, unchanged, when tsd-write fails —
#      the tsd row is best-effort, landing itself never depends on it.
#   9. testenv-batch.sh's suite-times hook (_append_suite_times) appends a suite-timing row —
#      the sole producer since the git-notes ledger and suite-times.sh were retired.
#  10. tsd-query.sh: baseline (avg), rate (count/hours), dwell (quantile), and by-group
#      (per-group avg, longest-first — sp-ezkp3's LPT lookup) against a synthetic fixture,
#      plus refusals for a bad family, a missing family, and a bad field name.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control): every refusal case (3, 4, 5, 8's
# sibling in reverse) is paired with the accepting case, so a check that never fires is caught
# rather than trusted on silence.
#
# tier: T2
# covers: tsd/src/lib.rs tsd/src/main.rs spira/tsd-query.sh spira/deps.toml spira/conf.sh
#         spira/lib.sh testenv/src/* spira/testenv/Containerfile
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1: did not want [$2] in [$3]"; }

printf 'test-tsd.sh\n'

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM

# ── build tsd-write (law-absence-needs-a-positive-control: no binary, no suite) ────────────
CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
[ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ] && CARGO_BIN="$HOME/.cargo/bin/cargo"
[ -n "$CARGO_BIN" ] || skip "cargo not found — tsd-write binary cannot be built"
# tsd-write is the tree's own build, on the suite's PATH (sp-gypjk) — never built here.
command -v tsd-write >/dev/null 2>&1 || bail "tsd-write is not on PATH"
TSD_BIN=tsd-write

# ── tsd's own #[test] units (tsd/src/lib.rs) ────────────────────────────────────────────────
CARGO_TEST_LOG="$T/cargo-test-tsd.log"
CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$T/tsd-test-target" \
    "$CARGO_BIN" test --locked -p tsd --manifest-path "$HERE/../Cargo.toml" \
    --no-fail-fast > "$CARGO_TEST_LOG" 2>&1
_cargo_test_rc=$?
report_cargo "$CARGO_TEST_LOG" "$_cargo_test_rc"

jpy() {  # jpy <file> <python-expr-on-"rows"> — rows is a list of parsed JSON lines
    python3 -c '
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
print(eval(sys.argv[2]))
' "$1" "$2"
}

# ============================================================================================
printf '\n%s\n' "1-2. a written row carries the envelope and JSON-sniffs correctly"
# ============================================================================================
RUN1="$T/run1"; mkdir -p "$RUN1"
"$TSD_BIN" --family suite-timing --root "$RUN1" --host h1 --ts 2026-09-25T00:00:00Z \
    --field-str suite=test-foo.sh --field duration_ms=1500 --field ok=true \
    --field-str tip=00000000000000000000000000000000000042
FAM1="$RUN1/tsd/suite-timing.jsonl"
[ -f "$FAM1" ] && ok "family file created" || bad "family file not created: $FAM1"
is "envelope: ts"     "2026-09-25T00:00:00Z" "$(jpy "$FAM1" 'rows[0]["ts"]')"
is "envelope: host"   "h1"                    "$(jpy "$FAM1" 'rows[0]["host"]')"
is "envelope: family" "suite-timing"          "$(jpy "$FAM1" 'rows[0]["family"]')"
is "--field JSON-sniffs a number"  "1500" "$(jpy "$FAM1" 'rows[0]["duration_ms"]')"
is "--field JSON-sniffs a bool"    "True" "$(jpy "$FAM1" 'rows[0]["ok"]')"
is "--field-str keeps an all-digit SHA a string" \
   "00000000000000000000000000000000000042" "$(jpy "$FAM1" 'rows[0]["tip"]')"

# Append-only: a second write adds a line, does not replace the first.
"$TSD_BIN" --family suite-timing --root "$RUN1" --host h1 --ts 2026-09-25T00:01:00Z \
    --field-str suite=test-bar.sh --field duration_ms=200
is "append-only: two writes -> two lines" "2" "$(jpy "$FAM1" 'len(rows)')"
is "first row untouched by the second write" \
   "test-foo.sh" "$(jpy "$FAM1" 'rows[0]["suite"]')"

# ============================================================================================
printf '\n%s\n' "3. a field colliding with the envelope is refused, before any line lands"
# ============================================================================================
RUN2="$T/run2"; mkdir -p "$RUN2"
"$TSD_BIN" --family landing-event --root "$RUN2" --field host=spoofed >/dev/null 2>&1
rc=$?
[ "$rc" -ne 0 ] && ok "host-colliding field refused (rc=$rc)" || bad "host collision accepted"
[ -f "$RUN2/tsd/landing-event.jsonl" ] \
    && bad "a line was appended despite the refused field" \
    || ok "no file created on refusal"

# Positive control: the same call minus the collision succeeds.
"$TSD_BIN" --family landing-event --root "$RUN2" --field-str state=LANDED >/dev/null 2>&1
[ -f "$RUN2/tsd/landing-event.jsonl" ] \
    && ok "the same call without the collision succeeds" \
    || bad "non-colliding call was also refused"

# ============================================================================================
printf '\n%s\n' "4. an invalid family name is refused; a valid one is not"
# ============================================================================================
RUN3="$T/run3"; mkdir -p "$RUN3"
for badfam in "Suite" "has space" "../escape" "a/b"; do
    "$TSD_BIN" --family "$badfam" --root "$RUN3" >/dev/null 2>&1
    rc=$?
    [ "$rc" -ne 0 ] && ok "family '$badfam' refused (rc=$rc)" || bad "family '$badfam' accepted"
done
[ -d "$RUN3/tsd" ] && bad "tsd/ was created despite every family being invalid" \
                    || ok "no tsd/ directory created for any invalid family"
"$TSD_BIN" --family suite-timing --root "$RUN3" >/dev/null 2>&1
[ -f "$RUN3/tsd/suite-timing.jsonl" ] && ok "a valid family name is accepted" \
                                       || bad "a valid family name was refused"

# ============================================================================================
printf '\n%s\n' "5. no --root and no SPIRA_RUN is refused"
# ============================================================================================
out="$(env -u SPIRA_RUN "$TSD_BIN" --family suite-timing 2>&1)"; rc=$?
[ "$rc" -ne 0 ] && ok "missing root refused (rc=$rc)" || bad "missing root accepted"
want "and it says why" "SPIRA_RUN" "$out"

# ============================================================================================
printf '\n%s\n' "6. concurrent writers to the same family never interleave a line"
# ============================================================================================
RUN4="$T/run4"; mkdir -p "$RUN4"
_pids=""
for i in $(seq 1 20); do
    "$TSD_BIN" --family landing-event --root "$RUN4" --host "h$i" \
        --field-str "bead=sp-concurrent-$i" --field-str state=LANDED &
    _pids="$_pids $!"
done
wait $_pids
FAM4="$RUN4/tsd/landing-event.jsonl"
is "20 concurrent writers -> 20 lines" "20" "$(jpy "$FAM4" 'len(rows)')"
is "every line parses as one JSON object (no interleaving)" \
   "20" "$(jpy "$FAM4" 'sum(1 for r in rows if isinstance(r, dict))')"

# ============================================================================================
printf '\n%s\n' "7-8. land_mark (lib.sh) writes a landing-event row, best-effort"
# ============================================================================================
# land_mark is now a one-line shim onto `landing-pass mark` (sp-cnnt6, "wave 4.16"), which
# does this dual-write in-process rather than shelling to tsd-write — the tree's own build,
# same reasoning as TSD_BIN above.
command -v landing-pass >/dev/null 2>&1 || bail "landing-pass is not on PATH"
RUN5="$T/run5"; DB5="$T/db5"; mkdir -p "$RUN5" "$DB5"
(
    export SPIRA_HOME="$T" SPIRA_RUN="$RUN5" SPIRA_DB="$DB5" SPIRA_REPO="$HERE/.." SPIRA_CONF=/nonexistent
    set -uo pipefail
    . "$HERE/lib.sh"
    landing-pass mark "sp-landtest" "LANDED" "deadbeef" ""
    echo "land_mark_rc=$?"
) > "$T/land_mark.out" 2>&1
land_out="$(cat "$T/land_mark.out")"
want "land_mark still succeeds" "land_mark_rc=0" "$land_out"
[ -f "$RUN5/landstate/sp-landtest" ] && ok "landstate file still written (unchanged behaviour)" \
                                      || bad "landstate file missing — land_mark's own job regressed"
FAM5="$RUN5/tsd/landing-event.jsonl"
[ -f "$FAM5" ] && ok "landing-event row appended" || bad "MUST-FAIL CHECK: no landing-event row (old lib.sh behaviour)"
if [ -f "$FAM5" ]; then
    is "landing-event: bead"  "sp-landtest" "$(jpy "$FAM5" 'rows[0]["bead"]')"
    is "landing-event: state" "LANDED"      "$(jpy "$FAM5" 'rows[0]["state"]')"
    is "landing-event: tip"   "deadbeef"    "$(jpy "$FAM5" 'rows[0]["tip"]')"
fi

# Best-effort: a tsd row that cannot be written must not break land_mark's own job. The
# write is in-process now (landing-pass/src/landstate.rs), so the failure planted is a tsd/
# path that cannot become a directory, not a failing tsd-write binary on PATH.
RUN6="$T/run6"; mkdir -p "$RUN6"
: > "$RUN6/tsd"   # a plain file where the tsd family dir must go: mkdir -p on it fails
(
    export SPIRA_HOME="$T" SPIRA_RUN="$RUN6" SPIRA_DB="$DB5" SPIRA_REPO="$HERE/.." SPIRA_CONF=/nonexistent
    set -uo pipefail
    . "$HERE/lib.sh"
    landing-pass mark "sp-landtest2" "LANDED" "cafef00d" ""
    echo "land_mark_rc=$?"
) > "$T/land_mark2.out" 2>&1
land_out2="$(cat "$T/land_mark2.out")"
want "land_mark succeeds even when the tsd row cannot be written" "land_mark_rc=0" "$land_out2"
[ -f "$RUN6/landstate/sp-landtest2" ] && ok "landstate still written with the tsd write failing" \
                                       || bad "landstate broke when the tsd write failed"
[ -f "$RUN6/tsd/landing-event.jsonl" ] && bad "a tsd row appeared despite the tsd write failing" \
                                        || ok "no tsd row written when the tsd write fails"

# ============================================================================================
printf '\n%s\n' "9. suite-timing rows (RETIRED here)"
# ============================================================================================
# The suite-times hook moved with spira/testenv-batch.sh into the testenv crate:
# testenv/src/timing.rs (append_writes_jsonl_under_run_tsd, rows_carry_the_envelope_and_typed_fields).

# ============================================================================================
printf '\n%s\n' "10. tsd-query.sh: baseline, rate, dwell, by-group, and their refusals"
# ============================================================================================
DUCKDB_BIN="$(command -v duckdb 2>/dev/null || true)"
if [ -z "$DUCKDB_BIN" ]; then
    echo "SKIP section 10: duckdb not found — tsd-query.sh needs it on PATH"
else
    RUN8="$T/run8"; mkdir -p "$RUN8"
    for v in 10 20 30 40 50; do
        "$TSD_BIN" --family suite-timing --root "$RUN8" --host h1 --ts 2026-09-25T00:00:00Z \
            --field-str suite=fixture.sh --field "wall_secs=$v"
    done
    qout() { SPIRA_HOME="$T" SPIRA_RUN="$RUN8" SPIRA_DB="$DB5" SPIRA_REPO="$HERE/.." SPIRA_CONF=/nonexistent \
                tsd-query.sh "$@" 2>&1; }

    out="$(qout baseline suite-timing wall_secs 999999)"
    want "baseline: avg(wall_secs) over 5 rows is 30" '"baseline":30' "$out"

    out="$(qout rate suite-timing 999999)"
    want "rate: 5 rows counted" '"n":5' "$out"

    out="$(qout dwell suite-timing wall_secs 0.5)"
    want "dwell: p0.5 of [10,20,30,40,50] is 30" '30' "$out"

    out="$(qout baseline "Bad Family" wall_secs 1)"; rc=$?
    [ "$rc" -ne 0 ] && ok "bad family name refused" || bad "bad family name accepted"

    out="$(qout baseline never-written wall_secs 1)"; rc=$?
    [ "$rc" -ne 0 ] && ok "a family with no rows yet is refused, not queried as empty" \
                    || bad "a nonexistent family was silently queried"

    out="$(qout baseline suite-timing "bad field" 1)"; rc=$?
    [ "$rc" -ne 0 ] && ok "bad field name refused" || bad "bad field name accepted"

    # by-group: per-group avg, longest-first, tab-separated (sp-ezkp3's LPT lookup).
    # other.sh (avg 150) must sort ahead of fixture.sh (avg 30) — proving GROUP BY
    # actually separates the two suites rather than averaging across all rows.
    "$TSD_BIN" --family suite-timing --root "$RUN8" --host h1 --ts 2026-09-25T00:00:01Z \
        --field-str suite=other.sh --field "wall_secs=100"
    "$TSD_BIN" --family suite-timing --root "$RUN8" --host h1 --ts 2026-09-25T00:00:02Z \
        --field-str suite=other.sh --field "wall_secs=200"

    out="$(qout by-group suite-timing suite wall_secs)"
    is "by-group: first row is the higher-avg group (other.sh)" \
       "other.sh" "$(printf '%s\n' "$out" | head -1 | cut -f1)"
    want "by-group: other.sh's avg is 150" "150" "$(printf '%s\n' "$out" | head -1 | cut -f2)"
    is "by-group: second row is fixture.sh" \
       "fixture.sh" "$(printf '%s\n' "$out" | sed -n 2p | cut -f1)"
    want "by-group: fixture.sh's avg is still 30 (unaffected by other.sh's rows)" \
         "30" "$(printf '%s\n' "$out" | sed -n 2p | cut -f2)"

    out="$(qout by-group suite-timing "Bad Group" wall_secs)"; rc=$?
    [ "$rc" -ne 0 ] && ok "by-group: bad group field name refused" \
                    || bad "by-group: bad group field name accepted"
fi

# ============================================================================================
printf '\n%s\n' "11. tsd-query.sh: suite-p50, suite-medians, last-run, slow-in-branch"
# ============================================================================================
# ACCEPTANCE: one query answers p50 wall time of a suite over its last 10 runs,
# local and CI both present. Two hosts stand in for "local" and "CI"; two older, out-of-window
# rows prove the "last 10" limit is live, not decorative (law-absence-needs-a-positive-control).
if [ -z "$DUCKDB_BIN" ]; then
    echo "SKIP section 11: duckdb not found — tsd-query.sh needs it on PATH"
else
    RUN9="$T/run9"; mkdir -p "$RUN9"
    qout9() { SPIRA_HOME="$T" SPIRA_RUN="$RUN9" SPIRA_DB="$DB5" SPIRA_REPO="$HERE/.." SPIRA_CONF=/nonexistent \
                tsd-query.sh "$@" 2>&1; }

    "$TSD_BIN" --family suite-timing --root "$RUN9" --host ancient-local --ts 2026-09-24T22:00:00Z \
        --field-str suite=acc.sh --field-str run_id=old1 --field-str branch=spira/sp-x \
        --field wall_secs=9999 --field rc=0 --field-str mode=parallel
    "$TSD_BIN" --family suite-timing --root "$RUN9" --host ancient-ci --ts 2026-09-24T22:30:00Z \
        --field-str suite=acc.sh --field-str run_id=old2 --field-str branch=spira/sp-x \
        --field wall_secs=9999 --field rc=0 --field-str mode=parallel

    i=0
    for w in 10 20 30 40 50 60 70 80 90 100; do
        i=$((i + 1))
        if [ $((i % 2)) -eq 1 ]; then h=local-dev; else h=gha-runner-1; fi
        "$TSD_BIN" --family suite-timing --root "$RUN9" --host "$h" \
            --ts "$(printf '2026-09-25T00:00:%02dZ' "$i")" \
            --field-str suite=acc.sh --field-str "run_id=r$i" --field-str branch=spira/sp-x \
            --field "wall_secs=$w" --field rc=0 --field-str mode=parallel
    done
    "$TSD_BIN" --family suite-timing --root "$RUN9" --host local-dev --ts 2026-09-25T00:00:11Z \
        --field-str suite=quick.sh --field-str run_id=r11 --field-str branch=spira/sp-x \
        --field wall_secs=5 --field rc=0 --field-str mode=parallel
    "$TSD_BIN" --family suite-timing --root "$RUN9" --host local-dev --ts 2026-09-25T00:00:12Z \
        --field-str suite=__batch__ --field-str run_id=r11 --field-str branch=spira/sp-x \
        --field wall_secs=15 --field rc=0 --field-str mode=parallel

    FAM9="$RUN9/tsd/suite-timing.jsonl"
    is "fixture carries local-host rows"  "5" "$(jpy "$FAM9" 'sum(1 for r in rows if r["host"]=="local-dev" and r["suite"]=="acc.sh")')"
    is "fixture carries CI-host rows"     "5" "$(jpy "$FAM9" 'sum(1 for r in rows if r["host"]=="gha-runner-1" and r["suite"]=="acc.sh")')"

    out="$(qout9 suite-p50 acc.sh 10)"
    want "suite-p50: p50 of last 10 runs (local+CI) is 55" '"p50":55' "$out"
    want "suite-p50: n is 10, excludes the 2 older out-of-window rows" '"n":10' "$out"

    out="$(qout9 suite-p50 acc.sh 12)"
    lack "suite-p50 with n=12 is NOT 55 — proves the window limit is live, not decorative" \
        '"p50":55' "$out"

    out="$(qout9 suite-p50 "bad suite!" 10)"; rc=$?
    [ "$rc" -ne 0 ] && ok "suite-p50: bad suite name refused" || bad "suite-p50: bad suite name accepted"

    out="$(qout9 suite-p50 acc.sh 0)"; rc=$?
    [ "$rc" -ne 0 ] && ok "suite-p50: n=0 refused" || bad "suite-p50: n=0 accepted"

    out="$(qout9 suite-medians 10)"
    want "suite-medians: acc.sh median is 55"   '"suite":"acc.sh","median":55' "$out"
    want "suite-medians: quick.sh median is 5"  '"suite":"quick.sh","median":5' "$out"

    out="$(qout9 last-run)"
    want "last-run: run_id is the most recently stamped row"  '"run_id":"r11"' "$out"
    want "last-run: sum_wall excludes __batch__ (5 from quick.sh)" '"sum_wall":5' "$out"
    want "last-run: batch_wall is the __batch__ row's wall_secs" '"batch_wall":15' "$out"

    out="$(qout9 slow-in-branch spira/sp-x 3)"
    is "slow-in-branch: the two 9999s lead" \
        "9999 9999" "$(printf '%s' "$out" | python3 -c 'import json,sys; print(" ".join(str(r["wall_secs"]) for r in json.load(sys.stdin)[:2]))')"
    is "slow-in-branch: __batch__ never appears" \
        "0" "$(printf '%s' "$out" | python3 -c 'import json,sys; print(sum(1 for r in json.load(sys.stdin) if r["suite"]=="__batch__"))')"
fi

tl_summary
