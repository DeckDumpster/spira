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
#         systemd/units.sh systemd/install.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

. "$HERE/testlib.sh"
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-reconciler-flow

command -v duckdb >/dev/null 2>&1 || { echo "SKIP test-reconciler-flow: duckdb not on PATH"; exit 77; }

CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
[ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ] && CARGO_BIN="$HOME/.cargo/bin/cargo"
# A testenv container installs the toolchain at /usr/local/cargo/bin (spira/testenv/
# Containerfile) but `podman exec` does not carry the image's own PATH into the exec'd
# process — only bare command -v/$HOME lookups above find it when run outside a container.
[ -z "$CARGO_BIN" ] && [ -x /usr/local/cargo/bin/cargo ] && CARGO_BIN=/usr/local/cargo/bin/cargo
if [ -z "$CARGO_BIN" ]; then
    echo "SKIP test-reconciler-flow: cargo not found — reconciler-flow binary cannot be built"
    exit 77
fi
export PATH="$(dirname "$CARGO_BIN"):$PATH"

T="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$T"' EXIT INT TERM
testdb_up reconcilerflow || { echo "test-reconciler-flow: could not build a fixture database"; exit 1; }

# Isolated build tree, its path dependencies (reconciler-engine, tsd) copied alongside it —
# same shape test-czar-pass.sh uses for the same reason: a Cargo path dependency resolves
# relative to the manifest, so the sibling must exist in the copy too.
FLOW_ROOT="$HERE/../reconciler-flow"
FLOW_BIN="$FLOW_ROOT/target/release/reconciler-flow"
if [ ! -x "$FLOW_BIN" ]; then
    cp -r "$FLOW_ROOT/." "$T/reconciler-flow-src"
    cp -r "$HERE/../reconciler-engine" "$T/reconciler-engine"
    cp -r "$HERE/../tsd" "$T/tsd"
    # Pin resolution to the workspace's own lock — unlocked, cargo re-resolves transitive
    # deps fresh and can land on a version needing a newer edition than the toolchain ships.
    cp "$HERE/../Cargo.lock" "$T/reconciler-flow-src/Cargo.lock"
    printf '  (building reconciler-flow into %s)\n' "$T/reconciler-flow-target"
    CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$T/reconciler-flow-target" \
        "$CARGO_BIN" build --release \
        --manifest-path "$T/reconciler-flow-src/Cargo.toml" 2>&1 | tail -5
    FLOW_BIN="$T/reconciler-flow-target/release/reconciler-flow"
fi
if [ ! -x "$FLOW_BIN" ]; then
    printf 'reconciler-flow binary not found at %s\n' "$FLOW_BIN" >&2
    printf '0 passed, 1 failed\n'
    exit 1
fi
ok "reconciler-flow binary built"

export SPIRA_HOME="$HERE"
export SPIRA_RUN="$T/run"
export SPIRA_TSD_BIN=""            # best-effort self-write disabled: not under test here
export SPIRA_DUCKDB_BIN="duckdb"
export SPIRA_BD="${TESTDB_BD:-bd-embedded}"
export SPIRA_FLOW_WINDOW_HOURS="0.5"
export SPIRA_FLOW_BASELINE_HOURS="24"
export SPIRA_FLOW_GRACE_SECS="2"   # short so the suite need not sleep for a real 30m window
export SPIRA_DESIRED_DIR="$T/desired"
export SPIRA_SCOPE_LABEL=""
mkdir -p "$SPIRA_RUN"

# Stub mail.sh: records every "send concierge" call so the alert path is observable without
# a real mailbox. Any other subcommand is refused loudly — a call this suite did not expect.
MAIL_LOG="$T/mail-calls.log"
cat > "$T/mail.sh" <<STUB
#!/usr/bin/env bash
if [ "\$1" = "send" ] && [ "\$2" = "concierge" ]; then
    { printf 'CALL %s\n' "\$*"; cat; printf '\n---\n'; } >> "$MAIL_LOG"
    exit 0
fi
printf 'stub mail.sh: unexpected invocation: %s\n' "\$*" >&2
exit 1
STUB
chmod +x "$T/mail.sh"
export SPIRA_MAIL_SH="$T/mail.sh"

run_pass() { "$FLOW_BIN" --pass >"$T/pass-out.log" 2>&1; }
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
"$FLOW_BIN" --pass >"$T/pass-out.log" 2>&1
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
tl_summary
