#!/usr/bin/env bash
#
# test-claim-retry.sh — claim_retry distinguishes a transient bd failure from a real empty
# result, and retries the former a bounded number of times before trusting it.
#
# THE DEFECT (sp-3ntca). Both the claim path (aeon.sh) and the readiness count (fayth_ready,
# via ready_count) discarded bd's exit code and stderr — `bdq ... --json 2>/dev/null |
# json_only | json_count` — so a transient store failure (lock or commit contention, or bd's
# own circuit breaker) read exactly like a query that ran cleanly and matched nothing. Two
# aeons summoned seconds apart raced the same store; one won, and the other's claim errored
# and was reported "nothing ready to claim" with ~90 beads actually ready.
#
# WHAT THIS SUITE DOES NOT USE. No real bd, no testdb: the defect is entirely about how a bd
# FAILURE is threaded through the shell, which a real database cannot be made to produce on
# demand — the same reasoning that has test-bd-lock-retry.sh stub bd directly rather than
# reaching for a fixture that cannot inject a lock error deterministically.
#
# defect: sp-3ntca
# covers: spira/lib.sh spira/aeon.sh
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
# PINNED NON-DEFAULT: proves the delay is actually read from the env rather than a literal
# baked into claim_retry, and keeps the suite fast regardless of the shipped default.
export SPIRA_CLAIM_RETRY_DELAY_S=0
CALLS="$T/calls"
export CALLS

echo "test-claim-retry.sh"

# ==========================================================================================
echo
echo "claim fails once then succeeds — claimed, never read as an error"
# ==========================================================================================
rm -f "$CALLS"
cat > "$SPIRA_BD" <<'STUB'
#!/usr/bin/env bash
n=0
[ -f "$CALLS" ] && n="$(cat "$CALLS")"
n=$((n + 1))
echo "$n" > "$CALLS"
if [ "$n" -eq 1 ]; then
    echo "Error: failed to open database: embeddeddolt: the database is locked by another dolt process" >&2
    exit 1
fi
echo '[{"id":"sp-abc","labels":["repo:fixture"]}]'
STUB
chmod +x "$SPIRA_BD"
out="$(claim_retry ready --limit 0 --claim --label plan)"; rc=$?
is   "retry-then-succeed: rc is 0"                  "0"       "$rc"
want "retry-then-succeed: claimed bead in output"   "sp-abc"  "$out"
is   "retry-then-succeed: bd was called twice"      "2"       "$(cat "$CALLS")"

# ==========================================================================================
echo
echo "claim always errors — retries exhausted: rc 1, CLAIM_RETRY_ERR carries bd's own stderr"
# ==========================================================================================
rm -f "$CALLS"
cat > "$SPIRA_BD" <<'STUB'
#!/usr/bin/env bash
n=0
[ -f "$CALLS" ] && n="$(cat "$CALLS")"
n=$((n + 1))
echo "$n" > "$CALLS"
echo "Error: failed to open database: embeddeddolt: the database is locked by another dolt process" >&2
exit 1
STUB
chmod +x "$SPIRA_BD"
ERRF="$T/claim-err"
out="$(claim_retry ready --limit 0 --claim --label plan 2>"$ERRF")"; rc=$?
errmsg="$(cat "$ERRF" 2>/dev/null)"
is   "always-errors: rc is 1"                                "1"                          "$rc"
is   "always-errors: stdout is empty — never a fabricated count" "" "$out"
want "always-errors: stderr names the lock"                 "locked by another dolt process" "$errmsg"
is   "always-errors: bd was retried SPIRA_CLAIM_RETRIES times" "${SPIRA_CLAIM_RETRIES:-3}" "$(cat "$CALLS")"

# ==========================================================================================
echo
echo "claim returns an empty list — a REAL zero: rc 0, no retry spent on it"
# ==========================================================================================
rm -f "$CALLS"
cat > "$SPIRA_BD" <<'STUB'
#!/usr/bin/env bash
n=0
[ -f "$CALLS" ] && n="$(cat "$CALLS")"
n=$((n + 1))
echo "$n" > "$CALLS"
echo '[]'
STUB
chmod +x "$SPIRA_BD"
out="$(claim_retry ready --limit 0 --claim --label plan)"; rc=$?
is "empty-list: rc is 0"                                    "0"   "$rc"
is "empty-list: bd's own empty result passes through as-is" "[]"  "$out"
is "empty-list: bd was called once — a clean empty is not retried" "1" "$(cat "$CALLS")"

tl_summary
