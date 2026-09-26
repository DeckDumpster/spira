#!/usr/bin/env bash
#
# test-ready.sh — ready.sh checks: sentinel timer, world halt, database, ready work,
# loom probe, agent presence, and cockpit panes — each branch exercised fail-first.
#
#   ./test-ready.sh
#
# SEAMS USED
# ----------
#   SPIRA_SYSTEMCTL   — fake systemctl in $BIN (hermetic: no real systemd)
#   SPIRA_BD          — fake bd in $BIN (hermetic: no real database)
#   SPIRA_HOME        — temp dir carrying fake sentinel.sh and seed.sh
#   SPIRA_LOOM_PROBE  — stub for the Loom HTTP probe
#   SPIRA_AGENT       — path to fake or absent agent binary
#   SPIRA_RUN         — temp dir (world.halted stamp lives here)
#   PATH              — $BIN first, for fake tmux
#
# FAIL-FIRST: every check's failing branch is exercised before the passing branch is
# trusted. A check that only tests absence is indistinguishable from one pointed at the
# wrong thing (law-absence-needs-a-positive-control).
#
# covers: spira/ready.sh spira/conf.sh
# covers: spira/world.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0; skip=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
skipped() { skip=$((skip+1)); printf '  skip  %s\n' "$1"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
BIN="$TMP/bin"; mkdir -p "$BIN"
FAKE_HOME="$TMP/fakehome"
mkdir -p "$FAKE_HOME/.config/systemd/user"
FAKE_SPIRA_HOME="$TMP/fakespira"
mkdir -p "$FAKE_SPIRA_HOME"
RUN="$TMP/run"; mkdir -p "$RUN"
DB="$TMP/db"

# ---------------------------------------------------------------------------
# Shared infrastructure
# ---------------------------------------------------------------------------

# Fake systemctl — controlled by FAKE_SC_ENABLED and FAKE_SC_ACTIVE env vars.
# FAKE_SC_ENABLED: unit name that reports as enabled.
# FAKE_SC_ACTIVE:  unit name that reports as active.
# Both are independent: a unit can be enabled-but-inactive (timer found but not running).
cat > "$BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
unit=""
for a in "$@"; do
    case "$a" in
        spira-*|dolt-*) unit="$a" ;;
    esac
done
if [[ "$*" == *"is-active"* ]]; then
    active="${FAKE_SC_ACTIVE:-}"
    if [[ -n "$active" ]] && [[ " $active " == *" $unit "* ]]; then
        [[ "$*" == *"--quiet"* ]] && exit 0
        printf 'active\n'; exit 0
    else
        [[ "$*" == *"--quiet"* ]] && exit 1
        printf 'inactive\n'; exit 1
    fi
fi
if [[ "$*" == *"is-enabled"* ]]; then
    enabled="${FAKE_SC_ENABLED:-}"
    if [[ -n "$enabled" ]] && [[ " $enabled " == *" $unit "* ]]; then
        printf 'enabled\n'; exit 0
    fi
    exit 1
fi
exit 0
MOCK
chmod +x "$BIN/systemctl"

# Fake bd — controlled by FAKE_BD_LIST (JSON) and FAKE_BD_RC env vars.
cat > "$BIN/bd" <<'MOCK'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*"--json"*)
        rc="${FAKE_BD_RC:-0}"
        if [ "$rc" != "0" ]; then
            printf 'error: cannot read database\n'; exit 1
        fi
        printf '%s\n' "${FAKE_BD_LIST:-[]}"
        exit 0 ;;
    *"list"*) exit 0 ;;
    *"ready"*) printf '[]'; exit 0 ;;
    *) exit 0 ;;
esac
MOCK
chmod +x "$BIN/bd"

# Fake seed.sh — controlled by FAKE_SEED_MISSING (number of missing statutes, 0=none).
# Output format matches real seed.sh --list: "slug + " (present) or "slug - " (missing).
# ready.sh counts lines ending in ' -' (space-hyphen at end of line).
cat > "$FAKE_SPIRA_HOME/seed.sh" <<'MOCK'
#!/usr/bin/env bash
missing="${FAKE_SEED_MISSING:-0}"
if [ "${1:-}" = "--list" ]; then
    printf 'law-always-present + \n'
    if [ "$missing" -gt 0 ]; then
        printf 'law-missing-one -\n'
        [ "$missing" -gt 1 ] && printf 'law-missing-two -\n'
    fi
    exit 0
fi
exit 0
MOCK
chmod +x "$FAKE_SPIRA_HOME/seed.sh"

# Fake sentinel.sh — controlled by FAKE_SENTINEL_BEADS (space-separated bead IDs) and
# FAKE_SENTINEL_RC. Each bead ID is printed on its own line, indented with two spaces,
# matching sentinel.sh's real output format (ready.sh counts lines starting with '^  ').
cat > "$FAKE_SPIRA_HOME/sentinel.sh" <<'MOCK'
#!/usr/bin/env bash
if [ "${1:-}" = "--report" ]; then
    rc="${FAKE_SENTINEL_RC:-0}"
    if [ "$rc" != "0" ]; then
        printf 'ERROR: database unavailable\n' >&2; exit 1
    fi
    printf '\nOpen beads under sp-test:\n'
    beads="${FAKE_SENTINEL_BEADS:-}"
    for b in $beads; do
        printf '  %s\n' "$b"
    done
    exit 0
fi
exit 1
MOCK
chmod +x "$FAKE_SPIRA_HOME/sentinel.sh"

# Loom probe — controlled by FAKE_LOOM_RESULT.
cat > "$BIN/loom-probe" <<'MOCK'
#!/usr/bin/env bash
printf '%s\n' "${FAKE_LOOM_RESULT:-200 42ms}"
MOCK
chmod +x "$BIN/loom-probe"

# Fake tmux — controlled by FAKE_TMUX_PANES (list-panes output) and
# FAKE_TMUX_SERVER_UP (default 1; set to 0 to simulate no server — list-panes exits 1).
# Use ${VAR-default} (no colon) so empty string means "no panes", not "use default".
cat > "$BIN/tmux" <<'MOCK'
#!/usr/bin/env bash
if [[ "$*" == *"list-panes"* ]]; then
    [[ "${FAKE_TMUX_SERVER_UP:-1}" = "0" ]] && exit 1
    panes="${FAKE_TMUX_PANES-panel %1
health %2}"
    [ -n "$panes" ] && printf '%s\n' "$panes"
    exit 0
fi
exit 0
MOCK
chmod +x "$BIN/tmux"

# Fake agent binary.
cat > "$BIN/fake-agent" <<'MOCK'
#!/usr/bin/env bash
printf 'fake-agent v0\n'
MOCK
chmod +x "$BIN/fake-agent"

# A sentinel unit file in the fake home to satisfy the predates-install check.
UNIT_DIR="$FAKE_HOME/.config/systemd/user"
printf '[Timer]\nOnBootSec=2min\n' > "$UNIT_DIR/spira-sentinel-prod.timer"

# run_ready: run ready.sh with test fixtures.
# Accepts extra env vars as KEY=value arguments before the final "--".
# Exit status is preserved; callers that don't care should add `|| true`.
run_ready() {
    local extra_env=()
    while [[ "${1:-}" != "--" && $# -gt 0 ]]; do
        extra_env+=("$1"); shift
    done
    [ "${1:-}" = "--" ] && shift
    env -i \
        PATH="$BIN:/usr/local/bin:/usr/bin:/bin" \
        HOME="$FAKE_HOME" \
        SPIRA_PATH="$BIN" \
        SPIRA_CONF="$TMP/no.conf" \
        SPIRA_HOME="$FAKE_SPIRA_HOME" \
        SPIRA_REPO="$TMP" \
        SPIRA_REPO_MAP="$TMP/no-map" \
        SPIRA_RUN="$RUN" \
        SPIRA_DB="$DB" \
        SPIRA_SYSTEMCTL="$BIN/systemctl" \
        SPIRA_BD="$BIN/bd" \
        SPIRA_GOAL="sp-test" \
        SPIRA_INSTANCE="prod" \
        SPIRA_LOOM_BIN="$BIN/fake-loom" \
        SPIRA_LOOM_ADDR="127.0.0.1:8788" \
        SPIRA_LOOM_BUDGET_MS="1500" \
        SPIRA_LOOM_PROBE="$BIN/loom-probe" \
        SPIRA_AGENT="fake-agent" \
        "${extra_env[@]}" \
        bash "$HERE/ready.sh" 2>/dev/null
}

# Create a fake loom binary so the loom binary-existence check passes.
touch "$BIN/fake-loom" && chmod +x "$BIN/fake-loom"
# Create .beads directory to simulate a present database.
mkdir -p "$DB/.beads"

echo "test-ready.sh"
echo

# ===========================================================================
# SENTINEL TIMER
# ===========================================================================
echo "--- sentinel timer ---"

# POSITIVE CONTROL: sentinel timer inactive → FAIL before trusting the pass case.
# FAKE_SC_ENABLED=... makes the unit "found" by spira_unit; FAKE_SC_ACTIVE unset means
# it is found but not active, which is the case that produces the FAIL.
echo "positive control: sentinel timer inactive"
out="$(run_ready "FAKE_SC_ENABLED=spira-sentinel-prod.timer" "FAKE_SC_ACTIVE=" -- || true)"
want "sentinel-fail: FAIL line present"          "  FAIL  sentinel timer not active" "$out"
nowant "sentinel-fail: no pass line"             "  pass  sentinel timer active"     "$out"

# PASS: sentinel timer active.
echo "sentinel timer active"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" --)"
want "sentinel-pass: pass line present"          "  pass  sentinel timer active"       "$out"
want "sentinel-pass: unit name shown"            "spira-sentinel-prod.timer"            "$out"
nowant "sentinel-pass: no FAIL line"             "  FAIL  sentinel timer"               "$out"

# UNKN: unit not found (spira_unit returns ?).
echo "sentinel timer unknown (unit not found)"
out="$(run_ready "FAKE_SC_ACTIVE=" "FAKE_SC_ENABLED=" -- || true)"
# When neither is-enabled nor is-active matches for any unit form, spira_unit returns ?
want "sentinel-unkn: ? line present"  "  ?     sentinel timer" "$out"

# ===========================================================================
# WORLD HALT
# ===========================================================================
echo ""
echo "--- world halt ---"

# POSITIVE CONTROL: stamp exists, newer than unit → FAIL (operator halt).
echo "positive control: world halted by operator"
touch "$RUN/world.halted"
# Make stamp newer than the unit file (touch sets mtime to now; unit file was created earlier).
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" -- || true)"
want "world-halt-op: FAIL line present"         "  FAIL  world halted"       "$out"
want "world-halt-op: stamp path shown"          "world.halted"               "$out"
want "world-halt-op: mtime shown"               "mtime"                      "$out"
nowant "world-halt-op: no PREDATES"             "PREDATES"                   "$out"
rm -f "$RUN/world.halted"

# Stamp predates install: make stamp older than unit file.
echo "world halted before install (stamp predates unit)"
# Back-date the stamp to an old time; ensure unit file is newer by touching it after.
touch "$RUN/world.halted"
touch -t "202001010000.00" "$RUN/world.halted" 2>/dev/null || true
# Recreate unit file so it is newer than the back-dated stamp.
printf '[Timer]\nOnBootSec=2min\n' > "$UNIT_DIR/spira-sentinel-prod.timer"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" -- || true)"
want "world-predates: FAIL with PREDATES"       "PREDATES"                   "$out"
want "world-predates: stamp path shown"         "world.halted"               "$out"
rm -f "$RUN/world.halted"

# PASS: no stamp.
echo "world not halted"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" --)"
want "world-pass: pass line present"            "  pass  world not halted"   "$out"
nowant "world-pass: no FAIL line"               "  FAIL  world halted"       "$out"

# ===========================================================================
# DATABASE
# ===========================================================================
echo ""
echo "--- database ---"

# POSITIVE CONTROL: database unreadable → FAIL before trusting the pass case.
echo "positive control: database unreadable"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=1" --)"
want "db-fail: FAIL database unreadable"        "  FAIL  database unreadable"  "$out"
nowant "db-fail: no pass for database"          "  pass  database readable"    "$out"

# PASS: database readable, bead count shown.
echo "database readable, no beads"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" --)"
want "db-pass: pass database readable"          "  pass  database readable"    "$out"
want "db-pass: bead count shown"                "0 bead(s)"                    "$out"

# Database: some beads.
echo "database readable, several beads"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=0" \
                 'FAKE_BD_LIST=[{"id":"sp-1"},{"id":"sp-2"},{"id":"sp-3"}]' --)"
want "db-count: 3 bead(s)"                      "3 bead(s)"                    "$out"

# Database: bd fails and dolt-beads.service active but port not yet open → UNKN (starting).
echo "database: dolt-beads active, port not open — starting up"
# Ports are pinned into the IANA ephemeral range (49152-65535) so this suite does not
# collide with a real registered service on the box.
_ready_port1=$((49152 + ($$ % 8000)))
_ready_port2=$((_ready_port1 + 1))

_dolt_data_dir="$TMP/dolt-data-ready"
mkdir -p "$_dolt_data_dir"
printf 'listener:\n  port: %s\n' "$_ready_port1" > "$_dolt_data_dir/dolt-server.yaml"
out="$(run_ready "FAKE_SC_ACTIVE=dolt-beads.service" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=1" \
                 "SPIRA_DOLT_DATA=$_dolt_data_dir" -- || true)"
want "db-starting: ? line present"        "  ?     database: dolt-beads.service active" "$out"
want "db-starting: names the port"        "port $_ready_port1"                           "$out"
want "db-starting: says server is starting" "server is starting"                         "$out"
nowant "db-starting: no FAIL unreadable"  "  FAIL  database unreadable"                  "$out"
unset _dolt_data_dir

# Database: bd fails and dolt-beads.service active but port IS open → FAIL unreadable.
echo "database: dolt-beads active, port IS open — still fails"
_dolt_data_dir2="$TMP/dolt-data-ready2"
mkdir -p "$_dolt_data_dir2"
printf 'listener:\n  port: %s\n' "$_ready_port2" > "$_dolt_data_dir2/dolt-server.yaml"
_nc_pid=""
if command -v nc >/dev/null 2>&1; then
    nc -lk "$_ready_port2" >/dev/null 2>&1 & _nc_pid=$!
    sleep 0.1
fi
out="$(run_ready "FAKE_SC_ACTIVE=dolt-beads.service" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=1" \
                 "SPIRA_DOLT_DATA=$_dolt_data_dir2" -- || true)"
[ -n "$_nc_pid" ] && { kill "$_nc_pid" 2>/dev/null; wait "$_nc_pid" 2>/dev/null || true; }
if [ -n "$_nc_pid" ]; then
    want "db-portopen: FAIL unreadable (port open)"  "  FAIL  database unreadable" "$out"
    nowant "db-portopen: no ? starting"              "server is starting"           "$out"
else
    skipped "db-portopen: nc unavailable — skip port-open case"
fi
unset _dolt_data_dir2 _nc_pid

# Database: bd fails with no SPIRA_DOLT_DATA → FAIL unreadable (no starting check).
echo "database: bd fails, no SPIRA_DOLT_DATA"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=1" -- || true)"
want "db-no-doltdata: FAIL unreadable"     "  FAIL  database unreadable" "$out"
nowant "db-no-doltdata: no ? starting"     "server is starting"          "$out"

# Statutes: missing → WARN.
echo "statutes: some missing"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
                 "FAKE_SEED_MISSING=2" --)"
want "statutes-warn: WARN for missing"          "  WARN  2 shipped statute"    "$out"
nowant "statutes-warn: no all-present pass"     "all shipped statutes in force" "$out"

# Statutes: all present → PASS.
echo "statutes: all present"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
                 "FAKE_SEED_MISSING=0" --)"
want "statutes-pass: pass for all statutes"     "  pass  all shipped statutes in force" "$out"

# ===========================================================================
# READY WORK
# ===========================================================================
echo ""
echo "--- ready work ---"

# POSITIVE CONTROL: sentinel.sh --report fails → ? before trusting the pass case.
echo "positive control: sentinel.sh --report fails"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
                 "FAKE_SENTINEL_RC=1" --)"
want "ready-unkn: ? for sentinel failure"       "  ?     ready work"           "$out"
nowant "ready-unkn: no pass"                    "  pass  sentinel sees"        "$out"

# WARN: no open work.
echo "no open work"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
                 "FAKE_SENTINEL_RC=0" "FAKE_SENTINEL_BEADS=" --)"
want "ready-warn: WARN for no work"             "  WARN  sentinel sees no open work" "$out"
nowant "ready-warn: no pass line"               "  pass  sentinel sees"              "$out"

# PASS: open work listed.
echo "open work present"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
                 "FAKE_SENTINEL_RC=0" \
                 "FAKE_SENTINEL_BEADS=  sp-abc  sp-def" --)"
want "ready-pass: pass line present"            "  pass  sentinel sees"        "$out"
want "ready-pass: count shown"                  "2 open bead"                  "$out"

# ===========================================================================
# LOOM
# ===========================================================================
echo ""
echo "--- loom ---"

# POSITIVE CONTROL: loom does not answer → FAIL.
echo "positive control: loom not answering"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
                 "FAKE_LOOM_RESULT=ERR 5000ms Connection refused" --)"
want "loom-fail: FAIL line present"             "  FAIL  loom does not answer" "$out"
nowant "loom-fail: no pass line"                "  pass  loom answers"         "$out"

# PASS: loom answers 200 within budget.
echo "loom answers 200"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
                 "FAKE_LOOM_RESULT=200 42ms" --)"
want "loom-pass: pass line present"             "  pass  loom answers 200"     "$out"
want "loom-pass: url shown"                     "/api/beads"                   "$out"
want "loom-pass: ms shown"                      "42ms"                         "$out"

# WARN: loom answers 200 but over budget.
echo "loom answers 200 but over budget"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
                 "SPIRA_LOOM_BUDGET_MS=100" \
                 "FAKE_LOOM_RESULT=200 500ms" --)"
want "loom-over-budget: WARN line present"      "  WARN  loom answers 200 but over budget" "$out"

# SKIP: loom binary absent AND loom unit not installed (installer deliberately skipped it).
# Binary absent + no unit = cargo was absent; the loop is unaffected; exit 0.
echo "loom binary absent, unit not installed"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
                 "SPIRA_LOOM_BIN=/nonexistent/loom" --)"
want "loom-skip: skip line present"             "  skip  loom not installed"   "$out"
nowant "loom-skip: no ? line"                   "  ?     loom"                 "$out"
nowant "loom-skip: no WARN line"                "  WARN  loom not installed"   "$out"
nowant "loom-skip: no FAIL line"                "  FAIL  loom"                 "$out"

# UNKN: loom binary absent but loom unit IS installed (something wrong — binary should exist).
echo "loom binary absent, unit installed"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer spira-loom-prod.service" \
                 "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
                 "SPIRA_LOOM_BIN=/nonexistent/loom" -- || true)"
want "loom-unkn: ? line present"                "  ?     loom"                 "$out"
nowant "loom-unkn: no WARN line"                "  WARN  loom not installed"   "$out"
nowant "loom-unkn: no FAIL"                     "  FAIL  loom"                 "$out"

# All cases above go through SPIRA_LOOM_PROBE, the fixture stub — they never reach the
# real python probe's retry loop. The grace cases below unset the stub (SPIRA_LOOM_PROBE=)
# so the loop in ready.sh itself is what is under test.
# Ports are pinned into the IANA ephemeral range so this suite does not collide with a
# real registered service on the box.
_grace_port=$((49152 + (($$ + 3) % 8000)))

# POSITIVE CONTROL: nothing ever listens -> FAIL, bounded by SPIRA_LOOM_READY_GRACE and
# not by the 20s default (sp-bn9go: a slow control here would mean the grace argument
# is being ignored).
echo "positive control: loom never answers, real probe -> FAIL within the configured grace"
_t0=$(date +%s)
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
                 "SPIRA_LOOM_PROBE=" \
                 "SPIRA_LOOM_ADDR=127.0.0.1:$_grace_port" \
                 "SPIRA_LOOM_READY_GRACE=1" -- || true)"
_elapsed=$(( $(date +%s) - _t0 ))
want "loom-grace-fail: FAIL line present"       "  FAIL  loom does not answer" "$out"
if [ "$_elapsed" -lt 10 ]; then
    ok "loom-grace-fail: bounded by grace, not the 20s default (${_elapsed}s)"
else
    bad "loom-grace-fail: bounded by grace, not the 20s default" "took ${_elapsed}s"
fi

# PASS: a loom that starts late is retried until it answers, not failed on the first
# cold probe — this is the defect sp-bn9go fixes.
echo "loom starts late, real probe -> retried until it answers"
cat > "$TMP/grace-server.py" <<'PYEOF'
import http.server, socketserver, sys, time
port = int(sys.argv[1])
time.sleep(float(sys.argv[2]))
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
    def log_message(self, *a):
        pass
socketserver.TCPServer.allow_reuse_address = True
with socketserver.TCPServer(("127.0.0.1", port), H) as httpd:
    httpd.serve_forever()
PYEOF
python3 "$TMP/grace-server.py" "$_grace_port" 1 >/dev/null 2>&1 & _grace_pid=$!
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
                 "SPIRA_LOOM_PROBE=" \
                 "SPIRA_LOOM_ADDR=127.0.0.1:$_grace_port" \
                 "SPIRA_LOOM_READY_GRACE=8" --)"
kill "$_grace_pid" 2>/dev/null; wait "$_grace_pid" 2>/dev/null || true
want "loom-grace-pass: pass line present"       "  pass  loom answers 200" "$out"
unset _grace_port _grace_pid _t0 _elapsed

# ===========================================================================
# AGENT
# ===========================================================================
echo ""
echo "--- agent ---"

# Snapshot must exist so the cockpit freshness check does not emit a FAIL that
# would trip the broad nowant "  FAIL" assertions below.
printf 'SP_AT=0\n' > "$RUN/cockpit.env"

# POSITIVE CONTROL: agent absent → WARN with "absent" before trusting present case.
echo "positive control: agent absent"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
                 "SPIRA_AGENT=nonexistent-agent-xyz" --)"
want "agent-absent: WARN absent"                "WARN  agent absent"           "$out"
want "agent-absent: names the agent"            "nonexistent-agent-xyz"        "$out"
nowant "agent-absent: no FAIL"                  "  FAIL"                       "$out"

# Agent present → WARN with "present".
echo "agent present"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
                 "SPIRA_AGENT=fake-agent" --)"
want "agent-present: WARN present"              "WARN  agent present"          "$out"
want "agent-present: names the agent"           "fake-agent"                   "$out"
# Agent is WARN even when present — the loop is armed but summons may still fail.
nowant "agent-present: no pass line for agent"  "  pass  agent"                "$out"

# ===========================================================================
# COCKPIT — snapshot freshness
# ===========================================================================
echo ""
echo "--- cockpit snapshot freshness ---"

# POSITIVE CONTROL: snapshot absent → FAIL before trusting the present/fresh case.
echo "positive control: snapshot absent"
rm -f "$RUN/cockpit.env"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
                 "FAKE_TMUX_PANES=panel %1
health %2" -- || true)"
want "snap-absent: FAIL for missing snapshot"   "  FAIL  no cockpit snapshot"          "$out"
nowant "snap-absent: no pass for snapshot"      "  pass  cockpit snapshot fresh"        "$out"

# Snapshot stale → FAIL.
echo "snapshot stale"
printf 'SP_AT=0\n' > "$RUN/cockpit.env"
touch -d "300 seconds ago" "$RUN/cockpit.env"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
                 "SPIRA_SNAP_STALE_S=60" \
                 "FAKE_TMUX_PANES=panel %1
health %2" -- || true)"
want "snap-stale: FAIL for stale snapshot"      "  FAIL  cockpit snapshot stale"        "$out"
want "snap-stale: age shown"                    "s ago"                                 "$out"
nowant "snap-stale: no pass for snapshot"       "  pass  cockpit snapshot fresh"         "$out"

# Snapshot fresh → pass.
echo "snapshot fresh"
printf 'SP_AT=0\n' > "$RUN/cockpit.env"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
                 "SPIRA_SNAP_STALE_S=60" \
                 "FAKE_TMUX_PANES=panel %1
health %2" --)"
want "snap-fresh: pass for fresh snapshot"      "  pass  cockpit snapshot fresh"         "$out"
nowant "snap-fresh: no FAIL"                    "  FAIL  cockpit snapshot"               "$out"

# ===========================================================================
# COCKPIT — panes
# ===========================================================================
echo ""
echo "--- cockpit panes ---"

# Ensure snapshot exists for all remaining cockpit tests.
printf 'SP_AT=0\n' > "$RUN/cockpit.env"

# POSITIVE CONTROL: both panes absent → WARN before trusting present case.
echo "positive control: cockpit panes absent"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
                 "FAKE_TMUX_PANES=" --)"
want "cockpit-absent: WARN for panel"           "WARN  cockpit panel pane absent"  "$out"
want "cockpit-absent: WARN for health"          "WARN  cockpit health pane absent" "$out"
nowant "cockpit-absent: no pass for panel"      "  pass  cockpit panel"            "$out"

# Both panes present → pass.
echo "cockpit panes present"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
                 "FAKE_TMUX_PANES=panel %1
health %2" --)"
want "cockpit-pass: pass for panel"             "  pass  cockpit panel pane present"  "$out"
want "cockpit-pass: pass for health"            "  pass  cockpit health pane present" "$out"
want "cockpit-pass: pane id shown panel"        "%1"                                  "$out"
want "cockpit-pass: pane id shown health"       "%2"                                  "$out"

# ===========================================================================
# COCKPIT — not installed (no unit AND no tmux server → skip, not FAIL)
# ===========================================================================
echo ""
echo "--- cockpit not installed ---"

# POSITIVE CONTROL: cockpit unit enabled, snapshot absent → FAIL (not skip).
# Verifies the guard fires before trusting the skip path.
echo "positive control: cockpit unit enabled, snapshot absent"
rm -f "$RUN/cockpit.env"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer spira-cockpit-prod.service" \
                 "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
                 "FAKE_TMUX_SERVER_UP=0" -- || true)"
want "ckp-no-inst-ctrl: FAIL for missing snapshot"  "  FAIL  no cockpit snapshot"  "$out"
nowant "ckp-no-inst-ctrl: no skip"                  "  skip  cockpit"              "$out"

# SKIP: no cockpit unit and no tmux server → skip, ready.sh exit 0.
echo "cockpit not installed: no unit, no tmux server"
rm -f "$RUN/cockpit.env"
out="$(run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
                 "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
                 "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
                 "FAKE_TMUX_SERVER_UP=0" --)"
want "ckp-no-inst: skip line present"               "  skip  cockpit"              "$out"
nowant "ckp-no-inst: no FAIL for snapshot"          "  FAIL  no cockpit snapshot"  "$out"

# Restore snapshot for subsequent tests.
printf 'SP_AT=0\n' > "$RUN/cockpit.env"

# ===========================================================================
# EXIT CODE
# ===========================================================================
echo ""
echo "--- exit code ---"

# Exit 1 when FAIL is present.
echo "exit 1 when sentinel timer is inactive"
run_ready "FAKE_SC_ACTIVE=" -- >/dev/null 2>&1 && bad "exit-fail: should exit 1 on FAIL" "exited 0" || ok "exit-fail: exits 1 on FAIL"

# Exit 1 when ? is present (binary absent but loom unit installed → UNKN).
echo "exit 1 when check unknown"
run_ready "SPIRA_LOOM_BIN=/nonexistent/loom" \
          "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
          "FAKE_SC_ENABLED=spira-sentinel-prod.timer spira-loom-prod.service" \
          "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" -- >/dev/null 2>&1 \
    && bad "exit-unkn: should exit 1 on ?" "exited 0" \
    || ok "exit-unkn: exits 1 on ?"

# Exit 0 when cockpit not installed (no unit + no tmux server → skip, not error).
echo "exit 0 when cockpit not installed"
rm -f "$RUN/cockpit.env"
run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
          "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
          "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
          "FAKE_TMUX_SERVER_UP=0" -- >/dev/null 2>&1 \
    && ok "exit-ckp-skip: exits 0 when cockpit not installed" \
    || bad "exit-ckp-skip: should exit 0 when cockpit not installed" "exited non-zero"
printf 'SP_AT=0\n' > "$RUN/cockpit.env"

# Exit 0 when loom not installed (binary absent + no unit → skip, not an error).
echo "exit 0 when loom not installed"
run_ready "SPIRA_LOOM_BIN=/nonexistent/loom" \
          "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
          "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
          "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
          "FAKE_TMUX_PANES=panel %1
health %2" -- >/dev/null 2>&1 \
    && ok "exit-loom-warn: exits 0 when loom not installed" \
    || bad "exit-loom-warn: should exit 0 when loom not installed" "exited non-zero"

# Exit 0 when all pass/warn.
echo "exit 0 when armed"
run_ready "FAKE_SC_ACTIVE=spira-sentinel-prod.timer" \
          "FAKE_SC_ENABLED=spira-sentinel-prod.timer" \
          "FAKE_BD_RC=0" "FAKE_BD_LIST=[]" \
          "FAKE_LOOM_RESULT=200 42ms" -- >/dev/null 2>&1 \
    && ok "exit-pass: exits 0 when armed" \
    || bad "exit-pass: should exit 0 when armed" "exited non-zero"

# ===========================================================================
printf '\ntest-ready: %d ok, %d fail, %d skip\n' "$pass" "$fail" "$skip"
[ "$fail" -eq 0 ]
