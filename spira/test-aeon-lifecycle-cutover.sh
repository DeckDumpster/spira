#!/usr/bin/env bash
#
# test-aeon-lifecycle-cutover.sh — aeon.sh onto the semantic layer (sp-xethq, design §3.5).
#
#   ./test-aeon-lifecycle-cutover.sh
#
# WHAT THIS PROVES, against a throwaway `dolt sql-server` for spira_lifecycle and a real bd
# testdb, both this suite starts and tears down itself:
#
#   - lc_claim_bead (lib.sh) moves a READY row to WORKING (Claim), and refuses a row that is
#     not claimable — the sp-zw9ot fixture: a bead already IN_DELIVERY is refused, unchanged.
#   - lc_claim_bead clears a dead holder's stale WORKING row (HolderDead) before claiming it.
#   - lc_release_bead (release_own_claim's own lifecycle half) returns a WORKING row to READY,
#     and is a harmless no-op past WORKING (SUBMITTED and beyond refuse Release, by design).
#   - End to end through the real aeon.sh: a claimed bead reads WORKING right up until the
#     model's own `work submit` applies — never "closed" — and the model that submitted it
#     had no `bd` on its PATH at all.
#
# host-reason: starts its own disposable `dolt sql-server`, same shape as
# test-lifecycle-container.sh / test-work-container.sh; testenv-batch.sh already provides
# the container this suite executes in.
#
# tier: T2
# covers: spira/aeon.sh spira/lib.sh spira/work-env.sh spira/conf.sh spira/chamber/builder.md work/* spira-lc/*
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
[ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ] && CARGO_BIN="$HOME/.cargo/bin/cargo"
[ -n "$CARGO_BIN" ] || skip "cargo not found on PATH or at ~/.cargo/bin"
DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH — install dolt before running this suite"

. "$HERE/conf.sh"
export PATH="$(dirname "$CARGO_BIN"):$(dirname "$DOLT_BIN"):$PATH"

unset TESTDB_SHARED TESTDB_NAME TESTDB_DIR TESTDB_BASELINE TESTDB_BIN \
      TESTDB_MODE TESTDB_STARTED_SERVICE SPIRA_DB
. "$HERE/testdb.sh"
testdb_require "test-aeon-lifecycle-cutover.sh"

REPO="$(cd "$HERE/.." && pwd)"
TMP="$(mktemp -d)"
PORT=$((SPIRA_LC_TESTDB_PORT + 900 + (RANDOM % 500)))
SERVER_PID=""
SERVE_PID=""

cleanup() {
    [ -n "$SERVE_PID" ] && kill "$SERVE_PID" >/dev/null 2>&1
    [ -n "$SERVER_PID" ] && kill "$SERVER_PID" >/dev/null 2>&1
    testdb_drop >/dev/null 2>&1
    rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

testdb_up "test-aeon-lifecycle-cutover"
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

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

CARGO_TARGET_DIR_FOR_BUILD="$TMP/cargo-target"
CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$CARGO_TARGET_DIR_FOR_BUILD" \
    "$CARGO_BIN" build --manifest-path "$REPO/spira-lc/Cargo.toml" --quiet 2>"$TMP/build-lc.log" \
    || bail "spira-lc failed to build: $(cat "$TMP/build-lc.log")"
CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$CARGO_TARGET_DIR_FOR_BUILD" \
    "$CARGO_BIN" build --manifest-path "$REPO/work/Cargo.toml" --quiet 2>"$TMP/build-work.log" \
    || bail "work failed to build: $(cat "$TMP/build-work.log")"
export SPIRA_LC_BIN="$CARGO_TARGET_DIR_FOR_BUILD/debug/spira-lc"
export SPIRA_WORK_BIN="$CARGO_TARGET_DIR_FOR_BUILD/debug/work"

export SPIRA_LC_HOST=127.0.0.1
export SPIRA_LC_PORT="$PORT"
export SPIRA_LC_DB=spira_lifecycle
export SPIRA_LC_DATA_DIR="$TMP"
export SPIRA_LC_USER=root
export SPIRA_LC_PASSWORD=""

"$SPIRA_LC_BIN" admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$TMP/schema.log" 2>&1
wantrc "schema applies cleanly" 0 $?
# This suite connects as root throughout (grants restrict the spira_lc user specifically,
# and grants.sql needs its @SPIRA_LC_PASSWORD@ placeholder filled first) — the grant
# boundary is test-lifecycle-container.sh's job, this suite exercises the verbs and aeon.sh's
# own wiring.

SOCK="$TMP/spira-lc.sock"
SPIRA_LC_SOCKET="$SOCK" "$SPIRA_LC_BIN" serve "$SOCK" >"$TMP/serve.log" 2>&1 &
SERVE_PID=$!
for _ in $(seq 1 50); do
    [ -S "$SOCK" ] && break
    sleep 0.1
done
[ -S "$SOCK" ] || bail "spira-lc serve never created its socket: $(cat "$TMP/serve.log")"
export SPIRA_LC_SOCKET="$SOCK"

seed_bead() {   # seed_bead <bead-id> <state> [holder] [lease_until]
    local holder_sql="NULL" lease_sql="NULL"
    [ -n "${3:-}" ] && holder_sql="'$3'"
    [ -n "${4:-}" ] && lease_sql="$4"
    root_sql --use-db spira_lifecycle sql -q \
        "INSERT INTO bead (bead_id, state, holds, version, updated_at, holder, lease_until) VALUES ('$1','$2','[]',0,0,$holder_sql,$lease_sql)" >/dev/null 2>&1
}
row_json() {   # row_json <bead-id> -> the row's JSON, for want/nowant substring checks
    root_sql --use-db spira_lifecycle sql -q "SELECT state, holder, version FROM bead WHERE bead_id='$1'" -r json 2>/dev/null
}

# shellcheck disable=SC1090
. "$HERE/lib.sh" 2>/dev/null || true

# ===========================================================================
echo
echo "lc_claim_bead: Ready -> Working, and the sp-zw9ot fixture (IN_DELIVERY is refused):"
# ===========================================================================
seed_bead "sp-lcrdy1" READY
lc_claim_bead "sp-lcrdy1" "aeon-t1" 999999999
is "claim from READY: applied (exit 0)" "0" "$?"
want "claim from READY: row is now WORKING" '"state":"WORKING"' "$(row_json sp-lcrdy1)"

# sp-zw9ot: batched at 16:59; an aeon claims at 17:00 -> refused.
seed_bead "sp-zw9ot" IN_DELIVERY
lc_claim_bead "sp-zw9ot" "aeon-t1" 999999999
rc=$?
is "sp-zw9ot fixture: claim on IN_DELIVERY is refused (exit 3)" "3" "$rc"
_zw9="$(row_json sp-zw9ot)"
want "sp-zw9ot fixture: row is untouched, still IN_DELIVERY" '"state":"IN_DELIVERY"' "$_zw9"
want "sp-zw9ot fixture: version did not advance on a refusal" '"version":0' "$_zw9"

# ===========================================================================
echo
echo "lc_claim_bead: a dead holder's stale WORKING row is cleared (HolderDead) then claimed:"
# ===========================================================================
seed_bead "sp-lcdead" WORKING "aeon-dead" 1
lc_claim_bead "sp-lcdead" "aeon-t2" 999999999
is "claim over a dead holder: applied (exit 0)" "0" "$?"
_dead="$(row_json sp-lcdead)"
want "claim over a dead holder: row is WORKING under the new holder" '"state":"WORKING"' "$_dead"
want "claim over a dead holder: holder is the new claimant" '"holder":"aeon-t2"' "$_dead"
want "claim over a dead holder: two transitions applied (HolderDead then Claim)" '"version":2' "$_dead"

# ===========================================================================
echo
echo "lc_release_bead: WORKING returns to READY; past WORKING is a harmless no-op:"
# ===========================================================================
seed_bead "sp-lcrel1" WORKING "aeon-t3" 999999999
lc_release_bead "sp-lcrel1" "aeon-t3"
want "release from WORKING: row is back to READY" '"state":"READY"' "$(row_json sp-lcrel1)"

seed_bead "sp-lcrel2" SUBMITTED
lc_release_bead "sp-lcrel2" "aeon-t3"
rc=$?
is "release past WORKING: never fails the caller" "0" "$rc"
want "release past WORKING: row is untouched (Release illegal from SUBMITTED)" '"state":"SUBMITTED"' "$(row_json sp-lcrel2)"

# ===========================================================================
echo
echo "lc_bead_verified: the disposition read that replaces bd status:"
# ===========================================================================
seed_bead "sp-lcver1" WORKING "aeon-t4" 999999999
lc_bead_verified sp-lcver1
is "WORKING is not yet verified" "1" "$?"

seed_bead "sp-lcver2" SUBMITTED
lc_bead_verified sp-lcver2
is "SUBMITTED is verified" "0" "$?"

seed_bead "sp-lcver3" DONE
lc_bead_verified sp-lcver3
is "DONE is verified" "0" "$?"

# ===========================================================================
echo
echo "End to end through the real aeon.sh: no bd on the model's PATH, WORKING until work submit:"
# ===========================================================================
ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
FREPO="$TMP/repo"; git clone -q "$ORIGIN" "$FREPO" 2>/dev/null
git -C "$FREPO" config user.email t@t; git -C "$FREPO" config user.name t
printf 'seed\n' > "$FREPO/f"
git -C "$FREPO" add f; git -C "$FREPO" commit -qm seed; git -C "$FREPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/spira-home"; mkdir -p "$SPIRA_HOME/chamber"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/aeon.sh" "$HERE/work-env.sh" "$SPIRA_HOME/"
cp -r "$HERE/actors" "$SPIRA_HOME/" 2>/dev/null || true
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$FREPO" > "$SPIRA_REPO_MAP"
cat > "$SPIRA_HOME/chamber/builder.fayth" <<'FAYTH'
FAYTH_NAME=builder
FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,needs-operator"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || bail "aeon.sh has no SPIRA_AGENT injection point — refusing to run the real model"

# The shim IS the model: it proves what its own environment actually grants it, then does
# the one thing this bead's brief tells a real builder to do — `work submit`.
cat > "$BIN/claude" <<SHIM
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="\$(sed -n 's/^work \\(sp-[a-z0-9-]*\\) .*/\\1/p' "$TMP/prompt" | head -1)"
printf '%s' "\$id" > "$TMP/last-bead"

if command -v bd >/dev/null 2>&1; then echo BD_FOUND > "$TMP/bd-found"; fi
[ -n "\${SPIRA_WORK_BEAD_ID:-}" ] || echo NO_BEAD_ID > "$TMP/no-bead-id"
[ "\${SPIRA_WORK_BEAD_ID:-}" = "\$id" ] || echo "BEAD_ID_MISMATCH:\${SPIRA_WORK_BEAD_ID:-}" > "$TMP/bead-id-mismatch"
env | grep -qi "SPIRA_DB\\|SPIRA_BD\\|SPIRA_LC_PASSWORD" && echo CREDENTIAL_LEAKED > "$TMP/credential-leaked"

printf 'my work\n' >> f
git add f && git -c user.email=a@a -c user.name=aeon commit -qm "\$id: the work"

work submit
printf '%s' "\$?" > "$TMP/work-submit-rc"
SHIM
chmod +x "$BIN/claude"

BID="$(bash "$HERE/bead.sh" file "aeon lifecycle cutover container fixture" --for builder --repo fixture 2>/dev/null | tail -1)"
[ -n "$BID" ] || bail "bead.sh file did not return an id"
seed_bead "$BID" READY

bash "$SPIRA_HOME/aeon.sh" builder >"$TMP/aeon.log" 2>&1
is "aeon.sh: exits 0 on a submitted work bead" "0" "$?"

is "model session ran (bead id captured)" "$BID" "$(cat "$TMP/last-bead" 2>/dev/null)"
nowant "no bd on the model's PATH" "BD_FOUND" "$(cat "$TMP/bd-found" 2>/dev/null || printf absent)"
is "SPIRA_WORK_BEAD_ID was set" "" "$(cat "$TMP/no-bead-id" 2>/dev/null || true)"
is "SPIRA_WORK_BEAD_ID names this bead, not another" "" "$(cat "$TMP/bead-id-mismatch" 2>/dev/null || true)"
is "no credential-shaped var leaked to the model" "" "$(cat "$TMP/credential-leaked" 2>/dev/null || true)"
is "work submit exited 0 (applied)" "0" "$(cat "$TMP/work-submit-rc" 2>/dev/null || echo missing)"

want "the machine's row: WORKING (claimed) then SUBMITTED (work submit) — never closed" \
    '"state":"SUBMITTED"' "$(row_json "$BID")"

bstatus="$(bd -C "$SPIRA_DB" show "$BID" --json 2>/dev/null | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
print(d[0].get("status","") if d else "")' 2>/dev/null)"
nowant "bd's own status never reads closed — there is no bd close to reinterpret" "closed" "$bstatus"

tl_summary
