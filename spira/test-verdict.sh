#!/usr/bin/env bash
#
# test-verdict.sh — merge-queue verdict: fast-forward landing pass.
#
# Twelve cases:
#   1. No open batch → forge is never reached.
#   2. Pending within CI max → nothing happens.
#   3. Pending, run old and stuck → treated as harness fault, re-run called.
#   4. Harness fault, retries remaining → re-run called, counter bumped.
#   5. Harness fault, retries exhausted → mail sent to operator.
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
# workflow-rerun and pr-close append to FORGE_LOG. All vars are exported above.
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
cat > "$SH/mail.sh" <<'MAIL'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$MAIL_LOG"
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
# 3. PENDING, RUN OLD AND STUCK — treated as harness fault; re-run is requested.
#    The run started 3601s ago with no job activity since, so it is stuck.
#    Positive control: run metadata has old started-at; without the activity check,
#    a freshly-started run would also trigger (case 12 is that positive control).
# =============================================================================
build_batch sp-vd-q1 sp-vd-q2 > /dev/null
printf 'started-at: %s\n' "$(( $(date +%s) - 3601 ))" > "$FORGE_RUN_METADATA_FILE"
printf 'pending\n' > "$FORGE_STATUS_FILE"
before_main="$(remote_main)"
out="$(verdict "$REPONAME")"
is   "3. pending-past-max: no push"   "$before_main" "$(remote_main)"
want "3. pending-past-max: rerun"     "rerun"        "$(cat "$FORGE_LOG")"
want "3. pending-past-max: reported"  "harness fault" "$out"
is   "3. pending-past-max: retries=1" "1" \
     "$(grep '^retries=' "$(batch_file)" | cut -d= -f2)"
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
# 5. HARNESS FAULT, RETRIES EXHAUSTED — mail sent; no re-run.
# =============================================================================
build_batch sp-vd-e1 sp-vd-e2 > /dev/null
{ grep -v '^retries=' "$(batch_file)"; printf 'retries=2\n'; } \
    > "$(batch_file).$$" && mv -f "$(batch_file).$$" "$(batch_file)"
printf 'harness_fault\n' > "$FORGE_STATUS_FILE"
out="$(verdict "$REPONAME")"
nowant "5. exhausted: no rerun"  "rerun"         "$(cat "$FORGE_LOG")"
want "5. exhausted: mail sent"   "send operator" "$(cat "$MAIL_LOG")"
want "5. exhausted: reported"    "retries exhausted" "$out"
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
is   "12. fresh-run: no push"    "$before_main12" "$(remote_main)"
nowant "12. fresh-run: no rerun" "rerun"          "$(cat "$FORGE_LOG")"
want "12. fresh-run: reported"   "pending"        "$out"
clean_case

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
