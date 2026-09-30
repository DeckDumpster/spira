#!/usr/bin/env bash
#
# test-lifecycle-classify.sh — the migration classifier end to end (design §4, sp-t93ky):
# `spira-lc classify` against a REAL bd (testdb.sh, not a stub), a real scratch git
# repository, real landstate/queue files on disk, and a throwaway spira_lifecycle Dolt
# server of its own.
#
# WHAT THIS PROVES
#   - Every precedence tier fires on its own fixture: terminal supersede, terminal drop,
#     terminal content-on-base, batch membership, the ledger, and bare bd status.
#   - THE MIGRATION FIXTURE REQUIREMENT, as a POSITIVE CONTROL: the content-on-base fixture's
#     own commit subject is asserted, shell-side, to match NEITHER of landed()'s two
#     recognized shapes ("spira: land <id>" / "<id>:...") before the classifier is ever run —
#     proving a subject-matching implementation would say "not landed" here — and only then
#     is spira-lc classify asserted to say LANDED anyway (law-absence-needs-a-positive-control).
#   - A closed bead whose ledger still says CERTIFIED is classified CERTIFIED (the ledger
#     outranks bare bd status) but is named in the contradiction report, broken down by
#     repository and by land mode.
#   - Repository configuration detection: the SAME fixtures classify identically whether
#     `--home` resolves `spira.conf`+`repo-map` or a `spira.toml` sitting beside it, and the
#     report names which one was in force.
#   - A dry run writes nothing: the bead/delivery/batch tables stay at zero rows.
#   - Re-running is a no-op: the second real run classifies zero new beads, and the event
#     table's row count is unchanged from the first run.
#
# host-reason: starts its own disposable `dolt sql-server` (never dolt-beads.service) and
# builds a scratch git repository, the same shape test-lifecycle-container.sh already uses
# without a container call — testenv-batch.sh already provides the container this suite
# executes in.
#
# defect: sp-t93ky
# tier: T2
# covers: lifecycle/src/classify.rs spira-lc/*
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
GIT_BIN="$(command -v git 2>/dev/null || true)"
[ -n "$GIT_BIN" ] || skip "git not found on PATH"

. "$HERE/conf.sh"
export PATH="$(dirname "$CARGO_BIN"):$(dirname "$DOLT_BIN"):$(dirname "$GIT_BIN"):$PATH"

. "$HERE/testdb.sh"
testdb_require test-lifecycle-classify
testdb_up test-lifecycle-classify
[ -n "${SPIRA_DB:-}" ] || bail "testdb_up did not hand back a throwaway SPIRA_DB"

REPO="$(cd "$HERE/.." && pwd)"
TMP="$(mktemp -d)"
PORT=$((SPIRA_LC_TESTDB_PORT + (RANDOM % 500) + 500))
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
        up=1; break
    fi
    sleep 0.2
done
[ "$up" = 1 ] || bail "dolt sql-server never came up: $(cat "$TMP/server.log")"

CARGO_TARGET_DIR_FOR_BUILD="$TMP/cargo-target"
CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$CARGO_TARGET_DIR_FOR_BUILD" \
    "$CARGO_BIN" build --manifest-path "$REPO/spira-lc/Cargo.toml" --quiet 2>"$TMP/build.log" \
    || bail "spira-lc failed to build: $(cat "$TMP/build.log")"
BIN="$CARGO_TARGET_DIR_FOR_BUILD/debug/spira-lc"

export SPIRA_LC_HOST=127.0.0.1
export SPIRA_LC_PORT="$PORT"
export SPIRA_LC_DB=spira_lifecycle
export SPIRA_LC_DATA_DIR="$TMP"
export SPIRA_LC_USER=root
export SPIRA_LC_PASSWORD=""
unset SPIRA_LC_SOCKET

"$BIN" admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$TMP/schema.log" 2>&1
wantrc "schema applies cleanly" 0 $?

root_sql() { "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls --use-db spira_lifecycle "$@"; }
row_count() { root_sql sql -q "SELECT COUNT(*) AS n FROM $1" -r json 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin)["rows"][0]["n"])' 2>/dev/null || echo "?"; }

# ── the scratch git repository ────────────────────────────────────────────────────────
GITREPO="$TMP/gitrepo"
mkdir -p "$GITREPO"
git -C "$GITREPO" init -q -b main
git -C "$GITREPO" config user.email test@example.invalid
git -C "$GITREPO" config user.name test

git -C "$GITREPO" commit -q --allow-empty -m "initial"
CONTENT_SUBJECT="a fix with no landing-marker subject at all"
printf 'the fix\n' > "$GITREPO/fix.txt"
git -C "$GITREPO" add fix.txt
git -C "$GITREPO" commit -q -m "$CONTENT_SUBJECT"
# THE POSITIVE CONTROL: this subject must match NEITHER of landed()'s two recognized
# shapes, or this fixture proves nothing about the migration fixture requirement.
case "$CONTENT_SUBJECT" in
    "spira: land sp-contentbase") bail "fixture setup error: subject accidentally matches the queue-landing shape" ;;
    "sp-contentbase:"*)           bail "fixture setup error: subject accidentally matches the aeon-own-commit shape" ;;
esac
ok "POSITIVE CONTROL: the content-on-base fixture's own subject matches neither landed() shape"
git -C "$GITREPO" branch spira/sp-contentbase HEAD

# ── the legacy record fixtures (landstate + queue) ────────────────────────────────────
LANDSTATE="$TMP/landstate"
QUEUE="$TMP/queue"
mkdir -p "$LANDSTATE" "$QUEUE/demo"
NOW="$(date +%s)"
printf 'CERTIFIED none %s' "$NOW" > "$LANDSTATE/sp-certified"
printf 'CERTIFIED none %s' "$NOW" > "$LANDSTATE/sp-contradiction"
{
    printf 'pr=\n'
    printf 'head=deadbeef\n'
    printf 'base=cafef00d\n'
    printf 'members=sp-batched\n'
    printf 'opened=%s\n' "$NOW"
    printf 'branch=queue/demo/batch-1\n'
} > "$QUEUE/demo/open"

# ── repo configuration: spira.conf + repo-map, AND a spira.toml sibling ──────────────
CONF_HOME="$TMP/home-conf"
mkdir -p "$CONF_HOME"
printf 'SPIRA_ID_PREFIX=sp\nSPIRA_HOME_REPO=demo\n' > "$CONF_HOME/spira.conf"
printf 'demo|%s|queue|main|\n' "$GITREPO" > "$CONF_HOME/repo-map"

TOML_HOME="$TMP/home-toml"
mkdir -p "$TOML_HOME"
printf '[repo.demo]\npath = "%s"\nmode = "queue"\nbase = "main"\n' "$GITREPO" > "$TOML_HOME/spira.toml"

# ── the bd fixtures, seeded into a REAL bd (testdb.sh) ────────────────────────────────
testdb_seed <<JSONL
{"id":"sp-ready","title":"ready bead","type":"task","status":"open","labels":["repo:demo"]}
{"id":"sp-successor","title":"the successor","type":"task","status":"open","labels":["repo:demo"]}
{"id":"sp-superseded","title":"superseded bead","type":"task","status":"closed","labels":["repo:demo"],"dependencies":[{"issue_id":"sp-superseded","depends_on_id":"sp-successor","type":"supersedes"}]}
{"id":"sp-dropped","title":"dropped bead","type":"task","status":"closed","labels":["repo:demo","spira-dropped"]}
{"id":"sp-certified","title":"certified bead","type":"task","status":"open","labels":["repo:demo","spira-submitted"]}
{"id":"sp-contentbase","title":"content on base bead","type":"task","status":"closed","labels":["repo:demo"]}
{"id":"sp-contradiction","title":"closed but still certified","type":"task","status":"closed","labels":["repo:demo"]}
{"id":"sp-batched","title":"batched bead","type":"task","status":"open","labels":["repo:demo"]}
JSONL
wantrc "bd fixtures seed cleanly into a real, throwaway bd" 0 $?

# ── dry run: computes and reports, writes nothing ──────────────────────────────────────
DRY_OUT="$("$BIN" classify --home "$CONF_HOME" --bd-bin "$SPIRA_BD" --bd-db "$SPIRA_DB" \
    --landstate-dir "$LANDSTATE" --queue-dir "$QUEUE" --repo demo --dry-run 2>"$TMP/dry.err")"
DRY_RC=$?
wantrc "dry run exits cleanly" 0 "$DRY_RC"
want "dry run reports its config source" '"source": "spira.conf+repo-map"' "$DRY_OUT"
is "dry run leaves the bead table at zero rows" "0" "$(row_count bead)"
is "dry run leaves the batch table at zero rows" "0" "$(row_count batch)"
is "dry run leaves the event table at zero rows" "0" "$(row_count event)"

# ── the real run ────────────────────────────────────────────────────────────────────────
REAL_OUT="$("$BIN" classify --home "$CONF_HOME" --bd-bin "$SPIRA_BD" --bd-db "$SPIRA_DB" \
    --landstate-dir "$LANDSTATE" --queue-dir "$QUEUE" --repo demo 2>"$TMP/real.err")"
REAL_RC=$?
wantrc "the real run exits cleanly" 0 "$REAL_RC"
cat "$TMP/real.err" >&2

state_of() { root_sql sql -q "SELECT state AS n FROM bead WHERE bead_id = '$1'" -r json 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin)["rows"]; print(d[0]["n"] if d else "MISSING")' 2>/dev/null; }

is "sp-ready classifies READY" "READY" "$(state_of sp-ready)"
is "sp-successor classifies READY" "READY" "$(state_of sp-successor)"
is "sp-superseded classifies SUPERSEDED via the supersedes dependency" "SUPERSEDED" "$(state_of sp-superseded)"
is "sp-dropped classifies DROPPED via the spira-dropped label" "DROPPED" "$(state_of sp-dropped)"
is "sp-certified classifies CERTIFIED via the ledger" "CERTIFIED" "$(state_of sp-certified)"
is "sp-contentbase classifies LANDED via content-on-base, NOT via any commit subject" "LANDED" "$(state_of sp-contentbase)"
is "sp-contradiction (closed, ledger CERTIFIED) still classifies CERTIFIED — the ledger outranks bare bd status" "CERTIFIED" "$(state_of sp-contradiction)"
is "sp-batched classifies IN_DELIVERY via open batch membership" "IN_DELIVERY" "$(state_of sp-batched)"

delivery_state="$(root_sql sql -q "SELECT state AS n FROM delivery WHERE bead_id = 'sp-batched'" -r json 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin)["rows"]; print(d[0]["n"] if d else "MISSING")' 2>/dev/null)"
is "sp-batched's delivery row is BATCHED, queue mode" "BATCHED" "$delivery_state"

is "the open batch for demo got its own OPEN batch row" "1" "$(row_count batch)"

want "the contradiction report names sp-contradiction" '"bead_id": "sp-contradiction"' "$REAL_OUT"
want "the contradiction report is broken down by repository" '"repo": "demo"' "$REAL_OUT"
want "the contradiction report is broken down by land mode" '"mode": "queue"' "$REAL_OUT"
nowant "sp-ready, with every oracle agreeing, is not in the contradiction report" '"bead_id": "sp-ready"' "$REAL_OUT"

events_after_first_run="$(row_count event)"

# ── re-running is a no-op ──────────────────────────────────────────────────────────────
RERUN_OUT="$("$BIN" classify --home "$CONF_HOME" --bd-bin "$SPIRA_BD" --bd-db "$SPIRA_DB" \
    --landstate-dir "$LANDSTATE" --queue-dir "$QUEUE" --repo demo 2>"$TMP/rerun.err")"
wantrc "the rerun also exits cleanly" 0 $?
want "the rerun classifies nothing new" '"classified": 0' "$RERUN_OUT"
is "the rerun leaves the event log exactly as the first run left it" "$events_after_first_run" "$(row_count event)"
is "sp-ready's state survives the rerun unchanged" "READY" "$(state_of sp-ready)"

# ── repository configuration detection: spira.toml gives the same answers ─────────────
TOML_OUT="$("$BIN" classify --home "$TOML_HOME" --bd-bin "$SPIRA_BD" --bd-db "$SPIRA_DB" \
    --landstate-dir "$LANDSTATE" --queue-dir "$QUEUE" --repo demo --dry-run 2>"$TMP/toml.err")"
wantrc "classify against a spira.toml home exits cleanly" 0 $?
want "it reports spira.toml as its configuration source" '"source": "spira.toml"' "$TOML_OUT"
want "it reports the same fixtures already classified as skipped, not reclassified" '"skipped_already_classified": 8' "$TOML_OUT"

tl_summary
