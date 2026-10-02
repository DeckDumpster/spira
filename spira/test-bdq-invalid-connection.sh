#!/usr/bin/env bash
#
# test-bdq-invalid-connection.sh — bdq retries once on a dropped pooled Dolt connection,
# and only on that failure.
#
# THE DEFECT (sp-ydog2). Under load a pooled connection Dolt already dropped surfaces to the
# Go driver as "invalid connection" on the next query — the query never reached the server, so
# retrying it is exactly as safe as the first attempt. bd itself did not retry, so every
# caller of bdq (queue-watch's "bd show" among them) saw the raw failure for the ~minute it
# took the pool to notice the connection was dead. claim_retry (test-claim-retry.sh) already
# retries the claim path on ANY failure for a different reason (claim contention, sp-3ntca);
# this suite is about the other bdq callers, which have no such retry.
#
# WHAT THIS SUITE DOES NOT USE. No real bd, no testdb: the defect is entirely about how bdq
# threads a specific error string through the shell, which a real database cannot be made to
# produce on demand — the same reasoning test-claim-retry.sh and test-install-dolt-ready.sh
# give for stubbing bd directly.
#
# PROPERTIES UNDER TEST
#   1. A call that fails once with "invalid connection" then succeeds is retried and its
#      result passes through as if the first attempt never happened.
#   2. POSITIVE CONTROL. An error that is NOT "invalid connection" is not retried — exactly
#      one attempt, and bd's own stderr reaches the caller.
#   3. POSITIVE CONTROL (bounded retry). A connection that never recovers is retried exactly
#      once, not forever, and the final failure's stderr reaches the caller.
#
# defect: sp-ydog2
# tier: T1
# covers: spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/run"

# SPIRA_CONF at a nonexistent path so no host config leaks verdicts into the suite
# (law-gates-run-in-a-clean-environment).
export SPIRA_HOME="$HERE"
export SPIRA_RUN="$T/run"
export SPIRA_CONF="$T/no-such.conf"
export SPIRA_DB="$T/db"
# shellcheck disable=SC1090
. "$HERE/lib.sh"

BIN="$T/bin"; mkdir -p "$BIN"
export SPIRA_BD="$BIN/bd"
CALLS="$T/calls"
export CALLS

echo "test-bdq-invalid-connection.sh"

# ==========================================================================================
echo
echo "1. invalid connection once, then succeeds — retried once, result passes through clean"
# ==========================================================================================
rm -f "$CALLS"
cat > "$SPIRA_BD" <<'STUB'
#!/usr/bin/env bash
n=0
[ -f "$CALLS" ] && n="$(cat "$CALLS")"
n=$((n + 1))
echo "$n" > "$CALLS"
if [ "$n" -eq 1 ]; then
    echo 'Error: failed to open database: failed to check if database "spira" exists on server 127.0.0.1:3307: invalid connection' >&2
    exit 1
fi
echo '{"id":"sp-abc"}'
STUB
chmod +x "$SPIRA_BD"
out="$(bdq show sp-abc)"; rc=$?
is   "retry-then-succeed: rc is 0"                "0"            "$rc"
want "retry-then-succeed: bd's second result"     "sp-abc"       "$out"
is   "retry-then-succeed: bd was called twice"    "2"            "$(cat "$CALLS")"

# ==========================================================================================
echo
echo "2. POSITIVE CONTROL — an unrelated error is not retried, exactly one call"
# ==========================================================================================
rm -f "$CALLS"
cat > "$SPIRA_BD" <<'STUB'
#!/usr/bin/env bash
n=0
[ -f "$CALLS" ] && n="$(cat "$CALLS")"
n=$((n + 1))
echo "$n" > "$CALLS"
echo "Error: bead sp-abc not found" >&2
exit 1
STUB
chmod +x "$SPIRA_BD"
ERRF="$T/err2"
out="$(bdq show sp-abc 2>"$ERRF")"; rc=$?
errmsg="$(cat "$ERRF" 2>/dev/null)"
is   "other-error: rc is 1"                          "1"  "$rc"
is   "other-error: stdout is empty"                  ""   "$out"
want "other-error: stderr names the real error"      "bead sp-abc not found" "$errmsg"
is   "other-error: bd was called exactly once — no blind retry" "1" "$(cat "$CALLS")"

# ==========================================================================================
echo
echo "3. POSITIVE CONTROL — invalid connection every time: bounded to one retry, not forever"
# ==========================================================================================
rm -f "$CALLS"
cat > "$SPIRA_BD" <<'STUB'
#!/usr/bin/env bash
n=0
[ -f "$CALLS" ] && n="$(cat "$CALLS")"
n=$((n + 1))
echo "$n" > "$CALLS"
echo 'Error: failed to open database: invalid connection' >&2
exit 1
STUB
chmod +x "$SPIRA_BD"
ERRF="$T/err3"
out="$(bdq show sp-abc 2>"$ERRF")"; rc=$?
errmsg="$(cat "$ERRF" 2>/dev/null)"
is   "bounded-retry: rc is 1"                       "1"  "$rc"
is   "bounded-retry: stdout is empty"                ""   "$out"
want "bounded-retry: stderr carries the last attempt's error" "invalid connection" "$errmsg"
is   "bounded-retry: bd was called exactly twice, not forever" "2" "$(cat "$CALLS")"

tl_summary
