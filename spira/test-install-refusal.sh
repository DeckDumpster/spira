#!/usr/bin/env bash
#
# test-install-refusal.sh — install.sh's shared, earliest refusal path (phase 0 preflight)
# exits 1 and writes nothing, before any later phase runs.
#
#   ./test-install-refusal.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. POSITIVE CONTROL: with doctor.sh passing, a full run reaches phase 1 and writes
#    spira.conf. Without this, "nothing was written" on refusal would be indistinguishable
#    from a fixture that never writes anything for any reason
#    (law-absence-needs-a-positive-control).
# 2. PREFLIGHT REFUSAL: doctor.sh failing exits install.sh with 1, before phase 1 writes
#    spira.conf, before phase 3 creates a database directory, and before phase 4 makes any
#    systemctl call — this is install.sh:33-39's exit-1 contract (gap G3), previously
#    asserted only by grepping for the phase name, never by running the refusal.
#
# covers: install.sh UC-instance-lifecycle-18
# tier: T2
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REAL_REPO="$(cd "$HERE/.." && pwd -P)"
pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
is1()     { [ "$2" = 1 ] && ok "$1" || bad "$1" "wanted exit 1, got $2"; }
absent()  { [ ! -e "$2" ] && ok "$1" || bad "$1" "did not want $2 to exist"; }
present() { [ -e "$2" ] && ok "$1" || bad "$1" "wanted $2 to exist"; }

echo "test-install-refusal.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
# Fixture — same structure as test-install-conflicts.sh.
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

# doctor.sh: fails iff DOCTOR_FAIL_FLAG names an existing file — the one knob this suite
# turns between the positive control and the refusal case.
cat > "$SPIRA_DIR/doctor.sh" <<'EOF'
#!/usr/bin/env bash
echo "spira doctor"
if [ -n "${DOCTOR_FAIL_FLAG:-}" ] && [ -f "$DOCTOR_FAIL_FLAG" ]; then
    echo "  FAIL  stub — planted failure"
    exit 1
fi
echo "  ok    stub — all checks passed"
exit 0
EOF
chmod +x "$SPIRA_DIR/doctor.sh"

cat > "$SPIRA_DIR/configure.sh" <<'EOF'
#!/usr/bin/env bash
_out="${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.conf"
if [ -f "$_out" ]; then exit 1; fi
mkdir -p "$(dirname "$_out")"
printf 'SPIRA_PROD = %s\n' "${CONFIGURE_PROD:-/nonexistent}" > "$_out"
EOF
chmod +x "$SPIRA_DIR/configure.sh"

cat > "$SPIRA_DIR/build.sh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$SPIRA_DIR/build.sh"

cat > "$SPIRA_DIR/seed.sh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$SPIRA_DIR/seed.sh"

cat > "$SPIRA_DIR/install-session-hook.sh" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
    status)  printf 'ok  SessionStart\n' ;;
    install) printf 'installed\n' ;;
    *)       exit 0 ;;
esac
exit 0
EOF
chmod +x "$SPIRA_DIR/install-session-hook.sh"

cat > "$SPIRA_DIR/install-intake.sh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$SPIRA_DIR/install-intake.sh"

cat > "$SPIRA_DIR/ready.sh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$SPIRA_DIR/ready.sh"

cat > "$COCKPIT_DIR/layout.sh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$COCKPIT_DIR/layout.sh"

ln -s "$REAL_REPO/install.sh" "$FIXTURE/install.sh"

MOCK_BIN="$TMP/mock-bin"
mkdir -p "$MOCK_BIN"
MOCK_LOG="$TMP/mock.log"

cat > "$MOCK_BIN/systemctl" <<EOF
#!/usr/bin/env bash
printf 'systemctl %s\n' "\$*" >> "${MOCK_LOG}"
case "\$*" in
    *is-active*) echo "inactive" ;;
    *list-unit-files*spira-watch*) printf 'spira-watch@testview.service enabled\n' ;;
    *list-units*spira-watch*)  printf 'spira-watch@testview.service loaded active running\n' ;;
    *list-units*active*spira-aeon*) true ;;
    *list-timers*) true ;;
esac
exit 0
EOF
chmod +x "$MOCK_BIN/systemctl"

cat > "$MOCK_BIN/loginctl" <<'EOF'
#!/usr/bin/env bash
case "$*" in *show-user*Linger*) echo "Linger=no" ;; esac
exit 0
EOF
chmod +x "$MOCK_BIN/loginctl"

cat > "$MOCK_BIN/tmux" <<'EOF'
#!/usr/bin/env bash
exit 1
EOF
chmod +x "$MOCK_BIN/tmux"

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
FAKE_RELEASES="$TMP/releases"
mkdir -p "$FAKE_HOME" "$FAKE_UNITDIR" "$FAKE_RUN" "$FAKE_RELEASES"

DOCTOR_FAIL_FLAG="$TMP/doctor-fail-flag"

run_install() {
    local extra_env=() install_args=() in_env=0
    for _a in "$@"; do
        [ "$_a" = "--" ] && { in_env=1; continue; }
        [ "$in_env" = 1 ] && { extra_env+=("$_a"); continue; }
        install_args+=("$_a")
    done
    unset _a in_env
    > "$MOCK_LOG"
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
        "SPIRA_RELEASES=$FAKE_RELEASES" \
        "SPIRA_REPO=$FAKE_REPO" \
        "SPIRA_COCKPIT=$COCKPIT_DIR" \
        "MOCK_LOG=$MOCK_LOG" \
        "DOCTOR_FAIL_FLAG=$DOCTOR_FAIL_FLAG" \
        SPIRA_INSTALL_FORCE=1 \
        SPIRA_INSTALL_CONFLICT_CONSIDERED=1 \
        "SPIRA_BD=$MOCK_BIN/bd" \
        "${extra_env[@]+"${extra_env[@]}"}" \
        bash "$FIXTURE/install.sh" "${install_args[@]+"${install_args[@]}"}" 2>&1
}

CONF_DEST="$FAKE_HOME/.config/spira/spira.conf"
DB_DIR="$FAKE_DB"

# ===========================================================================
echo
echo "POSITIVE CONTROL: with doctor.sh passing, install reaches phase 1 and writes spira.conf"
# ===========================================================================
rm -f "$DOCTOR_FAIL_FLAG"
rm -rf "$FAKE_HOME/.config/spira"
rm -rf "$DB_DIR"
_ok_out="$(run_install prod -- "SPIRA_DB=$DB_DIR")"; _ok_rc=$?
present "positive control: spira.conf was written"        "$CONF_DEST"
present "positive control: database directory was created" "$DB_DIR"
want    "positive control: doctor passed"          "ok    stub" "$_ok_out"

# ===========================================================================
echo
echo "PREFLIGHT REFUSAL: doctor.sh failing exits 1 and writes nothing"
# ===========================================================================
: > "$DOCTOR_FAIL_FLAG"
rm -rf "$FAKE_HOME/.config/spira" "$DB_DIR"
_fail_out="$(run_install prod -- "SPIRA_DB=$DB_DIR")"; _fail_rc=$?
is1     "refusal: exits 1"                                  "$_fail_rc"
want    "refusal: preflight failure is reported"            "preflight failed" "$_fail_out"
absent  "refusal: spira.conf was NOT written"                "$CONF_DEST"
absent  "refusal: database directory was NOT created"        "$DB_DIR"
[ ! -s "$MOCK_LOG" ] \
    && ok "refusal: no systemctl call was made" \
    || bad "refusal: no systemctl call was made" "$(cat "$MOCK_LOG")"

# ===========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
