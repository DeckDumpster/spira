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
#   - lc_claim_bead never takes over a WORKING row (sp-860zj): the row is the claim, and a
#     holder is cleared only by the stale-lease reaper's HolderDead, never by a claimant.
#   - lc_release_bead (release_own_claim's own lifecycle half) returns a WORKING row to READY,
#     and is a harmless no-op past WORKING (SUBMITTED and beyond refuse Release, by design).
#   - End to end through the real aeon.sh: a claimed bead reads WORKING right up until the
#     model's own `work submit` applies — never "closed" — and the model that submitted it
#     had no `bd` on its PATH at all.
#   - Stacked dependents (design stacked-dependents-2026-09-28 §1, sp-s9675.2): a bead
#     blocked only on a CERTIFIED-but-not-LANDED prerequisite is invisible to bd's own
#     `ready` (bd has no concept of CERTIFIED, so an open blocker hides it) but is claimed
#     end to end through aeon's ready set — the machine's READY rows, `spira-claim
#     fayth-ready --json` — once `lifecycle_enforce` is on.
#
# host-reason: starts its own disposable `dolt sql-server`, same shape as
# test-lifecycle-container.sh / test-work-container.sh; testenv-batch.sh already provides
# the container this suite executes in.
#
# tier: T2
# covers: aeon/src/* spira/lib.sh spira/conf.sh spira/chamber/builder.md work/* spira-lc/* spira-claim/src/*
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

# spira-lc and work are the tree's own build, invoked by name on the suite's PATH (sp-gypjk).
for _t in spira-lc work; do command -v "$_t" >/dev/null 2>&1 || bail "$_t is not on PATH"; done

export SPIRA_LC_HOST=127.0.0.1
export SPIRA_LC_PORT="$PORT"
export SPIRA_LC_DB=spira_lifecycle
export SPIRA_LC_DATA_DIR="$TMP"
export SPIRA_LC_USER=root
export SPIRA_LC_PASSWORD=""

spira-lc admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$TMP/schema.log" 2>&1
wantrc "schema applies cleanly" 0 $?
# This suite connects as root throughout (grants restrict the spira_lc user specifically,
# and grants.sql needs its @SPIRA_LC_PASSWORD@ placeholder filled first) — the grant
# boundary is test-lifecycle-container.sh's job, this suite exercises the verbs and aeon.sh's
# own wiring.

# ===========================================================================
echo
echo "spira-lc list carries stack/stack_depth (sp-s9675.2, DESIGN.md §5 item 14):"
# ===========================================================================
root_sql --use-db spira_lifecycle sql -q \
    "INSERT INTO bead (bead_id, state, holds, version, updated_at, stack, stack_depth) VALUES ('sp-lc-unstacked','READY','[]',0,0,JSON_OBJECT(),0)" >/dev/null 2>&1
root_sql --use-db spira_lifecycle sql -q \
    "INSERT INTO bead (bead_id, state, holds, version, updated_at, stack, stack_depth) VALUES ('sp-lc-stacked','CERTIFIED','[]',0,0,JSON_OBJECT('sp-lc-below','tip-below'),2)" >/dev/null 2>&1
list_json="$(spira-lc list 2>/dev/null)"
# POSITIVE CONTROL: the unstacked row must read depth 0, so the stacked row's depth 2 below
# is the SELECT actually returning the column, not every row defaulting to a fixed value.
stacked_row="$(printf '%s' "$list_json" | python3 -c '
import json, sys
rows = json.load(sys.stdin)
for r in rows:
    if r.get("bead_id") == "sp-lc-stacked":
        print(json.dumps(r, separators=(",", ":")))
')"
unstacked_row="$(printf '%s' "$list_json" | python3 -c '
import json, sys
rows = json.load(sys.stdin)
for r in rows:
    if r.get("bead_id") == "sp-lc-unstacked":
        print(json.dumps(r, separators=(",", ":")))
')"
want "an unstacked row reads stack_depth 0 (positive control)" '"stack_depth":"0"' "$unstacked_row"
want "spira-lc list carries a stacked row's real stack_depth, not the column-missing default" '"stack_depth":"2"' "$stacked_row"
want "spira-lc list carries the stacked row's own stack map" 'sp-lc-below' "$stacked_row"

SOCK="$TMP/spira-lc.sock"
SPIRA_LC_SOCKET="$SOCK" spira-lc serve "$SOCK" >"$TMP/serve.log" 2>&1 &
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
        "REPLACE INTO bead (bead_id, state, holds, version, updated_at, holder, lease_until) VALUES ('$1','$2','[]',0,0,$holder_sql,$lease_sql)" >/dev/null 2>&1
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
want "sp-zw9ot fixture: version did not advance on a refusal" '"version":"0"' "$_zw9"

# sp-zw9ot: the batch lands at 17:05 -> LANDED; a late submit at 17:16 is refused, row unchanged.
lc_event_bead "sp-zw9ot" IN_DELIVERY 0 "batch" '{"Delivered":{"merge_sha":"abc123","proof":"batch-merge"}}'
is "sp-zw9ot fixture: batch landing applies (exit 0)" "0" "$?"
_zw9="$(row_json sp-zw9ot)"
want "sp-zw9ot fixture: bead is LANDED" '"state":"LANDED"' "$_zw9"
want "sp-zw9ot fixture: landing advanced the version" '"version":"1"' "$_zw9"
lc_event_bead "sp-zw9ot" LANDED 1 "aeon-t1" '{"Submit":{"tip":"late-tip"}}'
is "sp-zw9ot fixture: late submit on LANDED is refused (exit 3)" "3" "$?"
_zw9="$(row_json sp-zw9ot)"
want "sp-zw9ot fixture: late submit left the row LANDED" '"state":"LANDED"' "$_zw9"
want "sp-zw9ot fixture: late submit did not advance the version" '"version":"1"' "$_zw9"

# ===========================================================================
echo
echo "lc_claim_bead: a WORKING row is refused, never taken over (sp-860zj):"
# ===========================================================================
seed_bead "sp-lcheld" WORKING "aeon-live" 999999999
lc_claim_bead "sp-lcheld" "aeon-t2" 999999999
is "claim over a live holder: refused (exit 3)" "3" "$?"
_held="$(row_json sp-lcheld)"
want "claim over a live holder: the holder keeps it" '"holder":"aeon-live"' "$_held"
want "claim over a live holder: no transition applied" '"version":"0"' "$_held"

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
# work-env.sh is retired (sp-zpaq0): the aeon binary builds its own restricted environment.
cp "$HERE/lib.sh" "$HERE/conf.sh" "$SPIRA_HOME/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SPIRA_HOME/"
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
# The restricted path is an explicit config decision (sp-74gzo), not a fact discovered from
# these binaries existing — without this, aeon.sh takes the legacy path even though
# spira-lc and work are both on PATH (checked above).
export SPIRA_LIFECYCLE_ENFORCE=1
command -v aeon >/dev/null 2>&1 \
    || bail "aeon is not on PATH — refusing to run the real model"

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

BID="$(bead.sh file "aeon lifecycle cutover container fixture" --for builder --repo fixture 2>/dev/null | tail -1)"
[ -n "$BID" ] || bail "bead.sh file did not return an id"
seed_bead "$BID" READY

aeon --home "$SPIRA_HOME" builder >"$TMP/aeon.log" 2>&1
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

# ===========================================================================
echo
echo "Stacked dependents (sp-s9675.2): a dependent blocked only on a CERTIFIED-not-LANDED"
echo "prerequisite is invisible to bd ready, but claimable through the machine's ready set:"
# ===========================================================================
# SPIRA_LIFECYCLE_ENFORCE is already 1 (set above, for $BID's own restricted-path run).

bead_id_lines() {   # bead_id_lines <bd-json-on-stdin> -> one id per line
    python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
except ValueError:
    d = []
d = d if isinstance(d, list) else [d]
for r in d:
    print(r.get("id", ""))
'
}

PREREQ="$(bash "$HERE/bead.sh" file "stack fixture: prerequisite" --for builder --repo fixture 2>/dev/null | tail -1)"
[ -n "$PREREQ" ] || bail "bead.sh file did not return an id for the prerequisite"
DEP="$(bash "$HERE/bead.sh" file "stack fixture: dependent" --for builder --repo fixture 2>/dev/null | tail -1)"
[ -n "$DEP" ] || bail "bead.sh file did not return an id for the dependent"

bash "$HERE/bead.sh" dep add "$DEP" "$PREREQ" >/dev/null 2>&1
wantrc "dep add wires the dependent's blocks edge onto the prerequisite" 0 $?

# The prerequisite is CERTIFIED in the lifecycle machine but bd never closes it — bd's own
# ready/blocker semantics know only open/closed, so it stays a real, open blocker in bd's
# eyes even once the lifecycle machine has moved past it. Its bd assignee is content the
# claim never reads. Its held bd row is fixture state, declared as data (sp-voip5), not a
# bd update --status.
testdb_restate "$PREREQ" in_progress aeon-other-actor
seed_bead "$PREREQ" CERTIFIED
seed_bead "$DEP" READY

# POSITIVE CONTROL: bd's own ready hides the dependent while its blocker is only CERTIFIED —
# the exact defect sp-s9675.2 found. If this ever stops failing, the fixture stopped
# reproducing it and the assertions below prove nothing.
ready_ids=" $(bdq "${READY_ARGS[@]}" --json 2>/dev/null | bead_id_lines | tr '\n' ' ') "
nowant "bd ready hides the dependent behind its bd-open, CERTIFIED blocker" " $DEP " "$ready_ids"

set_ids=" $(_spira_claim fayth-ready builder --json 2>/dev/null | bead_id_lines | tr '\n' ' ') "
want "the aeon's ready set (the machine's READY rows) carries the dependent" " $DEP " "$set_ids"
nowant "the aeon's ready set leaves out the CERTIFIED prerequisite bd shows in_progress" " $PREREQ " "$set_ids"

aeon --home "$SPIRA_HOME" builder >"$TMP/aeon-stack.log" 2>&1
is "aeon.sh: exits 0 claiming the stacked dependent" "0" "$?"
is "the dependent, not the prerequisite, was claimed (the machine's ready set)" "$DEP" "$(cat "$TMP/last-bead" 2>/dev/null)"
is "the dependent's own session ran work submit to completion" "0" "$(cat "$TMP/work-submit-rc" 2>/dev/null || echo missing)"

want "the dependent's machine row reads SUBMITTED: it was claimed, then work submit applied" \
    '"state":"SUBMITTED"' "$(row_json "$DEP")"

tl_summary
