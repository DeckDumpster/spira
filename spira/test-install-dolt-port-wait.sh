#!/usr/bin/env bash
#
# test-install-dolt-port-wait.sh — install.sh phase 4 waits for dolt-beads.service
# to listen on its port before proceeding, and times out with a named message when
# the port never opens; the phase 3 port-close wait prevents a collision between
# the dying temporary server and the starting unit.
#
#   ./test-install-dolt-port-wait.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. POSITIVE CONTROL (timeout). When dolt-beads.service never listens within
#    the wait window, install.sh exits 2 and names the port and the timeout.
#    Without this, the passing case below proves nothing.
# 2. PORT-OPEN WAIT PASSES. When the port opens within the wait window (simulated
#    by a delayed nc), install.sh reaches phase 7 (exits 3, not 2; phase 4 cleared).
# 3. PORT-CLOSE WAIT. When the temporary server holds the port after the kill,
#    install.sh waits for it to close before starting the unit, and the install
#    succeeds once the port is released and the unit listener opens.
#
# SEAMS
#   SPIRA_INSTALL_DOLT_WAIT      — max seconds to wait for port open (default 60)
#   SPIRA_INSTALL_DOLT_CLOSE_WAIT — max seconds to wait for port close (default 30)
#
# tier: T1
# covers: install.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
REAL_REPO="$(cd "$HERE/.." && pwd -P)"
is2()     { [ "$2" = 2 ] && ok "$1" || bad "$1" "wanted exit 2, got $2"; }
is3()     { [ "$2" = 3 ] && ok "$1" || bad "$1" "wanted exit 3 (installed-not-ready), got $2"; }

echo "test-install-dolt-port-wait.sh"

if ! command -v nc >/dev/null 2>&1; then
    printf '  skip  nc not available — all TCP-probe tests require nc\n'
    printf '\n1 skipped (nc absent)\n'
    exit 0
fi

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# ---------------------------------------------------------------------------
# Fixture layout (mirrors test-install-dolt-mode.sh).
# ---------------------------------------------------------------------------
FIXTURE="$TMP/harness"
SPIRA_DIR="$FIXTURE/spira"
SYSTEMD_DIR="$FIXTURE/systemd"
COCKPIT_DIR="$FIXTURE/cockpit"
mkdir -p "$SPIRA_DIR" "$SYSTEMD_DIR" "$COCKPIT_DIR"

for f in "$HERE/../systemd/"*.service "$HERE/../systemd/"*.timer; do
    [ -e "$f" ] || continue
    ln -s "$f" "$SYSTEMD_DIR/$(basename "$f")" 2>/dev/null || true
done
ln -s "$HERE/../systemd/install.sh" "$SYSTEMD_DIR/install.sh"
ln -s "$HERE/../systemd/units.sh"   "$SYSTEMD_DIR/units.sh"
ln -s "$HERE/conf.sh"   "$SPIRA_DIR/conf.sh"
ln -s "$HERE/lib.sh"    "$SPIRA_DIR/lib.sh"
ln -s "$HERE/watchd.sh" "$SPIRA_DIR/watchd.sh"
printf '# empty\n' > "$SPIRA_DIR/watchers"
printf '# empty\n' > "$SPIRA_DIR/repo-map.example"
mkdir -p "$SPIRA_DIR/statutes"

cat > "$SPIRA_DIR/doctor.sh" <<'EOF'
#!/usr/bin/env bash
echo "spira doctor"; echo "  ok    stub — all checks passed"; exit 0
EOF
chmod +x "$SPIRA_DIR/doctor.sh"

cat > "$SPIRA_DIR/configure.sh" <<'EOF'
#!/usr/bin/env bash
_out="${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.conf"
if [ -f "$_out" ]; then printf 'configure: config exists: %s\n' "$_out"; exit 1; fi
mkdir -p "$(dirname "$_out")"
printf 'SPIRA_PROD = %s\n' "${CONFIGURE_PROD:-/nonexistent}" > "$_out"
printf 'configure: wrote %s\n' "$_out"
EOF
chmod +x "$SPIRA_DIR/configure.sh"

cat > "$SPIRA_DIR/build.sh" <<'EOF'
#!/usr/bin/env bash
printf 'build.sh: stub\n'; exit 0
EOF
chmod +x "$SPIRA_DIR/build.sh"

cat > "$SPIRA_DIR/seed.sh" <<'EOF'
#!/usr/bin/env bash
printf 'seed: stub\n'; exit 0
EOF
chmod +x "$SPIRA_DIR/seed.sh"

cat > "$SPIRA_DIR/install-session-hook.sh" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
    status)  printf 'ok  SessionStart\n' ;;
    install) printf 'install-session-hook: stub\n' ;;
esac
exit 0
EOF
chmod +x "$SPIRA_DIR/install-session-hook.sh"

cat > "$SPIRA_DIR/install-intake.sh" <<'EOF'
#!/usr/bin/env bash
printf 'install-intake: stub\n'; exit 0
EOF
chmod +x "$SPIRA_DIR/install-intake.sh"

# ready.sh stub: always passes — so install.sh exits 0 on success.
cat > "$SPIRA_DIR/ready.sh" <<'EOF'
#!/usr/bin/env bash
printf 'spira ready\n'
printf '  pass  stub — ready\n'; exit 0
EOF
chmod +x "$SPIRA_DIR/ready.sh"

cat > "$COCKPIT_DIR/layout.sh" <<'EOF'
#!/usr/bin/env bash
printf 'layout.sh: stub\n'; exit 0
EOF
chmod +x "$COCKPIT_DIR/layout.sh"

ln -s "$REAL_REPO/install.sh" "$FIXTURE/install.sh"

# ---------------------------------------------------------------------------
# Mock binaries.
# ---------------------------------------------------------------------------
MOCK_BIN="$TMP/mock-bin"
mkdir -p "$MOCK_BIN"

cat > "$MOCK_BIN/systemctl" <<'EOF'
#!/usr/bin/env bash
case "$*" in
    *is-active*)      echo "active" ;;
    *list-unit-files*spira-watch*) true ;;
    *list-units*spira-watch*)      true ;;
    *list-units*active*spira-aeon*) true ;;
    *list-timers*)    true ;;
    *is-enabled*)     echo "enabled" ;;
    *enable*|*restart*|*start*) true ;;
    *daemon-reload*)  true ;;
esac
exit 0
EOF
chmod +x "$MOCK_BIN/systemctl"

cat > "$MOCK_BIN/loginctl" <<'EOF'
#!/usr/bin/env bash
case "$*" in *show-user*Linger*) echo "Linger=no" ;; esac; exit 0
EOF
chmod +x "$MOCK_BIN/loginctl"

cat > "$MOCK_BIN/tmux" <<'EOF'
#!/usr/bin/env bash
exit 1
EOF
chmod +x "$MOCK_BIN/tmux"

# bd stub — writes server-mode metadata.json on init --server.
FAKE_DB="$TMP/db"
_dbname="$(basename "$FAKE_DB")"
cat > "$MOCK_BIN/bd" <<BDSTUB
#!/usr/bin/env bash
case "\$*" in
    *-C*init*--server*)
        _db=""
        while [ \$# -gt 0 ]; do
            [ "\$1" = "-C" ] && { _db="\$2"; shift 2; continue; }
            shift
        done
        if [ -n "\$_db" ]; then
            mkdir -p "\$_db/.beads"
            printf '{"dolt_mode":"server","dolt_server_port":PORT,"dolt_database":"$_dbname","project_id":"test"}\n' \
                > "\$_db/.beads/metadata.json"
        fi
        exit 0 ;;
    *init*)     exit 0 ;;
    *list*)     printf '[]\n' ;;
    *memories*) printf '{}\n' ;;
    *)          exit 0 ;;
esac
BDSTUB
chmod +x "$MOCK_BIN/bd"

# Fake git repo so systemd/install.sh landref check passes.
FAKE_ORIGIN="$TMP/origin.git"
FAKE_REPO="$TMP/fakerepo"
git init -q --bare -b main "$FAKE_ORIGIN" 2>/dev/null
git init -q -b main "$FAKE_REPO" 2>/dev/null
git -C "$FAKE_REPO" config user.email t@t
git -C "$FAKE_REPO" config user.name test
printf 'seed\n' > "$FAKE_REPO/f"
git -C "$FAKE_REPO" add f
git -C "$FAKE_REPO" commit -qm "seed" 2>/dev/null
git -C "$FAKE_REPO" remote add origin "$FAKE_ORIGIN"
git -C "$FAKE_REPO" push -q origin main 2>/dev/null
git -C "$FAKE_REPO" fetch -q origin 2>/dev/null
git -C "$FAKE_REPO" symbolic-ref refs/remotes/origin/HEAD refs/remotes/origin/main

FAKE_HOME="$TMP/home"
FAKE_UNITDIR="$FAKE_HOME/.config/systemd/user"
FAKE_RUN="$TMP/run"
mkdir -p "$FAKE_HOME" "$FAKE_UNITDIR" "$FAKE_RUN"

# Pre-render units so the diff check passes.
_rendered="$(env -i \
    "PATH=$MOCK_BIN:$PATH" \
    "HOME=$FAKE_HOME" \
    SPIRA_CONF=/nonexistent \
    "SPIRA_PATH=$MOCK_BIN" \
    "SPIRA_WATCHERS=$SPIRA_DIR/watchers" \
    SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
    "SPIRA_RUN=$FAKE_RUN" \
    "SPIRA_HOME=$SPIRA_DIR" \
    "SPIRA_PROD=$SPIRA_DIR" \
    "SPIRA_REPO=$FAKE_REPO" \
    "SPIRA_COCKPIT=$COCKPIT_DIR" \
    SPIRA_INSTALL_FORCE=1 \
    "SPIRA_BD=$MOCK_BIN/bd" \
    bash "$SYSTEMD_DIR/install.sh" prod --render 2>/dev/null)"
_render_rc=$?
if [ "$_render_rc" = 0 ]; then
    _cur=""
    while IFS= read -r _line; do
        if [[ "$_line" =~ ^=====\ (.+)\ =====$ ]]; then
            _cur="${BASH_REMATCH[1]}"; > "$FAKE_UNITDIR/$_cur"
        elif [ -n "$_cur" ]; then
            printf '%s\n' "$_line" >> "$FAKE_UNITDIR/$_cur"
        fi
    done <<< "$_rendered"
    unset _cur _line
fi
unset _rendered _render_rc

run_install() {
    local extra_env=() install_args=() in_env=0
    for _a in "$@"; do
        [ "$_a" = "--" ] && { in_env=1; continue; }
        [ "$in_env" = 1 ] && { extra_env+=("$_a"); continue; }
        install_args+=("$_a")
    done
    unset _a in_env
    env -i \
        "PATH=$MOCK_BIN:$PATH" \
        "HOME=$FAKE_HOME" \
        SPIRA_CONF=/nonexistent \
        "SPIRA_PATH=$MOCK_BIN" \
        "SPIRA_WATCHERS=$SPIRA_DIR/watchers" \
        "SPIRA_RUN=$FAKE_RUN" \
        "SPIRA_HOME=$SPIRA_DIR" \
        "SPIRA_PROD=$SPIRA_DIR" \
        "SPIRA_REPO=$FAKE_REPO" \
        "SPIRA_COCKPIT=$COCKPIT_DIR" \
        SPIRA_INSTALL_FORCE=1 \
        "SPIRA_BD=$MOCK_BIN/bd" \
        "SPIRA_DB=$FAKE_DB" \
        "${extra_env[@]+"${extra_env[@]}"}" \
        bash "$FIXTURE/install.sh" "${install_args[@]+"${install_args[@]}"}" 2>&1
}

_dolt_port=19876
_dolt_data="$TMP/dolt-data"
mkdir -p "$_dolt_data"
cat > "$_dolt_data/dolt-server.yaml" <<YAML
listener:
  port: $_dolt_port
data_dir: "$_dolt_data"
YAML

# Fix the bd stub PORT placeholder now that we know the port.
sed -i "s|PORT|$_dolt_port|g" "$MOCK_BIN/bd"

# ==========================================================================
echo
echo "1. POSITIVE CONTROL — port never opens: exits 2 with named message:"
# ==========================================================================
# No nc listener started. SPIRA_INSTALL_DOLT_WAIT=3 keeps the test short.
rm -rf "$FAKE_DB"; mkdir -p "$FAKE_DB"

# A fresh db is needed so phase 3 runs bd init (which checks port open).
# Start nc briefly so phase 3's bd init can proceed, then stop it so phase 4 times out.
nc -lk "$_dolt_port" >/dev/null 2>&1 & _nc_init=$!
sleep 0.1

_timeout_out="$(run_install prod -- \
    "SPIRA_DOLT_DATA=$_dolt_data" \
    "SPIRA_INSTALL_DOLT_WAIT=3" \
    "SPIRA_INSTALL_DOLT_CLOSE_WAIT=3")"
_timeout_rc=$?

kill "$_nc_init" 2>/dev/null; wait "$_nc_init" 2>/dev/null || true
unset _nc_init

is2 "timeout: exits 2 (phase fails)"                     "$_timeout_rc"
want "timeout: names the port"      "$_dolt_port"         "$_timeout_out"
want "timeout: names timeout secs"  "after 3s"            "$_timeout_out"
want "timeout: says not listening"  "not listening on"    "$_timeout_out"

# ==========================================================================
echo
echo "2. PORT-OPEN WAIT — port opens within the window: reaches phase 7:"
# ==========================================================================
# nc starts after 3s. SPIRA_INSTALL_DOLT_WAIT=10 gives it room.
rm -rf "$FAKE_DB"; mkdir -p "$FAKE_DB"

nc -lk "$_dolt_port" >/dev/null 2>&1 & _nc_init2=$!
sleep 0.1

{ sleep 3; nc -lk "$_dolt_port" >/dev/null 2>&1; } & _nc_delayed=$!

_wait_out="$(run_install prod -- \
    "SPIRA_DOLT_DATA=$_dolt_data" \
    "SPIRA_INSTALL_DOLT_WAIT=10" \
    "SPIRA_INSTALL_DOLT_CLOSE_WAIT=3")"
_wait_rc=$?

kill "$_nc_init2" "$_nc_delayed" 2>/dev/null
wait "$_nc_init2" "$_nc_delayed" 2>/dev/null || true
unset _nc_init2 _nc_delayed

# Phase 4 cleared (port opened) so install reached phase 7. ready.sh stub exits 0 → install exits 0.
[ "$_wait_rc" = 0 ] \
    && ok  "port-open-wait: exits 0 (phase 7 reached and passed)" \
    || bad "port-open-wait: exits 0 (phase 7 reached and passed)" "got $_wait_rc"
want "port-open-wait: listening message" "dolt-beads.service listening on port" "$_wait_out"

# ==========================================================================
echo
echo "3. PORT-CLOSE WAIT — temp server holds port; unit start delayed, not failed:"
# ==========================================================================
# Use a mock dolt sql-server that runs nc on the port. After the kill, a new nc
# opens on the port after 1s (simulating dolt-beads.service starting).
# SPIRA_INSTALL_DOLT_CLOSE_WAIT=5 and SPIRA_INSTALL_DOLT_WAIT=10 give room.
rm -rf "$FAKE_DB"; mkdir -p "$FAKE_DB"

_nc_port2=19877
cat > "$_dolt_data/dolt-server.yaml" <<YAML2
listener:
  port: $_nc_port2
data_dir: "$_dolt_data"
YAML2
sed -i "s|$_dolt_port|$_nc_port2|g" "$MOCK_BIN/bd"

# Mock dolt: starts nc on the port, ignores SIGTERM briefly so port lingers.
cat > "$MOCK_BIN/dolt" <<DOLTMOCK
#!/usr/bin/env bash
[ "\${1:-}" = "sql-server" ] || { printf 'dolt: stub\n'; exit 0; }
nc -lk $_nc_port2 >/dev/null 2>&1 &
_nc=\$!
_done=0
trap '_done=1' SIGTERM
while [ "\$_done" = 0 ]; do sleep 0.2; done
sleep 1
kill "\$_nc" 2>/dev/null
DOLTMOCK
chmod +x "$MOCK_BIN/dolt"

# After 2s the mock dolt's nc will have closed; start a new nc to simulate the unit.
{ sleep 4; nc -lk "$_nc_port2" >/dev/null 2>&1; } & _nc_unit=$!

_close_out="$(run_install prod -- \
    "SPIRA_DOLT_DATA=$_dolt_data" \
    "SPIRA_INSTALL_DOLT_WAIT=10" \
    "SPIRA_INSTALL_DOLT_CLOSE_WAIT=5")"
_close_rc=$?

kill "$_nc_unit" 2>/dev/null; wait "$_nc_unit" 2>/dev/null || true
unset _nc_unit

[ "$_close_rc" = 0 ] \
    && ok  "port-close-wait: exits 0 (install succeeded despite port linger)" \
    || bad "port-close-wait: exits 0 (install succeeded despite port linger)" "got $_close_rc"
want "port-close-wait: listening message" "dolt-beads.service listening on port" "$_close_out"
nowant "port-close-wait: no timeout message" "not listening on" "$_close_out"

unset _nc_port2
rm -f "$MOCK_BIN/dolt"

echo
tl_summary
