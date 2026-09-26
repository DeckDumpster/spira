#!/usr/bin/env bash
#
# test-verdict.sh — merge-queue verdict: fast-forward landing pass.
#
# Twenty-six cases (of the original thirty-three; see below for where the rest
# went):
#   1. No open batch → forge is never reached.
#   2. Pending within CI max → nothing happens.
#   3. Pending, run old and stuck → run cancelled explicitly; no workflow-rerun.
#      Kept as an assembly control alongside case 1 (see test-verdict-action.sh).
#   4. Harness fault, retries remaining → re-run called, counter bumped.
#   5. Harness fault, retries exhausted → PR closed, members CERTIFIED, batch
#      removed, mail sent once; second pass sends no second mail.
#   6. Green, base unchanged, flaky annotation → fast-forward push; members LANDED;
#      flake observed; batch record removed.
#   7. Green, base moved (non-conflicting) → batch rebuilt on new base, re-pushed
#      to same PR; members stay BATCHED; no pr-close.
#   8. Red naming no suite, multi-member → bisect by halving; members CERTIFIED,
#      batch removed (sp-swux6).
#   9. Batch branch cleanup: branch deleted after green fast-forward.
#  10. Green, CI head SHA mismatches sealed batch head → no push; members CERTIFIED;
#      PR closed; operator mailed. (positive control for SHA mismatch detection)
#  11. Green, CI head SHA matches sealed batch head → normal fast-forward landing.
#  13. Green, base moved, member tips already in new base → members LANDED, not
#      re-queued. Positive control with case 7: case 7 proves CERTIFIED when tips
#      are NOT in the new base; this proves LANDED when they ARE.
#  15. All-suites repro ejects a member whose diff is claimed by no suite, before
#      the batch-head halve is attempted.
#  16. Positive control for case 15: same batch shape with no offender — members
#      CERTIFIED; no halve.
#  19. Flaky suite: fails once then passes on retry → member not ejected; flaky_suites=.
#  20. Always-red control: suite fails twice → member ejected (retry doesn't suppress).
#  24. Could-not-judge: rc=2 member doesn't shield others; guilty member ejected.
#  25. Deterministic: ejected set identical across two runs with different completion order.
#  26. Red-suite ineligible for quarantine: gate's red-twice verdict blocks observe-flake.
#  28. Green, base moved (conflicting) → PR closed; members returned to CERTIFIED;
#      conflict member named in log.
#  29. Red naming no suite, multi-member → the halving in case 8 also persists
#      the first half as $SPIRA_QUEUE_DIR/<repo>/bisect (sp-y931m).
#  30. A red batch whose sole member is the bisect's current forced group
#      ejects on the first strike (not the two-strike unreproduced-red track),
#      and a build-error annotation is logged on the ejected bead.
#  31. A batch matching the bisect's current forced group lands green → the
#      bisect advances to the sibling parked at the split.
#  32. Stale queue-ref reaping: a green landing cleans up local spira/queue/*
#      refs already ancestors of the base; a non-ancestor ref survives.
#  33. Green but check-status omits head-sha → cannot verify the sealed head
#      was tested; no push, no pr-close, batch held for retry.
#  35. Ejection mail names "reproduced-alone" (sp-bdkbw): the failure really
#      was rerun and reproduced, so the mail says so, plus the bead's title,
#      a diffstat against base, the failing assertion line, and the CI run.
#  36. Ejection mail names "suite-overlap" when nothing was reproduced: the
#      suite's own file is in the diff, so it is attributed by overlap, and
#      the mail says exactly that instead of claiming reproduction.
#  37. Ejection mail names "bisect-split" for a bisect-singleton eject, and
#      lists the sibling half that was left pending, not implicated.
#  38. Two red suites, two members, diff-evidence only (sp-2gcls): repro
#      faults for everyone, so each member is ejected on diff evidence alone.
#      Each note must name only the suite that member's own diff touched,
#      never the other member's suite, and never claim "Reproduced alone".
#  41. A suite already red on the batch's base is excluded from every
#      member's blame list; the genuine offender is still ejected, for the
#      suite it actually broke, and the exclusion is named in the mail
#      (sp-a2nk8).
#  42. Every red suite is already red on base → nobody is ejected, the batch
#      requeues unchanged, and the operator is told why (sp-a2nk8).
#  43. A red suite the attribution pass could not pin on any member (unselected
#      by any diff, phase 2/3 short-circuited by an earlier eject) is logged
#      explicitly as "unattributed" in the machine-readable results — never
#      just absent (sp-yivi7 archivist note, batch PR 297).
#
# MOVED (docs/test-plan/landing-merge-queue.md UC-43/44/49, section 4 cluster 1):
#   Cases 12, 14, 17, 18, 27 (age/idle/retry classification over verdict_action,
#   verdict_parse_run_metadata, verdict_repo_threshold, verdict_normalize_status)
#   demoted to T1 in test-verdict-action.sh. Cases 21-23 (TERM trap; MAXPAR
#   concurrency) moved to test-verdict-replay.sh (tier T3, off the per-push path).
#
# The forge seam is a local fixture; no network is reached.
# mail.sh and suites.sh are stubbed to capture calls.
#
# covers: spira/verdict.sh spira/forge.sh spira/conf.sh spira/batch.sh
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

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

cp "$HERE"/*.sh "$HERE"/*.py "$SH/"

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
    rm -f "$QUEUEDIR/$REPONAME/bisect"
    : > "$FORGE_LOG"
    : > "$FORGE_RUN_METADATA_FILE"
    : > "$MAIL_LOG"
    : > "$SUITES_LOG"
    rm -f "$RUN/attribution/results.jsonl"
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
printf 'green\nhead-sha: %s\nflaky: test-flaky-suite.sh\n' "$batch_head" > "$FORGE_STATUS_FILE"
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
# 7. GREEN, BASE MOVED (NON-CONFLICTING) — batch rebuilt on new base and
#    re-pushed to same PR; members stay BATCHED; no pr-close issued.
#    POSITIVE CONTROL (case 28): conflicting base move still closes+requeues.
# =============================================================================
batch_head="$(build_batch sp-vd-m1 sp-vd-m2)"
advance_base
printf 'green\nhead-sha: %s\n' "$batch_head" > "$FORGE_STATUS_FILE"
out="$(verdict "$REPONAME")"
nowant "7. moved-rebuild: pr-close NOT called"      "close" "$(cat "$FORGE_LOG")"
case "$(landstate sp-vd-m1)" in BATCHED*) ok "7. moved-rebuild: sp-vd-m1 stays BATCHED" ;;
    *) bad "7. moved-rebuild: sp-vd-m1 stays BATCHED" "got: $(landstate sp-vd-m1)" ;; esac
case "$(landstate sp-vd-m2)" in BATCHED*) ok "7. moved-rebuild: sp-vd-m2 stays BATCHED" ;;
    *) bad "7. moved-rebuild: sp-vd-m2 stays BATCHED" "got: $(landstate sp-vd-m2)" ;; esac
is   "7. moved-rebuild: batch record kept"  "1" "$([ -f "$(batch_file)" ] && echo 1 || echo 0)"
new_batch_head7="$(grep '^head=' "$(batch_file)" | cut -d= -f2)"
[ "$new_batch_head7" != "$batch_head" ] \
    && ok "7. moved-rebuild: batch head updated to new rebuild commit" \
    || bad "7. moved-rebuild: batch head updated" "head unchanged: $batch_head"
new_batch_base7="$(grep '^base=' "$(batch_file)" | cut -d= -f2)"
is "7. moved-rebuild: batch base updated to new main" \
    "$(git -C "$REMOTE" rev-parse main 2>/dev/null)" "$new_batch_base7"
want "7. moved-rebuild: rebuilt log line"   "rebuilt on moved base" "$out"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 8. RED NAMING NO SUITE — multi-member batch, no red-suite: annotations
#    (e.g. a CI system without Spira-shaped suites, or a runner that died before
#    emitting any). The queue bisects by halving: first half gets epoch=1
#    (batches immediately), second half gets epoch=now (waits BATCH_WAIT).
#    Members are returned to CERTIFIED, batch removed, no push.
# =============================================================================
build_batch sp-vd-r1 sp-vd-r2 > /dev/null
printf 'red\n' > "$FORGE_STATUS_FILE"
before_main="$(remote_main)"
out="$(verdict "$REPONAME")"
is   "8. red-no-suite: no push"         "$before_main" "$(remote_main)"
nowant "8. red-no-suite: no rerun"      "rerun"        "$(cat "$FORGE_LOG")"
is   "8. red-no-suite: batch removed"   "0"            "$([ -f "$(batch_file)" ] && echo 1 || echo 0)"
want "8. red-no-suite: bisect reported" "bisect halved" "$out"
case "$(landstate sp-vd-r1)" in CERTIFIED*) ok "8. red-no-suite: sp-vd-r1 CERTIFIED" ;;
    *) bad "8. red-no-suite: sp-vd-r1 CERTIFIED" "got: $(landstate sp-vd-r1)" ;; esac
case "$(landstate sp-vd-r2)" in CERTIFIED*) ok "8. red-no-suite: sp-vd-r2 CERTIFIED" ;;
    *) bad "8. red-no-suite: sp-vd-r2 CERTIFIED" "got: $(landstate sp-vd-r2)" ;; esac
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

printf 'green\nhead-sha: %s\n' "$batch_head9" > "$FORGE_STATUS_FILE"
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

# Case 12 (PR old, run freshly started → no cancellation) demoted to T1:
# test-verdict-action.sh case 3.

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
printf 'green\nhead-sha: %s\n' "$batch_head13" > "$FORGE_STATUS_FILE"
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

# Case 14 (PR old, run old, last-activity recent → progressing, no cancel)
# demoted to T1: test-verdict-action.sh case 4.

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

# Case 17 (single-step job: max of two last-activity lines) demoted to T1:
# test-verdict-action.sh case 5.
# Case 18 (per-repo CI_MAXSEC override) demoted to T1: test-verdict-action.sh
# case 6.

# =============================================================================
# 19. FLAKY SUITE — batch of 2 members, suite fails once then passes on retry.
#     Verdict ejects neither member; observe-flake is NOT called (the suite
#     arrived on a red-suite: line — the gate's non-flaky classification wins).
#     POSITIVE CONTROL (case 20): same shape but suite fails twice → ejected.
#     REGRESSION TEST (case 21): single member, same forge status, same repro
#     behaviour — observe-flake must not be called (sp-6zw2p).
# =============================================================================
# Mock: first call for each unique test_ref exits 1 (red); second exits 0 (green).
# State is tracked per-ref via a counter file in REPRO_STATE_DIR.
REPRO_STATE_DIR="$TMP/repro-state-19"
mkdir -p "$REPRO_STATE_DIR"
export REPRO_STATE_DIR
cat > "$SH/repro-flaky.sh" <<'REPRO'
#!/usr/bin/env bash
shift 2; shift 2  # skip --mode serial --suites csv
ref="${1:-}"
key="$(printf '%s' "$ref" | sha256sum | cut -c1-8)"
count_file="$REPRO_STATE_DIR/$key"
count=0; [ -f "$count_file" ] && count=$(cat "$count_file")
count=$(( count + 1 ))
printf '%d\n' "$count" > "$count_file"
[ "$count" -eq 1 ] && exit 1 || exit 0
REPRO
chmod +x "$SH/repro-flaky.sh"

base_sha19="$(git -C "$REPO" rev-parse origin/main)"
for id in sp-vd-f1 sp-vd-f2; do
    bwt19="$RUN/worktree/$id"
    git -C "$REPO" worktree add -q -b "spira/$id" "$bwt19" origin/main 2>/dev/null || true
    printf '%s\n' "$id" > "$bwt19/$id.txt"
    git -C "$bwt19" add -A
    git -C "$bwt19" commit -q -m "$id: work"
    printf 'BATCHED %s %s\n' "$(git -C "$REPO" rev-parse "spira/$id")" "$(date +%s)" \
        > "$LANDSTATE/$id"
done
tip_f1="$(git -C "$REPO" rev-parse "spira/sp-vd-f1")"
tip_f2="$(git -C "$REPO" rev-parse "spira/sp-vd-f2")"
wt19="$RUN/worktree/.b19"
git -C "$REPO" worktree add -q --detach "$wt19" "$base_sha19" 2>/dev/null || true
git -C "$wt19" merge -q --no-edit --no-ff -m "spira: land sp-vd-f1" "$tip_f1" >/dev/null 2>&1
git -C "$wt19" merge -q --no-edit --no-ff -m "spira: land sp-vd-f2" "$tip_f2" >/dev/null 2>&1
batch_head19="$(git -C "$wt19" rev-parse HEAD)"
git -C "$REPO" worktree remove -f "$wt19" 2>/dev/null || true
{ printf 'pr=71\nhead=%s\nbase=%s\nmembers=sp-vd-f1:%s sp-vd-f2:%s\nopened=%s\n' \
    "$batch_head19" "$base_sha19" "$tip_f1" "$tip_f2" "$(date +%s)"; } > "$(batch_file)"
printf 'red\nred-suite: test-flaky-repro.sh\n' > "$FORGE_STATUS_FILE"
: > "$SUITES_LOG"
out="$(SPIRA_QUEUE_REPRO_BATCH="$SH/repro-flaky.sh" verdict "$REPONAME")"
case "$(landstate sp-vd-f1)" in CERTIFIED*) ok "19. flaky-repro: sp-vd-f1 not ejected (CERTIFIED)" ;;
    EJECTED*) bad "19. flaky-repro: sp-vd-f1 not ejected" "was EJECTED" ;;
    *) bad "19. flaky-repro: sp-vd-f1 not ejected" "got: $(landstate sp-vd-f1)" ;; esac
case "$(landstate sp-vd-f2)" in CERTIFIED*) ok "19. flaky-repro: sp-vd-f2 not ejected (CERTIFIED)" ;;
    EJECTED*) bad "19. flaky-repro: sp-vd-f2 not ejected" "was EJECTED" ;;
    *) bad "19. flaky-repro: sp-vd-f2 not ejected" "got: $(landstate sp-vd-f2)" ;; esac
want   "19. flaky-repro: suite named in output"            "test-flaky-repro.sh" "$out"
nowant "19. flaky-repro: observe-flake not called"         "test-flaky-repro.sh" "$(cat "$SUITES_LOG")"
nowant "19. flaky-repro: no ejection"                      "ejected"             "$out"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 20. ALWAYS-RED CONTROL — same batch shape as case 19, but suite fails twice.
#     The member must still be ejected (retry does not suppress a real failure).
# =============================================================================
cat > "$SH/repro-always-red.sh" <<'REPRO'
#!/usr/bin/env bash
exit 1
REPRO
chmod +x "$SH/repro-always-red.sh"

base_sha20="$(git -C "$REPO" rev-parse origin/main)"
bwt20="$RUN/worktree/sp-vd-g1"
git -C "$REPO" worktree add -q -b "spira/sp-vd-g1" "$bwt20" origin/main 2>/dev/null || true
printf 'sp-vd-g1\n' > "$bwt20/sp-vd-g1.txt"
git -C "$bwt20" add -A
git -C "$bwt20" commit -q -m "sp-vd-g1: work"
tip_g1="$(git -C "$REPO" rev-parse "spira/sp-vd-g1")"
printf 'BATCHED %s %s\n' "$tip_g1" "$(date +%s)" > "$LANDSTATE/sp-vd-g1"
{ printf 'pr=72\nhead=%s\nbase=%s\nmembers=sp-vd-g1:%s\nopened=%s\n' \
    "$tip_g1" "$base_sha20" "$tip_g1" "$(date +%s)"; } > "$(batch_file)"
printf 'red\nred-suite: test-always-red.sh\n' > "$FORGE_STATUS_FILE"
out="$(SPIRA_QUEUE_REPRO_BATCH="$SH/repro-always-red.sh" verdict "$REPONAME")"
case "$(landstate sp-vd-g1)" in EJECTED*) ok "20. always-red: sp-vd-g1 ejected" ;;
    *) bad "20. always-red: sp-vd-g1 ejected" "got: $(landstate sp-vd-g1)" ;; esac
want "20. always-red: ejection reported" "ejected" "$out"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# Cases 21-23 (TERM trap during replay; MAXPAR bounding concurrent replays)
# moved to test-verdict-replay.sh (tier T3, off the per-push path): they cost
# 8s sleeps x3 members x2 batches here, and the parallel-vs-serial assertion
# was a load-sensitive wall-clock ratio rather than a deterministic count.

# =============================================================================
# 24. COULD-NOT-JUDGE MEMBER — rc=2 from one member does not exonerate others;
#     the guilty member (rc=0) is still ejected.
# =============================================================================
# Two members: sp-vd-r1 always fails (rc=1 → _repro_is_red returns 0 → ejected).
# sp-vd-r2's repro exits 2 (harness fault). Only sp-vd-r1 must be ejected.
cat > "$SH/repro-fault-one.sh" <<'REPRO'
#!/usr/bin/env bash
# Positional parsing: --mode <m> --suites <csv> <ref>
while [[ "${1:-}" == --* ]]; do shift 2; done
ref="${1:-}"
# Fault if REPRO_FAULT_TIP (sp-vd-r2's tip) is a parent of the merge ref.
# SPIRA_REPO is set by _repro_is_red; --no-walk avoids traversal.
git -C "${SPIRA_REPO:-.}" log --no-walk --pretty="%P" "${ref:-}" 2>/dev/null \
    | grep -qF "${REPRO_FAULT_TIP:-}" && exit 2
exit 1
REPRO
chmod +x "$SH/repro-fault-one.sh"

base_sha23="$(git -C "$REPO" rev-parse origin/main)"
for id in sp-vd-r1 sp-vd-r2; do
    bwt23="$RUN/worktree/$id"
    git -C "$REPO" worktree add -q -b "spira/$id" "$bwt23" origin/main 2>/dev/null || true
    printf '%s\n' "$id" > "$bwt23/$id.txt"
    git -C "$bwt23" add -A
    git -C "$bwt23" commit -q -m "$id: work"
    printf 'BATCHED %s %s\n' "$(git -C "$REPO" rev-parse "spira/$id")" "$(date +%s)" > "$LANDSTATE/$id"
done
tip_r1="$(git -C "$REPO" rev-parse "spira/sp-vd-r1")"
tip_r2="$(git -C "$REPO" rev-parse "spira/sp-vd-r2")"
# Tell the stub which tip to fault: r2's tip is a parent of its merge sha.
REPRO_FAULT_TIP="$tip_r2"
export REPRO_FAULT_TIP
wt23="$RUN/worktree/.b23"
git -C "$REPO" worktree add -q --detach "$wt23" "$base_sha23" 2>/dev/null || true
git -C "$wt23" merge -q --no-edit --no-ff -m "spira: land sp-vd-r1" "$tip_r1" >/dev/null 2>&1
git -C "$wt23" merge -q --no-edit --no-ff -m "spira: land sp-vd-r2" "$tip_r2" >/dev/null 2>&1
batch_head23="$(git -C "$wt23" rev-parse HEAD)"
git -C "$REPO" worktree remove -f "$wt23" 2>/dev/null || true
{ printf 'pr=83\nhead=%s\nbase=%s\nmembers=sp-vd-r1:%s sp-vd-r2:%s\nopened=%s\n' \
    "$batch_head23" "$base_sha23" "$tip_r1" "$tip_r2" "$(date +%s)"; } > "$(batch_file)"
printf 'red\nred-suite: test-fault.sh\n' > "$FORGE_STATUS_FILE"
SPIRA_QUEUE_REPRO_BATCH="$SH/repro-fault-one.sh" SPIRA_BATCH_MAXPAR=2 verdict "$REPONAME" >/dev/null
case "$(landstate sp-vd-r1)" in EJECTED*) ok "24. could-not-judge: sp-vd-r1 ejected" ;;
    *) bad "24. could-not-judge: sp-vd-r1 ejected" "got: $(landstate sp-vd-r1)" ;; esac
case "$(landstate sp-vd-r2)" in EJECTED*) bad "24. could-not-judge: sp-vd-r2 not ejected" "was EJECTED" ;;
    *) ok "24. could-not-judge: sp-vd-r2 not ejected (got: $(landstate sp-vd-r2 || echo none))" ;; esac
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 25. EJECTION SET DETERMINISTIC — same 3-member batch run twice; ejected set
#     is the same regardless of completion order.
# =============================================================================
cat > "$SH/repro-first-red.sh" <<'REPRO'
#!/usr/bin/env bash
# First unique ref seen exits 1 (red/ejected); all others exit 0 (green).
while [[ "${1:-}" == --* ]]; do shift 2; done
ref="${1:-}"
key="$(printf '%s' "$ref" | sha256sum | cut -c1-8)"
first_file="$REPRO_STATE_DIR/first-$key"
if [ ! -f "$first_file" ]; then
    printf '1\n' > "$first_file"
    exit 1
fi
exit 0
REPRO
chmod +x "$SH/repro-first-red.sh"

_run_shuffled_batch() {
    local bsha; bsha="$(git -C "$REPO" rev-parse origin/main)"
    local ids=(sp-vd-s1 sp-vd-s2 sp-vd-s3)
    for id in "${ids[@]}"; do
        local bwt="$RUN/worktree/$id"
        git -C "$REPO" worktree add -q -b "spira/$id" "$bwt" origin/main 2>/dev/null || true
        printf '%s\n' "$id" > "$bwt/$id.txt"
        git -C "$bwt" add -A
        git -C "$bwt" commit -q -m "$id: work"
        printf 'BATCHED %s %s\n' "$(git -C "$REPO" rev-parse "spira/$id")" "$(date +%s)" > "$LANDSTATE/$id"
    done
    local t1 t2 t3
    t1="$(git -C "$REPO" rev-parse "spira/sp-vd-s1")"
    t2="$(git -C "$REPO" rev-parse "spira/sp-vd-s2")"
    t3="$(git -C "$REPO" rev-parse "spira/sp-vd-s3")"
    local wt="$RUN/worktree/.b25"
    git -C "$REPO" worktree add -q --detach "$wt" "$bsha" 2>/dev/null || true
    git -C "$wt" merge -q --no-edit --no-ff -m "spira: land sp-vd-s1" "$t1" >/dev/null 2>&1
    git -C "$wt" merge -q --no-edit --no-ff -m "spira: land sp-vd-s2" "$t2" >/dev/null 2>&1
    git -C "$wt" merge -q --no-edit --no-ff -m "spira: land sp-vd-s3" "$t3" >/dev/null 2>&1
    local bhead; bhead="$(git -C "$wt" rev-parse HEAD)"
    git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    { printf 'pr=84\nhead=%s\nbase=%s\nmembers=sp-vd-s1:%s sp-vd-s2:%s sp-vd-s3:%s\nopened=%s\n' \
        "$bhead" "$bsha" "$t1" "$t2" "$t3" "$(date +%s)"; } > "$(batch_file)"
    printf 'red\nred-suite: test-det.sh\n' > "$FORGE_STATUS_FILE"
    SPIRA_QUEUE_REPRO_BATCH="$SH/repro-first-red.sh" SPIRA_BATCH_MAXPAR=3 verdict "$REPONAME" >/dev/null
    landstate sp-vd-s1 | head -1 | cut -d' ' -f1
    clean_case >/dev/null 2>&1
    git -C "$REPO" fetch -q origin 2>/dev/null || true
}

REPRO_STATE_DIR="$TMP/repro-state-25a"
mkdir -p "$REPRO_STATE_DIR"
export REPRO_STATE_DIR
res25a="$(_run_shuffled_batch)"

REPRO_STATE_DIR="$TMP/repro-state-25b"
mkdir -p "$REPRO_STATE_DIR"
export REPRO_STATE_DIR
res25b="$(_run_shuffled_batch)"

[ "$res25a" = "$res25b" ] \
    && ok "25. deterministic: same ejected set both runs ($res25a)" \
    || bad "25. deterministic: ejected set differs" "run1=$res25a run2=$res25b"

# =============================================================================
# 26. RED-SUITE INELIGIBLE FOR QUARANTINE (sp-6zw2p) — single member; forge
#     classified the suite as red-twice (red-suite:), so it is not flaky by
#     the gate's own verdict. Local repro returns red-then-green (would look
#     flaky) but the gate's classification wins: observe-flake must not be
#     called. Member survives (repro was inconclusive, not confirmatory).
#     POSITIVE CONTROL: case 20 proves that a truly red suite (fails twice
#     in repro) still ejects the member; this case proves survival is from
#     inconclusive repro, not from a missing ejection path.
# =============================================================================
REPRO_STATE_DIR26="$TMP/repro-state-26"
mkdir -p "$REPRO_STATE_DIR26"
export REPRO_STATE_DIR26
cat > "$SH/repro-redtwice.sh" <<'REPRO'
#!/usr/bin/env bash
shift 2; shift 2  # skip --mode <mode> --suites <csv>
ref="${1:-}"
key="$(printf '%s' "$ref" | sha256sum | cut -c1-8)"
count_file="$REPRO_STATE_DIR26/$key"
count=0; [ -f "$count_file" ] && count=$(cat "$count_file")
count=$(( count + 1 ))
printf '%d\n' "$count" > "$count_file"
[ "$count" -eq 1 ] && exit 1 || exit 0
REPRO
chmod +x "$SH/repro-redtwice.sh"

base_sha26="$(git -C "$REPO" rev-parse origin/main)"
bwt26="$RUN/worktree/sp-vd-r26"
git -C "$REPO" worktree add -q -b "spira/sp-vd-r26" "$bwt26" origin/main 2>/dev/null || true
printf 'sp-vd-r26\n' > "$bwt26/sp-vd-r26.txt"
git -C "$bwt26" add -A
git -C "$bwt26" commit -q -m "sp-vd-r26: work"
tip_r26="$(git -C "$REPO" rev-parse "spira/sp-vd-r26")"
printf 'BATCHED %s %s\n' "$tip_r26" "$(date +%s)" > "$LANDSTATE/sp-vd-r26"
{ printf 'pr=91\nhead=%s\nbase=%s\nmembers=sp-vd-r26:%s\nopened=%s\n' \
    "$tip_r26" "$base_sha26" "$tip_r26" "$(date +%s)"; } > "$(batch_file)"
printf 'red\nred-suite: test-gate-classified.sh\n' > "$FORGE_STATUS_FILE"
: > "$SUITES_LOG"
out="$(SPIRA_QUEUE_REPRO_BATCH="$SH/repro-redtwice.sh" verdict "$REPONAME")"
case "$(landstate sp-vd-r26)" in CERTIFIED*) ok "26. red-suite-no-quarantine: member survived (CERTIFIED)" ;;
    EJECTED*) bad "26. red-suite-no-quarantine: member survived" "was EJECTED" ;;
    *) bad "26. red-suite-no-quarantine: member survived" "got: $(landstate sp-vd-r26)" ;; esac
nowant "26. red-suite-no-quarantine: observe-flake not called" \
    "test-gate-classified.sh" "$(cat "$SUITES_LOG")"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# Case 27 (provision_fault normalized to harness_fault: rerun, no bisect)
# demoted to T1: test-verdict-action.sh case 7 (verdict_normalize_status +
# verdict_action). verdict_normalize_status is a single case arm called once,
# at the top of _verdict_process, before the status dispatch below it runs.

# =============================================================================
# 28. GREEN, BASE MOVED (CONFLICTING) — one member conflicts with the new base;
#     PR closed, members returned to CERTIFIED, conflict member named in log.
#     POSITIVE CONTROL: case 7 proves non-conflicting base move is rebuilt.
# =============================================================================
# sp-vd-c1 touches only c1.txt; sp-vd-c2 touches conflict.txt="from-c2".
# The base advance writes conflict.txt="from-base", creating a merge conflict.
base_sha28="$(git -C "$REPO" rev-parse origin/main)"
bwt28_c1="$RUN/worktree/sp-vd-c1"
bwt28_c2="$RUN/worktree/sp-vd-c2"
git -C "$REPO" worktree add -q -b "spira/sp-vd-c1" "$bwt28_c1" origin/main 2>/dev/null || true
git -C "$REPO" worktree add -q -b "spira/sp-vd-c2" "$bwt28_c2" origin/main 2>/dev/null || true
printf 'sp-vd-c1\n' > "$bwt28_c1/c1.txt"
git -C "$bwt28_c1" add -A && git -C "$bwt28_c1" commit -q -m "sp-vd-c1: work"
printf 'from-c2\n' > "$bwt28_c2/conflict.txt"
git -C "$bwt28_c2" add -A && git -C "$bwt28_c2" commit -q -m "sp-vd-c2: work"
tip28_c1="$(git -C "$REPO" rev-parse "spira/sp-vd-c1")"
tip28_c2="$(git -C "$REPO" rev-parse "spira/sp-vd-c2")"
wt28="$RUN/worktree/.b28"
git -C "$REPO" worktree add -q --detach "$wt28" "$base_sha28" 2>/dev/null || true
git -C "$wt28" merge -q --no-edit --no-ff -m "spira: land sp-vd-c1" "$tip28_c1" >/dev/null 2>&1
git -C "$wt28" merge -q --no-edit --no-ff -m "spira: land sp-vd-c2" "$tip28_c2" >/dev/null 2>&1
batch_head28="$(git -C "$wt28" rev-parse HEAD)"
git -C "$REPO" worktree remove -f "$wt28" 2>/dev/null || true
printf 'BATCHED %s %s\n' "$tip28_c1" "$(date +%s)" > "$LANDSTATE/sp-vd-c1"
printf 'BATCHED %s %s\n' "$tip28_c2" "$(date +%s)" > "$LANDSTATE/sp-vd-c2"
{ printf 'pr=95\nhead=%s\nbase=%s\nmembers=sp-vd-c1:%s sp-vd-c2:%s\nopened=%s\nbranch=spira/queue/test28\n' \
    "$batch_head28" "$base_sha28" "$tip28_c1" "$tip28_c2" "$(date +%s)"; } > "$(batch_file)"
# Advance main with a conflicting commit: same file, different content.
bwt28_adv="$RUN/worktree/.adv28"
git -C "$REPO" worktree remove -f "$bwt28_adv" 2>/dev/null || true
git -C "$REPO" worktree add -q --detach "$bwt28_adv" origin/main
printf 'from-base\n' > "$bwt28_adv/conflict.txt"
git -C "$bwt28_adv" add -A
git -C "$bwt28_adv" commit -q -m "other: conflicting landing"
git -C "$bwt28_adv" push -q origin "HEAD:main"
git -C "$REPO" worktree remove -f "$bwt28_adv" 2>/dev/null || true
git -C "$REPO" fetch -q origin

printf 'green\nhead-sha: %s\n' "$batch_head28" > "$FORGE_STATUS_FILE"
out="$(verdict "$REPONAME")"
want "28. moved-conflict: pr-close called"        "close"      "$(cat "$FORGE_LOG")"
case "$(landstate sp-vd-c1)" in CERTIFIED*) ok "28. moved-conflict: sp-vd-c1 CERTIFIED" ;;
    *) bad "28. moved-conflict: sp-vd-c1 CERTIFIED" "got: $(landstate sp-vd-c1)" ;; esac
case "$(landstate sp-vd-c2)" in CERTIFIED*) ok "28. moved-conflict: sp-vd-c2 CERTIFIED" ;;
    *) bad "28. moved-conflict: sp-vd-c2 CERTIFIED" "got: $(landstate sp-vd-c2)" ;; esac
is   "28. moved-conflict: batch record removed"   "0" "$([ -f "$(batch_file)" ] && echo 1 || echo 0)"
want "28. moved-conflict: conflict member named"  "sp-vd-c2"   "$out"
want "28. moved-conflict: base-moved reported"    "base moved"  "$out"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 29. BISECT PERSISTS ITS HALVES (sp-y931m) — case 8's red-no-suite bisect must
#     write $SPIRA_QUEUE_DIR/<repo>/bisect recording the first half, not just
#     mark epochs. Without this the next cut is chosen by priority alone and
#     can re-admit the very branch under isolation, as PRs 302-304 did.
# =============================================================================
build_batch sp-vd-bs1 sp-vd-bs2 sp-vd-bs3 sp-vd-bs4 > /dev/null
printf 'red\n' > "$FORGE_STATUS_FILE"
out="$(verdict "$REPONAME")"
bisect_f="$QUEUEDIR/$REPONAME/bisect"
is   "29. bisect split: state file written" "1" "$([ -s "$bisect_f" ] && echo 1 || echo 0)"
recorded_half="$(head -1 "$bisect_f" 2>/dev/null)"
want   "29. bisect split: recorded half names sp-vd-bs1"    "sp-vd-bs1" "$recorded_half"
want   "29. bisect split: recorded half names sp-vd-bs2"    "sp-vd-bs2" "$recorded_half"
nowant "29. bisect split: recorded half excludes sp-vd-bs3" "sp-vd-bs3" "$recorded_half"
second_line="$(sed -n '2p' "$bisect_f" 2>/dev/null)"
want "29. bisect split: sibling half parked on line 2" "sp-vd-bs3" "$second_line"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 30. BISECT NARROWS TO ONE, RED AGAIN — a red batch whose sole member is
#     exactly the bisect's current forced group ejects immediately (no
#     two-strike wait; that track is for an organic single-branch red, not a
#     branch bisection has already isolated). A build-error annotation, when
#     present, is logged on the ejected bead's note.
#     POSITIVE CONTROL: test-attribution.sh case 15 proves the two-strike
#     track still applies when there is no bisect state to match against.
# =============================================================================
build_batch sp-vd-bo1 > /dev/null
bo1_tip="$(git -C "$REPO" rev-parse spira/sp-vd-bo1)"
base_sha_30="$(grep '^base=' "$(batch_file)" | cut -d= -f2)"
mkdir -p "$QUEUEDIR/$REPONAME"
printf '%s sp-vd-bo1:%s\n' "$base_sha_30" "$bo1_tip" > "$QUEUEDIR/$REPONAME/bisect"
{ printf 'red\n'; printf 'build-error: error: could not compile `spira-core`\n'; } > "$FORGE_STATUS_FILE"
verdict "$REPONAME" > /dev/null
case "$(landstate sp-vd-bo1)" in
    EJECTED*) ok "30. bisect singleton: ejected on first strike" ;;
    *) bad "30. bisect singleton: ejected on first strike" "got: $(landstate sp-vd-bo1)" ;;
esac
is   "30. bisect singleton: bisect state cleared" "0" \
     "$([ -f "$QUEUEDIR/$REPONAME/bisect" ] && echo 1 || echo 0)"
want "30. bisect singleton: build error logged on the bead" "could not compile" "$(cat "$MAIL_LOG")"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 31. BISECT RESOLVES ON GREEN — a batch that is exactly the bisect's current
#     forced group lands clean; the group is innocent, so the bisect advances
#     to the sibling parked when it was split.
# =============================================================================
batch_head31="$(build_batch sp-vd-bg1 sp-vd-bg2)"
bg1_tip="$(git -C "$REPO" rev-parse spira/sp-vd-bg1)"
bg2_tip="$(git -C "$REPO" rev-parse spira/sp-vd-bg2)"
base_sha_31="$(grep '^base=' "$(batch_file)" | cut -d= -f2)"
mkdir -p "$QUEUEDIR/$REPONAME"
{
    printf '%s sp-vd-bg1:%s sp-vd-bg2:%s\n' "$base_sha_31" "$bg1_tip" "$bg2_tip"
    printf '%s sp-vd-bg3:deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\n' "$base_sha_31"
} > "$QUEUEDIR/$REPONAME/bisect"
printf 'green\nhead-sha: %s\n' "$batch_head31" > "$FORGE_STATUS_FILE"
out="$(verdict "$REPONAME")"
want "31. bisect resolve: fast-forward landed" "landed by fast-forward" "$out"
is   "31. bisect resolve: advances to sibling" "$base_sha_31 sp-vd-bg3:deadbeefdeadbeefdeadbeefdeadbeefdeadbeef" \
     "$(head -1 "$QUEUEDIR/$REPONAME/bisect" 2>/dev/null)"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 32. STALE QUEUE REF REAPING — after a green verdict, local spira/queue/*
#     refs whose tips are already on the base are cleaned up by
#     _reap_stale_queue_refs. A ref whose tip is NOT on the base (from an
#     ejected or pending batch) must survive.
#
#     POSITIVE CONTROL: the stale ref must actually be gone.
#     PAIR: the live ref (not an ancestor) must still be present.
# =============================================================================
batch_head32="$(build_batch sp-vd-ra sp-vd-rb)"
# stale-32: tip is the original base — already on main after the batch lands.
stale_sha32="$(git -C "$REPO" rev-parse origin/main 2>/dev/null)"
git -C "$REPO" branch "spira/queue/stale-32" "$stale_sha32"
# live-32: tip is a commit that is NOT on main yet.
{
    git -C "$REPO" worktree add -q --detach "$RUN/worktree/.live32" origin/main
    printf 'live\n' > "$RUN/worktree/.live32/live32.txt"
    git -C "$RUN/worktree/.live32" add -A
    git -C "$RUN/worktree/.live32" commit -q -m "live32: pending work"
    live32_sha="$(git -C "$RUN/worktree/.live32" rev-parse HEAD)"
    git -C "$REPO" worktree remove -f "$RUN/worktree/.live32" 2>/dev/null || true
} 2>/dev/null
git -C "$REPO" branch "spira/queue/live-32" "$live32_sha"
# Point the current batch at a unique branch name.
git -C "$REPO" branch -f "spira/queue/test32" "$batch_head32"
git -C "$REPO" push -q origin "spira/queue/test32"
git -C "$REPO" fetch -q origin
{
    grep -v '^branch=' "$(batch_file)"
    printf 'branch=spira/queue/test32\n'
} > "$(batch_file).$$" && mv -f "$(batch_file).$$" "$(batch_file)"

printf 'green\nhead-sha: %s\n' "$batch_head32" > "$FORGE_STATUS_FILE"
verdict "$REPONAME" > /dev/null
is "32. reap: stale ancestor queue ref cleaned up" "0" \
    "$(git -C "$REPO" show-ref --verify "refs/heads/spira/queue/stale-32" >/dev/null 2>&1 && echo 1 || echo 0)"
is "32. reap (pair): non-ancestor queue ref survives" "1" \
    "$(git -C "$REPO" show-ref --verify "refs/heads/spira/queue/live-32" >/dev/null 2>&1 && echo 1 || echo 0)"
# Cleanup the surviving live branch before clean_case (which skips spira/queue/*).
git -C "$REPO" branch -D "spira/queue/live-32" 2>/dev/null || true
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 33. GREEN, CHECK-STATUS OMITS HEAD-SHA — a missing head-sha means we cannot
#     confirm CI tested the sealed batch head, so it must be treated as
#     unverifiable: no push, no pr-close, batch held for retry next pass.
#     POSITIVE CONTROL: without the fix, an empty ci_head fails the "!="
#     mismatch test vacuously and falls through to fast-forward landing —
#     before the fix this case pushes main and closes batch_file.
# =============================================================================
build_batch sp-vd-hs1 sp-vd-hs2 > /dev/null
printf 'green\n' > "$FORGE_STATUS_FILE"
before_main33="$(remote_main)"
out="$(verdict "$REPONAME")"
is   "33. missing-sha: no push"              "$before_main33" "$(remote_main)"
nowant "33. missing-sha: no pr-close"        "close"          "$(cat "$FORGE_LOG")"
case "$(landstate sp-vd-hs1)" in BATCHED*) ok "33. missing-sha: sp-vd-hs1 stays BATCHED" ;;
    *) bad "33. missing-sha: sp-vd-hs1 stays BATCHED" "got: $(landstate sp-vd-hs1)" ;; esac
case "$(landstate sp-vd-hs2)" in BATCHED*) ok "33. missing-sha: sp-vd-hs2 stays BATCHED" ;;
    *) bad "33. missing-sha: sp-vd-hs2 stays BATCHED" "got: $(landstate sp-vd-hs2)" ;; esac
is   "33. missing-sha: batch record kept"    "1" "$([ -f "$(batch_file)" ] && echo 1 || echo 0)"
want "33. missing-sha: reported"             "head-sha missing" "$out"
clean_case

# =============================================================================
# 34. ATTRIBUTING A STASHED BATCH (express takeover, sp-os27w) — batch.sh moves
#     a not-green open batch to attributing-<pr> instead of waiting on it, and
#     verdict.sh must still run it through the same dispatch (here: red-no-suite
#     bisect, as case 8), after releasing the shared queue lock — a slow local
#     reproduction here must never hold up batch.sh (or a later verdict pass)
#     working the rest of the queue.
#
#     POSITIVE CONTROL: case 8 proves this dispatch against the primary
#     batch_file; this proves the identical outcome against an attributing-<pr>
#     file, plus the lock release the primary path never needed.
# =============================================================================
build_batch sp-vd-at1 sp-vd-at2 > /dev/null
mv -f "$(batch_file)" "$QUEUEDIR/$REPONAME/attributing-42"
printf 'red\n' > "$FORGE_STATUS_FILE"

LOCK_PROBE_FILE="$TMP/lock-probe"; export LOCK_PROBE_FILE
: > "$LOCK_PROBE_FILE"
cp "$SH/forge-fixture.sh" "$TMP/forge-fixture.sh.bak"
# Same fixture as above, but check-status sleeps: long enough that a probe taken
# partway through it (forked below, own subshell) can tell whether $name's shared
# queue lock is held or free while this (attribution's) check-status is in flight.
cat > "$SH/forge-fixture.sh" <<'FORGE'
#!/usr/bin/env bash
cmd="${1:-}"; shift; repo="${1:-}"; shift
case "$cmd" in
    check-status)
        ( sleep 1
          if flock -n "${SPIRA_QUEUE_DIR}/fixture-repo/lock" true 2>/dev/null; then
              printf 'free\n' > "${LOCK_PROBE_FILE}"
          else
              printf 'held\n' > "${LOCK_PROBE_FILE}"
          fi
        ) &
        sleep 3
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

out="$(verdict "$REPONAME")"
mv -f "$TMP/forge-fixture.sh.bak" "$SH/forge-fixture.sh"
chmod +x "$SH/forge-fixture.sh"

is   "34. attributing: queue lock free during attribution" "free" \
     "$(cat "$LOCK_PROBE_FILE" 2>/dev/null)"
want "34. attributing: names the stashed PR"   "attributing stashed batch PR 42" "$out"
want "34. attributing: bisect reported"        "bisect halved"                   "$out"
is   "34. attributing: file removed" "0" \
     "$([ -f "$QUEUEDIR/$REPONAME/attributing-42" ] && echo 1 || echo 0)"
case "$(landstate sp-vd-at1)" in CERTIFIED*) ok "34. attributing: sp-vd-at1 CERTIFIED" ;;
    *) bad "34. attributing: sp-vd-at1 CERTIFIED" "got: $(landstate sp-vd-at1)" ;; esac
case "$(landstate sp-vd-at2)" in CERTIFIED*) ok "34. attributing: sp-vd-at2 CERTIFIED" ;;
    *) bad "34. attributing: sp-vd-at2 CERTIFIED" "got: $(landstate sp-vd-at2)" ;; esac
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 35. EJECTION MAIL — REPRODUCED-ALONE METHOD (sp-bdkbw). Ryan on a prior
#     ejection mail: "this email doesn't help me understand why the bead was
#     ejected... what was the change and how was it determined to be the one
#     ejected?" The mail must name the method actually used, the bead's title,
#     a diffstat against the base, and the failing assertion lines.
#     Same shape as case 20 (always-red): the repro genuinely reruns the
#     suite and it fails again, so "Reproduced alone" is the honest claim.
#     Each assertion here fails against the pre-fix body, which only ever
#     said "after reproducing CI failures" with no title, diffstat or method.
# =============================================================================
testdb_seed <<JSONL
{"id":"sp-vd-em1","title":"fix the frobnicator overflow","status":"open","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"description":"The frobnicator overflows under sustained load and corrupts the output buffer."}
JSONL

cat > "$SH/repro-em1.sh" <<'REPRO'
#!/usr/bin/env bash
printf 'FAIL: test_frobnicator_overflow (expected 200 got 500)\n'
exit 1
REPRO
chmod +x "$SH/repro-em1.sh"

base_sha35="$(git -C "$REPO" rev-parse origin/main)"
bwt35="$RUN/worktree/sp-vd-em1"
git -C "$REPO" worktree add -q -b "spira/sp-vd-em1" "$bwt35" origin/main 2>/dev/null || true
printf 'sp-vd-em1\n' > "$bwt35/sp-vd-em1.txt"
git -C "$bwt35" add -A
git -C "$bwt35" commit -q -m "sp-vd-em1: work"
tip_em1="$(git -C "$REPO" rev-parse "spira/sp-vd-em1")"
printf 'BATCHED %s %s\n' "$tip_em1" "$(date +%s)" > "$LANDSTATE/sp-vd-em1"
{ printf 'pr=91\nhead=%s\nbase=%s\nmembers=sp-vd-em1:%s\nopened=%s\n' \
    "$tip_em1" "$base_sha35" "$tip_em1" "$(date +%s)"; } > "$(batch_file)"
printf 'red\nred-suite: test-always-red.sh\nrun-url: https://example.invalid/actions/runs/9001\n' \
    > "$FORGE_STATUS_FILE"
SPIRA_QUEUE_REPRO_BATCH="$SH/repro-em1.sh" verdict "$REPONAME" > /dev/null
mail35="$(cat "$MAIL_LOG")"
case "$(landstate sp-vd-em1)" in EJECTED*) ok "35. reproduced-alone: sp-vd-em1 ejected" ;;
    *) bad "35. reproduced-alone: sp-vd-em1 ejected" "got: $(landstate sp-vd-em1)" ;; esac
want   "35. reproduced-alone: names the method"  "Reproduced alone"                            "$mail35"
want   "35. reproduced-alone: names the title"   "fix the frobnicator overflow"                "$mail35"
want   "35. reproduced-alone: shows a diffstat"  "file changed"                                "$mail35"
want   "35. reproduced-alone: failing assertion" "test_frobnicator_overflow"                   "$mail35"
want   "35. reproduced-alone: CI run link"       "https://example.invalid/actions/runs/9001"   "$mail35"
want   "35. reproduced-alone: single-member batch noted" "this batch was this one branch"      "$mail35"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 36. EJECTION MAIL — SUITE-OVERLAP METHOD (sp-bdkbw). The local repro faults
#     (harness fault, never actually reran the suite), but the branch's diff
#     directly modifies the failing suite's own file — attributed by overlap,
#     not reproduction. The mail must say so and must NOT claim "Reproduced
#     alone", which would be a false claim of work the verdict never did.
# =============================================================================
testdb_seed <<JSONL
{"id":"sp-vd-em2","title":"quarantine the flaky uploader suite","status":"open","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"description":"Marks the uploader suite flaky pending a real fix."}
JSONL

cat > "$SH/repro-fault2.sh" <<'REPRO'
#!/usr/bin/env bash
exit 2
REPRO
chmod +x "$SH/repro-fault2.sh"

base_sha36="$(git -C "$REPO" rev-parse origin/main)"
bwt36="$RUN/worktree/sp-vd-em2"
git -C "$REPO" worktree add -q -b "spira/sp-vd-em2" "$bwt36" origin/main 2>/dev/null || true
mkdir -p "$bwt36/spira"
printf 'x\n' > "$bwt36/spira/test-vd-em2.sh"
git -C "$bwt36" add -A
git -C "$bwt36" commit -q -m "sp-vd-em2: work"
tip_em2="$(git -C "$REPO" rev-parse "spira/sp-vd-em2")"
printf 'BATCHED %s %s\n' "$tip_em2" "$(date +%s)" > "$LANDSTATE/sp-vd-em2"
{ printf 'pr=92\nhead=%s\nbase=%s\nmembers=sp-vd-em2:%s\nopened=%s\n' \
    "$tip_em2" "$base_sha36" "$tip_em2" "$(date +%s)"; } > "$(batch_file)"
printf 'red\nred-suite: spira/test-vd-em2.sh\nrun-url: https://example.invalid/actions/runs/9002\n' \
    > "$FORGE_STATUS_FILE"
SPIRA_QUEUE_REPRO_BATCH="$SH/repro-fault2.sh" verdict "$REPONAME" > /dev/null
mail36="$(cat "$MAIL_LOG")"
case "$(landstate sp-vd-em2)" in EJECTED*) ok "36. suite-overlap: sp-vd-em2 ejected" ;;
    *) bad "36. suite-overlap: sp-vd-em2 ejected" "got: $(landstate sp-vd-em2)" ;; esac
want   "36. suite-overlap: names the method"        "suite-overlap"                          "$mail36"
want   "36. suite-overlap: names the title"         "quarantine the flaky uploader suite"    "$mail36"
want   "36. suite-overlap: shows a diffstat"        "file changed"                            "$mail36"
nowant "36. suite-overlap: no false reproduction claim" "Reproduced alone"                     "$mail36"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 37. EJECTION MAIL — BISECT-SPLIT METHOD (sp-bdkbw). A bisect-singleton eject
#     (case 30's shape) is not a reproduction and not a suite-overlap match —
#     it is a binary-search narrowing. The mail must say "bisection split" and
#     name the sibling half left pending (not implicated), matching the Fix's
#     "ejected as part of a split group ... which half went back in".
# =============================================================================
testdb_seed <<JSONL
{"id":"sp-vd-em3","title":"tighten the batch bisector's halving math","status":"open","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"description":"Bisection halved the wrong side when the batch size was odd."}
JSONL

build_batch sp-vd-em3 > /dev/null
em3_tip="$(git -C "$REPO" rev-parse spira/sp-vd-em3)"
mkdir -p "$QUEUEDIR/$REPONAME"
base_sha_37="$(grep '^base=' "$(batch_file)" | cut -d= -f2)"
{
    printf '%s sp-vd-em3:%s\n' "$base_sha_37" "$em3_tip"
    printf '%s sp-vd-em3-sibling:deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\n' "$base_sha_37"
} > "$QUEUEDIR/$REPONAME/bisect"
{
    printf 'red\n'
    printf 'build-error: error: could not compile `spira-widget`\n'
    printf 'run-url: https://example.invalid/actions/runs/9003\n'
} > "$FORGE_STATUS_FILE"
verdict "$REPONAME" > /dev/null
mail37="$(cat "$MAIL_LOG")"
case "$(landstate sp-vd-em3)" in EJECTED*) ok "37. bisect-split: sp-vd-em3 ejected" ;;
    *) bad "37. bisect-split: sp-vd-em3 ejected" "got: $(landstate sp-vd-em3)" ;; esac
want   "37. bisect-split: names the method"         "bisection split"                              "$mail37"
want   "37. bisect-split: names the title"          "tighten the batch bisector's halving math"    "$mail37"
want   "37. bisect-split: shows a diffstat"         "file changed"                                  "$mail37"
want   "37. bisect-split: build-error evidence"     "could not compile"                             "$mail37"
want   "37. bisect-split: names the sibling half"   "sp-vd-em3-sibling"                              "$mail37"
nowant "37. bisect-split: no false reproduction claim" "Reproduced alone"                            "$mail37"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 38. BATCHER-OWNED PR RED (sp-lomk3): a batch PR whose own open-batch record
#     names owner=batcher is never split/ejected by verdict's own attribution —
#     the summoned batcher persona (sp-47kq1) gets the failing suites and CI run
#     link instead, via the batcher binary's judgement-ci subcommand. Case 39 is
#     the positive control: the identical CI red on an owner-less (legacy) batch
#     PR still runs today's ejection.
# =============================================================================
testdb_seed <<JSONL
{"id":"sp-vd-bo1","title":"batcher-owned member","status":"open","issue_type":"task","labels":["spira","plan","repo:$REPONAME"]}
JSONL

BATCHER_LOG="$TMP/batcher-log"; : > "$BATCHER_LOG"
cat > "$SH/batcher-stub.sh" <<STUB
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$BATCHER_LOG"
if [ "\${1:-}" = judgement-ci ]; then
    printf 'id=sp-vd-judged\n'
fi
STUB
chmod +x "$SH/batcher-stub.sh"
export SPIRA_BATCHER_BIN="$SH/batcher-stub.sh"

base_sha38="$(git -C "$REPO" rev-parse origin/main)"
bwt38="$RUN/worktree/sp-vd-bo1"
git -C "$REPO" worktree add -q -b "spira/sp-vd-bo1" "$bwt38" origin/main 2>/dev/null || true
printf 'sp-vd-bo1\n' > "$bwt38/sp-vd-bo1.txt"
git -C "$bwt38" add -A
git -C "$bwt38" commit -q -m "sp-vd-bo1: work"
tip_bo1="$(git -C "$REPO" rev-parse "spira/sp-vd-bo1")"
printf 'BATCHED %s %s\n' "$tip_bo1" "$(date +%s)" > "$LANDSTATE/sp-vd-bo1"
{ printf 'pr=93\nhead=%s\nbase=%s\nmembers=sp-vd-bo1:%s\nopened=%s\nowner=batcher\n' \
    "$tip_bo1" "$base_sha38" "$tip_bo1" "$(date +%s)"; } > "$(batch_file)"
printf 'red\nred-suite: test-owned.sh\nrun-url: https://example.invalid/actions/runs/9004\n' \
    > "$FORGE_STATUS_FILE"

out38="$(verdict "$REPONAME")"
want   "38. batcher-owned: summons judgement in the log" "summoned judgement" "$out38"
want   "38. batcher-owned: judgement-ci called with the red suite" "test-owned.sh" "$(cat "$BATCHER_LOG")"
want   "38. batcher-owned: judgement-ci called with the member" "sp-vd-bo1" "$(cat "$BATCHER_LOG")"
want   "38. batcher-owned: judgement-ci called with the run link" \
    "https://example.invalid/actions/runs/9004" "$(cat "$BATCHER_LOG")"
case "$(landstate sp-vd-bo1)" in BATCHED*) ok "38. batcher-owned: member left BATCHED, not ejected" ;;
    *) bad "38. batcher-owned: member left BATCHED, not ejected" "got: $(landstate sp-vd-bo1)" ;; esac
is     "38. batcher-owned: no ejection mail sent" "" "$(cat "$MAIL_LOG")"
is     "38. batcher-owned: PR never closed" "0" "$(grep -c 'close' "$FORGE_LOG")"
is     "38. batcher-owned: open batch record records the judgement bead" "sp-vd-judged" \
    "$(grep '^judgement=' "$(batch_file)" | cut -d= -f2-)"

calls_before_38b="$(wc -l < "$BATCHER_LOG")"
verdict "$REPONAME" > /dev/null
is     "38b. batcher-owned: a second red pass does not re-summon judgement" \
    "$calls_before_38b" "$(wc -l < "$BATCHER_LOG")"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 39. POSITIVE CONTROL for case 38: the same CI red shape on a batch PR with no
#     owner=batcher (today's batch.sh-cut record) still runs verdict's own
#     attribution — proves case 38's routing is conditioned on the record, not
#     a change to the default red path.
# =============================================================================
testdb_seed <<JSONL
{"id":"sp-vd-bo2","title":"legacy-owned member","status":"open","issue_type":"task","labels":["spira","plan","repo:$REPONAME"]}
JSONL

: > "$BATCHER_LOG"
cat > "$SH/repro-fault-bo2.sh" <<'REPRO'
#!/usr/bin/env bash
exit 2
REPRO
chmod +x "$SH/repro-fault-bo2.sh"

base_sha39="$(git -C "$REPO" rev-parse origin/main)"
bwt39="$RUN/worktree/sp-vd-bo2"
git -C "$REPO" worktree add -q -b "spira/sp-vd-bo2" "$bwt39" origin/main 2>/dev/null || true
mkdir -p "$bwt39/spira"
printf 'x\n' > "$bwt39/spira/test-owned.sh"
git -C "$bwt39" add -A
git -C "$bwt39" commit -q -m "sp-vd-bo2: work"
tip_bo2="$(git -C "$REPO" rev-parse "spira/sp-vd-bo2")"
printf 'BATCHED %s %s\n' "$tip_bo2" "$(date +%s)" > "$LANDSTATE/sp-vd-bo2"
{ printf 'pr=94\nhead=%s\nbase=%s\nmembers=sp-vd-bo2:%s\nopened=%s\n' \
    "$tip_bo2" "$base_sha39" "$tip_bo2" "$(date +%s)"; } > "$(batch_file)"
printf 'red\nred-suite: test-owned.sh\nrun-url: https://example.invalid/actions/runs/9005\n' \
    > "$FORGE_STATUS_FILE"

SPIRA_QUEUE_REPRO_BATCH="$SH/repro-fault-bo2.sh" verdict "$REPONAME" > /dev/null
is     "39. legacy-owned: judgement-ci never called" "0" "$(wc -l < "$BATCHER_LOG")"
case "$(landstate sp-vd-bo2)" in EJECTED*) ok "39. legacy-owned: member ejected by verdict's own attribution" ;;
    *) bad "39. legacy-owned: member ejected by verdict's own attribution" "got: $(landstate sp-vd-bo2)" ;; esac
unset SPIRA_BATCHER_BIN
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 40. TWO RED SUITES, TWO MEMBERS, DIFF-EVIDENCE ONLY (sp-2gcls). The repro
#     stub faults (harness fault) for every call, so neither suite is ever
#     actually run against anyone — both members are ejected on diff evidence
#     alone: em4a's diff adds test-vd-em4a.sh, em4b's diff adds the unrelated
#     test-vd-em4b.sh. Each ejection note must name only the suite that
#     member's OWN diff touched, not the other member's, and must not claim
#     "Reproduced alone" — nothing was reproduced for either.
#     Against the unfixed verdict.sh, both notes name BOTH suites (the
#     batch-wide list), exactly the sp-kn0hw defect: a suite the member never
#     touched, and that need not even exist in its tree, shows up as if it
#     had been tested.
# =============================================================================
testdb_seed <<JSONL
{"id":"sp-vd-em4a","title":"add and break the first canary suite","status":"open","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"description":"Adds test-vd-em4a.sh, red from the start."}
{"id":"sp-vd-em4b","title":"touch an unrelated second canary suite","status":"open","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"description":"Adds test-vd-em4b.sh, unrelated to em4a's failure."}
JSONL

cat > "$SH/repro-fault4.sh" <<'REPRO'
#!/usr/bin/env bash
exit 2
REPRO
chmod +x "$SH/repro-fault4.sh"

base_sha40="$(git -C "$REPO" rev-parse origin/main)"
bwt40a="$RUN/worktree/sp-vd-em4a"
git -C "$REPO" worktree add -q -b "spira/sp-vd-em4a" "$bwt40a" origin/main 2>/dev/null || true
printf 'x\n' > "$bwt40a/test-vd-em4a.sh"
git -C "$bwt40a" add -A
git -C "$bwt40a" commit -q -m "sp-vd-em4a: work"
bwt40b="$RUN/worktree/sp-vd-em4b"
git -C "$REPO" worktree add -q -b "spira/sp-vd-em4b" "$bwt40b" origin/main 2>/dev/null || true
printf 'x\n' > "$bwt40b/test-vd-em4b.sh"
git -C "$bwt40b" add -A
git -C "$bwt40b" commit -q -m "sp-vd-em4b: work"
for id in sp-vd-em4a sp-vd-em4b; do
    printf 'BATCHED %s %s\n' "$(git -C "$REPO" rev-parse "spira/$id")" "$(date +%s)" \
        > "$LANDSTATE/$id"
done
tip_em4a="$(git -C "$REPO" rev-parse "spira/sp-vd-em4a")"
tip_em4b="$(git -C "$REPO" rev-parse "spira/sp-vd-em4b")"
wt40="$RUN/worktree/.b40"
git -C "$REPO" worktree add -q --detach "$wt40" "$base_sha40" 2>/dev/null || true
git -C "$wt40" merge -q --no-edit --no-ff -m "spira: land sp-vd-em4a" "$tip_em4a" >/dev/null 2>&1
git -C "$wt40" merge -q --no-edit --no-ff -m "spira: land sp-vd-em4b" "$tip_em4b" >/dev/null 2>&1
batch_head40="$(git -C "$wt40" rev-parse HEAD)"
git -C "$REPO" worktree remove -f "$wt40" 2>/dev/null || true
{ printf 'pr=94\nhead=%s\nbase=%s\nmembers=sp-vd-em4a:%s sp-vd-em4b:%s\nopened=%s\n' \
    "$batch_head40" "$base_sha40" "$tip_em4a" "$tip_em4b" "$(date +%s)"; } > "$(batch_file)"
printf 'red\nred-suite: test-vd-em4a.sh\nred-suite: test-vd-em4b.sh\nrun-url: https://example.invalid/actions/runs/9004\n' \
    > "$FORGE_STATUS_FILE"
SPIRA_QUEUE_REPRO_BATCH="$SH/repro-fault4.sh" verdict "$REPONAME" > /dev/null

awk '/^send operator/{n++} {print > ("'"$TMP"'/mailblock40." n)}' "$MAIL_LOG"
mail40a="$(cat "$TMP/mailblock40.1" 2>/dev/null)"
mail40b="$(cat "$TMP/mailblock40.2" 2>/dev/null)"

case "$(landstate sp-vd-em4a)" in EJECTED*) ok "40. diff-evidence: sp-vd-em4a ejected" ;;
    *) bad "40. diff-evidence: sp-vd-em4a ejected" "got: $(landstate sp-vd-em4a)" ;; esac
case "$(landstate sp-vd-em4b)" in EJECTED*) ok "40. diff-evidence: sp-vd-em4b ejected" ;;
    *) bad "40. diff-evidence: sp-vd-em4b ejected" "got: $(landstate sp-vd-em4b)" ;; esac
want   "40. em4a note: names its own suite"          "test-vd-em4a.sh"   "$mail40a"
nowant "40. em4a note: silent on em4b's suite"       "test-vd-em4b.sh"   "$mail40a"
nowant "40. em4a note: no false reproduction claim"  "Reproduced alone"  "$mail40a"
want   "40. em4b note: names its own suite"          "test-vd-em4b.sh"   "$mail40b"
nowant "40. em4b note: silent on em4a's suite"       "test-vd-em4a.sh"   "$mail40b"
nowant "40. em4b note: no false reproduction claim"  "Reproduced alone"  "$mail40b"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 41. BASELINE-RED SUITE EXCLUDED FROM BLAME (sp-a2nk8). CI names two red
#     suites: one is already red on the batch's own base (predates every
#     member — every member's tree contains it), the other is a genuine
#     member-caused failure. Attribution must not blame anyone for the
#     base-broken suite; the real offender is still ejected, and only for the
#     suite it actually broke.
#     POSITIVE CONTROL: the stub reports RED/ok per named suite, so a matcher
#     that ignored the base-only check would still see both suites red
#     against the guilty member and could not be told apart by accident.
# =============================================================================
cat > "$SH/repro-baseline.sh" <<'REPRO'
#!/usr/bin/env bash
# Simulates testenv-batch.sh's own per-suite RED/ok reporting.
# Args: --mode <parallel|serial> --suites <csv> <ref>
shift 2
shift
csv="$1"; shift
ref="$1"
rc=0
IFS=',' read -ra suites <<< "$csv"
for s in "${suites[@]}"; do
    red=0
    case "$s" in
        test-base-broken.sh) red=1 ;;
        test-real-offender.sh)
            git -C "$SPIRA_REPO" ls-tree "$ref" -- guilty-marker.txt 2>/dev/null | grep -q . && red=1
            ;;
    esac
    if [ "$red" = 1 ]; then
        printf '  %-32s RED     rc=1 after 1s\n' "$s"
        rc=1
    else
        printf '  %-32s ok      1s\n' "$s"
    fi
done
exit "$rc"
REPRO
chmod +x "$SH/repro-baseline.sh"

base_sha41="$(git -C "$REPO" rev-parse origin/main)"
for id in sp-vd-bl1 sp-vd-bl2; do
    bwt41="$RUN/worktree/$id"
    git -C "$REPO" worktree add -q -b "spira/$id" "$bwt41" origin/main 2>/dev/null || true
    printf '%s\n' "$id" > "$bwt41/$id.txt"
done
printf 'offender\n' > "$RUN/worktree/sp-vd-bl2/guilty-marker.txt"
for id in sp-vd-bl1 sp-vd-bl2; do
    bwt41="$RUN/worktree/$id"
    git -C "$bwt41" add -A
    git -C "$bwt41" commit -q -m "$id: work"
    printf 'BATCHED %s %s\n' "$(git -C "$REPO" rev-parse "spira/$id")" "$(date +%s)" \
        > "$LANDSTATE/$id"
done
tip_bl1="$(git -C "$REPO" rev-parse "spira/sp-vd-bl1")"
tip_bl2="$(git -C "$REPO" rev-parse "spira/sp-vd-bl2")"
wt41="$RUN/worktree/.b41"
git -C "$REPO" worktree add -q --detach "$wt41" "$base_sha41" 2>/dev/null || true
git -C "$wt41" merge -q --no-edit --no-ff -m "spira: land sp-vd-bl1" "$tip_bl1" >/dev/null 2>&1
git -C "$wt41" merge -q --no-edit --no-ff -m "spira: land sp-vd-bl2" "$tip_bl2" >/dev/null 2>&1
batch_head41="$(git -C "$wt41" rev-parse HEAD)"
git -C "$REPO" worktree remove -f "$wt41" 2>/dev/null || true
{ printf 'pr=61\nhead=%s\nbase=%s\nmembers=sp-vd-bl1:%s sp-vd-bl2:%s\nopened=%s\n' \
    "$batch_head41" "$base_sha41" "$tip_bl1" "$tip_bl2" "$(date +%s)"; } > "$(batch_file)"
printf 'red\nred-suite: test-base-broken.sh\nred-suite: test-real-offender.sh\n' > "$FORGE_STATUS_FILE"
SPIRA_QUEUE_REPRO_BATCH="$SH/repro-baseline.sh" verdict "$REPONAME" > /dev/null
mail41="$(cat "$MAIL_LOG")"

case "$(landstate sp-vd-bl2)" in EJECTED*) ok "41. baseline-red: guilty ejected" ;;
    *) bad "41. baseline-red: guilty ejected" "got: $(landstate sp-vd-bl2)" ;; esac
case "$(landstate sp-vd-bl1)" in CERTIFIED*) ok "41. baseline-red: innocent CERTIFIED" ;;
    *) bad "41. baseline-red: innocent CERTIFIED" "got: $(landstate sp-vd-bl1)" ;; esac
want   "41. baseline-red: excludes base-broken suite from blame" \
       "Excluded from blame" "$mail41"
want   "41. baseline-red: names the excluded suite" \
       "test-base-broken.sh" "$mail41"
nowant "41. baseline-red: guilty member not blamed for the base suite" \
       "spira/sp-vd-bl2 (test-base-broken.sh" "$mail41"
want   "41. baseline-red: names the real offender's suite" \
       "spira/sp-vd-bl2 (test-real-offender.sh" "$mail41"
attrlog41="$(cat "$RUN/attribution/results.jsonl" 2>/dev/null)"
want "41. baseline-red: machine-readable log excludes the base-broken suite" \
     '"suite":"test-base-broken.sh","tier":"","case":"(attribution)","status":"excluded-baseline"' "$attrlog41"
want "41. baseline-red: machine-readable log attributes the real offender's suite" \
     '"suite":"test-real-offender.sh","tier":"","case":"(attribution)","status":"attributed"' "$attrlog41"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 42. EVERY RED SUITE ALREADY RED ON BASE — NOTHING ATTRIBUTABLE (sp-a2nk8).
#     CI names exactly one red suite and it is already red on the batch's own
#     base. Before the fix this reproduced against every member (every
#     member's tree contains the base) and, worst case, ejected the whole
#     batch for a breakage none of them own. Now: nobody is ejected, the
#     batch requeues unchanged, and the operator is told why.
#     POSITIVE CONTROL: case 41 proves a genuine offender still gets ejected
#     when one exists; this proves nobody is invented when one does not.
# =============================================================================
base_sha42="$(git -C "$REPO" rev-parse origin/main)"
for id in sp-vd-bz1 sp-vd-bz2; do
    bwt42="$RUN/worktree/$id"
    git -C "$REPO" worktree add -q -b "spira/$id" "$bwt42" origin/main 2>/dev/null || true
    printf '%s\n' "$id" > "$bwt42/$id.txt"
    git -C "$bwt42" add -A
    git -C "$bwt42" commit -q -m "$id: work"
    printf 'BATCHED %s %s\n' "$(git -C "$REPO" rev-parse "spira/$id")" "$(date +%s)" \
        > "$LANDSTATE/$id"
done
tip_bz1="$(git -C "$REPO" rev-parse "spira/sp-vd-bz1")"
tip_bz2="$(git -C "$REPO" rev-parse "spira/sp-vd-bz2")"
wt42="$RUN/worktree/.b42"
git -C "$REPO" worktree add -q --detach "$wt42" "$base_sha42" 2>/dev/null || true
git -C "$wt42" merge -q --no-edit --no-ff -m "spira: land sp-vd-bz1" "$tip_bz1" >/dev/null 2>&1
git -C "$wt42" merge -q --no-edit --no-ff -m "spira: land sp-vd-bz2" "$tip_bz2" >/dev/null 2>&1
batch_head42="$(git -C "$wt42" rev-parse HEAD)"
git -C "$REPO" worktree remove -f "$wt42" 2>/dev/null || true
{ printf 'pr=62\nhead=%s\nbase=%s\nmembers=sp-vd-bz1:%s sp-vd-bz2:%s\nopened=%s\n' \
    "$batch_head42" "$base_sha42" "$tip_bz1" "$tip_bz2" "$(date +%s)"; } > "$(batch_file)"
printf 'red\nred-suite: test-base-broken.sh\n' > "$FORGE_STATUS_FILE"
out42="$(SPIRA_QUEUE_REPRO_BATCH="$SH/repro-baseline.sh" verdict "$REPONAME")"
mail42="$(cat "$MAIL_LOG")"

case "$(landstate sp-vd-bz1)" in CERTIFIED*) ok "42. all-baseline-red: sp-vd-bz1 CERTIFIED" ;;
    *) bad "42. all-baseline-red: sp-vd-bz1 CERTIFIED" "got: $(landstate sp-vd-bz1)" ;; esac
case "$(landstate sp-vd-bz2)" in CERTIFIED*) ok "42. all-baseline-red: sp-vd-bz2 CERTIFIED" ;;
    *) bad "42. all-baseline-red: sp-vd-bz2 CERTIFIED" "got: $(landstate sp-vd-bz2)" ;; esac
nowant "42. all-baseline-red: no ejection"          "ejected"                 "$out42"
want   "42. all-baseline-red: not attributable in log" "not attributable"    "$out42"
want   "42. all-baseline-red: pr-close called"      "close"                   "$(cat "$FORGE_LOG")"
want   "42. all-baseline-red: operator told why"    "Red on base, not attributable" "$mail42"
want   "42. all-baseline-red: names the suite"      "test-base-broken.sh"     "$mail42"
attrlog42="$(cat "$RUN/attribution/results.jsonl" 2>/dev/null)"
want "42. all-baseline-red: machine-readable log excludes the suite, never silent" \
     '"suite":"test-base-broken.sh","tier":"","case":"(attribution)","status":"excluded-baseline"' "$attrlog42"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

# =============================================================================
# 43. A RED SUITE THE ATTRIBUTION PASS COULD NOT PIN ON ANY MEMBER MUST LOG THAT
#     OUTCOME EXPLICITLY, NEVER BY ABSENCE (sp-yivi7 archivist note; batch PR 297,
#     2026-09-24). Two red suites, two members: one suite is directly in one
#     member's diff (attributed via suite-overlap) and repro never reproduces
#     anyone (always green), so the diff-overlap eject happens in phase 1 and the
#     OTHER suite — selected by no one's diff — is never phase-2/3 tested. Before
#     this bead that second suite got no verdict logged anywhere; now it must show
#     up as "unattributed", not be missing.
# =============================================================================
cat > "$SH/repro-never-red.sh" <<'REPRO'
#!/usr/bin/env bash
exit 0
REPRO
chmod +x "$SH/repro-never-red.sh"

base_sha43="$(git -C "$REPO" rev-parse origin/main)"
for id in sp-vd-g1 sp-vd-g2; do
    bwt43="$RUN/worktree/$id"
    git -C "$REPO" worktree add -q -b "spira/$id" "$bwt43" origin/main 2>/dev/null || true
    printf '%s\n' "$id" > "$bwt43/$id.txt"
done
printf 'marker\n' > "$RUN/worktree/sp-vd-g1/test-diffmarked-suite.sh"
for id in sp-vd-g1 sp-vd-g2; do
    bwt43="$RUN/worktree/$id"
    git -C "$bwt43" add -A
    git -C "$bwt43" commit -q -m "$id: work"
    printf 'BATCHED %s %s\n' "$(git -C "$REPO" rev-parse "spira/$id")" "$(date +%s)" \
        > "$LANDSTATE/$id"
done
tip_g1="$(git -C "$REPO" rev-parse "spira/sp-vd-g1")"
tip_g2="$(git -C "$REPO" rev-parse "spira/sp-vd-g2")"
wt43="$RUN/worktree/.b43"
git -C "$REPO" worktree add -q --detach "$wt43" "$base_sha43" 2>/dev/null || true
git -C "$wt43" merge -q --no-edit --no-ff -m "spira: land sp-vd-g1" "$tip_g1" >/dev/null 2>&1
git -C "$wt43" merge -q --no-edit --no-ff -m "spira: land sp-vd-g2" "$tip_g2" >/dev/null 2>&1
batch_head43="$(git -C "$wt43" rev-parse HEAD)"
git -C "$REPO" worktree remove -f "$wt43" 2>/dev/null || true
{ printf 'pr=63\nhead=%s\nbase=%s\nmembers=sp-vd-g1:%s sp-vd-g2:%s\nopened=%s\n' \
    "$batch_head43" "$base_sha43" "$tip_g1" "$tip_g2" "$(date +%s)"; } > "$(batch_file)"
printf 'red\nred-suite: test-diffmarked-suite.sh\nred-suite: test-silent-suite.sh\n' > "$FORGE_STATUS_FILE"
SPIRA_QUEUE_REPRO_BATCH="$SH/repro-never-red.sh" verdict "$REPONAME" > /dev/null

case "$(landstate sp-vd-g1)" in EJECTED*) ok "43. silent-suite: diff-marked member ejected" ;;
    *) bad "43. silent-suite: diff-marked member ejected" "got: $(landstate sp-vd-g1)" ;; esac
attrlog43="$(cat "$RUN/attribution/results.jsonl" 2>/dev/null)"
want "43. silent-suite: attributed suite logged" \
     '"suite":"test-diffmarked-suite.sh","tier":"","case":"(attribution)","status":"attributed"' "$attrlog43"
want "43. silent-suite: unpinned suite logged explicitly as unattributed, not absent" \
     '"suite":"test-silent-suite.sh","tier":"","case":"(attribution)","status":"unattributed"' "$attrlog43"
clean_case
git -C "$REPO" fetch -q origin 2>/dev/null || true

tl_summary
