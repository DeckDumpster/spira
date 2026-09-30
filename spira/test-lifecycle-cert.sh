#!/usr/bin/env bash
#
# test-lifecycle-cert.sh — container-tier acceptance for certification onto events
# (sp-vd9dn): spira/lifecycle-cert.sh and gate.sh's verdict() against a real, disposable
# spira_lifecycle database.
#
# WHAT THIS PROVES:
#   - inert by default: with SPIRA_LIFECYCLE_ENFORCE off, nothing here writes anything.
#   - a GatePass whose tip does not match the row's own is refused — the mechanism the tip
#     invariant leans on (bead.rs's own TipMismatch, exercised through the shell wrapper).
#   - THE TIP INVARIANT ITSELF: lc_resubmit on a CERTIFIED row with a moved tip — a harness
#     rebase — sends the bead back to SUBMITTED with its gate key cleared.
#   - GateRed and GateInfra land on REWORK and (still) SUBMITTED respectively, and GateInfra
#     still advances the row's version (a recorded retry, not a no-op).
#   - a row the machine still shows WORKING is auto-submitted at the given tip before the
#     verdict is applied, so a caller never has to sequence Submit and GatePass itself.
#   - gate.sh's own verdict() calls through for real: a PASS certifies the row at the
#     branch's exact commit with a real, non-empty gate key; a FAIL sends it to REWORK.
#
# host-reason: starts its own disposable `dolt sql-server`, the same shape
# test-lifecycle-container.sh already uses without a container call — testenv-batch.sh
# already provides the container this suite executes in.
#
# defect: sp-vd9dn
# tier: T2
# covers: spira/lifecycle-cert.sh spira/gate.sh gate/src/engine.rs lifecycle/src/bead.rs
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
. "$HERE/testlib/gate-fixture.sh"

DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH — install dolt before running this suite"

. "$HERE/conf.sh"
export PATH="$PATH:$(dirname "$DOLT_BIN")"
unset SPIRA_LC_SOCKET

REPO_ROOT="$(cd "$HERE/.." && pwd)"
TMP="$(mktemp -d)"
PORT=$((SPIRA_LC_TESTDB_PORT + (RANDOM % 500)))
SERVER_PID=""
cleanup() {
    [ -n "$SERVER_PID" ] && kill "$SERVER_PID" >/dev/null 2>&1
    rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

mkdir -p "$TMP/data"
cat > "$TMP/server.yaml" <<YAML
log_level: warning
listener:
  port: $PORT
  max_connections: 100
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

# spira-lc is the tree under test's own build, found by name on the suite's PATH (sp-gypjk);
# lifecycle is switched on for this suite with SPIRA_LIFECYCLE_ENFORCE, never by a path.
command -v spira-lc >/dev/null 2>&1 || bail "spira-lc is not on PATH"
export SPIRA_LIFECYCLE_ENFORCE=1

export SPIRA_LC_HOST=127.0.0.1
export SPIRA_LC_PORT="$PORT"
export SPIRA_LC_DB=spira_lifecycle
export SPIRA_LC_DATA_DIR="$TMP"
export SPIRA_LC_USER=root
export SPIRA_LC_PASSWORD=""

spira-lc admin-apply-ddl "$REPO_ROOT/lifecycle/schema.sql" > "$TMP/schema.log" 2>&1
wantrc "schema applies cleanly" 0 $?
cat "$TMP/schema.log" >&2

# seed_bead <id> <state> <tip|-> <gate_key|->
seed_bead() {
    local id="$1" state="$2" tip="$3" gk="$4" tip_sql gk_sql
    tip_sql="NULL"; [ "$tip" = "-" ] || tip_sql="'$tip'"
    gk_sql="NULL"; [ "$gk" = "-" ] || gk_sql="'$gk'"
    root_sql --use-db spira_lifecycle sql -q \
        "INSERT INTO bead (bead_id, state, tip, gate_key, holds, version, updated_at) VALUES ('$id','$state',$tip_sql,$gk_sql,'[]',0,0)" \
        >/dev/null 2>&1
}
row_field() {   # row_field <id> <column> -> the column's value, or "" for SQL NULL/missing
    root_sql --use-db spira_lifecycle sql -q "SELECT $2 FROM bead WHERE bead_id='$1'" -r json 2>/dev/null \
        | python3 -c '
import json, sys
d = json.load(sys.stdin)
rows = d.get("rows") or []
if not rows:
    print("")
else:
    vals = list(rows[0].values())
    v = vals[0] if vals else None
    print(v if v is not None else "")
' 2>/dev/null
}

echo "test-lifecycle-cert.sh"

# ---------------------------------------------------------------------------------------
# INERT BY DEFAULT — lifecycle off (SPIRA_LIFECYCLE_ENFORCE unset), no lifecycle write at all.
# ---------------------------------------------------------------------------------------
echo
echo "inert by default (lifecycle off):"
unset SPIRA_LIFECYCLE_ENFORCE
. "$HERE/lifecycle-cert.sh"
lc_available; is "lc_available is false with lifecycle off" "1" "$?"
lc_certify sp-inert deadbeef pass k1 >/dev/null 2>&1; is "lc_certify is cannot-tell, not applied" "2" "$?"

export SPIRA_LIFECYCLE_ENFORCE=1
lc_available; is "POSITIVE CONTROL: lc_available is true once lifecycle is on" "0" "$?"

# ---------------------------------------------------------------------------------------
# A GatePass FOR A STALE TIP IS REFUSED (acceptance criterion 1).
# ---------------------------------------------------------------------------------------
echo
echo "certification for a stale tip is refused:"
seed_bead sp-stale SUBMITTED aaa111 -
lc_certify sp-stale bbb222 pass keyA >/dev/null 2>&1
is "GatePass for a different tip is refused" "3" "$?"
is "row state is unchanged" "SUBMITTED" "$(row_field sp-stale state)"
is "row tip is unchanged" "aaa111" "$(row_field sp-stale tip)"
is "row gate_key was never set" "" "$(row_field sp-stale gate_key)"

lc_certify sp-stale aaa111 pass keyA >/dev/null 2>&1
is "POSITIVE CONTROL: GatePass for the matching tip applies" "0" "$?"
is "row is now CERTIFIED" "CERTIFIED" "$(row_field sp-stale state)"
is "row carries the gate key" "keyA" "$(row_field sp-stale gate_key)"

# ---------------------------------------------------------------------------------------
# THE TIP INVARIANT — a harness rebase of a certified branch sends it back to SUBMITTED
# (acceptance criterion 2). sp-stale is CERTIFIED at aaa111 from the case above.
# ---------------------------------------------------------------------------------------
echo
echo "a harness rebase (moved tip) voids certification back to SUBMITTED:"
lc_resubmit sp-stale ccc333 >/dev/null 2>&1
is "lc_resubmit applies" "0" "$?"
is "row is back to SUBMITTED" "SUBMITTED" "$(row_field sp-stale state)"
is "row tip is the new (rebased) one" "ccc333" "$(row_field sp-stale tip)"
is "row's gate key is cleared — the old verdict no longer answers for this tree" "" "$(row_field sp-stale gate_key)"

# ---------------------------------------------------------------------------------------
# GateRed and GateInfra.
# ---------------------------------------------------------------------------------------
echo
echo "GateRed sends a submitted bead to REWORK:"
seed_bead sp-red SUBMITTED ddd444 -
lc_certify sp-red ddd444 red "branch-red" >/dev/null 2>&1
is "GateRed applies" "0" "$?"
is "row is REWORK" "REWORK" "$(row_field sp-red state)"

# GateRedReason (lifecycle/src/reason.rs) is a closed enum — spira-lc refuses to parse a
# --kind whose reason is not one of its six kebab-case variants. gate.sh's own verdict()
# reasons (branch-red, syntax, beads-data, foreign-harness, ...) are free text, so
# lc_certify's red arm must map every one of them, including one this table has never seen,
# onto a variant the machine accepts — not pass the raw string through (SEEN RED: it did,
# and "branch-red" itself — the one gate.sh actually emits — failed to parse).
echo
echo "GateRed with an unrecognized raw reason still applies (mapped, not passed through):"
seed_bead sp-red2 SUBMITTED fff777 -
lc_certify sp-red2 fff777 red "a-reason-spira-lc-has-never-heard-of" >/dev/null 2>&1
is "GateRed with an unmapped reason still applies" "0" "$?"
is "row is REWORK" "REWORK" "$(row_field sp-red2 state)"

echo
echo "GateInfra leaves a submitted bead SUBMITTED but still advances its version:"
seed_bead sp-infra SUBMITTED eee555 -
v0="$(row_field sp-infra version)"
lc_certify sp-infra eee555 infra - >/dev/null 2>&1
is "GateInfra applies" "0" "$?"
is "row is still SUBMITTED" "SUBMITTED" "$(row_field sp-infra state)"
is "row's version advanced (a recorded retry, not a silent no-op)" "$((v0 + 1))" "$(row_field sp-infra version)"

# ---------------------------------------------------------------------------------------
# A WORKING bead is auto-submitted before the verdict lands — a caller of lc_certify never
# has to sequence Submit and GatePass itself.
# ---------------------------------------------------------------------------------------
echo
echo "a WORKING bead is auto-submitted at the given tip, then certified:"
seed_bead sp-working WORKING - -
lc_certify sp-working fff666 pass keyB >/dev/null 2>&1
is "lc_certify from WORKING applies" "0" "$?"
is "row is CERTIFIED" "CERTIFIED" "$(row_field sp-working state)"
is "row tip is the one it was certified at" "fff666" "$(row_field sp-working tip)"

# ---------------------------------------------------------------------------------------
# gate.sh's OWN verdict() calls through, for real — no stub, real git, real gate command.
# (acceptance criterion 3's mechanism: a gate that runs is the only path to a certification.)
# ---------------------------------------------------------------------------------------
echo
echo "gate.sh: a real PASS certifies the row at the branch's exact commit and gate key:"
gate_fixture_init "$TMP/gatefx"
gate_fixture_branch spira/sp-gpass
printf 'repo | %s | push | origin/main |  | true\n' "$REPO" > "$MAP"
TIP_PASS="$(git -C "$REPO" rev-parse spira/sp-gpass)"
seed_bead sp-gpass WORKING - -
gout="$(gate_fixture_run spira/sp-gpass repo PATH="$PATH" \
    SPIRA_GATE_BEAD=sp-gpass SPIRA_LIFECYCLE_ENFORCE=1 SPIRA_LC_HOST=127.0.0.1 SPIRA_LC_PORT="$PORT" \
    SPIRA_LC_DB=spira_lifecycle SPIRA_LC_DATA_DIR="$TMP" SPIRA_LC_USER=root SPIRA_LC_PASSWORD=)"
want "gate.sh: reports PASS" "VERDICT=PASS" "$gout"
is "gate.sh PASS: row is CERTIFIED" "CERTIFIED" "$(row_field sp-gpass state)"
is "gate.sh PASS: row tip is the branch's exact commit" "$TIP_PASS" "$(row_field sp-gpass tip)"
gk_gpass="$(row_field sp-gpass gate_key)"
[ -n "$gk_gpass" ] && ok "gate.sh PASS: row carries a real (non-empty) gate key" \
    || bad "gate.sh PASS: row carries a real (non-empty) gate key" "gate_key was empty"

echo
echo "gate.sh: a real FAIL sends the row to REWORK, not CERTIFIED:"
# The gate command must fail on the BRANCH and pass on the BASE — "false" for both would
# read as the BASE failing too (BASE_FAIL/infra, not the branch's own fault). "bad.txt"
# exists only on the branch, so `test ! -f bad.txt` fails there and passes on origin/main.
gate_fixture_branch spira/sp-gred bad.txt trip
printf 'repo | %s | push | origin/main |  | test ! -f bad.txt\n' "$REPO" > "$MAP"
seed_bead sp-gred WORKING - -
gout2="$(gate_fixture_run spira/sp-gred repo PATH="$PATH" \
    SPIRA_GATE_BEAD=sp-gred SPIRA_LIFECYCLE_ENFORCE=1 SPIRA_LC_HOST=127.0.0.1 SPIRA_LC_PORT="$PORT" \
    SPIRA_LC_DB=spira_lifecycle SPIRA_LC_DATA_DIR="$TMP" SPIRA_LC_USER=root SPIRA_LC_PASSWORD=)"
want "gate.sh: reports FAIL" "VERDICT=FAIL" "$gout2"
is "gate.sh FAIL: row is REWORK, not CERTIFIED" "REWORK" "$(row_field sp-gred state)"

tl_summary
