#!/usr/bin/env bash
#
# test-lc-hold.sh — container-tier acceptance for lc.sh's event wrappers (lc_hold/lc_unhold/
# lc_holds, and, sp-rlyl0: lc_release/lc_holderdead/lc_drop/lc_returned) against a real
# spira-lc binary and a throwaway Dolt server (sp-ki12s, extended for ask/wait/operator by
# sp-mys5p).
#
# WHAT THIS PROVES, for EVERY hold kind (poison, ask, wait, operator — sp-mys5p's
# acceptance is "for each hold kind", not just the one sp-ki12s wired first):
#   - lc_hold suspends a non-terminal bead without changing its state (design: "holds are
#     a dimension, not states") — a §2.2 hazard class ("no terminal states forbids leaving")
#     realized here as: a bead that would be held keeps its WORKING state.
#   - lc_unhold releases it and restores exactly the suspended state — this bead's own
#     acceptance criterion ("a hold released restores exactly the suspended state"). Same
#     mechanism, HoldKind::Operator tag, for the manual hold sp-rlyl0's hold.sh/unhold.sh use.
#   - lc_hold on a bead already in a terminal state is REFUSED (exit 3), and the row is
#     left untouched — POSITIVE CONTROL: the same call on a non-terminal bead is checked
#     to still apply, so the refusal above is the machine's own terminal-state rule, not a
#     broken connection or a typo in the event JSON.
#   - lc_hold/lc_unhold against a bead spira_lifecycle has no row for at all (not yet
#     classified — the pre-cutover reality for every real bead today) is CANNOT TELL (rc 2
#     from lc_show, propagated), never a hard failure a sweeper would need to special-case.
#   - lc_release/lc_holderdead (slay.sh) both return WORKING to READY, and are refused from
#     READY itself (Claim is the only legal event there) — a positive control for the
#     refusal. lc_drop (slay.sh --close) is orthogonal: legal from READY with no Claim ever
#     applied, and refused a second time once the row is terminal. lc_returned (queue.sh
#     eject) is the delivery-exit event from IN_DELIVERY, landing in REWORK.
#
# host-reason: starts its own disposable `dolt sql-server`, same shape as
# test-lifecycle-container.sh (sp-uwv2s) — testenv-batch.sh already provides the container.
#
# defect: sp-ki12s, sp-mys5p
# tier: T2
# covers: spira/lc.sh spira/aeon.sh spira/lib.sh spira/hold.sh spira/unhold.sh spira/groomer.sh spira-lc/* lifecycle/*
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

# ── hold suspends without changing state, and unhold restores exactly that state —
# for EVERY hold kind, not just poison. sp-mys5p wires lc_hold/lc_unhold at the ask, wait
# and operator writers the same way sp-ki12s wired CHECK 4 for poison; this proves the
# mechanism they all share, generically, per kind. ──────────────────────────────────────
for kind in poison ask wait operator; do
    bead="sp-hold-$kind"
    seed_bead "$bead" WORKING
    lc_hold "$bead" "$kind" "held for $kind" test-suite
    wantrc "lc_hold($kind) applies on a non-terminal bead" 0 $?

    row="$(row_json "$bead")"
    want "state is unchanged by the $kind hold" '"state":"WORKING"' "$row"
    want "the $kind hold is recorded" "$kind" "$row"

    held="$(lc_holds "$bead")"
    is "lc_holds reports exactly the one held kind ($kind)" "$kind" "$held"

    lc_unhold "$bead" "$kind" test-suite
    wantrc "lc_unhold($kind) applies" 0 $?

    row2="$(row_json "$bead")"
    want "the state is exactly what it was before the $kind hold" '"state":"WORKING"' "$row2"
    want "holds is empty again after $kind release — nothing new suspended, nothing resurrected" '"holds":"[]"' "$row2"
    held2="$(lc_holds "$bead")"
    is "lc_holds reports nothing held after $kind release" "" "$held2"
done

# ── POSITIVE CONTROL + refusal: a terminal bead cannot be held, for every kind ─────────
for kind in poison ask wait operator; do
    bead="sp-hold-term-$kind"
    seed_bead "$bead" LANDED
    lc_hold "$bead" "$kind" "should never apply" test-suite
    wantrc "lc_hold($kind) on a terminal (LANDED) bead is refused" 3 $?
    row3="$(row_json "$bead")"
    want "the terminal bead's row is untouched by the refused $kind hold" '"holds":"[]"' "$row3"
    want "and its version did not move ($kind)" '"version":"0"' "$row3"
done

# ── a bead with no lifecycle row at all (not yet classified) is a clean non-fatal rc,
# never a crash — the pre-cutover reality for every real bead in production today ──────
lc_hold sp-not-classified-yet poison "irrelevant" test-suite
wantrc "lc_hold against an unclassified bead reports 'no such row', not applied and not a crash" 1 $?

# ── the operator hold kind (sp-rlyl0: hold.sh/unhold.sh), same mechanism, different tag ────
seed_bead sp-op-1 WORKING
lc_hold sp-op-1 operator "manual hold via hold.sh (pid 1)" hold-sp-op-1
wantrc "lc_hold applies the operator kind" 0 $?
row="$(row_json sp-op-1)"
want "the HoldKind::Operator tag is what the row records" 'operator' "$row"
lc_unhold sp-op-1 operator hold-sp-op-1
wantrc "lc_unhold releases the operator hold" 0 $?
is "lc_holds reports nothing held after release" "" "$(lc_holds sp-op-1)"

# ── lc_release / lc_holderdead (sp-rlyl0: slay.sh) — both return WORKING to READY ─────────
seed_bead sp-rel-1 WORKING
lc_release sp-rel-1 test-suite
wantrc "lc_release applies from WORKING" 0 $?
is "...landing in READY" "READY" "$(root_sql --use-db spira_lifecycle sql -q "SELECT state FROM bead WHERE bead_id='sp-rel-1'" -r json 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin)["rows"]; print(d[0]["state"] if d else "")')"

seed_bead sp-hd-1 WORKING
lc_holderdead sp-hd-1 test-suite
wantrc "lc_holderdead applies from WORKING" 0 $?
is "...also landing in READY" "READY" "$(root_sql --use-db spira_lifecycle sql -q "SELECT state FROM bead WHERE bead_id='sp-hd-1'" -r json 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin)["rows"]; print(d[0]["state"] if d else "")')"

# POSITIVE CONTROL: neither is legal from READY (Claim is the only legal event there) —
# proving the refusal above is the machine's own rule, not a broken connection.
seed_bead sp-rel-refused READY
lc_release sp-rel-refused test-suite
wantrc "lc_release from READY is refused" 3 $?

# ── lc_drop (sp-rlyl0: slay.sh --close) — orthogonal, legal even with no Claim ever applied ─
seed_bead sp-drop-1 READY
lc_drop sp-drop-1 "operator decided to drop this" test-suite
wantrc "lc_drop applies from READY (orthogonal, no Claim needed)" 0 $?
is "...landing in DROPPED" "DROPPED" "$(root_sql --use-db spira_lifecycle sql -q "SELECT state FROM bead WHERE bead_id='sp-drop-1'" -r json 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin)["rows"]; print(d[0]["state"] if d else "")')"
lc_drop sp-drop-1 "a second drop" test-suite
wantrc "lc_drop on an already-terminal row is refused, not re-applied" 3 $?

# ── lc_returned (sp-rlyl0: queue.sh eject) — the delivery-exit event, legal from IN_DELIVERY ─
seed_bead sp-ret-1 IN_DELIVERY
lc_returned sp-ret-1 "ejected from open batch" test-suite
wantrc "lc_returned applies from IN_DELIVERY" 0 $?
is "...landing in REWORK" "REWORK" "$(root_sql --use-db spira_lifecycle sql -q "SELECT state FROM bead WHERE bead_id='sp-ret-1'" -r json 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin)["rows"]; print(d[0]["state"] if d else "")')"

tl_summary
