#!/usr/bin/env bash
#
# test-check2-reaper.sh — CHECK 2's stale-lease reclaim scan and CHECK 2c's consistency
#   sweep, both against a real spira-lc (dispatch test plan G9, G10).
#
#   ./test-check2-reaper.sh
#
# G9: check2_reclaim_stale (lib.sh) scans every spira-lc bead row in WORKING state ONCE
# (sp-i2m7y: this was one `bdq reclaim` per partition, with a --exclude-label flag) and
# fires a real HolderDead event, WORKING -> READY, on every one whose lease has been
# expired past SPIRA_RECLAIM_GRACE_SECS and carries no "wait" hold. bump_reclaim is stubbed
# as a recorder here — what is under test is which rows the scan selects and the real
# spira-lc transition it drives, not bump_reclaim's own bd write (covered elsewhere).
#
# G10: check2c_lc_consistency (lib.sh) replaces the bd-based release_orphan_claims_partitions
# (sp-i2m7y). The race it fixed — a status reset that leaves the assignee standing, because
# bd's status and assignee are two separate writes — cannot happen in spira_lifecycle, so
# this is a detector, not a repair: it names a row whose holder and state disagree (seeded
# directly by SQL here, bypassing the CAS on purpose, since no legal transition can produce
# one) rather than releasing anything.
#
# host-reason: starts its own disposable `dolt sql-server`, same shape as test-lc-hold.sh
# (sp-ki12s) — testenv-batch.sh already provides the container.
#
# defect: sp-9ce60 (dispatch test plan, gaps G9/G10)
# tier: T2
# covers: spira/lib.sh sentinel/src/* spira/lc.sh UC-dispatch-20
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
has() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH — install dolt before running this suite"

TMP="$(mktemp -d)"
LC_SERVER_PID=""
trap '[ -n "$LC_SERVER_PID" ] && kill "$LC_SERVER_PID" >/dev/null 2>&1; rm -rf "$TMP"' EXIT INT TERM
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
log() { :; }
# shellcheck disable=SC1090
. "$HERE/lib.sh"
export PATH="$PATH:$(dirname "$DOLT_BIN")"
unset SPIRA_LC_SOCKET
_real_bdq="$(declare -f bdq)"
bdq() { :; }   # G9 exercises the spira-lc transition, not bd's own note/event writes; G10 restores the real one below

echo "test-check2-reaper.sh"

# ── a throwaway spira-lc/Dolt server, the same shape test-lc-hold.sh uses ─────────────
REPO="$(cd "$HERE/.." && pwd)"
LC_PORT=$((SPIRA_LC_TESTDB_PORT + 1200 + (RANDOM % 300)))
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

# spira-lc is the tree's own build, by name on this suite's PATH (sp-gypjk) — never a
# second cargo build of it here.
# lc.sh consults spira-lc only with lifecycle ON (sp-gypjk: the switch, not a binary path).
export SPIRA_LIFECYCLE_ENFORCE=1
export SPIRA_LC_HOST=127.0.0.1
export SPIRA_LC_PORT="$LC_PORT"
export SPIRA_LC_DB=spira_lifecycle
export SPIRA_LC_DATA_DIR="$LC_TMP"
export SPIRA_LC_USER=root
export SPIRA_LC_PASSWORD=""
spira-lc admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$LC_TMP/schema.log" 2>&1
wantrc "spira_lifecycle schema applies cleanly" 0 $?

# shellcheck disable=SC1090
. "$HERE/lc.sh"

NOW="$(date +%s)"
# seed_working <id> <lease-delta-secs> [hold] — a WORKING row whose lease expired
# <lease-delta-secs> ago (negative: still live), optionally carrying one hold.
seed_working() {
    local id="$1" delta="$2" hold="${3:-[]}"
    lc_root_sql --use-db spira_lifecycle sql -q "DELETE FROM bead WHERE bead_id = '$id'" >/dev/null 2>&1
    lc_root_sql --use-db spira_lifecycle sql -q \
        "INSERT INTO bead (bead_id, state, holder, lease_until, holds, version, updated_at) VALUES ('$id','WORKING','aeon-x',$((NOW - delta)),'$hold',0,0)" >/dev/null 2>&1
}
row_state() { lc_root_sql --use-db spira_lifecycle sql -q "SELECT state FROM bead WHERE bead_id='$1'" -r csv 2>/dev/null | tail -1; }

# ======================================================================================
echo
echo "G9 case 1 — positive control: an expired WORKING lease is reaped and charged:"
# ======================================================================================
# Without this, an implementation that never fires HolderDead reads as correct.
export SPIRA_RECLAIM_GRACE_SECS=100
BUMPED="$TMP/bumped"; : > "$BUMPED"
bump_reclaim() { printf '%s %s\n' "$1" "$2" >> "$BUMPED"; }
seed_working sp-dead1 999999
progressed=0; progress() { progressed=$((progressed+1)); }
check2_reclaim_stale
is  "expired lease -> row is READY"       "READY"                "$(row_state sp-dead1)"
has "expired lease -> bump_reclaim charged" "sp-dead1 stale-lease" "$(cat "$BUMPED")"
is  "expired lease -> progress reported once" "1" "$progressed"

echo
echo "G9 case 2 — a lease within the grace window is left alone:"
# ======================================================================================
: > "$BUMPED"; progressed=0
seed_working sp-live1 10
check2_reclaim_stale
is "live lease -> row still WORKING" "WORKING" "$(row_state sp-live1)"
is "live lease -> no charge" "" "$(cat "$BUMPED")"
is "live lease -> progress not reported" "0" "$progressed"

echo
echo "G9 case 3 — a wait-held bead is exempt even with an expired lease:"
# ======================================================================================
# check2_protect_waiting already decided this bead is legitimately waiting; the reaper
# must not re-litigate that decision.
: > "$BUMPED"; progressed=0
seed_working sp-waiting1 999999 '["wait"]'
check2_reclaim_stale
is "wait-held bead -> row still WORKING" "WORKING" "$(row_state sp-waiting1)"
is "wait-held bead -> no charge" "" "$(cat "$BUMPED")"
is "wait-held bead -> progress not reported" "0" "$progressed"

echo
echo "G9 case 4 — nothing WORKING at all: no charge, no progress:"
# ======================================================================================
# A REAPER WITH NOTHING TO REAP OVER SAYS SO. Without this, a loop that silently does
# nothing over an empty WORKING set reads exactly like a harness with no dead leases.
lc_root_sql --use-db spira_lifecycle sql -q "DELETE FROM bead" >/dev/null 2>&1
: > "$BUMPED"; progressed=0
check2_reclaim_stale
is "no WORKING rows -> no charge" "" "$(cat "$BUMPED")"
is "no WORKING rows -> progress not reported" "0" "$progressed"
unset -f progress bump_reclaim
eval "$_real_bdq"

# ======================================================================================
echo
echo "G10 — CHECK 2c moved onto spira-lc (sp-i2m7y): check2c_lc_consistency detects, never"
echo "repairs, a holder/state row the machine's own CAS should make unreachable:"
# ======================================================================================
# release_orphan_claims_partitions (the bd-based sweep G10 used to drive) is gone — its
# race (a status reset that leaves the assignee standing) cannot occur here, because
# holder and state change together in one version-checked transaction. What replaces it
# is a pure consistency scan with nothing to write, so these rows are seeded by SQL
# directly, bypassing the CAS on purpose — the only way to produce the anomaly at all.
lc_root_sql --use-db spira_lifecycle sql -q "DELETE FROM bead" >/dev/null 2>&1

echo
echo "case 0 — positive control: a consistent WORKING row is not flagged:"
seed_working sp-fine1 -10
out="$(check2c_lc_consistency)"
is "consistent row -> no output" "" "$out"

echo
echo "case 1 — WORKING with no holder is flagged:"
lc_root_sql --use-db spira_lifecycle sql -q \
    "INSERT INTO bead (bead_id, state, holder, holds, version, updated_at) VALUES ('sp-bad1','WORKING',NULL,'[]',0,0)" >/dev/null 2>&1
out="$(check2c_lc_consistency)"
has "WORKING-no-holder -> flagged" "sp-bad1" "$out"
has "WORKING-no-holder -> names the anomaly" "WORKING with no holder" "$out"

echo
echo "case 2 — READY with a holder still set is flagged:"
lc_root_sql --use-db spira_lifecycle sql -q "DELETE FROM bead WHERE bead_id = 'sp-bad1'" >/dev/null 2>&1
lc_root_sql --use-db spira_lifecycle sql -q \
    "INSERT INTO bead (bead_id, state, holder, holds, version, updated_at) VALUES ('sp-bad2','READY','aeon-stale','[]',0,0)" >/dev/null 2>&1
out="$(check2c_lc_consistency)"
has "READY-with-holder -> flagged" "sp-bad2" "$out"
has "READY-with-holder -> names the anomaly" "READY with a holder still set" "$out"

tl_summary
