#!/usr/bin/env bash
#
# test-timeout.sh — a session killed by the lane cap (rc=124) charges no attempt.
#
#   ./test-timeout.sh
#
# THE DEFECT THIS REPRODUCES. sp-m56w ran four consecutive ops sessions at 480s each. Every
# one ended rc=124 (timeout killed the claude process), with nothing committed. The cleanup
# path in aeon.sh classified the trace as `unlanded` and bumped the attempt counter. The bead
# poisoned at attempt 3; the fourth session claimed it one second after the label landed.
#
# WHAT IS UNDER TEST:
#   1. SP_OPS_AGE reports 0 when an ops aeon pid is live, not the stale log mtime.
#
# RETIRED (sp-8itaf): SESSION_RC=124/attempt-vs-timeout-counter charging, the
# poison-after-N-timeouts escalation (lib.sh's spira_ask_timeout_loop) and the
# poison-raced-the-predicate clean exit are all gone from this file. The first and third
# are the Rust aeon's disposition table (`cargo test -p aeon decide::tests::disposition_table`,
# `tests::poison_raced_releases_and_records`, `session::tests::timeout_is_124`), already
# noted below. spira_ask_timeout_loop, timeouts_of and bump_timeout had no caller left
# anywhere in the tree (zero live callers) and were deleted outright, not ported.
#
# defect: sp-06hs
# tier: T2
# covers: spira/*.sh UC-aeon-execution-25
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
isge(){ [ "${2:-0}" -le "${3:-0}" ] 2>/dev/null && ok "$1" \
        || bad "$1" "wanted [$3] >= [$2], got empty or non-integer"; }

echo "test-timeout.sh"
echo
echo "structural:"

# RETIRED WITH aeon.sh: the timeout-before-requeue-before-outcome order and "the timeout
# path charges no attempt" are the Rust aeon's disposition table — `cargo test -p aeon
# decide::tests::disposition_table` and `session::tests::timeout_is_124`.

# RETIRED WITH aeon.sh: the poison-after-claim guard (before any workspace setup) and the
# session-rc capture are the Rust aeon's — `cargo test -p aeon tests::poison_raced_releases_and_records`
# and `session::tests::timeout_is_124`.

# The "cleanup disarms errexit first" check (D7) lived in test-attempts.sh (retired with aeon.sh), with the
# hazard demonstration that shows why; this file only used a bare copy of the assertion.

# ======================================================================================
# SP_OPS_AGE: active session reports 0, not a stale log mtime.
# Tests the aeon_alive + pid-file logic cockpit-collect's now_keys probe uses.
# Run inline (sourcing lib.sh and the logic directly) rather than via the full probe,
# which requires a live beads db and a configured git repo. No real database is needed —
# aeon_alive is a pure /proc read, so this sources lib.sh directly (test-roster-warn.sh's
# old precedent) rather than standing up a testdb/dolt server for nothing.
# ======================================================================================
echo
echo "SP_OPS_AGE (cockpit logic):"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT; trap 'exit 143' INT TERM
export SPIRA_HOME="$TMP" SPIRA_RUN="$TMP/run" SPIRA_CONF="$TMP/no-such.conf"
# shellcheck disable=SC1090
. "$HERE/lib.sh"
RUN="$TMP/run"; mkdir -p "$RUN"
# age_of is defined in cockpit-collect (io::mtime_age_secs), not lib.sh. Inline it here so
# the test runs standalone.
age_of() { local f="$1" m; m="$(stat -c %Y "$f" 2>/dev/null)" || { printf '?'; return; }
           [ -n "$m" ] || { printf '?'; return; }; printf '%d' $(( $(date +%s) - m )); }
# Inline the ops-age logic from cockpit-collect's now_keys to test it independently of
# the rest of the probe.
ops_age_of() {   # ops_age_of <run-dir> -> 0 if live, else log mtime age in seconds
    local run="$1" age pf
    age="$(age_of "$run/ops.log")"
    for pf in "$run"/aeon-ops-*.pid; do
        [ -e "$pf" ] || continue
        if aeon_alive "$pf"; then age=0; break; fi
    done
    printf '%s' "$age"
}

# No ops.log: should return '?'.
age="$(ops_age_of "$RUN")"
is "no ops.log reports ?" "?" "$age"

# An ops.log with a known mtime and no pidfile: age should be a positive integer.
echo "x" > "$RUN/ops.log"
touch -d '2 minutes ago' "$RUN/ops.log"
age="$(ops_age_of "$RUN")"
isge "stale log, no pidfile: age >= 100s" 100 "$age"

# A pidfile for a dead process: falls through to log mtime.
PF="$RUN/aeon-ops-sp-fake.pid"
printf '%d' 999999999 > "$PF"
age_stale="$(ops_age_of "$RUN")"
isge "dead pidfile: age still reflects log mtime" 100 "$age_stale"
rm -f "$PF"

# A pidfile whose process is alive AND runs aeon.sh → age = 0.
# Start a background process named so that its argv[1] contains 'aeon.sh'.
# The stub blocks on a FIFO read with no children, so there is no grandchild
# process left in the suite's process group after cleanup. Closing the write
# end of the FIFO (exec 3>&-) delivers EOF to the stub's read, which exits
# cleanly without needing SIGTERM or any kill/wait sequence.
mkfifo "$TMP/hold"
cat > "$TMP/aeon.sh" <<'STUB'
#!/usr/bin/env bash
read -r || true
STUB
chmod +x "$TMP/aeon.sh"
( exec bash "$TMP/aeon.sh" < "$TMP/hold" ) &
STUB_PID=$!
exec 3>"$TMP/hold"        # open write end; unblocks stub's stdin open
printf '%d' "$STUB_PID" > "$PF"
age_live="$(ops_age_of "$RUN")"
exec 3>&-                 # close write end; stub's read returns EOF; stub exits
wait "$STUB_PID" 2>/dev/null; rm -f "$PF"
is "live pidfile: SP_OPS_AGE is 0" "0" "$age_live"

wait  # reap zombie subshells from earlier command substitutions before exit
tl_summary
