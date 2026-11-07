#!/usr/bin/env bash
#
# test-ops-read-model.sh — the lifecycle read model the panes read instead of scanning bd:
# the ops_live / ops_round / ops_recent views, the where-stuck views (ops_edges, ops_dwell and
# its p95 helper), the title and priority mirrored into the bead
# row, and the read-only user. Against a real Dolt, on a store sized for its query plans.
#
#   - each view answers through `spira-lc ops-view`, and its plan is asserted (an index
#     access on every table it reads, none on `event`) — never a wall-clock;
#   - the plan matcher is proved on a query that does scan, before its silence is believed;
#   - where-stuck: replays of a clean history and of each stuck case give ops_dwell its p95s and
#     entry times; refusals and edge rates land in ops_edges. Which beads are stuck is decided by
#     loom/src/stuck.rs and tested there, with the legal-edge graph and the argument refusals in
#     spira-lc and schema.sql against the migrations in lifecycle/tests/schema_views.rs;
#   - the backfill gives every live row a title; a mirror write keeps `version`;
#   - the read-only user reads the views and writes nothing.
#
# host-reason: starts its own disposable `dolt sql-server`, same shape as test-lc-hold.sh.
#
# requires: jq
# tier: T2
# covers: lifecycle/* loom/src/stuck.rs spira-lc/src/ops.rs spira-lc/src/cutover.rs spira-lc/src/migrate.rs bead/src/main.rs
# timeout: 240
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=60
. "$HERE/testlib/lc-fixture.sh"

TMP="$(mktemp -d)"
trap 'lcfix_down; rm -rf "$TMP"' EXIT INT TERM
lcfix_up || bail "could not build a lifecycle fixture"
LC_DIR="$HERE/../lifecycle"

export SPIRA_LC_ADMIN_USER=root SPIRA_LC_ADMIN_PASSWORD=""
tl_setup_done
views="SELECT table_name FROM information_schema.tables WHERE table_schema = 'spira_lifecycle' AND table_type = 'VIEW' ORDER BY 1"
ALL_VIEWS="ops_dwell ops_dwell_p95 ops_edges ops_live ops_recent ops_round"
is "a fresh database (schema.sql alone) already has every view" "$ALL_VIEWS" "$(lcfix_sql -r csv -q "$views" | sed 1d | paste -sd' ')"
lcfix_sql -q "DROP VIEW ops_dwell; DROP VIEW ops_dwell_p95; DROP VIEW ops_edges; DROP INDEX event_at_idx ON event; DROP VIEW ops_live; DROP TABLE bead_dep; DROP VIEW ops_round; DROP VIEW ops_recent; DROP INDEX bead_state_since_idx ON bead; DROP INDEX batch_state_idx ON batch; ALTER TABLE bead DROP COLUMN title; ALTER TABLE bead DROP COLUMN priority" >/dev/null 2>&1 || bail "could not rebuild the pre-0007 shape"
tl_setup_done
is "positive control: the pre-0007 store has no views" 0 "$(lcfix_sql -r csv -q "$views" | sed 1d | wc -l)"
out="$(spira-lc admin-migrate "$LC_DIR/migrations" 2>&1)" || bail "admin-migrate failed: $out"
is "migrating the pre-0007 store makes every view" "$ALL_VIEWS" "$(lcfix_sql -r csv -q "$views" | sed 1d | paste -sd' ')"
is "migrating restores the window index" 1 "$(lcfix_sql -r csv -q "SELECT COUNT(DISTINCT index_name) FROM information_schema.statistics WHERE table_schema='spira_lifecycle' AND index_name='event_at_idx'" | sed -n 2p)"
again="$(spira-lc admin-migrate "$LC_DIR/migrations" 2>&1)"
wantrc "a second migrate run is a no-op" 0 $?
want "the second run finds nothing pending" "already applied" "$again"

is "migrating the pre-0007 store gives ops_live its claimable and blocker" "blocker claimable" "$(lcfix_sql -r csv -q "SELECT column_name FROM information_schema.columns WHERE table_name = 'ops_live' AND column_name IN ('claimable','blocker') ORDER BY 1" | sed 1d | paste -sd' ')"

# SEED: 12 beads, 24 events: 6 live beads (OPEN..IN_DELIVERY), 4 landed in the last day, then one landed
# 3 days ago and one dropped; 2 events inside the window, 22 older. No plan changed from 300 beads /
# 3,000 events down to 12 / 24, the floor: 12 beads is the fewest that hold every state and 24 events keep
# both sides of the window. Nothing smaller was tried, so no first-changing size is known.
NOW="$(date +%s)"
SEED_BEADS=12
SEED_EVENTS=24
seed_sql() {
    cat <<SQL
BEGIN;
INSERT INTO bead (bead_id,state,tip,gate_key,holder,lease_until,holds,reason,version,since,title,priority,updated_at,stack_depth)
WITH RECURSIVE s(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM s WHERE i < $SEED_BEADS - 1)
SELECT CONCAT('sp-b',i),
       CASE WHEN i < 6 THEN ELT(i % 6 + 1,'OPEN','READY','WORKING','SUBMITTED','CERTIFIED','IN_DELIVERY') WHEN i < 10 OR i % 2 = 1 THEN 'LANDED' ELSE 'DROPPED' END,
       NULL,NULL,NULL,NULL,'[]',NULL,1,
       CASE WHEN i < 6 THEN NULL WHEN i < 10 THEN $NOW - 3600 * (i % 20) WHEN i % 2 = 1 THEN $NOW - 3 * 86400 - i ELSE NULL END,
       IF(i < 2, CONCAT('t',i), NULL), IF(i < 2, 2, NULL), 5, 0
  FROM s;
INSERT INTO event (machine,lc_key,event,expect,from_state,to_state,applied,refusal,evidence,actor,at)
WITH RECURSIVE s(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM s WHERE i < $SEED_EVENTS - 1)
SELECT 'bead',CONCAT('sp-b',i % $SEED_BEADS),'Claim','READY','READY','WORKING',1,NULL,'{}','fixture',IF(i % 20 = 0, $NOW, $NOW - 20 * 86400)
  FROM s;
INSERT INTO batch (batch_id,repo,state,head,base,version,opened_at) VALUES ('b-open','r','OPEN','h','b',1,$NOW),('b-old','r','LANDED','h','b',1,$NOW - 9 * 86400);
INSERT INTO batch_member (batch_id,bead_id,tip) VALUES ('b-open','sp-b1','t1'),('b-open','sp-b2','t2'),('b-old','sp-b3','t3');
COMMIT;
SELECT (SELECT COUNT(*) FROM bead), (SELECT COUNT(*) FROM event);
SQL
}
seeded="$(seed_sql | lcfix_sql -r csv 2>"$TMP/seed.err" | tail -1)" || bail "seed failed: $(head -c 400 "$TMP/seed.err")"
is "the fixture holds $SEED_BEADS beads" "$SEED_BEADS" "${seeded%%,*}"
is "the fixture holds $SEED_EVENTS events" "$SEED_EVENTS" "${seeded##*,}"

declare -A PLAN
plan_of() { lcfix_sql -r csv -q "EXPLAIN FORMAT=TREE $1" 2>&1 | tr -s " \n" " |"; }
scans() { grep -qE '(└─|├─) Table"?\|' <<<"$1"; }
reads() { grep -q "IndexedTableAccess($2)" <<<"$1"; }

scan="$(plan_of "SELECT * FROM bead WHERE title = 'x'")"
want "the plan reader returns a plan" "bead" "$scan"
if scans "$scan"; then ok "positive control: a query on an unindexed column is seen as a scan"; else bad "positive control: a query on an unindexed column is seen as a scan" "$scan"; fi

check_plans() {
    local when="$1" v t
    for v in ops_live ops_recent ops_round; do
        PLAN[$v]="$(plan_of "SELECT * FROM $v")"
        if scans "${PLAN[$v]}"; then bad "$v scans no table ($when)" "${PLAN[$v]}"; else ok "$v scans no table ($when)"; fi
        nowant "$v never reads event ($when)" "name: event" "${PLAN[$v]}"
    done
    for v in ops_live ops_recent; do reads "${PLAN[$v]}" bead && ok "$v reads bead by index ($when)" || bad "$v reads bead by index ($when)" "${PLAN[$v]}"; done
    reads "${PLAN[ops_live]}" bead_dep && ok "ops_live reads bead_dep by index ($when)" || bad "ops_live reads bead_dep by index ($when)" "${PLAN[ops_live]}"
    for t in batch batch_member bead; do reads "${PLAN[ops_round]}" $t && ok "ops_round reads $t by index ($when)" || bad "ops_round reads $t by index ($when)" "${PLAN[ops_round]}"; done
}
window_index="index: [event.machine,event.applied,event.at,"
lookup_index="index: [event.machine,event.applied,event.lc_key,event.to_state,event.at]"
check_stuck_plans() {
    local when="$1" v p
    for v in ops_edges ops_dwell_p95 ops_dwell; do
        PLAN[$v]="$(plan_of "SELECT * FROM $v")"
        if scans "${PLAN[$v]}"; then bad "$v scans no table ($when)" "${PLAN[$v]}"; else ok "$v scans no table ($when)"; fi
    done
    want "ops_edges takes its window from the event time index ($when)" "$window_index" "${PLAN[ops_edges]}"
    want "the p95 helper takes its window from the event time index ($when)" "$window_index" "${PLAN[ops_dwell_p95]}"
    want "ops_dwell looks each bead's entry up by key in the event index ($when)" "$lookup_index" "${PLAN[ops_dwell]}"
    reads "${PLAN[ops_dwell]}" bead && ok "ops_dwell reads bead by index ($when)" || bad "ops_dwell reads bead by index ($when)" "${PLAN[ops_dwell]}"
    p="$(plan_of "$GANTT_SQL")"
    if scans "$p"; then bad "the stuck page's gantt query scans no table ($when)" "$p"; else ok "the stuck page's gantt query scans no table ($when)"; fi
    want "the gantt query takes its window from the event time index ($when)" "$window_index" "$p"
    p="$(plan_of "SELECT seq, event, from_state, to_state, applied, refusal, evidence, actor, at FROM event WHERE machine = 'bead' AND lc_key = 'sp-1' ORDER BY seq")"
    if scans "$p"; then bad "a bead's timeline query scans no table ($when)" "$p"; else ok "a bead's timeline query scans no table ($when)"; fi
    p="$(plan_of "SELECT bead_id, state, holder FROM bead WHERE bead_id = 'sp-1'")"
    reads "$p" bead && ok "a bead's row is read by key ($when)" || bad "a bead's row is read by key ($when)" "$p"
}
GANTT_SQL="$(spira-lc ops-gantt --print-sql)"
tl_setup_done
check_plans "no statistics"
check_stuck_plans "no statistics"
lcfix_sql -q "ANALYZE TABLE bead, batch, batch_member" >/dev/null 2>&1
tl_setup_done
check_plans "after ANALYZE"
lcfix_sql -q "ANALYZE TABLE event" >/dev/null 2>&1
tl_setup_done
check_stuck_plans "after ANALYZE"

view_ids() { spira-lc ops-view "$1" | jq -r '[.[].bead_id] | sort | join(" ")'; }
is "ops_live is the 6 non-terminal rows" 6 "$(view_ids ops_live | wc -w)"
is "ops_recent is the landings of the last day" 4 "$(view_ids ops_recent | wc -w)"
is "ops_round is the open batch's members only" "sp-b1 sp-b2" "$(view_ids ops_round)"
want "a view carries the mirrored title" '"title":"t0"' "$(spira-lc ops-view ops_live)"
snap="$(spira-lc ops-snapshot)"
snap_keys="$(printf '%s' "$snap" | jq -r 'keys | join(" ")')"
is "ops-snapshot carries every section the pane draws" "batches dwell dwell_p95 edges ejects graph live now recent" "$snap_keys"
want "ops-snapshot live rows carry since and title" '"since"' "$(printf '%s' "$snap" | jq -c '.live[:1]')"

untitled() { lcfix_sql -r csv -q "SELECT COUNT(*) FROM ops_live WHERE title IS NULL" | sed -n 2p; }
[ "$(untitled)" -gt 0 ] && ok "positive control: live rows are untitled before the backfill" || bad "positive control: live rows are untitled before the backfill" "$(untitled)"
mkdir -p "$TMP/bin"
cat > "$TMP/bin/bd" <<'STUB'
#!/bin/sh
[ "$1" = show ] && printf '[{"id":"%s","title":"from bd %s","priority":3,"labels":["plan","repo:spira"]}]\n' "$2" "$2"
[ "$1 $2 $3" = "dep list sp-b5" ] && printf '[{"depends_on_id":"sp-b4","type":"blocks"},{"depends_on_id":"sp-b3","type":"parent-child"}]\n'
[ "$1 $2" = "dep list" ] && [ "$3" != sp-b5 ] && echo '[]'
exit 0
STUB
chmod +x "$TMP/bin/bd"
tl_config SPIRA_BD="$TMP/bin/bd" SPIRA_DB="" || bail "cannot declare the stub bd"
tl_setup_done
spira-lc backfill-titles >/dev/null 2>&1
wantrc "the backfill completes" 0 $?
is "the backfill leaves no live row untitled" 0 "$(untitled)"
is "the backfill leaves no recent landing untitled" 0 "$(lcfix_sql -r csv -q 'SELECT COUNT(*) FROM ops_recent WHERE title IS NULL' | sed -n 2p)"
want "the backfill wrote bd's title" "from bd sp-b8" "$(lcfix_sql -r csv -q "SELECT title FROM bead WHERE bead_id='sp-b8'")"
is "the backfill keeps an existing title" "t0" "$(lcfix_sql -r csv -q "SELECT title FROM bead WHERE bead_id='sp-b0'" | sed -n 2p)"

unrepoed() { lcfix_sql -r csv -q "SELECT COUNT(*) FROM bead WHERE repo IS NULL AND state IN ('OPEN','READY','WORKING','SUBMITTED','CERTIFIED','IN_DELIVERY','REWORK')" | sed -n 2p; }
[ "$(unrepoed)" -gt 0 ] && ok "positive control: live rows have no repo before the backfill" || bad "positive control: live rows have no repo before the backfill" "$(unrepoed)"
spira-lc backfill-repos >/dev/null 2>&1; wantrc "the repo backfill completes" 0 $?
is "the repo backfill leaves no live row without a repo" 0 "$(unrepoed)"

live_row() { spira-lc ops-view ops_live | jq -r --arg id "$1" '[.[] | select(.bead_id == $id)] | if length == 0 then "absent" else "\(.[0].claimable) \(.[0].blocker // "None")" end'; }
bead_deps() { lcfix_sql -r csv -q 'SELECT COUNT(*) FROM bead_dep' | sed -n 2p; }
is "positive control: no edges before the backfill, so a READY bead is claimable" "1 None" "$(live_row sp-b1)"
spira-lc backfill-deps >/dev/null 2>&1
wantrc "the dependency backfill completes" 0 $?
is "the backfill mirrors the edges bd lists, every type" 2 "$(bead_deps)"
is "a blocks edge to a live bead makes the dependent unclaimable and names the blocker" "0 sp-b4" "$(live_row sp-b5)"
lcfix_sql -q "DELETE FROM bead_dep WHERE depends_on = 'sp-b3'" >/dev/null
tl_setup_done
spira-lc backfill-deps >/dev/null 2>&1
wantrc "a second backfill runs" 0 $?
is "a second backfill finds edges mirrored and leaves the store alone" 1 "$(bead_deps)"
spira-lc backfill-deps --force >/dev/null 2>&1
wantrc "a forced backfill runs" 0 $?
is "--force reads bd again" 2 "$(bead_deps)"

spira-lc create-bead sp-A --priority 1 >/dev/null; spira-lc create-bead sp-B --priority 1 >/dev/null
tl_setup_done
spira-lc dep-add sp-B sp-A >/dev/null; wantrc "dep-add mirrors an edge" 0 $?
is "A blocks B: B is blocked by A and not claimable" "0 sp-A" "$(live_row sp-B)"
is "A itself stays claimable" "1 None" "$(live_row sp-A)"
spira-lc dep-add sp-B sp-A >/dev/null; wantrc "dep-add is idempotent" 0 $?
lcfix_sql -q "UPDATE bead SET state='LANDED', since=UNIX_TIMESTAMP() WHERE bead_id='sp-A'" >/dev/null
tl_setup_done
is "when A lands, B becomes claimable with no blocker" "1 None" "$(live_row sp-B)"
lcfix_sql -q "UPDATE bead SET state='READY', since=NULL WHERE bead_id='sp-A'" >/dev/null
tl_setup_done
is "positive control: a blocker that is live again blocks again" "0 sp-A" "$(live_row sp-B)"
lc_blocked() { spira-lc list --ids "$1" | jq -r '.[0].blocked_by | if length == 0 then "none" else join(",") end'; }
lc_show_blocked() { spira-lc show "$1" | jq -r '.bead.blocked_by | if length == 0 then "none" else join(",") end'; }
is "list carries the unlanded blocker as blocked_by" "sp-A" "$(lc_blocked sp-B)"
is "show carries the same blocked_by" "sp-A" "$(lc_show_blocked sp-B)"
is "a bead nothing blocks has an empty blocked_by" "none" "$(lc_blocked sp-A)"
lcfix_sql -q "UPDATE bead SET state='LANDED' WHERE bead_id='sp-A'" >/dev/null
tl_setup_done
is "a landed blocker drops out of blocked_by" "none" "$(lc_blocked sp-B)"
lcfix_sql -q "UPDATE bead SET state='READY' WHERE bead_id='sp-A'" >/dev/null
tl_setup_done
spira-lc dep-remove sp-B sp-A >/dev/null; wantrc "dep-remove drops the edge" 0 $?
is "a removed edge no longer blocks" "1 None" "$(live_row sp-B)"
spira-lc dep-add sp-B sp-A --type relates-to >/dev/null
tl_setup_done
is "a non-blocks edge is recorded and never blocks" "1 None" "$(live_row sp-B)"
lcfix_sql -q "UPDATE bead SET holds='[\"ask\"]' WHERE bead_id='sp-B'" >/dev/null
tl_setup_done
is "a held bead is not claimable" "0 None" "$(live_row sp-B)"

spira-lc create-bead sp-mirror --title "it's mine" --priority 1 >/dev/null; wantrc "create-bead mirrors a title and priority" 0 $?
spira-lc create-bead sp-mirror --priority 0 >/dev/null; wantrc "a second create-bead for the same bead mirrors again" 0 $?
row="$(lcfix_sql -r csv -q "SELECT title, priority, version FROM bead WHERE bead_id='sp-mirror'" | sed -n 2p)"
is "a mirror write sets title and priority, keeps an earlier title, and never bumps the version" "it's mine,0,0" "$row"
spira-lc create-bead sp-filed-here --repo pokedumpster --priority 2 >/dev/null; wantrc "create-bead takes --repo" 0 $?
is "a bead created with --repo gets the column" "pokedumpster" "$(lcfix_sql -r csv -q "SELECT repo FROM bead WHERE bead_id='sp-filed-here'" | sed -n 2p)"
is "list --repo returns exactly that repo's beads" "sp-filed-here" "$(spira-lc list --repo pokedumpster | jq -r '[.[].bead_id] | join(",")')"
is "list --repo with --state composes" "" "$(spira-lc list --repo pokedumpster --state LANDED | jq -r '[.[].bead_id] | join(",")')"
want "list rows carry repo" '"repo"' "$(spira-lc list --ids sp-filed-here)"

sed -e "s/@SPIRA_LC_PASSWORD@/rw-pw/" -e "s/@SPIRA_LC_RO_PASSWORD@/ro-pw/" "$LC_DIR/grants.sql" > "$TMP/grants.sql"
spira-lc admin-apply-ddl "$TMP/grants.sql" >/dev/null 2>"$TMP/grants.err" || bail "grants failed: $(cat "$TMP/grants.err")"
tl_setup_done
ro() { "$LCFIX_DOLT" --data-dir "$LCFIX_DIR" --host 127.0.0.1 --port "$SPIRA_LC_PORT" -u spira_lc_ro -p ro-pw --no-tls --use-db spira_lifecycle sql "$@" 2>&1; }
is "the read-only user reads a view" 1 "$(ro -r csv -q "SELECT COUNT(*) FROM ops_round WHERE bead_id='sp-b1'" | sed -n 2p)"
for w in "UPDATE bead SET title='x' WHERE bead_id='sp-b1'" "INSERT INTO bead (bead_id,state,holds,version,updated_at) VALUES ('sp-w','READY','[]',0,0)" "DELETE FROM bead WHERE bead_id='sp-b1'" "UPDATE ops_live SET title='x'" "INSERT INTO event (machine,lc_key,event,expect,from_state,to_state,applied,evidence,actor,at) VALUES ('bead','k','e','a','a','a',1,'{}','x',0)"; do
    r="$(ro -q "$w")"; rc=$?
    if [ "$rc" != 0 ] && grep -qi "denied" <<<"$r"; then ok "the read-only user is refused: ${w%% *} ${w:0:40}"; else bad "the read-only user is refused: $w" "rc=$rc $r"; fi
done
is "the refused writes changed nothing" "t1" "$(lcfix_sql -r csv -q "SELECT title FROM bead WHERE bead_id='sp-b1'" | sed -n 2p | sed 's/from bd sp-b1/t1/')"

# --- where-stuck: replay each case on a store wiped to a known history -------------------------------
NOW="$(date +%s)"
ws_history_sql() {
    cat <<SQL
BEGIN;
DELETE FROM batch_member; DELETE FROM batch; DELETE FROM event; DELETE FROM bead;
INSERT INTO bead (bead_id,state,holds,version,updated_at)
WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM n WHERE i < 19)
SELECT CONCAT('ws-h',i),'LANDED','[]',1,$NOW FROM n;
INSERT INTO bead (bead_id,state,holds,version,updated_at) VALUES
('ws-fresh-ready','READY','[]',1,$NOW),('ws-fresh-working','WORKING','[]',1,$NOW),('ws-fresh-submitted','SUBMITTED','[]',1,$NOW),
('ws-fresh-certified','CERTIFIED','[]',1,$NOW),('ws-fresh-in_delivery','IN_DELIVERY','[]',1,$NOW);
INSERT INTO event (machine,lc_key,event,expect,from_state,to_state,applied,refusal,evidence,actor,at)
WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM n WHERE i < 19),
e(j) AS (SELECT 0 UNION ALL SELECT j+1 FROM e WHERE j < 6)
SELECT 'bead',CONCAT('ws-h',i),
       ELT(j+1,'Claim','Release','Claim','Submit','GatePass','Deliver','Delivered'),
       ELT(j+1,'READY','WORKING','READY','WORKING','SUBMITTED','CERTIFIED','IN_DELIVERY'),
       ELT(j+1,'READY','WORKING','READY','WORKING','SUBMITTED','CERTIFIED','IN_DELIVERY'),
       ELT(j+1,'WORKING','READY','WORKING','SUBMITTED','CERTIFIED','IN_DELIVERY','LANDED'),
       1,NULL,'{}','fixture',
       $NOW - 4 * 3600 - i * 60 + ELT(j+1,-3000,-1200,-1140 + i,-100,0,600 + i,900 + 2 * i)
  FROM n JOIN e;
INSERT INTO event (machine,lc_key,event,expect,from_state,to_state,applied,refusal,evidence,actor,at) VALUES
('bead','ws-fresh-ready','Claim','READY','READY','WORKING',1,NULL,'{}','fixture',$NOW - 3000),
('bead','ws-fresh-ready','Release','WORKING','WORKING','READY',1,NULL,'{}','fixture',$NOW - 30),
('bead','ws-fresh-working','Fresh','READY','READY','WORKING',1,NULL,'{}','fixture',$NOW - 30),
('bead','ws-fresh-submitted','Fresh','WORKING','WORKING','SUBMITTED',1,NULL,'{}','fixture',$NOW - 30),
('bead','ws-fresh-certified','Fresh','SUBMITTED','SUBMITTED','CERTIFIED',1,NULL,'{}','fixture',$NOW - 30),
('bead','ws-fresh-in_delivery','Fresh','CERTIFIED','CERTIFIED','IN_DELIVERY',1,NULL,'{}','fixture',$NOW - 30);
COMMIT;
SQL
}
ws_stuck_sql() {
    cat <<SQL
BEGIN;
INSERT INTO bead (bead_id,state,holds,version,updated_at) VALUES
('sp-2cah6','CERTIFIED','[]',1,$NOW),('sp-penudq','CERTIFIED','[]',1,$NOW),('sp-h4umcj','WORKING','[]',1,$NOW),('ws-strand','IN_DELIVERY','[]',1,$NOW);
INSERT INTO event (machine,lc_key,event,expect,from_state,to_state,applied,refusal,evidence,actor,at) VALUES
('bead','sp-2cah6','Fresh','SUBMITTED','SUBMITTED','CERTIFIED',1,NULL,'{}','fixture',$NOW - 20 * 3600),
('bead','sp-penudq','Fresh','SUBMITTED','SUBMITTED','CERTIFIED',1,NULL,'{}','fixture',$NOW - 23 * 3600),
('bead','sp-h4umcj','Fresh','READY','READY','WORKING',1,NULL,'{}','fixture',$NOW - 30 * 3600),
('bead','ws-strand','Fresh','CERTIFIED','CERTIFIED','IN_DELIVERY',1,NULL,'{}','fixture',$NOW - 10 * 3600),
('bead','sp-2cah6','GateRed','CERTIFIED','CERTIFIED','CERTIFIED',0,'IllegalTransition','{}','fixture',$NOW - 600),
('bead','sp-2cah6','GateRed','CERTIFIED','CERTIFIED','CERTIFIED',0,'IllegalTransition','{}','fixture',$NOW - 300),
('bead','sp-penudq','GateRed','CERTIFIED','CERTIFIED','CERTIFIED',0,'IllegalTransition','{}','fixture',$NOW - 7200);
COMMIT;
SQL
}

ws_history_sql | lcfix_sql >/dev/null 2>"$TMP/ws.err" || bail "history seed failed: $(head -c 400 "$TMP/ws.err")"
tl_setup_done
is "the history replay holds 25 beads and 146 events" "25,146" "$(lcfix_sql -r csv -q "SELECT (SELECT COUNT(*) FROM bead), (SELECT COUNT(*) FROM event)" | tail -1)"

dwell_count() { jq 'length' <<<"$DWELL"; }
p95_of() { jq -r --arg s "$1" '[.[] | select(.state == $s and .p95_s != null)][0].p95_s | tonumber | floor' <<<"$DWELL"; }
DWELL="$(spira-lc ops-view ops_dwell)"
is "the measured p95 of CERTIFIED is the 19th of 20 observed dwells" 618 "$(p95_of CERTIFIED)"
is "the measured p95 of IN_DELIVERY is the 19th of 20 observed dwells" 318 "$(p95_of IN_DELIVERY)"
is "the measured p95 of SUBMITTED" 100 "$(p95_of SUBMITTED)"
is "the measured p95 of READY comes from the re-readied dwells" 78 "$(p95_of READY)"
is "the measured p95 of WORKING takes the longer of its two kinds of dwell" 1800 "$(p95_of WORKING)"
is "a clean fixture lists its five live beads" 5 "$(dwell_count)"

ws_stuck_sql | lcfix_sql >/dev/null 2>"$TMP/ws.err" || bail "stuck seed failed: $(head -c 400 "$TMP/ws.err")"
tl_setup_done
is "the stuck replay holds 29 beads and 153 events" "29,153" "$(lcfix_sql -r csv -q "SELECT (SELECT COUNT(*) FROM bead), (SELECT COUNT(*) FROM event)" | tail -1)"
DWELL="$(spira-lc ops-view ops_dwell)"
entered_at() { jq -r --arg id "$1" '.[] | select(.bead_id == $id) | .entered_at | tonumber | floor' <<<"$DWELL"; }
is "the stuck replay lists nine live beads" 9 "$(dwell_count)"
is "a bead's time in its state starts at the event that entered it" "$((NOW - 23 * 3600))" "$(entered_at sp-penudq)"

EDGES="$(spira-lc ops-view ops_edges)"
edge() { jq -r --arg k "$1" --arg f "$2" --arg t "$3" --arg ev "$4" '
    [.[] | select(.kind == $k and .from_state == $f and (.to_state // "") == $t and ($ev == "*" or (.event // "") == $ev))]
    | if length == 0 then "none" else "\(map(.n_1h | tonumber) | add) \(map(.n_24h | tonumber) | add)" end' <<<"$EDGES"; }
is "applied SUBMITTED to CERTIFIED: 20 history, 2 stuck and 1 fresh in the day, the fresh one in the hour" "1 23" "$(edge applied SUBMITTED CERTIFIED "*")"
is "applied READY to WORKING counts both claims of each of 20 beads, and two fresh" "2 42" "$(edge applied READY WORKING "*")"
is "applied WORKING to READY counts the 20 releases and the fresh one" "1 21" "$(edge applied WORKING READY "*")"
is "two events drive SUBMITTED to CERTIFIED: GatePass is named with its own count" "0 20" "$(edge applied SUBMITTED CERTIFIED GatePass)"
is "two events drive SUBMITTED to CERTIFIED: Fresh is named with its own count" "1 3" "$(edge applied SUBMITTED CERTIFIED Fresh)"
is "two events drive READY to WORKING: Claim is named with its own count" "1 41" "$(edge applied READY WORKING Claim)"
is "two events drive READY to WORKING: Fresh is named with its own count" "1 1" "$(edge applied READY WORKING Fresh)"
is "every applied row names its event" 0 "$(jq '[.[] | select(.kind == "applied" and ((.event // "") == ""))] | length' <<<"$EDGES")"
is "refused events by state and kind: two in the hour, three in the day" "2 3" "$(edge refused CERTIFIED "" GateRed)"
is "an applied event that stays in its state is not an edge" "none" "$(edge applied CERTIFIED CERTIFIED "*")"
is "a refusal carries its reason" "IllegalTransition" "$(jq -r '[.[] | select(.kind == "refused") | .refusal] | unique | join(",")' <<<"$EDGES")"

since_seed_sql() {
    cat <<SQL
INSERT INTO bead (bead_id,state,holds,version,since,updated_at) VALUES ('sp-s1','SUBMITTED','[]',1,999,5),('sp-s2','WORKING','[]',1,NULL,6),('sp-s3','OPEN','[]',1,NULL,7);
INSERT INTO event (machine,lc_key,event,expect,from_state,to_state,applied,refusal,evidence,actor,at) VALUES
('bead','sp-s1','Claim','READY','READY','WORKING',1,NULL,'{}','fixture',100),
('bead','sp-s1','Submit','WORKING','WORKING','SUBMITTED',1,NULL,'{}','fixture',200),
('bead','sp-s1','Renew','SUBMITTED','SUBMITTED','SUBMITTED',1,NULL,'{}','fixture',300),
('bead','sp-s2','Claim','READY','READY','WORKING',1,NULL,'{}','fixture',400),
('bead','sp-s2','Submit','WORKING','WORKING','SUBMITTED',0,'Illegal','{}','fixture',500);
SQL
}
since_seed_sql | lcfix_sql >/dev/null 2>"$TMP/since.err" || bail "since seed failed: $(head -c 400 "$TMP/since.err")"
tl_setup_done
is "the since fixture holds three beads" 3 "$(lcfix_sql -r csv -q "SELECT COUNT(*) FROM bead WHERE bead_id LIKE 'sp-s_'" | tail -1)"
since_of() { lcfix_sql -r csv -q "SELECT since FROM bead WHERE bead_id = '$1'" | sed -n 2p; }
is "positive control: a stale since is not the entry time before the backfill" 999 "$(since_of sp-s1)"
since_ev=$(lcfix_sql -r csv -q "SELECT MAX(e.at) FROM event e JOIN bead b ON b.bead_id = e.lc_key AND b.state = e.to_state WHERE e.machine = 'bead' AND e.applied = 1 AND e.from_state <> e.to_state AND e.lc_key = 'sp-s1'" | sed -n 2p)
is "the join's answer for the moved bead is its SUBMITTED event" 200 "$since_ev"
out="$(spira-lc admin-migrate "$LC_DIR/migrations" 2>&1)" || bail "admin-migrate failed: $out"
is "the backfill puts a bead at the time of its last change of state, not of a same-state event" 200 "$(since_of sp-s1)"
is "the backfill ignores a refused event" 400 "$(since_of sp-s2)"
is "a bead with no state change falls back to its updated_at" 7 "$(since_of sp-s3)"
is "no bead is left without a since" 0 "$(lcfix_sql -r csv -q "SELECT COUNT(*) FROM bead WHERE since IS NULL" | sed -n 2p)"
again="$(spira-lc admin-migrate "$LC_DIR/migrations" 2>&1)"
want "the backfill is applied once" "already applied" "$again"
is "list reports since from the column" 200 "$(spira-lc list --ids sp-s1 | jq -r '.[0].since')"

tl_summary
