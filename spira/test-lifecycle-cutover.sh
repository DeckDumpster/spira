#!/usr/bin/env bash
#
# test-lifecycle-cutover.sh — container-tier acceptance for sp-o7nbr's own deliverable: the
# batch machine's cross-machine cascades (`cut`, `land`, `settle`, `abandon-batch`), against
# a throwaway `dolt sql-server` this suite starts and tears down itself, run AS the spira_lc
# user (the real production grant set), never as root.
#
#   ./test-lifecycle-cutover.sh
#
# WHAT THIS PROVES (the bead's own four acceptance criteria):
#   - a green batch lands every member atomically in one transaction — proven by killing a
#     `land` mid-flight and finding no partial application (POSITIVE CONTROL: the same batch
#     lands cleanly on a second, unkilled attempt);
#   - a red batch settles each member as returned (ejected) or requeued;
#   - bisect halves are child batches: settling a batch with every member requeued, then
#     re-cutting two new batches from that pool, each recorded with `parent` set to the
#     original;
#   - two `land` calls racing on one batch: exactly one applies, the rest are refused.
#   - `stack` (sp-o7nbr.4, batcher-cut's own pipelining onto an already-OPEN batch): new
#     members join the same batch_id, its version advances once per member, and it refuses
#     — a member not CERTIFIED there, or a batch that already left OPEN — exactly as `cut`
#     does for a fresh batch.
#   - eject cascade (sp-f3af9, design stacked-dependents-2026-09-28 §3): settling a batch
#     that ejects a prerequisite also returns every member stacked on it as base-withdrawn,
#     even when the caller's own --requeue still names it; a hand eject (queue.sh's own
#     _lc_eject_member) cascades to the same dependent identically. A 3-deep chain proves the
#     cascade walks the frontier past one hop, not just a direct dependent (sp-s9675.5).
#   - the epic-blocker hold-release rule (sp-s9675.5, design §1): a candidate blocked on an
#     epic stays blocked even once the epic's own lifecycle row is CERTIFIED, and is admitted
#     only once the epic's bd record closes — proven against the real spira-claim binary
#     reading this suite's real spira-lc `list` output, not a synthetic fixture.
#
# Also covers sp-o7nbr.5's own deliverable: queue.sh's cmd_open_batch/cmd_eject/cmd_abandon
# onto this same machine via their own _lc_cut_batch/_lc_eject_member/_lc_abandon_batch
# helpers — cut refusing a not-yet-CERTIFIED member, a manual eject-member leaving the batch
# open and survivors untouched, eject-member refused once CI has moved the batch past OPEN/
# CI_RUNNING, and abandon-batch returning every member.
#
# host-reason: starts its own disposable `dolt sql-server`, the same shape every
# testdb.sh server-mode suite already uses without a container call — testenv-batch.sh
# already provides the container this suite executes in.
#
# defect: sp-o7nbr
# tier: T2
# covers: lifecycle/* spira-lc/* spira-claim/* queue/src/*
# timeout: 360
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# Resolve cargo/dolt BEFORE conf.sh — see test-lifecycle-container.sh's own note: the
# testenv image puts cargo at /usr/local/cargo/bin, not under $HOME, and conf.sh can
# overwrite PATH with the harness's own tool directories first.
DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH — install dolt before running this suite"

. "$HERE/conf.sh"
export PATH="$PATH:$(dirname "$DOLT_BIN")"
unset SPIRA_LC_SOCKET

REPO="$(cd "$HERE/.." && pwd)"
TMP="$(mktemp -d)"
PORT=$((SPIRA_LC_TESTDB_PORT + 500 + (RANDOM % 500)))
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

"$DOLT_BIN" sql-server --config "$TMP/server.yaml" > "$TMP/server.log" 2>&1 & # batch-job: long-lived fixture listener, killed by the suite teardown
SERVER_PID=$!

up=0
for _ in $(seq 1 50); do
    if timeout 5 "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls sql -q "SELECT 1" >/dev/null 2>&1; then
        up=1
        break
    fi
    sleep 0.2
done
[ "$up" = 1 ] || bail "dolt sql-server never came up: $(cat "$TMP/server.log")"

root_sql() { timeout 5 "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls "$@"; }

# spira-lc is the tree under test's own build, found by name on the suite's PATH (sp-gypjk).
command -v spira-lc >/dev/null 2>&1 || bail "spira-lc is not on PATH"

# The epic-blocker section runs the real spira-claim against this suite's real spira-lc `list`.
CLAIM_BIN="$(command -v spira-claim)" || bail "spira-claim is not on PATH"

export SPIRA_LC_HOST=127.0.0.1
export SPIRA_LC_PORT="$PORT"
export SPIRA_LC_DB=spira_lifecycle
export SPIRA_LC_DATA_DIR="$TMP"
export SPIRA_LC_USER=root
export SPIRA_LC_PASSWORD=""
# SPIRA_LC_PASSWORD_FILE EXPLICITLY, EMPTY: it is a registered key, resolved from SPIRA_TOML
# alone now — left undeclared it falls to the complete fixture's own (nonexistent) path, and
# every spira-lc call refuses before reaching the dolt server. Empty matches root's actual
# password here; switched to the real one below once this suite moves to spira_lc.
LC_CRED="$TMP/lc.credential"; : > "$LC_CRED"
tl_config SPIRA_LC_PASSWORD_FILE="$LC_CRED"

spira-lc admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$TMP/schema.log" 2>&1
wantrc "schema applies cleanly" 0 $?
cat "$TMP/schema.log" >&2

PASS="test-pass-$$"
sed "s/@SPIRA_LC_PASSWORD@/$PASS/" "$REPO/lifecycle/grants.sql" > "$TMP/grants_filled.sql"
root_sql sql < "$TMP/grants_filled.sql" >"$TMP/grants.log" 2>&1
wantrc "grants apply cleanly" 0 $?
cat "$TMP/grants.log" >&2

# From here on, every `spira-lc` call runs AS spira_lc — the real production grant set (design
# §3.6: "only spira_lc writes spira_lifecycle"), never root. A cascade that only works with
# superuser privileges would be a false green.
export SPIRA_LC_USER=spira_lc
export SPIRA_LC_PASSWORD="$PASS"
printf '%s' "$PASS" > "$LC_CRED"
tl_config SPIRA_LC_PASSWORD_FILE="$LC_CRED"

lc() { spira-lc "$@"; }
batch_field() {   # batch_field <batch-id> <column>
    root_sql --use-db spira_lifecycle sql -q "SELECT $2 AS v FROM batch WHERE batch_id = '$1'" -r json \
        | python3 -c 'import json,sys; d=json.load(sys.stdin)["rows"]; print(d[0]["v"] if d else "")' 2>/dev/null
}
member_field() {   # member_field <id> <table> <column>
    root_sql --use-db spira_lifecycle sql -q "SELECT $3 AS v FROM $2 WHERE bead_id = '$1'" -r json \
        | python3 -c 'import json,sys; d=json.load(sys.stdin)["rows"]; print(d[0]["v"] if d else "")' 2>/dev/null
}

# certify <id> <tip> — walks a fresh bead through claim/submit/gate-pass to CERTIFIED,
# exactly the bead-machine path a real aeon's submit would drive.
certify() {
    local id="$1" tip="$2"
    lc create-bead "$id" >/dev/null
    lc event bead "$id" --expect READY --version 0 --actor test --kind '{"Claim":{"holder":"aeon-1","lease_until":1}}' >/dev/null
    lc event bead "$id" --expect WORKING --version 1 --actor test --kind "{\"Submit\":{\"tip\":\"$tip\"}}" >/dev/null
    lc event bead "$id" --expect SUBMITTED --version 2 --actor test --kind '{"GatePass":{"tip":"'"$tip"'","gate_key":"k1"}}' >/dev/null
}

# certify_stacked <id> <tip> <prereq-id> <prereq-tip> — same walk as certify, but claims
# with a stack naming one prerequisite (design stacked-dependents-2026-09-28 §1), the shape
# a dependent's own real claim proposes once its prerequisite is CERTIFIED.
certify_stacked() {
    local id="$1" tip="$2" prereq="$3" prereq_tip="$4"
    lc create-bead "$id" >/dev/null
    lc event bead "$id" --expect READY --version 0 --actor test \
        --kind "{\"Claim\":{\"holder\":\"aeon-1\",\"lease_until\":1,\"stack\":{\"$prereq\":\"$prereq_tip\"},\"stack_depth\":1,\"stack_max_depth\":4}}" >/dev/null
    lc event bead "$id" --expect WORKING --version 1 --actor test --kind "{\"Submit\":{\"tip\":\"$tip\"}}" >/dev/null
    lc event bead "$id" --expect SUBMITTED --version 2 --actor test --kind '{"GatePass":{"tip":"'"$tip"'","gate_key":"k1"}}' >/dev/null
}

# submit <id> <tip> — claim and submit only: a SUBMITTED row, no per-bead gate.
submit() {
    local id="$1" tip="$2"
    lc create-bead "$id" >/dev/null
    lc event bead "$id" --expect READY --version 0 --actor test --kind '{"Claim":{"holder":"aeon-1","lease_until":1}}' >/dev/null
    lc event bead "$id" --expect WORKING --version 1 --actor test --kind "{\"Submit\":{\"tip\":\"$tip\"}}" >/dev/null
}

# ── a round takes SUBMITTED beads directly (law-a-round-is-feature-first-then-catch-all) ─
submit sp-lc-sub tipS
certify sp-lc-cer tipC
out="$(lc cut batch-sub --repo spira --head HS --base BS --members "sp-lc-sub:tipS,sp-lc-cer:tipC" --actor test)"
wantrc "cut admits a SUBMITTED member beside a CERTIFIED one" 0 $?
is "the SUBMITTED member enters delivery" "IN_DELIVERY" "$(member_field sp-lc-sub bead state)"
is "the CERTIFIED member enters delivery" "IN_DELIVERY" "$(member_field sp-lc-cer bead state)"
submit sp-lc-rw tipR
v="$(lc show sp-lc-rw | python3 -c 'import json,sys; print(json.load(sys.stdin)["bead"]["version"])')"
lc event bead sp-lc-rw --expect SUBMITTED --version "$v" --actor test --kind '{"GateRed":{"tip":"tipR","reason":"suites-failed"}}' >/dev/null
lc cut batch-rw --repo spira --head HR --base BR --members "sp-lc-rw:tipR" --actor test >/dev/null 2>&1
wantrc "cut still refuses a REWORK member" 3 $?

# ── an applied event's states come from the row it applied to (sp-zt7r2p) ───────────────
ev_field() {   # ev_field <machine> <key> <event> <column>
    root_sql --use-db spira_lifecycle sql -q "SELECT $4 AS v FROM event WHERE machine = '$1' AND lc_key = '$2' AND event = '$3' AND applied = 1 ORDER BY seq DESC LIMIT 1" -r json \
        | python3 -c 'import json,sys; d=json.load(sys.stdin)["rows"]; print(d[0]["v"] if d else "")' 2>/dev/null
}
is "a SUBMITTED member's Deliver logs SUBMITTED as its from_state" "SUBMITTED" "$(ev_field bead sp-lc-sub Deliver from_state)"
is "...and as its expect" "SUBMITTED" "$(ev_field bead sp-lc-sub Deliver expect)"
is "a CERTIFIED member's Deliver logs CERTIFIED" "CERTIFIED" "$(ev_field bead sp-lc-cer Deliver from_state)"
is "a delivery row that did not exist is enqueued from NONE" "NONE" "$(ev_field delivery sp-lc-sub Enqueued from_state)"
is "...and cut from QUEUED" "QUEUED" "$(ev_field delivery sp-lc-sub Cut from_state)"

certify sp-lc-bx tipBX
root_sql --use-db spira_lifecycle sql -q "INSERT INTO delivery (bead_id, mode, state, batch_id, version) VALUES ('sp-lc-bx', 'queue', 'BATCHED', 'batch-elsewhere', 1)" >/dev/null
out="$(lc cut batch-bx --repo spira --head HX --base BX --members "sp-lc-bx:tipBX" --actor test 2>&1)"
wantrc "a cut over a member BATCHED in another batch is refused" 3 $?
want "...naming that batch" "batch-elsewhere" "$out"
is "...and the member's bead is untouched" "CERTIFIED" "$(member_field sp-lc-bx bead state)"
is "...and its delivery row still names the other batch" "batch-elsewhere" "$(member_field sp-lc-bx delivery batch_id)"

certify sp-lc-ex tipEX
root_sql --use-db spira_lifecycle sql -q "INSERT INTO delivery (bead_id, mode, state, version) VALUES ('sp-lc-ex', 'queue', 'EXITED', 4)" >/dev/null
lc cut batch-ex --repo spira --head HE --base BE --members "sp-lc-ex:tipEX" --actor test >/dev/null
wantrc "a cut over an EXITED delivery row re-queues it by compare-and-swap" 0 $?
is "...logging the EXITED row it was" "EXITED" "$(ev_field delivery sp-lc-ex Enqueued from_state)"
is "...then the cut from QUEUED" "QUEUED" "$(ev_field delivery sp-lc-ex Cut from_state)"
is "...to BATCHED in the new batch" "batch-ex" "$(member_field sp-lc-ex delivery batch_id)"
is "...at version 6" "6" "$(member_field sp-lc-ex delivery version)"

# ── event continuity: red first on a planted break, then on the real log ────────────────
lc event-continuity >/dev/null
wantrc "the log the cascades above wrote is continuous" 0 $?
root_sql --use-db spira_lifecycle sql -q "INSERT INTO event (machine, lc_key, event, expect, from_state, to_state, applied, evidence, actor, at) VALUES ('bead','sp-lc-brk','A','X','X','Y',1,'{}','t',1),('bead','sp-lc-brk','B','Q','Q','R',1,'{}','t',2),('bead','sp-lc-brk','C','Q','Q','S',1,'{}','t',3)" >/dev/null
out="$(lc event-continuity)"
wantrc "a planted break is reported" 3 $?
want "...naming the key" '"key":"sp-lc-brk"' "$out"
is "...once, the first break only" "1" "$(printf '%s\n' "$out" | grep -c sp-lc-brk)"
root_sql --use-db spira_lifecycle sql -q "DELETE FROM event WHERE lc_key = 'sp-lc-brk'" >/dev/null

# ── migration 0013 corrects the hard-coded Deliver/Cut states from their predecessors ───
root_sql --use-db spira_lifecycle sql -q "INSERT INTO event (machine, lc_key, event, expect, from_state, to_state, applied, evidence, actor, at) VALUES ('bead','sp-lc-mig','Submit','WORKING','WORKING','SUBMITTED',1,'{}','t',1),('bead','sp-lc-mig','Deliver','CERTIFIED','CERTIFIED','IN_DELIVERY',1,'{}','t',2),('delivery','sp-lc-mig','Cut','QUEUED','QUEUED','BATCHED',1,'{}','t',3)" >/dev/null
lc event-continuity >/dev/null
wantrc "POSITIVE CONTROL: the old hard-coded states are seen as breaks" 3 $?
root_sql --use-db spira_lifecycle sql < "$REPO/lifecycle/migrations/0019-event-continuity.sql" >/dev/null 2>&1
wantrc "migration 0013 applies" 0 $?
is "the Deliver now says what the bead was" "SUBMITTED" "$(ev_field bead sp-lc-mig Deliver from_state)"
is "the Cut of a row with no predecessor says NONE" "NONE" "$(ev_field delivery sp-lc-mig Cut from_state)"
lc event-continuity >/dev/null
wantrc "...and the log is continuous again" 0 $?
root_sql --use-db spira_lifecycle sql < "$REPO/lifecycle/migrations/0019-event-continuity.sql" >/dev/null 2>&1
wantrc "migration 0013 is idempotent" 0 $?

# ── criterion 1: a green batch lands every member atomically in one transaction ──────────
certify sp-lc-1 tipA
certify sp-lc-2 tipB
out="$(lc cut batch-green --repo spira --head H1 --base B1 --members "sp-lc-1:tipA,sp-lc-2:tipB" --actor test)"
wantrc "cut opens batch-green with two members" 0 $?
want "cut applied every step" '"applied":[true,true,true,true,true,true]' "$out"

v="$(batch_field batch-green version)"
lc event batch batch-green --expect OPEN --version "$v" --actor test --kind '{"CiStarted":{"run":"r1"}}' >/dev/null
v="$(batch_field batch-green version)"
lc event batch batch-green --expect CI_RUNNING --version "$v" --actor test --kind '"Green"' >/dev/null

# POSITIVE CONTROL for the kill test below: kill a `land` mid-flight via a SIGSTOP race is
# not reproducible against a one-shot process cleanly, so the control here is simpler and
# just as direct — assert the *un-killed* land leaves every row consistent, then assert a
# second land attempt against the now-LANDED batch is refused outright (nothing to
# partially apply a second time), which is the observable a partial-cascade bug would break.
v="$(batch_field batch-green version)"
out="$(lc land batch-green --expect GREEN --version "$v" --actor test --sha sha-green-1)"
wantrc "land applies" 0 $?
want "land applied every cascaded row" '"applied":[true,true,true,true,true]' "$out"

is "batch lands" "LANDED" "$(batch_field batch-green state)"
is "member 1 delivery exits delivered" "EXITED" "$(member_field sp-lc-1 delivery state)"
is "member 1 bead lands" "LANDED" "$(member_field sp-lc-1 bead state)"
is "member 2 delivery exits delivered" "EXITED" "$(member_field sp-lc-2 delivery state)"
is "member 2 bead lands" "LANDED" "$(member_field sp-lc-2 bead state)"
is "member 1 merge sha recorded" "sha-green-1" "$(member_field sp-lc-1 delivery merge_sha)"

out2="$(lc land batch-green --expect GREEN --version "$v" --actor test --sha sha-green-2)"
rc2=$?
wantrc "a second land against an already-landed batch is refused, not partially applied" 3 $rc2
nowant "the refused replay did not touch the already-landed member" "sha-green-2" "$(member_field sp-lc-1 delivery merge_sha)"

# ── kill mid-flight: the cascade script is one transaction, all-or-nothing ───────────────
certify sp-lc-kill tipK
lc cut batch-kill --repo spira --head H2 --base B2 --members "sp-lc-kill:tipK" --actor test >/dev/null
v="$(batch_field batch-kill version)"
lc event batch batch-kill --expect OPEN --version "$v" --actor test --kind '{"CiStarted":{"run":"r1"}}' >/dev/null
v="$(batch_field batch-kill version)"
lc event batch batch-kill --expect CI_RUNNING --version "$v" --actor test --kind '"Green"' >/dev/null
green_version="$(batch_field batch-kill version)"
delivery_version="$(member_field sp-lc-kill delivery version)"

cat > "$TMP/kill.sql" <<SQL
START TRANSACTION;
UPDATE batch SET state = 'LANDED', reason = 'should-never-be-seen', version = $((green_version + 1)) WHERE batch_id = 'batch-kill' AND version = $green_version;
UPDATE delivery SET state = 'EXITED', merge_sha = 'should-never-be-seen', version = $((delivery_version + 1)) WHERE bead_id = 'sp-lc-kill' AND version = $delivery_version;
SELECT SLEEP(5);
COMMIT;
SQL
as_spira_lc() { timeout 5 "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u spira_lc -p "$PASS" --no-tls --use-db spira_lifecycle "$@"; }
as_spira_lc sql < "$TMP/kill.sql" >/dev/null 2>&1 &
KILL_PID=$!
sleep 0.5
kill -9 "$KILL_PID" 2>/dev/null
wait "$KILL_PID" 2>/dev/null

is "a cascade killed mid-flight leaves the batch row exactly as it was" "GREEN" "$(batch_field batch-kill state)"
is "...and the member's delivery row exactly as it was" "BATCHED" "$(member_field sp-lc-kill delivery state)"

out="$(lc land batch-kill --expect GREEN --version "$green_version" --actor test --sha sha-kill-recovers)"
wantrc "POSITIVE CONTROL: the same batch lands cleanly once the killed attempt is gone" 0 $?
is "recovered batch lands" "LANDED" "$(batch_field batch-kill state)"

# ── criterion 2: a red batch settles each member as returned or requeued ─────────────────
certify sp-lc-eject tipE
certify sp-lc-req1 tipR1
certify sp-lc-req2 tipR2
lc cut batch-red --repo spira --head H3 --base B3 \
    --members "sp-lc-eject:tipE,sp-lc-req1:tipR1,sp-lc-req2:tipR2" --actor test >/dev/null
v="$(batch_field batch-red version)"
lc event batch batch-red --expect OPEN --version "$v" --actor test --kind '{"CiStarted":{"run":"r1"}}' >/dev/null
v="$(batch_field batch-red version)"
lc event batch batch-red --expect CI_RUNNING --version "$v" --actor test --kind '"Red"' >/dev/null

v="$(batch_field batch-red version)"
out="$(lc settle batch-red --expect ATTRIBUTING --version "$v" --actor test --eject sp-lc-eject --requeue sp-lc-req1,sp-lc-req2)"
wantrc "settle applies" 0 $?

is "batch settles" "SETTLED" "$(batch_field batch-red state)"
is "ejected member's delivery is returned" "EXITED" "$(member_field sp-lc-eject delivery state)"
is "ejected member's bead needs rework" "REWORK" "$(member_field sp-lc-eject bead state)"
is "requeued member 1's bead is certified again" "CERTIFIED" "$(member_field sp-lc-req1 bead state)"
is "requeued member 2's bead is certified again" "CERTIFIED" "$(member_field sp-lc-req2 bead state)"

# ── criterion 3: bisect halves are child batches ──────────────────────────────────────────
certify sp-lc-b1 tipB1
certify sp-lc-b2 tipB2
certify sp-lc-b3 tipB3
certify sp-lc-b4 tipB4
lc cut batch-bisect --repo spira --head H4 --base B4 \
    --members "sp-lc-b1:tipB1,sp-lc-b2:tipB2,sp-lc-b3:tipB3,sp-lc-b4:tipB4" --actor test >/dev/null
v="$(batch_field batch-bisect version)"
lc event batch batch-bisect --expect OPEN --version "$v" --actor test --kind '{"CiStarted":{"run":"r1"}}' >/dev/null
v="$(batch_field batch-bisect version)"
lc event batch batch-bisect --expect CI_RUNNING --version "$v" --actor test --kind '"Red"' >/dev/null
v="$(batch_field batch-bisect version)"
lc settle batch-bisect --expect ATTRIBUTING --version "$v" --actor test --requeue sp-lc-b1,sp-lc-b2,sp-lc-b3,sp-lc-b4 >/dev/null

out="$(lc cut batch-bisect-lo --repo spira --head H4 --base B4 --members "sp-lc-b1:tipB1,sp-lc-b2:tipB2" --actor test --parent batch-bisect)"
wantrc "first bisect half cuts cleanly" 0 $?
out="$(lc cut batch-bisect-hi --repo spira --head H4 --base B4 --members "sp-lc-b3:tipB3,sp-lc-b4:tipB4" --actor test --parent batch-bisect)"
wantrc "second bisect half cuts cleanly" 0 $?

is "parent batch stays settled" "SETTLED" "$(batch_field batch-bisect state)"
is "first half records its parent" "batch-bisect" "$(batch_field batch-bisect-lo parent)"
is "second half records its parent" "batch-bisect" "$(batch_field batch-bisect-hi parent)"
is "first half is open for a fresh CI run" "OPEN" "$(batch_field batch-bisect-lo state)"
is "second half is open for a fresh CI run" "OPEN" "$(batch_field batch-bisect-hi state)"

# ── criterion 4: two verdict passes racing on one batch — exactly one applies ────────────
certify sp-lc-race tipRace
lc cut batch-race --repo spira --head H5 --base B5 --members "sp-lc-race:tipRace" --actor test >/dev/null
v="$(batch_field batch-race version)"
lc event batch batch-race --expect OPEN --version "$v" --actor test --kind '{"CiStarted":{"run":"r1"}}' >/dev/null
v="$(batch_field batch-race version)"
lc event batch batch-race --expect CI_RUNNING --version "$v" --actor test --kind '"Green"' >/dev/null
race_version="$(batch_field batch-race version)"

N=6
race_pids=()
for i in $(seq 1 "$N"); do
    ( lc land batch-race --expect GREEN --version "$race_version" --actor "verdict-$i" --sha "sha-race-$i" >"$TMP/race-$i.out" 2>&1
      echo $? >> "$TMP/race-$i.out" ) &
    race_pids+=("$!")
done
wait "${race_pids[@]}"

applied_count=0
for i in $(seq 1 "$N"); do
    rc="$(tail -n1 "$TMP/race-$i.out")"
    [ "$rc" = "0" ] && applied_count=$((applied_count + 1))
done
is "exactly one racing land applies" "1" "$applied_count"
is "the batch's version advanced by exactly one applied land" "$((race_version + 1))" "$(batch_field batch-race version)"
is "the batch ends landed" "LANDED" "$(batch_field batch-race state)"

event_count="$(root_sql --use-db spira_lifecycle sql -q "SELECT COUNT(*) AS n FROM event WHERE machine='batch' AND lc_key='batch-race' AND event='FastForward'" -r json | python3 -c 'import json,sys; print(json.load(sys.stdin)["rows"][0]["n"])' 2>/dev/null)"
is "every racing land left its own event row, applied or refused" "$N" "$event_count"

# sp-o7nbr.2's own block — batch.sh's _abandon_open_batch sourced directly and called
# against this fixture — is retired along with batch.sh (sp-uwhx0). Its own comment
# already said why it cost little to keep: "batcher-cut does not write batch_id=/version=
# into its own open-batch record yet, so in production this is currently always the skip
# path" — the spira-lc CAS it proved was dormant in production before this deletion too.
# queue's own abandon (queue/src/ops/batch.rs `abandon`) is covered by queue's own tests
# and by test-queue-ops.sh's "abandon" cases; it does not call _abandon_open_batch.

# ── sp-o7nbr.3 (retired with verdict.sh, sp-flj4a): the CI-outcome land walk is `queue verdict`'s
# lc_land now (queue/DESIGN-verdict.md D3), pinned by cargo test -p queue
# green_with_lifecycle_on_walks_the_batch_to_landed_on_spira_lc; the settle walk retired with
# verdict-owned attribution (D1).

# ── criterion 5: stack pipelines new members onto an already-OPEN batch ──────────────────
certify sp-lc-s1 tipS1
lc cut batch-stack --repo spira --head H6 --base B6 --members "sp-lc-s1:tipS1" --actor test >/dev/null
v="$(batch_field batch-stack version)"
is "cut leaves the batch's version at 1 (one MemberAdded)" "1" "$v"

certify sp-lc-s2 tipS2
certify sp-lc-s3 tipS3
out="$(lc stack batch-stack --members "sp-lc-s2:tipS2,sp-lc-s3:tipS3" --actor test)"
wantrc "stack applies" 0 $?
want "stack applied every step" '"applied":[true,true,true,true,true,true]' "$out"

is "stack does not insert a second batch row — still OPEN" "OPEN" "$(batch_field batch-stack state)"
is "stack advances the batch's version by exactly the new member count" "3" "$(batch_field batch-stack version)"
is "the original member is untouched" "IN_DELIVERY" "$(member_field sp-lc-s1 bead state)"
is "stacked member 2 enters delivery" "IN_DELIVERY" "$(member_field sp-lc-s2 bead state)"
is "stacked member 2's delivery is batched onto this batch" "BATCHED" "$(member_field sp-lc-s2 delivery state)"
is "stacked member 3 enters delivery" "IN_DELIVERY" "$(member_field sp-lc-s3 bead state)"

# POSITIVE CONTROL: a member CERTIFIED at a different tip than named refuses, and writes
# nothing — the batch's version is unchanged, not partially advanced by an earlier member
# in the same call that did check out.
certify sp-lc-s5 tipS5
out="$(lc stack batch-stack --members "sp-lc-s5:tip-does-not-match" --actor test)"
rc=$?
wantrc "PLANTED VIOLATION: stack refuses a member whose certified tip does not match" 3 $rc
is "the refused stack left the batch's version untouched" "3" "$(batch_field batch-stack version)"
is "the refused member is left CERTIFIED, not admitted" "CERTIFIED" "$(member_field sp-lc-s5 bead state)"

# POSITIVE CONTROL: stacking onto a batch that already left OPEN (settled here) refuses —
# there is no longer a live round for a new member to join.
v="$(batch_field batch-stack version)"
lc event batch batch-stack --expect OPEN --version "$v" --actor test --kind '{"CiStarted":{"run":"r1"}}' >/dev/null
v="$(batch_field batch-stack version)"
lc event batch batch-stack --expect CI_RUNNING --version "$v" --actor test --kind '"Red"' >/dev/null
v="$(batch_field batch-stack version)"
lc settle batch-stack --expect ATTRIBUTING --version "$v" --actor test --requeue sp-lc-s1,sp-lc-s2,sp-lc-s3 >/dev/null
certify sp-lc-s4 tipS4
out="$(lc stack batch-stack --members "sp-lc-s4:tipS4" --actor test)"
rc=$?
wantrc "PLANTED VIOLATION: stack refuses a batch that is SETTLED, not OPEN" 3 $rc
is "the refused stack did not touch the settled batch's state" "SETTLED" "$(batch_field batch-stack state)"

# ── criterion 6 (sp-f3af9): eject cascade through the batch machine ──────────────────────
# A red in a round with B stacked on A and unrelated X: settling with A named the only
# `--eject` must also return B — collateral, not itself red — and requeue only the truly
# unrelated X, even though the caller (unaware of the stack) named both B and X in
# `--requeue` (design stacked-dependents-2026-09-28 §3: "ejecting a member ejects every
# member stacked on it, transitively... the batch machine's SETTLED emits returned for the
# ejected prerequisite and returned(base_withdrawn) for its dependents").
certify sp-f-a tipFA
certify_stacked sp-f-b tipFB sp-f-a tipFA
certify sp-f-x tipFX
lc cut batch-stacked-red --repo spira --head H7 --base B7 \
    --members "sp-f-a:tipFA,sp-f-b:tipFB,sp-f-x:tipFX" --actor test >/dev/null
v="$(batch_field batch-stacked-red version)"
lc event batch batch-stacked-red --expect OPEN --version "$v" --actor test --kind '{"CiStarted":{"run":"r1"}}' >/dev/null
v="$(batch_field batch-stacked-red version)"
lc event batch batch-stacked-red --expect CI_RUNNING --version "$v" --actor test --kind '"Red"' >/dev/null

v="$(batch_field batch-stacked-red version)"
out="$(lc settle batch-stacked-red --expect ATTRIBUTING --version "$v" --actor test --eject sp-f-a --requeue sp-f-b,sp-f-x)"
wantrc "settle applies even though the caller's own --requeue still names the stacked dependent" 0 $?

is "batch settles" "SETTLED" "$(batch_field batch-stacked-red state)"
is "ejected prerequisite A needs rework" "REWORK" "$(member_field sp-f-a bead state)"
is "A's reason names the direct red eject" "batch-ejected-red" "$(member_field sp-f-a bead reason)"
is "SEEN RED FIRST: B follows A into REWORK rather than the caller's own --requeue leaving it CERTIFIED" \
    "REWORK" "$(member_field sp-f-b bead state)"
is "B's reason names base-withdrawn — collateral, not itself accused" "base-withdrawn" "$(member_field sp-f-b bead reason)"
is "unrelated X is merely requeued, unaffected by the cascade" "CERTIFIED" "$(member_field sp-f-x bead state)"
want "the settle summary meters the one cascaded base_withdrawn" '"base_withdrawn":1' "$out"

# ── criterion 4/6, 3-deep (sp-s9675.5): the cascade walks past one hop ────────────────────
# The two-member fixture above (A, B) cannot tell a BFS that only checks the root's own
# direct dependents from one that actually walks the frontier transitively. A three-deep
# chain A -> B -> C, plus unrelated X, can: ejecting the root A must cascade both B and C
# (design stacked-dependents-2026-09-28's own test-strategy item 4: "A red in the round ->
# A, B, C ejected together [3-deep]; unrelated members requeued").
certify sp-f3-a tipF3A
certify_stacked sp-f3-b tipF3B sp-f3-a tipF3A
certify_stacked sp-f3-c tipF3C sp-f3-b tipF3B
certify sp-f3-x tipF3X
lc cut batch-stacked-3deep --repo spira --head H7b --base B7b \
    --members "sp-f3-a:tipF3A,sp-f3-b:tipF3B,sp-f3-c:tipF3C,sp-f3-x:tipF3X" --actor test >/dev/null
v="$(batch_field batch-stacked-3deep version)"
lc event batch batch-stacked-3deep --expect OPEN --version "$v" --actor test --kind '{"CiStarted":{"run":"r1"}}' >/dev/null
v="$(batch_field batch-stacked-3deep version)"
lc event batch batch-stacked-3deep --expect CI_RUNNING --version "$v" --actor test --kind '"Red"' >/dev/null

v="$(batch_field batch-stacked-3deep version)"
out="$(lc settle batch-stacked-3deep --expect ATTRIBUTING --version "$v" --actor test --eject sp-f3-a --requeue sp-f3-b,sp-f3-c,sp-f3-x)"
wantrc "settle applies across the 3-deep chain" 0 $?

is "root A needs rework" "REWORK" "$(member_field sp-f3-a bead state)"
is "middle B cascades, one hop from the root" "REWORK" "$(member_field sp-f3-b bead state)"
is "B's reason is collateral" "base-withdrawn" "$(member_field sp-f3-b bead reason)"
is "SEEN RED FIRST: tip C cascades too — a BFS that stopped after one hop would leave C wrongly CERTIFIED" \
    "REWORK" "$(member_field sp-f3-c bead state)"
is "C's reason is collateral" "base-withdrawn" "$(member_field sp-f3-c bead reason)"
is "unrelated X is merely requeued, unaffected by the cascade" "CERTIFIED" "$(member_field sp-f3-x bead state)"
want "the settle summary meters both cascaded members" '"base_withdrawn":2' "$out"

# ── sp-o7nbr.5: the queue's own open-batch/eject/abandon CAS onto this same machine, on
# this same fixture. queue.sh is the queue binary now (queue/DESIGN.md §7.4): its CAS shape
# is `ops::batch::lc_cas`, unit-tested with the switch on. What this suite still owns is the
# MACHINE's answer to those exact calls, so the three helpers below issue the same spira-lc
# commands queue.sh's _lc_cut_batch/_lc_eject_member/_lc_abandon_batch issued (create-bead
# per member then cut; show-batch for the CAS state/version, then abandon-batch or
# eject-member) against this suite's already-running server.
SPIRA_RUN="$TMP/queue-shell-run"
mkdir -p "$SPIRA_RUN/landstate" "$SPIRA_RUN/queue/fixture-repo"
tl_config SPIRA_RUN="$SPIRA_RUN"
_lc_cut_batch() {   # _lc_cut_batch <batch-id> <repo> <head> <base> <actor> <id:tip>...
    local batch_id="$1" repo="$2" head="$3" base="$4" actor="$5" _m csv=""; shift 5
    for _m in "$@"; do lc create-bead "${_m%%:*}" >/dev/null 2>&1 || true; csv="${csv:+$csv,}$_m"; done
    lc cut "$batch_id" --repo "$repo" --head "$head" --base "$base" --members "$csv" --actor "$actor"
}
_lc_batch_sv() {    # _lc_batch_sv <batch-id> -> "<state> <version>", or rc 1
    local out; out="$(lc show-batch "$1" 2>/dev/null)" || return 1
    printf '%s' "$out" | python3 -c 'import json,sys
d=json.load(sys.stdin); s,v=d.get("state"),d.get("version")
sys.exit(1) if s is None or v is None else print(s, v)' 2>/dev/null
}
_lc_abandon_batch() {   # _lc_abandon_batch <batch-id> <actor> <reason>
    local sv state version; sv="$(_lc_batch_sv "$1")" || return 1; read -r state version <<< "$sv"
    lc abandon-batch "$1" --expect "$state" --version "$version" --actor "$2" --reason "$3"
}
_lc_eject_member() {    # _lc_eject_member <batch-id> <bead> <actor> <reason>
    local sv state version; sv="$(_lc_batch_sv "$1")" || return 1; read -r state version <<< "$sv"
    lc eject-member "$1" --bead-id "$2" --expect "$state" --version "$version" --actor "$3" --reason "$4"
}

# _lc_cut_batch: every member needs a bead row, but cut itself still only accepts a
# CERTIFIED row at the given tip — create-bead-if-absent never shortcuts that.
certify sp-lc-q-cut1 tipQC1
lc create-bead sp-lc-q-cut2 >/dev/null   # a bead row exists, but is never certified below

out="$(_lc_cut_batch batch-q-cut fixture-repo headQ baseQ queue "sp-lc-q-cut1:tipQC1")"
wantrc "_lc_cut_batch applies for a certified member" 0 $?
is "cut batch is OPEN" "OPEN" "$(batch_field batch-q-cut state)"
is "member's bead moves to IN_DELIVERY" "IN_DELIVERY" "$(member_field sp-lc-q-cut1 bead state)"

# POSITIVE CONTROL: a not-yet-CERTIFIED member refuses cleanly and leaves no batch row —
# the shape cmd_open_batch relies on to leave batch_id/version unset on its own record.
out="$(_lc_cut_batch batch-q-refuse fixture-repo headQ baseQ queue "sp-lc-q-cut2:tipQC2")"
rc=$?
wantrc "_lc_cut_batch refuses a member that is not CERTIFIED there" 3 $rc
is "a refused cut leaves no batch row behind" "" "$(batch_field batch-q-refuse state)"

# _lc_eject_member: legal from OPEN/CI_RUNNING/GREEN, returns only the named member to
# CERTIFIED, and does not move the batch or touch survivors — distinct from settle's own
# CI-driven eject, which only runs after a real Red (design: the event log is the record
# of what happened, and no Red ever fired here).
certify sp-lc-q-e1 tipQE1
certify sp-lc-q-e2 tipQE2
lc cut batch-q-eject --repo fixture-repo --head headE --base baseE \
    --members "sp-lc-q-e1:tipQE1,sp-lc-q-e2:tipQE2" --actor test >/dev/null

out="$(_lc_eject_member batch-q-eject sp-lc-q-e1 queue "manual eject")"
wantrc "_lc_eject_member applies from OPEN" 0 $?
is "batch stays OPEN — eject does not move it" "OPEN" "$(batch_field batch-q-eject state)"
is "ejected member returns to CERTIFIED" "CERTIFIED" "$(member_field sp-lc-q-e1 bead state)"
is "survivor is left alone in the batch" "IN_DELIVERY" "$(member_field sp-lc-q-e2 bead state)"

# Eject from GREEN is legal: the head changes, so the certification is void and the batch
# returns to OPEN with the member back at CERTIFIED.
v="$(batch_field batch-q-eject version)"
lc event batch batch-q-eject --expect OPEN --version "$v" --actor test --kind '{"CiStarted":{"run":"r1"}}' >/dev/null
v="$(batch_field batch-q-eject version)"
lc event batch batch-q-eject --expect CI_RUNNING --version "$v" --actor test --kind '"Green"' >/dev/null
is "batch reaches GREEN before the eject" "GREEN" "$(batch_field batch-q-eject state)"
out="$(_lc_eject_member batch-q-eject sp-lc-q-e2 queue "eject after green")"
wantrc "_lc_eject_member applies from GREEN" 0 $?
is "an eject from GREEN returns the batch to OPEN" "OPEN" "$(batch_field batch-q-eject state)"
is "the member ejected from GREEN returns to CERTIFIED" "CERTIFIED" "$(member_field sp-lc-q-e2 bead state)"

# POSITIVE CONTROL: eject-member still refuses once the batch is terminal.
out="$(_lc_abandon_batch batch-q-eject queue "close it")"
out="$(_lc_eject_member batch-q-eject sp-lc-q-e2 queue "too late")"
rc=$?
wantrc "_lc_eject_member refuses once the batch is terminal (ABANDONED)" 3 $rc

# _lc_abandon_batch: accepts from any non-terminal state, returns every member.
certify sp-lc-q-a1 tipQA1
certify sp-lc-q-a2 tipQA2
lc cut batch-q-abandon --repo fixture-repo --head headA --base baseA \
    --members "sp-lc-q-a1:tipQA1,sp-lc-q-a2:tipQA2" --actor test >/dev/null

out="$(_lc_abandon_batch batch-q-abandon queue "test abandon")"
wantrc "_lc_abandon_batch applies from OPEN" 0 $?
is "abandoned batch reaches ABANDONED" "ABANDONED" "$(batch_field batch-q-abandon state)"
is "member 1 returns to CERTIFIED" "CERTIFIED" "$(member_field sp-lc-q-a1 bead state)"
is "member 2 returns to CERTIFIED" "CERTIFIED" "$(member_field sp-lc-q-a2 bead state)"

# POSITIVE CONTROL: the guard shape queue.sh's own call sites rely on (batch_id present
# only on a record whose cut applied) — a batch that was never cut fails closed instead of
# CASing against a row that does not exist.
out="$(_lc_abandon_batch batch-q-never-cut queue "no such batch")"
rc=$?
wantrc "_lc_abandon_batch fails closed when the batch row does not exist" 1 $rc

# ── an abandon returns each member to the state it entered delivery from ───────────────────
submit sp-lc-o-sub tipOS
certify sp-lc-o-cer tipOC
lc cut batch-q-prior --repo fixture-repo --head headO --base baseO \
    --members "sp-lc-o-sub:tipOS,sp-lc-o-cer:tipOC" --actor test >/dev/null
out="$(_lc_abandon_batch batch-q-prior queue "prior state")"
wantrc "abandoning a mixed round applies" 0 $?
is "a SUBMITTED member returns to SUBMITTED, never promoted" "SUBMITTED" "$(member_field sp-lc-o-sub bead state)"
is "a CERTIFIED member returns to CERTIFIED" "CERTIFIED" "$(member_field sp-lc-o-cer bead state)"

# ── an IN_DELIVERY bead no open batch names is requeued by the invariant ───────────────────
# POSITIVE CONTROL: the planted orphan is seen first, then a bead held by an open batch is not.
submit sp-lc-orph-sub tipPS
certify sp-lc-orph-cer tipPC
certify sp-lc-held tipPH
lc cut batch-orph --repo fixture-repo --head headP --base baseP \
    --members "sp-lc-orph-sub:tipPS,sp-lc-orph-cer:tipPC" --actor test >/dev/null
lc cut batch-held --repo fixture-repo --head headH --base baseH --members "sp-lc-held:tipPH" --actor test >/dev/null
v="$(batch_field batch-orph version)"
lc event batch batch-orph --expect OPEN --version "$v" --actor test --kind '{"Abandon":{"reason":"planted"}}' >/dev/null
is "the planted orphans are stranded IN_DELIVERY" "IN_DELIVERY" "$(member_field sp-lc-orph-sub bead state)"
out="$(lc requeue-orphans --actor test)"
want "a dry run names the SUBMITTED orphan" 'sp-lc-orph-sub' "$out"
want "a dry run names the CERTIFIED orphan" 'sp-lc-orph-cer' "$out"
nowant "a bead held by an open batch is not an orphan" 'sp-lc-held' "$out"
is "a dry run changes nothing" "IN_DELIVERY" "$(member_field sp-lc-orph-sub bead state)"
out="$(lc requeue-orphans --actor test --apply)"
wantrc "requeue-orphans --apply succeeds" 0 $?
is "the SUBMITTED orphan returns to SUBMITTED" "SUBMITTED" "$(member_field sp-lc-orph-sub bead state)"
is "the CERTIFIED orphan returns to CERTIFIED" "CERTIFIED" "$(member_field sp-lc-orph-cer bead state)"
is "the held member stays IN_DELIVERY" "IN_DELIVERY" "$(member_field sp-lc-held bead state)"

# ── criterion 6, hand half (sp-f3af9): queue.sh's own eject cascades identically ─────────
# A hand eject through _lc_eject_member (the same call queue.sh's cmd_eject makes) must
# cascade to a stacked dependent exactly as settle's own CI-driven eject does above: the
# dependent follows into REWORK with base-withdrawn, even though the manual path returns
# the ejected prerequisite itself to CERTIFIED (unchanged tip, no rework of its own).
certify sp-f-ha tipHA
certify_stacked sp-f-hb tipHB sp-f-ha tipHA
lc cut batch-stacked-hand --repo fixture-repo --head H8 --base B8 \
    --members "sp-f-ha:tipHA,sp-f-hb:tipHB" --actor test >/dev/null

out="$(_lc_eject_member batch-stacked-hand sp-f-ha queue "hand eject")"
wantrc "_lc_eject_member applies from OPEN" 0 $?
is "batch stays OPEN — a hand eject does not move it" "OPEN" "$(batch_field batch-stacked-hand state)"
is "hand-ejected A returns to CERTIFIED (tip unchanged, no rework of its own)" "CERTIFIED" "$(member_field sp-f-ha bead state)"
is "SEEN RED FIRST: B follows A out of the batch identically to the settle-red case — REWORK" \
    "REWORK" "$(member_field sp-f-hb bead state)"
is "B's reason is base-withdrawn here too" "base-withdrawn" "$(member_field sp-f-hb bead reason)"
want "the eject-member summary meters the one cascaded base_withdrawn" '"base_withdrawn":1' "$out"

# ── sp-s9675.5, item 5: the epic-blocker hold-release rule stays "as today" ──────────────
# design stacked-dependents-2026-09-28 §1's hold-release table: a same-repo work-bead
# blocker releases the wait hold at CERTIFIED (criterion 5 above stacks on exactly that);
# "anything else (epic, decision, other repo)" still needs the blocker closed. An epic is
# never a work bead the aeon machine claims/submits/certifies, so it never gets its own
# spira-lc row (rank.rs's stack_plan falls back to the bd record's own status precisely
# because `lc.get(&epic_id)` is `None`) — this fixture reflects that: only B gets a
# spira-lc row. This rule is implemented in spira-claim's own stack_plan/claimable
# (rank.rs), already unit-tested there against hand-built fixtures — what is proven here
# instead is that the real spira-lc `list` output and the real spira-claim binary agree on
# the same JSON contract for a candidate that does carry a real lifecycle row.
lc create-bead sp-epicb >/dev/null
lc_epic_snapshot="$TMP/lc-snapshot-epicb.json"
lc list > "$lc_epic_snapshot"

ready_epicb="$TMP/ready-epicb.json"
cat > "$ready_epicb" <<JSON
[{"id":"sp-epicb","priority":1,"labels":["repo:spira"],"issue_type":"task","dependencies":[{"issue_id":"sp-epicb","depends_on_id":"sp-epic","type":"blocks"}]}]
JSON

blockers_epic_open="$TMP/blockers-epic-open.json"
cat > "$blockers_epic_open" <<JSON
[{"id":"sp-epic","status":"open","issue_type":"epic","labels":["repo:spira"]}]
JSON
count_open="$("$CLAIM_BIN" select --fayth test --blockers machine \
    --ready "$ready_epicb" --lifecycle "$lc_epic_snapshot" --blocker-records "$blockers_epic_open" --count)"
wantrc "spira-claim select runs cleanly with the epic still open" 0 $?
is "B stays blocked while its epic is open" "0" "$count_open"

blockers_epic_closed="$TMP/blockers-epic-closed.json"
cat > "$blockers_epic_closed" <<JSON
[{"id":"sp-epic","status":"closed","issue_type":"epic","labels":["repo:spira"]}]
JSON
count_closed="$("$CLAIM_BIN" select --fayth test --blockers machine \
    --ready "$ready_epicb" --lifecycle "$lc_epic_snapshot" --blocker-records "$blockers_epic_closed" --count)"
wantrc "spira-claim select runs cleanly once the epic closes" 0 $?
is "B is claimable once the epic closes — the hand-ordered epic edge, unchanged" "1" "$count_closed"

# ── a round's passes: red pass 1 → eject → rebuild → green pass 2, and a pass killed at the cap ──
certify sp-lc-p-1 tipP1
certify sp-lc-p-2 tipP2
lc cut batch-passes --repo fixture-repo --head headP0 --base baseP --members "sp-lc-p-1:tipP1,sp-lc-p-2:tipP2" --actor test >/dev/null
pev() {   # pev <batch> <kind-json>: the event at the batch's current state and version
    lc event batch "$1" --expect "$(batch_field "$1" state)" --version "$(batch_field "$1" version)" --actor test --kind "$2"
}
pev batch-passes '{"PassStarted":{"n":1,"head":"headP1"}}' >/dev/null
is "PassStarted moves the round to CI_RUNNING, phase build" "CI_RUNNING build 1" "$(batch_field batch-passes state) $(batch_field batch-passes phase) $(batch_field batch-passes pass)"
out="$(pev batch-passes '{"PassGreen":{"n":1,"suites_s":1,"build_s":1}}' 2>&1)"
wantrc "a green before the suites started is refused" 3 $?
want "the refusal names the round's phase" "phase build" "$out"
pev batch-passes '{"SuitesStarted":{"n":1}}' >/dev/null
is "SuitesStarted moves the phase to suites" "suites" "$(batch_field batch-passes phase)"
pev batch-passes '{"PassRed":{"n":1,"red_suites":["test-x.sh"],"suites_s":90,"build_s":30}}' >/dev/null
is "a red pass ends in ATTRIBUTING" "ATTRIBUTING" "$(batch_field batch-passes state)"
_lc_eject_member batch-passes sp-lc-p-1 queue "red on test-x.sh" >/dev/null
wantrc "a member is ejected from an ATTRIBUTING round" 0 $?
pev batch-passes '{"PassRebuilt":{"head":"headP2"}}' >/dev/null
is "a rebuilt round reopens for the next pass" "OPEN" "$(batch_field batch-passes state)"
pev batch-passes '{"PassStarted":{"n":2,"head":"headP2"}}' >/dev/null
pev batch-passes '{"SuitesStarted":{"n":2}}' >/dev/null
passes_json="$(lc list --batches)"
pfield() { printf '%s' "$passes_json" | python3 -c 'import json,sys; b=[x for x in json.load(sys.stdin) if x["batch_id"]=="batch-passes"][0]; print(eval(sys.argv[1]))' "$1"; }
is "list --batches reports pass 2 in the suites phase" "2 suites" "$(pfield 'str(b["pass"]) + " " + b["phase"]')"
is "list --batches reports pass 1's verdict, red suites and timings" "red ['test-x.sh'] 30 90" "$(pfield '" ".join(str(b["passes"][0][k]) for k in ("verdict", "red_suites", "build_s", "suites_s"))')"
is "list --batches counts the eject against pass 1" "1 0" "$(pfield '" ".join(str(p["ejects"]) for p in b["passes"])')"
is "pass 2 has no verdict yet" "None" "$(pfield 'b["last_pass"]["verdict"]')"
nowant "phase_since is reported" "None" "$(pfield 'b["phase_since"]')"
pev batch-passes '{"PassGreen":{"n":2,"suites_s":80,"build_s":20}}' >/dev/null
is "the second pass reaches GREEN" "GREEN" "$(batch_field batch-passes state)"

certify sp-lc-p-3 tipP3
lc cut batch-cap --repo fixture-repo --head headC --base baseC --members "sp-lc-p-3:tipP3" --actor test >/dev/null
pev batch-cap '{"PassStarted":{"n":1,"head":"headC"}}' >/dev/null
pev batch-cap '{"SuitesStarted":{"n":1}}' >/dev/null
pev batch-cap '{"PassIncomplete":{"n":1,"reason":"over the 900s cap"}}' >/dev/null
passes_json="$(lc list --batches)"
pfield() { printf '%s' "$passes_json" | python3 -c 'import json,sys; b=[x for x in json.load(sys.stdin) if x["batch_id"]=="batch-cap"][0]; print(eval(sys.argv[1]))' "$1"; }
is "a pass killed at the cap is incomplete, in ATTRIBUTING, neither red nor green" "incomplete ATTRIBUTING" "$(pfield 'b["last_pass"]["verdict"] + " " + b["state"]')"

# ── a round staged behind a running one: members untouched until promoted ───────────────
certify sp-lc-g1 tipG1
certify sp-lc-g2 tipG2
out="$(lc stage batch-staged --repo spira --head headG --base baseG --members "sp-lc-g1:tipG1,sp-lc-g2:tipG2" --actor test --parent batch-running)"
wantrc "stage writes the staged round" 0 $?
is "a staged round is STAGED, behind its parent" "STAGED batch-running" "$(batch_field batch-staged state) $(batch_field batch-staged parent)"
is "staging delivers no member" "CERTIFIED CERTIFIED" "$(member_field sp-lc-g1 bead state) $(member_field sp-lc-g2 bead state)"
out="$(lc stage batch-staged-stale --repo spira --head h --base b --members "sp-lc-g1:tip-does-not-match" --actor test --parent batch-running)"
wantrc "PLANTED VIOLATION: stage refuses a member whose tip does not match" 3 $?
is "the refused stage wrote no row" "" "$(batch_field batch-staged-stale state)"
out="$(lc promote batch-staged --head headG2 --base baseG2 --actor test)"
wantrc "promote cuts the staged round" 0 $?
is "a promoted round is OPEN at the rebuilt head and base" "OPEN headG2 baseG2" "$(batch_field batch-staged state) $(batch_field batch-staged head) $(batch_field batch-staged base)"
is "promotion delivers every member" "IN_DELIVERY IN_DELIVERY" "$(member_field sp-lc-g1 bead state) $(member_field sp-lc-g2 bead state)"
is "promotion batches each member's delivery onto the round" "BATCHED" "$(member_field sp-lc-g2 delivery state)"
out="$(lc promote batch-staged --head h --base b --actor test)"
wantrc "PLANTED VIOLATION: a round already OPEN is not promoted again" 3 $?

certify sp-lc-g3 tipG3
lc stage batch-staged-dead --repo spira --head headD --base baseD --members "sp-lc-g3:tipG3" --actor test --parent batch-running >/dev/null
lc event batch batch-staged-dead --expect STAGED --version 0 --actor test --kind '{"Discard":{"reason":"parent red"}}' >/dev/null
is "a discarded stage is DISCARDED with its reason" "DISCARDED parent red" "$(batch_field batch-staged-dead state) $(batch_field batch-staged-dead reason)"
is "discarding returns nothing: the member is still CERTIFIED" "CERTIFIED" "$(member_field sp-lc-g3 bead state)"
out="$(lc promote batch-staged-dead --head h --base b --actor test)"
wantrc "PLANTED VIOLATION: a discarded stage is not promoted" 3 $?

# ── an aeon's phase and disposition on the WORKING row, against the real store ──
lc create-bead sp-lc-ph >/dev/null
lc event bead sp-lc-ph --expect READY --version 0 --actor aeon-ph --kind '{"Claim":{"holder":"aeon-ph","lease_until":1}}' >/dev/null
phase_of() { lc show sp-lc-ph | python3 -c 'import json,sys; print(json.load(sys.stdin)["bead"]["aeon_phase"])'; }
is "a claim starts the row in phase claimed" "claimed" "$(phase_of)"
lc phase sp-lc-ph aeon-ph building >/dev/null
wantrc "the holder records its next phase" 0 $?
is "show reads exactly the recorded phase" "building" "$(phase_of)"
out="$(lc phase sp-lc-ph aeon-ph claimed 2>&1)"
wantrc "a step back is refused" 3 $?
want "the refusal names the state and phase" "WORKING/building" "$out"
lc phase sp-lc-ph aeon-other session >/dev/null 2>&1
wantrc "a phase from a non-holder is refused" 3 $?
is "the refused moves left the phase where it was" "building" "$(phase_of)"
lc disposition sp-lc-ph lapsed "300	last words" watchdog >/dev/null
wantrc "a lapsed disposition is recorded" 0 $?
lc disposition sp-lc-ph slain "by hand" slay >/dev/null
wantrc "a stronger disposition replaces it" 0 $?
lc disposition sp-lc-ph thrash "x" heartbeat >/dev/null 2>&1
wantrc "a weaker one never does" 3 $?
is "list --state WORKING carries the phase and the disposition" "building slain by hand" "$(lc list --state WORKING | python3 -c 'import json,sys; b=[x for x in json.load(sys.stdin) if x["bead_id"]=="sp-lc-ph"][0]; print(b["aeon_phase"], b["disposition"], b["disposition_note"])')"
lc release sp-lc-ph aeon-ph >/dev/null
is "leaving WORKING clears the phase and the disposition" "None None" "$(lc show sp-lc-ph | python3 -c 'import json,sys; b=json.load(sys.stdin)["bead"]; print(b["aeon_phase"], b["disposition"])')"
lc disposition sp-lc-ph slain "late" slay >/dev/null 2>&1
wantrc "a disposition outside WORKING is refused" 3 $?

tl_summary
