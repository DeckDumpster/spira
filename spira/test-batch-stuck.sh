#!/usr/bin/env bash
# test-batch-stuck.sh — queue-stuck alert measures movement, not backlog depth.
#
# mark_moved writes without a trailing newline (matching land_mark in lib.sh).
# Readers that use "|| continue" on the read skip every production record and
# leave last_moved=0, triggering the fallback to oldest_epoch and false-positive
# mail. Cases e and g fail against the unfixed reader (law-a-regression-test-must-be-seen-to-fail).
#
#   ctrl. Positive control: old BATCHED + old cert → mail fires (proves stub works).
#   e.    DEEP but MOVING: old cert, recent BATCHED → no mail.
#   f.    STALLED: old cert, old BATCHED (movement stopped long ago) → mail.
#         f2. Recent BATCHED added → flag clears.
#         f3. Recent BATCHED removed → re-fires.
#   g.    Post-landing false positive (sp-wlt1r): recent LANDED + old cert → no mail.
#         Fails pre-fix because the no-newline LANDED record is skipped, last_moved=0,
#         fallback to oldest cert age fires the alert seconds after a landing.
#   h.    Brand-new queue: no BATCHED/LANDED at all → stuck check skipped, no mail
#         (law-a-control-that-cannot-check-must-refuse).
#   i.    IDLE then FRESH CERT (sp-w4tyd, github#321): old BATCHED (idle stretch,
#         nothing waiting), then a single fresh certification → no mail. The stall
#         clock must start at the newer of last-moved and oldest-cert, not at
#         last-moved alone — a queue idle for hours with nothing queued is not
#         "stuck" the instant something is finally certified.
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
    SPIRA_SUITE_STATE_FILE="spira/suite-state" \
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
# No trailing newline — matches land_mark in lib.sh.  A reader using "|| continue"
# would skip this record (read returns non-zero at EOF-without-delimiter).
mark_moved() {
    local id="$1" state="$2" epoch="$3"
    printf '%s none %s' "$state" "$epoch" > "$LANDSTATE/$id"
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
# Old CERTIFIED cert + old BATCHED record (last movement ≥ threshold ago).
# If this fails the "no mail" assertions below are vacuous.
# ============================================================================
seed
branch "sp-ctrl" "$OLD"
mark_moved "sp-ctrl-batched" BATCHED "$OLD"
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
# f. STALLED queue: old certs, last movement old (≥ threshold ago).
#    Expected: mail sent, flag created.
#    f2. Recent movement recorded: flag clears (queue still non-empty).
#    f3. Recent movement removed (stall resumes, old BATCHED is the last record):
#        alert re-fires.
#    Before fix: no-newline records were skipped, last_moved=0, fallback to
#    oldest_epoch; flag never cleared, so f3 was silent.
# ============================================================================
seed
branch "sp-f1" "$OLD"
branch "sp-f2" "$(( OLD + 60 ))"
mark_moved "sp-f-old" BATCHED "$OLD"

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

# f3. Remove recent movement (old BATCHED sp-f-old remains) → re-fires.
rm -f "$LANDSTATE/sp-f-moved"
outF3="$(SPIRA_QUEUE_STUCK_AGE=$STUCK_AGE SPIRA_QUEUE_BATCH_MAX=$BATCH_MAX \
    SPIRA_QUEUE_BATCH_WAIT=$BATCH_WAIT batch "$REPONAME" 2>&1)"
want "f3. re-stall: alert re-fires" "mailed operator" "$outF3"
is   "f3. re-stall: flag re-created" "1" \
    "$([ -f "$RUN/queue-stuck-$REPONAME" ] && echo 1 || echo 0)"
clean_case

# ============================================================================
# g. Post-landing false positive (sp-wlt1r recurrence): recent LANDED record
#    (no trailing newline, land_mark format) + old CERTIFIED cert → no mail.
#    Before fix: no-newline LANDED record skipped, last_moved=0, fallback to
#    oldest cert epoch (~7200s), stuck mail fires seconds after a landing.
# ============================================================================
seed
branch "sp-g1" "$OLD"
mark_moved "sp-g-landed" LANDED "$RECENT"

outG="$(SPIRA_QUEUE_STUCK_AGE=$STUCK_AGE SPIRA_QUEUE_BATCH_MAX=$BATCH_MAX \
    SPIRA_QUEUE_BATCH_WAIT=$BATCH_WAIT batch "$REPONAME" 2>&1)"
nowant "g. post-landing: no stuck mail" "mailed operator" "$outG"
is    "g. post-landing: stuck flag absent" "0" \
    "$([ -f "$RUN/queue-stuck-$REPONAME" ] && echo 1 || echo 0)"
clean_case

# ============================================================================
# h. Brand-new queue: certified certs, no BATCHED/LANDED history.
#    Cannot measure stall — stuck check skipped (law-a-control-that-cannot-check-must-refuse).
#    Before fix: fell back to oldest cert age, mailed immediately on first batch run.
# ============================================================================
seed
branch "sp-h1" "$OLD"

outH="$(SPIRA_QUEUE_STUCK_AGE=$STUCK_AGE SPIRA_QUEUE_BATCH_MAX=$BATCH_MAX \
    SPIRA_QUEUE_BATCH_WAIT=$BATCH_WAIT batch "$REPONAME" 2>&1)"
nowant "h. no-history: no stuck mail" "mailed operator" "$outH"
want   "h. no-history: skipped log"   "no BATCHED/LANDED record" "$outH"
clean_case

# ============================================================================
# i. IDLE then FRESH CERT (sp-w4tyd): old BATCHED far in the past (idle stretch,
#    nothing was waiting), then one branch certified moments ago.
#    Expected: no mail — nothing has actually been waiting.
#    Before fix: stuck_age measured from last_moved alone (~7200s ≥ threshold),
#    paging the operator seconds after the first certification of the day.
# ============================================================================
seed
mark_moved "sp-i-idle" BATCHED "$OLD"
branch "sp-i1" "$RECENT"

outI="$(SPIRA_QUEUE_STUCK_AGE=$STUCK_AGE SPIRA_QUEUE_BATCH_MAX=$BATCH_MAX \
    SPIRA_QUEUE_BATCH_WAIT=$BATCH_WAIT batch "$REPONAME" 2>&1)"
nowant "i. idle-then-fresh-cert: no stuck mail" "mailed operator" "$outI"
is    "i. idle-then-fresh-cert: stuck flag absent" "0" \
    "$([ -f "$RUN/queue-stuck-$REPONAME" ] && echo 1 || echo 0)"
clean_case

printf '\nresults: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
