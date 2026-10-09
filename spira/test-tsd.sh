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

# ── tsd-write (law-absence-needs-a-positive-control: no binary, no suite) ──────────────────
# tsd-write is the tree's own build, on the suite's PATH (sp-gypjk) — never built here.
command -v tsd-write >/dev/null 2>&1 || bail "tsd-write is not on PATH"
TSD_BIN=tsd-write

# (this crate's own #[test]s run once per round as the workspace unit-test step, not here.)

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

DB5="$T/db5"; mkdir -p "$DB5"

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
    qout() { tl_config SPIRA_RUN="$RUN8"
             SPIRA_HOME="$T" SPIRA_DB="$DB5" SPIRA_REPO="$HERE/.." SPIRA_CONF=/nonexistent \
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
    qout9() { tl_config SPIRA_RUN="$RUN9"
              SPIRA_HOME="$T" SPIRA_DB="$DB5" SPIRA_REPO="$HERE/.." SPIRA_CONF=/nonexistent \
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

# ============================================================================================
printf '\n%s\n' "12. tsd-query.sh: where, rework, time, slots, sentinel, bead — known values, speed, refusal"
# ============================================================================================
if [ -z "$DUCKDB_BIN" ]; then
    echo "SKIP section 12: duckdb not found — tsd-query.sh needs it on PATH"
else
    RUN12="$T/run12"; mkdir -p "$RUN12/tsd"
    qout12() { tl_config SPIRA_RUN="$RUN12"
               SPIRA_HOME="$T" SPIRA_DB="$DB5" SPIRA_REPO="$HERE/.." SPIRA_CONF=/nonexistent \
                 tsd-query.sh "$@" 2>&1; }
    jget12() { python3 -c 'import json,sys; r=json.load(sys.stdin); print(eval(sys.argv[1]))' "$1"; }

    python3 -I - "$RUN12/tsd" <<'PY'
import json, sys, time, datetime
d = sys.argv[1]
now = time.time()
def iso(age): return datetime.datetime.fromtimestamp(now - age, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
def w(fam, rows, extra=()):
    with open(f"{d}/{fam}.jsonl", "w") as f:
        for r in rows: f.write(json.dumps({"family": fam, **r}) + "\n")
        for line in extra: f.write(line + "\n")
stage = []
def st(key, seq, to, age, reason=None, frm="X"):
    r = {"ts": iso(age), "seq": seq, "machine": "bead", "key": key, "event": "e", "from_state": frm,
         "to_state": to, "applied": True, "actor": "a", "source": "lifecycle"}
    if reason: r["reason"] = reason
    stage.append(r)
for k, seq, to, age, reason in [
    ("sp-a", 1, "READY", 10800, None), ("sp-a", 2, "WORKING", 7200, None), ("sp-a", 3, "SUBMITTED", 3600, None),
    ("sp-b", 1, "WORKING", 1800, None),
    ("sp-c", 1, "WORKING", 20000, None), ("sp-c", 2, "SUBMITTED", 19000, None), ("sp-c", 3, "REWORK", 18000, "gate-red"),
    ("sp-c", 4, "WORKING", 17000, None), ("sp-c", 5, "SUBMITTED", 16000, None), ("sp-c", 6, "LANDED", 15000, None),
    ("sp-d", 1, "WORKING", 9000, None), ("sp-d", 2, "REWORK", 8000, "gate-red"),
    ("sp-e", 1, "WORKING", 5000, None), ("sp-e", 2, "REWORK", 4000, "ejected"), ("sp-e", 3, "LANDED", 2000, None)]:
    st(k, seq, to, age, reason)
stage.append({"ts": iso(100), "seq": 1, "machine": "batch", "key": "r1", "from_state": "OPEN", "to_state": "REWORK", "applied": True})
w("bead-stage", stage)
slots = [{"ts": iso(3600 - 60 * i), "live": 4, "ceiling": 6, "ready": 2} for i in range(10)]
slots += [{"ts": iso(3600 - 60 * i), "live": 5, "ceiling": 6, "ready": 0} for i in range(10, 14)]
w("slots", slots, extra=["not json at all", '{"ts":"x","live":"?","ceiling":"?","ready":"?","family":"slots"}'])
ph = []
for n, (a, b) in enumerate([(2, 8), (3, 27), (1, 9)]):
    ph += [{"ts": iso(500 - n), "pass": f"p{n}", "check": "CHECK1", "secs": a},
           {"ts": iso(500 - n), "pass": f"p{n}", "check": "CHECK2", "secs": b}]
w("sentinel-phase", ph, extra=["{broken"])
w("aeon-session", [{"ts": iso(100), "bead": "sp-a", "fayth": "builder", "status": "submitted", "wall_s": 100},
                   {"ts": iso(90), "bead": "sp-b", "fayth": "builder", "status": "submitted", "wall_s": 50}])
w("gate-run", [{"ts": iso(100), "bead": "sp-a", "status": "GREEN", "reason": "x", "ran_secs": 30},
               {"ts": iso(90), "bead": "sp-b", "status": "RED", "reason": "y", "ran_secs": 20}])
w("batch-round", [{"ts": iso(100), "duration_ms": "4000"}])
PY

    out="$(qout12 where)"
    is "where: three stages occupied"          "3" "$(printf '%s' "$out" | jget12 'len(r)')"
    is "where: states in pipeline order"       "WORKING SUBMITTED REWORK" "$(printf '%s' "$out" | jget12 '" ".join(x["state"] for x in r)')"
    is "where: one bead WORKING (B; C and E left it)" "1" "$(printf '%s' "$out" | jget12 'r[0]["wip"]')"
    is "where: SUBMITTED dwell is A's age, about an hour" "1" \
        "$(printf '%s' "$out" | jget12 '1 if 3600 <= r[1]["dwell_p50_s"] <= 3700 else 0')"
    lack "where: batch-machine rows never count as bead stages" '"wip":2' "$out"

    out="$(qout12 rework)"
    is "rework: total reopens"                 "3" "$(printf '%s' "$out" | jget12 'r[0]["reopens"]')"
    is "rework: landed beads"                  "2" "$(printf '%s' "$out" | jget12 'r[0]["landed"]')"
    is "rework: reasons, most frequent first"  "gate-red:2 ejected:1" \
        "$(printf '%s' "$out" | jget12 '" ".join("%s:%s" % (x["reason"], x["reopens"]) for x in r[1:])')"

    out="$(qout12 slots)"
    is "slots: empty slot-minutes while work was ready" "20" "$(printf '%s' "$out" | jget12 'r[0]["empty_with_work_min"]')"
    is "slots: empty slot-minutes with nothing ready"   "3"  "$(printf '%s' "$out" | jget12 'r[0]["empty_idle_min"]')"

    out="$(qout12 time)"
    is "time: aeon seconds"  "150"  "$(printf '%s' "$out" | jget12 'r[0]["aeon_s"]')"
    is "time: gate seconds"  "50"   "$(printf '%s' "$out" | jget12 'r[0]["gate_s"]')"
    is "time: round seconds" "4"    "$(printf '%s' "$out" | jget12 'r[0]["round_s"]')"
    is "time: empty-slot seconds" "1380" "$(printf '%s' "$out" | jget12 'r[0]["idle_slot_s"]')"

    out="$(qout12 sentinel)"
    is "sentinel: p50 pass seconds" "10" "$(printf '%s' "$out" | jget12 'int(r[0]["p50"])')"
    is "sentinel: p90 pass seconds" "26" "$(printf '%s' "$out" | jget12 'int(r[0]["p90"])')"
    is "sentinel: top phase leads" "CHECK2:44" "$(printf '%s' "$out" | jget12 'r[0]["top_phases"][0]')"

    out="$(qout12 bead sp-c)"
    is "bead: six stage rows, oldest first" "WORKING" "$(printf '%s' "$out" | jget12 'r[0]["what"].split(" -> ")[1]')"
    is "bead: the rework reason is on the timeline" "1" "$(printf '%s' "$out" | jget12 'sum(1 for x in r if "(gate-red)" in x["what"])')"
    out="$(qout12 bead sp-a)"
    is "bead: aeon session joins the timeline" "1" "$(printf '%s' "$out" | jget12 'sum(1 for x in r if x["kind"]=="aeon")')"
    out="$(qout12 bead 'x; drop')"; rc=$?
    [ "$rc" -ne 0 ] && ok "bead: bad id refused" || bad "bead: bad id accepted"

    mv "$RUN12/tsd/bead-stage.jsonl" "$RUN12/tsd/bead-stage.away"
    for q in where rework "bead sp-a"; do
        qout12 $q >/dev/null; rc=$?
        [ "$rc" -ne 0 ] && ok "$q: missing family exits non-zero, never an empty answer" || bad "$q: missing family exited 0"
    done
    mv "$RUN12/tsd/bead-stage.away" "$RUN12/tsd/bead-stage.jsonl"

    RUN13="$T/run13"; mkdir -p "$RUN13/tsd"
    python3 -I - "$RUN13/tsd" <<'PY'
import json, sys, time, datetime, random
d = sys.argv[1]; now = time.time(); random.seed(1)
def iso(age): return datetime.datetime.fromtimestamp(now - age, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
D = 30 * 86400
def w(fam, rows):
    with open(f"{d}/{fam}.jsonl", "w") as f:
        for r in rows: f.write(json.dumps({"family": fam, **r}) + "\n")
w("slots", [{"ts": iso(D - 60 * i), "live": 3, "ceiling": 6, "ready": 1} for i in range(D // 60)])
w("sentinel-phase", [{"ts": iso(D - 100 * (i // 4)), "pass": f"p{i // 4}", "check": f"C{i % 4}", "secs": random.randint(0, 9)} for i in range(4 * D // 100)])
st = []
for b in range(4000):
    t0 = random.randint(3600, D)
    for q, (to, dt) in enumerate([("READY", 0), ("WORKING", 300), ("SUBMITTED", 900), ("LANDED", 1500)]):
        st.append({"ts": iso(max(t0 - dt, 1)), "seq": q + 1, "machine": "bead", "key": f"sp-{b}", "from_state": "X", "to_state": to, "applied": True})
w("bead-stage", st)
w("aeon-session", [{"ts": iso(random.randint(1, D)), "bead": f"sp-{i % 4000}", "fayth": "builder", "status": "ok", "wall_s": 100} for i in range(6000)])
w("gate-run", [{"ts": iso(random.randint(1, D)), "bead": f"sp-{i % 4000}", "status": "GREEN", "ran_secs": 90} for i in range(6000)])
PY
    for q in where rework slots time sentinel "bead sp-7"; do
        out=$(tl_config SPIRA_RUN="$RUN13" SPIRA_HOME="$T" SPIRA_DB="$DB5" SPIRA_REPO="$HERE/.." SPIRA_CONF=/nonexistent \
            tsd-query.sh $q 2>/dev/null); rc=$?
        [ "$rc" -eq 0 ] && [ -n "$out" ] && ok "$q over 30 days of rows answers" || bad "$q over 30 days: rc=$rc"
    done
fi

tl_summary
