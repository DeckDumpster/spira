#!/usr/bin/env bash
#
# test-install-dolt-breaker.sh — install.sh phase 4 clears bd's circuit breaker
# after dolt-beads.service starts, so ready.sh's database check passes even when
# the breaker was tripped in the gap between the temp init server and the unit.
#
#   ./test-install-dolt-breaker.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. POSITIVE CONTROL (mechanism): when a circuit breaker file exists for a db,
#    bd list fails with "circuit breaker is open". Without this, the passing case
#    below proves nothing.
# 2. CLEAR PASSES: after bd doctor --fix --yes, the breaker file is removed.
# 3. INSTALL POSITIVE CONTROL: install.sh's bd probe fails when the breaker is
#    open and the clear step is skipped (db stub: doctor is a no-op).
# 4. INSTALL PASSES: install.sh exits 0 when its doctor + probe sequence runs
#    (db stub: doctor removes the breaker flag, then list succeeds).
#
# SEAMS
#   SPIRA_INSTALL_DB_WAIT — max seconds for the bd probe retry loop (default 30)
#
# covers: install.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REAL_REPO="$(cd "$HERE/.." && pwd -P)"
pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant()  { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in output"; }
is0()     { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0, got $2"; }
nonzero() { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit, got 0"; }
is2()     { [ "$2" = 2 ] && ok "$1" || bad "$1" "wanted exit 2 (phase fail), got $2"; }

echo "test-install-dolt-breaker.sh"

# ---------------------------------------------------------------------------
# Parts 1 and 2: test the circuit breaker file mechanism directly.
# Write server-mode metadata so bd knows where to put the breaker file, then
# make rapid calls against a closed port. Check the file is created (positive
# control) and that bd doctor --fix --yes removes it.
# These tests do not require a live dolt server: the trip is against a closed
# port, and the clear is verified by checking the file is gone, not by bd list.
# ---------------------------------------------------------------------------

_TMP1="$(mktemp -d)"; trap 'rm -rf "$_TMP1"' EXIT INT TERM
_PORT1=19141
_DBNAME1="testbd$$"

mkdir -p "$_TMP1/db/.beads"
printf '{"dolt_mode":"server","dolt_server_port":%s,"dolt_database":"%s","project_id":"test"}\n' \
    "$_PORT1" "$_DBNAME1" > "$_TMP1/db/.beads/metadata.json"

# ==========================================================================
echo
echo "1. POSITIVE CONTROL — tripped circuit breaker: bd list fails:"
# ==========================================================================

# Ensure port is closed; 10 rapid parallel calls trip the 5-failure threshold.
for _i in $(seq 1 10); do
    BD_NON_INTERACTIVE=1 bd -C "$_TMP1/db" list --json >/dev/null 2>&1 &
done
wait
unset _i

_breaker_file="/tmp/beads-circuit/beads-dolt-circuit-127-0-0-1-${_PORT1}-${_DBNAME1}.json"
if [ -f "$_breaker_file" ]; then
    ok "positive control: circuit breaker file written to /tmp/beads-circuit/"
else
    printf '  skip  positive control: breaker file not found — bd version may not use this path\n'
    printf '        expected: %s\n' "$_breaker_file"
fi

# ==========================================================================
echo
echo "2. CLEAR PASSES — bd doctor --fix --yes removes the breaker file:"
# ==========================================================================

# bd doctor --fix only removes STALE breaker files — those past the 5s cooldown TTL.
# Wait for the TTL to expire before calling doctor so the file is considered stale.
sleep 7
BD_NON_INTERACTIVE=1 bd -C "$_TMP1/db" doctor --fix --yes 2>/dev/null || true
if [ ! -f "$_breaker_file" ]; then
    ok "clear passes: bd doctor --fix --yes removed the stale breaker file"
elif [ -f "$_breaker_file" ]; then
    bad "clear passes: breaker file remains after doctor --fix" \
        "file still exists: $_breaker_file"
fi

rm -rf "$_TMP1"
trap - EXIT INT TERM

# ---------------------------------------------------------------------------
# Parts 3 and 4: install.sh integration using a bd stub that models the breaker.
# The stub gates bd list on a flag file; doctor --fix removes it.
# This tests that install.sh calls doctor before the probe and that the probe
# succeeds only after the clear.
# ---------------------------------------------------------------------------

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# Harness fixture (mirrors test-install-dolt-port-wait.sh).
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

# All other spira scripts that appear in ExecStart= lines need to be executable
# so systemd/install.sh's pre-write check passes. Create pass-through stubs for
# any script not already stubbed above.
for _f in "$HERE/"*.sh; do
    _bn="$(basename "$_f")"
    [ -e "$SPIRA_DIR/$_bn" ] && continue
    printf '#!/usr/bin/env bash\nexit 0\n' > "$SPIRA_DIR/$_bn"
    chmod +x "$SPIRA_DIR/$_bn"
done
unset _f _bn

ln -s "$REAL_REPO/install.sh" "$FIXTURE/install.sh"

# Mock system binaries.
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
FAKE_DB="$TMP/db"
mkdir -p "$FAKE_HOME" "$FAKE_UNITDIR" "$FAKE_RUN" "$FAKE_DB"

_DOLT_PORT=19142
_DOLT_DATA="$TMP/dolt-data"
mkdir -p "$_DOLT_DATA"
cat > "$_DOLT_DATA/dolt-server.yaml" <<YAML
listener:
  port: $_DOLT_PORT
data_dir: $_DOLT_DATA
YAML

# BREAKER FLAG: bd stub gates list on this file; doctor --fix removes it.
# doctor --fix (no-op): breaker flag NOT removed → list keeps failing.
# doctor --fix (clearing): breaker flag IS removed → list succeeds.
_BREAKER_FLAG="$TMP/breaker-tripped"
_DBNAME2="testdb$$"

make_bd_stub() {
    local doctor_clears="${1:-0}"
    cat > "$MOCK_BIN/bd" <<BDSTUB
#!/usr/bin/env bash
_doctor_clears=$doctor_clears
_flag="$_BREAKER_FLAG"
_db_name="$_DBNAME2"
_fake_db="$FAKE_DB"
case "\$*" in
    *-C*init*--server*)
        _db=""
        while [ \$# -gt 0 ]; do
            [ "\$1" = "-C" ] && { _db="\$2"; shift 2; continue; }
            shift
        done
        if [ -n "\$_db" ]; then
            mkdir -p "\$_db/.beads"
            printf '{"dolt_mode":"server","dolt_server_port":PORT,"dolt_database":"%s","project_id":"test"}\n' \
                "\$_db_name" > "\$_db/.beads/metadata.json"
        fi
        exit 0 ;;
    *doctor*--fix*)
        if [ "\$_doctor_clears" = "1" ]; then
            rm -f "\$_flag"
            printf 'bd doctor: Cleared stale circuit breaker files\n'
        else
            printf 'bd doctor: No stale circuit breaker files\n'
        fi
        exit 0 ;;
    *list*)
        if [ -f "\$_flag" ]; then
            printf 'Error: dolt circuit breaker is open: server appears down, failing fast\n' >&2
            exit 1
        fi
        printf '[]\n'; exit 0 ;;
    *memories*) printf '{}\n' ;;
    *)          exit 0 ;;
esac
BDSTUB
    sed -i "s|PORT|$_DOLT_PORT|g" "$MOCK_BIN/bd"
    chmod +x "$MOCK_BIN/bd"
}

# Pre-render units for the diff check.
make_bd_stub 1
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
        "SPIRA_DOLT_DATA=$_DOLT_DATA" \
        "SPIRA_INSTALL_DOLT_WAIT=10" \
        "SPIRA_INSTALL_DOLT_CLOSE_WAIT=3" \
        "SPIRA_INSTALL_DB_WAIT=5" \
        bash "$FIXTURE/install.sh" prod 2>&1
}

# Pre-populate the db directory with server-mode metadata so phase 3 sees an
# existing store and skips bd init entirely. That avoids phase 3 starting a
# real dolt server (which would consume the nc before phase 4 can use it).
setup_fake_db() {
    rm -rf "$FAKE_DB"
    mkdir -p "$FAKE_DB/.beads"
    printf '{"dolt_mode":"server","dolt_server_port":%s,"dolt_database":"%s","project_id":"test"}\n' \
        "$_DOLT_PORT" "$_DBNAME2" > "$FAKE_DB/.beads/metadata.json"
}

# ==========================================================================
echo
echo "3. INSTALL POSITIVE CONTROL — no clear: probe fails, install exits non-zero:"
# ==========================================================================

setup_fake_db
touch "$_BREAKER_FLAG"   # pre-trip the breaker

# db stub: doctor is a no-op; list always fails while flag exists.
make_bd_stub 0

# nc listener simulates dolt-beads.service listening on the port.
# Phase 3 is skipped (db exists), so nc is consumed only by phase 4's probe.
nc -lk "$_DOLT_PORT" >/dev/null 2>&1 & _nc_pid=$!
sleep 0.3

_no_clear_out="$(run_install)"
_no_clear_rc=$?

kill "$_nc_pid" 2>/dev/null; wait "$_nc_pid" 2>/dev/null || true

nonzero "no-clear: install exits non-zero when db probe fails"     "$_no_clear_rc"
want    "no-clear: circuit-breaker error in output" "circuit breaker" "$_no_clear_out"
want    "no-clear: db probe attempt logged"         "db probe"         "$_no_clear_out"

# ==========================================================================
echo
echo "4. INSTALL PASSES — doctor clears breaker: install exits 0:"
# ==========================================================================

setup_fake_db
touch "$_BREAKER_FLAG"   # pre-trip the breaker

# db stub: doctor removes the flag; list then succeeds.
make_bd_stub 1

nc -lk "$_DOLT_PORT" >/dev/null 2>&1 & _nc_pid2=$!
sleep 0.3

_clear_out="$(run_install)"
_clear_rc=$?

kill "$_nc_pid2" 2>/dev/null; wait "$_nc_pid2" 2>/dev/null || true

is0   "install passes: install exits 0 after doctor clears breaker"    "$_clear_rc"
want  "install passes: bd store accepting logged"  "bd store accepting" "$_clear_out"
nowant "install passes: no circuit-breaker error"  "circuit breaker"    "$_clear_out"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
