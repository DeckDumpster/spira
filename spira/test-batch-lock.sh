#!/usr/bin/env bash
#
# test-batch-lock.sh — batch.sh and verdict.sh take a per-repo lock; batch.sh
#   checks for unrecorded open spira/queue/* PRs before opening a new one.
#
# CASES:
#   a. Lock held externally: batch.sh exits 0 with "holds the lock" and opens
#      no PR.  Positive control: with lock released, PR opens.
#   b. Forge reports an existing spira/queue/* PR: batch.sh refuses (message +
#      operator mail once) with no pr-create call.
#      Positive control: with pr-list-queue empty, PR opens.
#   c. Lock held externally: verdict.sh exits 0 with "holds the lock".
#
# SEEN RED WITHOUT THE FIX.
#   a: removing the flock block from batch.sh causes the lock-held run to open
#      a PR; the "0 pr-create" assertion fails.
#   b: removing the pr-list-queue check from batch.sh causes a second PR to open
#      despite the forge reporting one; the "0 pr-create" assertion fails.
#   c: removing the flock block from verdict.sh causes the lock-held run to
#      attempt forge calls; the "holds the lock" assertion fails.
#
# defect: sp-qdtnw
# covers: spira/batch.sh spira/verdict.sh spira/forge.sh spira/conf.sh
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
testdb_require test-batch-lock
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up batchlock || { echo "test-batch-lock: could not build fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"
REMOTE="$TMP/remote.git"
RUN="$TMP/run"
SH="$TMP/spira"
REPONAME=fixture-repo
LANDSTATE="$RUN/landstate"
QUEUEDIR="$RUN/queue"
LOCKFILE="$QUEUEDIR/$REPONAME/lock"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/$REPONAME"

cp "$HERE"/*.sh "$SH/"

# Gate stub: always passes, never delays.
cat > "$SH/gate.sh" <<'GSTUB'
#!/usr/bin/env bash
exit 0
GSTUB
chmod +x "$SH/gate.sh"

# Write repo-map: queue mode, pinned to non-default land value.
cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

# ─── Shared log files — exported so fixtures can write to them ───────────────
FORGE_LOG="$TMP/forge-log"
FORGE_QUEUE_PRS="$TMP/forge-queue-prs"
MAIL_LOG="$TMP/mail-log"
export FORGE_LOG FORGE_QUEUE_PRS MAIL_LOG

: > "$FORGE_LOG"
: > "$FORGE_QUEUE_PRS"
: > "$MAIL_LOG"

# ─── Forge fixture ────────────────────────────────────────────────────────────
# pr-create: logs to FORGE_LOG and returns an incrementing PR number.
# pr-list-queue: returns lines from FORGE_QUEUE_PRS (empty by default).
# Pinned to a non-default SPIRA_FORGE so an assertion passing against "used gh"
# fails rather than passing vacuously against a forge that was never reached.
cat > "$SH/forge-fixture.sh" <<'FORGE'
#!/usr/bin/env bash
cmd="${1:-}"; shift; repo="${1:-}"; shift
case "$cmd" in
    main-gate-status) printf 'green deadbeef\n' ;;
    pr-create)
        head="${1:-}"
        n=$(( $(wc -l < "$FORGE_LOG" 2>/dev/null || echo 0) + 1 ))
        printf '%s\n' "$n" >> "$FORGE_LOG"
        printf '%s\n' "$n"
        ;;
    pr-list-queue)
        cat "$FORGE_QUEUE_PRS" 2>/dev/null || true
        ;;
    *) printf 'forge-fixture: unknown: %s\n' "$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"

# Mail stub.
cat > "$SH/mail.sh" <<'MAILSTUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$MAIL_LOG"
MAILSTUB
chmod +x "$SH/mail.sh"

batch() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_MAX=8 \
    SPIRA_QUEUE_BATCH_WAIT=0 \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
        bash "$SH/batch.sh" "$@" 2>&1
}

verdict_run() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
        bash "$SH/verdict.sh" "$@" 2>&1
}

seed() {
    testdb_reset
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
}

plant_bead() {
    printf \
        '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$1" "$1" "$1" | testdb_seed
}

branch() {
    local id="$1" epoch="${2:-$(date +%s)}"
    local base blob tree tip
    base="$(git -C "$REPO" rev-parse main)"
    blob="$(printf '%s\n' "$id" | git -C "$REPO" hash-object -w --stdin)"
    tree="$(printf '100644 blob %s\t%s.txt\n' "$blob" "$id" | git -C "$REPO" mktree)"
    tip="$(git -C "$REPO" commit-tree "$tree" -p "$base" -m "$id: work")"
    [ -n "$tip" ] || { printf 'test-batch-lock: branch setup failed for %s\n' "$id" >&2; exit 1; }
    git -C "$REPO" update-ref "refs/heads/spira/$id" "$tip"
    printf 'CERTIFIED %s %s\n' "$tip" "$epoch" > "$LANDSTATE/$id"
    plant_bead "$id"
}

open_batch_file() { printf '%s/%s/open' "$QUEUEDIR" "$REPONAME"; }
batch_pr()        { grep '^pr=' "$(open_batch_file)" 2>/dev/null | cut -d= -f2; }
pr_create_count() { wc -l < "$FORGE_LOG" 2>/dev/null | tr -d ' ' || printf '0'; }

clean_case() {
    rm -f "$QUEUEDIR/$REPONAME/open"
    rm -f "$RUN/queue-unrecorded-pr-$REPONAME"
    : > "$FORGE_LOG"
    : > "$FORGE_QUEUE_PRS"
    : > "$MAIL_LOG"
    find "$LANDSTATE" -maxdepth 1 -type f 2>/dev/null -delete
    local wt="$RUN/worktree/.batch-$(basename "$REPO")"
    if [ -d "$wt" ]; then
        git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    fi
    rm -rf "$RUN/worktree" && mkdir -p "$RUN/worktree"
    git -C "$REPO" worktree prune 2>/dev/null || true
    git -C "$REPO" for-each-ref --format='%(refname:short)' \
        'refs/heads/spira/*' 'refs/heads/spira/*/*' 2>/dev/null \
        | while read -r br; do
            git -C "$REPO" branch -D "$br" >/dev/null 2>&1 || true
        done
}

echo "test-batch-lock.sh"

# =============================================================================
# POSITIVE CONTROL: batch opens a PR when the lock is free and pr-list-queue
# returns nothing.  Proves the forge is reached and guards do not block normal
# use.
# =============================================================================
seed
for i in $(seq 1 8); do branch "sp-bl0-$i"; done
out="$(batch "$REPONAME")"
is  "positive: exits 0"    "0" "$?"
is  "positive: PR opened"  "1" "$(batch_pr)"
is  "positive: 1 pr-create" "1" "$(pr_create_count)"
clean_case

# =============================================================================
# a. LOCK HELD: batch.sh exits 0 with "holds the lock"; no pr-create call.
#
# The test holds the lock on fd 8.  batch.sh opens a new fd 9 to the same file;
# flock -n 9 fails because the test's OFD holds the exclusive lock.
# =============================================================================
seed
for i in $(seq 1 8); do branch "sp-bla-$i"; done

exec 8>"$LOCKFILE"
flock -n 8 || { echo "test-batch-lock: could not acquire pre-lock for case a"; exit 1; }

out="$(batch "$REPONAME")"
is   "a: exits 0 when lock held"     "0" "$?"
want "a: holds the lock message"     "holds the lock" "$out"
is   "a: no pr-create"               "0" "$(pr_create_count)"

exec 8>&-
clean_case

# =============================================================================
# b. UNRECORDED QUEUE PR: forge reports an existing spira/queue/* PR; batch.sh
#    refuses to open another and mails the operator exactly once.
#
# Positive control: the forge IS reached (pr-list-queue is called), confirming
# the guard is exercised and not bypassed.
# =============================================================================
seed
for i in $(seq 1 8); do branch "sp-blb-$i"; done

printf '99\n' > "$FORGE_QUEUE_PRS"

out="$(batch "$REPONAME")"
is   "b: exits 0 with existing queue PR" "0" "$?"
want "b: open queue PR message"          "open queue PR" "$out"
is   "b: no pr-create call"              "0" "$(pr_create_count)"
want "b: operator mailed"               "unrecorded open queue PR" "$(cat "$MAIL_LOG")"

# Second call: operator not mailed again (flag file suppresses it).
: > "$MAIL_LOG"
batch "$REPONAME" > /dev/null
is "b: operator not mailed twice" "0" "$(wc -l < "$MAIL_LOG" | tr -d ' ')"

: > "$FORGE_QUEUE_PRS"
clean_case

# =============================================================================
# c. VERDICT LOCK: verdict.sh exits 0 with "holds the lock" when lock is held.
# =============================================================================
seed

exec 8>"$LOCKFILE"
flock -n 8 || { echo "test-batch-lock: could not acquire pre-lock for case c"; exit 1; }

vout="$(verdict_run "$REPONAME")"
is   "c: exits 0 when lock held"     "0" "$?"
want "c: holds the lock message"     "holds the lock" "$vout"

exec 8>&-
clean_case

# =============================================================================
# d. STDERR PRESERVED (batch.sh): a >&2 write after lock acquisition must
#    reach the caller.  Uses a repo-map whose landref does not resolve, so
#    batch.sh reaches "cannot resolve base ref" (written >&2) after the lock.
#    SEEN RED WITHOUT THE FIX: exec 9>file 2>/dev/null permanently silences fd
#    2, so the message is lost and the want assertion fails.
# =============================================================================
clean_case

BROKEN_MAP="$TMP/broken-map"
BROKEN_QDIR="$TMP/broken-queue"
mkdir -p "$BROKEN_QDIR/$REPONAME"
printf '%s | %s | queue | origin/nonexistent | | |\n' "$REPONAME" "$REPO" > "$BROKEN_MAP"

dout="$(SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$BROKEN_MAP" \
    SPIRA_QUEUE_DIR="$BROKEN_QDIR" \
    SPIRA_QUEUE_BATCH_MAX=8 \
    SPIRA_QUEUE_BATCH_WAIT=0 \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
        bash "$SH/batch.sh" "$REPONAME" 2>&1)"
want "d: batch stderr after lock reaches caller" "cannot resolve base ref" "$dout"

# =============================================================================
# e. STDERR PRESERVED (verdict.sh): a >&2 write after lock acquisition must
#    reach the caller.  A malformed open-batch file (no pr= field) triggers
#    "malformed batch record" (written >&2) after the lock.
#    SEEN RED WITHOUT THE FIX: exec 9>file 2>/dev/null permanently silences fd
#    2, so the message is lost and the want assertion fails.
# =============================================================================
clean_case
mkdir -p "$QUEUEDIR/$REPONAME"
printf 'branch=test\n' > "$QUEUEDIR/$REPONAME/open"

eout="$(verdict_run "$REPONAME")"
want "e: verdict stderr after lock reaches caller" "malformed batch record" "$eout"
clean_case

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
