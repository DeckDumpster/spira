#!/usr/bin/env bash
#
# test-doctor-store.sh — doctor.sh's doctor_check_store: is the store listener reachable.
#
#   ./test-doctor-store.sh
#
# MERGED (test-plan-2026-09-23) from test-doctor-dolt.sh, test-doctor-missing-store.sh and
# test-doctor-embedded.sh, which each drove the same function through a different slice of
# its inputs. test-doctor-installing.sh's dolt-dir/service cases are folded in here too; its
# home-repo-row case tested a section doctor.sh no longer has (repositories moved out of
# runtime health) and is dropped, not carried forward.
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. POSITIVE CONTROL. Missing store, inactive service, and embedded mode each FAIL before
#    the corresponding passing case is trusted.
# 2. MISSING STORE. No .beads: FAIL naming the store; SPIRA_DOCTOR_INSTALLING=1 downgrades
#    to WARN naming phase 3. The FAIL/WARN text never suggests a bare `bd init`, which
#    creates an embedded store — not the one the harness needs.
# 3. MANAGED DOLT SERVER. SPIRA_DOLT_DATA set: directory + dolt-server.yaml + active service
#    all ok; a missing directory or inactive service FAILs (WARN under INSTALLING, naming
#    phases 3 and 4). bd unable to read an existing store while the service is inactive FAILs
#    the same way, and also downgrades to WARN under INSTALLING — a prior install left the
#    store on disk with its service stopped, which is exactly what install.sh's own preflight
#    sees on a reinstall.
# 4. UNMANAGED DOLT SERVER. SPIRA_DOLT_DATA empty: a live TCP listener at the recorded
#    host:port is ok; nothing answering is a FAIL naming the host:port. No metadata at all
#    is "managed independently", not a fault.
# 5. EMBEDDED MODE. dolt_mode=embedded in metadata.json FAILs, naming the serialised-lock
#    cost and the SPIRA_DOLT_DATA remedy.
#
# tier: T1
# covers: spira/doctor.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-doctor-store.sh"
SRV_PID=""
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"; [ -n "$SRV_PID" ] && kill "$SRV_PID" 2>/dev/null; true' EXIT INT TERM

BIN="$TMP/bin"
mkdir -p "$BIN" "$TMP/run" "$TMP/home"

make_bd() {
    local list_result="${1:-ok}"   # "ok" or "fail"
    cat > "$BIN/bd" <<MOCK
#!/usr/bin/env bash
case "\$*" in
    *"list"*"--limit"*|*"list"*"--json"*)
        [ "$list_result" = ok ] && { printf '[]\n'; exit 0; } || exit 1 ;;
    *) exit 0 ;;
esac
MOCK
    chmod +x "$BIN/bd"
}
make_bd ok

make_systemctl() {
    local dolt_result="${1:-active}"   # "active" or "inactive"
    cat > "$BIN/systemctl" <<MOCK
#!/usr/bin/env bash
case "\$*" in
    *"is-active"*"--quiet"*"dolt-beads"*)
        [ "$dolt_result" = active ] && exit 0 || exit 1 ;;
    *) exit 0 ;;
esac
MOCK
    chmod +x "$BIN/systemctl"
}
make_systemctl active

run_doctor() {
    local extra="${1:-}"
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$BIN" \
        SPIRA_SYSTEMCTL="$BIN/systemctl" \
        SPIRA_BD="$BIN/bd" \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_INSTANCE=prod \
        SPIRA_DOLT_DATA="${SPIRA_DOLT_DATA_OVERRIDE-}" \
        ${extra} \
        bash "$HERE/doctor.sh" 2>/dev/null
}
store_section() { sed -n '/^store$/,/^$/p' <<< "$1"; }

# ==========================================================================
echo
echo "1. POSITIVE CONTROL — missing store without SPIRA_DOCTOR_INSTALLING FAILs:"
# ==========================================================================
rm -rf "$TMP/db"
ctrl_out="$(SPIRA_DOLT_DATA_OVERRIDE= run_doctor || true)"
ctrl_store="$(store_section "$ctrl_out")"
want   "positive control: FAIL on missing store" "has no .beads" "$ctrl_store"
nowant "positive control: no bare bd init suggested" "bd init" "$ctrl_store"

# ==========================================================================
echo
echo "2. INSTALLING — missing store downgrades to WARN naming phase 3:"
# ==========================================================================
inst_out="$(SPIRA_DOLT_DATA_OVERRIDE= run_doctor "SPIRA_DOCTOR_INSTALLING=1" || true)"
inst_store="$(store_section "$inst_out")"
want   "installing: WARN names phase 3" "phase 3" "$inst_store"
nowant "installing: no FAIL for missing store" "FAIL" "$inst_store"

# ==========================================================================
echo
echo "3. MANAGED DOLT SERVER — positive control: inactive service FAILs:"
# ==========================================================================
mkdir -p "$TMP/db/.beads"
DOLT_DIR="$TMP/dolt-data"
mkdir -p "$DOLT_DIR"
printf 'port: 3306\n' > "$DOLT_DIR/dolt-server.yaml"
make_systemctl inactive
inactive_out="$(SPIRA_DOLT_DATA_OVERRIDE="$DOLT_DIR" run_doctor || true)"
inactive_store="$(store_section "$inactive_out")"
want "managed: inactive service FAILs"        "FAIL"          "$inactive_store"
want "managed: names dolt-beads.service"      "dolt-beads.service" "$inactive_store"

# ==========================================================================
echo
echo "4. MANAGED DOLT SERVER — directory, yaml, active service: all ok, no FAIL:"
# ==========================================================================
make_systemctl active
managed_out="$(SPIRA_DOLT_DATA_OVERRIDE="$DOLT_DIR" run_doctor || true)"
managed_store="$(store_section "$managed_out")"
want   "managed: dolt data directory ok" "dolt data directory" "$managed_store"
want   "managed: dolt-server.yaml ok"    "dolt-server.yaml"    "$managed_store"
want   "managed: service active ok"      "dolt-beads.service is active" "$managed_store"
nowant "managed: no FAIL"                "FAIL" "$managed_store"

# ==========================================================================
echo
echo "5. MANAGED DOLT SERVER — missing directory FAILs, naming the path:"
# ==========================================================================
missing_out="$(SPIRA_DOLT_DATA_OVERRIDE="$TMP/does-not-exist" run_doctor || true)"
missing_store="$(store_section "$missing_out")"
want "missing dir: FAIL" "FAIL" "$missing_store"
want "missing dir: names the path" "does-not-exist" "$missing_store"

# ==========================================================================
echo
echo "6. INSTALLING — missing dir and inactive service both downgrade to WARN:"
# ==========================================================================
make_systemctl inactive
inst2_out="$(SPIRA_DOLT_DATA_OVERRIDE="$TMP/does-not-exist" run_doctor "SPIRA_DOCTOR_INSTALLING=1" || true)"
inst2_store="$(store_section "$inst2_out")"
want   "installing: dolt-dir WARN names phase 3" "phase 3" "$inst2_store"
want   "installing: service WARN names phase 4" "phase 4" "$inst2_store"
nowant "installing: no FAIL at all" "FAIL" "$inst2_store"

# ==========================================================================
echo
echo "7. POSITIVE CONTROL — bd cannot read an existing store, service inactive, FAILs:"
# ==========================================================================
make_bd fail
inactive_read_out="$(SPIRA_DOLT_DATA_OVERRIDE="$DOLT_DIR" run_doctor || true)"
inactive_read_store="$(store_section "$inactive_read_out")"
want "positive control: bd-cannot-read FAILs" "FAIL  bd cannot read" "$inactive_read_store"

# ==========================================================================
echo
echo "8. INSTALLING — bd cannot read (server not yet started) downgrades to WARN:"
# ==========================================================================
inst3_out="$(SPIRA_DOLT_DATA_OVERRIDE="$DOLT_DIR" run_doctor "SPIRA_DOCTOR_INSTALLING=1" || true)"
inst3_store="$(store_section "$inst3_out")"
want   "installing: bd-cannot-read WARN names phase 4" "bd cannot read $TMP/db — dolt-beads.service not active; install.sh will start it in phase 4" "$inst3_store"
nowant "installing: no FAIL for bd-cannot-read" "FAIL  bd cannot read" "$inst3_store"
make_bd ok
make_systemctl active

# ==========================================================================
echo
echo "9. UNMANAGED SERVER — no metadata at all: managed independently, no FAIL:"
# ==========================================================================
rm -f "$TMP/db/.beads/metadata.json"
make_systemctl active
unmanaged_out="$(SPIRA_DOLT_DATA_OVERRIDE= run_doctor || true)"
unmanaged_store="$(store_section "$unmanaged_out")"
want   "unmanaged: managed independently ok" "managed independently" "$unmanaged_store"
nowant "unmanaged: no FAIL" "FAIL" "$unmanaged_store"

# ==========================================================================
echo
echo "10. UNMANAGED SERVER — a live listener at the recorded host:port is ok:"
# ==========================================================================
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
listening_out="$(SPIRA_DOLT_DATA_OVERRIDE= run_doctor || true)"
listening_store="$(store_section "$listening_out")"
want   "unmanaged listening: OK names the port" "server answering on" "$listening_store"
nowant "unmanaged listening: no FAIL" "FAIL" "$listening_store"
kill "$SRV_PID" 2>/dev/null; wait "$SRV_PID" 2>/dev/null; SRV_PID=""

# ==========================================================================
echo
echo "11. UNMANAGED SERVER — nothing answering FAILs, naming host:port:"
# ==========================================================================
DEAD_PORT="$(python3 -c "
import socket; s = socket.socket()
s.bind(('127.0.0.1', 0))
print(s.getsockname()[1])
s.close()
")"
printf '{"dolt_mode":"server","dolt_database":"db","dolt_server_host":"127.0.0.1","dolt_server_port":%s,"project_id":"test-ec2t"}\n' \
    "$DEAD_PORT" > "$TMP/db/.beads/metadata.json"
dead_out="$(SPIRA_DOLT_DATA_OVERRIDE= run_doctor || true)"
dead_store="$(store_section "$dead_out")"
want "not answering: FAIL" "no server answers" "$dead_store"
want "not answering: names host:port" "127.0.0.1:$DEAD_PORT" "$dead_store"

# ==========================================================================
echo
echo "12. EMBEDDED MODE — positive control: FAILs naming cost and remedy:"
# ==========================================================================
printf '{"dolt_mode":"embedded","dolt_database":"db","project_id":"test-ec2t"}\n' \
    > "$TMP/db/.beads/metadata.json"
embedded_out="$(SPIRA_DOLT_DATA_OVERRIDE= run_doctor || true)"
embedded_store="$(store_section "$embedded_out")"
want "embedded: FAIL" "FAIL" "$embedded_store"
want "embedded: names the cost" "one lock" "$embedded_store"
want "embedded: names the remedy" "SPIRA_DOLT_DATA" "$embedded_store"

echo
tl_summary
