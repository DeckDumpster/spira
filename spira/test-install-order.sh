#!/usr/bin/env bash
#
# test-install-order.sh — systemd/install.sh arms nothing that reads the beads database
#   before the database answers, and never stops a manifest unit as if it were a watcher.
#
# THE DEFECTS (acceptance phase B, 2026-09-26: deploy's pre-health check refused on
# "spira-sentinel-prod.service is a failed systemd unit" and "spira-refresh-prod.service
# is a failed systemd unit", both left by the install that had just run):
#
#   1. ORDER. units.sh lists dolt-beads.service near the END of ENABLE, after every timer.
#      Enabling a timer whose elapsed-since-boot condition is met fires its service at
#      once, so on an install over an existing database whose server is down (any
#      re-install after uninstall.sh) the sentinel ran before dolt-beads started, logged
#      DATABASE UNREADABLE, exited 1 and stayed failed.
#   2. PRUNE. The watcher prune removed every spira-watch-*-<instance>.service that is not
#      a watcher row, including ordinary manifest units whose names fit the pattern. Those
#      units now live outside the watcher namespace (spira-refresh, spira-notify); a box
#      still carrying the old names has them migrated away, and the new names are never
#      disabled.
#
# CASES (law-absence-needs-a-positive-control):
#   - dolt-beads.service is applied before the first timer, and bd is asked whether the
#     database answers BETWEEN the two (the wait) — positive control: the log does contain
#     both the dolt-beads apply and the sentinel timer apply, so "before" is not vacuous.
#   - a bd that answers only on its third try holds the timers until it does.
#   - old-named spira-watch-refresh/-notify units are disabled; spira-refresh/-notify never are.
#
# tier: T2
# covers: install/src/bin/units_install.rs install/src/manifest.rs
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REAL_REPO="$(cd "$HERE/.." && pwd -P)"
REAL_COCKPIT="$(cd "$HERE/../cockpit" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/lib-test-install.sh"

echo "test-install-order.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

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
PROD="$(install_fixture_prod "$TMP/prod" "$HERE")"   # release-shaped prod root: spira/ -> this tree, bin/ unit stubs
printf 'listener:\n  port: 3307\n' > "$DOLT_DATA/dolt-server.yaml"
LOG="$TMP/calls.log"; BD_TRIES="$TMP/bd-tries"

# ONE LOG FOR BOTH MOCKS, so the order between systemctl calls and bd's readiness probe is
# the order they happened in.
cat > "$MOCK_BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
printf 'systemctl %s\n' "$*" >> "$CALL_LOG"
case "$*" in
    *is-active*)      printf 'inactive\n' ;;
    *is-enabled*)     printf 'disabled\n' ;;
    *list-unit-files*spira-watch*) printf 'spira-watch-refresh-%s.service enabled\nspira-watch-notify-%s.service enabled\n' "$MOCK_INST" "$MOCK_INST" ;;
    *show*Type*)      printf 'oneshot\n' ;;
esac
exit 0
MOCK
chmod +x "$MOCK_BIN/systemctl"
# bd: answers `sql select 1` only once BD_ANSWER_AFTER probes have failed; anything else
# (conf.sh's schema probe) succeeds silently.
cat > "$MOCK_BIN/bd" <<'MOCK'
#!/usr/bin/env bash
case " $* " in
    *" sql "*)
        n="$(cat "$BD_TRIES" 2>/dev/null || echo 0)"; n=$((n+1)); printf '%s' "$n" > "$BD_TRIES"
        printf 'bd sql probe %s\n' "$n" >> "$CALL_LOG"
        [ "$n" -gt "${BD_ANSWER_AFTER:-0}" ] ;;
    *) exit 0 ;;
esac
MOCK
chmod +x "$MOCK_BIN/bd"
for b in loginctl spira-supervise; do printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_BIN/$b"; chmod +x "$MOCK_BIN/$b"; done

# ACTIVE_WAIT=0: the mock systemctl never reports active, so the end-state wait would burn its ceiling per run.
inst() {
    : > "$LOG"; rm -f "$BD_TRIES"; rm -f "$DEST"/*.service "$DEST"/*.timer 2>/dev/null
    env -i PATH="$MOCK_BIN:$PATH" HOME="$TMP/home" SPIRA_CONF=/nonexistent SPIRA_PATH="$MOCK_BIN" \
        SPIRA_WATCHERS="$FIXTURE/spira/watchers" \
        SPIRA_DOLT_DATA="$DOLT_DATA" SPIRA_TESTDB_DATA= SPIRA_DB="$DB" SPIRA_BD="$MOCK_BIN/bd" \
        SPIRA_RUN="$RUN_DIR" SPIRA_HOME="$HERE" SPIRA_PROD="$PROD" SPIRA_REPO="$FAKE_REPO" \
        SPIRA_COCKPIT="$REAL_COCKPIT" \
        SPIRA_INSTANCE=prod MOCK_INST=prod CALL_LOG="$LOG" BD_TRIES="$BD_TRIES" \
        BD_ANSWER_AFTER="${BD_ANSWER_AFTER:-0}" \
        SPIRA_INSTALL_FORCE=1 SPIRA_DRAIN_INTERVAL=0 SPIRA_INSTALL_ACTIVE_WAIT=0 \
        units-install 2>&1
}
first_line() { grep -nE "$1" "$LOG" | head -1 | cut -d: -f1; }

# ==========================================================================
echo
echo "1. the database server is applied, and answering, before the first timer:"
# ==========================================================================
out="$(BD_ANSWER_AFTER=0 inst)"
db_at="$(first_line 'systemctl --user enable --now dolt-beads\.service')"
probe_at="$(first_line '^bd sql probe')"
tm_at="$(first_line 'systemctl --user enable --now spira-sentinel-prod\.timer')"
[ -n "$db_at" ] && [ -n "$tm_at" ] \
    && ok  "positive control: the log holds both the dolt-beads apply and the sentinel timer apply" \
    || bad "positive control: the log holds both the dolt-beads apply and the sentinel timer apply" "db=[$db_at] timer=[$tm_at]"
[ -n "$db_at" ] && [ -n "$tm_at" ] && [ "$db_at" -lt "$tm_at" ] \
    && ok  "dolt-beads.service is applied before the sentinel timer" \
    || bad "dolt-beads.service is applied before the sentinel timer" "db line [$db_at], timer line [$tm_at]"
_first_timer="$(first_line 'systemctl --user enable --now [^ ]*\.timer')"
[ -n "$db_at" ] && [ -n "$_first_timer" ] && [ "$db_at" -lt "$_first_timer" ] \
    && ok  "dolt-beads.service is applied before ANY timer" \
    || bad "dolt-beads.service is applied before ANY timer" "db line [$db_at], first timer line [$_first_timer]"
[ -n "$probe_at" ] && [ "$probe_at" -gt "${db_at:-0}" ] && [ "$probe_at" -lt "${tm_at:-0}" ] \
    && ok  "the database is probed between the server start and the first timer" \
    || bad "the database is probed between the server start and the first timer" "probe line [$probe_at]"

# ==========================================================================
echo
echo "2. a database that answers only on its third probe holds the timers until then:"
# ==========================================================================
out="$(BD_ANSWER_AFTER=2 inst)"
last_probe="$(grep -nE '^bd sql probe 3' "$LOG" | head -1 | cut -d: -f1)"
tm_at="$(first_line 'systemctl --user enable --now spira-sentinel-prod\.timer')"
[ -n "$last_probe" ] && [ -n "$tm_at" ] && [ "$last_probe" -lt "$tm_at" ] \
    && ok  "the sentinel timer is armed only after the probe that answered" \
    || bad "the sentinel timer is armed only after the probe that answered" "probe 3 line [$last_probe], timer line [$tm_at]"
want "the wait is said out loud" "waiting for the beads database" "$out"

# ==========================================================================
echo
echo "3. a box carrying the old watcher-namespaced names is migrated; the new names are left alone:"
# ==========================================================================
want   "old spira-watch-refresh is disabled"  "disable --now spira-watch-refresh-prod.service" "$(cat "$LOG")"
want   "old spira-watch-notify is disabled"   "disable --now spira-watch-notify-prod.service"  "$(cat "$LOG")"
nowant "spira-refresh is not disabled"        "disable --now spira-refresh-prod.service" "$(cat "$LOG")"
nowant "spira-notify is not disabled"         "disable --now spira-notify-prod.service"  "$(cat "$LOG")"

tl_summary
