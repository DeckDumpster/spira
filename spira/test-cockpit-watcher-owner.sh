#!/usr/bin/env bash
#
# test-cockpit-watcher-owner.sh — two properties of the watcher ownership fix:
#
#   1. START DELEGATES WHEN A UNIT EXISTS. cockpit-remote start must call
#      systemctl --user start rather than forking a setsid orphan when the
#      watcher's systemd unit is installed.  Test asserts the running watcher's
#      pid is the one systemctl started — the unit's MainPID.
#
#   2. FAILED UNIT ESCALATION NAMES THE LOCK HOLDER. watchd notify must
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
# sp-mczuy: PART 2's orphan fixture flipped in round 213's certification (VM, maxpar 16):
# "not ok 2 - notify fixture: orphan holds the lock # lock acquisition failed", checks 3-4
# failing as a consequence. Root cause: the fixture's own acquisition was a single
# non-blocking `flock -n`, racing the test's `wait_locked`/`lock_free` probe further down
# this file, which transiently takes-and-releases a probe flock on the SAME path while
# polling for "has the orphan grabbed it yet" — the probe's subshell can still be mid-exit,
# holding the flock, the instant the fixture's one-shot attempt lands (the async-release
# race sp-os3of found in the Rust lock tests). With no retry, that single collision failed
# the fixture for good. Per law-a-test-that-flips-is-deleted: fixed in place (not
# deleted/re-added across two beads, since the root cause and fix landed together) by
# giving the fixture's own acquisition a bounded wait (`flock -w`, a kernel wait queue —
# never a fixed sleep) instead of a single try. See the fixture's own comment below.
# Proof: green in 20 consecutive separate `testenv` runs, and alongside a 30-suite load
# list to keep the race reproducible.
#
# tier: T1
# covers: cockpit/remote/cockpit-remote watchd/* UC-cockpit-observability-48
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
CR="$HERE/../cockpit/remote/cockpit-remote"
WATCHD="$(command -v watchd)" || { echo "watchd is not on PATH" >&2; exit 1; }


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

# watchd (sp-48f6g: a compiled binary, looked up by bare name — cockpit-remote's own
# _watch_unit_name does `command -v watchd`, so this stub goes on PATH ahead of the real
# one, never referenced by a constructed "$h/watchd" path): manifest returns a daemon row
# pointing at the cockpit-remote binary.
cat > "$FAKE_HOME/watchd" <<WDEOF
#!/usr/bin/env bash
case "\${1:-}" in manifest) printf 'view|daemon|%s watch|\n' "$CR" ;; esac
WDEOF
chmod +x "$FAKE_HOME/watchd"

# mail: deposits escalation mail so asks() can count it.
cat > "$FAKE_HOME/mail" <<'MAILEOF'
#!/usr/bin/env bash
mkdir -p "$SPIRA_MAIL/operator/new"
cat > "$SPIRA_MAIL/operator/new/$(date +%s%N)"
exit 0
MAILEOF
chmod +x "$FAKE_HOME/mail"

# ===========================================================================
# PART 1 (retired sp-48f6g) used to source watchd.sh and call its internal
# _wd_orphan_lock directly. watchd.sh is now the compiled binary `watchd`, which has no
# internal function to source — the orphan-lock probe (Ops::Real::orphan_lock) is exercised
# only through the real dependency it exists to serve, `watchd notify`'s escalation, which
# PART 2 below already drives end to end against this exact fixture: no orphan → no "pid "
# line (positive control), orphan present → names its pid and the lock path, orphan killed
# → the next fresh condition escalates clean again. Nothing PART 1 checked in isolation is
# left unchecked; it is checked at the one boundary this binary can still be driven through.
#
# A test binary with a unique name that acquires a lock on its first argument.
# Uses trap+subshell pattern: the bash process holds fd 9 while a background
# child runs WITHOUT fd 9 (closed before exec), so killing bash releases the
# lock without leaving a child that still holds it.
#
# sp-mczuy: acquisition below used to be a single non-blocking `flock -n`, which flipped
# PART 2 under load (cert round 213). The caller starts this fixture and then immediately
# polls for "is it held yet" with its own `wait_locked`/`lock_free` (further down this
# file), which itself takes and releases a probe flock on the SAME path while it polls.
# That probe's subshell can still be mid-exit — holding the flock — the instant this
# fixture's own one-shot attempt lands, the same async-release race sp-os3of found in the
# Rust lock tests; with no retry, that single collision fails the fixture for good, and
# the test then times out waiting for a holder that already gave up. Fix: wait on the real
# condition — the lock becoming free — with a bound, via `flock`'s own blocking wait
# (kernel wait queue, not a userspace poll or a fixed sleep) instead of a single try.
ORPHAN_BIN="$TMP/view-watcher-testfixture"
cat > "$ORPHAN_BIN" <<'ORPHANEOF'
#!/usr/bin/env bash
exec 9>"${1:?need lock path}"
flock -w 5 9 || { echo "lock acquisition timed out" >&2; exit 75; }
trap 'exit 0' TERM INT
( exec 9>&-; exec sleep 60 ) &
wait
ORPHANEOF
chmod +x "$ORPHAN_BIN"

ORPHAN_LOCK="$TMP/orphan-watch.lock"

# ===========================================================================
echo
echo "PART 2 — watchd notify: escalation names lock holder for a failed unit"
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

tl_config SPIRA_PATH="$MOCK_BIN" SPIRA_RUN="$RUN" SPIRA_INSTANCE=test \
    SPIRA_WATCHERS="$MAN" SPIRA_MAIL="$MAIL" SPIRA_NOTIFY_AGE=0 SPIRA_ACTIONABLE=WAKEME
run_notify() {
    rm -rf "$MAIL"; mkdir -p "$MAIL"
    rm -f "$WDIR/notify-health.escalated"
    # The fake home's mail stub and the mock binaries go FIRST on PATH: watchd calls
    # mail and systemctl by name (sp-gypjk).
    env -i \
        HOME="$TMP/home" \
        PATH="$FAKE_HOME:$MOCK_BIN:$PATH" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$FAKE_HOME" \
        SPIRA_TOML="$SPIRA_TOML" \
        "$WATCHD" notify 2>/dev/null || true
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

# tmux stub: always succeeds so watch_loop stays alive holding the lock.
cat > "$MOCK_BIN/tmux" <<'TMUXEOF'
#!/usr/bin/env bash
exit 0
TMUXEOF
chmod +x "$MOCK_BIN/tmux"

run_cr() {
    env -i \
        HOME="$TMP/fake-home" \
        PATH="$FAKE_HOME:$MOCK_BIN:$PATH" \
        TMPDIR="$TMP" \
        SPIRA_HOME="$FAKE_HOME" \
        COCKPIT_SELF="$CR" \
        COCKPIT_POLL=0 \
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
start_calls="$(grep -c ' start ' "$CALLS_LOG" 2>/dev/null || true)"
is "pc: unit disabled → systemctl start NOT called" "0" "$start_calls"

# Clean up setsid orphan and any children still holding the lock.
if command -v fuser >/dev/null 2>&1; then
    fuser -k "$CR_LOCK" 2>/dev/null || true
else
    orphan_pid="$(lock_pid "$CR_LOCK" 2>/dev/null || true)"
    [ -n "$orphan_pid" ] && kill "$orphan_pid" 2>/dev/null || true
fi
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
        flock -w 5 "$CR_LOCK" bash -c "exec sleep 60" &
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

start_calls="$(grep -c ' start ' "$CALLS_LOG" 2>/dev/null || true)"
if [ "${start_calls:-0}" -gt 0 ]; then
    ok "unit enabled → systemctl start called"
else
    bad "unit enabled → systemctl start called" \
        "calls: $(cat "$CALLS_LOG" 2>/dev/null | tr '\n' ' ')"
fi

want "unit name passed to systemctl" "spira-watch-view-test.service" \
    "$(cat "$CALLS_LOG" 2>/dev/null)"

# The mock unit takes the lock with a bounded blocking wait (a non-blocking attempt races the
# probe flock in lock_free), and the assertion polls for it rather than reading one instant.
if wait_locked "$CR_LOCK"; then
    ok "unit enabled → watcher running (lock held)"
else
    bad "unit enabled → watcher running (lock held)" "lock never became held"
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
tl_summary
