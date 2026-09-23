#!/usr/bin/env bash
#
# test-install-aeons.sh — install.sh restarts only what changed; drains oneshots;
# guards against live aeons; exits 0 with prod aeons on a test-instance install.
#
#   ./test-install-aeons.sh
#
# THE PROPERTIES UNDER TEST
# -------------------------
# 1. AEON GUARD: install.sh refuses to run (exit non-zero) when live aeon units
#    are reported for THIS INSTANCE. SPIRA_INSTALL_FORCE=1 bypasses the guard.
# 2. NO-OP INSTALL: when nothing changed and all units are active, install.sh
#    restarts nothing and exits 0.
# 3. SELECTIVE RESTART: only the unit(s) whose rendered content changed are
#    restarted; others are skipped with an "unchanged" log line.
# 4. AEON SAFETY: install.sh never restarts a spira-aeon-* unit (the guard may
#    query them, but they are structurally absent from UNITS and ENABLE).
# 5. ONESHOT DRAIN: when a changed timer's backing service is a running oneshot,
#    install.sh waits for it to finish before restarting.
# 6. WATCH PRESERVATION: a spira-watch-*-<instance>.service unit whose name IS in
#    the manifest is not disabled by the manifest-prune step.
# 7. END-STATE CHECK: after install, install.sh exits non-zero if any enabled
#    unit is not active.
#
# THE FIXTURE USES A MOCK systemctl THAT RECORDS CALLS AND RETURNS CONTROLLED OUTPUT.
# Pin a non-default SPIRA_RUN so nothing touches the operator's live directory
# (law-gates-run-in-a-clean-environment).
#
# defect: sp-1j0r, sp-syub
# covers: systemd/install.sh spira/skew.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REAL_REPO="$(cd "$HERE/.." && pwd -P)"
REAL_COCKPIT="$(cd "$HERE/../cockpit" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
nonzero(){ [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit, got 0"; }
iszero() { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0, got $2"; }

echo "test-install-aeons.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
# Fake git repo for SPIRA_REPO. The landref check in install.sh refuses when
# the checkout is not on its landref or is behind it. REAL_REPO is on the
# aeon's working branch, so it would fail the check; FAKE_REPO is a throwaway
# repo on main with origin/HEAD set, so the check passes and the aeon guard
# test can focus on live-aeon refusal rather than landref refusal.
# ---------------------------------------------------------------------------
FAKE_ORIGIN="$TMP/origin.git"
FAKE_REPO="$TMP/repo"
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
# The templates substitute @SPIRA_REPO@ in ExecStart lines; the ExecStart check requires
# those targets to be executable. Symlink the two scripts that templates use this way.
for _s in concierge.sh beads-push.sh; do
    [ -f "$REAL_REPO/$_s" ] && ln -sf "$REAL_REPO/$_s" "$FAKE_REPO/$_s"
done
unset _s

# ---------------------------------------------------------------------------
# Fixture: minimal harness tree mirroring what test-install-halt.sh builds.
# ---------------------------------------------------------------------------
FIXTURE="$TMP/harness"
mkdir -p "$FIXTURE/systemd" "$FIXTURE/spira"

for f in "$HERE/../systemd/"*.service "$HERE/../systemd/"*.timer; do
    [ -e "$f" ] || continue
    ln -s "$f" "$FIXTURE/systemd/$(basename "$f")"
done
ln -s "$HERE/../systemd/install.sh" "$FIXTURE/systemd/install.sh"
for f in conf.sh watchd.sh lib.sh; do
    [ -e "$HERE/$f" ] && ln -s "$HERE/$f" "$FIXTURE/spira/$f"
done

# The watchers file has one daemon row so the prune loop knows "testview" is legitimate.
cat > "$FIXTURE/spira/watchers" <<'WATCHERS'
testview|daemon|/bin/true
WATCHERS

printf '# empty\n' > "$FIXTURE/spira/repo-map.example"
printf '#!/usr/bin/env bash\nexit 0\n' > "$FIXTURE/spira/install-session-hook.sh"
chmod +x "$FIXTURE/spira/install-session-hook.sh"

DEST="$TMP/home/.config/systemd/user"
SPIRA_RUN_DIR="$TMP/run"
MOCK_BIN="$TMP/mock-bin"
mkdir -p "$DEST" "$SPIRA_RUN_DIR" "$MOCK_BIN"
MOCK_LOG="$TMP/systemctl.log"
# DRAIN_STATE is read by the oneshot-drain mock to simulate a transitioning service.
DRAIN_STATE="$TMP/drain_state"

# ---------------------------------------------------------------------------
# Mock systemctl records every call and returns scenario-controlled output.
#
#   MOCK_AEONS      space-separated aeon unit names to report as active; empty = none
#   MOCK_IS_ACTIVE  what is-active returns for every unit; defaults to "active"
#   MOCK_ONESHOT_SVC   service name that is-active should report as a transitioning
#                      oneshot (active on first query, inactive thereafter)
#   DRAIN_STATE        file holding current state for MOCK_ONESHOT_SVC queries
#
# The mock returns per-instance watcher unit names for list-unit-files/list-units
# so the prune loop has a real instance to decide about. Under per-instance naming
# the prune queries 'spira-watch-*-prod.service'; this mock returns a unit matching
# that pattern so the logic can be exercised.
# ---------------------------------------------------------------------------
cat > "$MOCK_BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${MOCK_LOG}"
case "$*" in
    *list-units*active*spira-aeon*)
        for a in ${MOCK_AEONS:-}; do printf '%s\n' "$a"; done
        ;;
    *list-unit-files*spira-watch*)
        printf 'spira-watch-testview-prod.service enabled\n'
        ;;
    *list-units*spira-watch*)
        printf 'spira-watch-testview-prod.service loaded active running Test watcher\n'
        ;;
    *show*Type*)
        # Return "oneshot" only for the designated drain target.
        if [[ "$*" == *"${MOCK_ONESHOT_SVC:-__none__}"* ]]; then
            printf 'oneshot\n'
        else
            printf 'simple\n'
        fi
        ;;
    *is-active*"${MOCK_ONESHOT_SVC:-__none__}"*)
        # Transition: "active" on first query, then "inactive".
        state="$(cat "${DRAIN_STATE}" 2>/dev/null || printf 'inactive')"
        printf '%s\n' "$state"
        printf 'inactive\n' > "${DRAIN_STATE}"
        ;;
    *is-active*)
        printf '%s\n' "${MOCK_IS_ACTIVE:-active}"
        ;;
    *list-timers*)
        true
        ;;
esac
exit 0
MOCK
chmod +x "$MOCK_BIN/systemctl"

printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_BIN/loginctl"
chmod +x "$MOCK_BIN/loginctl"
printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_BIN/spira-supervise"
chmod +x "$MOCK_BIN/spira-supervise"

# ---------------------------------------------------------------------------
# inst [args] — run install.sh in the controlled environment.
# Reads MOCK_AEONS, MOCK_IS_ACTIVE, MOCK_FORCE, MOCK_ONESHOT_SVC from caller's scope.
# MOCK_FORCE is passed as SPIRA_INSTALL_FORCE; empty means the guard is active.
# SPIRA_DRAIN_INTERVAL=0 makes the drain loop poll without sleeping.
# ---------------------------------------------------------------------------
inst() {
    > "$MOCK_LOG"
    env -i \
        "PATH=$PATH" \
        "HOME=$TMP/home" \
        SPIRA_CONF=/nonexistent \
        "SPIRA_PATH=$MOCK_BIN" \
        "SPIRA_WATCHERS=$FIXTURE/spira/watchers" \
        SPIRA_DOLT_DATA= \
        SPIRA_TESTDB_DATA= \
        "SPIRA_RUN=$SPIRA_RUN_DIR" \
        "SPIRA_HOME=$HERE" \
        "SPIRA_PROD=$HERE" \
        "SPIRA_REPO=$FAKE_REPO" \
        "SPIRA_COCKPIT=$REAL_COCKPIT" \
        "SPIRA_SUPERVISE_BIN=$MOCK_BIN/spira-supervise" \
        "MOCK_LOG=$MOCK_LOG" \
        "MOCK_AEONS=${MOCK_AEONS:-}" \
        "MOCK_IS_ACTIVE=${MOCK_IS_ACTIVE:-active}" \
        "MOCK_ONESHOT_SVC=${MOCK_ONESHOT_SVC:-__none__}" \
        "DRAIN_STATE=$DRAIN_STATE" \
        "SPIRA_INSTALL_FORCE=${MOCK_FORCE:-}" \
        SPIRA_DRAIN_INTERVAL=0 \
        bash "$FIXTURE/systemd/install.sh" "$@" 2>&1
}

# Seed DEST with rendered units so every installed unit matches what install.sh
# would render — this is the "nothing changed" baseline.
rendered="$(MOCK_AEONS= MOCK_IS_ACTIVE=active MOCK_FORCE= MOCK_ONESHOT_SVC=__none__ inst --render)"
render_rc=$?
if [ "$render_rc" != 0 ]; then
    printf 'fixture: install.sh --render failed (rc=%s) — cannot continue\n' "$render_rc"
    printf '%s\n' "$rendered"
    exit 1
fi
current_unit=""
while IFS= read -r line; do
    if [[ "$line" =~ ^=====\ (.+)\ =====$ ]]; then
        current_unit="${BASH_REMATCH[1]}"; > "$DEST/$current_unit"
    elif [ -n "$current_unit" ]; then
        printf '%s\n' "$line" >> "$DEST/$current_unit"
    fi
done <<< "$rendered"

# ==========================================================================
echo
echo "AEON GUARD — install.sh refuses while this instance's aeons are live:"
# ==========================================================================

# Under per-instance naming the guard matches spira-aeon-*-prod.service.
aeon_out="$(MOCK_AEONS="spira-aeon-builder-9999-prod.service" MOCK_IS_ACTIVE=active MOCK_FORCE= \
             MOCK_ONESHOT_SVC=__none__ inst)"
aeon_rc=$?
aeon_log="$(cat "$MOCK_LOG")"

nonzero "aeon guard: exit non-zero when aeons are live"            "$aeon_rc"
want    "aeon guard: names the live aeon in error output"          "spira-aeon-builder-9999" "$aeon_out"
want    "aeon guard: mentions SPIRA_INSTALL_FORCE override"        "SPIRA_INSTALL_FORCE" "$aeon_out"
# The guard must fire BEFORE any unit file is written or daemon-reload is called.
nowant  "aeon guard: daemon-reload not called when guard fires"    "daemon-reload" "$aeon_log"

# ==========================================================================
echo
echo "FORCE OVERRIDE — SPIRA_INSTALL_FORCE=1 bypasses the aeon guard:"
# ==========================================================================

force_out="$(MOCK_AEONS="spira-aeon-builder-9999-prod.service" MOCK_IS_ACTIVE=active MOCK_FORCE=1 \
              MOCK_ONESHOT_SVC=__none__ inst)"
force_rc=$?
force_log="$(cat "$MOCK_LOG")"

iszero  "force override: exit 0 with SPIRA_INSTALL_FORCE=1"        "$force_rc"
# No unit files changed (DEST was seeded from the same templates), so daemon-reload
# is not triggered even when SPIRA_INSTALL_FORCE bypasses the aeon guard.
nowant  "force override: no daemon-reload when no file changed"     "daemon-reload" "$force_log"

# ==========================================================================
echo
echo "NO-OP INSTALL — nothing changed; all active:"
# ==========================================================================
# A no-op install (all units already match rendered content and are active)
# must restart nothing and exit 0.
noop_out="$(MOCK_AEONS= MOCK_IS_ACTIVE=active MOCK_FORCE= MOCK_ONESHOT_SVC=__none__ inst)"
noop_rc=$?
noop_log="$(cat "$MOCK_LOG")"

iszero  "no-op: exit 0 when nothing changed"                       "$noop_rc"
nowant  "no-op: daemon-reload NOT called when nothing changed"      "daemon-reload" "$noop_log"
nowant  "no-op: no restart command"                                 "restart" "$noop_log"
nowant  "no-op: no enable --now command"                            "enable --now" "$noop_log"
want    "no-op: unchanged units reported as skipped"                "unchanged" "$noop_out"

# ==========================================================================
echo
echo "SELECTIVE RESTART — one unit content changed; only that unit restarted:"
# ==========================================================================
# Write a different sentinel timer to DEST so it appears changed to install.sh.
# Under per-instance naming the installed file is spira-sentinel-prod.timer.
timer_file="$DEST/spira-sentinel-prod.timer"
printf '# deliberately altered to trigger restart\n' >> "$timer_file"

selective_out="$(MOCK_AEONS= MOCK_IS_ACTIVE=active MOCK_FORCE= MOCK_ONESHOT_SVC=__none__ inst)"
selective_rc=$?
selective_log="$(cat "$MOCK_LOG")"

iszero  "selective: exit 0 after selective restart"                 "$selective_rc"
want    "selective: sentinel timer is restarted"                    "spira-sentinel-prod.timer" "$selective_log"
nowant  "selective: other timers not restarted"                     "restart spira-ops" "$selective_log"
nowant  "selective: other timers not enable --now"                  "enable --now spira-ops" "$selective_log"

# Restore the timer to baseline so subsequent tests see no changes.
rendered_timer="$(printf '%s\n' "$rendered" | awk '/^===== spira-sentinel-prod.timer =====$/{found=1;next} /^===== /{found=0} found')"
printf '%s\n' "$rendered_timer" > "$timer_file"

# ==========================================================================
echo
echo "AEON SAFETY — install never restarts a spira-aeon-* unit:"
# ==========================================================================
aeon_safe_out="$(MOCK_AEONS= MOCK_IS_ACTIVE=active MOCK_FORCE= MOCK_ONESHOT_SVC=__none__ inst)"
aeon_safe_rc=$?
aeon_safe_log="$(cat "$MOCK_LOG")"

iszero  "aeon safety: install exits 0"                              "$aeon_safe_rc"
# The guard QUERIES for aeons but must never restart one.
nowant  "aeon safety: no restart spira-aeon-* call"                "restart spira-aeon-" "$aeon_safe_log"
nowant  "aeon safety: no enable spira-aeon-* call"                 "enable spira-aeon-" "$aeon_safe_log"

# ==========================================================================
echo
echo "WATCH PRESERVATION — manifest instance is not disabled:"
# ==========================================================================
watch_out="$(MOCK_AEONS= MOCK_IS_ACTIVE=active MOCK_FORCE= MOCK_ONESHOT_SVC=__none__ inst)"
watch_log="$(cat "$MOCK_LOG")"

nowant  "watch: testview instance not disabled"   "disable --now spira-watch-testview-prod" "$watch_log"

# ==========================================================================
echo
echo "ONESHOT DRAIN — changed timer; backing service is a running oneshot:"
# ==========================================================================
# Alter the sentinel timer content so it appears changed.
printf '# altered for drain test\n' >> "$DEST/spira-sentinel-prod.timer"
# Tell the mock that spira-sentinel-prod.service is a running oneshot that transitions
# to inactive after the first is-active probe.
printf 'active\n' > "$DRAIN_STATE"

drain_out="$(MOCK_AEONS= MOCK_IS_ACTIVE=active MOCK_FORCE= \
              MOCK_ONESHOT_SVC=spira-sentinel-prod.service inst)"
drain_rc=$?
drain_log="$(cat "$MOCK_LOG")"

iszero  "drain: exit 0 after draining the oneshot"                  "$drain_rc"
want    "drain: is-active was queried on the backing service"        "is-active spira-sentinel-prod.service" "$drain_log"
want    "drain: drain message emitted"                              "mid-pass" "$drain_out"
want    "drain: timer was applied after drain"                      "spira-sentinel-prod.timer" "$drain_log"

# Restore timer.
printf '%s\n' "$rendered_timer" > "$DEST/spira-sentinel-prod.timer"

# ==========================================================================
echo
echo "END-STATE CHECK — install exits non-zero when a unit is not active:"
# ==========================================================================
badstate_out="$(MOCK_AEONS= MOCK_IS_ACTIVE=failed MOCK_FORCE= MOCK_ONESHOT_SVC=__none__ inst)"
badstate_rc=$?

nonzero "end-state: exit non-zero when units are not active"       "$badstate_rc"
want    "end-state: output names the failure"                      "not active" "$badstate_out"

# ==========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
