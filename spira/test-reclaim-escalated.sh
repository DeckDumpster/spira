#!/usr/bin/env bash
#
# test-reclaim-escalated.sh — end-to-end: an escalated bead (IN_PROGRESS, waiting on an
#   unanswered operator ask dep) is not reclaimed by either the time-based reaper (CHECK 2)
#   or the ghost check (CHECK 2b).
#
#   ./test-reclaim-escalated.sh
#
# WHY THIS EXISTS. sp-9zpm: sp-mfa4 (IN_PROGRESS, waiting on sp-rmnw, an unanswered
#   needs-ryan ask) was reclaimed six times — the time-based reaper and the /proc ghost
#   check each treated it as a dead worker. Two fixes closed both paths:
#     - sp-rzyl: check2_protect_waiting applies a spira-lc "wait" hold (sp-i2m7y: this was
#       the SPIRA_RECLAIM_SKIP_LABEL bd label) to a work bead whose only open dep carries
#       the ask label; check2_reclaim_stale's own scan skips a wait-held bead.
#     - sp-qsa1: strand-classify.py exempts beads carrying the ask label or a wait hold
#       from ghost classification.
#
#   test-check2-reclaim.sh verifies the hold is applied/released by protect_waiting, against
#   recorder stubs. test-strand-partition.sh's ghost-classifier cases verify
#   strand-classify.py respects WAIT_HELD in isolation (D7: merged from the now-deleted
#   test-reclaim-needs-ryan.sh). THIS TEST verifies the chain end-to-end: protect_waiting
#   holds the bead in a real spira-lc, the hold is present in the data extracted from
#   spira-lc and fed to the ghost check, and the ghost check does not raise ghost for it.
#
# THREE CASES (all sides exercised — law-absence-needs-a-positive-control). The plain
# dead-worker positive control lives with the rest of the classifier table in
# test-strand-partition.sh (D7) — this file keeps only the chain a hermetic classifier
# fixture cannot exercise: a real spira-lc write reaching the classifier's input.
#
#   1. CHAIN — BEFORE PROTECTION: work bead has expired lease + ask dep, but protect_waiting
#      has not run yet (no wait hold on the bead). Ghost IS raised — proving the fix was
#      necessary and that a stale-lease work bead is ghost-eligible by default.
#
#   2. CHAIN — PROTECTED: protect_waiting holds the work bead (the ask dep is still open).
#      The same bead, re-extracted from the real spira-lc, is NOT ghost. This is the core
#      assertion: the real-store hold from protect_waiting propagates correctly through to
#      the ghost-check input.
#
#   3. CHAIN — AFTER ANSWER: the ask dep closes. protect_waiting releases the hold. The
#      bead, re-extracted, IS ghost again — confirming the protection is lifted once an
#      answer arrives and the bead is returned to the reaper for normal reclaim.
#
# A real fixture database (testdb.sh) drives protect_waiting's dependency reads; a real,
# throwaway spira-lc/Dolt (the same shape test-lc-hold.sh uses) drives its hold path. JSON
# extracted from bd feeds strand-classify.py, with WAIT_HELD read from the real spira-lc; a
# past lease is injected so the time condition in the ghost check fires.
#
# host-reason: starts its own disposable `dolt sql-server`, same shape as
# test-lc-hold.sh (sp-ki12s) — testenv-batch.sh already provides the container.
#
# tier: T2
# defect: sp-9zpm
# covers: spira/lib.sh spira/lc.sh spira/strand-classify.py UC-dispatch-21
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
[ -n "$CARGO_BIN" ] || skip "cargo not found on PATH or at ~/.cargo/bin"
DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH — install dolt before running this suite"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-reclaim-escalated
TMP="$(mktemp -d)"
LC_SERVER_PID=""
trap 'testdb_drop; [ -n "$LC_SERVER_PID" ] && kill "$LC_SERVER_PID" >/dev/null 2>&1; rm -rf "$TMP"' EXIT INT TERM
testdb_up reclaimescalated || { echo "test-reclaim-escalated: could not build fixture database"; exit 1; }
export PATH="$(dirname "$CARGO_BIN"):$(dirname "$DOLT_BIN"):$PATH"
unset SPIRA_LC_SOCKET

# Stub what sentinel.sh defines but lib.sh needs.
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
acted=0; progressed=0
act()      { acted=$((acted+1)); }
progress() { progressed=$((progressed+1)); act "$@"; }
log()      { : ; }
# shellcheck disable=SC1090
. "$HERE/lib.sh"

B() { bd -C "$SPIRA_DB" "$@"; }

# ── a throwaway spira-lc/Dolt server, the same shape test-lc-hold.sh uses ─────────────
REPO="$(cd "$HERE/.." && pwd)"
LC_PORT=$((SPIRA_LC_TESTDB_PORT + 900 + (RANDOM % 300)))
LC_TMP="$TMP/lc"; mkdir -p "$LC_TMP/data"
cat > "$LC_TMP/server.yaml" <<YAML
log_level: warning
listener:
  port: $LC_PORT
  max_connections: 50
  read_timeout_millis: 30000
  write_timeout_millis: 30000
data_dir: "$LC_TMP/data"
behavior:
  dolt_transaction_commit: false
  event_scheduler: "OFF"
YAML
"$DOLT_BIN" sql-server --config "$LC_TMP/server.yaml" > "$LC_TMP/server.log" 2>&1 &
LC_SERVER_PID=$!
lc_up=0
for _ in $(seq 1 50); do
    if "$DOLT_BIN" --data-dir "$LC_TMP" --host 127.0.0.1 --port "$LC_PORT" -u root -p "" --no-tls sql -q "SELECT 1" >/dev/null 2>&1; then
        lc_up=1; break
    fi
    sleep 0.2
done
[ "$lc_up" = 1 ] || bail "dolt sql-server for spira_lifecycle never came up: $(cat "$LC_TMP/server.log")"
lc_root_sql() { "$DOLT_BIN" --data-dir "$LC_TMP" --host 127.0.0.1 --port "$LC_PORT" -u root -p "" --no-tls "$@"; }

LC_CARGO_TARGET="$LC_TMP/cargo-target"
CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$LC_CARGO_TARGET" \
    "$CARGO_BIN" build --manifest-path "$REPO/spira-lc/Cargo.toml" --quiet 2>"$LC_TMP/build.log" \
    || bail "spira-lc failed to build: $(cat "$LC_TMP/build.log")"
export SPIRA_LC_BIN="$LC_CARGO_TARGET/debug/spira-lc"
export SPIRA_LC_HOST=127.0.0.1
export SPIRA_LC_PORT="$LC_PORT"
export SPIRA_LC_DB=spira_lifecycle
export SPIRA_LC_DATA_DIR="$LC_TMP"
export SPIRA_LC_USER=root
export SPIRA_LC_PASSWORD=""
"$SPIRA_LC_BIN" admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$LC_TMP/schema.log" 2>&1
wantrc "spira_lifecycle schema applies cleanly" 0 $?

# shellcheck disable=SC1090
. "$HERE/lc.sh"

# seed_lc <bead-id> -> a fresh READY row, dropping any row a prior case left behind (the
# lifecycle server is one long-lived instance across this whole suite).
seed_lc() {
    lc_root_sql --use-db spira_lifecycle sql -q "DELETE FROM bead WHERE bead_id = '$1'" >/dev/null 2>&1
    "$SPIRA_LC_BIN" create-bead "$1" >/dev/null 2>&1
}

# The ask label used for these tests (fixture configuration).
ASK="${SPIRA_ASK_LABEL:-needs-operator}"

# Past timestamp: any bead with this lease is long expired.
PAST="2020-01-01T00:00:00Z"

# classify_with_lease — run strand-classify.py against a JSON array of beads, injecting a
# past lease_expires_at into every bead so the ghost check's time condition fires, and
# WAIT_HELD from the real spira-lc (lc_list_held, sourced above). The rest of the bead data
# (labels, status, assignee) is whatever the caller passes in — typically live data
# extracted from the fixture database.
classify_with_lease() {
    python3 -c '
import sys, json
data = json.loads(sys.argv[1])
if not isinstance(data, list): data = [data]
for b in data: b["lease_expires_at"] = sys.argv[2]
print(json.dumps(data))
' "$1" "$PAST" > "$TMP/beads.json"
    printf '[]' > "$TMP/ready.json"
    BEADS_FILE="$TMP/beads.json" \
    READY_FILE="$TMP/ready.json" \
    HOLDERS="" LIVE=1 GHOST_GRACE=0 \
    SPIRA_ASK_LABEL="$ASK" \
    WAIT_HELD="$(lc_list_held wait | tr '\n' ' ')" \
        python3 "$HERE/strand-classify.py"
}

echo "test-reclaim-escalated.sh"

# ======================================================================================
echo
echo "case 1 — chain before protection: work bead without a wait hold IS ghost:"
# ======================================================================================
# This is the exact pre-fix state: the work bead is IN_PROGRESS with an ask dep, but
# neither the ask label nor a wait hold applies to the work bead itself. The ghost
# classifier sees only a stale-lease in_progress bead and raises ghost — exactly the
# behaviour that produced the sp-mfa4 respawn loop. This case proves the fix is necessary.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-ask0","title":"unanswered decision","status":"open","issue_type":"decision","labels":["$ASK","plan","spira"],"assignee":""}
{"id":"sp-work0","title":"work waiting for answer","status":"in_progress","issue_type":"task","labels":["plan","spira"],"assignee":"aeon-pre","dependencies":[{"depends_on_id":"sp-ask0","type":"blocks"}]}
JSONL
seed_lc sp-work0
# Extract live bead data from the real database. No wait hold on sp-work0 yet.
raw_json="$(bdjson list --all --status in_progress --limit 0)"
out="$(classify_with_lease "$raw_json")"
want   "before protection: ghost raised"  "ghost"    "$out"
want   "before protection: bead named"   "sp-work0"  "$out"

# ======================================================================================
echo
echo "case 2 — chain after protection: protect_waiting applies a wait hold → NOT ghost:"
# ======================================================================================
# protect_waiting runs (ask dep is still open). It holds sp-work0 (spira-lc "wait").
# We re-extract from the real spira-lc — the hold is now present in the live data.
# strand-classify.py must not raise ghost: the protection written to the store reaches
# the classifier, which is the end-to-end assertion this test exists to make.
acted=0
check2_protect_waiting
lc_held sp-work0 wait; is "protect_waiting holds sp-work0" "0" "$?"
raw_json="$(bdjson list --all --status in_progress --limit 0)"
out="$(classify_with_lease "$raw_json")"
nowant "after protection: ghost NOT raised"  "ghost"    "$out"
nowant "after protection: bead NOT named"   "sp-work0"  "$out"

# ======================================================================================
echo
echo "case 3 — chain after answer: dep closes, hold released → ghost IS raised:"
# ======================================================================================
# The operator answers. The ask dep closes. protect_waiting re-runs and releases the hold
# on sp-work0 because no open dep still carries the ask label.
# Re-extracting from spira-lc now shows no protection; the ghost check fires.
# This confirms the bead is correctly returned to the reaper once an answer arrives.
B close sp-ask0 >/dev/null 2>&1
acted=0
check2_protect_waiting
lc_held sp-work0 wait; is "after answer: wait hold released" "1" "$?"
raw_json="$(bdjson list --all --status in_progress --limit 0)"
out="$(classify_with_lease "$raw_json")"
want   "after answer: ghost IS raised"  "ghost"    "$out"
want   "after answer: bead named"      "sp-work0"  "$out"

tl_summary
