#!/usr/bin/env bash
#
# test-install-dolt-mode.sh — phase 3: refuses an existing embedded store;
# writes dolt_mode=server when initialising a fresh store with SPIRA_DOLT_DATA.
#
#   ./test-install-dolt-mode.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. POSITIVE CONTROL (embedded refusal). An existing .beads with dolt_mode=embedded
#    must trigger a phase 3 FAIL. Without this, the passing cases prove nothing.
# 2. SERVER MODE SKIP. An existing store with dolt_mode=server is accepted (skip).
# 3. FRESH INSTALL. With SPIRA_DOLT_DATA set and a TCP listener on the configured
#    port, phase 3 calls bd init --server and the resulting metadata has dolt_mode=server.
#    (Requires nc; skipped if unavailable.)
#
# covers: install.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REAL_REPO="$(cd "$HERE/.." && pwd -P)"
pass=0; fail=0
ok()    { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()   { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()  { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
is2()     { [ "$2" = 2 ] && ok "$1" || bad "$1" "wanted exit 2, got $2"; }
nowant()  { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in output"; }

echo "test-install-dolt-mode.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# ---------------------------------------------------------------------------
# Fixture layout (same skeleton as other install suites).
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
echo "spira doctor"
echo "  ok    stub — all checks passed"
exit 0
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
    status)  printf 'ok  SessionStart\nok  PostCompact\n' ;;
    install) printf 'install-session-hook: stub\n' ;;
    *)       exit 0 ;;
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
printf '  FAIL  stub\n'; exit 1
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
    *is-active*)      echo "inactive" ;;
    *list-unit-files*spira-watch*) printf 'spira-watch@testview.service enabled\n' ;;
    *list-units*spira-watch*)      printf 'spira-watch@testview.service loaded active running\n' ;;
    *list-units*active*spira-aeon*) true ;;
    *list-timers*)    true ;;
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

# bd stub for the embedded/skip tests (no init needed, just list).
cat > "$MOCK_BIN/bd" <<'EOF'
#!/usr/bin/env bash
case "$*" in
    *init*)     exit 0 ;;
    *list*)     printf '[]\n' ;;
    *memories*) printf '{}\n' ;;
    *)          exit 0 ;;
esac
EOF
chmod +x "$MOCK_BIN/bd"

# ---------------------------------------------------------------------------
# Fake git repo so systemd/install.sh landref check passes.
# ---------------------------------------------------------------------------
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

# Pre-render units so the diff check in install.sh passes.
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

# ---------------------------------------------------------------------------
# run_install <install_args...> [-- <extra_env_assignments...>]
# ---------------------------------------------------------------------------
FAKE_DB="$TMP/db"
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
        SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
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

# ==========================================================================
echo
echo "1. POSITIVE CONTROL — embedded store triggers a phase 3 FAIL:"
# ==========================================================================
# Plant an embedded store. This must cause install.sh to exit 2 and name the
# mode. Without this, the passing case below proves nothing.
mkdir -p "$FAKE_DB/.beads"
printf '{"dolt_mode":"embedded","dolt_database":"db","project_id":"test"}\n' \
    > "$FAKE_DB/.beads/metadata.json"

_emb_out="$(run_install prod)"
_emb_rc=$?
is2  "embedded: exits 2 (phase fail)" "$_emb_rc"
want "embedded: names 'embedded' in error" "embedded" "$_emb_out"
want "embedded: names the remedy: SPIRA_DOLT_DATA" "SPIRA_DOLT_DATA" "$_emb_out"

# ==========================================================================
echo
echo "2. SERVER-MODE EXISTING STORE — skips without error:"
# ==========================================================================
printf '{"dolt_mode":"server","dolt_server_port":3307,"dolt_database":"db","project_id":"test"}\n' \
    > "$FAKE_DB/.beads/metadata.json"

_srv_out="$(run_install prod)"
_srv_rc=$?
want   "server-mode existing: emits 'already done'" "already done" "$_srv_out"
nowant "server-mode existing: no embedded FAIL" "is embedded" "$_srv_out"

# ==========================================================================
echo
echo "3. FRESH INSTALL — dolt_mode=server written by phase 3:"
# ==========================================================================
# Requires nc for a TCP listener. Skip gracefully if unavailable.
rm -rf "$FAKE_DB"
mkdir -p "$FAKE_DB"

_dolt_port=19873
_dolt_data="$TMP/dolt-data"
mkdir -p "$_dolt_data"
cat > "$_dolt_data/dolt-server.yaml" <<YAML
listener:
  port: $_dolt_port
data_dir: "$_dolt_data"
YAML

# bd that writes metadata.json on init --server, simulating bd init's behaviour.
_dbname="$(basename "$FAKE_DB")"
cat > "$MOCK_BIN/bd" <<FAKESCRIPT
#!/usr/bin/env bash
case "\$*" in
    *-C*init*--server*)
        # Locate the -C argument.
        _db=""
        while [ \$# -gt 0 ]; do
            [ "\$1" = "-C" ] && { _db="\$2"; shift 2; continue; }
            shift
        done
        if [ -n "\$_db" ]; then
            mkdir -p "\$_db/.beads"
            printf '{"dolt_mode":"server","dolt_server_port":$_dolt_port,"dolt_database":"$_dbname","project_id":"test"}\n' \
                > "\$_db/.beads/metadata.json"
        fi
        exit 0
        ;;
    *init*)     exit 0 ;;
    *list*)     printf '[]\n' ;;
    *memories*) printf '{}\n' ;;
    *)          exit 0 ;;
esac
FAKESCRIPT
chmod +x "$MOCK_BIN/bd"

_nc_pid=""
if command -v nc >/dev/null 2>&1; then
    nc -lk "$_dolt_port" >/dev/null 2>&1 &
    _nc_pid=$!
    sleep 0.1
fi

if [ -n "$_nc_pid" ]; then
    _fresh_out="$(run_install prod -- "SPIRA_DOLT_DATA=$_dolt_data")"
    _fresh_rc=$?

    nowant "fresh-install: no embedded FAIL" "is embedded" "$_fresh_out"

    _fresh_mode=""
    if [ -f "$FAKE_DB/.beads/metadata.json" ]; then
        _fresh_mode="$(python3 -c \
            'import json,sys; print(json.load(sys.stdin).get("dolt_mode",""))' \
            < "$FAKE_DB/.beads/metadata.json" 2>/dev/null || true)"
    fi
    [ "$_fresh_mode" = "server" ] \
        && ok  "fresh-install: metadata.json has dolt_mode=server" \
        || bad "fresh-install: metadata.json has dolt_mode=server" \
               "got [${_fresh_mode:-missing}]"

    kill "$_nc_pid" 2>/dev/null; wait "$_nc_pid" 2>/dev/null || true
    unset _nc_pid
else
    ok "fresh-install: nc unavailable — TCP probe cannot be tested in this environment"
    ok "fresh-install: nc unavailable — dolt_mode check skipped"
fi
unset _dolt_port _dolt_data _dbname

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
