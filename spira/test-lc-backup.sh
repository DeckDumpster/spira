#!/usr/bin/env bash
# test-lc-backup.sh — the lifecycle store's off-box backup, its age alarm and its restore drill.
#
# The store is a real dolt database on a throwaway data directory; the destination is a real
# dolt file:// backup. Nothing here touches a live server.
#
# tier: T1
# covers: spira/lc-backup.sh spira/watchers spira/conf.d/SPIRA_LC_BACKUP_* systemd/lc-backup.service systemd/lc-backup.timer install/src/manifest.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
echo "test-lc-backup.sh"

command -v dolt >/dev/null 2>&1 || skip "no dolt on PATH"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export DOLT_ROOT_PATH="$TMP/doltroot"; mkdir -p "$DOLT_ROOT_PATH"
# batch-job: fixture setup against a throwaway local store, no remote
dlt() { dolt "$@"; }
dlt config --global --add user.name t >/dev/null; dlt config --global --add user.email t@t >/dev/null

DATA="$TMP/data"; RUN="$TMP/run"; DEST="$TMP/dest"; mkdir -p "$DATA/spira_lifecycle" "$DATA/spira" "$RUN" "$DEST"
( cd "$DATA/spira_lifecycle" && dlt init >/dev/null \
    && dlt sql -q "CREATE TABLE bead (id INT PRIMARY KEY); CREATE TABLE audit (id INT PRIMARY KEY); INSERT INTO bead VALUES (1),(2),(3); INSERT INTO audit VALUES (1);" \
    && dlt sql -q "CALL DOLT_COMMIT('-Am','seed')" >/dev/null ) || bail "fixture store"
( cd "$DATA/spira" && dlt init >/dev/null \
    && dlt sql -q "CREATE TABLE bead (id INT PRIMARY KEY); INSERT INTO bead VALUES (1),(2);" \
    && dlt sql -q "CALL DOLT_COMMIT('-Am','seed')" >/dev/null ) || bail "fixture content store"

lcb() {
    SPIRA_TOML="$(tl_layer SPIRA_RUN="$RUN" SPIRA_DOLT_DATA="$DATA" SPIRA_LC_BACKUP_URL="${URL-file://$DEST}" \
        SPIRA_LC_BACKUP_MAX_AGE=1000 SPIRA_LC_BACKUP_ALLOW_LOCAL="${LOCALOK-1}" SPIRA_LC_BACKUP_DB="${DBS-spira_lifecycle spira}")" \
        bash "$HERE/lc-backup.sh" "$@" 2>&1
}

echo; echo "refusals — no destination, same disk:"
URL="" lcb run >/dev/null; wantrc "run with no destination refuses" 1 $?
out="$(URL="" lcb age)"; rc=$?; wantrc "age with no destination is red" 1 $rc; want "age names the gap" no-destination "$out"
out="$(URL="" lcb watch --ticks 1)"; want "watcher fires RED on no destination" "RED:" "$out"; want "watcher row records red" red "$(cat "$RUN/watchd/lc-backup.state" 2>/dev/null)"
rm -rf "$RUN/watchd"
out="$(LOCALOK=0 lcb run)"; rc=$?; wantrc "same-disk file:// destination refused" 1 $rc; want "refusal names the exit" ALLOW_LOCAL "$out"

echo; echo "age — stale and fresh fixtures:"
out="$(lcb age)"; rc=$?; wantrc "never backed up is red" 1 $rc; want "never is named" never "$out"
printf '%s deadbeef\n' "$(( $(date +%s) - 5000 ))" > "$RUN/lc-backup.spira_lifecycle.ok"
out="$(DBS=spira_lifecycle lcb age)"; rc=$?; wantrc "stale stamp is red" 1 $rc; want "stale is named" stale "$out"
printf '%s deadbeef\n' "$(( $(date +%s) - 10 ))" > "$RUN/lc-backup.spira_lifecycle.ok"
out="$(DBS=spira_lifecycle lcb age)"; rc=$?; wantrc "fresh stamp is green" 0 $rc; want "fresh is named" ok "$out"
printf 'junk\n' > "$RUN/lc-backup.spira_lifecycle.ok"; DBS=spira_lifecycle lcb age >/dev/null; wantrc "unreadable stamp cannot check (2)" 2 $?
rm -f "$RUN/lc-backup.spira_lifecycle.ok"
printf '%s deadbeef\n' "$(( $(date +%s) - 10 ))" > "$RUN/lc-backup.spira_lifecycle.ok"
out="$(lcb age)"; rc=$?; wantrc "one fresh database does not mask a never-backed-up one" 1 $rc; want "the missing one is named" "spira: never" "$out"
printf '%s deadbeef\n' "$(( $(date +%s) - 10 ))" > "$RUN/lc-backup.spira.ok"
lcb age >/dev/null; wantrc "both fresh is green" 0 $?
printf '%s deadbeef\n' "$(( $(date +%s) - 5000 ))" > "$RUN/lc-backup.spira.ok"
out="$(lcb age)"; rc=$?; wantrc "either stale is red" 1 $rc; want "the stale one is named" "spira: stale" "$out"
rm -f "$RUN"/lc-backup.*.ok

echo; echo "run, then drill — uncommitted rows are carried, counts identical:"
( cd "$DATA/spira_lifecycle" && dlt sql -q "INSERT INTO bead VALUES (4)" )
out="$(lcb run)"; rc=$?; wantrc "run succeeds" 0 $rc;  [ $rc = 0 ] || bad "run output: ${out//$'\n'/ | }"; want "run reports the upload" uploaded "$out"
obj="$(ls "$DEST"/spira_lifecycle/*.tar.* 2>/dev/null | head -1)"
[ -s "$obj" ] && ok "one tar object lands under <db>/<stamp>" || bad "one tar object lands under <db>/<stamp>"
[ -s "$(ls "$DEST"/spira/*.tar.* 2>/dev/null | head -1)" ] && ok "the content store lands under spira/<stamp>" || bad "the content store lands under spira/<stamp>"
[ -s "$RUN/lc-backup.spira.ok" ] && [ -s "$RUN/lc-backup.spira_lifecycle.ok" ] && ok "each database has its own stamp" || bad "each database has its own stamp"
lcb age >/dev/null; wantrc "age green after run" 0 $?
out="$(lcb drill)"; rc=$?; wantrc "drill ok on identical stores" 0 $rc; want "drill reports lifecycle tables" "spira_lifecycle: 2 table(s) identical" "$out"; want "drill restores the content store too" "spira: 1 table(s) identical" "$out"
( cd "$DATA/spira_lifecycle" && dlt sql -q "INSERT INTO bead VALUES (5)" && dlt sql -q "CALL DOLT_COMMIT('-Am','later')" >/dev/null )
lcb drill >/dev/null; wantrc "drill compares AS OF the backed-up commit, so later live writes are not drift" 0 $?
lcb drill "$obj" >/dev/null; wantrc "drill restores a given object" 0 $?
lcb drill --db spira >/dev/null; wantrc "drill restores one named database" 0 $?
( cd "$DATA/spira" && dlt sql -q "INSERT INTO bead VALUES (9); CALL DOLT_COMMIT('-Am','x')" >/dev/null )
out="$(lcb drill)"; rc=$?; wantrc "a later live change to one database is not drift" 0 $rc

echo; echo "drill — positive control: a snapshot of another store is red:"
mkdir -p "$TMP/other" && ( cd "$TMP/other" && mkdir spira_lifecycle snap && cd spira_lifecycle && dlt init >/dev/null \
    && dlt sql -q "CREATE TABLE bead (id INT PRIMARY KEY); CALL DOLT_COMMIT('-Am','x')" >/dev/null && dlt sql -q "CALL DOLT_BACKUP('sync-url','file://$TMP/other/snap')" >/dev/null \
    && tar -C "$TMP/other/snap" -czf "$TMP/other.tar.gz" . )
out="$(lcb drill "$TMP/other.tar.gz")"; rc=$?; wantrc "drill red on another store's snapshot" 1 $rc; want "drill names the head mismatch" "stamped head" "$out"

echo; echo "s3 — one signed PUT, no read verbs:"
cat > "$TMP/sink.py" <<'PYEOF'
import http.server, sys
log = sys.argv[2]
class H(http.server.BaseHTTPRequestHandler):
    def _rec(self):
        n = int(self.headers.get("Content-Length", 0)); self.rfile.read(n)
        open(log, "a").write("%s %s %s %s\n" % (self.command, self.path, (self.headers.get("Authorization") or "")[:16], self.headers.get("x-amz-server-side-encryption")))
        self.send_response(200); self.send_header("Content-Length", "0"); self.end_headers()
    do_PUT = do_GET = do_DELETE = do_HEAD = _rec
    def log_message(self, *a): pass
srv = http.server.HTTPServer(("127.0.0.1", 0), H)
open(sys.argv[1], "w").write(str(srv.server_port))
srv.serve_forever()
PYEOF
python3 "$TMP/sink.py" "$TMP/port" "$TMP/reqs" & SINK=$!
trap 'kill $SINK 2>/dev/null; rm -rf "$TMP"' EXIT
for _ in $(seq 300); do [ -s "$TMP/port" ] && break; sleep 0.1; done
[ -s "$TMP/port" ] || bail "s3 sink did not start"
export AWS_ACCESS_KEY_ID=AKTEST AWS_SECRET_ACCESS_KEY=sk AWS_DEFAULT_REGION=us-west-2 AWS_ENDPOINT_URL_S3="http://127.0.0.1:$(cat "$TMP/port")"
rm -f "$RUN"/lc-backup.*.ok
out="$(URL="s3://bkt/spira/" lcb run)"; rc=$?; wantrc "s3 run succeeds" 0 $rc; [ $rc = 0 ] || bad "s3 run output: ${out//$'\n'/ | }"
[ "$(wc -l < "$TMP/reqs")" = 2 ] && ok "one request per database" || bad "one request per database"
grep -q '^PUT /bkt/spira/spira/[0-9TZ]*\.tar\.[a-z]* AWS4-HMAC-SHA256 AES256$' "$TMP/reqs" && grep -q '^PUT /bkt/spira/spira_lifecycle/[0-9TZ]*\.tar\.[a-z]* AWS4-HMAC-SHA256 AES256$' "$TMP/reqs" && ok "signed PUT of <prefix><db>/<stamp>.tar.* with SSE" || bad "signed PUT shape: $(cat "$TMP/reqs")"
lcb age >/dev/null; wantrc "age green after s3 run" 0 $?
rm -f "$RUN"/lc-backup.*.ok; out="$(unset AWS_SECRET_ACCESS_KEY; URL="s3://bkt/spira/" lcb run)"; rc=$?
wantrc "s3 run without credentials fails" 1 $rc
[ -z "$(ls "$RUN"/lc-backup.*.ok 2>/dev/null)" ] && ok "failed upload writes no stamp" || bad "failed upload writes no stamp"

echo; echo "units and rows:"
grep -q '^ExecStart=@SPIRA_HOME@/lc-backup.sh run' "$HERE/../systemd/lc-backup.service" && ok "service runs the backup" || bad "service runs the backup"
grep -q '^ExecStartPost=@SPIRA_HOME@/lc-backup.sh drill' "$HERE/../systemd/lc-backup.service" && ok "service drills after the backup" || bad "service drills"
grep -q '^Unit=lc-backup.service' "$HERE/../systemd/lc-backup.timer" && ok "timer starts the service" || bad "timer starts the service"
grep -q 'lc-backup.timer", true' "$HERE/../install/src/manifest.rs" && ok "manifest enables the timer" || bad "manifest enables the timer"
grep -q '^lc-backup|daemon|' "$HERE/watchers" && ok "age alarm has a watcher row" || bad "age alarm has a watcher row"
tl_summary
