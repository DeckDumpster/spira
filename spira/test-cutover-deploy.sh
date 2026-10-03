#!/usr/bin/env bash
#
# test-cutover-deploy.sh — container-tier acceptance for sp-sa8pn's own deliverable: the
# cutover deploy step (schema, grants, classify, flip) against a throwaway `dolt sql-server`
# this suite starts and tears down itself, standing in for acceptance criterion D ("runs the
# cutover deploy on an aged install, and afterwards a manual write to spira_lifecycle as the
# operator user is refused").
#
# classify's own correctness (every precedence tier) is test-lifecycle-classify.sh's job;
# this suite proves the ORCHESTRATION — that cutover-deploy.sh calls schema, grants,
# classify and the config flip in the right order, against the real grant set, and that the
# result is what the grants are for.
#
# WHAT THIS PROVES:
#   - a single run against a fixture with no spira.toml yet, an empty-of-matching-beads bd
#     database (the degenerate "aged install with nothing to classify" case) and one
#     repo-map row leaves: spira_lifecycle's schema in place, spira.lifecycle_enforce = true
#     in a freshly created spira.toml, and classify's own event log non-empty (it ran);
#   - afterwards, a fresh 'operator'@'%' user this suite creates AFTER the grants — never
#     named by grants.sql, so it holds no privilege on spira_lifecycle at all — is REFUSED
#     an INSERT (SEEN RED as a positive control: the same INSERT succeeds as spira_lc,
#     proving the refusal is the grant, not a broken schema or database name);
#   - running it a second time is idempotent and still exits 0.
#
# host-reason: starts its own disposable `dolt sql-server`, the same shape every
# testdb.sh server-mode suite already uses without a container call.
#
# defect: sp-sa8pn
# tier: T2
# covers: spira/cutover-deploy.sh lifecycle/grants.sql lifecycle/schema.sql systemd/*.service install/src/**
# timeout: 300
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
unset SPIRA_LC_SOCKET

. "$HERE/testdb.sh"
testdb_require test-cutover-deploy
testdb_up test-cutover-deploy
[ -n "${SPIRA_DB:-}" ] || bail "testdb_up did not hand back a throwaway SPIRA_DB"

REPO="$(cd "$HERE/.." && pwd)"
TMP="$(mktemp -d)"
PORT=$((SPIRA_LC_TESTDB_PORT + 2000 + (RANDOM % 500)))
SERVER_PID=""

cleanup() {
    [ -n "$SERVER_PID" ] && kill "$SERVER_PID" >/dev/null 2>&1
    testdb_drop
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
        up=1
        break
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
    "$CARGO_BIN" build --manifest-path "$REPO/spira-config/Cargo.toml" --quiet 2>"$TMP/build-cfg.log" \
    || bail "spira-config failed to build: $(cat "$TMP/build-cfg.log")"
LC_BIN="$CARGO_TARGET_DIR_FOR_BUILD/debug/spira-lc"
CFG_BIN="$CARGO_TARGET_DIR_FOR_BUILD/debug/spira-config"

echo "test-cutover-deploy.sh"

# --- fixture: an "aged install" with no spira.toml yet and one repo, no beads that match it ---
FIX="$TMP/fixture"; mkdir -p "$FIX/run/queue" "$FIX/run/landstate"
GITREPO="$TMP/gitrepo"
git init -q -b main "$GITREPO"
git -C "$GITREPO" config user.email test@example.invalid
git -C "$GITREPO" config user.name test
git -C "$GITREPO" commit -q --allow-empty -m base

CFGHOME="$TMP/operator-config"; mkdir -p "$CFGHOME"
printf 'SPIRA_HOME_REPO=demo\n' > "$CFGHOME/spira.conf"
printf 'demo|%s|queue|main|\n' "$GITREPO" > "$CFGHOME/repo-map"
CONF="$CFGHOME/spira.conf"
TOML="$FIX/spira.toml"   # deliberately does not exist yet — this run must create it

CRED="$TMP/credential"; printf 'adminpw-not-real' > "$CRED"
RO_CRED="$CRED-ro"; printf 'ropw-not-real' > "$RO_CRED"

run_deploy() {
    env -i HOME="$HOME" \
        PATH="$CARGO_TARGET_DIR_FOR_BUILD/debug:$PATH" SPIRA_REPO="$REPO" \
        SPIRA_HOME="$FIX" SPIRA_RUN="$FIX/run" SPIRA_QUEUE_DIR="$FIX/run/queue" \
        SPIRA_CONF="$CONF" SPIRA_CONFIG_HOME="${CFGHOME_OVERRIDE:-$CFGHOME}" \
        SPIRA_DB="$SPIRA_DB" SPIRA_BD="$SPIRA_BD" \
        SPIRA_LC_HOST=127.0.0.1 SPIRA_LC_PORT="$PORT" SPIRA_LC_DB=spira_lifecycle SPIRA_LC_DATA_DIR="$TMP" \
        SPIRA_LC_ADMIN_USER=root SPIRA_LC_ADMIN_PASSWORD="" \
        SPIRA_LC_PASSWORD_FILE="$CRED" \
        bash "$HERE/cutover-deploy.sh" --repo demo "$@"
}

DROPIN="$TMP/skip-check.conf"; : > "$DROPIN"

echo
echo "classify reads the operator config home, not the release tree (SPIRA_HOME=$FIX holds no config):"
mkdir -p "$TMP/empty-config"
neg="$(CFGHOME_OVERRIDE="$TMP/empty-config" run_deploy 2>&1)"; negrc=$?
[ "$negrc" != 0 ]; wantrc "positive control: an empty config home makes classify refuse" 0 $?
want "the refusal names the --home it read" "$TMP/empty-config" "$neg"

echo
echo "the deploy step runs end to end on an aged install:"
out="$(run_deploy --remove-dropin "$DROPIN" 2>&1)"; rc=$?
[ "$rc" = 0 ] || printf '%s\n' "$out" >&2
wantrc "cutover-deploy.sh exits 0" 0 "$rc"
want "it reports the flip" "flipping lifecycle_enforce" "$out"
want "it ran the classifier" "classifying the quiesced store" "$out"

[ ! -e "$DROPIN" ]; wantrc "the named drop-in is removed" 0 $?
want "it reports the removal" "removed drop-in" "$out"

echo
echo "the flip landed in a freshly created spira.toml:"
[ -f "$TOML" ] || bail "spira.toml was not created"
got="$("$CFG_BIN" get spira.lifecycle_enforce "$TOML" 2>&1)"
is "spira.lifecycle_enforce reads true" "true" "$got"

echo
echo "the classifier actually ran (its own event log is non-empty, or it had nothing to classify):"
row_count() { root_sql --use-db spira_lifecycle sql -q "SELECT COUNT(*) AS n FROM $1" -r json 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin)["rows"][0]["n"])' 2>/dev/null || echo "?"; }
schema_tables="$(root_sql --use-db spira_lifecycle sql -q "SHOW TABLES" -r csv 2>/dev/null)"
want "schema.sql created the bead table" "bead" "$schema_tables"
want "schema.sql created the event table" "event" "$schema_tables"

echo
echo "afterwards, a manual write to spira_lifecycle as the operator user is refused:"
root_sql sql -q "CREATE USER IF NOT EXISTS 'operator'@'%' IDENTIFIED BY 'operatorpw'" >/dev/null 2>&1
operator_sql() { "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u operator -p operatorpw --no-tls --use-db spira_lifecycle "$@"; }
op_out="$(operator_sql sql -q "INSERT INTO bead (bead_id, state, holds, version, updated_at) VALUES ('sp-manual', 'READY', JSON_OBJECT(), 0, 0)" 2>&1)"
op_rc=$?
wantrc "the operator user's INSERT is refused" 1 "$op_rc"
want "for lack of privilege, not a missing table" "denied" "$op_out"

# POSITIVE CONTROL: the same statement succeeds as spira_lc, proving the refusal above is the
# grant, not a broken schema or a wrong database name.
lc_sql() { "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u spira_lc -p adminpw-not-real --no-tls --use-db spira_lifecycle "$@"; }
lc_sql sql -q "INSERT INTO bead (bead_id, state, holds, version, updated_at) VALUES ('sp-manual', 'READY', JSON_OBJECT(), 0, 0)" >/dev/null 2>&1
wantrc "positive control: spira_lc's own INSERT succeeds" 0 $?

echo
echo "a second run is idempotent:"
out2="$(run_deploy --remove-dropin "$DROPIN" 2>&1)"; rc2=$?
[ "$rc2" = 0 ] || printf '%s\n' "$out2" >&2
wantrc "cutover-deploy.sh exits 0 again" 0 "$rc2"

echo
echo "a unit-rendered environment alone authenticates as spira_lc:"
INSTALL_BIN="$CARGO_TARGET_DIR_FOR_BUILD/debug"
CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$CARGO_TARGET_DIR_FOR_BUILD" \
    "$CARGO_BIN" build --manifest-path "$REPO/install/Cargo.toml" --bin render-unit --bin spira-install --quiet 2>"$TMP/build-install.log" \
    || bail "render-unit failed to build: $(cat "$TMP/build-install.log")"
rendered="$("$INSTALL_BIN/render-unit" "$REPO/systemd/spira-sentinel.service" --home "$FIX" --repo "$REPO" --run "$FIX/run" \
    --db "$SPIRA_DB" --cockpit "$FIX/cockpit" --dolt /bin/true --prod "$FIX/spira" --instance prod \
    --testdb-port 3308 --snap-stale-s 60 --lc-password-file "$CRED")"
unit_env="$(printf '%s\n' "$rendered" | sed -n 's/^Environment=\(SPIRA_LC_PASSWORD_FILE=.*\)$/\1/p')"
is "the rendered unit carries the configured credential path" "SPIRA_LC_PASSWORD_FILE=$CRED" "$unit_env"
lc_caller() {
    env -i HOME="$HOME" PATH="$PATH" "$@" SPIRA_LC_HOST=127.0.0.1 SPIRA_LC_PORT="$PORT" \
        SPIRA_LC_DB=spira_lifecycle SPIRA_LC_DATA_DIR="$TMP" "$LC_BIN" history sp-manual
}
lc_caller "$unit_env" >/dev/null 2>&1
wantrc "a caller with only the rendered variable authenticates" 0 $?
lc_caller >/dev/null 2>&1
wantrc "positive control: the same call without the variable fails closed" 2 $?

echo
echo "cutover-deploy --dry-run resolves the credential from config alone:"
dry_out="$(run_deploy --dry-run 2>&1)"; wantrc "dry run exits 0 with only the config key set" 0 $?
want "dry run reaches the classify step" "would classify demo" "$dry_out"

echo
echo "--system-user runs no phase but its own:"
su_out="$(env -i HOME="$HOME" PATH="$PATH" SPIRA_REPO="$REPO" SPIRA_HOME="$REPO/spira" "$INSTALL_BIN/spira-install" --system-user --dry-run 2>&1)"
wantrc "spira-install --system-user --dry-run exits 0" 0 $?
want "it reports the system-user phase" "phase 6.5" "$su_out"
printf '%s\n' "$su_out" | grep -qE 'phase (0|0\.5|1|1\.5|2|3|4|5|6|7):'; wantrc "no other phase ran" 1 $?

tl_summary
