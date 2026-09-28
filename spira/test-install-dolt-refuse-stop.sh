#!/usr/bin/env bash
#
# test-install-dolt-refuse-stop.sh — dolt-beads.service ships RefuseManualStop=yes, and
# install.sh's own apply path for it never asks systemd for a stop/restart job.
#
# WHY THIS UNIT CARRIES THE GUARD (sp-39gqq): aeons stopped the live bead database twice
# on 2026-09-25 by running suites directly on the host. RefuseManualStop=yes denies
# `systemctl stop`/`restart` on the unit unless the caller lifts the refusal explicitly
# (see the unit's own header for the sanctioned deliberate-stop path). That refusal also
# denies install.sh's ordinary upgrade restart, so install.sh must use a path that does
# not depend on a stop job at all — `kill`, which Restart=always turns into a restart
# without ever enqueuing one. RefuseManualStop's real-systemd semantics (restart refused,
# kill unaffected, Restart=always revives the process) are systemd's own documented
# contract (systemd.unit(5)) and already confirmed against this exact unit in a production
# incident (wiki/notes/sp-8cylj-host-mismatch.md); this suite proves install.sh's own
# dispatch chooses the kill path for dolt-beads.service and never calls `restart` on it.
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. POSITIVE CONTROL (render): a template missing the line does not match the check —
#    a suite that never fails on the defect proves nothing when it passes.
# 2. RENDER: RefuseManualStop=yes is present in the real template AND survives render.py
#    substitution into the actual unit text install.sh installs.
# 3. POSITIVE CONTROL (dispatch): a unit not named dolt-beads.service, changed and
#    active, is applied with `restart` — the ordinary path still works for everyone else.
# 4. DISPATCH: dolt-beads.service, changed and active (the upgrade/deploy case — sp-hsnqk,
#    "this deploy restarts dolt-beads"), is applied with `kill`, never `restart`, and
#    install.sh still exits 0.
#
# tier: T1
# covers: systemd/install.sh systemd/dolt-beads.service systemd/render.py
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REAL_REPO="$(cd "$HERE/.." && pwd -P)"
REAL_COCKPIT="$(cd "$HERE/../cockpit" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/lib-test-install.sh"
iszero() { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0, got $2"; }

echo "test-install-dolt-refuse-stop.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

TEMPLATE="$REAL_REPO/systemd/dolt-beads.service"
RENDER_PY="$REAL_REPO/systemd/render.py"

# ==========================================================================
echo
echo "1. POSITIVE CONTROL — a template missing the line does not match:"
# ==========================================================================
STRIPPED="$TMP/dolt-beads-no-guard.service"
grep -v '^RefuseManualStop=' "$TEMPLATE" > "$STRIPPED"
if grep -q '^RefuseManualStop=yes$' "$STRIPPED"; then
    bad "positive control: stripped template does not claim RefuseManualStop=yes" "found it anyway"
else
    ok "positive control: stripped template does not claim RefuseManualStop=yes"
fi

# ==========================================================================
echo
echo "2. RENDER — the real template and its rendered unit both carry the guard:"
# ==========================================================================
if grep -q '^RefuseManualStop=yes$' "$TEMPLATE"; then
    ok "template: systemd/dolt-beads.service declares RefuseManualStop=yes"
else
    bad "template: systemd/dolt-beads.service declares RefuseManualStop=yes" "not found"
fi

DOLT_BIN="$(command -v dolt 2>/dev/null || echo /usr/bin/dolt)"
rendered="$(python3 "$RENDER_PY" "$TEMPLATE" \
    --home /fake_home --repo /fake_repo --run /fake_run --db /fake_db \
    --cockpit /fake_cockpit --dolt-data /fake_dolt_data \
    --testdb-data /fake_testdb_data --dolt "$DOLT_BIN" \
    --prod /fake_prod --instance prod --testdb-port 3307 2>&1)"
want "render: rendered unit declares RefuseManualStop=yes" "RefuseManualStop=yes" "$rendered"

# ==========================================================================
echo
echo "3 & 4. DISPATCH — install.sh's apply path for a changed, active unit:"
# ==========================================================================
FAKE_ORIGIN="$TMP/origin.git"; FAKE_REPO="$TMP/repo"
git init -q --bare -b main "$FAKE_ORIGIN" 2>/dev/null
git init -q -b main "$FAKE_REPO" 2>/dev/null
git -C "$FAKE_REPO" config user.email t@t; git -C "$FAKE_REPO" config user.name test
printf 'seed\n' > "$FAKE_REPO/f"; git -C "$FAKE_REPO" add f; git -C "$FAKE_REPO" commit -qm seed 2>/dev/null
git -C "$FAKE_REPO" remote add origin "$FAKE_ORIGIN"; git -C "$FAKE_REPO" push -q origin main 2>/dev/null
git -C "$FAKE_REPO" fetch -q origin 2>/dev/null
git -C "$FAKE_REPO" symbolic-ref refs/remotes/origin/HEAD refs/remotes/origin/main
for _s in concierge.sh beads-push.sh; do
    [ -f "$REAL_REPO/$_s" ] && ln -sf "$REAL_REPO/$_s" "$FAKE_REPO/$_s"
done

FIXTURE="$TMP/harness"
install_fixture_build "$FIXTURE"

DEST="$TMP/home/.config/systemd/user"
RUN_DIR="$TMP/run"; MOCK_BIN="$TMP/mock-bin"
DOLT_DATA="$TMP/dolt"; DB="$TMP/db"
mkdir -p "$DEST" "$RUN_DIR" "$MOCK_BIN" "$DOLT_DATA" "$DB/.beads"
printf 'listener:\n  port: 3307\n' > "$DOLT_DATA/dolt-server.yaml"
LOG="$TMP/calls.log"

# systemctl mock — STATE_DIR/<unit> existing means that unit is active, so is-active
# reflects what this run has actually applied (a real enable-now/restart/kill leaves a
# unit active; a refused restart leaves it exactly as it was). dolt-beads.service and
# spira-sentinel-prod.timer start pre-marked active (the upgrade shape: already running,
# content changed under it — _unit_action -> restart for both). `restart
# dolt-beads.service` is answered the way real systemd answers it under
# RefuseManualStop=yes (refused, non-zero, state unchanged) so old code that still called
# restart would be caught leaving the unit un-upgraded rather than caught crashing — the
# log and the end state, not a raw exit code alone, are what prove which path was taken.
STATE_DIR="$TMP/systemctl-state"; mkdir -p "$STATE_DIR"
: > "$STATE_DIR/dolt-beads.service"
: > "$STATE_DIR/spira-sentinel-prod.timer"
cat > "$MOCK_BIN/systemctl" <<MOCK
#!/usr/bin/env bash
STATE_DIR="$STATE_DIR"
MOCK
cat >> "$MOCK_BIN/systemctl" <<'MOCK'
_args="$*"
printf 'systemctl %s\n' "$_args" >> "$CALL_LOG"
_unit="${_args##* }"
case "$*" in
    *is-active*)
        [ -f "$STATE_DIR/$_unit" ] && printf 'active\n' || printf 'inactive\n' ;;
    *is-enabled*)                     printf 'disabled\n' ;;
    *"restart dolt-beads.service"*)
        printf 'Failed to restart dolt-beads.service: Operation refused, unit may be activated/deactivated only indirectly.\n' >&2
        exit 1 ;;
    *"kill dolt-beads.service"*)      : > "$STATE_DIR/dolt-beads.service" ;;
    *"restart "*|*"enable --now "*)   : > "$STATE_DIR/$_unit" ;;
    *list-unit-files*spira-watch*)    : ;;
    *show*Type*)                      printf 'simple\n' ;;
esac
exit 0
MOCK
chmod +x "$MOCK_BIN/systemctl"
printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_BIN/bd"; chmod +x "$MOCK_BIN/bd"
for b in loginctl spira-supervise; do printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_BIN/$b"; chmod +x "$MOCK_BIN/$b"; done

: > "$LOG"
out="$(env -i PATH="$PATH" HOME="$TMP/home" SPIRA_CONF=/nonexistent SPIRA_PATH="$MOCK_BIN" \
    SPIRA_WATCHERS="$FIXTURE/spira/watchers" \
    SPIRA_DOLT_DATA="$DOLT_DATA" SPIRA_TESTDB_DATA= SPIRA_DB="$DB" SPIRA_BD="$MOCK_BIN/bd" \
    SPIRA_RUN="$RUN_DIR" SPIRA_HOME="$HERE" SPIRA_PROD="$HERE" SPIRA_REPO="$FAKE_REPO" \
    SPIRA_COCKPIT="$REAL_COCKPIT" SPIRA_SUPERVISE_BIN="$MOCK_BIN/spira-supervise" \
    SPIRA_INSTANCE=prod SPIRA_INSTALL_FORCE=1 SPIRA_DRAIN_INTERVAL=0 \
    CALL_LOG="$LOG" \
    bash "$FIXTURE/systemd/install.sh" 2>&1)"
rc=$?
iszero "install.sh exits 0 against a dolt-beads that refuses restart" "$rc"

nowant "dispatch: install.sh never asks systemd to restart dolt-beads.service" \
    "systemctl --user restart dolt-beads.service" "$(cat "$LOG")"
want "dispatch: install.sh kills dolt-beads.service instead" \
    "systemctl --user kill dolt-beads.service" "$(cat "$LOG")"
want "dispatch: install.sh reports the kill-based restart" \
    "restarted dolt-beads.service (kill" "$out"

want "positive control: an ordinary changed+active unit is still applied with restart" \
    "systemctl --user restart spira-sentinel-prod.timer" "$(cat "$LOG")"

tl_summary
