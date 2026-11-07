#!/usr/bin/env bash
#
# test-lifecycle-cutover.sh — container-tier checks that need a real `dolt sql-server`, run AS
# the spira_lc user (the real production grant set), never as root: event continuity and its
# migration, requeue-orphans, reconcile-landed, the epic-blocker hold release against the real
# spira-claim, a dwarf's phase and disposition, the dispatcher's verdicts, and the mending
# rows. The batch machine's cascades (cut, stack, stage, promote, land, settle, eject,
# abandon, a round's passes) are unit tests over the plans in spira-lc/src/cutover.rs; the
# racing land is in test-lifecycle-container.sh.
#
#   ./test-lifecycle-cutover.sh
#
# host-reason: starts its own disposable `dolt sql-server`, the same shape every
# testdb.sh server-mode suite already uses without a container call — testenv-batch.sh
# already provides the container this suite executes in.
#
# defect: sp-o7nbr
# tier: T2
# covers: lifecycle/* spira-lc/* spira-claim/* queue/src/* spira/mend-fences
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
PORT=$((3309 + 500 + (RANDOM % 500)))
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
# exactly the bead-machine path a real dwarf's submit would drive.
certify() {
    local id="$1" tip="$2"
    lc create-bead "$id" >/dev/null
    lc event bead "$id" --expect READY --version 0 --actor test --kind '{"Claim":{"holder":"dwarf-1","lease_until":1}}' >/dev/null
    lc event bead "$id" --expect WORKING --version 1 --actor test --kind "{\"Submit\":{\"tip\":\"$tip\"}}" >/dev/null
    lc event bead "$id" --expect SUBMITTED --version 2 --actor test --kind '{"GatePass":{"tip":"'"$tip"'","gate_key":"k1"}}' >/dev/null
}

# submit <id> <tip> — claim and submit only: a SUBMITTED row, no per-bead gate.
submit() {
    local id="$1" tip="$2"
    lc create-bead "$id" >/dev/null
    lc event bead "$id" --expect READY --version 0 --actor test --kind '{"Claim":{"holder":"dwarf-1","lease_until":1}}' >/dev/null
    lc event bead "$id" --expect WORKING --version 1 --actor test --kind "{\"Submit\":{\"tip\":\"$tip\"}}" >/dev/null
}

ev_field() {   # ev_field <machine> <key> <event> <column>
    root_sql --use-db spira_lifecycle sql -q "SELECT $4 AS v FROM event WHERE machine = '$1' AND lc_key = '$2' AND event = '$3' AND applied = 1 ORDER BY seq DESC LIMIT 1" -r json \
        | python3 -c 'import json,sys; d=json.load(sys.stdin)["rows"]; print(d[0]["v"] if d else "")' 2>/dev/null
}
CONT_RUN="$TMP/continuity-run"; mkdir -p "$CONT_RUN"
cont() { SPIRA_TOML="$(tl_layer SPIRA_RUN="$CONT_RUN")" spira-lc event-continuity; }
cont_ack() { SPIRA_TOML="$(tl_layer SPIRA_RUN="$CONT_RUN")" spira-lc event-continuity "$@"; }
# ── event continuity: red first on a planted break, then on the real log ────────────────
cont >/dev/null
wantrc "the log the cascades above wrote is continuous" 0 $?
root_sql --use-db spira_lifecycle sql -q "INSERT INTO event (machine, lc_key, event, expect, from_state, to_state, applied, evidence, actor, at) VALUES ('bead','sp-lc-brk','A','X','X','Y',1,'{}','t',1),('bead','sp-lc-brk','B','Q','Q','R',1,'{}','t',2),('bead','sp-lc-brk','C','Q','Q','S',1,'{}','t',3)" >/dev/null
out="$(cont)"
wantrc "a planted break is reported" 3 $?
want "...naming the key" '"key":"sp-lc-brk"' "$out"
is "...once, the first break only" "1" "$(printf '%s\n' "$out" | grep -c sp-lc-brk)"
root_sql --use-db spira_lifecycle sql -q "DELETE FROM event WHERE lc_key = 'sp-lc-brk'" >/dev/null

# ── an acknowledged break passes with its reason shown; a new one is red ────────────────
rm -f "$CONT_RUN/event-continuity.acknowledged"
root_sql --use-db spira_lifecycle sql -q "INSERT INTO event (machine, lc_key, event, expect, from_state, to_state, applied, evidence, actor, at) VALUES ('bead','sp-lc-old','A','X','X','Y',1,'{}','t',1),('bead','sp-lc-old','B','Q','Q','R',1,'{}','t',2)" >/dev/null
cont >/dev/null
wantrc "an unacknowledged old break is red (seen red first)" 3 $?
out="$(cont_ack --acknowledge --reason "historical, unrepairable" --through 999999999)"; rc=$?
wantrc "acknowledging with a reason and a watermark succeeds" 0 $rc
out="$(cont)"; rc=$?
wantrc "the acknowledged break passes" 0 $rc
want "...still reported" '"key":"sp-lc-old"' "$out"
want "...with its recorded reason" '"acknowledged":"historical, unrepairable"' "$out"
cont_ack --acknowledge --through 1 >/dev/null
wantrc "acknowledging without a reason is refused" 3 $?
root_sql --use-db spira_lifecycle sql -q "INSERT INTO event (machine, lc_key, event, expect, from_state, to_state, applied, evidence, actor, at) VALUES ('bead','sp-lc-new','A','X','X','Y',1,'{}','t',1),('bead','sp-lc-new','B','Q','Q','R',1,'{}','t',2)" >/dev/null
out="$(cont)"; rc=$?
wantrc "a new break exits 3 while the old one stays acknowledged" 3 $rc
want "...naming the new key" '"key":"sp-lc-new"' "$out"
root_sql --use-db spira_lifecycle sql -q "DELETE FROM event WHERE lc_key IN ('sp-lc-old','sp-lc-new')" >/dev/null
rm -f "$CONT_RUN/event-continuity.acknowledged"

# ── migration 0013 corrects the hard-coded Deliver/Cut states from their predecessors ───
root_sql --use-db spira_lifecycle sql -q "INSERT INTO event (machine, lc_key, event, expect, from_state, to_state, applied, evidence, actor, at) VALUES ('bead','sp-lc-mig','Submit','WORKING','WORKING','SUBMITTED',1,'{}','t',1),('bead','sp-lc-mig','Deliver','CERTIFIED','CERTIFIED','IN_DELIVERY',1,'{}','t',2),('delivery','sp-lc-mig','Cut','QUEUED','QUEUED','BATCHED',1,'{}','t',3)" >/dev/null
cont >/dev/null
wantrc "POSITIVE CONTROL: the old hard-coded states are seen as breaks" 3 $?
root_sql --use-db spira_lifecycle sql < "$REPO/lifecycle/migrations/0019-event-continuity.sql" >/dev/null 2>&1
wantrc "migration 0013 applies" 0 $?
is "the Deliver now says what the bead was" "SUBMITTED" "$(ev_field bead sp-lc-mig Deliver from_state)"
is "the Cut of a row with no predecessor says NONE" "NONE" "$(ev_field delivery sp-lc-mig Cut from_state)"
cont >/dev/null
wantrc "...and the log is continuous again" 0 $?
root_sql --use-db spira_lifecycle sql < "$REPO/lifecycle/migrations/0019-event-continuity.sql" >/dev/null 2>&1
wantrc "migration 0013 is idempotent" 0 $?

SPIRA_RUN="$TMP/queue-shell-run"
mkdir -p "$SPIRA_RUN/landstate" "$SPIRA_RUN/queue/fixture-repo"
tl_config SPIRA_RUN="$SPIRA_RUN"
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

# ── an IN_DELIVERY bead no open batch names is requeued by the invariant ───────────────────
# POSITIVE CONTROL: the planted orphan is seen first, then a bead held by an open batch is not.
submit sp-lc-orph-sub 48562c548301af8fc16ae40479bf412689bb60f5
certify sp-lc-orph-cer c604f0b918747bac3b192b7de12bd875743a5d7a
certify sp-lc-held c114c63afe9b40e5cc412a8eb1fb63cb13fa7b56
lc cut batch-orph --repo fixture-repo --head headP --base baseP \
    --members "sp-lc-orph-sub:48562c548301af8fc16ae40479bf412689bb60f5,sp-lc-orph-cer:c604f0b918747bac3b192b7de12bd875743a5d7a" --actor test >/dev/null
lc cut batch-held --repo fixture-repo --head headH --base baseH --members "sp-lc-held:c114c63afe9b40e5cc412a8eb1fb63cb13fa7b56" --actor test >/dev/null
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

# ── a batch that opened and never began is a phantom: requeue-orphans releases its members ─
# POSITIVE CONTROL: a fresh OPEN batch and an aged OPEN batch with a pass in flight are not phantoms.
certify sp-lc-ph-a ed2e563137985950e2b283f7966d7cf5976e6fa5
certify sp-lc-ph-b 1d365c5fe4c023f429ab3917c5801474e7faba76
certify sp-lc-ph-fresh 6d5b313a9351f5cd9641a785337872e2f10783c4
certify sp-lc-ph-live 18dc358b5a86a837a8dd926e63e47e2d38bec084
lc cut batch-phantom --repo fixture-repo --head headPH --base basePH --members "sp-lc-ph-a:ed2e563137985950e2b283f7966d7cf5976e6fa5,sp-lc-ph-b:1d365c5fe4c023f429ab3917c5801474e7faba76" --actor test >/dev/null
lc cut batch-phantom-fresh --repo fixture-repo --head headPF --base basePF --members "sp-lc-ph-fresh:6d5b313a9351f5cd9641a785337872e2f10783c4" --actor test >/dev/null
lc cut batch-phantom-live --repo fixture-repo --head headPL --base basePL --members "sp-lc-ph-live:18dc358b5a86a837a8dd926e63e47e2d38bec084" --actor test >/dev/null
root_sql --use-db spira_lifecycle sql -q "UPDATE batch SET opened_at = 1 WHERE batch_id IN ('batch-phantom', 'batch-phantom-live'); UPDATE batch SET pass = 1 WHERE batch_id = 'batch-phantom-live'" >/dev/null
out="$(lc requeue-orphans --actor test)"
want "a dry run names the phantom batch" 'batch-phantom"' "$out"
want "a dry run names a phantom batch's member" 'sp-lc-ph-b' "$out"
nowant "a fresh OPEN batch is not a phantom" 'sp-lc-ph-fresh' "$out"
nowant "an aged batch with a pass in flight is not a phantom" 'sp-lc-ph-live' "$out"
is "a dry run leaves the phantom batch OPEN" "OPEN" "$(batch_field batch-phantom state)"
out="$(lc requeue-orphans --actor test --apply)"
wantrc "requeue-orphans --apply releases a phantom batch" 0 $?
is "the phantom batch is ABANDONED" "ABANDONED" "$(batch_field batch-phantom state)"
is "a phantom batch's member returns to CERTIFIED" "CERTIFIED" "$(member_field sp-lc-ph-a bead state)"
is "...and its delivery row exits" "EXITED" "$(member_field sp-lc-ph-b delivery state)"
is "the fresh batch stays OPEN" "OPEN" "$(batch_field batch-phantom-fresh state)"
is "the live batch stays OPEN" "OPEN" "$(batch_field batch-phantom-live state)"

# ── an ejected member of a phantom batch keeps its verdict: only its delivery row is cleared ─
# The eject's Returned event is recorded but the row writes never landed, so the bead still reads
# IN_DELIVERY and the delivery row BATCHED. POSITIVE CONTROL: its innocent co-member is requeued.
certify sp-lc-ej-red 2b0c3f2d6a1f4a5f8d3c0a9e7b6c5d4e3f2a1b0c
certify sp-lc-ej-ok 3c1d4a3e7b2a5b6a9e4d1b0f8c7d6e5f4a3b2c1d
lc cut batch-ejected --repo fixture-repo --head headEJ --base baseEJ --members "sp-lc-ej-red:2b0c3f2d6a1f4a5f8d3c0a9e7b6c5d4e3f2a1b0c,sp-lc-ej-ok:3c1d4a3e7b2a5b6a9e4d1b0f8c7d6e5f4a3b2c1d" --actor test >/dev/null
root_sql --use-db spira_lifecycle sql -q "UPDATE batch SET opened_at = 1 WHERE batch_id = 'batch-ejected'; INSERT INTO event (machine, lc_key, event, expect, from_state, to_state, applied, refusal, evidence, actor, at) VALUES ('bead', 'sp-lc-ej-red', 'Returned', 'IN_DELIVERY', 'IN_DELIVERY', 'REWORK', 1, NULL, '{\"Returned\":{\"reason\":\"batch-ejected\"}}', 'test', 2)" >/dev/null
out="$(lc requeue-orphans --actor test --apply)"
wantrc "requeue-orphans --apply releases the round holding an ejected member" 0 $?
is "the ejected member is not requeued" "REWORK" "$(member_field sp-lc-ej-red bead state)"
is "...and its stale delivery row exits" "EXITED" "$(member_field sp-lc-ej-red delivery state)"
is "an innocent co-member returns to CERTIFIED" "CERTIFIED" "$(member_field sp-lc-ej-ok bead state)"

# ── reconcile-landed: merged work the lifecycle missed reaches LANDED on ancestry ────────────
RG="$TMP/reconcile-repo"
git init -q -b main "$RG"
git -C "$RG" config user.email t@example.invalid
git -C "$RG" config user.name t
git -C "$RG" commit -q --allow-empty -m "base"
git -C "$RG" commit -q --allow-empty -m "sp-lc-rec-sub: the merged work"
MERGED="$(git -C "$RG" rev-parse HEAD)"
git -C "$RG" checkout -q -b side
git -C "$RG" commit -q --allow-empty -m "sp-lc-rec-side: work that never reached main"
SIDE="$(git -C "$RG" rev-parse HEAD)"
git -C "$RG" checkout -q main
submit sp-lc-rec-sub bd5e5de53738b69ea9989845b3219b182ef175ba
submit sp-lc-rec-side fe4fa7df3973668d669b9edf2b10b60ad0203bac
submit sp-lc-rec-batched 3c12f1f196afe28678e99c3aa227416019b60546
lc cut batch-rec-phantom --repo fixture-repo --head headR --base baseR --members "sp-lc-rec-batched:3c12f1f196afe28678e99c3aa227416019b60546" --actor test >/dev/null
rec() { lc reconcile-landed "$1" --sha "$2" --ref main --repo-path "$RG" --actor test; }
rec sp-lc-rec-side "$SIDE" >/dev/null 2>&1
wantrc "reconcile-landed refuses a commit that is not on the landing ref" 3 $?
is "...and the bead stays SUBMITTED" "SUBMITTED" "$(member_field sp-lc-rec-side bead state)"
rec sp-lc-rec-sub "$(git -C "$RG" rev-parse main~1)" >/dev/null 2>&1
wantrc "reconcile-landed refuses a commit that does not name the bead" 3 $?
is "...and that bead stays SUBMITTED" "SUBMITTED" "$(member_field sp-lc-rec-sub bead state)"
rec sp-lc-rec-sub "$MERGED" >/dev/null
wantrc "reconcile-landed lands a SUBMITTED bead whose commit is on the landing ref" 0 $?
is "the SUBMITTED bead is LANDED" "LANDED" "$(member_field sp-lc-rec-sub bead state)"
git -C "$RG" commit -q --allow-empty -m "sp-lc-rec-batched: merged by a PR"
rec sp-lc-rec-batched "$(git -C "$RG" rev-parse HEAD)" >/dev/null
wantrc "reconcile-landed lands a bead BATCHED in a phantom batch" 0 $?
is "the BATCHED bead is LANDED" "LANDED" "$(member_field sp-lc-rec-batched bead state)"
is "...and its delivery exits" "EXITED" "$(member_field sp-lc-rec-batched delivery state)"
is "...with the merge sha" "$(git -C "$RG" rev-parse HEAD)" "$(member_field sp-lc-rec-batched delivery merge_sha)"
rec sp-lc-rec-batched "$MERGED" >/dev/null 2>&1
wantrc "reconcile-landed refuses a bead that is already terminal" 3 $?

# ── sp-s9675.5, item 5: the epic-blocker hold-release rule stays "as today" ──────────────
# design stacked-dependents-2026-09-28 §1's hold-release table: a same-repo work-bead
# blocker releases the wait hold at CERTIFIED (criterion 5 above stacks on exactly that);
# "anything else (epic, decision, other repo)" still needs the blocker closed. An epic is
# never a work bead the dwarf machine claims/submits/certifies, so it never gets its own
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
count_open="$("$CLAIM_BIN" select --rune test --blockers machine \
    --ready "$ready_epicb" --lifecycle "$lc_epic_snapshot" --blocker-records "$blockers_epic_open" --count)"
wantrc "spira-claim select runs cleanly with the epic still open" 0 $?
is "B stays blocked while its epic is open" "0" "$count_open"

blockers_epic_closed="$TMP/blockers-epic-closed.json"
cat > "$blockers_epic_closed" <<JSON
[{"id":"sp-epic","status":"closed","issue_type":"epic","labels":["repo:spira"]}]
JSON
count_closed="$("$CLAIM_BIN" select --rune test --blockers machine \
    --ready "$ready_epicb" --lifecycle "$lc_epic_snapshot" --blocker-records "$blockers_epic_closed" --count)"
wantrc "spira-claim select runs cleanly once the epic closes" 0 $?
is "B is claimable once the epic closes — the hand-ordered epic edge, unchanged" "1" "$count_closed"

# ── a round's events, driven at the batch's current state and version ──
pev() {   # pev <batch> <kind-json>: the event at the batch's current state and version
    lc event batch "$1" --expect "$(batch_field "$1" state)" --version "$(batch_field "$1" version)" --actor test --kind "$2"
}
# ── a dwarf's phase and disposition on the WORKING row, against the real store ──
lc create-bead sp-lc-ph >/dev/null
lc event bead sp-lc-ph --expect READY --version 0 --actor dwarf-ph --kind '{"Claim":{"holder":"dwarf-ph","lease_until":1}}' >/dev/null
phase_of() { lc show sp-lc-ph | python3 -c 'import json,sys; print(json.load(sys.stdin)["bead"]["dwarf_phase"])'; }
is "a claim starts the row in phase claimed" "claimed" "$(phase_of)"
lc phase sp-lc-ph dwarf-ph building >/dev/null
wantrc "the holder records its next phase" 0 $?
is "show reads exactly the recorded phase" "building" "$(phase_of)"
out="$(lc phase sp-lc-ph dwarf-ph claimed 2>&1)"
wantrc "a step back is refused" 3 $?
want "the refusal names the state and phase" "WORKING/building" "$out"
lc phase sp-lc-ph dwarf-other session >/dev/null 2>&1
wantrc "a phase from a non-holder is refused" 3 $?
is "the refused moves left the phase where it was" "building" "$(phase_of)"
lc disposition sp-lc-ph lapsed "300	last words" watchdog >/dev/null
wantrc "a lapsed disposition is recorded" 0 $?
lc disposition sp-lc-ph slain "by hand" slay >/dev/null
wantrc "a stronger disposition replaces it" 0 $?
lc disposition sp-lc-ph thrash "x" heartbeat >/dev/null 2>&1
wantrc "a weaker one never does" 3 $?
is "list --state WORKING carries the phase and the disposition" "building slain by hand" "$(lc list --state WORKING | python3 -c 'import json,sys; b=[x for x in json.load(sys.stdin) if x["bead_id"]=="sp-lc-ph"][0]; print(b["dwarf_phase"], b["disposition"], b["disposition_note"])')"
lc release sp-lc-ph dwarf-ph >/dev/null
is "leaving WORKING clears the phase and the disposition" "None None" "$(lc show sp-lc-ph | python3 -c 'import json,sys; b=json.load(sys.stdin)["bead"]; print(b["dwarf_phase"], b["disposition"])')"
lc disposition sp-lc-ph slain "late" slay >/dev/null 2>&1
wantrc "a disposition outside WORKING is refused" 3 $?

# ── the dispatcher's verdicts ride the bead row: written once per pass, read with the live rows ──
verdict_of() { lc list --live | python3 -c 'import json,sys; b=[x for x in json.load(sys.stdin) if x["bead_id"]=="sp-lc-ph"][0]; print(b["claim_verdict"], b["claim_blocker"], b["claim_runes"], b["claim_at"])'; }
version_of() { lc show sp-lc-ph | python3 -c 'import json,sys; print(json.load(sys.stdin)["bead"]["version"])'; }
v_before="$(version_of)"
is "a row nobody judged carries no verdict" "None None None None" "$(verdict_of)"
printf '%s' '{"computed_at":"2026-10-30T22:20:00Z","entries":[{"id":"sp-lc-ph","verdict":"blocked","blocker":"sp-lc-parent"}]}' | lc claim-verdicts >/dev/null
wantrc "a pass of verdicts is written" 0 $?
is "list --live returns the verdict, its blocker and the pass stamp" "blocked sp-lc-parent None 2026-10-30T22:20:00Z" "$(verdict_of)"
printf '%s' '{"computed_at":"2026-10-30T22:21:00Z","entries":[{"id":"sp-lc-ph","verdict":"claimable","runes":["builder","ops"]}]}' | lc claim-verdicts >/dev/null
is "the next pass replaces the verdict whole" "claimable None builder,ops 2026-10-30T22:21:00Z" "$(verdict_of)"
printf '%s' '{"computed_at":"yesterday","entries":[]}' | lc claim-verdicts >/dev/null 2>&1
wantrc "a pass with a bad stamp is refused" 3 $?
is "a verdict is a reading, not a transition: the row's version did not move" "$v_before" "$(version_of)"
is "the refused pass left the verdict alone" "claimable None builder,ops 2026-10-30T22:21:00Z" "$(verdict_of)"

tl_summary
