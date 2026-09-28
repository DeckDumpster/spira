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
# GAP G11 (deterministic concurrency, case 1). A wall-clock sleep before recording proves
# nothing about a host fast enough to finish runner 1 before runner 2 even attempts the lock.
# The barrier below replaces it: runner 1's mail signals "I am inside the critical
# section, still holding the lock" over a FIFO and then blocks on a second FIFO until the
# test releases it. The driver's blocking read of the first FIFO cannot return before that
# write happens, so runner 2 is only launched once runner 1 is provably still holding the
# lock — true on any host at any speed.
#
# THIS SUITE ALSO ABSORBS THE mail STUB'S OTHER TWO CALLERS (duplicate cluster D8): a
# pool-paused info row must never reach mail (cmd_check skips info rows before the
# escalation path), and a starved row's evidence must carry its own partition's label
# through the full stack, not just through the classifier in isolation.
#
# The fixture uses --from to bypass live graph queries. STRAND_GRACE=0 ensures the strand
# fires immediately without waiting for the 15-minute grace window.
#
# defect: sp-uq55c
# tier: T1
# covers: strand/src/* UC-ops-detection-remediation-21
# hermetic-ok: no systemd, no gh; reads SPIRA_DB for conf.sh schema check only (read-only)
set -uo pipefail
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

# THE STRAND IS A BINARY (strand.sh is gone), invoked by name from the tree's build on PATH.

# COUNT_FILE: each call to our mail stub atomically increments it.
COUNT_FILE="$TMP/count"
echo 0 > "$COUNT_FILE"

# mail stub: records "send" invocations.
mkdir -p "$TMP/strand-home"
cat > "$TMP/strand-home/mail" <<STUB
#!/usr/bin/env bash
[ "\${1:-}" = send ] || exit 0
( flock -x 9; n=\$(cat "$COUNT_FILE"); echo \$((n + 1)) > "$COUNT_FILE" ) 9>"$COUNT_FILE.lock"
cat >/dev/null
STUB
chmod +x "$TMP/strand-home/mail"

# strand check environment: SPIRA_RUN controls STATE path; SPIRA_HOME points to the
# mail stub; SPIRA_STRAND_GRACE=0 disables the 15-minute grace window.
CHECK_ENV=(
    SPIRA_RUN="$TMP/run"
    SPIRA_STRAND_GRACE=0
    SPIRA_LABELS=-
    SPIRA_HOME="$TMP/strand-home" PATH="$TMP/strand-home:$PATH"
)

run_check() {
    env "${CHECK_ENV[@]}" strand check --from "$TMP/fixture.tsv" >/dev/null 2>&1
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

# THE BARRIER. Runner 1's mail (called from inside cmd_check's locked section) writes
# to LOCKED_FIFO and then blocks reading PROCEED_FIFO. The driver's blocking read of
# LOCKED_FIFO cannot return before that write happens, so by the time it does, runner 1 is
# provably still holding the lock (it cannot reach `exec 9>&-` until mail returns).
# Only then does the driver launch runner 2, which must find the lock held.
LOCKED_FIFO="$TMP/locked.fifo"; PROCEED_FIFO="$TMP/proceed.fifo"
mkfifo "$LOCKED_FIFO" "$PROCEED_FIFO"

BARRIER_HOME="$TMP/strand-home-barrier"; mkdir -p "$BARRIER_HOME"
cat > "$BARRIER_HOME/mail" <<STUB
#!/usr/bin/env bash
[ "\${1:-}" = send ] || exit 0
printf locked > "$LOCKED_FIFO"
read -r _ < "$PROCEED_FIFO"
( flock -x 9; n=\$(cat "$COUNT_FILE"); echo \$((n + 1)) > "$COUNT_FILE" ) 9>"$COUNT_FILE.lock"
cat >/dev/null
STUB
chmod +x "$BARRIER_HOME/mail"

env SPIRA_RUN="$TMP/run" SPIRA_STRAND_GRACE=0 SPIRA_LABELS=- SPIRA_HOME="$BARRIER_HOME" PATH="$BARRIER_HOME:$PATH" \
    strand check --from "$TMP/fixture.tsv" >"$TMP/runner1.log" 2>&1 &
P1=$!

# Blocks until runner 1's mail signals it is inside the critical section.
read -r _ < "$LOCKED_FIFO"

# Runner 2 now races for the same lock runner 1 still holds. Deterministically declines.
LOG2="$TMP/runner2.log"
run_check_logged() {
    env "${CHECK_ENV[@]}" strand check --from "$TMP/fixture.tsv" >"$LOG2" 2>&1
}
run_check_logged
P2_rc=$?

# Release runner 1 and wait for it to finish.
printf go > "$PROCEED_FIFO"
wait "$P1"

n="$(cat "$COUNT_FILE")"
is "concurrent runs: exactly 1 escalation" "1" "$n"
is "runner 2 exits 0 while declining" "0" "$P2_rc"
want "runner 2 logs the decline" "declining" "$(cat "$LOG2" 2>/dev/null)"

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

LOG3="$TMP/decline.log"
env "${CHECK_ENV[@]}" strand check --from "$TMP/fixture.tsv" >"$LOG3" 2>/dev/null

# Release the lock so subsequent cleanup can remove the file.
exec 9>&-

if grep -q "declining" "$LOG3" 2>/dev/null; then
    ok "declining runner: decline message logged"
else
    bad "declining runner: decline message logged" "(not found in stdout; got: $(cat "$LOG3" 2>/dev/null))"
fi

# ======================================================================================
echo
echo "case 3 — no mail on pool-paused (D8): an info row never reaches mail:"
# ======================================================================================
# cmd_check skips rows with disposition=info before the escalation path — pool-paused,
# capacity-paused and fleet-saturated all share this. One representative info row proves
# the skip; the classifier-level distinction between them is strand-classify.py's job
# (test-strand-classify.sh), not this suite's.
rm -f "$TMP/run/strands.json" "$TMP/run/strands.json.lock"
printf 'pool-paused\t-\tinfo\t1 bead(s) ready but the task pool is set to zero: sp-example\taeons.sh pool <n>\n' \
    > "$TMP/fixture-info.tsv"

MAIL_SENT="$TMP/mail-sent-info"
env SPIRA_RUN="$TMP/run" SPIRA_STRAND_GRACE=0 SPIRA_LABELS=- SPIRA_HOME="$TMP/strand-home" PATH="$TMP/strand-home:$PATH" \
    strand check --from "$TMP/fixture-info.tsv" >/dev/null 2>&1
n_info="$(cat "$COUNT_FILE")"
is "info row: mail not called (count unchanged)" "1" "$n_info"

# ======================================================================================
echo
echo "case 4 — full stack: a starved row's evidence carries its own partition's label:"
# ======================================================================================
# Exercises strand.sh check's own plumbing (classify()'s --from label-prepending, and
# escalate()'s mail body), not just the classifier in isolation (sp-15u9f).
rm -f "$TMP/run/strands.json" "$TMP/run/strands.json.lock"
printf 'starved\t-\tescalate\t1 bead(s) ready and no live aeon; 0 of 3 aeon slot(s) are serving this partition (1 live across fleet): sp-pa1\tcheck sentinel\n' \
    > "$TMP/fixture-partition.tsv"

ARGS_A="$TMP/mail-args-a"
mkdir -p "$TMP/home-a"
cat > "$TMP/home-a/mail" <<STUB
#!/usr/bin/env bash
[ "\${1:-}" = send ] || exit 0
printf '%s\n' "\$@" >> "$ARGS_A"
cat >> "$ARGS_A"
STUB
chmod +x "$TMP/home-a/mail"

env SPIRA_RUN="$TMP/run" SPIRA_STRAND_GRACE=0 SPIRA_LABELS=spira,plan SPIRA_HOME="$TMP/home-a" PATH="$TMP/home-a:$PATH" \
    strand check --from "$TMP/fixture-partition.tsv" >/dev/null 2>&1

args_a="$(cat "$ARGS_A" 2>/dev/null || true)"
want "full stack: sp-pa1 in evidence"          "sp-pa1"     "$args_a"
want "full stack: title names plan partition"  "spira,plan" "$args_a"

tl_summary
