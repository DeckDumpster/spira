#!/usr/bin/env bash
#
# test-doctor-embedded.sh — doctor FAILs when the production store is in embedded mode.
#
#   ./test-doctor-embedded.sh
#
# WHAT THIS TESTS
# ---------------
# 1. POSITIVE CONTROL. A store with dolt_mode=embedded in .beads/metadata.json must
#    trigger a FAIL before the passing case can be trusted (law-a-matcher-reads-code-not-prose).
# 2. SERVER MODE, SERVER ANSWERING. With dolt_mode=server and a live listener, no FAIL is emitted.
# 3. CONTENT OF EMBEDDED FAIL. The FAIL names the cost (serialised lock) and the remedy.
# 4. UNMANAGED SERVER NOT ANSWERING. When SPIRA_DOLT_DATA is empty and the configured port has
#    no listener, doctor FAILs naming the host:port — "managed independently" is checked, not assumed.
#
# covers: spira/doctor.sh
# covers: spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in output"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in output"; }

echo "test-doctor-embedded.sh"
SRV_PID=""
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"; [ -n "$SRV_PID" ] && kill "$SRV_PID" 2>/dev/null; true' EXIT INT TERM

BIN="$TMP/bin"
mkdir -p "$BIN" "$TMP/db/.beads" "$TMP/run" "$TMP/home"

cat > "$BIN/bd" <<'FAKESCRIPT'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*"--limit"*) printf '[]\n'; exit 0 ;;
    *) exit 0 ;;
esac
FAKESCRIPT
chmod +x "$BIN/bd"

cat > "$BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
case "$*" in
    *"is-active"*"--quiet"*) exit 0 ;;
    *"is-active"*)           printf 'active\n' ;;
    *"is-enabled"*)          printf 'enabled\n'; exit 0 ;;
esac
exit 0
MOCK
chmod +x "$BIN/systemctl"

touch "$TMP/watchers-empty"

run_doctor() {
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$BIN" \
        SPIRA_SYSTEMCTL="$BIN/systemctl" \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_INSTANCE=prod \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_REPO_MAP=/nonexistent \
        SPIRA_NOTIFY=/nonexistent \
        SPIRA_WATCHERS="$TMP/watchers-empty" \
        SPIRA_DOLT_DATA="" \
        bash "$HERE/doctor.sh" 2>/dev/null
}

# ==========================================================================
echo
echo "positive control — embedded mode triggers a FAIL:"
# ==========================================================================
# Plant dolt_mode=embedded. If this case does not produce a FAIL, the check
# cannot fire and the passing cases below prove nothing.
printf '{"dolt_mode":"embedded","dolt_database":"db","project_id":"test-ec2t"}\n' \
    > "$TMP/db/.beads/metadata.json"

embedded_out="$(run_doctor || true)"
want "embedded mode triggers a FAIL" "FAIL" \
    "$(printf '%s\n' "$embedded_out" | grep -i 'embedded' || true)"
want "FAIL names the cost: serialised lock" "one lock" "$embedded_out"
want "FAIL names the remedy: SPIRA_DOLT_DATA" "SPIRA_DOLT_DATA" "$embedded_out"

# ==========================================================================
echo
echo "server mode — server answering — no FAIL:"
# ==========================================================================
# Start a listener so the port check passes; proves the OK fires only when a
# server is actually present (positive control for the pass case).
mkfifo "$TMP/srv-port"
python3 -c "
import socket
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(('127.0.0.1', 0))
s.listen(5)
with open('$TMP/srv-port', 'w') as f:
    f.write(str(s.getsockname()[1]))
while True:
    c,_ = s.accept(); c.close()
" &
SRV_PID=$!
SRV_PORT=$(cat "$TMP/srv-port")
printf '{"dolt_mode":"server","dolt_database":"db","dolt_server_host":"127.0.0.1","dolt_server_port":%d,"project_id":"test-ec2t"}\n' \
    "$SRV_PORT" > "$TMP/db/.beads/metadata.json"

server_out="$(run_doctor || true)"
nowant "server answering: no embedded FAIL" "embedded" \
    "$(printf '%s\n' "$server_out" | grep 'FAIL' || true)"
want "server answering: OK line names the port" "server answering on" "$server_out"
kill "$SRV_PID" 2>/dev/null; wait "$SRV_PID" 2>/dev/null; SRV_PID=""

# ==========================================================================
echo
echo "unmanaged server, not answering — FAIL:"
# ==========================================================================
# Bind to port 0 to get a free port from the OS, then close immediately.
# A hardcoded port is the observed source of flakiness: if something on the
# host is listening there, doctor prints OK and the FAIL assertion fails.
DEAD_PORT="$(python3 -c "
import socket; s = socket.socket()
s.bind(('127.0.0.1', 0))
print(s.getsockname()[1])
s.close()
")"
printf '{"dolt_mode":"server","dolt_database":"db","dolt_server_host":"127.0.0.1","dolt_server_port":%s,"project_id":"test-ec2t"}\n' \
    "$DEAD_PORT" > "$TMP/db/.beads/metadata.json"

unmanaged_out="$(run_doctor || true)"
want "not answering: FAIL emitted" "FAIL" \
    "$(printf '%s\n' "$unmanaged_out" | grep -i 'no server answers' || true)"
want "not answering: FAIL names the host and port" "127.0.0.1:$DEAD_PORT" "$unmanaged_out"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
