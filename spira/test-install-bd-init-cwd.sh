#!/usr/bin/env bash
#
# test-install-bd-init-cwd.sh — phase 3: bd init runs in $SPIRA_DB as cwd,
# not via -C; the post-init list --limit 0 call still uses -C.
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. POSITIVE CONTROL (cwd guard). The stub bd fails when invoked with -C and
#    the init verb. Calling the stub directly proves it catches that form, so
#    it can be trusted as a regression guard: if install.sh ever reverts to
#    -C init, the stub will fail the install.
# 2. INIT CWD. Install.sh phase 3 invokes bd init with cwd equal to SPIRA_DB
#    (the stub records cwd; the assertion checks it).
# 3. LIST USES -C. The post-init `bd -C SPIRA_DB list --limit 0` call (the
#    skip-check on a second run) still passes -C, proving only init lost it.
# 4. REAL BD FRESH-BOX. With the real bd binary, an empty directory outside any
#    .beads tree gains .beads after the cd form of bd init. Skipped if bd absent.
#
# tier: T1
# covers: install/src/bin/install.rs
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
REAL_REPO="$(cd "$HERE/.." && pwd -P)"
iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0, got $2"; }
nonzero() { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit, got 0"; }

echo "test-install-bd-init-cwd.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# ---------------------------------------------------------------------------
# Fixture — same skeleton as other install suites.
# ---------------------------------------------------------------------------
. "$HERE/lib-test-install.sh"
FIXTURE="$TMP/harness"
SPIRA_DIR="$FIXTURE/spira"
SYSTEMD_DIR="$FIXTURE/systemd"
COCKPIT_DIR="$FIXTURE/cockpit"
mk_install_fixture "$FIXTURE" "$TMP"

cat > "$SPIRA_DIR/doctor" <<'EOF'
#!/usr/bin/env bash
echo "spira doctor"
echo "  ok    stub — all checks passed"
exit 0
EOF
chmod +x "$SPIRA_DIR/doctor"

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

install_fixture_release_stub "$SPIRA_DIR"

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

# Unit binaries (sp-gypjk): the units ExecStart $FIXTURE/bin/<tool>, the release layout.
. "$HERE/lib-test-install.sh"
install_fixture_release_bins "$FIXTURE"

# ---------------------------------------------------------------------------
# Mock binaries.
# ---------------------------------------------------------------------------
MOCK_BIN="$TMP/mock-bin"
BD_LOG="$TMP/bd.log"
BD_CWD_FILE="$TMP/bd.cwd"
mkdir -p "$MOCK_BIN"
# The sentinel, queue and aeon units ExecStart @SPIRA_*_BIN@ (8e220de40); install refuses a
# unit whose target is not executable, and FAKE_REPO has no bin/. No-op stubs, pinned below.
for _b in sentinel queue aeon; do
    printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_BIN/$_b"; chmod +x "$MOCK_BIN/$_b"
done; unset _b

# The stub used throughout the test:
# - Fails on -C + init (regression guard: if install.sh ever reverts to -C, fails)
# - Succeeds on init without -C, records cwd and creates .beads
# - Passes -C + list (post-init skip check keeps -C)
write_cwd_stub() {
cat > "$MOCK_BIN/bd" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$BD_LOG"
case "\$*" in
    *-C*init*)
        printf 'Error: cannot use -C directory: no beads project found\n' >&2
        exit 1
        ;;
    *init*)
        pwd > "$BD_CWD_FILE"
        mkdir -p "\$(pwd)/.beads"
        printf '{"project_id":"test"}\n' > "\$(pwd)/.beads/metadata.json"
        exit 0
        ;;
    *list*)    printf '[]\n'; exit 0 ;;
    *memories*) printf '{}\n'; exit 0 ;;
    *)         exit 0 ;;
esac
EOF
chmod +x "$MOCK_BIN/bd"
}
write_cwd_stub

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

FAKE_HOME="$TMP/home"
FAKE_UNITDIR="$FAKE_HOME/.config/systemd/user"
FAKE_RUN="$TMP/run"
FAKE_DB="$TMP/db"
mkdir -p "$FAKE_HOME" "$FAKE_UNITDIR" "$FAKE_RUN"

# Pre-render units so the diff check in install.sh does not block.
_rendered="$(env -i \
    "PATH=$MOCK_BIN:$SPIRA_DIR:$PATH" \
    "HOME=$FAKE_HOME" \
    SPIRA_CONF=/nonexistent \
    "SPIRA_PATH=$MOCK_BIN" \
    "SPIRA_WATCHERS=$SPIRA_DIR/watchers" \
    SPIRA_DOLT_DATA= "SPIRA_TESTDB_DATA=/nonexistent-testdb" \
    "SPIRA_RUN=$FAKE_RUN" \
    "SPIRA_HOME=$SPIRA_DIR" \
    "SPIRA_PROD=$SPIRA_DIR" \
    "SPIRA_REPO=$FAKE_REPO" \
    "SPIRA_COCKPIT=$COCKPIT_DIR" \
    SPIRA_INSTALL_FORCE=1 \
    SPIRA_INSTALL_LC_STORE_CONSIDERED=1 \
    "SPIRA_BD=$MOCK_BIN/bd" \
    units-install prod --render  2>/dev/null)"
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

# run_install <install_args> [-- <extra_env>...]
# Runs install.sh with SPIRA_TESTDB_DATA=/nonexistent-testdb so conf.sh's
# :=default does not pick up a live testdb path from the container's workspace.
run_install() {
    local extra_env=() install_args=() in_env=0
    for _a in "$@"; do
        [ "$_a" = "--" ] && { in_env=1; continue; }
        [ "$in_env" = 1 ] && { extra_env+=("$_a"); continue; }
        install_args+=("$_a")
    done
    unset _a in_env
    env -i \
        "PATH=$MOCK_BIN:$SPIRA_DIR:$PATH" \
        "HOME=$FAKE_HOME" \
        SPIRA_CONF=/nonexistent \
        "SPIRA_PATH=$MOCK_BIN" \
        "SPIRA_WATCHERS=$SPIRA_DIR/watchers" \
        SPIRA_DOLT_DATA= "SPIRA_TESTDB_DATA=/nonexistent-testdb" \
        "SPIRA_RUN=$FAKE_RUN" \
        "SPIRA_HOME=$SPIRA_DIR" \
        "SPIRA_PROD=$SPIRA_DIR" \
        "SPIRA_REPO=$FAKE_REPO" \
        "SPIRA_COCKPIT=$COCKPIT_DIR" \
        SPIRA_INSTALL_FORCE=1 \
        SPIRA_INSTALL_LC_STORE_CONSIDERED=1 \
        "SPIRA_BD=$MOCK_BIN/bd" \
        "SPIRA_DB=$FAKE_DB" \
        "${extra_env[@]+"${extra_env[@]}"}" \
        spira-install "${install_args[@]+"${install_args[@]}"}" 2>&1
}

# ==========================================================================
echo
echo "1. POSITIVE CONTROL — stub fails on -C init, proving it guards the regression:"
# ==========================================================================
# Call the stub directly with the old form (-C dir init). This must fail, which
# proves the stub would catch a regression if install.sh reverted to -C.
_ctrl_dir="$TMP/ctrl-dir"; mkdir -p "$_ctrl_dir"
_ctrl_out="$("$MOCK_BIN/bd" -C "$_ctrl_dir" init 2>&1)"; _ctrl_rc=$?
nonzero "positive-ctrl: stub exits non-zero on -C + init"                "$_ctrl_rc"
want    "positive-ctrl: stub names the -C refusal"  "cannot use -C directory" "$_ctrl_out"
unset _ctrl_dir

# ==========================================================================
echo
echo "2. INIT CWD — phase 3 invokes bd init with cwd == SPIRA_DB (no -C):"
# ==========================================================================
rm -f "$BD_LOG" "$BD_CWD_FILE"
rm -rf "$FAKE_DB"; mkdir -p "$FAKE_DB"

_cwd_out="$(run_install prod)"

want  "cwd: phase 3 reports initialising database"  "initialising database" "$_cwd_out"

_recorded_cwd="$(cat "$BD_CWD_FILE" 2>/dev/null || echo 'NOT RECORDED')"
[ "$_recorded_cwd" = "$FAKE_DB" ] \
    && ok  "cwd: init cwd equals SPIRA_DB" \
    || bad "cwd: init cwd equals SPIRA_DB" "got [$_recorded_cwd], want [$FAKE_DB]"

[ -d "$FAKE_DB/.beads" ] \
    && ok  "cwd: .beads created inside SPIRA_DB" \
    || bad "cwd: .beads created inside SPIRA_DB" ".beads not found under [$FAKE_DB]"

# ==========================================================================
echo
echo "3. LIST USES -C — post-init list --limit 0 still passes -C SPIRA_DB:"
# ==========================================================================
# On a second run .beads exists, so install.sh calls:
#   bd -C $SPIRA_DB list --limit 0 --json
# Verify -C appears in that call so we know only the init call lost -C.
rm -f "$BD_LOG"
_list_out="$(run_install prod)"

want "list-uses-C: second run reports skip (database exists)" "already done" "$_list_out"

_log_content="$(cat "$BD_LOG" 2>/dev/null || echo '')"
[[ "$_log_content" == *"-C"*"list"* ]] \
    && ok  "list-uses-C: list call includes -C" \
    || bad "list-uses-C: list call includes -C" "log=[$_log_content]"

# ==========================================================================
echo
echo "4. REAL BD FRESH-BOX — empty dir outside .beads tree gains .beads:"
# ==========================================================================
# Find the real bd binary. Skip if absent.
_real_bd="${SPIRA_BD:-$(command -v bd 2>/dev/null || true)}"
[ -x "${_real_bd:-}" ] || {
    ok "real-bd: bd not found — skipping fresh-box test"
    tl_summary; exit
}

# Use a directory under /var/tmp to stay outside /home, which may have a
# .beads parent that bd would find by walking up.
_fb_base="$(mktemp -d /var/tmp/test-install-bd-fresh-XXXXXX 2>/dev/null \
    || mktemp -d /tmp/test-install-bd-fresh-XXXXXX)"
trap 'rm -rf "$TMP" "$_fb_base"' EXIT INT TERM

_fb_db="$_fb_base/db"
mkdir -p "$_fb_db"

# Confirm there is no .beads above _fb_db.
_walk="$_fb_db"; _found_beads=0
while [ "$_walk" != "/" ] && [ "$_walk" != "" ]; do
    [ -d "$_walk/.beads" ] && { _found_beads=1; break; }
    _walk="$(dirname "$_walk")"
done
if [ "$_found_beads" = 1 ]; then
    ok "real-bd: parent .beads found above test dir — cannot isolate; skipping"
    tl_summary; exit
fi
unset _walk _found_beads

# Run the embedded init form: (cd _fb_db && bd init).
# This mirrors exactly what install.sh now does.
_fb_out="$(
    cd "$_fb_db" && BD_NON_INTERACTIVE=1 "$_real_bd" init \
        --non-interactive --prefix sp --skip-agents --skip-hooks -q 2>&1
)"
_fb_rc=$?

iszero "real-bd: bd init with cwd exits 0 on fresh empty directory" "$_fb_rc"
[ -d "$_fb_db/.beads" ] \
    && ok  "real-bd: .beads created in SPIRA_DB" \
    || bad "real-bd: .beads created in SPIRA_DB" \
           "directory [$_fb_db/.beads] not found; bd output=[$_fb_out]"

# ==========================================================================
echo
tl_summary
