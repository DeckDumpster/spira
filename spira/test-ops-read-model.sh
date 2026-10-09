#!/usr/bin/env bash
#
# test-ops-read-model.sh — the lifecycle read model the panes read instead of scanning bd:
# the ops_live / ops_round / ops_recent views, the title and priority mirrored into the bead
# row, and the read-only user. Against a real Dolt, on a store of 12,000 beads and 100,000
# events.
#
#   - each view answers through `spira-lc ops-view`, and its plan is asserted (an index
#     access on every table it reads, none on `event`) — never a wall-clock;
#   - the plan matcher is proved on a query that does scan, before its silence is believed;
#   - the backfill gives every live row a title; a mirror write keeps `version`;
#   - the read-only user reads the views and writes nothing.
#
# host-reason: starts its own disposable `dolt sql-server`, same shape as test-lc-hold.sh.
#
# tier: T2
# covers: lifecycle/* spira-lc/src/ops.rs spira-lc/src/cutover.rs spira-lc/src/migrate.rs bead/src/main.rs
# timeout: 240
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/testlib/lc-fixture.sh"

TMP="$(mktemp -d)"
trap 'lcfix_down; rm -rf "$TMP"' EXIT INT TERM
lcfix_up || bail "could not build a lifecycle fixture"
LC_DIR="$HERE/../lifecycle"

export SPIRA_LC_ADMIN_USER=root SPIRA_LC_ADMIN_PASSWORD=""
views="SELECT table_name FROM information_schema.tables WHERE table_schema = 'spira_lifecycle' AND table_type = 'VIEW' ORDER BY 1"
is "a fresh database (schema.sql alone) already has the three views" "ops_live ops_recent ops_round" "$(lcfix_sql -r csv -q "$views" | sed 1d | paste -sd' ')"
lcfix_sql -q "DROP VIEW ops_live; DROP VIEW ops_round; DROP VIEW ops_recent; DROP INDEX bead_state_since_idx ON bead; DROP INDEX batch_state_idx ON batch; ALTER TABLE bead DROP COLUMN title; ALTER TABLE bead DROP COLUMN priority" >/dev/null 2>&1 || bail "could not rebuild the pre-0007 shape"
is "positive control: the pre-0007 store has no views" 0 "$(lcfix_sql -r csv -q "$views" | sed 1d | wc -l)"
out="$(spira-lc admin-migrate "$LC_DIR/migrations" 2>&1)" || bail "admin-migrate failed: $out"
is "migrating the pre-0007 store makes the three views" "ops_live ops_recent ops_round" "$(lcfix_sql -r csv -q "$views" | sed 1d | paste -sd' ')"
again="$(spira-lc admin-migrate "$LC_DIR/migrations" 2>&1)"
wantrc "a second migrate run is a no-op" 0 $?
want "the second run finds nothing pending" "already applied" "$again"

views_of() { python3 -I -c 'import re,sys; t=open(sys.argv[1]).read(); print(re.sub(r"\s+"," ",t[t.index("CREATE "+sys.argv[2]+"VIEW ops_live"):]).replace("CREATE OR REPLACE VIEW","CREATE VIEW"))' "$1" "$2"; }
is "schema.sql and migration 0007 define the same views" "$(views_of "$LC_DIR/migrations/0007-ops-read-model.sql" "")" "$(views_of "$LC_DIR/schema.sql" "OR REPLACE ")"

cols="SELECT column_name FROM information_schema.columns WHERE table_schema = 'spira_lifecycle' AND table_name = 'ops_live' AND column_name IN ('persona','rework') ORDER BY 1"
is "ops_live carries the persona and rework columns the NOW rows render" "persona rework" "$(lcfix_sql -r csv -q "$cols" | sed 1d | paste -sd' ')"

NOW="$(date +%s)"
python3 - "$NOW" > "$TMP/seed.sql" <<'PY'
import sys
now = int(sys.argv[1])
rows = []
for i in range(12000):
    if i < 60:
        state, since = ["OPEN", "READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY"][i % 6], "NULL"
    elif i < 100:
        state, since = "LANDED", now - 3600 * (i % 20)
    elif i % 2:
        state, since = "LANDED", now - 3 * 86400 - i
    else:
        state, since = "DROPPED", "NULL"
    title = "'t%d'" % i if i < 30 else "NULL"
    rows.append("('sp-b%d','%s',NULL,NULL,NULL,NULL,'[]',NULL,1,%s,%s,%s,5,0)" % (i, state, since, title, 2 if i < 30 else "NULL"))
print("INSERT INTO bead (bead_id,state,tip,gate_key,holder,lease_until,holds,reason,version,since,title,priority,updated_at,stack_depth) VALUES")
print(",\n".join(rows) + ";")
ev = ",\n".join("('bead','sp-b%d','Claim','READY','READY','WORKING',1,NULL,'{}','fixture',%d)" % (i % 12000, now) for i in range(100000))
print("INSERT INTO event (machine,lc_key,event,expect,from_state,to_state,applied,refusal,evidence,actor,at) VALUES")
print(ev + ";")
print("INSERT INTO batch (batch_id,repo,state,head,base,version,opened_at) VALUES ('b-open','r','OPEN','h','b',1,%d),('b-old','r','LANDED','h','b',1,%d);" % (now, now - 9 * 86400))
print("INSERT INTO batch_member (batch_id,bead_id,tip) VALUES ('b-open','sp-b1','t1'),('b-open','sp-b2','t2'),('b-old','sp-b3','t3');")
PY
lcfix_sql < "$TMP/seed.sql" >/dev/null 2>"$TMP/seed.err" || bail "seed failed: $(head -c 400 "$TMP/seed.err")"
is "the fixture holds 12,000 beads" 12000 "$(lcfix_sql -r csv -q 'SELECT COUNT(*) FROM bead' | sed -n 2p)"
is "the fixture holds 100,000 events" 100000 "$(lcfix_sql -r csv -q 'SELECT COUNT(*) FROM event' | sed -n 2p)"

plan_of() { lcfix_sql -r csv -q "EXPLAIN FORMAT=TREE $1" 2>&1 | tr -s " \n" " |"; }
scans() { grep -qE '(└─|├─) Table"?\|' <<<"$1"; }
reads() { grep -q "IndexedTableAccess($2)" <<<"$1"; }

scan="$(plan_of "SELECT * FROM bead WHERE title = 'x'")"
want "the plan reader returns a plan" "bead" "$scan"
if scans "$scan"; then ok "positive control: a query on an unindexed column is seen as a scan"; else bad "positive control: a query on an unindexed column is seen as a scan" "$scan"; fi

check_plans() {
    local when="$1" v p t
    for v in ops_live ops_recent ops_round; do
        p="$(plan_of "SELECT * FROM $v")"
        if scans "$p"; then bad "$v scans no table ($when)" "$p"; else ok "$v scans no table ($when)"; fi
        nowant "$v never reads event ($when)" "name: event" "$p"
    done
    for t in bead; do
        for v in ops_live ops_recent; do reads "$(plan_of "SELECT * FROM $v")" $t && ok "$v reads $t by index ($when)" || bad "$v reads $t by index ($when)" "$(plan_of "SELECT * FROM $v")"; done
    done
    p="$(plan_of "SELECT * FROM ops_round")"
    for t in batch batch_member bead; do reads "$p" $t && ok "ops_round reads $t by index ($when)" || bad "ops_round reads $t by index ($when)" "$p"; done
}
check_plans "no statistics"
lcfix_sql -q "ANALYZE TABLE bead, batch, batch_member" >/dev/null 2>&1
check_plans "after ANALYZE"

view_ids() { spira-lc ops-view "$1" | python3 -c 'import json,sys; print(" ".join(sorted(r["bead_id"] for r in json.load(sys.stdin))))'; }
is "ops_live is the 60 non-terminal rows" 60 "$(view_ids ops_live | wc -w)"
is "ops_recent is the landings of the last day" 40 "$(view_ids ops_recent | wc -w)"
is "ops_round is the open batch's members only" "sp-b1 sp-b2" "$(view_ids ops_round)"
want "a view carries the mirrored title" '"title":"t0"' "$(spira-lc ops-view ops_live)"
spira-lc ops-view bead >/dev/null 2>&1; wantrc "ops-view names only the three views" 2 $?

untitled() { lcfix_sql -r csv -q "SELECT COUNT(*) FROM ops_live WHERE title IS NULL" | sed -n 2p; }
before="$(untitled)"
[ "$before" -gt 0 ] && ok "positive control: live rows are untitled before the backfill" || bad "positive control: live rows are untitled before the backfill" "$before"
mkdir -p "$TMP/bin"
cat > "$TMP/bin/bd" <<'STUB'
#!/usr/bin/env bash
[ "$1" = show ] && printf '[{"id":"%s","title":"from bd %s","priority":3}]\n' "$2" "$2"
STUB
chmod +x "$TMP/bin/bd"
tl_config SPIRA_BD="$TMP/bin/bd" SPIRA_DB="" || bail "cannot declare the stub bd"
bf="$(spira-lc backfill-titles 2>&1)"
wantrc "the backfill completes" 0 $?
is "the backfill leaves no live row untitled" 0 "$(untitled)"
is "the backfill leaves no recent landing untitled" 0 "$(lcfix_sql -r csv -q 'SELECT COUNT(*) FROM ops_recent WHERE title IS NULL' | sed -n 2p)"
want "the backfill wrote bd's title" "from bd sp-b40" "$(lcfix_sql -r csv -q "SELECT title FROM bead WHERE bead_id='sp-b40'")"
is "the backfill keeps an existing title" "t0" "$(lcfix_sql -r csv -q "SELECT title FROM bead WHERE bead_id='sp-b0'" | sed -n 2p)"

spira-lc create-bead sp-mirror --title "it's mine" --priority 1 >/dev/null
spira-lc create-bead sp-mirror --priority 0 >/dev/null
row="$(lcfix_sql -r csv -q "SELECT title, priority, version FROM bead WHERE bead_id='sp-mirror'" | sed -n 2p)"
is "a mirror write sets title and priority, keeps an earlier title, and never bumps the version" "it's mine,0,0" "$row"
spira-lc create-bead sp-mirror --priority 9 >/dev/null 2>&1; wantrc "a priority outside 0..4 is refused" 2 $?

sed -e "s/@SPIRA_LC_PASSWORD@/rw-pw/" -e "s/@SPIRA_LC_RO_PASSWORD@/ro-pw/" "$LC_DIR/grants.sql" > "$TMP/grants.sql"
spira-lc admin-apply-ddl "$TMP/grants.sql" >/dev/null 2>"$TMP/grants.err" || bail "grants failed: $(cat "$TMP/grants.err")"
ro() { "$LCFIX_DOLT" --data-dir "$LCFIX_DIR" --host 127.0.0.1 --port "$SPIRA_LC_PORT" -u spira_lc_ro -p ro-pw --no-tls --use-db spira_lifecycle sql "$@" 2>&1; }
is "the read-only user reads a view" 1 "$(ro -r csv -q "SELECT COUNT(*) FROM ops_round WHERE bead_id='sp-b1'" | sed -n 2p)"
for w in "UPDATE bead SET title='x' WHERE bead_id='sp-b1'" "INSERT INTO bead (bead_id,state,holds,version,updated_at) VALUES ('sp-w','READY','[]',0,0)" "DELETE FROM bead WHERE bead_id='sp-b1'" "UPDATE ops_live SET title='x'" "INSERT INTO event (machine,lc_key,event,expect,from_state,to_state,applied,evidence,actor,at) VALUES ('bead','k','e','a','a','a',1,'{}','x',0)"; do
    r="$(ro -q "$w")"; rc=$?
    if [ "$rc" != 0 ] && grep -qi "denied" <<<"$r"; then ok "the read-only user is refused: ${w%% *} ${w:0:40}"; else bad "the read-only user is refused: $w" "rc=$rc $r"; fi
done
is "the refused writes changed nothing" "t1" "$(lcfix_sql -r csv -q "SELECT title FROM bead WHERE bead_id='sp-b1'" | sed -n 2p | sed 's/from bd sp-b1/t1/')"

tl_summary
