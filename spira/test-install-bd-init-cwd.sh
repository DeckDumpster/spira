#!/usr/bin/env bash
#
# test-install-bd-init-cwd.sh — phase 3: bd init runs in $SPIRA_DB as cwd,
# not via -C; the post-init list --limit 0 call still uses -C.
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. POSITIVE CONTROL (cwd guard). A stub bd that fails on `-C … init` must
#    trigger a phase 3 FAIL before the fixed form is trusted. Without this,
#    the passing case proves nothing.
# 2. INIT CWD. After the fix, install.sh's phase 3 invokes bd init with cwd
#    equal to SPIRA_DB (not -C). The stub records cwd; the assertion checks it.
# 3. LIST USES -C. The post-init `list --limit 0` call (the skip check on a
#    second run) still passes -C SPIRA_DB, proving only the init call lost -C.
# 4. REAL BD FRESH-BOX. With the real bd binary, an empty directory outside any
#    .beads tree gains .beads after phase 3 succeeds. Skipped if bd is absent.
#
# covers: install.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REAL_REPO="$(cd "$HERE/.." && pwd -P)"
pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant()  { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0, got $2"; }
nonzero() { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit, got 0"; }
is2()     { [ "$2" = 2 ] && ok "$1" || bad "$1" "wanted exit 2, got $2"; }

echo "test-install-bd-init-cwd.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# ---------------------------------------------------------------------------
# Fixture — same skeleton as other install suites.
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
BD_LOG="$TMP/bd.log"
BD_CWD_FILE="$TMP/bd.cwd"
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

# bd stub used for the -C guard and cwd tests.
# When invoked with -C + init → rc 1 (simulates bd 1.2.1's refusal).
# When invoked with init (no -C) → records cwd, creates .beads, exits 0.
# When invoked with -C + list → exits 0, prints [].
cat > "$MOCK_BIN/bd" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$BD_LOG"
case "\$*" in
    *-C*init*)
        # -C with init: simulate bd 1.2.1 refusing on a directory with no project.
        printf 'Error: cannot use -C directory: no beads project found\n' >&2
        exit 1
        ;;
    *init*)
        # No -C: record cwd and succeed.
        pwd > "$BD_CWD_FILE"
        mkdir -p "\$(pwd)/.beads"
        printf '{"project_id":"test"}\n' > "\$(pwd)/.beads/metadata.json"
        exit 0
        ;;
    *list*)
        printf '[]\n'
        exit 0
        ;;
    *memories*)
        printf '{}\n'
        exit 0
        ;;
    *)
        exit 0
        ;;
esac
EOF
chmod +x "$MOCK_BIN/bd"

# ---------------------------------------------------------------------------
# Fake git repo so landref check passes.
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
FAKE_DB="$TMP/db"
mkdir -p "$FAKE_HOME" "$FAKE_UNITDIR" "$FAKE_RUN"

# Pre-render units so the diff check in install.sh does not block.
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
echo "1. POSITIVE CONTROL — stub bd fails on -C init, so phase 3 must FAIL:"
# ==========================================================================
# Plant a bd that always fails on -C + init. An empty SPIRA_DB has no .beads,
# so install.sh must attempt an init and trigger the failure.

# Override the stub with a version that always fails on -C + init.
cat > "$MOCK_BIN/bd" <<EOF2
#!/usr/bin/env bash
case "\$*" in
    *-C*init*) printf 'Error: cannot use -C directory: no beads project found\n' >&2; exit 1 ;;
    *list*)    printf '[]\n'; exit 0 ;;
    *memories*) printf '{}\n'; exit 0 ;;
    *)         exit 0 ;;
esac
EOF2
chmod +x "$MOCK_BIN/bd"

mkdir -p "$FAKE_DB"
rm -rf "$FAKE_DB/.beads"

_ctrl_out="$(run_install prod)"
_ctrl_rc=$?

is2    "positive-ctrl: -C init causes phase 3 FAIL (exit 2)" "$_ctrl_rc"
want   "positive-ctrl: names 'database' as the failed phase"  "phase database failed" "$_ctrl_out"

# ==========================================================================
echo
echo "2. INIT CWD — phase 3 invokes bd init with cwd == SPIRA_DB (no -C):"
# ==========================================================================
# Restore the cwd-recording stub.
rm -f "$BD_LOG" "$BD_CWD_FILE"
cat > "$MOCK_BIN/bd" <<EOF3
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
EOF3
chmod +x "$MOCK_BIN/bd"

rm -rf "$FAKE_DB"
mkdir -p "$FAKE_DB"

_cwd_out="$(run_install prod)"
_cwd_rc=$?

iszero  "cwd: phase 3 exits 0 with no-C form"          "$_cwd_rc"
want    "cwd: output reports initialising database"     "initialising database" "$_cwd_out"

_recorded_cwd="$(cat "$BD_CWD_FILE" 2>/dev/null || echo 'NOT RECORDED')"
[ "$_recorded_cwd" = "$FAKE_DB" ] \
    && ok  "cwd: init cwd equals SPIRA_DB" \
    || bad "cwd: init cwd equals SPIRA_DB" "got [$_recorded_cwd], want [$FAKE_DB]"

# ==========================================================================
echo
echo "3. LIST USES -C — post-init list --limit 0 still passes -C SPIRA_DB:"
# ==========================================================================
# On a second run .beads exists, so install.sh skips init and calls
# `bd -C $SPIRA_DB list --limit 0 --json`. Verify -C appears in that call.

_list_out="$(run_install prod)"
_list_rc=$?

iszero "list-uses-C: second run exits 0 (database already exists)" "$_list_rc"
want   "list-uses-C: output reports skip"                          "already done" "$_list_out"

# BD_LOG now has the calls from the second run. The list call must have -C.
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
    printf '\n%d passed, %d failed\n' "$pass" "$fail"
    [ "$fail" -eq 0 ]
    exit $?
}

# Use a directory under /var/tmp (or /tmp) to stay outside /home, which may
# have a .beads parent that bd would find by walking up.
_fb_base="$(mktemp -d /var/tmp/test-install-bd-fresh-XXXXXX 2>/dev/null \
    || mktemp -d /tmp/test-install-bd-fresh-XXXXXX)"
trap 'rm -rf "$TMP" "$_fb_base"' EXIT INT TERM

_fb_db="$_fb_base/db"
mkdir -p "$_fb_db"

# Confirm there is no .beads above _fb_db (the thing bd walks up to find).
_walk="$_fb_db"
_found_beads=0
while [ "$_walk" != "/" ] && [ "$_walk" != "" ]; do
    [ -d "$_walk/.beads" ] && { _found_beads=1; break; }
    _walk="$(dirname "$_walk")"
done
if [ "$_found_beads" = 1 ]; then
    ok "real-bd: parent .beads found above test dir — cannot isolate; skipping"
    printf '\n%d passed, %d failed\n' "$pass" "$fail"
    [ "$fail" -eq 0 ]
    exit $?
fi
unset _walk _found_beads

# Run just the embedded init form: (cd _fb_db && bd init --non-interactive ...).
# This mirrors exactly what install.sh now does for the embedded case.
_fb_out="$(
    cd "$_fb_db" && BD_NON_INTERACTIVE=1 "$_real_bd" init \
        --non-interactive --prefix sp --skip-agents --skip-hooks -q 2>&1
)"
_fb_rc=$?

iszero "real-bd: bd init with cwd exits 0 on fresh empty directory" "$_fb_rc"
[ -d "$_fb_db/.beads" ] \
    && ok  "real-bd: .beads created in SPIRA_DB" \
    || bad "real-bd: .beads created in SPIRA_DB" "directory [$_fb_db/.beads] not found; bd output=[$_fb_out]"

# ==========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
