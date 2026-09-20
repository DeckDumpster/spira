#!/usr/bin/env bash
#
# test-verdict.sh — merge-queue verdict: fast-forward landing pass.
#
# Sixteen cases:
#   1. No open batch → forge is never reached.
#   2. Pending within CI max → nothing happens.
#   3. Pending, run old and stuck → run cancelled explicitly; no workflow-rerun.
#   4. Harness fault, retries remaining → re-run called, counter bumped.
#   5. Harness fault, retries exhausted → PR closed, members CERTIFIED, batch
#      removed, mail sent once; second pass sends no second mail.
#   6. Green, base unchanged, flaky annotation → fast-forward push; members LANDED;
#      flake observed; batch record removed.
#   7. Green, base moved → PR closed; members returned to CERTIFIED; batch removed.
#   8. Red naming no suite → the branch was not judged: re-run as a harness fault,
#      never held open (sp-swux6).
#   9. Batch branch cleanup: branch deleted after green fast-forward.
#  10. Green, CI head SHA mismatches sealed batch head → no push; members CERTIFIED;
#      PR closed; operator mailed. (positive control for SHA mismatch detection)
#  11. Green, CI head SHA matches sealed batch head → normal fast-forward landing.
#  12. PR old, run freshly started → no cancellation. Regression: verdict was ageing
#      the PR instead of the run; healthy CI was cancelled when opened exceeded the
#      threshold, regardless of whether the current run was making progress.
#  13. Green, base moved, member tips already in new base → members LANDED, not
#      re-queued. Positive control with case 7: case 7 proves CERTIFIED when tips
#      are NOT in the new base; this proves LANDED when they ARE.
#  14. PR old, run old, but last-activity recent → run still progressing; no cancel.
#  15. All-suites repro ejects a member whose diff is claimed by no suite, before
#      the batch-head halve is attempted.
#  16. Positive control for case 15: same batch shape with no offender — members
#      CERTIFIED; no halve.
#
# The forge seam is a local fixture; no network is reached.
# mail.sh and suites.sh are stubbed to capture calls.
#
# covers: spira/verdict.sh spira/forge.sh spira/conf.sh spira/batch.sh
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()    { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()   { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()    { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()  { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-verdict
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up verdict || { echo "test-verdict: could not build fixture database"; exit 1; }
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

cp "$HERE"/*.sh "$SH/"

# Write repo-map: queue mode, pinned to a non-default.
cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

# ─── Shared log paths — EXPORTED so all subprocess scripts can read them ──────
FORGE_LOG="$TMP/forge-log"
FORGE_STATUS_FILE="$TMP/forge-status"
FORGE_RUN_METADATA_FILE="$TMP/forge-run-metadata"
MAIL_LOG="$TMP/mail-log"
SUITES_LOG="$TMP/suites-log"
export FORGE_LOG FORGE_STATUS_FILE FORGE_RUN_METADATA_FILE MAIL_LOG SUITES_LOG

printf 'pending\n' > "$FORGE_STATUS_FILE"
: > "$FORGE_LOG"
: > "$FORGE_RUN_METADATA_FILE"
: > "$MAIL_LOG"
: > "$SUITES_LOG"

# ─── Forge fixture ────────────────────────────────────────────────────────────
# check-status reads FORGE_STATUS_FILE; run-metadata reads FORGE_RUN_METADATA_FILE;
# run-cancel, workflow-rerun and pr-close append to FORGE_LOG. All vars exported above.
# Pinned to a non-default SPIRA_FORGE so an assertion passing against "used gh"
# fails rather than passing vacuously.
cat > "$SH/forge-fixture.sh" <<'FORGE'
#!/usr/bin/env bash
cmd="${1:-}"; shift; repo="${1:-}"; shift
case "$cmd" in
    check-status)
        cat "${FORGE_STATUS_FILE}" 2>/dev/null || printf 'pending\n'
        ;;
    run-id)
        printf 'run-99\n'
        ;;
    run-metadata)
        cat "${FORGE_RUN_METADATA_FILE}" 2>/dev/null || true
        ;;
    run-cancel)
        printf '%s\tcancel\n' "${1:-}" >> "$FORGE_LOG"
        ;;
    workflow-rerun)
        printf '%s\trerun\n' "${1:-}" >> "$FORGE_LOG"
        ;;
    pr-close)
        printf '%s\tclose\n' "${1:-}" >> "$FORGE_LOG"
        ;;
    *) printf 'forge-fixture: unknown command: %s\n' "$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"

# ─── mail.sh stub ─────────────────────────────────────────────────────────────
# Capture both command line and stdin body
cat > "$SH/mail.sh" <<'MAIL'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$MAIL_LOG"
# Also capture stdin body for test verification
cat >> "$MAIL_LOG"
MAIL
chmod +x "$SH/mail.sh"

# ─── suites.sh stub ───────────────────────────────────────────────────────────
cat > "$SH/suites.sh" <<'SUITES'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$SUITES_LOG"
SUITES
chmod +x "$SH/suites.sh"

# ─── Helpers ──────────────────────────────────────────────────────────────────

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
remote_main() { git -C "$REMOTE" rev-parse main 2>/dev/null; }
landstate()   { cat "$LANDSTATE/${1:-}" 2>/dev/null; }

# merge_batch_externally — simulate GitHub auto-merging the open batch PR before
# verdict.sh ran: creates a merge commit on origin/main whose second parent is
# batch_head, so every member tip is reachable from the new main.
merge_batch_externally() {
    local batch_head="$1"
    local bwt="$RUN/worktree/.merge-ext"
    git -C "$REPO" worktree remove -f "$bwt" 2>/dev/null || true
    git -C "$REPO" worktree add -q --detach "$bwt" origin/main
    git -C "$bwt" merge --no-ff -q -m "Merge pull request: queue" "$batch_head" \
        >/dev/null 2>&1
    git -C "$bwt" push -q origin "HEAD:main"
    git -C "$REPO" worktree remove -f "$bwt" 2>/dev/null || true
    git -C "$REPO" fetch -q origin
}

# advance_base — land a throwaway commit directly on top of origin/main via a
# detached-HEAD worktree so the push is always a fast-forward from remote's POV.
advance_base() {
    local bwt="$RUN/worktree/.advance-base"
    git -C "$REPO" worktree remove -f "$bwt" 2>/dev/null || true
    git -C "$REPO" worktree add -q --detach "$bwt" origin/main
    printf 'other\n' > "$bwt/other.txt"
    git -C "$bwt" add -A
    git -C "$bwt" commit -q -m "other: unrelated landing"
    git -C "$bwt" push -q origin "HEAD:main"
    git -C "$REPO" worktree remove -f "$bwt" 2>/dev/null || true
    git -C "$REPO" fetch -q origin
}

# build_batch <id1> <id2> ... — create member branches, a batch merge commit,
# and write an open batch record. Prints batch_head sha on stdout.
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

    local now; now="$(date +%s)"
    {
        printf 'pr=42\n'
        printf 'head=%s\n' "$batch_head"
        printf 'base=%s\n' "$base_sha"
        printf 'members=%s\n' "${members[*]}"
        printf 'opened=%s\n' "$now"
        printf 'branch=spira/queue/test\n'
    } > "$(batch_file)"

    printf '%s\n' "$batch_head"
}

clean_case() {
    rm -f "$(batch_file)"
    : > "$FORGE_LOG"
    : > "$FORGE_RUN_METADATA_FILE"
    : > "$MAIL_LOG"
    : > "$SUITES_LOG"
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

echo "test-verdict.sh"

# =============================================================================
# 1. NO OPEN BATCH — forge is never reached.
#    Positive control: replace the forge with one that FAILS loudly if called,
#    so "no forge call" cannot pass against a verdict that silently errors out.
# =============================================================================
cat > "$SH/forge-fixture-never.sh" <<'NEVERNEVER'
#!/usr/bin/env bash
printf 'FORGE REACHED UNEXPECTEDLY: %s\n' "$*" >&2
exit 1
NEVERNEVER
chmod +x "$SH/forge-fixture-never.sh"
out="$( SPIRA_FORGE="$SH/forge-fixture-never.sh" verdict "$REPONAME" )"
is   "1. no batch: exit 0 without forge"        "0" "$?"
nowant "1. no batch: forge not reached"         "FORGE REACHED" "$out"

# =============================================================================
# 2. PENDING WITHIN CI MAX — nothing happens; no re-run.
# =============================================================================
build_batch sp-vd-p1 sp-vd-p2 > /dev/null
printf 'pending\n' > "$FORGE_STATUS_FILE"
before_main="$(remote_main)"
out="$(verdict "$REPONAME")"
is   "2. pending: no push"      "$before_main" "$(remote_main)"
nowant "2. pending: no rerun"   "rerun"        "$(cat "$FORGE_LOG")"
want "2. pending: reported"     "pending"      "$out"
clean_case

# =============================================================================
# 3. PENDING, RUN OLD AND STUCK — run cancelled explicitly; no workflow-rerun.
#    Run started 3601s ago with no last-activity → stuck. Cancel is called so
#    the shutdown is logged explicitly, not seen as an unexplained runner signal.
#    Positive control for case 12: fresh run on the same old PR is NOT cancelled.
# =============================================================================
build_batch sp-vd-q1 sp-vd-q2 > /dev/null
printf 'started-at: %s\n' "$(( $(date +%s) - 3601 ))" > "$FORGE_RUN_METADATA_FILE"
printf 'pending\n' > "$FORGE_STATUS_FILE"
before_main="$(remote_main)"
out="$(verdict "$REPONAME")"
is   "3. pending-past-max: no push"     "$before_main" "$(remote_main)"
want "3. pending-past-max: cancel"      "cancel"       "$(cat "$FORGE_LOG")"
nowant "3. pending-past-max: no rerun"  "rerun"        "$(cat "$FORGE_LOG")"
want "3. pending-past-max: stuck reported" "stuck"     "$out"
clean_case

# =============================================================================
# 4. HARNESS FAULT, RETRIES REMAINING — re-run called, counter incremented.
# =============================================================================
build_batch sp-vd-h1 sp-vd-h2 > /dev/null
{ grep -v '^retries=' "$(batch_file)"; printf 'retries=1\n'; } \
    > "$(batch_file).$$" && mv -f "$(batch_file).$$" "$(batch_file)"
printf 'harness_fault\n' > "$FORGE_STATUS_FILE"
before_main="$(remote_main)"
out="$(verdict "$REPONAME")"
is   "4. harness-fault: no push"   "$before_main" "$(remote_main)"
want "4. harness-fault: rerun"     "rerun"        "$(cat "$FORGE_LOG")"
is   "4. harness-fault: retries=2" "2" \
     "$(grep '^retries=' "$(batch_file)" | cut -d= -f2)"
clean_case

# =============================================================================
# 5. HARNESS FAULT, RETRIES EXHAUSTED — PR closed, members CERTIFIED, batch
#    removed, mail sent once; a second verdict pass sends no second mail.
#    POSITIVE CONTROL: case 4 proves the retry path increments the counter;
#    this case proves the exhausted path clears the batch so no re-mail occurs.
# =============================================================================
build_batch sp-vd-e1 sp-vd-e2 > /dev/null
{ grep -v '^retries=' "$(batch_file)"; printf 'retries=2\n'; } \
    > "$(batch_file).$$" && mv -f "$(batch_file).$$" "$(batch_file)"
printf 'harness_fault\n' > "$FORGE_STATUS_FILE"
out="$(verdict "$REPONAME")"
nowant "5. exhausted: no rerun"         "rerun"             "$(cat "$FORGE_LOG")"
want "5. exhausted: mail sent"          "send operator"     "$(cat "$MAIL_LOG")"
want "5. exhausted: pr-close called"    "close"             "$(cat "$FORGE_LOG")"
is   "5. exhausted: batch record removed" "0" "$([ -f "$(batch_file)" ] && echo 1 || echo 0)"
case "$(landstate sp-vd-e1)" in CERTIFIED*) ok "5. exhausted: sp-vd-e1 CERTIFIED" ;;
    *) bad "5. exhausted: sp-vd-e1 CERTIFIED" "got: $(landstate sp-vd-e1)" ;; esac
case "$(landstate sp-vd-e2)" in CERTIFIED*) ok "5. exhausted: sp-vd-e2 CERTIFIED" ;;
    *) bad "5. exhausted: sp-vd-e2 CERTIFIED" "got: $(landstate sp-vd-e2)" ;; esac
want "5. exhausted: reported"           "retries exhausted" "$out"
# Second pass: batch is gone; no second mail.
: > "$MAIL_LOG"
verdict "$REPONAME" > /dev/null
nowant "5. exhausted: no second mail"   "send operator"     "$(cat "$MAIL_LOG")"
clean_case

# =============================================================================
# 6. GREEN, BASE UNCHANGED, WITH FLAKY ANNOTATION — fast-forward push; all
#    members LANDED; flake observed; batch record removed.
#    POSITIVE CONTROL: the forge check-status is actually called here — the
#    forge-fixture-never.sh from case 1 is not in use.
# =============================================================================
batch_head="$(build_batch sp-vd-g1 sp-vd-g2)"
printf 'green\nflaky: test-flaky-suite.sh\n' > "$FORGE_STATUS_FILE"
out="$(verdict "$REPONAME")"
is   "6. green: remote main advanced"  "$batch_head" "$(remote_main)"
case "$(landstate sp-vd-g1)" in LANDED*) ok "6. green: sp-vd-g1 LANDED" ;;
    *) bad "6. green: sp-vd-g1 LANDED" "got: $(landstate sp-vd-g1)" ;; esac
case "$(landstate sp-vd-g2)" in LANDED*) ok "6. green: sp-vd-g2 LANDED" ;;
    *) bad "6. green: sp-vd-g2 LANDED" "got: $(landstate sp-vd-g2)" ;; esac
is   "6. green: batch record removed"  "0" "$([ -f "$(batch_file)" ] && echo 1 || echo 0)"
want "6. green: landed reported"       "landed by fast-forward" "$out"
want "6. green: flake observed"        "test-flaky-suite.sh" "$(cat "$SUITES_LOG")"
clean_case
# Restore remote main to the post-landing state for subsequent cases.
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 7. GREEN, BASE MOVED — PR closed; members returned to CERTIFIED; batch removed.
# =============================================================================
batch_head="$(build_batch sp-vd-m1 sp-vd-m2)"
# Advance the remote's main AFTER the batch was built, simulating a concurrent
# landing. Use a detached-HEAD worktree to guarantee a fast-forward push.
advance_base
before_remote_main="$(remote_main)"
printf 'green\n' > "$FORGE_STATUS_FILE"
out="$(verdict "$REPONAME")"
want "7. moved: pr-close called"       "close" "$(cat "$FORGE_LOG")"
case "$(landstate sp-vd-m1)" in CERTIFIED*) ok "7. moved: sp-vd-m1 CERTIFIED" ;;
    *) bad "7. moved: sp-vd-m1 CERTIFIED" "got: $(landstate sp-vd-m1)" ;; esac
case "$(landstate sp-vd-m2)" in CERTIFIED*) ok "7. moved: sp-vd-m2 CERTIFIED" ;;
    *) bad "7. moved: sp-vd-m2 CERTIFIED" "got: $(landstate sp-vd-m2)" ;; esac
is   "7. moved: batch record removed"  "0" "$([ -f "$(batch_file)" ] && echo 1 || echo 0)"
is   "7. moved: remote main not advanced to batch" \
     "$before_remote_main" "$(remote_main)"
want "7. moved: base-moved reported"   "base moved" "$out"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 8. RED NAMING NO SUITE — CI never judged the branch (a runner kill, a cold image,
#    an unresolvable base ref). Attribution has nothing to eject, so it used to leave
#    the batch open with no next action and freeze the queue (sp-swux6). It must take
#    the harness-fault path instead: re-run, count the attempt, push nothing.
# =============================================================================
build_batch sp-vd-r1 sp-vd-r2 > /dev/null
printf 'red\n' > "$FORGE_STATUS_FILE"
before_main="$(remote_main)"
out="$(verdict "$REPONAME")"
is   "8. red-no-suite: no push"       "$before_main" "$(remote_main)"
want "8. red-no-suite: rerun"         "rerun"        "$(cat "$FORGE_LOG")"
is   "8. red-no-suite: retries=1"     "1" \
     "$(grep '^retries=' "$(batch_file)" | cut -d= -f2)"
want "8. red-no-suite: says why"      "not judged"   "$out"
nowant "8. red-no-suite: not held"    "leaving batch open" "$out"
clean_case

# =============================================================================
# 8.5 TOGETHER-ONLY RED — multiple members, red only when batched, all
#     members requeued, PR closed, batch removed, and notification sent.
#
#     POSITIVE CONTROL: a red-repro mock that fails on batch head but passes
#     on individual members — verifies the together-only condition triggers.
#     NOTIFICATION CONTROL: verify _attr_notify_red is called with the right
#     decision text ("together-only red") and recipient email is sent.
# =============================================================================
# Create a mock testenv-batch.sh that:
#   - Fails (exit 1) when called for the batch HEAD or batch branches
#   - Succeeds (exit 0) when called for individual member branches
# Signature: testenv-batch --mode serial --suites <suites> <branch>
cat > "$SH/repro-together-only.sh" <<'REPROMOCK'
#!/usr/bin/env bash
# Mock: fail only for batch head or 'spira/queue/*' branches; pass for individuals
# Args: --mode serial --suites <suites> <branch>
shift 2  # skip --mode serial
shift 2  # skip --suites <suites>
branch="${1:-}"
if [[ "$branch" == "spira/queue"* ]]; then
    exit 1  # Batch head is red (together-only)
else
    exit 0  # Individual member branches are green
fi
REPROMOCK
chmod +x "$SH/repro-together-only.sh"

batch_head85="$(build_batch sp-vd-t85-1 sp-vd-t85-2)"
# Name the batch branch
{
    grep -v '^branch=' "$(batch_file)"
    printf 'branch=spira/queue/test85\n'
} > "$(batch_file).$$" && mv -f "$(batch_file).$$" "$(batch_file)"

printf 'red\nred-suite: test-suite-together-only.sh\n' > "$FORGE_STATUS_FILE"
before_main85="$(remote_main)"

out="$( \
    SPIRA_QUEUE_REPRO_BATCH="$SH/repro-together-only.sh" \
    verdict "$REPONAME" \
)"

# Verify: batch stays open (no push to main)
is   "8.5 together-only: no push"          "$before_main85" "$(remote_main)"
# Verify: batch record removed (verdict closes it)
is   "8.5 together-only: batch removed"    "0" "$([ -f "$(batch_file)" ] && echo 1 || echo 0)"
# Verify: both members marked CERTIFIED (halved, not ejected)
case "$(landstate sp-vd-t85-1)" in CERTIFIED*) ok "8.5 together-only: sp-vd-t85-1 CERTIFIED" ;;
    *) bad "8.5 together-only: sp-vd-t85-1 CERTIFIED" "got: $(landstate sp-vd-t85-1)" ;; esac
case "$(landstate sp-vd-t85-2)" in CERTIFIED*) ok "8.5 together-only: sp-vd-t85-2 CERTIFIED" ;;
    *) bad "8.5 together-only: sp-vd-t85-2 CERTIFIED" "got: $(landstate sp-vd-t85-2)" ;; esac
# Verify: decision text mentions together-only in output
want "8.5 together-only: reported as together-only" "together-only" "$out"
# Verify: mail notification sent with together-only decision
want "8.5 together-only: mail notification sent"    "send operator"  "$(cat "$MAIL_LOG")"
want "8.5 together-only: notification mentions decision" "Together-only red" "$(cat "$MAIL_LOG")"

clean_case

# =============================================================================
# 9. BATCH BRANCH CLEANUP: after a green fast-forward, the batch branch is
#    deleted locally and on the remote.
#
#    POSITIVE CONTROL: without the deletion code in verdict.sh, the branch
#    remains in refs/heads after landing; the is-gone assertions below fail.
# =============================================================================
batch_head9="$(build_batch sp-vd-b1 sp-vd-b2)"
# Create the batch branch as batch.sh does.
git -C "$REPO" branch -f "spira/queue/test9" "$batch_head9"
git -C "$REPO" push -q origin "spira/queue/test9"
git -C "$REPO" fetch -q origin
# Update the open batch record to name this branch.
{
    grep -v '^branch=' "$(batch_file)"
    printf 'branch=spira/queue/test9\n'
} > "$(batch_file).$$" && mv -f "$(batch_file).$$" "$(batch_file)"

printf 'green\n' > "$FORGE_STATUS_FILE"
verdict "$REPONAME" > /dev/null
is "9. cleanup: batch branch gone locally" "0" \
    "$(git -C "$REPO" show-ref --verify "refs/heads/spira/queue/test9" >/dev/null 2>&1 && echo 1 || echo 0)"
is "9. cleanup: batch branch gone from remote" "0" \
    "$(git -C "$REMOTE" show-ref --verify "refs/heads/spira/queue/test9" >/dev/null 2>&1 && echo 1 || echo 0)"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 10. GREEN, CI HEAD SHA MISMATCH — verdict refuses to land.
#     Positive control: plant a wrong SHA first; the mismatch check must fire
#     before we trust case 11's silence (matching SHA → proceeds normally).
# =============================================================================
batch_head10="$(build_batch sp-vd-s1 sp-vd-s2)"
printf 'green\nhead-sha: deadbeef1234567890abcdef1234567890abcdef\n' > "$FORGE_STATUS_FILE"
before_main10="$(remote_main)"
out="$(verdict "$REPONAME")"
is   "10. sha-mismatch: no push"            "$before_main10" "$(remote_main)"
want "10. sha-mismatch: pr-close called"    "close"          "$(cat "$FORGE_LOG")"
case "$(landstate sp-vd-s1)" in CERTIFIED*) ok "10. sha-mismatch: sp-vd-s1 CERTIFIED" ;;
    *) bad "10. sha-mismatch: sp-vd-s1 CERTIFIED" "got: $(landstate sp-vd-s1)" ;; esac
case "$(landstate sp-vd-s2)" in CERTIFIED*) ok "10. sha-mismatch: sp-vd-s2 CERTIFIED" ;;
    *) bad "10. sha-mismatch: sp-vd-s2 CERTIFIED" "got: $(landstate sp-vd-s2)" ;; esac
is   "10. sha-mismatch: batch record removed" "0" "$([ -f "$(batch_file)" ] && echo 1 || echo 0)"
want "10. sha-mismatch: mail sent"          "send operator" "$(cat "$MAIL_LOG")"
want "10. sha-mismatch: mismatch reported"  "mismatch"      "$out"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 11. GREEN, CI HEAD SHA MATCHES SEALED HEAD — normal fast-forward landing.
#     Verifies no false positive when the SHA matches.
# =============================================================================
batch_head11="$(build_batch sp-vd-t1 sp-vd-t2)"
printf 'green\nhead-sha: %s\n' "$batch_head11" > "$FORGE_STATUS_FILE"
out="$(verdict "$REPONAME")"
is   "11. sha-match: remote main advanced"  "$batch_head11" "$(remote_main)"
case "$(landstate sp-vd-t1)" in LANDED*) ok "11. sha-match: sp-vd-t1 LANDED" ;;
    *) bad "11. sha-match: sp-vd-t1 LANDED" "got: $(landstate sp-vd-t1)" ;; esac
case "$(landstate sp-vd-t2)" in LANDED*) ok "11. sha-match: sp-vd-t2 LANDED" ;;
    *) bad "11. sha-match: sp-vd-t2 LANDED" "got: $(landstate sp-vd-t2)" ;; esac
is   "11. sha-match: batch record removed"  "0" "$([ -f "$(batch_file)" ] && echo 1 || echo 0)"
want "11. sha-match: landed reported"       "landed by fast-forward" "$out"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 12. PR OLD, RUN FRESHLY STARTED — no cancellation.
#     Regression: verdict was using PR opened time instead of run start time.
#     A batch PR is always older than CI_MAXSEC (pre-flight alone takes 14-40m),
#     so every pending PR was cancelled by the old code regardless of run health.
#     Positive control: case 3 verifies a genuinely old, stuck run IS cancelled.
#     This case verifies that a fresh run on an old PR is NOT cancelled.
# =============================================================================
build_batch sp-vd-u1 sp-vd-u2 > /dev/null
{
    grep -v '^opened=' "$(batch_file)"
    printf 'opened=%s\n' "$(( $(date +%s) - 7200 ))"
} > "$(batch_file).$$" && mv -f "$(batch_file).$$" "$(batch_file)"
# Run started 60s ago — well within CI_MAXSEC=3600.
printf 'started-at: %s\n' "$(( $(date +%s) - 60 ))" > "$FORGE_RUN_METADATA_FILE"
printf 'pending\n' > "$FORGE_STATUS_FILE"
before_main12="$(remote_main)"
out="$(verdict "$REPONAME")"
is     "12. fresh-run: no push"         "$before_main12" "$(remote_main)"
nowant "12. fresh-run: no cancel"       "cancel"         "$(cat "$FORGE_LOG")"
nowant "12. fresh-run: no rerun"        "rerun"          "$(cat "$FORGE_LOG")"
want   "12. fresh-run: reported"        "pending"        "$out"
clean_case

# =============================================================================
# 13. GREEN, BASE MOVED, MEMBER TIPS ALREADY IN NEW BASE — members LANDED.
#     Simulates a batch PR auto-merged by GitHub before verdict.sh ran.
#     POSITIVE CONTROL (combined with case 7): case 7 proves that CERTIFIED is
#     written when tips are NOT in the new base; this case proves LANDED when
#     they ARE — without this case, silence in case 7 could hide a code path
#     that always certifies rather than checking ancestry.
# =============================================================================
batch_head13="$(build_batch sp-vd-n1 sp-vd-n2)"
merge_batch_externally "$batch_head13"
printf 'green\n' > "$FORGE_STATUS_FILE"
out="$(verdict "$REPONAME")"
want "13. moved-in-base: pr-close called"        "close"   "$(cat "$FORGE_LOG")"
case "$(landstate sp-vd-n1)" in LANDED*) ok "13. moved-in-base: sp-vd-n1 LANDED" ;;
    *) bad "13. moved-in-base: sp-vd-n1 LANDED" "got: $(landstate sp-vd-n1)" ;; esac
case "$(landstate sp-vd-n2)" in LANDED*) ok "13. moved-in-base: sp-vd-n2 LANDED" ;;
    *) bad "13. moved-in-base: sp-vd-n2 LANDED" "got: $(landstate sp-vd-n2)" ;; esac
is   "13. moved-in-base: batch record removed"   "0" "$([ -f "$(batch_file)" ] && echo 1 || echo 0)"
want "13. moved-in-base: base-moved reported"    "base moved"         "$out"
want "13. moved-in-base: already-in-base logged" "already in moved base" "$out"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 14. PR OLD, RUN OLD, LAST-ACTIVITY RECENT — run still progressing; no cancel.
#     Positive control for the activity guard: cancelling a progressing run
#     would surface as an unexplained runner shutdown with cancel-in-progress.
#     The run started 3700s ago (past MAXSEC), but a step completed 30s ago.
# =============================================================================
build_batch sp-vd-v1 sp-vd-v2 > /dev/null
{
    grep -v '^opened=' "$(batch_file)"
    printf 'opened=%s\n' "$(( $(date +%s) - 7200 ))"
} > "$(batch_file).$$" && mv -f "$(batch_file).$$" "$(batch_file)"
printf 'started-at: %s\nlast-activity: %s\n' \
    "$(( $(date +%s) - 3700 ))" "$(( $(date +%s) - 30 ))" > "$FORGE_RUN_METADATA_FILE"
printf 'pending\n' > "$FORGE_STATUS_FILE"
before_main14="$(remote_main)"
out="$(verdict "$REPONAME")"
is     "14. progressing: no push"       "$before_main14" "$(remote_main)"
nowant "14. progressing: no cancel"     "cancel"         "$(cat "$FORGE_LOG")"
nowant "14. progressing: no rerun"      "rerun"          "$(cat "$FORGE_LOG")"
want   "14. progressing: reported"      "progressing"    "$out"
clean_case

# =============================================================================
# 15. ALL-SUITES REPRO EJECTS GUILTY MEMBER BEFORE HALVING.
#     A member whose diff is claimed by no suite (only a non-.sh file changed)
#     still gets ejected when it reproduces the red suite alone against all
#     red suites — the filter-lift fallback runs before the batch-head halve.
#     POSITIVE CONTROL (case 16): same shape of batch with no offender — the
#     all-suites repro finds nothing, the batch head is also green, and all
#     members are returned to CERTIFIED without ejection or halving.
# =============================================================================
cat > "$SH/repro-batch-attr.sh" <<'REPRO'
#!/usr/bin/env bash
# Red if the test-ref tree contains guilty-marker.txt; green otherwise.
ref="${@: -1}"
git -C "$SPIRA_REPO" ls-tree "$ref" -- guilty-marker.txt 2>/dev/null | grep -q . && exit 1
exit 0
REPRO
chmod +x "$SH/repro-batch-attr.sh"

# sp-vd-a1: innocent — adds only a plain text file (claims no .sh covers)
# sp-vd-a2: guilty   — adds guilty-marker.txt (also uncovered, but repro fails)
base_sha15="$(git -C "$REPO" rev-parse origin/main)"
for id in sp-vd-a1 sp-vd-a2; do
    bwt15="$RUN/worktree/$id"
    git -C "$REPO" worktree add -q -b "spira/$id" "$bwt15" origin/main 2>/dev/null || true
    printf '%s\n' "$id" > "$bwt15/$id.txt"
done
printf 'offender\n' > "$RUN/worktree/sp-vd-a2/guilty-marker.txt"
for id in sp-vd-a1 sp-vd-a2; do
    bwt15="$RUN/worktree/$id"
    git -C "$bwt15" add -A
    git -C "$bwt15" commit -q -m "$id: work"
    printf 'BATCHED %s %s\n' "$(git -C "$REPO" rev-parse "spira/$id")" "$(date +%s)" \
        > "$LANDSTATE/$id"
done
tip_a1="$(git -C "$REPO" rev-parse "spira/sp-vd-a1")"
tip_a2="$(git -C "$REPO" rev-parse "spira/sp-vd-a2")"
wt15="$RUN/worktree/.b15"
git -C "$REPO" worktree add -q --detach "$wt15" "$base_sha15" 2>/dev/null || true
git -C "$wt15" merge -q --no-edit --no-ff -m "spira: land sp-vd-a1" "$tip_a1" >/dev/null 2>&1
git -C "$wt15" merge -q --no-edit --no-ff -m "spira: land sp-vd-a2" "$tip_a2" >/dev/null 2>&1
batch_head15="$(git -C "$wt15" rev-parse HEAD)"
git -C "$REPO" worktree remove -f "$wt15" 2>/dev/null || true
{ printf 'pr=55\nhead=%s\nbase=%s\nmembers=sp-vd-a1:%s sp-vd-a2:%s\nopened=%s\n' \
    "$batch_head15" "$base_sha15" "$tip_a1" "$tip_a2" "$(date +%s)"; } > "$(batch_file)"
printf 'red\nred-suite: test-attr-suite.sh\n' > "$FORGE_STATUS_FILE"
out="$(SPIRA_QUEUE_REPRO_BATCH="$SH/repro-batch-attr.sh" verdict "$REPONAME")"
case "$(landstate sp-vd-a2)" in EJECTED*) ok "15. all-suites-repro: guilty ejected" ;;
    *) bad "15. all-suites-repro: guilty ejected" "got: $(landstate sp-vd-a2)" ;; esac
case "$(landstate sp-vd-a1)" in CERTIFIED*) ok "15. all-suites-repro: innocent CERTIFIED" ;;
    *) bad "15. all-suites-repro: innocent CERTIFIED" "got: $(landstate sp-vd-a1)" ;; esac
nowant "15. all-suites-repro: no halve"     "halved"   "$out"
want   "15. all-suites-repro: ejection out" "ejected"  "$out"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 16. POSITIVE CONTROL: batch with no offender — all-suites repro green for
#     every member, batch head repro also green; members CERTIFIED, no halve.
# =============================================================================
base_sha16="$(git -C "$REPO" rev-parse origin/main)"
for id in sp-vd-b1 sp-vd-b2; do
    bwt16="$RUN/worktree/$id"
    git -C "$REPO" worktree add -q -b "spira/$id" "$bwt16" origin/main 2>/dev/null || true
    printf '%s\n' "$id" > "$bwt16/$id.txt"
    git -C "$bwt16" add -A
    git -C "$bwt16" commit -q -m "$id: work"
    printf 'BATCHED %s %s\n' "$(git -C "$REPO" rev-parse "spira/$id")" "$(date +%s)" \
        > "$LANDSTATE/$id"
done
tip_b1="$(git -C "$REPO" rev-parse "spira/sp-vd-b1")"
tip_b2="$(git -C "$REPO" rev-parse "spira/sp-vd-b2")"
wt16="$RUN/worktree/.b16"
git -C "$REPO" worktree add -q --detach "$wt16" "$base_sha16" 2>/dev/null || true
git -C "$wt16" merge -q --no-edit --no-ff -m "spira: land sp-vd-b1" "$tip_b1" >/dev/null 2>&1
git -C "$wt16" merge -q --no-edit --no-ff -m "spira: land sp-vd-b2" "$tip_b2" >/dev/null 2>&1
batch_head16="$(git -C "$wt16" rev-parse HEAD)"
git -C "$REPO" worktree remove -f "$wt16" 2>/dev/null || true
{ printf 'pr=56\nhead=%s\nbase=%s\nmembers=sp-vd-b1:%s sp-vd-b2:%s\nopened=%s\n' \
    "$batch_head16" "$base_sha16" "$tip_b1" "$tip_b2" "$(date +%s)"; } > "$(batch_file)"
printf 'red\nred-suite: test-attr-suite.sh\n' > "$FORGE_STATUS_FILE"
out="$(SPIRA_QUEUE_REPRO_BATCH="$SH/repro-batch-attr.sh" verdict "$REPONAME")"
case "$(landstate sp-vd-b1)" in CERTIFIED*) ok "16. all-suites-ctrl: sp-vd-b1 CERTIFIED" ;;
    *) bad "16. all-suites-ctrl: sp-vd-b1 CERTIFIED" "got: $(landstate sp-vd-b1)" ;; esac
case "$(landstate sp-vd-b2)" in CERTIFIED*) ok "16. all-suites-ctrl: sp-vd-b2 CERTIFIED" ;;
    *) bad "16. all-suites-ctrl: sp-vd-b2 CERTIFIED" "got: $(landstate sp-vd-b2)" ;; esac
nowant "16. all-suites-ctrl: no ejection" "ejected" "$out"
nowant "16. all-suites-ctrl: no halve"    "halved"  "$out"
clean_case

# =============================================================================
# 17. SINGLE-STEP JOB: run.updated_at recent, job/step timestamps stale.
#     Forge emits two last-activity lines: one from run.updated_at (recent) and
#     one from job/step timestamps (stale, because the single step hasn't finished).
#     Verdict must take the MAX — the run is progressing, not stuck.
#     POSITIVE CONTROL: the older last-activity line alone would exceed IDLE_SEC
#     and trigger cancellation; the fact that it doesn't proves max is taken.
# =============================================================================
build_batch sp-vd-w1 sp-vd-w2 > /dev/null
{
    grep -v '^opened=' "$(batch_file)"
    printf 'opened=%s\n' "$(( $(date +%s) - 7200 ))"
} > "$(batch_file).$$" && mv -f "$(batch_file).$$" "$(batch_file)"
printf 'started-at: %s\nlast-activity: %s\nlast-activity: %s\n' \
    "$(( $(date +%s) - 3700 ))" \
    "$(( $(date +%s) - 30 ))" \
    "$(( $(date +%s) - 3700 ))" \
    > "$FORGE_RUN_METADATA_FILE"
printf 'pending\n' > "$FORGE_STATUS_FILE"
before_main17="$(remote_main)"
out="$(verdict "$REPONAME")"
is     "17. single-step: no push"     "$before_main17" "$(remote_main)"
nowant "17. single-step: no cancel"   "cancel"         "$(cat "$FORGE_LOG")"
want   "17. single-step: progressing" "progressing"    "$out"
clean_case

# =============================================================================
# 18. PER-REPO CI_MAXSEC OVERRIDE: SPIRA_QUEUE_CI_MAXSEC_<NAME> used when set.
#     POSITIVE CONTROL: run started 120s ago is within global MAXSEC=3600 and
#     would NOT be cancelled without the per-repo override. With the override
#     (FIXTURE_REPO=60), it is past the repo's limit and is cancelled.
# =============================================================================
build_batch sp-vd-x1 sp-vd-x2 > /dev/null
printf 'started-at: %s\n' "$(( $(date +%s) - 120 ))" > "$FORGE_RUN_METADATA_FILE"
printf 'pending\n' > "$FORGE_STATUS_FILE"
before_main18="$(remote_main)"
# Positive control: without the override the run is within global MAXSEC=3600.
out="$(verdict "$REPONAME")"
is     "18. per-repo ctrl: no cancel without override" "$before_main18" "$(remote_main)"
nowant "18. per-repo ctrl: no cancel"  "cancel"  "$(cat "$FORGE_LOG")"
want   "18. per-repo ctrl: pending"    "pending" "$out"
# Now set the per-repo override: 60s limit, run is 120s old → stuck.
: > "$FORGE_LOG"
out="$(SPIRA_QUEUE_CI_MAXSEC_FIXTURE_REPO=60 verdict "$REPONAME")"
want "18. per-repo: cancel with override" "cancel" "$(cat "$FORGE_LOG")"
want "18. per-repo: stuck reported"       "stuck"  "$out"
clean_case

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
