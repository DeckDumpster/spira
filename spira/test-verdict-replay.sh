#!/usr/bin/env bash
#
# test-verdict-replay.sh — verdict.sh's red-attribution replay mechanics: TERM
# during a live replay, and MAXPAR bounding how many replays run at once.
#
# Demoted from test-verdict.sh cases 21-23 (docs/test-plan/landing-merge-queue.md
# UC-49): those two wall-clock cases used a real 8s sleep per member, three
# members, run twice (parallel then serial) — 8s×3×2 on every single push. The
# parallel/serial comparison also depended on a wall-clock ratio (parallel ≤ 85%
# of serial), which is exactly the class of assertion this harness has deleted
# suites over when load made the ratio flip (law-a-test-that-flips-is-deleted).
#
# Cases 22/23 are replaced here by a concurrency counter: the repro stub records
# how many copies of itself are alive at once, under a real flock, and sleeps
# 1s instead of 8s. "Parallel, up to MAXPAR" becomes "the observed peak equals
# MAXPAR" — a deterministic count instead of a load-sensitive ratio, and six
# times faster. This suite is off the per-push path (tier T3): batch.sh's own
# per-member concurrency is exercised on every real batch at the batch/main
# stage, not on every branch's certification.
#
# tier: T3
# covers: spira/verdict.sh UC-landing-merge-queue-49
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()    { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()   { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()    { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()  { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-verdict-replay
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up verdict || { echo "test-verdict-replay: could not build fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"
REMOTE="$TMP/remote.git"
RUN="$TMP/run"
SH="$TMP/spira"
REPONAME=fixture-repo
LANDSTATE="$RUN/landstate"
QUEUEDIR="$RUN/queue"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/$REPONAME"

cp "$HERE"/*.sh "$HERE"/*.py "$SH/"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

FORGE_LOG="$TMP/forge-log"
FORGE_STATUS_FILE="$TMP/forge-status"
FORGE_RUN_METADATA_FILE="$TMP/forge-run-metadata"
MAIL_LOG="$TMP/mail-log"
SUITES_LOG="$TMP/suites-log"
export FORGE_LOG FORGE_STATUS_FILE FORGE_RUN_METADATA_FILE MAIL_LOG SUITES_LOG

: > "$FORGE_LOG"; : > "$FORGE_RUN_METADATA_FILE"; : > "$MAIL_LOG"; : > "$SUITES_LOG"

cat > "$SH/forge-fixture.sh" <<'FORGE'
#!/usr/bin/env bash
cmd="${1:-}"; shift; repo="${1:-}"; shift
case "$cmd" in
    check-status) cat "${FORGE_STATUS_FILE}" 2>/dev/null || printf 'pending\n' ;;
    run-id) printf 'run-99\n' ;;
    run-metadata) cat "${FORGE_RUN_METADATA_FILE}" 2>/dev/null || true ;;
    run-cancel) printf '%s\tcancel\n' "${1:-}" >> "$FORGE_LOG" ;;
    workflow-rerun) printf '%s\trerun\n' "${1:-}" >> "$FORGE_LOG" ;;
    pr-close) printf '%s\tclose\n' "${1:-}" >> "$FORGE_LOG" ;;
    *) printf 'forge-fixture: unknown command: %s\n' "$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"

cat > "$SH/mail.sh" <<'MAIL'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$MAIL_LOG"
cat >> "$MAIL_LOG"
MAIL
chmod +x "$SH/mail.sh"

cat > "$SH/suites.sh" <<'SUITES'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$SUITES_LOG"
SUITES
chmod +x "$SH/suites.sh"

verdict() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_CI_MAXSEC=3600 \
    SPIRA_QUEUE_CI_IDLE_SEC=600 \
    SPIRA_QUEUE_INFRA_RETRIES=2 \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
        bash "$SH/verdict.sh" "$@" 2>&1
}

batch_file() { printf '%s/%s/open' "$QUEUEDIR" "$REPONAME"; }

build_batch() {
    local base_sha; base_sha="$(git -C "$REPO" rev-parse origin/main)"
    local wt="$RUN/worktree/.batch-build"
    git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    git -C "$REPO" worktree add -q --detach "$wt" "$base_sha"
    local members=()
    for id in "$@"; do
        local bwt="$RUN/worktree/$id"
        rm -rf "$bwt"
        git -C "$REPO" worktree add -q -b "spira/$id" "$bwt" origin/main 2>/dev/null || true
        printf '%s\n' "$id" > "$bwt/$id.txt"
        git -C "$bwt" add -A
        git -C "$bwt" commit -q -m "$id: work"
        local tip; tip="$(git -C "$REPO" rev-parse "spira/$id")"
        git -C "$wt" merge -q --no-edit --no-ff -m "spira: land $id" "$tip" >/dev/null 2>&1
        members+=("$id:$tip")
        printf 'BATCHED %s %s\n' "$tip" "$(date +%s)" > "$LANDSTATE/$id"
    done
    local batch_head; batch_head="$(git -C "$wt" rev-parse HEAD)"
    git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    {
        printf 'pr=42\nhead=%s\nbase=%s\nmembers=%s\nopened=%s\nbranch=spira/queue/test\n' \
            "$batch_head" "$base_sha" "${members[*]}" "$(date +%s)"
    } > "$(batch_file)"
    printf '%s\n' "$batch_head"
}

clean_case() {
    rm -f "$(batch_file)"
    rm -f "$QUEUEDIR/$REPONAME/bisect"
    : > "$FORGE_LOG"; : > "$FORGE_RUN_METADATA_FILE"; : > "$MAIL_LOG"; : > "$SUITES_LOG"
    printf 'pending\n' > "$FORGE_STATUS_FILE"
    find "$LANDSTATE" -maxdepth 1 -type f -delete 2>/dev/null || true
    local wt
    for wt in "$RUN/worktree"/*; do
        [ -d "$wt" ] || continue
        git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    done
    rm -rf "$RUN/worktree" && mkdir -p "$RUN/worktree"
    git -C "$REPO" worktree prune 2>/dev/null || true
    git -C "$REPO" for-each-ref --format='%(refname:short)' 'refs/heads/spira/*' 2>/dev/null \
        | while read -r br; do git -C "$REPO" branch -D "$br" 2>/dev/null || true; done
}

echo "test-verdict-replay.sh"

# =============================================================================
# 21. TERM TRAP: a verdict.sh process interrupted by TERM during attribution
#     writes "attribution of PR ... interrupted after ...s" to landing.log.
#
#     POSITIVE CONTROL: the repro stub writes its own PID to a flag file before
#     sleeping. The test waits for that file before sending TERM, proving that
#     TERM arrives while the replay is genuinely in progress.
# =============================================================================
_repro21_pid_file="$RUN/repro21-pid"
rm -f "$_repro21_pid_file"

cat > "$SH/repro-slow.sh" <<REPRO
#!/usr/bin/env bash
printf '%s\n' "\$\$" > "$_repro21_pid_file"
sleep 60
REPRO
chmod +x "$SH/repro-slow.sh"

base_sha21t="$(git -C "$REPO" rev-parse origin/main)"
bwt21t="$RUN/worktree/sp-vd-t1"
git -C "$REPO" worktree add -q -b "spira/sp-vd-t1" "$bwt21t" origin/main 2>/dev/null || true
printf 'sp-vd-t1\n' > "$bwt21t/sp-vd-t1.txt"
git -C "$bwt21t" add -A
git -C "$bwt21t" commit -q -m "sp-vd-t1: work"
tip_t1="$(git -C "$REPO" rev-parse "spira/sp-vd-t1")"
printf 'BATCHED %s %s\n' "$tip_t1" "$(date +%s)" > "$LANDSTATE/sp-vd-t1"
{ printf 'pr=99\nhead=%s\nbase=%s\nmembers=sp-vd-t1:%s\nopened=%s\n' \
    "$tip_t1" "$base_sha21t" "$tip_t1" "$(date +%s)"; } > "$(batch_file)"
printf 'red\nred-suite: test-slow-suite.sh\n' > "$FORGE_STATUS_FILE"
: > "$RUN/landing.log"

SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
SPIRA_REPO_MAP="$SH/repo-map" \
SPIRA_QUEUE_DIR="$QUEUEDIR" \
SPIRA_FORGE="$SH/forge-fixture.sh" \
SPIRA_QUEUE_REPRO_BATCH="$SH/repro-slow.sh" \
    bash "$SH/verdict.sh" "$REPONAME" &
vd_pid=$!

_w=0
while [ ! -f "$_repro21_pid_file" ] && [ "$_w" -lt 100 ]; do
    sleep 0.1; _w=$((_w+1))
done
if [ -f "$_repro21_pid_file" ]; then
    ok "21. TERM trap: positive control — repro started before TERM"
else
    bad "21. TERM trap: positive control — repro started before TERM" \
        "repro stub never wrote PID file (verdict may have exited early)"
fi

_repro_pid="$(cat "$_repro21_pid_file" 2>/dev/null || true)"
kill -TERM "$vd_pid" 2>/dev/null || true
[ -n "$_repro_pid" ] && kill -9 "$_repro_pid" 2>/dev/null || true
wait "$vd_pid" 2>/dev/null || true

want "21. TERM trap: interrupted line in landing.log" \
    "attribution of PR 99 interrupted" \
    "$(cat "$RUN/landing.log" 2>/dev/null)"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# ─── Concurrency-counter stub for cases 22/23 ─────────────────────────────────
# Each invocation bumps a lock-protected counter, records the running peak,
# sleeps 1s (long enough that overlapping invocations really do overlap under
# container load), then decrements. exit 1 keeps the batch red so every member
# is replayed.
CTR_DIR="$TMP/ctr"
mkdir -p "$CTR_DIR"
reset_counter() { printf '0' > "$CTR_DIR/count"; printf '0' > "$CTR_DIR/max"; }

cat > "$SH/repro-concurrency.sh" <<REPRO
#!/usr/bin/env bash
{
    flock -x 200
    cur=\$(( \$(cat "$CTR_DIR/count" 2>/dev/null || echo 0) + 1 ))
    printf '%s' "\$cur" > "$CTR_DIR/count"
    mx=\$(cat "$CTR_DIR/max" 2>/dev/null || echo 0)
    [ "\$cur" -gt "\$mx" ] && printf '%s' "\$cur" > "$CTR_DIR/max"
} 200>"$CTR_DIR/lock"
sleep 1
{
    flock -x 200
    cur=\$(( \$(cat "$CTR_DIR/count" 2>/dev/null || echo 0) - 1 ))
    printf '%s' "\$cur" > "$CTR_DIR/count"
} 200>"$CTR_DIR/lock"
exit 1
REPRO
chmod +x "$SH/repro-concurrency.sh"

build_replay_batch() {
    local prefix="$1"
    build_batch "${prefix}1" "${prefix}2" "${prefix}3" > /dev/null
    printf 'red\nred-suite: test-concurrency.sh\n' > "$FORGE_STATUS_FILE"
}

# =============================================================================
# 22. PARALLEL — 3 members, SPIRA_BATCH_MAXPAR=3: replays run concurrently, so
#     the observed peak concurrency reaches 3.
# =============================================================================
reset_counter
build_replay_batch sp-vd-p
SPIRA_QUEUE_REPRO_BATCH="$SH/repro-concurrency.sh" SPIRA_BATCH_MAXPAR=3 \
    verdict "$REPONAME" >/dev/null
is "22. parallel: peak concurrency reaches MAXPAR=3" "3" "$(cat "$CTR_DIR/max")"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 23. SERIAL (positive control for case 22) — same 3-member batch, MAXPAR=1:
#     replays run one at a time, so peak concurrency never exceeds 1. Without
#     this control, a MAXPAR that is silently ignored would still show
#     concurrency ≤ 3 in case 22 and look like a pass.
# =============================================================================
reset_counter
build_replay_batch sp-vd-q
SPIRA_QUEUE_REPRO_BATCH="$SH/repro-concurrency.sh" SPIRA_BATCH_MAXPAR=1 \
    verdict "$REPONAME" >/dev/null
is "23. serial: peak concurrency stays at 1" "1" "$(cat "$CTR_DIR/max")"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

echo
echo "results: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
