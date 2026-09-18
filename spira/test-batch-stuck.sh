#!/usr/bin/env bash
# test-batch-stuck.sh — queue-stuck alert measures movement, not backlog depth.
#
# Two cases, each written to fail against the code that preceded this fix:
#
#   e. DEEP but MOVING queue: oldest cert is old, BATCHED entry is recent → no mail.
#      Current (pre-fix) code mails because it checks cert age, not movement time.
#      law-a-regression-test-must-be-seen-to-fail: assert NO mail; old code mails.
#
#   f. STALLED queue: old certs, no recent BATCHED/LANDED → mail sent.
#      Sub-case f2: movement recorded → flag clears (queue still non-empty).
#      Sub-case f3: queue stalls again → alert re-fires.
#      Current (pre-fix) code: flag only clears when queue empties, so f3 is silent.
#      law-a-regression-test-must-be-seen-to-fail: assert re-fire; old code is silent.
#
# Positive control: case f (stalled → mails) must pass on current code too; if the
# mail stub never fires, "no mail" in case e is vacuous (law-absence-needs-a-positive-control).
#
# covers: spira/batch.sh
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-batch-stuck
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up batch || { echo "test-batch-stuck: could not build fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"
REMOTE="$TMP/remote.git"
RUN="$TMP/run"
SH="$TMP/spira"
REPONAME=fixture-repo
LANDSTATE="$RUN/landstate"
QUEUEDIR="$RUN/queue"
MAIL_LOG="$TMP/mail.log"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/$REPONAME"

cp "$HERE"/*.sh "$SH/"

# Mail stub: records the call, always exits 0.  The real mail.sh writes to
# $SPIRA_MAIL; the stub writes to $MAIL_LOG so assertions can count calls
# without touching any real mailbox.
: > "$MAIL_LOG"
cat > "$SH/mail.sh" <<MSTUB
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$MAIL_LOG"
exit 0
MSTUB
chmod +x "$SH/mail.sh"

# Gate stub: unused in these cases (batch trigger is suppressed), kept for
# completeness so batch.sh's gate seam resolves.
cat > "$SH/gate.sh" <<'GSTUB'
#!/usr/bin/env bash
exit 0
GSTUB
chmod +x "$SH/gate.sh"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

B() { bd -C "$SPIRA_DB" "$@"; }

batch() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_SUITE_STATE="spira/suite-state" \
        bash "$SH/batch.sh" "$@" 2>&1
}

seed() {
    testdb_reset
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
}

# branch <id> <epoch> — create git branch + CERTIFIED landstate at the given epoch.
branch() {
    local id="$1" epoch="$2"
    local wt="$RUN/worktree/$id"
    git -C "$REPO" worktree add -q -b "spira/$id" "$wt" main 2>/dev/null || true
    printf '%s\n' "$id" > "$wt/$id.txt"
    git -C "$wt" add -A
    git -C "$wt" commit -q -m "$id: work"
    local tip; tip="$(git -C "$REPO" rev-parse "spira/$id")"
    printf 'CERTIFIED %s %s\n' "$tip" "$epoch" > "$LANDSTATE/$id"
    printf \
        '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$id" "$id" "$id" | testdb_seed
}

# mark_moved <id> <state> <epoch> — write a BATCHED or LANDED record in landstate.
# These have no git branch; queue_certified_list skips them.  The stuck-check
# loop reads them to determine when the queue last made progress.
mark_moved() {
    local id="$1" state="$2" epoch="$3"
    printf '%s none %s\n' "$state" "$epoch" > "$LANDSTATE/$id"
}

clean_case() {
    rm -f "$QUEUEDIR/$REPONAME/open"
    rm -f "$RUN/queue-stuck-$REPONAME"
    : > "$MAIL_LOG"
    local f
    for f in "$LANDSTATE"/*; do [ -f "$f" ] && rm -f "$f"; done
    git -C "$REPO" branch \
        | grep 'spira/' | tr -d ' ' \
        | xargs -r git -C "$REPO" branch -D 2>/dev/null || true
    local wt
    for wt in "$RUN/worktree"/sp-*; do
        [ -d "$wt" ] && git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    done
}

mail_count() { grep -c 'send operator' "$MAIL_LOG" 2>/dev/null || echo 0; }

NOW="$(date +%s)"
OLD=$(( NOW - 7200 ))   # two hours ago — exceeds the default threshold
RECENT=$(( NOW - 30 ))  # 30 seconds ago — well within any threshold

# Use a short stuck threshold so the test does not depend on wall time.
STUCK_AGE=300
# Suppress the batch trigger so batch.sh stops after the stuck check.
BATCH_MAX=100
BATCH_WAIT=86400

# ============================================================================
# POSITIVE CONTROL — confirm the mail stub fires for a genuinely stalled queue.
# If this fails the "no mail" assertion in case e is vacuous.
# ============================================================================
seed
branch "sp-ctrl" "$OLD"
# No BATCHED/LANDED entries → stuck_age ≥ threshold.
ctrl_out="$(SPIRA_QUEUE_STUCK_AGE=$STUCK_AGE SPIRA_QUEUE_BATCH_MAX=$BATCH_MAX \
    SPIRA_QUEUE_BATCH_WAIT=$BATCH_WAIT batch "$REPONAME" 2>&1)"
want "ctrl: stalled queue sends mail" "mailed operator" "$ctrl_out"
is   "ctrl: mail stub was called"  "1" "$(mail_count)"
clean_case

# ============================================================================
# e. DEEP but MOVING queue: old certs, recent BATCHED entry.
#    Expected: no mail (stuck_age < threshold).
#    Before fix: mails (age of oldest cert = 7200s ≥ threshold=300s).
# ============================================================================
seed
branch "sp-e1" "$OLD"
branch "sp-e2" "$(( OLD + 60 ))"
mark_moved "sp-e-batched" BATCHED "$RECENT"

outE="$(SPIRA_QUEUE_STUCK_AGE=$STUCK_AGE SPIRA_QUEUE_BATCH_MAX=$BATCH_MAX \
    SPIRA_QUEUE_BATCH_WAIT=$BATCH_WAIT batch "$REPONAME" 2>&1)"
nowant "e. deep+moving: no stuck mail" "mailed operator" "$outE"
is    "e. deep+moving: stuck flag absent" "0" \
    "$([ -f "$RUN/queue-stuck-$REPONAME" ] && echo 1 || echo 0)"
clean_case

# ============================================================================
# f. STALLED queue: old certs, no recent movement.
#    Expected: mail sent, flag created.
#    f2. After movement recorded: flag clears (queue still non-empty).
#    f3. Movement gone again (another stall): alert re-fires.
#    Before fix: flag never clears until queue empties, so f3 is silent.
# ============================================================================
seed
branch "sp-f1" "$OLD"
branch "sp-f2" "$(( OLD + 60 ))"
# No BATCHED/LANDED entries.

outF="$(SPIRA_QUEUE_STUCK_AGE=$STUCK_AGE SPIRA_QUEUE_BATCH_MAX=$BATCH_MAX \
    SPIRA_QUEUE_BATCH_WAIT=$BATCH_WAIT batch "$REPONAME" 2>&1)"
want "f. stalled: mail sent"         "mailed operator" "$outF"
is   "f. stalled: flag created" "1" \
    "$([ -f "$RUN/queue-stuck-$REPONAME" ] && echo 1 || echo 0)"

# f2. Add a recent BATCHED entry (movement), call again → flag clears.
mark_moved "sp-f-moved" BATCHED "$RECENT"
outF2="$(SPIRA_QUEUE_STUCK_AGE=$STUCK_AGE SPIRA_QUEUE_BATCH_MAX=$BATCH_MAX \
    SPIRA_QUEUE_BATCH_WAIT=$BATCH_WAIT batch "$REPONAME" 2>&1)"
is "f2. movement: flag cleared" "0" \
    "$([ -f "$RUN/queue-stuck-$REPONAME" ] && echo 1 || echo 0)"
nowant "f2. movement: no duplicate mail" "mailed operator" "$outF2"

# f3. Remove movement record (old stall resumes), call again → re-fires.
rm -f "$LANDSTATE/sp-f-moved"
outF3="$(SPIRA_QUEUE_STUCK_AGE=$STUCK_AGE SPIRA_QUEUE_BATCH_MAX=$BATCH_MAX \
    SPIRA_QUEUE_BATCH_WAIT=$BATCH_WAIT batch "$REPONAME" 2>&1)"
want "f3. re-stall: alert re-fires" "mailed operator" "$outF3"
is   "f3. re-stall: flag re-created" "1" \
    "$([ -f "$RUN/queue-stuck-$REPONAME" ] && echo 1 || echo 0)"
clean_case

printf '\nresults: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
