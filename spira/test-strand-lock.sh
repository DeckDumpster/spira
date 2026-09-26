#!/usr/bin/env bash
#
# test-strand-lock.sh — two concurrent strand.sh check runs must file exactly one escalation.
#
#   ./test-strand-lock.sh
#
# WHY THIS EXISTS. sp-uq55c: strand.sh's "escalate exactly once per episode" guarantee broke
# under concurrent callers — sentinel + concierge, or operator + timer. Both runners read
# was_escalated=0 from state_apply before either had written escalated=1 via state_mark, so
# both filed the same escalation. Observed 2026-09-10: sp-9dcp3 and sp-km7d0, identical
# title, identical 14 ids, identical evidence, both reached Ryan's queue.
#
# TWO BUGS, ONE FIX:
#   1. The read-modify-write sequence in cmd_check was not locked — a second concurrent runner
#      saw stale state and acted on it. Fixed with flock --nonblock on $STATE.lock; the second
#      runner declines rather than proceeding on stale state.
#   2. Both state_apply and state_mark wrote to the fixed name path+".tmp", shared by every
#      concurrent writer. A partial write by one could be promoted by the other via os.replace.
#      Fixed by using tempfile.mkstemp (unique name per writer, same directory).
#
# PRE-FIX FAILURE (seen against the unfixed tree, 2026-09-10):
#
#   escalation count: 2   (wanted 1)
#
# The fixture uses --from to bypass live graph queries. STRAND_GRACE=0 ensures the strand
# fires immediately without waiting for the 15-minute grace window.
#
# defect: sp-uq55c
# tier: T1
# covers: spira/strand.sh
# hermetic-ok: no systemd, no gh; reads SPIRA_DB for conf.sh schema check only (read-only)
set -uo pipefail
# covers: spira/strand.sh

HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

mkdir -p "$TMP/run"

# A starved partition — one row in strand-classify.py's output format.
# kind=starved id=- disp=escalate → hits the escalation path in cmd_check.
printf 'starved\t-\tescalate\t0 ready beads, 0 live aeons — nothing can move\tfile a needs-ryan ask with the symptoms\n' \
    > "$TMP/fixture.tsv"

echo "sentinel ok" > "$TMP/run/sentinel.log"

# COUNT_FILE: each call to our mail.sh stub atomically increments it.
COUNT_FILE="$TMP/count"
echo 0 > "$COUNT_FILE"

# mail.sh stub: records "send" invocations. sleep 0.2 before recording widens the race
# window between state_apply (read) and state_mark (write) so both concurrent runners
# have time to read was_escalated=0 before either writes escalated=1 — reliably
# reproducing the lost-update on the unfixed tree.
mkdir -p "$TMP/strand-home"
cat > "$TMP/strand-home/mail.sh" <<STUB
#!/usr/bin/env bash
[ "\${1:-}" = send ] || exit 0
sleep 0.2
( flock -x 9; n=\$(cat "$COUNT_FILE"); echo \$((n + 1)) > "$COUNT_FILE" ) 9>"$COUNT_FILE.lock"
cat >/dev/null
STUB
chmod +x "$TMP/strand-home/mail.sh"

# strand.sh check environment: SPIRA_RUN controls STATE path; SPIRA_HOME points to the
# mail.sh stub; SPIRA_STRAND_GRACE=0 disables the 15-minute grace window.
CHECK_ENV=(
    SPIRA_RUN="$TMP/run"
    SPIRA_STRAND_GRACE=0
    SPIRA_LABELS=-
    SPIRA_HOME="$TMP/strand-home"
)

run_check() {
    env "${CHECK_ENV[@]}" bash "$HERE/strand.sh" check --from "$TMP/fixture.tsv" >/dev/null 2>&1
}

echo "test-strand-lock.sh"

# ======================================================================================
echo
echo "case 0 — single run files exactly one escalation (baseline):"
# ======================================================================================
run_check
n="$(cat "$COUNT_FILE")"
is "single run: 1 escalation" "1" "$n"

# ======================================================================================
echo
echo "case 1 — two concurrent runs file exactly one escalation (the lock invariant):"
# ======================================================================================
# Reset state from case 0 so the episode starts fresh.
rm -f "$TMP/run/strands.json" "$TMP/run/strands.json.lock"
echo 0 > "$COUNT_FILE"

run_check &
P1=$!
run_check &
P2=$!
wait "$P1" "$P2"

n="$(cat "$COUNT_FILE")"
is "concurrent runs: exactly 1 escalation" "1" "$n"

# ======================================================================================
echo
echo "case 2 — declining runner logs its reason (law-absence-needs-a-positive-control):"
# ======================================================================================
# A runner that cannot acquire the lock must SAY SO. A silent decline is indistinguishable
# from a runner that never started (law-absence-needs-a-positive-control).
#
# Deterministic setup: hold the lock file directly from the test, then run strand.sh check
# in the foreground. It must decline immediately and log the reason.
rm -f "$TMP/run/strands.json" "$TMP/run/strands.json.lock"

# Acquire the lock fd that strand.sh uses for cmd_check.
exec 9>"$TMP/run/strands.json.lock"
flock -x 9

LOG2="$TMP/decline.log"
env "${CHECK_ENV[@]}" bash "$HERE/strand.sh" check --from "$TMP/fixture.tsv" >"$LOG2" 2>/dev/null

# Release the lock so subsequent cleanup can remove the file.
exec 9>&-

if grep -q "declining" "$LOG2" 2>/dev/null; then
    ok "declining runner: decline message logged"
else
    bad "declining runner: decline message logged" "(not found in stdout; got: $(cat "$LOG2" 2>/dev/null))"
fi
tl_summary
