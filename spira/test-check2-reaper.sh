#!/usr/bin/env bash
#
# test-check2-reaper.sh — CHECK 2's stale-lease reclaim scan (against a real spira-lc) and
#   CHECK 2c's per-partition orphan-claim sweep, both previously untested (dispatch test
#   plan G9, G10).
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
# G10: CHECK 2c's release_orphan_claims_partitions (lib.sh) sweeps every partition
# fayth_partitions names, not one hardcoded `${SPIRA_SCOPE_LABEL},plan`. Orphaned claims in
# an ops partition are released exactly like a plan one — the "one hardcoded partition"
# defect the fayth_partitions comment says was already removed from CHECK 2 and CHECK 5.
# Untouched by sp-i2m7y (bd claim/assignee, not spira-lc — CHECK 2c's own cutover is a
# separate bead); its stubbed `bd` fixture needs no spira-lc at all.
#
# host-reason: G9 starts its own disposable `dolt sql-server`, same shape as
# test-lc-hold.sh (sp-ki12s) — testenv-batch.sh already provides the container.
#
# defect: sp-9ce60 (dispatch test plan, gaps G9/G10)
# tier: T2
# covers: spira/lib.sh spira/sentinel.sh spira/lc.sh UC-dispatch-20
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
has() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
[ -n "$CARGO_BIN" ] || skip "cargo not found on PATH or at ~/.cargo/bin"
DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH — install dolt before running this suite"

TMP="$(mktemp -d)"
LC_SERVER_PID=""
trap '[ -n "$LC_SERVER_PID" ] && kill "$LC_SERVER_PID" >/dev/null 2>&1; rm -rf "$TMP"' EXIT INT TERM
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
log() { :; }
# shellcheck disable=SC1090
. "$HERE/lib.sh"
# PATH is set AFTER lib.sh (which sources conf.sh transitively) — conf.sh can overwrite
# PATH with the harness's own tool directories first (test-poison.sh's own note).
export PATH="$(dirname "$CARGO_BIN"):$(dirname "$DOLT_BIN"):$PATH"
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
echo "G10 case 0 — positive control: a single partition's orphan is released:"
# ======================================================================================
mkdir -p "$TMP/bin" "$TMP/state"
cat > "$TMP/bin/bd" <<'STUB'
#!/usr/bin/env bash
case " $* " in
    *" list "*"--label plan "*) cat "$BD_STATE/plan.json" ;;
    *" list "*"--label ops "*)  cat "$BD_STATE/ops.json" ;;
    *" assign "*) printf '%s\n' "$*" >> "$BD_STATE/assigns"; exit 0 ;;
    *) printf '[]' ;;
esac
exit 0
STUB
chmod +x "$TMP/bin/bd"
export BD_STATE="$TMP/state"
printf '[{"id":"sp-plan1","assignee":"aeon-p","status":"open"}]' > "$BD_STATE/plan.json"
printf '[]' > "$BD_STATE/ops.json"
: > "$BD_STATE/assigns"

out="$(PATH="$TMP/bin:$PATH" SPIRA_BD=bd SPIRA_DB=fixture release_orphan_claims_partitions $'plan\t')"
has "plan-only sweep releases sp-plan1" "sp-plan1" "$out"
is  "plan-only sweep releases exactly one" "1" "$(grep -c '^RELEASED' <<< "$out")"

echo
echo "G10 case 1 — a second, non-plan partition (ops) is swept too, not just plan:"
# ======================================================================================
# Before the fix this called release_orphan_claims once, hardcoded to
# \${SPIRA_SCOPE_LABEL},plan — an orphaned claim in ops (or spike, or groom) was never
# released. Without this case, a fix that still sweeps only 'plan' reads as correct.
: > "$BD_STATE/assigns"
printf '[{"id":"sp-ops1","assignee":"aeon-o","status":"open"}]' > "$BD_STATE/ops.json"
out="$(PATH="$TMP/bin:$PATH" SPIRA_BD=bd SPIRA_DB=fixture release_orphan_claims_partitions $'plan\t\nops\t')"
has "plan partition still released"  "sp-plan1" "$out"
has "ops partition ALSO released"    "sp-ops1"  "$out"
is  "both partitions swept -> 2 released" "2" "$(grep -c '^RELEASED' <<< "$out")"

echo
echo "G10 case 2 — no partitions declared: nothing is swept, and nothing is released:"
: > "$BD_STATE/assigns"
out="$(PATH="$TMP/bin:$PATH" SPIRA_BD=bd SPIRA_DB=fixture release_orphan_claims_partitions '')"
is "empty partition list -> no output" "" "$out"

tl_summary
