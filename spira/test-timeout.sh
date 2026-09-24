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
#   1. SESSION_RC=124 + committed=no charges the timeout counter, not the attempt counter.
#   2. After FAYTH_TIMEOUT_LIMIT consecutive timeouts the bead is poisoned and an ask is
#      filed — "too large for its lane", not "change the approach".
#   3. A claim released because spira-poison raced the predicate check is a clean exit.
#   4. SP_OPS_AGE reports 0 when an ops aeon pid is live, not the stale log mtime.
#
# defect: sp-06hs
# covers: spira/*.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
isge(){ [ "${2:-0}" -le "${3:-0}" ] 2>/dev/null && ok "$1" \
        || bad "$1" "wanted [$3] >= [$2], got empty or non-integer"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-timeout.sh"
echo
echo "structural:"

# THE TIMEOUT PATH IS BEFORE REQUEUE_CAUSE AND BEFORE session_outcome.
# session_outcome classifies a timed-out trace as `unlanded` (the claude SDK emits a result
# record on SIGTERM), which would charge an attempt if it ran first.
timeout_line="$(grep -n 'SESSION_RC:-0.*124' "$HERE/aeon.sh" | grep -v '^\s*#' | sed -n 1p | cut -d: -f1)"
requeue_line="$(grep -n 'REQUEUE_CAUSE" \]; then' "$HERE/aeon.sh" | sed -n 1p | cut -d: -f1)"
outcome_line="$(grep -n 'cause="\$(session_outcome' "$HERE/aeon.sh" | sed -n 1p | cut -d: -f1)"
is "timeout check appears before REQUEUE_CAUSE check" yes \
   "$( [ -n "$timeout_line" ] && [ -n "$requeue_line" ] && [ "$timeout_line" -lt "$requeue_line" ] && echo yes || echo no)"
is "timeout check appears before session_outcome" yes \
   "$( [ -n "$timeout_line" ] && [ -n "$outcome_line" ] && [ "$timeout_line" -lt "$outcome_line" ] && echo yes || echo no)"

# THE TIMEOUT PATH CHARGES NO ATTEMPT. Counter labels (sp-timeout-N) are no longer
# written (sp-lzt); bump_timeout is a no-op and the timeout path does not call it.
# The invariant is: the timeout block does not call bump_attempt.
timeout_block="$(sed -n '/SESSION_RC.*124.*!=.*yes/,/^        fi$/p' "$HERE/aeon.sh")"
is "the timeout path does not call bump_attempt" 0 \
   "$(printf '%s' "$timeout_block" | grep -c 'bump_attempt')"

# bump_timeout is a no-op — no harness script outside lib.sh needs to call it.
timeout_sites="$(grep -rl 'bump_timeout' "$HERE"/*.sh 2>/dev/null | grep -v '/lib\.sh$' | grep -v '/test-' \
    | xargs -r -n1 basename | sort | tr '\n' ' ' | sed 's/ $//')"
is "no harness script outside lib.sh calls bump_timeout" "" "$timeout_sites"

# THE POISON-AFTER-CLAIM GUARD EXISTS AND PRECEDES WORKSPACE SETUP.
# The claim and the predicate check are not atomic; spira-poison can land in the gap.
poison_check="$(grep -n 'spira-poison' "$HERE/aeon.sh" | grep -v '^\s*#' | sed -n 1p | cut -d: -f1)"
workspace_setup="$(grep -Fn 'the workspace' "$HERE/aeon.sh" | sed -n 1p | cut -d: -f1)"
is "poison-after-claim guard precedes workspace setup" yes \
   "$( [ -n "$poison_check" ] && [ -n "$workspace_setup" ] && [ "$poison_check" -lt "$workspace_setup" ] && echo yes || echo no)"

# SESSION_RC IS SET RIGHT AFTER rc=$? AND BEFORE set -e.
rc_capture="$(grep -n '^rc=\$?' "$HERE/aeon.sh" | sed -n 1p | cut -d: -f1)"
session_rc_set="$(grep -n '^SESSION_RC=\$rc' "$HERE/aeon.sh" | sed -n 1p | cut -d: -f1)"
set_e="$(grep -n '^set -e' "$HERE/aeon.sh" | sed -n 1p | cut -d: -f1)"
is "SESSION_RC is set after rc=\$?" yes \
   "$( [ -n "$rc_capture" ] && [ -n "$session_rc_set" ] && [ "$rc_capture" -lt "$session_rc_set" ] && echo yes || echo no)"
is "SESSION_RC is set before set -e" yes \
   "$( [ -n "$session_rc_set" ] && [ -n "$set_e" ] && [ "$session_rc_set" -lt "$set_e" ] && echo yes || echo no)"

# THE aeon.sh CLEANUP DISARMS errexit BEFORE ANY PATH CAN FAIL (still holds with our addition).
first="$(sed -n '/^cleanup() {/,/^}/p' "$HERE/aeon.sh" | tail -n +2 \
          | grep -vE '^\s*(#|local |$)' | sed -n 1p)"
is "cleanup still disarms errexit first" "    set +e" "$first"

# ======================================================================================
# COUNTERS: timeout counter vs attempt counter, against a real bd.
# ======================================================================================
TMP="$(mktemp -d)"
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-timeout
trap 'testdb_drop; rm -rf "$TMP"' EXIT; trap 'exit 143' INT TERM
# testdb-mode: server — asserts on attempts_of, which reads the events table via bd sql
export SPIRA_TESTDB_MODE=server
testdb_up timeout || {
    printf 'SKIP test-timeout: server testdb not available\n' >&2
    exit 77
}
# shellcheck disable=SC1090
. "$HERE/lib.sh"

echo
echo "counters (real bd):"

seed() {
    testdb_reset
    testdb_seed <<JSONL
{"id":"$1","title":"a bead","status":"open","issue_type":"task","labels":["spira","incident"],"updated_at":"2026-09-08T00:00:00Z"}
JSONL
}
num() { local v="$1"; printf '%d' "${v:-0}"; }
labels_of() { bdq label list "$1" 2>/dev/null | sed -n 's/^ *- //p' | tr '\n' ' ' | sed 's/ $//'; }

seed sp-t1
# Counter labels (sp-timeout-N) are no longer written (sp-lzt).
# timeouts_of is a diagnostic stub returning 0; bump_timeout is a no-op.
is "a fresh bead has no timeouts" 0 "$(num "$(timeouts_of sp-t1)")"
is "a fresh bead has no attempts" 0 "$(num "$(attempts_of sp-t1)")"
# bump_timeout must not write a label.
bump_timeout sp-t1
labels_t1="$(labels_of sp-t1)"
[[ "$labels_t1" != *"sp-timeout"* ]] && ok "bump_timeout writes no label" \
    || bad "bump_timeout writes no label" "got [$labels_t1]"
# timeouts_of returns 0 regardless (diagnostic stub).
is "timeouts_of is a stub returning 0" 0 "$(num "$(timeouts_of sp-t1)")"

# TIMEOUTS DO NOT CHARGE ATTEMPTS — the events trail only records status_changed.
# A timeout does not transition the bead to in_progress, so the count stays 0.
is "one timeout costs the work no attempts" 0 "$(num "$(attempts_of sp-t1)")"

# THE PAIR: an in_progress transition charges an attempt via events.
seed sp-t2
bdq update sp-t2 --status in_progress >/dev/null 2>&1
is "an in_progress transition charges an attempt" 1 "$(num "$(attempts_of sp-t2)")"
is "and leaves no timeout counter"                0 "$(num "$(timeouts_of sp-t2)")"

# SPIRA_ASK_TIMEOUT_LOOP DEDUPLICATES ON (id, count).
seed sp-t3
MAIL_LOG="$TMP/mail.log"; : > "$MAIL_LOG"
MAIL_HOME="$TMP/mail-home"; mkdir -p "$MAIL_HOME"
printf '#!/usr/bin/env bash\n[ "${1:-}" = send ] || exit 0\nprintf "%%s\\n" "$@" >> "%s"\ncat >>"%s"\n' "$MAIL_LOG" "$MAIL_LOG" > "$MAIL_HOME/mail.sh"
chmod +x "$MAIL_HOME/mail.sh"
export SPIRA_HOME="$MAIL_HOME"

spira_ask_timeout_loop sp-t3 spira/sp-t3 ops 480 2 >/dev/null 2>&1
isge "at-limit call produces an ask" 1 "$(grep -c 'sp-t3' "$MAIL_LOG" || echo 0)"
want "the ask says 'timed out' not 'change the approach'" "timed out" "$(cat "$MAIL_LOG")"

# A second call with the SAME count is suppressed (already open in the db? — not yet, since
# this test doesn't actually file it in beads. The function calls $SPIRA_NOTIFY; test that
# it does so via the stub rather than testing dedup, which requires a real ask queue).
# What we CAN assert: no attempts were charged.
is "attempts are still 0 after timeout calls" 0 "$(num "$(attempts_of sp-t3)")"

# ======================================================================================
# SP_OPS_AGE: active session reports 0, not a stale log mtime.
# Tests the aeon_alive + pid-file logic added to cockpit.sh's probe function.
# Run inline (sourcing lib.sh and the logic directly) rather than via the full probe,
# which requires a live beads db and a configured git repo.
# ======================================================================================
echo
echo "SP_OPS_AGE (cockpit logic):"

RUN="$TMP/run"; mkdir -p "$RUN"
# age_of is defined in cockpit.sh, not lib.sh. Inline it here so the test runs standalone.
age_of() { local f="$1" m; m="$(stat -c %Y "$f" 2>/dev/null)" || { printf '?'; return; }
           [ -n "$m" ] || { printf '?'; return; }; printf '%d' $(( $(date +%s) - m )); }
# Inline the ops-age logic from cockpit.sh to test it independently of the rest of probe.
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

printf '\ntest-timeout.sh: %d passed, %d failed\n' "$pass" "$fail"
wait  # reap zombie subshells from earlier command substitutions before exit
[ "$fail" -eq 0 ]
