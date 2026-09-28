#!/usr/bin/env bash
#
# test-reconciler-flow.sh — reconciler-flow: backlog trend, stage velocity against a
# trailing 24h baseline (with optional per-stage floors), stage dwell, and round health
# (flip rate), over the run/tsd/ time series. Reads the `bead-stage` family (design
# reconciler-time-series-2026-09-27 §3) — the lifecycle machine's own projected states —
# never `landing-event`, which belongs to the abolished landing-certification stage.
#
# WHAT THIS SUITE CHECKS
#   1. A fresh environment (no bead-stage rows yet) reports UNOBSERVABLE for velocity,
#      dwell and round-health — never SATISFIED (law-a-control-that-cannot-check-must-refuse).
#      Backlog trend is SATISFIED with no history: there is nothing yet to have grown past.
#   2. POSITIVE CONTROL, then the design's own literal test case: with a healthy baseline and
#      work waiting to land, a healthy current rate is SATISFIED; a current rate of zero
#      with work still waiting is a GAP — but not yet inside its grace period.
#   3. The same gap, still open on the next pass past its grace period, is confirmed
#      (is_gap) and alerts the Concierge mailbox — the only action a flow gap ever gets,
#      since it has no deterministic remedy. Once confirmed, the same unresolved streak
#      does not alert again on the following pass — deduplicated per gap, not per pass
#      (reconciler_engine::alert::should_alert, law-repeating-conditions-escalate-once).
#   4. A velocity floor from the desired-state document fires even when the trailing
#      baseline ratio alone would not.
#   5. Backlog trend: growth well past the trailing baseline is a gap; ordinary variance
#      is not.
#   6. Stage dwell: a p95 past its configured limit, pooled over every in-flight bead and
#      batch state, is a gap; short dwell is not.
#   7. Round health: a round reverting from CI_RUNNING to ATTRIBUTING (the batch machine's
#      own Red transition) repeatedly is a gap; a quiet window is not.
#   8. A duckdb that cannot run at all (not the file — the query engine) is UNOBSERVABLE
#      too, distinct from a missing family file.
#   10. Unobservable has its own grace period (SPIRA_FLOW_UNOBSERVABLE_GRACE_SECS, default
#       1h), decoupled from a gap's (SPIRA_FLOW_GRACE_SECS): outlasting the gap grace alone
#       does not confirm it.
#   11. Unobservable past its own grace period alerts the Concierge exactly once, through
#       the same deduplicated path a gap uses (sp-fufyb) — never once per pass.
#
# Driven through the real binary against a real bd (testdb.sh) and a real duckdb over
# hand-written run/tsd/ fixtures — not a model of either (law-prefer-the-real-dependency).
#
# defect: sp-rh0x3
# covers: reconciler-flow/src/**.rs spira/conf.sh spira/build-tarball.sh
#         systemd/spira-reconciler-flow.service systemd/spira-reconciler-flow.timer
#         install/src/manifest.rs install/src/bin/units_install.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

. "$HERE/testlib.sh"
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-reconciler-flow

command -v duckdb >/dev/null 2>&1 || { echo "SKIP test-reconciler-flow: duckdb not on PATH"; exit 77; }


T="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$T"' EXIT INT TERM
testdb_up reconcilerflow || { echo "test-reconciler-flow: could not build a fixture database"; exit 1; }

# reconciler-flow is the tree under test's own build, on PATH (sp-gypjk).
command -v reconciler-flow >/dev/null 2>&1 || { echo "test-reconciler-flow: reconciler-flow is not on PATH" >&2; exit 1; }

export SPIRA_HOME="$HERE"
export SPIRA_RUN="$T/run"
export SPIRA_DUCKDB_BIN="duckdb"
export SPIRA_BD="${TESTDB_BD:-bd-embedded}"
export SPIRA_FLOW_WINDOW_HOURS="0.5"
export SPIRA_FLOW_BASELINE_HOURS="24"
export SPIRA_FLOW_GRACE_SECS="2"   # short so the suite need not sleep for a real 30m window
export SPIRA_DESIRED_DIR="$T/desired"
export SPIRA_SCOPE_LABEL=""
mkdir -p "$SPIRA_RUN"

# Stub mail: records every "send concierge" call so the alert path is observable without
# a real mailbox. Any other subcommand is refused loudly — a call this suite did not expect.
MAIL_LOG="$T/mail-calls.log"
cat > "$T/mail" <<STUB
#!/usr/bin/env bash
if [ "\$1" = "send" ] && [ "\$2" = "concierge" ]; then
    { printf 'CALL %s\n' "\$*"; cat; printf '\n---\n'; } >> "$MAIL_LOG"
    exit 0
fi
printf 'stub mail: unexpected invocation: %s\n' "\$*" >&2
exit 1
STUB
chmod +x "$T/mail"
export SPIRA_MAIL_SH="$T/mail"

run_pass() { reconciler-flow --pass >"$T/pass-out.log" 2>&1; }
status_of() {
    # Last reconciler-status.jsonl line for key $1, field $2 ("status" or "is_gap").
    # Under run/tsd/ (design reconciler-time-series-2026-09-27 §2, sp-69m85).
    python3 -c '
import json, sys
path, key, field = sys.argv[1], sys.argv[2], sys.argv[3]
last = None
with open(path) as f:
    for line in f:
        line = line.strip()
        if not line:
            continue
        row = json.loads(line)
        if row.get("key") == key:
            last = row
if last is None:
    print("NO-ROW")
else:
    print(last.get(field))
' "$SPIRA_RUN/tsd/reconciler-status.jsonl" "$1" "$2"
}
mail_count() {
    local n
    n="$(grep -c '^CALL send concierge' "$MAIL_LOG" 2>/dev/null)"
    printf '%s' "${n:-0}"
}
mail_count_for() {
    # Alerts naming invariant $1 specifically — a plain reset_env leaves velocity, dwell
    # and round-health unobservable together (none of them has a bead-stage row yet),
    # so a raw mail_count conflates three invariants' independent alerts into one number.
    local n
    n="$(grep -c "^invariant: $1\$" "$MAIL_LOG" 2>/dev/null)"
    printf '%s' "${n:-0}"
}

epoch_iso() { date -u -d "@$1" +%Y-%m-%dT%H:%M:%SZ; }
SEQ_COUNTER=0
emit_stage() {
    # emit_stage <machine> <key> <from_state> <to_state> <age_seconds_ago> — one bead-stage
    # row (tsd-lifecycle-export's own shape, sp-qz2yj). seq is assigned in call order, so a
    # caller emitting a key's rows oldest-first gets them ordered the way the real exporter
    # would have numbered them.
    local machine="$1" key="$2" from="$3" to="$4" age="$5" ts now
    now="$(date +%s)"
    ts="$(epoch_iso "$((now - age))")"
    SEQ_COUNTER=$((SEQ_COUNTER + 1))
    mkdir -p "$SPIRA_RUN/tsd"
    printf '{"ts":"%s","host":"t","family":"bead-stage","seq":%s,"machine":"%s","key":"%s","event":"e","from_state":"%s","to_state":"%s","applied":true,"actor":"a","source":"legacy"}\n' \
        "$ts" "$SEQ_COUNTER" "$machine" "$key" "$from" "$to" >> "$SPIRA_RUN/tsd/bead-stage.jsonl"
}
# emit_landed <bead> <age_seconds_ago> — a bead's transition into LANDED, the only shape
# velocity_metrics reads; the from_state does not matter to it.
emit_landed() { emit_stage bead "$1" IN_DELIVERY LANDED "$2"; }
# emit_waiting <bead> <age_seconds_ago> — makes <bead>'s latest bead-stage row SUBMITTED, one
# of the states waiting_to_land counts as still needing to land.
emit_waiting() { emit_stage bead "$1" WORKING SUBMITTED "$2"; }
write_flow_doc() {
    # write_flow_doc <velocity_floor_queue> <dwell_limit_review>
    mkdir -p "$SPIRA_DESIRED_DIR/versions"
    printf '1\n' > "$SPIRA_DESIRED_DIR/current"
    cat > "$SPIRA_DESIRED_DIR/versions/000001.toml" <<TOML
[meta]
version = 1
content_hash = "fixture"
materialized_at = "2026-09-25T00:00:00Z"

[[resource]]
apiVersion = "spira/v1"
kind = "Flow"
producer = "test"
[resource.metadata]
name = "flow"
[resource.spec.velocity_floor]
queue = $1
[resource.spec.dwell_limit_seconds]
review = $2
TOML
}
seed_beads() {
    # seed_beads <count> — open beads in the fixture store, for backlog_count.
    local n="$1" i
    for i in $(seq 1 "$n"); do
        printf '{"id":"rcf-b%s","title":"t","status":"open","issue_type":"task","labels":["plan"],"updated_at":"2026-09-04T00:00:00Z"}\n' "$i"
    done | testdb_seed
}
emit_slots_sample() {
    # emit_slots_sample <live> <ceiling> <ready> <paused 0|1> <age_seconds_ago> — one run/tsd/
    # slots row (sp-69m85), oldest-first is not required: reconciler-flow orders by its own ts.
    local live="$1" ceiling="$2" ready="$3" paused="$4" age="$5" ts
    ts="$(epoch_iso "$(($(date +%s) - age))")"
    mkdir -p "$SPIRA_RUN/tsd"
    printf '{"ts":"%s","host":"t","family":"slots","live":%s,"ceiling":%s,"ready":%s,"capacity_paused":%s}\n' \
        "$ts" "$live" "$ceiling" "$ready" "$paused" >> "$SPIRA_RUN/tsd/slots.jsonl"
}
emit_sentinel_phase() {
    # emit_sentinel_phase <pass> <check> <secs> <age_seconds_ago> — one run/tsd/sentinel-phase
    # row (sp-69m85).
    local pass="$1" check="$2" secs="$3" age="$4" ts
    ts="$(epoch_iso "$(($(date +%s) - age))")"
    mkdir -p "$SPIRA_RUN/tsd"
    printf '{"ts":"%s","host":"t","family":"sentinel-phase","pass":"%s","check":"%s","secs":%s}\n' \
        "$ts" "$pass" "$check" "$secs" >> "$SPIRA_RUN/tsd/sentinel-phase.jsonl"
}
emit_bead_stage() {
    # emit_bead_stage <key> <to_state> <age_seconds_ago> <seq> — one run/tsd/bead-stage row
    # (sp-qz2yj), applied, in the lifecycle's own state names.
    local key="$1" to_state="$2" age="$3" seq="$4" ts
    ts="$(epoch_iso "$(($(date +%s) - age))")"
    mkdir -p "$SPIRA_RUN/tsd"
    printf '{"ts":"%s","host":"t","family":"bead-stage","seq":%s,"machine":"bead","key":"%s","event":"e","from_state":"X","to_state":"%s","applied":true,"actor":"a","source":"legacy"}\n' \
        "$ts" "$seq" "$key" "$to_state" >> "$SPIRA_RUN/tsd/bead-stage.jsonl"
}
reset_env() {
    testdb_reset
    rm -rf "$SPIRA_RUN" "$SPIRA_DESIRED_DIR"
    mkdir -p "$SPIRA_RUN"
    : > "$MAIL_LOG"
}

# ============================================================================
echo "1. Fresh environment: unobservable, never satisfied — except backlog, which has"
echo "   nothing to compare against yet"
# ============================================================================
reset_env
run_pass
is  "flow:velocity:queue is unobservable with no bead-stage rows" unobservable "$(status_of flow:velocity:queue status)"
is  "flow:dwell:review is unobservable with no bead-stage rows"   unobservable "$(status_of flow:dwell:review status)"
is  "flow:round-health is unobservable with no bead-stage rows"   unobservable "$(status_of flow:round-health status)"
is  "flow:backlog is satisfied with no baseline history"             satisfied    "$(status_of flow:backlog status)"
is  "no concierge alert on an all-unobservable/satisfied pass" "0" "$(mail_count)"

# ============================================================================
echo
echo "2. Velocity: positive control (healthy), then the design's own case — zero rate"
echo "   with work waiting to land"
# ============================================================================
reset_env
# Baseline: a landing every ~2h for the last 24h (12 events -> 0.5/h baseline).
for h in 2 4 6 8 10 12 14 16 18 20 22 24; do emit_landed "rcf-base-$h" $((h*3600)); done

echo "flow:velocity:queue — no work waiting: a zero current rate is fine"
run_pass
is "no work waiting -> satisfied even at zero current rate" satisfied "$(status_of flow:velocity:queue status)"

echo "flow:velocity:queue — work waiting to land, healthy current rate -> satisfied"
emit_waiting rcf-c1 60
emit_landed rcf-recent 60   # one LANDED a minute ago: current window is healthy
run_pass
is "work waiting to land with a healthy current rate -> satisfied" satisfied "$(status_of flow:velocity:queue status)"

echo "flow:velocity:queue — work waiting to land, zero current rate -> gap, inside grace"
reset_env
for h in 2 4 6 8 10 12 14 16 18 20 22 24; do emit_landed "rcf-base-$h" $((h*3600)); done
emit_waiting rcf-c1 60
run_pass
is  "zero rate with work waiting is a gap"        gap   "$(status_of flow:velocity:queue status)"
is  "but not yet confirmed — inside its grace period" False "$(status_of flow:velocity:queue is_gap)"
is  "no concierge alert while inside grace" "0" "$(mail_count)"

# ============================================================================
echo
echo "3. The same gap, past its grace period, is confirmed and alerts the Concierge"
# ============================================================================
sleep 3
run_pass
is  "the gap is confirmed on the next pass past grace" True "$(status_of flow:velocity:queue is_gap)"
is  "exactly one alert fired for the first confirmed pass" "1" "$(mail_count)"
want "the alert names the gap" "flow:velocity:queue" "$(cat "$MAIL_LOG")"

echo "the same unresolved streak does not alert again on the next pass"
run_pass
is  "still confirmed as a gap" True "$(status_of flow:velocity:queue is_gap)"
is  "no additional alert for the same streak" "1" "$(mail_count)"

# ============================================================================
echo
echo "4. A velocity floor from the desired-state document fires even when the baseline"
echo "   ratio alone would not"
# ============================================================================
reset_env
# Trailing baseline (2h window, overridden below): 3 events -> 1.5/h -> a baseline-ratio
# threshold of 0.75/h. Current window: 1 event in the last 30 minutes -> 2.0/h, which
# clears that threshold — but not a document floor of 3.0/h.
emit_landed rcf-base-1 3600
emit_landed rcf-base-2 7000
emit_landed rcf-recent 300
emit_waiting rcf-c1 60
write_flow_doc 3.0 999999
SPIRA_FLOW_BASELINE_HOURS=2 run_pass
is "a configured floor fires even when the baseline ratio alone would not" gap "$(status_of flow:velocity:queue status)"

# ============================================================================
echo
echo "5. Backlog trend: growth past the trailing baseline is a gap; ordinary variance is not"
# ============================================================================
reset_env
now="$(date +%s)"
mkdir -p "$SPIRA_RUN/tsd"
for h in 1 4 8 12 16 20 24; do
    ts="$(epoch_iso "$((now - h*3600))")"
    printf '{"ts":"%s","host":"t","family":"backlog","count":10}\n' "$ts" >> "$SPIRA_RUN/tsd/backlog.jsonl"
done
seed_beads 11
run_pass
is "ordinary variance (11 vs a baseline of 10) is satisfied" satisfied "$(status_of flow:backlog status)"

reset_env
mkdir -p "$SPIRA_RUN/tsd"
for h in 1 4 8 12 16 20 24; do
    ts="$(epoch_iso "$((now - h*3600))")"
    printf '{"ts":"%s","host":"t","family":"backlog","count":10}\n' "$ts" >> "$SPIRA_RUN/tsd/backlog.jsonl"
done
seed_beads 20
run_pass
is "growth well past the baseline (20 vs 10) is a gap" gap "$(status_of flow:backlog status)"

# ============================================================================
echo
echo "6. Stage dwell: p95 past its configured limit, pooled over every in-flight bead and"
echo "   batch state, is a gap; short dwell is not"
# ============================================================================
reset_env
write_flow_doc 0 3600
for i in 1 2 3 4 5; do
    emit_stage bead "rcf-dw-$i" READY   WORKING   $((300 + i*60 + 1000))  # entering WORKING
    emit_stage bead "rcf-dw-$i" WORKING SUBMITTED $((300 + i*60))         # leaving WORKING: ~1000s dwell, under the limit
done
run_pass
is "dwell well under its configured limit is satisfied" satisfied "$(status_of flow:dwell:review status)"

reset_env
write_flow_doc 0 3600
for i in 1 2 3 4 5; do
    emit_stage bead "rcf-dw-$i" READY   WORKING   $((300 + i*60 + 9000))  # entering WORKING
    emit_stage bead "rcf-dw-$i" WORKING SUBMITTED $((300 + i*60))         # leaving WORKING: ~9000s dwell, over the limit
done
run_pass
is "dwell past its configured limit is a gap" gap "$(status_of flow:dwell:review status)"

# ============================================================================
echo
echo "7. Round health: a round reverting from CI_RUNNING to ATTRIBUTING (the batch machine's"
echo "   own Red transition) repeatedly is a gap; a quiet window is not"
# ============================================================================
reset_env
for i in 1 2 3 4 5 6 7 8; do
    emit_stage batch "rcf-rh-$i" OPEN       CI_RUNNING $((600 + i*30 + 60))
    emit_stage batch "rcf-rh-$i" CI_RUNNING GREEN      $((600 + i*30))
done
run_pass
is "a quiet window with no flips is satisfied" satisfied "$(status_of flow:round-health status)"

reset_env
# A healthy history (outside the current 30-minute window, inside the 24h baseline) so the
# baseline itself reflects "normal", not the thrash the current window is about to have —
# otherwise the current window's own bad data would dilute the very baseline it is compared
# against, and a first-ever thrash would never clear a ratio-based threshold.
for i in $(seq 1 40); do
    age=$((3600 + i*300))
    emit_stage batch "rcf-rh-healthy-$i" OPEN       CI_RUNNING $((age + 60))
    emit_stage batch "rcf-rh-healthy-$i" CI_RUNNING GREEN      "$age"
done
for i in 1 2 3 4 5 6 7 8; do
    emit_stage batch "rcf-rh-$i" OPEN       CI_RUNNING $((600 + i*30 + 60))
    emit_stage batch "rcf-rh-$i" CI_RUNNING ATTRIBUTING $((600 + i*30))   # every one flips red
done
run_pass
is "thrashing (every round reverting to red) is a gap" gap "$(status_of flow:round-health status)"

# ============================================================================
echo
echo "8. An unreadable duckdb (the engine itself, not the file) is unobservable — never"
echo "   satisfied and never mistaken for the missing-family case"
# ============================================================================
reset_env
emit_landed rcf-any 60
SPIRA_DUCKDB_BIN="$T/no-such-duckdb-binary" run_pass
is "a broken duckdb binary is unobservable, not satisfied" unobservable "$(status_of flow:velocity:queue status)"

# ============================================================================
echo
echo "9. A truly fresh box: SPIRA_RUN itself does not exist yet (install.sh normally"
echo "   creates it once; this binary must not depend on that having survived — a bash"
echo "   watcher gets it again on every invocation via lib.sh, this binary sources nothing)"
# ============================================================================
reset_env
rm -rf "$SPIRA_RUN"
SAVED_SPIRA_RUN="$SPIRA_RUN"
reconciler-flow --pass >"$T/pass-out.log" 2>&1
rc=$?
is "a pass exits 0 even when SPIRA_RUN does not exist yet" "0" "$rc"
[ -d "$SAVED_SPIRA_RUN" ] && ok "the pass creates SPIRA_RUN itself" || bad "the pass creates SPIRA_RUN itself" "still missing: $SAVED_SPIRA_RUN"
mkdir -p "$SPIRA_RUN"

# ============================================================================
echo
echo "10. Unobservable has its own grace period, decoupled from a gap's: outlasting the"
echo "    short gap grace (SPIRA_FLOW_GRACE_SECS=2, this suite's default) alone must not"
echo "    confirm an unobservable invariant against its own, longer grace"
# ============================================================================
reset_env
SPIRA_FLOW_UNOBSERVABLE_GRACE_SECS=999 run_pass
is "unobservable, first pass, inside its own long grace" unobservable "$(status_of flow:velocity:queue status)"
sleep 3
SPIRA_FLOW_UNOBSERVABLE_GRACE_SECS=999 run_pass
is "still unobservable past the gap's own 2s grace" unobservable "$(status_of flow:velocity:queue status)"
is "not confirmed — inside its own (999s) grace" False "$(status_of flow:velocity:queue is_gap)"
is "no concierge alert while inside the unobservable grace" "0" "$(mail_count_for flow:velocity:queue)"

# ============================================================================
echo
echo "11. Unobservable past its own grace period alerts the Concierge exactly once,"
echo "    through the same deduplicated path a gap uses (sp-fufyb) — not once per pass"
# ============================================================================
reset_env
SPIRA_FLOW_UNOBSERVABLE_GRACE_SECS=2 run_pass
is "unobservable, first pass, inside grace" unobservable "$(status_of flow:velocity:queue status)"
is "no alert yet" "0" "$(mail_count_for flow:velocity:queue)"
sleep 3
SPIRA_FLOW_UNOBSERVABLE_GRACE_SECS=2 run_pass
is "confirmed past its own grace" True "$(status_of flow:velocity:queue is_gap)"
is "exactly one alert fired for the first confirmed pass" "1" "$(mail_count_for flow:velocity:queue)"
want "the alert names the invariant" "flow:velocity:queue" "$(cat "$MAIL_LOG")"

echo "the same unresolved streak does not alert again on the next pass"
SPIRA_FLOW_UNOBSERVABLE_GRACE_SECS=2 run_pass
is "still unobservable" unobservable "$(status_of flow:velocity:queue status)"
is "no additional alert for the same streak" "1" "$(mail_count_for flow:velocity:queue)"

# ============================================================================
echo
echo "12. Idle capacity: a free slot while ready work exists, across two consecutive slots"
echo "    samples, is a gap; a single blip, a full fleet, a paused fleet or nothing ready is not"
# ============================================================================
reset_env
run_pass
is "no slots rows yet -> unobservable" unobservable "$(status_of flow:idle-capacity status)"

reset_env
emit_slots_sample 8 8 12 0 60
emit_slots_sample 8 8 12 0 30
run_pass
is "a full fleet (live == ceiling) is satisfied" satisfied "$(status_of flow:idle-capacity status)"

reset_env
emit_slots_sample 8 8 12 0 60   # busy
emit_slots_sample 3 8 12 0 30   # idle, but only the most recent sample
run_pass
is "one idle sample after a busy one is not yet a gap" satisfied "$(status_of flow:idle-capacity status)"

reset_env
emit_slots_sample 3 8 12 1 60   # idle, but capacity is deliberately paused
emit_slots_sample 3 8 12 1 30
run_pass
is "idle capacity while deliberately paused is satisfied" satisfied "$(status_of flow:idle-capacity status)"

reset_env
emit_slots_sample 3 8 0 0 60    # idle, but nothing is ready to claim
emit_slots_sample 3 8 0 0 30
run_pass
is "idle capacity with nothing ready to claim is satisfied" satisfied "$(status_of flow:idle-capacity status)"

reset_env
emit_slots_sample 3 8 12 0 60   # idle
emit_slots_sample 3 8 12 0 30   # idle again — positive control
run_pass
is "idle capacity across two consecutive samples is a gap" gap "$(status_of flow:idle-capacity status)"
is "but not yet confirmed — inside its grace period" False "$(status_of flow:idle-capacity is_gap)"
is "no concierge alert while inside grace" "0" "$(mail_count_for flow:idle-capacity)"
sleep 3
run_pass
is "the gap is confirmed on the next pass past grace" True "$(status_of flow:idle-capacity is_gap)"
is "exactly one alert fired" "1" "$(mail_count_for flow:idle-capacity)"
run_pass
is "the same unresolved streak does not alert again" "1" "$(mail_count_for flow:idle-capacity)"

# ============================================================================
echo
echo "13. Sentinel overrun: a pass's own wall time (its sentinel-phase rows summed) past twice"
echo "    the configured timer period is a gap; the most recent pass is the one that counts"
# ============================================================================
reset_env
SPIRA_FLOW_SENTINEL_PERIOD_SECS=100 run_pass
is "no sentinel-phase rows yet -> unobservable" unobservable "$(status_of flow:sentinel-overrun status)"

reset_env
emit_sentinel_phase pass-1 CHECK1 60 90
emit_sentinel_phase pass-1 CHECK2 60 30   # pass-1 total: 120s, under 2x100=200
SPIRA_FLOW_SENTINEL_PERIOD_SECS=100 run_pass
is "a pass under twice the period is satisfied" satisfied "$(status_of flow:sentinel-overrun status)"

reset_env
emit_sentinel_phase pass-old CHECK1 500 3600   # an old pass, badly overrun, but stale
emit_sentinel_phase pass-new CHECK1 60 30      # the most recent pass: fine
SPIRA_FLOW_SENTINEL_PERIOD_SECS=100 run_pass
is "an old overrunning pass does not matter once a newer pass is fine" satisfied "$(status_of flow:sentinel-overrun status)"

reset_env
emit_sentinel_phase pass-2 CHECK1 150 90
emit_sentinel_phase pass-2 CHECK2 150 30   # pass-2 total: 300s, over 2x100=200 — positive control
SPIRA_FLOW_SENTINEL_PERIOD_SECS=100 run_pass
is "a pass past twice the period is a gap" gap "$(status_of flow:sentinel-overrun status)"
is "but not yet confirmed — inside its grace period" False "$(status_of flow:sentinel-overrun is_gap)"
sleep 3
SPIRA_FLOW_SENTINEL_PERIOD_SECS=100 run_pass
is "the gap is confirmed on the next pass past grace" True "$(status_of flow:sentinel-overrun is_gap)"
is "exactly one alert fired" "1" "$(mail_count_for flow:sentinel-overrun)"
SPIRA_FLOW_SENTINEL_PERIOD_SECS=100 run_pass
is "the same unresolved streak does not alert again" "1" "$(mail_count_for flow:sentinel-overrun)"

# ============================================================================
echo
echo "14. Rework: reopens per landed bead over its own (6h default, pinned here to 3h) window,"
echo "    above 1.0, is a gap — but report-only until 24h of bead-stage history exist"
# ============================================================================
reset_env
SPIRA_FLOW_REWORK_WINDOW_HOURS=3 run_pass
is "no bead-stage rows yet -> unobservable" unobservable "$(status_of flow:rework status)"

reset_env
# 24h+ of history (warm), a healthy ratio within the 3h rework window.
for h in 1 6 12 18 24 30; do emit_bead_stage "rcf-warm-$h" LANDED $((h*3600)) "$h"; done
emit_bead_stage rcf-r1 REWORK 3600 100
emit_bead_stage rcf-l1 LANDED 3000 101
emit_bead_stage rcf-l2 LANDED 1800 102
SPIRA_FLOW_REWORK_WINDOW_HOURS=3 run_pass
is "warm history, a healthy ratio, is satisfied" satisfied "$(status_of flow:rework status)"

reset_env
# 24h+ of history (warm) again, but this time the ratio crosses the threshold — positive control.
for h in 1 6 12 18 24 30; do emit_bead_stage "rcf-warm2-$h" LANDED $((h*3600)) "$h"; done
emit_bead_stage rcf-r2 REWORK 3600 200
emit_bead_stage rcf-r3 REWORK 3000 201
emit_bead_stage rcf-r4 REWORK 2400 202
emit_bead_stage rcf-l3 LANDED 1800 203
SPIRA_FLOW_REWORK_WINDOW_HOURS=3 run_pass
is "warm history, ratio over threshold, is a gap" gap "$(status_of flow:rework status)"
is "but not yet confirmed — inside its grace period" False "$(status_of flow:rework is_gap)"
is "no alert while inside grace" "0" "$(mail_count_for flow:rework)"
sleep 3
SPIRA_FLOW_REWORK_WINDOW_HOURS=3 run_pass
is "the gap is confirmed on the next pass past grace" True "$(status_of flow:rework is_gap)"
is "exactly one alert fired" "1" "$(mail_count_for flow:rework)"

reset_env
# Only ~2h of bead-stage history — under the 24h warm-up — but a ratio far over the threshold.
emit_bead_stage rcf-cold-1 LANDED 7200 1
emit_bead_stage rcf-r5 REWORK 3600 2
emit_bead_stage rcf-r6 REWORK 3000 3
SPIRA_FLOW_REWORK_WINDOW_HOURS=3 run_pass
is "still recorded as a gap even before the warm-up ends (report-only, not hidden)" gap "$(status_of flow:rework status)"
is "no alert before 24h of history exist, even with a ratio this far over" "0" "$(mail_count_for flow:rework)"
sleep 3
SPIRA_FLOW_REWORK_WINDOW_HOURS=3 run_pass
is "confirmed past its gap grace, but still report-only" True "$(status_of flow:rework is_gap)"
is "still no alert — report-only until 24h of history exist" "0" "$(mail_count_for flow:rework)"

# ============================================================================
echo
echo "15. Stage dwell regression: a lifecycle state's p90 dwell past 3x its trailing baseline"
echo "    p90 is a gap — a healthy baseline is established with enough volume that the current"
echo "    window's own bad data cannot dilute it (same shape section 7's round-health uses)"
# ============================================================================
reset_env
run_pass
is "no bead-stage rows yet -> unobservable" unobservable "$(status_of flow:dwell-regression:working status)"

seed_dwell_baseline() {
    # 80 beads dwelling ~100s in WORKING, completions spread from 1h to ~23h ago — well
    # outside the current 30-minute window, but inside the 24h baseline.
    local i seq=1 age_leave age_enter
    for i in $(seq 1 80); do
        age_leave=$((3600 + i * 1000))
        age_enter=$((age_leave + 100))
        emit_bead_stage "rcf-dwbl-$i" WORKING "$age_enter" "$seq"; seq=$((seq + 1))
        emit_bead_stage "rcf-dwbl-$i" SUBMITTED "$age_leave" "$seq"; seq=$((seq + 1))
    done
}

reset_env
seed_dwell_baseline
# Current window (last ~17 minutes): 8 beads dwelling ~250s — within 3x the ~100s baseline.
seq=9000
for i in $(seq 1 8); do
    age_leave=$((60 + i * 120))
    age_enter=$((age_leave + 250))
    emit_bead_stage "rcf-dwcur-$i" WORKING "$age_enter" "$seq"; seq=$((seq + 1))
    emit_bead_stage "rcf-dwcur-$i" SUBMITTED "$age_leave" "$seq"; seq=$((seq + 1))
done
run_pass
is "current dwell within 3x the baseline is satisfied" satisfied "$(status_of flow:dwell-regression:working status)"

reset_env
seed_dwell_baseline
# Current window: 8 beads dwelling ~3600s — 36x the ~100s baseline. Positive control.
seq=9000
for i in $(seq 1 8); do
    age_leave=$((60 + i * 120))
    age_enter=$((age_leave + 3600))
    emit_bead_stage "rcf-dwbad-$i" WORKING "$age_enter" "$seq"; seq=$((seq + 1))
    emit_bead_stage "rcf-dwbad-$i" SUBMITTED "$age_leave" "$seq"; seq=$((seq + 1))
done
run_pass
is "current dwell past 3x the baseline is a gap" gap "$(status_of flow:dwell-regression:working status)"
is "but not yet confirmed — inside its grace period" False "$(status_of flow:dwell-regression:working is_gap)"
is "no alert while inside grace" "0" "$(mail_count_for flow:dwell-regression:working)"
sleep 3
run_pass
is "the gap is confirmed on the next pass past grace" True "$(status_of flow:dwell-regression:working is_gap)"
is "exactly one alert fired" "1" "$(mail_count_for flow:dwell-regression:working)"
run_pass
is "the same unresolved streak does not alert again" "1" "$(mail_count_for flow:dwell-regression:working)"
is "a state with no transitions of its own stays satisfied" satisfied "$(status_of flow:dwell-regression:ready status)"

# ============================================================================
tl_summary
