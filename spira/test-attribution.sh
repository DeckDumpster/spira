#!/usr/bin/env bash
#
# test-attribution.sh — merge-queue attribution: eject reproducers, halve
# together-only reds, quarantine flaky suites, eject on second unreproduced red,
# and verify the local-gate meter.
#
# Six cases (nineteen assertions):
#   1. Three members with one planted breaker → that member ejected, two survivors
#      returned to CERTIFIED, PR closed, batch removed.
#   2. Two members that break only together → batch halved (first half
#      gets epoch=1, second waits); PR closed, batch removed.
#   3. A planted flake (no member reproduces, batch head also green) →
#      suites.sh observe-flake called, all members requeued, batch removed.
#   4. Single member, second unreproduced red on same tip → ejected.
#   5. Meter: CAUGHT written by queue.sh submit failure; ESCAPED written by
#      attribution; queue.sh stats reports both correctly.
#   6. Diff-based attribution ejects the member whose change touches the red suite.
#   7. ...and reads only the member's own change: a member forked before the base
#      advanced over the red suite is not blamed for the advance.
#   8. Per-member filtering: guilty (covered diff + repro) ejected; innocent
#      (inert diff + in REPRO_FAIL_FILE) skipped by selection, NOT ejected.
#   9. New-base suite: a suite that exists only on the advanced base is still
#      reproduced when a single member breaks it.
#  10. Whole-tree unselected: a member whose diff covers no file declared by the
#      red suite is still ejected when that suite fails against its merged tree.
#
# The repro batch is stubbed via SPIRA_QUEUE_REPRO_BATCH; no container is used.
# The forge is a local fixture; no network is reached.
#
# covers: spira/verdict.sh spira/queue.sh spira/forge.sh spira/conf.sh spira/batch.sh
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
testdb_require test-attribution
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up attribution || { echo "test-attribution: could not build fixture database"; exit 1; }
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

# ─── Shared log paths ─────────────────────────────────────────────────────────
FORGE_LOG="$TMP/forge-log"
FORGE_STATUS_FILE="$TMP/forge-status"
SUITES_LOG="$TMP/suites-log"
REPRO_FAIL_FILE="$TMP/repro-fail"  # space-separated branch names that repro as red
export FORGE_LOG FORGE_STATUS_FILE SUITES_LOG REPRO_FAIL_FILE

printf 'red\n' > "$FORGE_STATUS_FILE"
: > "$FORGE_LOG"
: > "$SUITES_LOG"
: > "$REPRO_FAIL_FILE"

# ─── Forge fixture ────────────────────────────────────────────────────────────
# Returns check-status from FORGE_STATUS_FILE; records pr-close to FORGE_LOG.
cat > "$SH/forge-fixture.sh" <<'FORGE'
#!/usr/bin/env bash
cmd="${1:-}"; shift; repo="${1:-}"; shift
case "$cmd" in
    check-status) cat "${FORGE_STATUS_FILE}" 2>/dev/null || printf 'pending\n' ;;
    run-id)       printf 'run-99\n' ;;
    pr-close)     printf '%s\tclose\n' "${1:-}" >> "$FORGE_LOG" ;;
    workflow-rerun) printf '%s\trerun\n' "${1:-}" >> "$FORGE_LOG" ;;
    pr-create)
        n=$(( $(wc -l < "$FORGE_LOG" 2>/dev/null || echo 0) + 1 ))
        printf '%s\n' "$n"
        ;;
    *) printf 'forge-fixture: unknown: %s\n' "$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"

# ─── Repro batch stub ─────────────────────────────────────────────────────────
# Returns exit 1 (red) if any REPRO_FAIL_FILE entry matches the branch argument
# exactly or is a git ancestor of it (handles merged-SHA callers from _repro_is_red).
cat > "$SH/repro-stub.sh" <<'REPRO'
#!/usr/bin/env bash
br=""
while [ $# -gt 0 ]; do
    case "$1" in --mode|--suites) shift 2 ;; *) br="$1"; shift ;; esac
done
fail_list="$(cat "${REPRO_FAIL_FILE}" 2>/dev/null || true)"
for f in $fail_list; do
    if [ "$f" = "$br" ]; then
        [ -n "${REPRO_FAIL_LINE:-}" ] && printf '%s\n' "$REPRO_FAIL_LINE"
        exit 1
    fi
    [ -n "${SPIRA_REPO:-}" ] || continue
    if git -C "$SPIRA_REPO" merge-base --is-ancestor "$f" "$br" 2>/dev/null; then
        [ -n "${REPRO_FAIL_LINE:-}" ] && printf '%s\n' "$REPRO_FAIL_LINE"
        exit 1
    fi
done
exit 0
REPRO
chmod +x "$SH/repro-stub.sh"

# ─── suites.sh stub ───────────────────────────────────────────────────────────
cat > "$SH/suites.sh" <<'SUITES'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$SUITES_LOG"
SUITES
chmod +x "$SH/suites.sh"

# ─── mail.sh stub ─────────────────────────────────────────────────────────────
cat > "$SH/mail.sh" <<'MAIL'
#!/usr/bin/env bash
exit 0
MAIL
chmod +x "$SH/mail.sh"

# ─── Helpers ──────────────────────────────────────────────────────────────────

verdict() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_CI_MAXSEC=3600 \
    SPIRA_QUEUE_INFRA_RETRIES=2 \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
    SPIRA_QUEUE_REPRO_BATCH="$SH/repro-stub.sh" \
        bash "$SH/verdict.sh" "$@" 2>&1
}

notes_of() {
    "${TESTDB_BD:-bd}" -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' \
        | python3 -c 'import sys,json; d=json.load(sys.stdin); print((d[0].get("notes","") or ""))' 2>/dev/null
}

batch_file() { printf '%s/%s/open' "$QUEUEDIR" "$REPONAME"; }
landstate()  { cat "$LANDSTATE/${1:-}" 2>/dev/null; }
land_state_of() { awk '{print $1}' "$LANDSTATE/${1:-}" 2>/dev/null; }

# plant_bead <id>
plant_bead() {
    printf \
        '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-16T00:00:00Z","closed_at":"2026-09-16T00:00:00Z","dependencies":[]}\n' \
        "$1" "$1" | testdb_seed
}

# build_members id1 id2 ... — create member branches and write open batch record.
# Returns batch_head on stdout.
build_members() {
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
    local batch_br="spira/queue/test-$$"
    git -C "$REPO" branch -f "$batch_br" "$batch_head" 2>/dev/null || true
    git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true

    {
        printf 'pr=42\n'
        printf 'head=%s\n' "$batch_head"
        printf 'base=%s\n' "$base_sha"
        printf 'members=%s\n' "${members[*]}"
        printf 'opened=%s\n' "$(date +%s)"
        printf 'branch=%s\n' "$batch_br"
    } > "$(batch_file)"

    printf '%s\n' "$batch_head"
}

clean_case() {
    rm -f "$(batch_file)"
    : > "$FORGE_LOG"
    : > "$SUITES_LOG"
    : > "$REPRO_FAIL_FILE"
    printf 'red\nred-suite: test-canary.sh\n' > "$FORGE_STATUS_FILE"
    find "$LANDSTATE" -maxdepth 1 -type f -delete 2>/dev/null || true
    rm -rf "$QUEUEDIR/$REPONAME/unreproduced" 2>/dev/null || true
    rm -rf "$RUN/landing.log" 2>/dev/null || true
    local wt
    for wt in "$RUN/worktree"/*; do
        [ -d "$wt" ] || continue
        git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    done
    rm -rf "$RUN/worktree" && mkdir -p "$RUN/worktree"
    git -C "$REPO" worktree prune 2>/dev/null || true
    git -C "$REPO" for-each-ref --format='%(refname:short)' 'refs/heads/spira/*' 2>/dev/null \
        | while read -r br; do git -C "$REPO" branch -D "$br" 2>/dev/null || true; done
    testdb_reset
}

# make_branch <id> <file> — create branch spira/<id> with one specific file.
make_branch() {
    local id="$1" file="$2"
    local bwt="$RUN/worktree/$id"
    rm -rf "$bwt"
    git -C "$REPO" worktree add -q -b "spira/$id" "$bwt" origin/main 2>/dev/null || true
    mkdir -p "$bwt/$(dirname "$file")"
    printf '%s\n' "$id" > "$bwt/$file"
    git -C "$bwt" add -A
    git -C "$bwt" commit -q -m "$id: work"
}

# build_batch <id1> <id2> ... — merge pre-created branches and write open batch record.
build_batch() {
    local base_sha; base_sha="$(git -C "$REPO" rev-parse origin/main)"
    local wt="$RUN/worktree/.batch-build"
    git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    git -C "$REPO" worktree add -q --detach "$wt" "$base_sha"
    local members=() id tip
    for id in "$@"; do
        tip="$(git -C "$REPO" rev-parse "spira/$id")"
        git -C "$wt" merge -q --no-edit --no-ff -m "spira: land $id" "$tip" >/dev/null 2>&1
        members+=("$id:$tip")
        printf 'BATCHED %s %s\n' "$tip" "$(date +%s)" > "$LANDSTATE/$id"
    done
    local batch_head; batch_head="$(git -C "$wt" rev-parse HEAD)"
    local batch_br="spira/queue/test-$$"
    git -C "$REPO" branch -f "$batch_br" "$batch_head" 2>/dev/null || true
    git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    {
        printf 'pr=42\n'
        printf 'head=%s\n' "$batch_head"
        printf 'base=%s\n' "$base_sha"
        printf 'members=%s\n' "${members[*]}"
        printf 'opened=%s\n' "$(date +%s)"
        printf 'branch=%s\n' "$batch_br"
    } > "$(batch_file)"
    printf '%s\n' "$batch_head"
}

echo "test-attribution.sh"

# Seed the status file default for all red cases.
printf 'red\nred-suite: test-canary.sh\n' > "$FORGE_STATUS_FILE"

# =============================================================================
# POSITIVE CONTROL: repro stub is actually reached. Without this, "no ejection"
# passes just as well against a verdict that errors before calling the stub.
# =============================================================================
testdb_reset
make_branch sp-at-ctrl spira/canary.sh
build_batch sp-at-ctrl > /dev/null
plant_bead sp-at-ctrl
printf 'spira/sp-at-ctrl\n' > "$REPRO_FAIL_FILE"
verdict "$REPONAME" > /dev/null
is "positive-control: ejected member lands in EJECTED state" "EJECTED" "$(land_state_of sp-at-ctrl)"
clean_case

# =============================================================================
# 1. THREE MEMBERS, ONE BREAKER — ejects the breaker; survivors re-pushed to
#    the SAME PR branch so CI re-runs without a new local gate pass.
#
#    POSITIVE CONTROL: the test is seen to fail on the current verdict.sh,
#    which returns survivors to CERTIFIED and closes the PR. The new assertions
#    require BATCHED state, a resealed batch file, and the remote branch updated.
# =============================================================================
testdb_reset
make_branch sp-at-a sp-at-a.txt
make_branch sp-at-b spira/canary.sh
make_branch sp-at-c sp-at-c.txt
build_batch sp-at-a sp-at-b sp-at-c > /dev/null
for id in sp-at-a sp-at-b sp-at-c; do plant_bead "$id"; done
_old_head1="$(grep '^head=' "$(batch_file)" | cut -d= -f2)"
_batch_br1="$(grep '^branch=' "$(batch_file)" | cut -d= -f2)"
# Only sp-at-b reproduces the red.
printf 'spira/sp-at-b\n' > "$REPRO_FAIL_FILE"
out="$(verdict "$REPONAME")"
is   "1. breaker: sp-at-b ejected"               "EJECTED"  "$(land_state_of sp-at-b)"
is   "1. breaker: sp-at-a stays BATCHED"          "BATCHED"  "$(land_state_of sp-at-a)"
is   "1. breaker: sp-at-c stays BATCHED"          "BATCHED"  "$(land_state_of sp-at-c)"
is   "1. breaker: same PR number"                 "42"       "$(grep '^pr=' "$(batch_file)" | cut -d= -f2)"
is   "1. breaker: batch record kept"              "1"        "$([ -f "$(batch_file)" ] && echo 1 || echo 0)"
nowant "1. breaker: PR not closed"                "close"    "$(cat "$FORGE_LOG")"
want "1. breaker: re-push reported"               "re-pushed to same PR" "$out"
_new_head1="$(grep '^head=' "$(batch_file)" | cut -d= -f2)"
_remote_head1="$(git -C "$REMOTE" rev-parse "$_batch_br1" 2>/dev/null || echo none)"
is   "1. breaker: head resealed"                  "$_new_head1" "$_remote_head1"
[ "${_new_head1:-}" != "${_old_head1:-}" ] \
    && ok "1. breaker: new head differs from old" \
    || bad "1. breaker: new head differs from old" "head unchanged: ${_new_head1:-}"
clean_case

# =============================================================================
# 2. TWO MEMBERS, TOGETHER-ONLY RED — batch halves; neither member ejected.
#    First half (sp-at-d) gets epoch=1; second half (sp-at-e) gets epoch=now.
# =============================================================================
testdb_reset
build_members sp-at-d sp-at-e > /dev/null
for id in sp-at-d sp-at-e; do plant_bead "$id"; done
# Neither individual branch reproduces; the batch branch does.
# REPRO_FAIL_FILE lists the batch branch (stored as spira/queue/test-*).
# The repro stub sees the branch name passed to testenv-batch.sh. For the
# batch head test, verdict.sh passes branch_name (the batch branch).
# We need the batch branch name to plant the failure.
batch_br="$(grep '^branch=' "$(batch_file)" | cut -d= -f2)"
printf '%s\n' "$batch_br" > "$REPRO_FAIL_FILE"
out="$(verdict "$REPONAME")"
is   "2. halve: sp-at-d is CERTIFIED"     "CERTIFIED"  "$(land_state_of sp-at-d)"
is   "2. halve: sp-at-e is CERTIFIED"     "CERTIFIED"  "$(land_state_of sp-at-e)"
is   "2. halve: batch record removed"     "0"          "$([ -f "$(batch_file)" ] && echo 1 || echo 0)"
# First half (sp-at-d) should have epoch=1 (old); second half should have epoch=now (>1).
d_epoch="$(awk '{print $3}' "$LANDSTATE/sp-at-d" 2>/dev/null || echo 0)"
e_epoch="$(awk '{print $3}' "$LANDSTATE/sp-at-e" 2>/dev/null || echo 0)"
is   "2. halve: first half has epoch=1"   "1"          "$d_epoch"
[ "${e_epoch:-0}" -gt 1 ] && ok "2. halve: second half has epoch>1" \
    || bad "2. halve: second half epoch should be >1" "got $e_epoch"
want "2. halve: halved reported"          "together-only red" "$out"
clean_case

# =============================================================================
# 3. UNREPRODUCED RED — no member reproduces, batch head also green; suite came
#    on a red-suite: line so it is ineligible for observe-flake; members requeued.
# =============================================================================
testdb_reset
build_members sp-at-f sp-at-g > /dev/null
for id in sp-at-f sp-at-g; do plant_bead "$id"; done
# Nobody reproduces (REPRO_FAIL_FILE is empty).
out="$(verdict "$REPONAME")"
is     "3. unrep-red: sp-at-f returned CERTIFIED" "CERTIFIED"  "$(land_state_of sp-at-f)"
is     "3. unrep-red: sp-at-g returned CERTIFIED" "CERTIFIED"  "$(land_state_of sp-at-g)"
is     "3. unrep-red: batch record removed"       "0"          "$([ -f "$(batch_file)" ] && echo 1 || echo 0)"
nowant "3. unrep-red: red-suite not quarantined"  "observe-flake" "$(cat "$SUITES_LOG")"
want   "3. unrep-red: reported as requeued"       "requeued"   "$out"
clean_case

# =============================================================================
# 4. SINGLE MEMBER, SECOND UNREPRODUCED RED — first time requeues; second time
#    (same tip) ejects.
# =============================================================================
testdb_reset
build_members sp-at-h > /dev/null
plant_bead sp-at-h
# First verdict: does not reproduce.
out1="$(verdict "$REPONAME")"
is     "4a. unrep-1: not ejected"             "CERTIFIED"  "$(land_state_of sp-at-h)"
nowant "4a. unrep-1: red-suite not quarantined" "observe-flake" "$(cat "$SUITES_LOG")"
is     "4a. unrep-1: batch removed"           "0"          "$([ -f "$(batch_file)" ] && echo 1 || echo 0)"

# Rebuild the batch for the SAME member at the SAME tip.
tip="$(awk '{print $2}' "$LANDSTATE/sp-at-h" 2>/dev/null)"
local_tip="$(git -C "$REPO" rev-parse "spira/sp-at-h" 2>/dev/null)"
is   "4a. unrep-1: tip unchanged"      "$local_tip" "$tip"

# Rebuild batch record (member still exists, same tip).
base_sha="$(git -C "$REPO" rev-parse origin/main)"
batch_head="$(git -C "$REPO" rev-parse "spira/sp-at-h")"
batch_br="spira/queue/test2-$$"
git -C "$REPO" branch -f "$batch_br" "$batch_head" 2>/dev/null || true
{
    printf 'pr=43\n'
    printf 'head=%s\n' "$batch_head"
    printf 'base=%s\n' "$base_sha"
    printf 'members=%s:%s\n' "sp-at-h" "$tip"
    printf 'opened=%s\n' "$(date +%s)"
    printf 'branch=%s\n' "$batch_br"
} > "$(batch_file)"
printf 'BATCHED %s %s\n' "$tip" "$(date +%s)" > "$LANDSTATE/sp-at-h"

# Second verdict with same tip: still does not reproduce → ejected.
out2="$(verdict "$REPONAME")"
is   "4b. unrep-2: ejected on second"  "EJECTED"    "$(land_state_of sp-at-h)"
is   "4b. unrep-2: batch removed"      "0"          "$([ -f "$(batch_file)" ] && echo 1 || echo 0)"
want "4b. unrep-2: reported ejected"   "ejected"    "$out2"
clean_case

# =============================================================================
# 5. METER — CAUGHT written directly (batch.sh attribution now produces it);
#    ESCAPED written by verdict attribution; queue.sh stats reports both.
# =============================================================================
testdb_reset
# Write a CAUGHT line directly — testing that stats reads it, not how it is written.
printf 'QUEUE CAUGHT %s branch=sp-at-caught\n' "$(date +%s)" \
    >> "$RUN/landing.log" 2>/dev/null

# Run a verdict with a member that reproduces (ESCAPED).
build_members sp-at-meter > /dev/null
plant_bead sp-at-meter
printf 'spira/sp-at-meter\n' > "$REPRO_FAIL_FILE"
verdict "$REPONAME" > /dev/null

# Check stats.
stats_out="$(SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_REPO_MAP="$SH/repo-map" \
    bash "$SH/queue.sh" stats 2>&1)"
want "5. meter: caught=1 in stats"    "caught:          1" "$stats_out"
want "5. meter: escaped=1 in stats"   "escaped:         1" "$stats_out"
clean_case

# =============================================================================
# 6. DIFF-BASED ATTRIBUTION — repro unavailable but one member's diff contains
#    the red suite; that member is ejected, the survivor re-pushed to same PR.
#    Simulates container failure: repro stub returns green for all branches.
# =============================================================================
testdb_reset
base_sha_6="$(git -C "$REPO" rev-parse origin/main)"
wt_build6="$RUN/worktree/.batch-build-6"
git -C "$REPO" worktree add -q --detach "$wt_build6" "$base_sha_6"

wt_guilty="$RUN/worktree/sp-at-dg"
wt_clean="$RUN/worktree/sp-at-dc"
git -C "$REPO" worktree add -q -b "spira/sp-at-dg" "$wt_guilty" origin/main
git -C "$REPO" worktree add -q -b "spira/sp-at-dc" "$wt_clean" origin/main

# sp-at-dg adds the red suite file to its branch (test-canary.sh).
printf 'modified\n' > "$wt_guilty/test-canary.sh"
git -C "$wt_guilty" add -A && git -C "$wt_guilty" commit -q -m "sp-at-dg: work"
tip_dg="$(git -C "$REPO" rev-parse spira/sp-at-dg)"

# sp-at-dc adds an unrelated file.
printf 'clean\n' > "$wt_clean/sp-at-dc.txt"
git -C "$wt_clean" add -A && git -C "$wt_clean" commit -q -m "sp-at-dc: work"
tip_dc="$(git -C "$REPO" rev-parse spira/sp-at-dc)"

git -C "$wt_build6" merge -q --no-edit --no-ff -m "spira: land sp-at-dg" "$tip_dg" >/dev/null 2>&1
git -C "$wt_build6" merge -q --no-edit --no-ff -m "spira: land sp-at-dc" "$tip_dc" >/dev/null 2>&1
batch_head6="$(git -C "$wt_build6" rev-parse HEAD)"
batch_br6="spira/queue/test6-$$"
git -C "$REPO" branch -f "$batch_br6" "$batch_head6"
git -C "$REPO" worktree remove -f "$wt_build6"

for id in sp-at-dg sp-at-dc; do plant_bead "$id"; done
printf 'BATCHED %s %s\n' "$tip_dg" "$(date +%s)" > "$LANDSTATE/sp-at-dg"
printf 'BATCHED %s %s\n' "$tip_dc" "$(date +%s)" > "$LANDSTATE/sp-at-dc"
{
    printf 'pr=44\n'
    printf 'head=%s\n' "$batch_head6"
    printf 'base=%s\n' "$base_sha_6"
    printf 'members=sp-at-dg:%s sp-at-dc:%s\n' "$tip_dg" "$tip_dc"
    printf 'opened=%s\n' "$(date +%s)"
    printf 'branch=%s\n' "$batch_br6"
} > "$(batch_file)"

# Repro stub returns green for all (simulates container startup failure).
: > "$REPRO_FAIL_FILE"
out="$(verdict "$REPONAME")"
is   "6. diff-attr: sp-at-dg ejected (diff has red suite)"  "EJECTED"  "$(land_state_of sp-at-dg)"
is   "6. diff-attr: sp-at-dc stays BATCHED"                  "BATCHED"  "$(land_state_of sp-at-dc)"
is   "6. diff-attr: same PR number"                          "44"       "$(grep '^pr=' "$(batch_file)" | cut -d= -f2)"
is   "6. diff-attr: batch record kept"                       "1"        "$([ -f "$(batch_file)" ] && echo 1 || echo 0)"
nowant "6. diff-attr: PR not closed"                         "close"    "$(cat "$FORGE_LOG")"
want "6. diff-attr: re-push reported"                        "re-pushed to same PR" "$out"
_new_head6="$(grep '^head=' "$(batch_file)" | cut -d= -f2)"
_remote_head6="$(git -C "$REMOTE" rev-parse "$batch_br6" 2>/dev/null || echo none)"
is   "6. diff-attr: head resealed"                           "$_new_head6" "$_remote_head6"
[ "${_new_head6:-}" != "${batch_head6:-}" ] \
    && ok "6. diff-attr: new head differs from old" \
    || bad "6. diff-attr: new head differs from old" "head unchanged: ${_new_head6:-}"
clean_case

# =============================================================================
# 7. DIFF-BASED ATTRIBUTION READS THE MEMBER'S OWN CHANGE — both members forked
#    before the base advanced, and the advance touched the suite that goes red.
#    Diffing the base against a member's tip shows that suite as changed by a
#    member that never touched it; all six members of PR 87 were ejected that way
#    (2026-09-19).
# =============================================================================
testdb_reset
wt_old="$RUN/worktree/sp-at-old"
wt_old2="$RUN/worktree/sp-at-old2"
git -C "$REPO" worktree add -q -b "spira/sp-at-old" "$wt_old" origin/main
git -C "$REPO" worktree add -q -b "spira/sp-at-old2" "$wt_old2" origin/main
printf 'unrelated\n' > "$wt_old/sp-at-old.txt"
git -C "$wt_old" add -A && git -C "$wt_old" commit -q -m "sp-at-old: work"
printf 'unrelated\n' > "$wt_old2/sp-at-old2.txt"
git -C "$wt_old2" add -A && git -C "$wt_old2" commit -q -m "sp-at-old2: work"
tip_old="$(git -C "$REPO" rev-parse spira/sp-at-old)"
tip_old2="$(git -C "$REPO" rev-parse spira/sp-at-old2)"

# The base advances past both fork points, touching the suite that goes red.
wt_adv="$RUN/worktree/.advance-7"
git -C "$REPO" worktree add -q --detach "$wt_adv" origin/main
printf 'advanced\n' >> "$wt_adv/test-canary.sh"
git -C "$wt_adv" add -A && git -C "$wt_adv" commit -q -m "base: touch test-canary.sh"
git -C "$wt_adv" push -q origin HEAD:main
git -C "$REPO" worktree remove -f "$wt_adv"
git -C "$REPO" fetch -q origin
base_sha_7="$(git -C "$REPO" rev-parse origin/main)"

wt_build7="$RUN/worktree/.batch-build-7"
git -C "$REPO" worktree add -q --detach "$wt_build7" "$base_sha_7"
git -C "$wt_build7" merge -q --no-edit --no-ff -m "spira: land sp-at-old" "$tip_old" >/dev/null 2>&1
git -C "$wt_build7" merge -q --no-edit --no-ff -m "spira: land sp-at-old2" "$tip_old2" >/dev/null 2>&1
batch_head7="$(git -C "$wt_build7" rev-parse HEAD)"
batch_br7="spira/queue/test7-$$"
git -C "$REPO" branch -f "$batch_br7" "$batch_head7"
git -C "$REPO" worktree remove -f "$wt_build7"

for id in sp-at-old sp-at-old2; do plant_bead "$id"; done
printf 'BATCHED %s %s\n' "$tip_old" "$(date +%s)" > "$LANDSTATE/sp-at-old"
printf 'BATCHED %s %s\n' "$tip_old2" "$(date +%s)" > "$LANDSTATE/sp-at-old2"
{
    printf 'pr=45\n'
    printf 'head=%s\n' "$batch_head7"
    printf 'base=%s\n' "$base_sha_7"
    printf 'members=sp-at-old:%s sp-at-old2:%s\n' "$tip_old" "$tip_old2"
    printf 'opened=%s\n' "$(date +%s)"
    printf 'branch=%s\n' "$batch_br7"
} > "$(batch_file)"

printf 'red\nred-suite: test-canary.sh\n' > "$FORGE_STATUS_FILE"
: > "$REPRO_FAIL_FILE"
out="$(verdict "$REPONAME")"
nowant "7. own-change: sp-at-old not ejected for the base's advance"  "EJECTED" "$(land_state_of sp-at-old)"
nowant "7. own-change: sp-at-old2 not ejected for the base's advance" "EJECTED" "$(land_state_of sp-at-old2)"
nowant "7. own-change: no ejection reported"                          "ejected sp-at-old" "$out"
clean_case

# =============================================================================
# 8. PER-MEMBER FILTERING — guilty (spira/suites.sh diff → test-suites-hygiene.sh
#    selected + repro red) ejected; innocent (inert .txt diff, also in
#    REPRO_FAIL_FILE) skipped by per-member selection, NOT ejected.
# =============================================================================
testdb_reset
printf 'red\nred-suite: test-suites-hygiene.sh\n' > "$FORGE_STATUS_FILE"
make_branch sp-at-guilty spira/suites.sh
make_branch sp-at-innocent sp-at-innocent.txt
build_batch sp-at-guilty sp-at-innocent > /dev/null
for id in sp-at-guilty sp-at-innocent; do plant_bead "$id"; done
printf 'spira/sp-at-guilty\nspira/sp-at-innocent\n' > "$REPRO_FAIL_FILE"
verdict "$REPONAME" > /dev/null
is   "8. per-member: sp-at-guilty ejected"          "EJECTED"  "$(land_state_of sp-at-guilty)"
is   "8. per-member: sp-at-innocent stays BATCHED"   "BATCHED"  "$(land_state_of sp-at-innocent)"
clean_case

# =============================================================================
# 9. NEW-BASE SUITE — a suite that exists only on the advanced base (added after
#    the member forked) is still reproduced when the member breaks it.
#    Without the fix, testenv-batch exits 2 ("unknown suite") against the bare
#    member tip, reads as not-reproduced, and the member escapes ejection.
#    The fix merges the member onto the batch base before running repro, so the
#    new suite is present and a genuine failure is caught.
# =============================================================================
testdb_reset
# Create member branch forked from current origin/main (before the suite lands).
wt_nb="$RUN/worktree/sp-at-nb"
git -C "$REPO" worktree add -q -b "spira/sp-at-nb" "$wt_nb" origin/main
printf 'unrelated\n' > "$wt_nb/sp-at-nb.txt"
git -C "$wt_nb" add -A && git -C "$wt_nb" commit -q -m "sp-at-nb: work"

# Advance origin/main to add a new suite that didn't exist when sp-at-nb forked.
wt_adv9="$RUN/worktree/.advance-9"
git -C "$REPO" worktree add -q --detach "$wt_adv9" origin/main
printf '#!/usr/bin/env bash\nset -uo pipefail\n' > "$wt_adv9/test-sp-i981i-new.sh"
git -C "$wt_adv9" add -A && git -C "$wt_adv9" commit -q -m "base: add test-sp-i981i-new.sh"
git -C "$wt_adv9" push -q origin HEAD:main
git -C "$REPO" worktree remove -f "$wt_adv9"
git -C "$REPO" fetch -q origin

# Build batch: base is now the advanced origin/main; sp-at-nb's tip is from before.
build_batch sp-at-nb > /dev/null
plant_bead sp-at-nb

# Forge: the new suite (present only on the base, not in sp-at-nb's bare tree) is red.
printf 'red\nred-suite: test-sp-i981i-new.sh\n' > "$FORGE_STATUS_FILE"
# Repro: sp-at-nb breaks the new suite; matched by ancestry in the repro stub.
printf 'spira/sp-at-nb\n' > "$REPRO_FAIL_FILE"

verdict "$REPONAME" > /dev/null
is "9. new-base suite: sp-at-nb ejected (suite present only on advanced base)" \
    "EJECTED" "$(land_state_of sp-at-nb)"
clean_case

# =============================================================================
# 10. WHOLE-TREE UNSELECTED — a member whose diff touches no file covered by the
#     red suite is still ejected when the suite fails against its merged tree.
#     Per-member selection (--no-all-fallback) skips the member; without the
#     unselected-suite fallback the system halves, which never converges on a
#     member+base break.
#     Real case: test-incident-cause.sh covers spira/incident.sh (and others)
#     but not spira/testenv-batch.sh; a member that adds an undeclared site to
#     testenv-batch.sh is invisible to per-member selection.
# =============================================================================
testdb_reset
# test-incident-cause.sh covers spira/incident.sh (and a few others), not
# spira/testenv-batch.sh — so sp-at-uns-a's diff will not select it.
printf 'red\nred-suite: test-incident-cause.sh\n' > "$FORGE_STATUS_FILE"
make_branch sp-at-uns-a spira/testenv-batch.sh
make_branch sp-at-uns-b sp-at-uns-b.txt
build_batch sp-at-uns-a sp-at-uns-b > /dev/null
for id in sp-at-uns-a sp-at-uns-b; do plant_bead "$id"; done
# sp-at-uns-a fails the suite when merged onto base; sp-at-uns-b does not.
printf 'spira/sp-at-uns-a\n' > "$REPRO_FAIL_FILE"
verdict "$REPONAME" > /dev/null
is "10. unselected: sp-at-uns-a ejected (whole-tree suite, diff not mapped)" \
    "EJECTED" "$(land_state_of sp-at-uns-a)"
is "10. unselected: sp-at-uns-b stays BATCHED" \
    "BATCHED" "$(land_state_of sp-at-uns-b)"
clean_case

# =============================================================================
# 11. EJECTION NOTE CONTAINS FAIL LINES — repro stub prints a FAIL line;
#     the ejected bead's reopen note carries that line verbatim so the next
#     aeon has the failing assertion, not just the suite name
#     (law-a-retry-must-change-an-input, law-escalations-carry-their-evidence).
# =============================================================================
testdb_reset
make_branch sp-at-fnl spira/canary.sh
build_batch sp-at-fnl > /dev/null
plant_bead sp-at-fnl
printf 'spira/sp-at-fnl\n' > "$REPRO_FAIL_FILE"
export REPRO_FAIL_LINE="FAIL stub-assertion: expected [landed] got []"
verdict "$REPONAME" > /dev/null
unset REPRO_FAIL_LINE
is   "11. fail-note: sp-at-fnl ejected"           "EJECTED"  "$(land_state_of sp-at-fnl)"
want "11. fail-note: note contains the FAIL line"  "FAIL stub-assertion: expected [landed] got []" "$(notes_of sp-at-fnl)"
clean_case

# =============================================================================
# 12. EJECTION NOTE FALLBACK — repro output has no FAIL line (e.g. a FENCE
#     guard message); last lines of output must still appear in the note so
#     a retrier has more than just the suite name
#     (law-escalations-carry-their-evidence).
# =============================================================================
testdb_reset
make_branch sp-at-nfl spira/canary.sh
build_batch sp-at-nfl > /dev/null
plant_bead sp-at-nfl
printf 'spira/sp-at-nfl\n' > "$REPRO_FAIL_FILE"
export REPRO_FAIL_LINE="FENCE builder: FAYTH_LABELS='spira,plan' does not require 'workspace'"
verdict "$REPONAME" > /dev/null
unset REPRO_FAIL_LINE
is   "12. no-fail-line: sp-at-nfl ejected"                    "EJECTED" "$(land_state_of sp-at-nfl)"
want "12. no-fail-line: note contains fallback output line"    "FENCE builder" "$(notes_of sp-at-nfl)"
clean_case

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
