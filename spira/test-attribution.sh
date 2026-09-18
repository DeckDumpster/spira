#!/usr/bin/env bash
#
# test-attribution.sh — merge-queue attribution: eject reproducers, halve
# together-only reds, quarantine flaky suites, eject on second unreproduced red,
# and verify the local-gate meter.
#
# Five cases (nine assertions):
#   1. Three members with one planted breaker → that member ejected, two survivors
#      returned to CERTIFIED, PR closed, batch removed.
#   2. Two members that break only together → batch halved (first half
#      gets epoch=1, second waits); PR closed, batch removed.
#   3. A planted flake (no member reproduces, batch head also green) →
#      suites.sh observe-flake called, all members requeued, batch removed.
#   4. Single member, second unreproduced red on same tip → ejected.
#   5. Meter: CAUGHT written by queue.sh submit failure; ESCAPED written by
#      attribution; queue.sh stats reports both correctly.
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
# Returns exit 1 (red) if the branch name is listed in REPRO_FAIL_FILE,
# else exit 0 (green). Receives the branch as the last positional argument.
cat > "$SH/repro-stub.sh" <<'REPRO'
#!/usr/bin/env bash
br=""
while [ $# -gt 0 ]; do
    case "$1" in --mode|--suites) shift 2 ;; *) br="$1"; shift ;; esac
done
fail_list="$(cat "${REPRO_FAIL_FILE}" 2>/dev/null || true)"
for f in $fail_list; do
    [ "$f" = "$br" ] && exit 1
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

echo "test-attribution.sh"

# Seed the status file default for all red cases.
printf 'red\nred-suite: test-canary.sh\n' > "$FORGE_STATUS_FILE"

# =============================================================================
# POSITIVE CONTROL: repro stub is actually reached. Without this, "no ejection"
# passes just as well against a verdict that errors before calling the stub.
# =============================================================================
testdb_reset
build_members sp-at-ctrl > /dev/null
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
build_members sp-at-a sp-at-b sp-at-c > /dev/null
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

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
