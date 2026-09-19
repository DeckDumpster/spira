#!/usr/bin/env bash
#
# test-cockpit-watcher-owner.sh — two properties of the watcher ownership fix:
#
#   1. START DELEGATES WHEN A UNIT EXISTS. cockpit-remote start must call
#      systemctl --user start rather than forking a setsid orphan when the
#      watcher's systemd unit is installed.  Test asserts the running watcher's
#      pid is the one systemctl started — the unit's MainPID.
#
#   2. FAILED UNIT ESCALATION NAMES THE LOCK HOLDER. watchd.sh notify must
#      include the pid holding the lock and the lock path in the escalation
#      when a daemon unit is in failed/inactive state and an orphan holds the
#      lock that keeps the unit from starting.
#
# POSITIVE CONTROLS (law-a-regression-test-must-be-seen-to-fail):
#   For (1): when is-enabled returns false, cockpit-remote forks a setsid
#             orphan — no systemctl start call.  Proves the start-delegation
#             check is live.
#   For (2): when no orphan holds the lock, the escalation carries no "pid"
#             line — proves the orphan probe is live.
#
# covers: cockpit/remote/cockpit-remote spira/watchd.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
CR="$HERE/../cockpit/remote/cockpit-remote"
WATCHD="$HERE/watchd.sh"

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-cockpit-watcher-owner.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"; kill $(jobs -p) 2>/dev/null || true' EXIT

MOCK_BIN="$TMP/bin"; mkdir -p "$MOCK_BIN"
RUN="$TMP/run"; mkdir -p "$RUN"
WDIR="$RUN/watchd"; mkdir -p "$WDIR"
MAIL="$TMP/mail"

# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------

# lock_free <path>: 0 when nobody holds the lock, 1 when somebody does.
lock_free() { ( flock -n 9 ) 9>>"$1" 2>/dev/null; }

# lock_pid <path>: pid of the process holding an exclusive flock.
# Reads /proc/locks first; falls back to fuser for the exec-fd+flock pattern
# where fl_pid in /proc/locks is a dead flock subprocess.
lock_pid() {
    local f="$1" ino pid
    [ -e "$f" ] || return 1
    ino="$(stat -c '%i' "$f" 2>/dev/null)" || return 1
    case "$ino" in ''|*[!0-9]*) return 1 ;; esac
    pid="$(awk -v ino="$ino" '{ n=split($6,a,":"); if(a[n]==ino){ print $5; exit } }' \
           /proc/locks 2>/dev/null)"
    if [ -n "$pid" ] && [ -d "/proc/$pid" ]; then
        printf '%s' "$pid"; return 0
    fi
    if command -v fuser >/dev/null 2>&1; then
        pid="$(fuser "$f" 2>/dev/null | tr -s ' ' '\n' | grep -m1 '^[0-9]')"
        case "$pid" in ''|*[!0-9]*) ;; *) printf '%s' "$pid"; return 0 ;; esac
    fi
    return 1
}

# wait_locked <path>: spin until the lock is held, or timeout.
wait_locked() {
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        lock_free "$1" || return 0; sleep 0.1
    done
    return 1
}

# wait_free <path>: spin until the lock is free.
wait_free() {
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        lock_free "$1" && return 0; sleep 0.1
    done
    return 1
}

asks() { find "$MAIL/operator/new" -type f 2>/dev/null | wc -l | tr -d ' '; }

# ---------------------------------------------------------------------------
# Fake SPIRA_HOME for unit-name discovery.
# ---------------------------------------------------------------------------
FAKE_HOME="$TMP/spira-home"; mkdir -p "$FAKE_HOME"

# conf.sh: provides watch_unit_name with a test-specific name.
cat > "$FAKE_HOME/conf.sh" <<'CONFEOF'
watch_unit_name() { printf 'spira-watch-view-test.service'; }
CONFEOF

# watchd.sh: manifest returns a daemon row pointing at the cockpit-remote binary.
cat > "$FAKE_HOME/watchd.sh" <<WDEOF
#!/usr/bin/env bash
case "\${1:-}" in manifest) printf 'view|daemon|%s watch|\n' "$CR" ;; esac
WDEOF
chmod +x "$FAKE_HOME/watchd.sh"

# mail.sh: deposits escalation mail so asks() can count it.
cat > "$FAKE_HOME/mail.sh" <<'MAILEOF'
#!/usr/bin/env bash
mkdir -p "$SPIRA_MAIL/operator/new"
cat > "$SPIRA_MAIL/operator/new/$(date +%s%N)"
exit 0
MAILEOF
chmod +x "$FAKE_HOME/mail.sh"

# ===========================================================================
echo
echo "PART 1 — _wd_orphan_lock: finds a process holding a lock"
# ===========================================================================

# A test binary with a unique name that acquires a lock on its first argument and sleeps.
ORPHAN_BIN="$TMP/view-watcher-testfixture"
cat > "$ORPHAN_BIN" <<'ORPHANEOF'
#!/usr/bin/env bash
exec 9>"${1:?need lock path}"
flock -n 9 || { echo "lock already held" >&2; exit 75; }
sleep 60
ORPHANEOF
chmod +x "$ORPHAN_BIN"

ORPHAN_LOCK="$TMP/orphan-watch.lock"

# Helper: source watchd.sh and call _wd_orphan_lock with a given target.
call_orphan_lock() {
    local target="$1"
    env -i HOME="$TMP/home" PATH="$PATH" \
        SPIRA_CONF=/nonexistent SPIRA_RUN="$RUN" \
        SPIRA_INSTANCE=test SPIRA_WATCHERS="$TMP/watchers" \
        bash << EOF
. "$WATCHD" 2>/dev/null
_wd_orphan_lock "$target"
EOF
}

# POSITIVE CONTROL: with no orphan running, the function returns nothing.
out="$(call_orphan_lock "$ORPHAN_BIN $ORPHAN_LOCK" 2>/dev/null || true)"
is "pc: no orphan → _wd_orphan_lock returns nothing" "" "$out"

# Start the orphan binary directly so _wd_orphan_lock can find it by cmdline.
"$ORPHAN_BIN" "$ORPHAN_LOCK" & ORPHAN_PID="$!"
if ! wait_locked "$ORPHAN_LOCK"; then
    bad "pc: orphan holds the lock" "fixture failed to acquire lock"
else
    ok "pc: orphan holds the lock (positive control)"
fi

# Now the function must return pid + lock path.
out="$(call_orphan_lock "$ORPHAN_BIN $ORPHAN_LOCK" 2>/dev/null || true)"
want "found: output contains the pid"       "$ORPHAN_PID" "$out"
want "found: output contains the lock path" "$ORPHAN_LOCK" "$out"

# Kill the orphan; function must clear.
kill "$ORPHAN_PID" 2>/dev/null; wait "$ORPHAN_PID" 2>/dev/null || true
out="$(call_orphan_lock "$ORPHAN_BIN $ORPHAN_LOCK" 2>/dev/null || true)"
is "clear: after orphan exits, _wd_orphan_lock returns nothing" "" "$out"

# ===========================================================================
echo
echo "PART 2 — watchd.sh notify: escalation names lock holder for a failed unit"
# ===========================================================================

MAN="$TMP/watchers"
printf 'view|daemon|%s %s|\n' "$ORPHAN_BIN" "$ORPHAN_LOCK" > "$MAN"

# Mock systemctl: unit is in failed state.
cat > "$MOCK_BIN/systemctl" <<'SCTLEOF'
#!/usr/bin/env bash
[ "$1" = "--user" ] && shift
cmd="${1:-}"; shift
case "$cmd" in
    is-active) printf 'failed\n'; exit 1 ;;
    show)
        for a in "$@"; do
            case "$a" in *.service)
                printf 'Id=%s\nActiveState=failed\nNRestarts=7\n\n' "$a" ;;
            esac
        done ;;
esac
exit 0
SCTLEOF
chmod +x "$MOCK_BIN/systemctl"

UF="$WDIR/view.unhealthy"
# Backdate so age > SPIRA_NOTIFY_AGE=0 immediately.
printf '%s\n' "$(( $(date +%s) - 7200 ))" > "$UF"

run_notify() {
    rm -rf "$MAIL"; mkdir -p "$MAIL"
    rm -f "$WDIR/notify-health.escalated"
    env -i \
        HOME="$TMP/home" \
        PATH="$MOCK_BIN:$PATH" \
        SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$RUN" \
        SPIRA_INSTANCE=test \
        SPIRA_WATCHERS="$MAN" \
        SPIRA_MAIL="$MAIL" \
        SPIRA_HOME="$FAKE_HOME" \
        SPIRA_NOTIFY_AGE=0 \
        SPIRA_ACTIONABLE=WAKEME \
        bash "$WATCHD" notify 2>/dev/null || true
}

# POSITIVE CONTROL: no orphan → escalation fires but contains no "pid N holds" line.
run_notify
esc="$(cat "$MAIL/operator/new/"* 2>/dev/null || true)"
if [ "$(asks)" -gt 0 ]; then
    nowant "pc: no orphan → escalation has no pid-holds line" "pid " "$esc"
else
    bad "pc: escalation fires for failed unit even without orphan" "no mail"
fi

# Start the orphan binary directly so _wd_orphan_lock can find it by cmdline.
"$ORPHAN_BIN" "$ORPHAN_LOCK" & ORPHAN_PID="$!"
if ! wait_locked "$ORPHAN_LOCK"; then
    bad "notify fixture: orphan holds the lock" "lock acquisition failed"
fi

# Reset backdate so this pass also triggers.
printf '%s\n' "$(( $(date +%s) - 7200 ))" > "$UF"
run_notify
esc="$(cat "$MAIL/operator/new/"* 2>/dev/null || true)"
if [ "$(asks)" -gt 0 ]; then
    want "orphan: escalation names the holding pid"   "$ORPHAN_PID" "$esc"
    want "orphan: escalation names the lock path"     "$ORPHAN_LOCK" "$esc"
else
    bad "orphan: escalation fires with orphan present" "no mail"
fi

kill "$ORPHAN_PID" 2>/dev/null; wait "$ORPHAN_PID" 2>/dev/null || true

# ===========================================================================
echo
echo "PART 3 — cockpit-remote start: delegates to systemctl when unit is enabled"
# ===========================================================================

CR_LOCK="$TMP/cockpit-watch.lock"
CALLS_LOG="$TMP/calls.log"
UNIT_PID_FILE="$TMP/unit-main.pid"

# Mock systemctl: exports needed at mock-script creation time are passed via env.
export CALLS_LOG CR_LOCK UNIT_PID_FILE

cat > "$MOCK_BIN/systemctl" <<'SCTLEOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$CALLS_LOG"
[ "$1" = "--user" ] && shift
cmd="${1:-}"; shift
case "$cmd" in
    is-enabled) exit 0 ;;
    start)
        flock -n "$CR_LOCK" bash -c "exec sleep 60" &
        printf '%s\n' "$!" > "$UNIT_PID_FILE"
        exit 0 ;;
    stop)
        [ -r "$UNIT_PID_FILE" ] && kill "$(cat "$UNIT_PID_FILE")" 2>/dev/null || true
        exit 0 ;;
    *) exit 0 ;;
esac
SCTLEOF
chmod +x "$MOCK_BIN/systemctl"

# tmux stub: has-session → false so watch_loop exits immediately (avoids hanging).
cat > "$MOCK_BIN/tmux" <<'TMUXEOF'
#!/usr/bin/env bash
case "$*" in has-session*) exit 1 ;; *) exit 0 ;; esac
TMUXEOF
chmod +x "$MOCK_BIN/tmux"

run_cr() {
    env -i \
        HOME="$TMP/fake-home" \
        PATH="$MOCK_BIN:$PATH" \
        TMPDIR="$TMP" \
        SPIRA_HOME="$FAKE_HOME" \
        COCKPIT_SELF="$CR" \
        CALLS_LOG="$CALLS_LOG" \
        CR_LOCK="$CR_LOCK" \
        UNIT_PID_FILE="$UNIT_PID_FILE" \
        bash "$CR" "$@" 2>/dev/null
}

# -- POSITIVE CONTROL: unit disabled → setsid fork, no systemctl start call --------
cat > "$MOCK_BIN/systemctl" <<'SDEOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$CALLS_LOG"
[ "$1" = "--user" ] && shift
cmd="${1:-}"; shift
case "$cmd" in is-enabled) exit 1 ;; *) exit 0 ;; esac
SDEOF
chmod +x "$MOCK_BIN/systemctl"

rm -f "$CR_LOCK" "$UNIT_PID_FILE"; : > "$CALLS_LOG"
run_cr start || true
# Give setsid fork time to start.
wait_locked "$CR_LOCK" || true

if lock_free "$CR_LOCK"; then
    bad "pc: unit disabled → setsid orphan created (lock held)" "lock is free"
else
    ok "pc: unit disabled → setsid orphan created (lock held)"
fi
start_calls="$(grep -c ' start ' "$CALLS_LOG" 2>/dev/null || echo 0)"
is "pc: unit disabled → systemctl start NOT called" "0" "$start_calls"

# Clean up setsid orphan.
orphan_pid="$(lock_pid "$CR_LOCK" 2>/dev/null || true)"
[ -n "$orphan_pid" ] && kill "$orphan_pid" 2>/dev/null || true
wait_free "$CR_LOCK" || true

# -- Unit enabled: systemctl start is called, lock holder = started pid --------------
cat > "$MOCK_BIN/systemctl" <<'SCTLEOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$CALLS_LOG"
[ "$1" = "--user" ] && shift
cmd="${1:-}"; shift
case "$cmd" in
    is-enabled) exit 0 ;;
    start)
        flock -n "$CR_LOCK" bash -c "exec sleep 60" &
        printf '%s\n' "$!" > "$UNIT_PID_FILE"
        exit 0 ;;
    stop)
        [ -r "$UNIT_PID_FILE" ] && kill "$(cat "$UNIT_PID_FILE")" 2>/dev/null || true ;;
    *) exit 0 ;;
esac
SCTLEOF
chmod +x "$MOCK_BIN/systemctl"

rm -f "$CR_LOCK" "$UNIT_PID_FILE"; : > "$CALLS_LOG"
run_cr start || true
wait_locked "$CR_LOCK" || true

start_calls="$(grep -c ' start ' "$CALLS_LOG" 2>/dev/null || echo 0)"
if [ "$start_calls" -gt 0 ]; then
    ok "unit enabled → systemctl start called"
else
    bad "unit enabled → systemctl start called" \
        "calls: $(cat "$CALLS_LOG" 2>/dev/null | tr '\n' ' ')"
fi

want "unit name passed to systemctl" "spira-watch-view-test.service" \
    "$(cat "$CALLS_LOG" 2>/dev/null)"

if lock_free "$CR_LOCK"; then
    bad "unit enabled → watcher running (lock held)" "lock is free after start"
else
    ok "unit enabled → watcher running (lock held)"
fi

lock_holder="$(lock_pid "$CR_LOCK" 2>/dev/null || true)"
main_pid="$(cat "$UNIT_PID_FILE" 2>/dev/null || true)"
if [ -n "$lock_holder" ] && [ -n "$main_pid" ] && [ "$lock_holder" = "$main_pid" ]; then
    ok "watcher pid matches unit's MainPID ($lock_holder)"
else
    bad "watcher pid matches unit's MainPID" \
        "lock holder=$lock_holder, systemctl started=$main_pid"
fi

# Clean up.
[ -r "$UNIT_PID_FILE" ] && kill "$(cat "$UNIT_PID_FILE")" 2>/dev/null || true

# ===========================================================================
echo
printf 'test-cockpit-watcher-owner.sh: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
