#!/usr/bin/env bash
#
# test-install-dolt-ready.sh — install.sh phase 3 waits for dolt to answer a real
# query (not just its TCP port) before running bd init, and retries bd init once
# on "invalid connection", removing the partial database it left first.
#
#   ./test-install-dolt-ready.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. POSITIVE CONTROL (readiness). When dolt never answers a query, phase 3 fails
#    and names the port and the timeout. Without this, property 2 proves nothing.
# 2. READINESS WAIT. A dolt that answers only after a few failed queries (TCP
#    already open the whole time) still lets install succeed — the port-open
#    check alone would have raced bd init against an unready server.
# 3. POSITIVE CONTROL (no blind retry). A bd init failure that is NOT "invalid
#    connection" is not retried — exactly one attempt, phase fails immediately.
# 4. RETRY WITH CLEANUP. A bd init that fails once with "invalid connection"
#    leaves a partial .beads/; install removes it and retries exactly once,
#    succeeding on the second attempt.
# 5. POSITIVE CONTROL (bounded retry). A bd init that always fails with "invalid
#    connection" is retried exactly once, not forever — two attempts, then fail.
# 6. T2 ACCEPTANCE. A deliberately slow-to-ready server (readiness fails a few
#    times) combined with one "invalid connection" bd init still initialises,
#    with the partial database cleaned up before the retry that succeeds.
#
# SEAMS
#   SPIRA_INSTALL_DOLT_READY_WAIT — max seconds to wait for a real query to answer
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
is2()     { [ "$2" = 2 ] && ok "$1" || bad "$1" "wanted exit 2 (phase fail), got $2"; }
eq()      { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$3], got [$2]"; }

echo "test-install-dolt-ready.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# ---------------------------------------------------------------------------
# Fixture layout (mirrors test-install-dolt-breaker.sh).
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

# All other spira scripts named in ExecStart= lines need to be executable so
# systemd/install.sh's pre-write check passes.
for _f in "$HERE/"*.sh; do
    _bn="$(basename "$_f")"
    [ -e "$SPIRA_DIR/$_bn" ] && continue
    printf '#!/usr/bin/env bash\nexit 0\n' > "$SPIRA_DIR/$_bn"
    chmod +x "$SPIRA_DIR/$_bn"
done
unset _f _bn

ln -s "$REAL_REPO/install.sh" "$FIXTURE/install.sh"

# ---------------------------------------------------------------------------
# Mock system binaries.
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

# Python TCP listener stands in for a dolt server that has an open port but says
# nothing about whether it can serve queries — that distinction lives entirely
# in the dolt mock below, not in this listener.
cat > "$TMP/listener.py" <<'PYEOF'
import socket, sys, signal
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(('127.0.0.1', int(sys.argv[1])))
s.listen(10)
while True:
    try: s.accept()[0].close()
    except Exception: break
PYEOF

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
mkdir -p "$FAKE_HOME" "$FAKE_UNITDIR" "$FAKE_RUN"

_DOLT_PORT="$(python3 -c "import socket; s=socket.socket(); s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1); s.bind(('127.0.0.1',0)); p=s.getsockname()[1]; s.close(); print(p)")"
_DOLT_DATA="$TMP/dolt-data"
mkdir -p "$_DOLT_DATA"
cat > "$_DOLT_DATA/dolt-server.yaml" <<YAML
listener:
  port: $_DOLT_PORT
data_dir: $_DOLT_DATA
YAML
_DBNAME="$(basename "$FAKE_DB")"

# make_dolt_stub <ready_after_n_calls>
# The mock's "sql -q select 1" call fails until it has been called
# <ready_after_n_calls> times (0 = ready on the first call).
make_dolt_stub() {
    local ready_after="${1:-0}"
    cat > "$MOCK_BIN/dolt" <<DOLTSTUB
#!/usr/bin/env bash
_ready_after=$ready_after
_calls_file="$TMP/dolt-ready-calls"
case "\$*" in
    *"sql -q select 1"*)
        _n=0
        [ -f "\$_calls_file" ] && _n="\$(cat "\$_calls_file")"
        _n=\$((_n + 1))
        printf '%s' "\$_n" > "\$_calls_file"
        if [ "\$_n" -le "\$_ready_after" ]; then
            printf 'error: connection refused\n' >&2
            exit 1
        fi
        printf '+---+\n| 1 |\n+---+\n'
        exit 0 ;;
    *"sql-server"*)
        # Not exercised by this suite: the port is always pre-opened by the
        # python listener before install.sh runs.
        sleep 300 ;;
    *) exit 0 ;;
esac
DOLTSTUB
    chmod +x "$MOCK_BIN/dolt"
}

# make_bd_stub <fail_mode> <fail_count>
#   fail_mode=ok           — init --server always succeeds.
#   fail_mode=invalid      — fails <fail_count> times with "invalid connection",
#                             leaving a partial .beads/PARTIAL marker; the call
#                             after the marker is gone (removed by install.sh)
#                             succeeds. If the marker is still present on a
#                             later call, the stub reports that distinctly so
#                             the test can tell a missing-cleanup apart from a
#                             coincidental pass.
#   fail_mode=other        — fails once with an unrelated error, always.
make_bd_stub() {
    local fail_mode="$1" fail_count="${2:-1}"
    cat > "$MOCK_BIN/bd" <<BDSTUB
#!/usr/bin/env bash
_fail_mode="$fail_mode"
_fail_count=$fail_count
_calls_file="$TMP/bd-init-calls"
_db_name="$_DBNAME"
case "\$*" in
    *init*--server*)
        _db="\$PWD"
        _n=0
        [ -f "\$_calls_file" ] && _n="\$(cat "\$_calls_file")"
        _n=\$((_n + 1))
        printf '%s' "\$_n" > "\$_calls_file"
        case "\$_fail_mode" in
            ok) : ;;
            other)
                printf 'Error: something else went wrong\n' >&2
                exit 1 ;;
            invalid)
                if [ "\$_n" -le "\$_fail_count" ]; then
                    if [ -f "\$_db/.beads/PARTIAL" ]; then
                        printf 'Error: PARTIAL NOT CLEANED before retry\n' >&2
                        exit 1
                    fi
                    mkdir -p "\$_db/.beads"
                    touch "\$_db/.beads/PARTIAL"
                    printf 'Error: dial tcp 127.0.0.1:${_DOLT_PORT}: i/o timeout: failed to create database: invalid connection\n' >&2
                    exit 1
                fi
                if [ -f "\$_db/.beads/PARTIAL" ]; then
                    printf 'Error: PARTIAL NOT CLEANED before retry\n' >&2
                    exit 1
                fi ;;
        esac
        if [ -n "\$_db" ]; then
            mkdir -p "\$_db/.beads"
            printf '{"dolt_mode":"server","dolt_server_port":${_DOLT_PORT},"dolt_database":"%s","project_id":"test"}\n' \
                "\$_db_name" > "\$_db/.beads/metadata.json"
        fi
        exit 0 ;;
    *doctor*--fix*) exit 0 ;;
    *list*)         printf '[]\n'; exit 0 ;;
    *memories*)     printf '{}\n' ;;
    *)              exit 0 ;;
esac
BDSTUB
    chmod +x "$MOCK_BIN/bd"
}

# Pre-render units for the diff check (dolt not on the render path).
make_dolt_stub 0
make_bd_stub ok
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
    local output_file="$1"; shift
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
        SPIRA_INSTALL_CONFLICT_CONSIDERED=1 \
        "SPIRA_BD=$MOCK_BIN/bd" \
        "SPIRA_DB=$FAKE_DB" \
        "SPIRA_DOLT_DATA=$_DOLT_DATA" \
        "SPIRA_INSTALL_DOLT_WAIT=10" \
        "SPIRA_INSTALL_DOLT_CLOSE_WAIT=3" \
        "SPIRA_INSTALL_DB_WAIT=5" \
        "$@" \
        bash "$FIXTURE/install.sh" prod >"$output_file" 2>&1
    return $?
}

reset_state() {
    rm -rf "$FAKE_DB"; mkdir -p "$FAKE_DB"
    rm -f "$TMP/dolt-ready-calls" "$TMP/bd-init-calls"
}

start_listener() {
    python3 "$TMP/listener.py" "$_DOLT_PORT" &
    _LISTENER_PID=$!
    sleep 0.3
}

stop_listener() {
    kill "$_LISTENER_PID" 2>/dev/null; wait "$_LISTENER_PID" 2>/dev/null || true
}

# ==========================================================================
echo
echo "1. POSITIVE CONTROL — dolt never answers a query: phase 3 fails:"
# ==========================================================================
reset_state
make_dolt_stub 999999   # never reaches readiness
make_bd_stub ok

start_listener
_out1="$(mktemp)"
run_install "$_out1" "SPIRA_INSTALL_DOLT_READY_WAIT=2"
_rc1=$?
stop_listener

is2  "never-ready: exits 2 (phase fails)"        "$_rc1"
want "never-ready: names the port"       "$_DOLT_PORT" "$(cat "$_out1")"
want "never-ready: names the timeout"    "did not answer queries within 2s" "$(cat "$_out1")"

# ==========================================================================
echo
echo "2. READINESS WAIT — dolt answers after a few failed queries: install succeeds:"
# ==========================================================================
reset_state
make_dolt_stub 2   # fails twice, then answers
make_bd_stub ok

start_listener
_out2="$(mktemp)"
run_install "$_out2" "SPIRA_INSTALL_DOLT_READY_WAIT=10"
_rc2=$?
stop_listener

is0 "readiness-wait: install exits 0 despite a slow-to-ready server" "$_rc2"
_calls2="$(cat "$TMP/dolt-ready-calls" 2>/dev/null || echo 0)"
[ "${_calls2:-0}" -ge 3 ] \
    && ok  "readiness-wait: install retried the query until it answered" \
    || bad "readiness-wait: install retried the query until it answered" "only $_calls2 call(s)"

# ==========================================================================
echo
echo "3. POSITIVE CONTROL — bd init fails with an unrelated error: no retry:"
# ==========================================================================
reset_state
make_dolt_stub 0
make_bd_stub other

start_listener
_out3="$(mktemp)"
run_install "$_out3" "SPIRA_INSTALL_DOLT_READY_WAIT=10"
_rc3=$?
stop_listener

is2 "other-error: exits 2 (phase fails)" "$_rc3"
eq  "other-error: bd init was attempted exactly once (no blind retry)" \
    "$(cat "$TMP/bd-init-calls" 2>/dev/null || echo 0)" "1"
want "other-error: reports bd init failure" "bd init (server mode) failed" "$(cat "$_out3")"

# ==========================================================================
echo
echo "4. RETRY WITH CLEANUP — invalid connection once, cleaned up, retry succeeds:"
# ==========================================================================
reset_state
make_dolt_stub 0
make_bd_stub invalid 1

start_listener
_out4="$(mktemp)"
run_install "$_out4" "SPIRA_INSTALL_DOLT_READY_WAIT=10"
_rc4=$?
stop_listener

is0 "retry-cleanup: install exits 0 after the retry" "$_rc4"
eq  "retry-cleanup: bd init was attempted exactly twice" \
    "$(cat "$TMP/bd-init-calls" 2>/dev/null || echo 0)" "2"
[ ! -f "$FAKE_DB/.beads/PARTIAL" ] \
    && ok  "retry-cleanup: partial marker is gone" \
    || bad "retry-cleanup: partial marker is gone" "still present"
nowant "retry-cleanup: no PARTIAL-NOT-CLEANED report" "PARTIAL NOT CLEANED" "$(cat "$_out4")"
_mode4=""
if [ -f "$FAKE_DB/.beads/metadata.json" ]; then
    _mode4="$(python3 -c \
        'import json,sys; print(json.load(sys.stdin).get("dolt_mode",""))' \
        < "$FAKE_DB/.beads/metadata.json" 2>/dev/null || true)"
fi
eq "retry-cleanup: metadata.json has dolt_mode=server" "$_mode4" "server"

# ==========================================================================
echo
echo "5. POSITIVE CONTROL — invalid connection every time: bounded to one retry:"
# ==========================================================================
reset_state
make_dolt_stub 0
make_bd_stub invalid 999999   # never stops failing

start_listener
_out5="$(mktemp)"
run_install "$_out5" "SPIRA_INSTALL_DOLT_READY_WAIT=10"
_rc5=$?
stop_listener

is2 "bounded-retry: exits 2 (phase fails)" "$_rc5"
eq  "bounded-retry: bd init was attempted exactly twice, not forever" \
    "$(cat "$TMP/bd-init-calls" 2>/dev/null || echo 0)" "2"
want "bounded-retry: reports bd init failure" "bd init (server mode) failed" "$(cat "$_out5")"

# ==========================================================================
echo
echo "6. T2 ACCEPTANCE — slow-to-ready server plus one invalid-connection init:"
# ==========================================================================
reset_state
make_dolt_stub 2      # slow-to-ready: two failed queries before it answers
make_bd_stub invalid 1  # one partial init, cleaned up, retry succeeds

start_listener
_out6="$(mktemp)"
run_install "$_out6" "SPIRA_INSTALL_DOLT_READY_WAIT=10"
_rc6=$?
stop_listener

is0 "t2: install still initialises against a slow-to-ready server" "$_rc6"
[ ! -f "$FAKE_DB/.beads/PARTIAL" ] \
    && ok  "t2: partial database was cleaned up before the retry" \
    || bad "t2: partial database was cleaned up before the retry" "still present"
_mode6=""
if [ -f "$FAKE_DB/.beads/metadata.json" ]; then
    _mode6="$(python3 -c \
        'import json,sys; print(json.load(sys.stdin).get("dolt_mode",""))' \
        < "$FAKE_DB/.beads/metadata.json" 2>/dev/null || true)"
fi
eq "t2: metadata.json has dolt_mode=server" "$_mode6" "server"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
