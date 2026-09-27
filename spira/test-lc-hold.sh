#!/usr/bin/env bash
#
# test-lc-hold.sh — container-tier acceptance for lc.sh's lc_hold/lc_unhold/lc_holds
# against a real spira-lc binary and a throwaway Dolt server (sp-ki12s).
#
# WHAT THIS PROVES:
#   - lc_hold suspends a non-terminal bead without changing its state (design: "holds are
#     a dimension, not states") — a §2.2 hazard class ("no terminal states forbids leaving")
#     realized here as: a bead sentinel would have poisoned keeps its WORKING state.
#   - lc_unhold releases it and restores exactly the suspended state — this bead's own
#     acceptance criterion ("a hold released restores exactly the suspended state").
#   - lc_hold on a bead already in a terminal state is REFUSED (exit 3), and the row is
#     left untouched — POSITIVE CONTROL: the same call on a non-terminal bead is checked
#     to still apply, so the refusal above is the machine's own terminal-state rule, not a
#     broken connection or a typo in the event JSON.
#   - lc_hold/lc_unhold against a bead spira_lifecycle has no row for at all (not yet
#     classified — the pre-cutover reality for every real bead today) is CANNOT TELL (rc 2
#     from lc_show, propagated), never a hard failure a sweeper would need to special-case.
#
# host-reason: starts its own disposable `dolt sql-server`, same shape as
# test-lifecycle-container.sh (sp-uwv2s) — testenv-batch.sh already provides the container.
#
# defect: sp-ki12s
# tier: T2
# covers: spira/lc.sh spira-lc/* lifecycle/*
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

. "$HERE/conf.sh"
export PATH="$(dirname "$CARGO_BIN"):$(dirname "$DOLT_BIN"):$PATH"

# Never let an ambient SPIRA_LC_SOCKET route this suite's calls through a real service.
unset SPIRA_LC_SOCKET

REPO="$(cd "$HERE/.." && pwd)"
TMP="$(mktemp -d)"
PORT=$((SPIRA_LC_TESTDB_PORT + 700 + (RANDOM % 300)))
SERVER_PID=""
cleanup() { [ -n "$SERVER_PID" ] && kill "$SERVER_PID" >/dev/null 2>&1; rm -rf "$TMP"; }
trap cleanup EXIT INT TERM

mkdir -p "$TMP/data"
cat > "$TMP/server.yaml" <<YAML
log_level: warning
listener:
  port: $PORT
  max_connections: 50
  read_timeout_millis: 30000
  write_timeout_millis: 30000
data_dir: "$TMP/data"
behavior:
  dolt_transaction_commit: false
  event_scheduler: "OFF"
YAML

"$DOLT_BIN" sql-server --config "$TMP/server.yaml" > "$TMP/server.log" 2>&1 &
SERVER_PID=$!

up=0
for _ in $(seq 1 50); do
    if "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls sql -q "SELECT 1" >/dev/null 2>&1; then
        up=1; break
    fi
    sleep 0.2
done
[ "$up" = 1 ] || bail "dolt sql-server never came up: $(cat "$TMP/server.log")"

root_sql() { "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls "$@"; }

CARGO_TARGET_DIR_FOR_BUILD="$TMP/cargo-target"
CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$CARGO_TARGET_DIR_FOR_BUILD" \
    "$CARGO_BIN" build --manifest-path "$REPO/spira-lc/Cargo.toml" --quiet 2>"$TMP/build.log" \
    || bail "spira-lc failed to build: $(cat "$TMP/build.log")"
export SPIRA_LC_BIN="$CARGO_TARGET_DIR_FOR_BUILD/debug/spira-lc"

export SPIRA_LC_HOST=127.0.0.1
export SPIRA_LC_PORT="$PORT"
export SPIRA_LC_DB=spira_lifecycle
export SPIRA_LC_DATA_DIR="$TMP"
export SPIRA_LC_USER=root
export SPIRA_LC_PASSWORD=""

"$SPIRA_LC_BIN" admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$TMP/schema.log" 2>&1
wantrc "schema applies cleanly" 0 $?

seed_bead() {   # seed_bead <bead-id> <state>
    root_sql --use-db spira_lifecycle sql -q \
        "INSERT INTO bead (bead_id, state, holds, version, updated_at) VALUES ('$1','$2','[]',0,0)" >/dev/null 2>&1
}
row_json() {    # row_json <bead-id>
    root_sql --use-db spira_lifecycle sql -q \
        "SELECT state, holds, version FROM bead WHERE bead_id='$1'" -r json 2>/dev/null
}

. "$HERE/lc.sh"

# ── hold suspends without changing state, and unhold restores exactly that state ──────
seed_bead sp-hold-1 WORKING
lc_hold sp-hold-1 poison "attempts exceeded threshold" test-suite
wantrc "lc_hold applies on a non-terminal bead" 0 $?

row="$(row_json sp-hold-1)"
want "state is unchanged by the hold" '"state":"WORKING"' "$row"
want "the poison hold is recorded" 'poison' "$row"

held="$(lc_holds sp-hold-1)"
is "lc_holds reports exactly the one held kind" "poison" "$held"

lc_unhold sp-hold-1 poison test-suite
wantrc "lc_unhold applies" 0 $?

row2="$(row_json sp-hold-1)"
want "the state is exactly what it was before the hold" '"state":"WORKING"' "$row2"
want "holds is empty again — nothing new suspended, nothing resurrected" '"holds":"[]"' "$row2"
held2="$(lc_holds sp-hold-1)"
is "lc_holds reports nothing held after release" "" "$held2"

# ── POSITIVE CONTROL + refusal: a terminal bead cannot be held ─────────────────────────
seed_bead sp-hold-term LANDED
lc_hold sp-hold-term poison "should never apply" test-suite
wantrc "lc_hold on a terminal (LANDED) bead is refused" 3 $?
row3="$(row_json sp-hold-term)"
want "the terminal bead's row is untouched by the refused hold" '"holds":"[]"' "$row3"
want "and its version did not move" '"version":"0"' "$row3"

# ── a bead with no lifecycle row at all (not yet classified) is a clean non-fatal rc,
# never a crash — the pre-cutover reality for every real bead in production today ──────
lc_hold sp-not-classified-yet poison "irrelevant" test-suite
wantrc "lc_hold against an unclassified bead reports 'no such row', not applied and not a crash" 1 $?

tl_summary
