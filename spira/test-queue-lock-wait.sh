#!/usr/bin/env bash
#
# test-queue-lock-wait.sh — verdict.sh and batch.sh wait for the per-repo queue lock
#   (flock -w) instead of skipping on the first miss, and count consecutive misses so a
#   real gridlock announces itself instead of blending into the noise (sp-7w54q).
#
# CASES:
#   a. WAIT SUCCEEDS (verdict.sh): lock released before SPIRA_QUEUE_LOCK_WAIT expires —
#      verdict.sh acquires it and completes instead of skipping its turn.
#   b. WAIT SUCCEEDS (batch.sh): same guard, exercised through batch.sh.
#   c. STARVATION COUNTER: consecutive flock timeouts increment lock-skips; a
#      successful acquisition resets the counter.
#   d. STARVATION SIGNAL fires exactly once at the threshold, not on every tick above it.
#
# note: test-batch-lock.sh, which this replaces, was deleted for flipping under a
#   16-wide full-corpus run (law-a-test-that-flips-is-deleted). This suite avoids that
#   suite's git-branch/worktree churn entirely: verdict.sh's lock check runs before any
#   branch is touched, so no branch, worktree or forge PR traffic is needed to exercise it.
#
# SEEN RED WITHOUT THE FIX.
#   a, b: reverting to flock -n prints "holds the lock" even though the holder releases a
#      moment later — the nowant assertion fails.
#   c: removing the lock-skips file writes leaves the counter at 0 always.
#   d: using -ge instead of -eq re-fires the starvation line on every tick past threshold.
#
# defect: sp-7w54q
# covers: spira/verdict.sh spira/batch.sh spira/conf.sh
# timeout: 60
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"
REMOTE="$TMP/remote.git"
RUN="$TMP/run"
SH="$TMP/spira"
REPONAME=fixture-repo
QUEUEDIR="$RUN/queue"
LOCKFILE="$QUEUEDIR/$REPONAME/lock"
SKIPS_FILE="$QUEUEDIR/$REPONAME/lock-skips"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH" "$QUEUEDIR/$REPONAME"

cp "$HERE"/*.sh "$SH/"

# Pinned to a non-default base ref (origin/main, not the default land ref) so an
# assertion that passed against the shipped default would not pass here vacuously.
cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

# Forge fixture: only main-gate-status is ever reached on the paths these cases
# exercise (no open batch, no attributing PR); anything else is a caller bug.
cat > "$SH/forge-fixture.sh" <<'FORGE'
#!/usr/bin/env bash
cmd="${1:-}"
case "$cmd" in
    main-gate-status) printf 'green deadbeef\n' ;;
    *) exit 1 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"

run_env() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_MAX=8 SPIRA_QUEUE_BATCH_WAIT=0 \
    SPIRA_QUEUE_LOCK_WAIT="${LOCK_WAIT:-90}" \
    SPIRA_QUEUE_LOCK_STARVE_MAX="${STARVE_MAX:-5}" \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
        bash "$SH/$1" "$REPONAME" 2>&1
}
verdict_run() { run_env verdict.sh; }
batch_run()   { run_env batch.sh; }

clean_case() { rm -f "$SKIPS_FILE"; }

# =============================================================================
# a. WAIT SUCCEEDS (verdict.sh): a holder releases the lock well inside
#    SPIRA_QUEUE_LOCK_WAIT — verdict.sh must wait for it, not skip.
#
# POSITIVE CONTROL (first): with no holder at all, verdict.sh completes clean and
# never prints "holds the lock" — proves the base case is silent before testing that
# a released-in-time holder is also silent.
# =============================================================================
out0="$(LOCK_WAIT=5 verdict_run)"
nowant "positive control: verdict clean with no holder" "holds the lock" "$out0"
is     "positive control: verdict exits 0 with no holder" "0" "$?"
clean_case

mkfifo "$TMP/ready_a"
( exec 8>"$LOCKFILE"; flock -n 8; printf x > "$TMP/ready_a"; sleep 1 ) &
HOLDER_A=$!
read -r _ < "$TMP/ready_a"

outa="$(LOCK_WAIT=5 verdict_run)"
rca=$?
wait "$HOLDER_A"
nowant "a: verdict does not skip when lock is released in time" "holds the lock" "$outa"
is     "a: verdict exits 0 after waiting"                       "0" "$rca"
clean_case

# =============================================================================
# b. WAIT SUCCEEDS (batch.sh): same guard, exercised through batch.sh's copy of it.
# =============================================================================
mkfifo "$TMP/ready_b"
( exec 8>"$LOCKFILE"; flock -n 8; printf x > "$TMP/ready_b"; sleep 1 ) &
HOLDER_B=$!
read -r _ < "$TMP/ready_b"

outb="$(LOCK_WAIT=5 batch_run)"
rcb=$?
wait "$HOLDER_B"
nowant "b: batch does not skip when lock is released in time" "holds the lock" "$outb"
is     "b: batch exits 0 after waiting"                        "0" "$rcb"
clean_case

# =============================================================================
# c. STARVATION COUNTER: consecutive flock timeouts increment lock-skips; a
#    successful acquisition resets it (absent file == 0).
# =============================================================================
exec 8>"$LOCKFILE"
flock -n 8 || bail "could not acquire pre-lock for case c"

LOCK_WAIT=1 verdict_run > /dev/null
is "c: first skip writes counter=1" "1" "$(cat "$SKIPS_FILE" 2>/dev/null || printf '0')"

LOCK_WAIT=1 verdict_run > /dev/null
is "c: second skip writes counter=2" "2" "$(cat "$SKIPS_FILE" 2>/dev/null || printf '0')"

exec 8>&-
LOCK_WAIT=1 verdict_run > /dev/null
is "c: success resets counter (file absent)" "0" "$(cat "$SKIPS_FILE" 2>/dev/null | wc -c | tr -d ' ')"
clean_case

# =============================================================================
# d. STARVATION SIGNAL fires exactly once at the threshold, not on every tick
#    above it.
#
# POSITIVE CONTROL (first case here): below threshold the plain "holds the lock"
# message prints and "starvation" does not — proves the matcher can tell the two
# messages apart before relying on its absence/presence at and above threshold.
# =============================================================================
exec 8>"$LOCKFILE"
flock -n 8 || bail "could not acquire pre-lock for case d"

hout1="$(LOCK_WAIT=1 STARVE_MAX=3 verdict_run)"
nowant "positive control: run 1 — no starvation signal below threshold" "starvation" "$hout1"
want   "positive control: run 1 — normal holds-the-lock message"        "holds the lock" "$hout1"

hout2="$(LOCK_WAIT=1 STARVE_MAX=3 verdict_run)"
nowant "d: run 2 — still no starvation signal below threshold" "starvation" "$hout2"

hout3="$(LOCK_WAIT=1 STARVE_MAX=3 verdict_run)"
want   "d: run 3 — starvation signal fires at threshold" "starvation" "$hout3"

hout4="$(LOCK_WAIT=1 STARVE_MAX=3 verdict_run)"
nowant "d: run 4 — starvation signal does not re-fire above threshold" "starvation" "$hout4"

exec 8>&-
clean_case

tl_summary
