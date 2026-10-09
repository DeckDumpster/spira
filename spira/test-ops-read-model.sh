#!/usr/bin/env bash
#
# test-ops-read-model.sh — the lifecycle read model the panes read instead of scanning bd:
# the ops_live / ops_round / ops_recent views, the where-stuck views (ops_edges, ops_dwell and
# its p95 helper) with the legal-edge graph, the title and priority mirrored into the bead
# row, and the read-only user. Against a real Dolt, on a store of 12,000 beads and 100,000
# events.
#
#   - each view answers through `spira-lc ops-view`, and its plan is asserted (an index
#     access on every table it reads, none on `event`) — never a wall-clock;
#   - the plan matcher is proved on a query that does scan, before its silence is believed;
#   - where-stuck: a clean fixture shows no stuck rows; replays of each stuck case
#     (CERTIFIED with no way out, WORKING orphaned, IN_DELIVERY stranded) produce exactly
#     those rows; refusals and edge rates land in ops_edges; ops-graph is the machine's table;
#   - the backfill gives every live row a title; a mirror write keeps `version`;
#   - the read-only user reads the views and writes nothing.
#
# host-reason: starts its own disposable `dolt sql-server`, same shape as test-lc-hold.sh.
#
# tier: T2
# covers: lifecycle/* loom/src/stuck.rs spira-lc/src/ops.rs spira-lc/src/cutover.rs spira-lc/src/migrate.rs bead/src/main.rs
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
ALL_VIEWS="ops_dwell ops_dwell_p95 ops_edges ops_live ops_recent ops_round"
is "a fresh database (schema.sql alone) already has every view" "$ALL_VIEWS" "$(lcfix_sql -r csv -q "$views" | sed 1d | paste -sd' ')"
lcfix_sql -q "DROP VIEW ops_dwell; DROP VIEW ops_dwell_p95; DROP VIEW ops_edges; DROP INDEX event_at_idx ON event; DROP VIEW ops_live; DROP TABLE bead_dep; DROP VIEW ops_round; DROP VIEW ops_recent; DROP INDEX bead_state_since_idx ON bead; DROP INDEX batch_state_idx ON batch; ALTER TABLE bead DROP COLUMN title; ALTER TABLE bead DROP COLUMN priority" >/dev/null 2>&1 || bail "could not rebuild the pre-0007 shape"
is "positive control: the pre-0007 store has no views" 0 "$(lcfix_sql -r csv -q "$views" | sed 1d | wc -l)"
out="$(spira-lc admin-migrate "$LC_DIR/migrations" 2>&1)" || bail "admin-migrate failed: $out"
is "migrating the pre-0007 store makes every view" "$ALL_VIEWS" "$(lcfix_sql -r csv -q "$views" | sed 1d | paste -sd' ')"
is "migrating restores the window index" 1 "$(lcfix_sql -r csv -q "SELECT COUNT(DISTINCT index_name) FROM information_schema.statistics WHERE table_schema='spira_lifecycle' AND index_name='event_at_idx'" | sed -n 2p)"
again="$(spira-lc admin-migrate "$LC_DIR/migrations" 2>&1)"
wantrc "a second migrate run is a no-op" 0 $?
want "the second run finds nothing pending" "already applied" "$again"

views_of() { python3 -I -c 'import re,sys; t=re.sub(r"--[^\n]*","",open(sys.argv[1]).read()); print(re.sub(r"\s+"," ",t[t.index("CREATE "+sys.argv[2]+"VIEW "+sys.argv[3]):]).replace("CREATE OR REPLACE VIEW","CREATE VIEW").strip())' "$1" "$2" "$3"; }
is "schema.sql and migrations 0007, 0009 and 0011 define the same views" "$(views_of "$LC_DIR/migrations/0011-bead-dep.sql" "OR REPLACE " ops_live) $(views_of "$LC_DIR/migrations/0007-ops-read-model.sql" "" ops_round) $(views_of "$LC_DIR/migrations/0009-where-stuck.sql" "" ops_edges)" "$(views_of "$LC_DIR/schema.sql" "OR REPLACE " ops_live)"
is "migrating the pre-0007 store gives ops_live its claimable and blocker" "blocker claimable" "$(lcfix_sql -r csv -q "SELECT column_name FROM information_schema.columns WHERE table_name = 'ops_live' AND column_name IN ('claimable','blocker') ORDER BY 1" | sed 1d | paste -sd' ')"

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
ev = ",\n".join("('bead','sp-b%d','Claim','READY','READY','WORKING',1,NULL,'{}','fixture',%d)" % (i % 12000, now if i % 20 == 0 else now - 20 * 86400) for i in range(100000))
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
    reads "$(plan_of "SELECT * FROM ops_live")" bead_dep && ok "ops_live reads bead_dep by index ($when)" || bad "ops_live reads bead_dep by index ($when)" "$(plan_of "SELECT * FROM ops_live")"
    p="$(plan_of "SELECT * FROM ops_round")"
    for t in batch batch_member bead; do reads "$p" $t && ok "ops_round reads $t by index ($when)" || bad "ops_round reads $t by index ($when)" "$p"; done
}
window_index="index: [event.machine,event.applied,event.at,"
lookup_index="index: [event.machine,event.applied,event.lc_key,event.to_state,event.at]"
check_stuck_plans() {
    local when="$1" v p
    for v in ops_edges ops_dwell_p95 ops_dwell; do
        p="$(plan_of "SELECT * FROM $v")"
        if scans "$p"; then bad "$v scans no table ($when)" "$p"; else ok "$v scans no table ($when)"; fi
    done
    p="$(plan_of "SELECT * FROM ops_edges")"
    want "ops_edges takes its window from the event time index ($when)" "$window_index" "$p"
    p="$(plan_of "SELECT * FROM ops_dwell_p95")"
    want "the p95 helper takes its window from the event time index ($when)" "$window_index" "$p"
    p="$(plan_of "SELECT * FROM ops_dwell")"
    want "ops_dwell looks each bead's entry up by key in the event index ($when)" "$lookup_index" "$p"
    reads "$p" bead && ok "ops_dwell reads bead by index ($when)" || bad "ops_dwell reads bead by index ($when)" "$p"
    p="$(plan_of "$(spira-lc ops-gantt --print-sql)")"
    if scans "$p"; then bad "the stuck page's gantt query scans no table ($when)" "$p"; else ok "the stuck page's gantt query scans no table ($when)"; fi
    want "the gantt query takes its window from the event time index ($when)" "$window_index" "$p"
    p="$(plan_of "SELECT seq, event, from_state, to_state, applied, refusal, evidence, actor, at FROM event WHERE machine = 'bead' AND lc_key = 'sp-1' ORDER BY seq")"
    if scans "$p"; then bad "a bead's timeline query scans no table ($when)" "$p"; else ok "a bead's timeline query scans no table ($when)"; fi
    p="$(plan_of "SELECT bead_id, state, holder FROM bead WHERE bead_id = 'sp-1'")"
    reads "$p" bead && ok "a bead's row is read by key ($when)" || bad "a bead's row is read by key ($when)" "$p"
}
check_plans "no statistics"
check_stuck_plans "no statistics"
lcfix_sql -q "ANALYZE TABLE bead, batch, batch_member" >/dev/null 2>&1
check_plans "after ANALYZE"
lcfix_sql -q "ANALYZE TABLE event" >/dev/null 2>&1
check_stuck_plans "after ANALYZE"

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
[ "$1 $2 $3" = "dep list sp-b9" ] && printf '[{"depends_on_id":"sp-b8","type":"blocks"},{"depends_on_id":"sp-b3","type":"parent-child"}]\n'
[ "$1 $2" = "dep list" ] && [ "$3" != sp-b9 ] && echo '[]'
exit 0
STUB
chmod +x "$TMP/bin/bd"
tl_config SPIRA_BD="$TMP/bin/bd" SPIRA_DB="" || bail "cannot declare the stub bd"
bf="$(spira-lc backfill-titles 2>&1)"
wantrc "the backfill completes" 0 $?
is "the backfill leaves no live row untitled" 0 "$(untitled)"
is "the backfill leaves no recent landing untitled" 0 "$(lcfix_sql -r csv -q 'SELECT COUNT(*) FROM ops_recent WHERE title IS NULL' | sed -n 2p)"
want "the backfill wrote bd's title" "from bd sp-b40" "$(lcfix_sql -r csv -q "SELECT title FROM bead WHERE bead_id='sp-b40'")"
is "the backfill keeps an existing title" "t0" "$(lcfix_sql -r csv -q "SELECT title FROM bead WHERE bead_id='sp-b0'" | sed -n 2p)"

live_row() { spira-lc ops-view ops_live | python3 -I -c 'import json,sys; r=[x for x in json.load(sys.stdin) if x["bead_id"]==sys.argv[1]]; print(r[0]["claimable"], r[0]["blocker"]) if r else print("absent")' "$1"; }
is "positive control: no edges before the backfill, so a READY bead is claimable" "1 None" "$(live_row sp-b1)"
bd_out="$(spira-lc backfill-deps 2>&1)"
wantrc "the dependency backfill completes" 0 $?
is "the backfill mirrors the edges bd lists, every type" 2 "$(lcfix_sql -r csv -q 'SELECT COUNT(*) FROM bead_dep' | sed -n 2p)"
is "a blocks edge to a live bead makes the dependent unclaimable and names the blocker" "0 sp-b8" "$(live_row sp-b9)"
lcfix_sql -q "DELETE FROM bead_dep WHERE depends_on = 'sp-b3'" >/dev/null; spira-lc backfill-deps >/dev/null 2>&1
is "a second backfill finds edges mirrored and leaves the store alone" 1 "$(lcfix_sql -r csv -q 'SELECT COUNT(*) FROM bead_dep' | sed -n 2p)"
spira-lc backfill-deps --force >/dev/null 2>&1
is "--force reads bd again" 2 "$(lcfix_sql -r csv -q 'SELECT COUNT(*) FROM bead_dep' | sed -n 2p)"

spira-lc create-bead sp-A --priority 1 >/dev/null; spira-lc create-bead sp-B --priority 1 >/dev/null
spira-lc dep-add sp-B sp-A >/dev/null; wantrc "dep-add mirrors an edge" 0 $?
is "A blocks B: B is blocked by A and not claimable" "0 sp-A" "$(live_row sp-B)"
is "A itself stays claimable" "1 None" "$(live_row sp-A)"
spira-lc dep-add sp-B sp-A >/dev/null; wantrc "dep-add is idempotent" 0 $?
lcfix_sql -q "UPDATE bead SET state='LANDED', since=UNIX_TIMESTAMP() WHERE bead_id='sp-A'" >/dev/null
is "when A lands, B becomes claimable with no blocker" "1 None" "$(live_row sp-B)"
lcfix_sql -q "UPDATE bead SET state='READY', since=NULL WHERE bead_id='sp-A'" >/dev/null
is "positive control: a blocker that is live again blocks again" "0 sp-A" "$(live_row sp-B)"
lc_blocked() { spira-lc list --ids "$1" | python3 -I -c 'import json,sys; print(",".join(json.load(sys.stdin)[0]["blocked_by"]) or "none")'; }
lc_show_blocked() { spira-lc show "$1" | python3 -I -c 'import json,sys; print(",".join(json.load(sys.stdin)["bead"]["blocked_by"]) or "none")'; }
is "list carries the unlanded blocker as blocked_by" "sp-A" "$(lc_blocked sp-B)"
is "show carries the same blocked_by" "sp-A" "$(lc_show_blocked sp-B)"
is "a bead nothing blocks has an empty blocked_by" "none" "$(lc_blocked sp-A)"
lcfix_sql -q "UPDATE bead SET state='LANDED' WHERE bead_id='sp-A'" >/dev/null
is "a landed blocker drops out of blocked_by" "none" "$(lc_blocked sp-B)"
lcfix_sql -q "UPDATE bead SET state='READY' WHERE bead_id='sp-A'" >/dev/null
spira-lc dep-remove sp-B sp-A >/dev/null; wantrc "dep-remove drops the edge" 0 $?
is "a removed edge no longer blocks" "1 None" "$(live_row sp-B)"
spira-lc dep-add sp-B sp-A --type relates-to >/dev/null
is "a non-blocks edge is recorded and never blocks" "1 None" "$(live_row sp-B)"
lcfix_sql -q "UPDATE bead SET holds='[\"ask\"]' WHERE bead_id='sp-B'" >/dev/null
is "a held bead is not claimable" "0 None" "$(live_row sp-B)"
spira-lc dep-add sp-B "not a key" >/dev/null 2>&1; wantrc "dep-add refuses a non-key" 2 $?

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

# --- where-stuck: replay each case on a store wiped to a known history -------------------------------
lcfix_sql -q "DELETE FROM batch_member; DELETE FROM batch; DELETE FROM event; DELETE FROM bead" >/dev/null 2>&1 || bail "could not wipe the store for the where-stuck replays"
NOW="$(date +%s)"
ws_seed() {
python3 - "$NOW" "$1" <<'PY'
import sys
now, phase = int(sys.argv[1]), sys.argv[2]
beads, events = [], []
def bead(i, state):
    beads.append("('%s','%s','[]',1,%d)" % (i, state, now))
def ev(i, name, a, b, at, applied=1, refusal=None):
    events.append("('bead','%s','%s','%s','%s','%s',%d,%s,'{}','fixture',%d)" % (i, name, a, a, b, applied, "'%s'" % refusal if refusal else "NULL", at))
if phase == "history":
    for i in range(20):
        k, T = "ws-h%d" % i, now - 4 * 3600 - i * 60
        bead(k, "LANDED")
        ev(k, "Claim", "READY", "WORKING", T - 3000)
        ev(k, "Release", "WORKING", "READY", T - 1200)
        ev(k, "Claim", "READY", "WORKING", T - 1140 + i)
        ev(k, "Submit", "WORKING", "SUBMITTED", T - 100)
        ev(k, "GatePass", "SUBMITTED", "CERTIFIED", T)
        ev(k, "Deliver", "CERTIFIED", "IN_DELIVERY", T + 600 + i)
        ev(k, "Delivered", "IN_DELIVERY", "LANDED", T + 900 + 2 * i)
    for state, frm in [("READY", None), ("WORKING", "READY"), ("SUBMITTED", "WORKING"), ("CERTIFIED", "SUBMITTED"), ("IN_DELIVERY", "CERTIFIED")]:
        k = "ws-fresh-" + state.lower()
        bead(k, state)
        if frm: ev(k, "Fresh", frm, state, now - 30)
        else:
            ev(k, "Claim", "READY", "WORKING", now - 3000); ev(k, "Release", "WORKING", "READY", now - 30)
elif phase == "stuck":
    for k, state, frm, ago in [("sp-2cah6", "CERTIFIED", "SUBMITTED", 20 * 3600), ("sp-penudq", "CERTIFIED", "SUBMITTED", 23 * 3600),
                               ("sp-h4umcj", "WORKING", "READY", 30 * 3600), ("ws-strand", "IN_DELIVERY", "CERTIFIED", 10 * 3600)]:
        bead(k, state)
        ev(k, "Fresh", frm, state, now - ago)
    ev("sp-2cah6", "GateRed", "CERTIFIED", "CERTIFIED", now - 600, 0, "IllegalTransition")
    ev("sp-2cah6", "GateRed", "CERTIFIED", "CERTIFIED", now - 300, 0, "IllegalTransition")
    ev("sp-penudq", "GateRed", "CERTIFIED", "CERTIFIED", now - 7200, 0, "IllegalTransition")
print("INSERT INTO bead (bead_id,state,holds,version,updated_at) VALUES\n" + ",\n".join(beads) + ";")
print("INSERT INTO event (machine,lc_key,event,expect,from_state,to_state,applied,refusal,evidence,actor,at) VALUES\n" + ",\n".join(events) + ";")
PY
}
ws_seed history | lcfix_sql >/dev/null 2>"$TMP/ws.err" || bail "history seed failed: $(head -c 400 "$TMP/ws.err")"

dwell_rows() { spira-lc ops-view ops_dwell | python3 -I -c '
import json, sys
now = int(sys.argv[1])
rows = json.load(sys.stdin)
stuck = sorted((r["bead_id"], r["state"]) for r in rows if r["p95_s"] is not None and int(r["entered_at"]) < now - int(r["p95_s"]))
print(len(rows), " ".join("%s:%s" % s for s in stuck))' "$(date +%s)"; }
p95_of() { spira-lc ops-view ops_dwell | python3 -I -c 'import json,sys; print({r["state"]: int(r["p95_s"]) for r in json.load(sys.stdin) if r["p95_s"] is not None}.get(sys.argv[1]))' "$1"; }

is "the measured p95 of CERTIFIED is the 19th of 20 observed dwells" 618 "$(p95_of CERTIFIED)"
is "the measured p95 of IN_DELIVERY is the 19th of 20 observed dwells" 318 "$(p95_of IN_DELIVERY)"
is "the measured p95 of SUBMITTED" 100 "$(p95_of SUBMITTED)"
is "the measured p95 of READY comes from the re-readied dwells" 78 "$(p95_of READY)"
is "the measured p95 of WORKING takes the longer of its two kinds of dwell" 1800 "$(p95_of WORKING)"
is "a clean fixture lists its five live beads and shows no stuck row" "5 " "$(dwell_rows)"

ws_seed stuck | lcfix_sql >/dev/null 2>"$TMP/ws.err" || bail "stuck seed failed: $(head -c 400 "$TMP/ws.err")"
is "each stuck case is flagged, and nothing else" "9 sp-2cah6:CERTIFIED sp-h4umcj:WORKING sp-penudq:CERTIFIED ws-strand:IN_DELIVERY" "$(dwell_rows)"
entered="$(spira-lc ops-view ops_dwell | python3 -I -c 'import json,sys; print({r["bead_id"]: int(r["entered_at"]) for r in json.load(sys.stdin)}["sp-penudq"])')"
is "a bead's time in its state starts at the event that entered it" "$((NOW - 23 * 3600))" "$entered"

edge() { spira-lc ops-view ops_edges | python3 -I -c '
import json, sys
k, f, t, ev = sys.argv[1:5]
for r in json.load(sys.stdin):
    if r["kind"] == k and r["from_state"] == f and (r["to_state"] or "") == t and (r["event"] or "") == ev:
        print(int(r["n_1h"]), int(r["n_24h"])); break
else:
    print("none")' "$@"; }
is "applied SUBMITTED to CERTIFIED: 20 history, 2 stuck and 1 fresh in the day, the fresh one in the hour" "1 23" "$(edge applied SUBMITTED CERTIFIED "")"
is "applied READY to WORKING counts both claims of each of 20 beads, and two fresh" "2 42" "$(edge applied READY WORKING "")"
is "applied WORKING to READY counts the 20 releases and the fresh one" "1 21" "$(edge applied WORKING READY "")"
is "refused events by state and kind: two in the hour, three in the day" "2 3" "$(edge refused CERTIFIED "" GateRed)"
is "an applied event that stays in its state is not an edge" "none" "$(edge applied CERTIFIED CERTIFIED "")"
refused_only="$(spira-lc ops-view ops_edges | python3 -I -c 'import json,sys; print(sorted({r["refusal"] for r in json.load(sys.stdin) if r["kind"]=="refused"}))')"
is "a refusal carries its reason" "['IllegalTransition']" "$refused_only"

graph="$(spira-lc ops-graph)"
gedge() { python3 -I -c '
import json, sys
f, e, t = sys.argv[2:5]
print(any(x["from"] == f and x["event"] == e and x["to"] == t for x in json.loads(sys.argv[1])))' "$graph" "$@"; }
is "the graph has the claim edge, spelled as the state column spells it" True "$(gedge READY Claim WORKING)"
is "the graph has the gate edge" True "$(gedge SUBMITTED GatePass CERTIFIED)"
is "the graph carries the legal exit from CERTIFIED to REWORK on a red gate" True "$(gedge CERTIFIED GateRed REWORK)"
is "the graph has no exit from a terminal state" 0 "$(python3 -I -c 'import json,sys; print(sum(x["from"] in ("LANDED","DROPPED","SUPERSEDED","DONE") for x in json.loads(sys.argv[1])))' "$graph")"

tl_summary
