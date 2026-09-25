#!/usr/bin/env bash
# test-batch.sh — merge-queue batch builder.
#
# Ten cases:
#   1. 8 certified branches trigger a batch at once.
#   2. 3 certified branches trigger a batch only after the planted wait.
#   3. A suite-state transition branch is ordered before regular branches.
#   4. A branch that conflicts with a prior batch member is skipped (stays CERTIFIED).
#   5. An open batch record prevents a second batch from opening.
#   b. Local gate passes: gate called once, PR opened.
#   c. Local gate red, one member reproduces: ejected, rebuilt batch gated and PR opened.
#   e. Local gate red, no member reproduces: PR IS opened; CI adjudicates.
#   f. Local gate red, one member reproduces: rebuild happens (ejection path not bypassed).
#   d. Meter line written for (b) and (c); queue.sh stats reports local_red_rate.
#   r. A CERTIFIED branch that conflicts with base but rebases cleanly is handed to
#      the landing pass rather than gated inline under the queue lock (sp-5yw3b).
#
# Cases 6, 7, 8, A, B moved to test-batch-reconcile.sh (sp-s088v.16): the pre-guard
# reconciliation shapes (landing-record parsing, already-in-base, branch-gone,
# stale-certification) share one row-per-shape fixture there instead of this file's.
#
# The forge seam is a local fixture that records pr-create calls and returns
# incrementing PR numbers; no network is reached.
# The gate is a stub (always pass for cases 1-5; controlled for b/c).
# SPIRA_QUEUE_REPRO_BATCH is a stub that returns red only for planted branches.
#
# covers: spira/batch.sh spira/forge.sh spira/conf.sh spira/landing.sh spira/queue.sh
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
testdb_require test-batch
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up batch || { echo "test-batch: could not build fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
SPIRA_QUEUE_LOCAL_GATE=""

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

# Gate stub: always pass, count calls. Tests that need a different gate overwrite this.
GATE_COUNT="$TMP/gate-count"
: > "$GATE_COUNT"
cat > "$SH/gate.sh" <<GSTUB
#!/usr/bin/env bash
printf '%s\n' "\$1" >> "$GATE_COUNT"
printf 'gate: VERDICT=PASS reason=stub branch=%s repo=%s suite=none\n' "\$1" "\${2:-?}" >&2
exit 0
GSTUB
chmod +x "$SH/gate.sh"

# Repro stub: always green (no branch reproduces by default).
# Tests that need a red member write the branch name to REPRO_FAIL_FILE.
# Every invocation is also appended to REPRO_CALL_LOG, so a test can assert the
# seam was never reached (case r).
REPRO_FAIL_FILE="$TMP/repro-fail-file"
: > "$REPRO_FAIL_FILE"
export REPRO_FAIL_FILE
REPRO_CALL_LOG="$TMP/repro-call-log"
: > "$REPRO_CALL_LOG"
export REPRO_CALL_LOG
cat > "$SH/repro-stub.sh" <<'REPRO'
#!/usr/bin/env bash
br=""
while [ $# -gt 0 ]; do
    case "$1" in --mode|--suites) shift 2 ;; *) br="$1"; shift ;; esac
done
printf '%s\n' "$br" >> "${REPRO_CALL_LOG:-/dev/null}"
fail_list="$(cat "${REPRO_FAIL_FILE}" 2>/dev/null || true)"
for f in $fail_list; do [ "$f" = "$br" ] && exit 1; done
exit 0
REPRO
chmod +x "$SH/repro-stub.sh"

# Forge fixture: log every pr-create call, return incrementing PR numbers.
# A real forge is never reached in this suite.
# Paths are baked into the script at write time (unquoted heredoc) so the fixture
# does not need FORGE_LOG/BODY_LOG/COMMENT_LOG passed through the environment.
FORGE_LOG="$TMP/forge-log"
BODY_LOG="$TMP/body-log"
COMMENT_LOG="$TMP/comment-log"
TITLE_LOG="$TMP/title-log"
cat > "$SH/forge-fixture.sh" << FORGE
#!/usr/bin/env bash
# Fixture forge seam.  Pinned to a non-default SPIRA_FORGE so that an assertion
# passing against "used gh" fails rather than passing vacuously.
cmd="\${1:-}"; shift; repo="\${1:-}"; shift
case "\$cmd" in
    main-gate-status) printf 'green deadbeef\n' ;;
    pr-create)
        head="\${1:-}" base="\${2:-}" title="\${3:-}"
        n=\$(( \$(wc -l < "$FORGE_LOG" 2>/dev/null || echo 0) + 1 ))
        body="\$(cat)"
        printf '%s\t%s\n' "\$head" "\$n" >> "$FORGE_LOG"
        printf '%s\n' "\$title" >> "$TITLE_LOG"
        printf '%s\n' "\$body" >> "$BODY_LOG"
        printf '%s\n' "\$n"
        ;;
    pr-number)
        head="\${1:-}"
        grep "^\${head}	" "$FORGE_LOG" 2>/dev/null | tail -1 | cut -f2
        ;;
    pr-comment)
        pr_n="\${1:-}" comment="\${2:-}"
        printf '%s\t%s\n' "\$pr_n" "\$comment" >> "$COMMENT_LOG"
        ;;
    *) printf 'forge-fixture: unknown command: %s\n' "\$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"
: > "$FORGE_LOG"
: > "$BODY_LOG"
: > "$TITLE_LOG"
: > "$COMMENT_LOG"

# Write repo-map: queue mode, pinned to a non-default land value.
cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

B() { bd -C "$SPIRA_DB" "$@"; }

batch() {
    local _gate="${SPIRA_QUEUE_LOCAL_GATE:-}"
    [ "$_gate" = "0" ] || _gate="1"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_SUITE_STATE_FILE="spira/suite-state" \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
    SPIRA_QUEUE_REPRO_BATCH="$SH/repro-stub.sh" \
    SPIRA_QUEUE_LOCAL_GATE="$_gate" \
        bash "$SH/batch.sh" "$@" 2>&1
}

seed() {
    testdb_reset
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
}

# plant_bead <id>  — seed a closed bead with no repo: label (defaults to home repo)
plant_bead() {
    printf \
        '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$1" "$1" "$1" | testdb_seed
}

# plant_bead_t <id> <title>  — seed a closed bead with a specific title
plant_bead_t() {
    printf \
        '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$1" "$2" "$1" | testdb_seed
}

# branch <id> [epoch]  — create a spira/<id> branch on main with one commit
#                        and plant CERTIFIED landstate at <epoch> (default: now)
branch() {
    local id="$1" epoch="${2:-$(date +%s)}"
    local wt="$RUN/worktree/$id"
    git -C "$REPO" worktree add -q -b "spira/$id" "$wt" main 2>/dev/null || true
    printf '%s\n' "$id" > "$wt/$id.txt"
    git -C "$wt" add -A
    git -C "$wt" commit -q -m "$id: work"
    local tip; tip="$(git -C "$REPO" rev-parse "spira/$id")"
    printf 'CERTIFIED %s %s\n' "$tip" "$epoch" > "$LANDSTATE/$id"
    plant_bead "$id"
}

# branch_t <id> <title> [epoch]  — branch with a custom bead title
branch_t() {
    local id="$1" title="$2" epoch="${3:-$(date +%s)}"
    local wt="$RUN/worktree/$id"
    git -C "$REPO" worktree add -q -b "spira/$id" "$wt" main 2>/dev/null || true
    printf '%s\n' "$id" > "$wt/$id.txt"
    git -C "$wt" add -A
    git -C "$wt" commit -q -m "$id: work"
    local tip; tip="$(git -C "$REPO" rev-parse "spira/$id")"
    printf 'CERTIFIED %s %s\n' "$tip" "$epoch" > "$LANDSTATE/$id"
    plant_bead_t "$id" "$title"
}

# branch_p <id> <priority> [epoch] — branch with an explicit bead priority,
# so a test can prove ordering is (or is not) driven by priority.
branch_p() {
    local id="$1" priority="$2" epoch="${3:-$(date +%s)}"
    local wt="$RUN/worktree/$id"
    git -C "$REPO" worktree add -q -b "spira/$id" "$wt" main 2>/dev/null || true
    printf '%s\n' "$id" > "$wt/$id.txt"
    git -C "$wt" add -A
    git -C "$wt" commit -q -m "$id: work"
    local tip; tip="$(git -C "$REPO" rev-parse "spira/$id")"
    printf 'CERTIFIED %s %s\n' "$tip" "$epoch" > "$LANDSTATE/$id"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","priority":%d,"labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$id" "$id" "$priority" "$id" | testdb_seed
}

# branch_p_express <id> <priority> [epoch] — branch_p, with the express label added.
branch_p_express() {
    local id="$1" priority="$2" epoch="${3:-$(date +%s)}"
    local wt="$RUN/worktree/$id"
    git -C "$REPO" worktree add -q -b "spira/$id" "$wt" main 2>/dev/null || true
    printf '%s\n' "$id" > "$wt/$id.txt"
    git -C "$wt" add -A
    git -C "$wt" commit -q -m "$id: work"
    local tip; tip="$(git -C "$REPO" rev-parse "spira/$id")"
    printf 'CERTIFIED %s %s\n' "$tip" "$epoch" > "$LANDSTATE/$id"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","priority":%d,"labels":["express"],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$id" "$id" "$priority" "$id" | testdb_seed
}

tip_of() { git -C "$REPO" rev-parse "spira/$1" 2>/dev/null; }
bisect_file() { printf '%s/%s/bisect' "$QUEUEDIR" "$REPONAME"; }

open_batch_file() { printf '%s/%s/open' "$QUEUEDIR" "$REPONAME"; }
batch_pr()        { grep '^pr=' "$(open_batch_file)" 2>/dev/null | cut -d= -f2; }
is_batched()      { grep -q '^BATCHED' "$LANDSTATE/${1:-}" 2>/dev/null; }
is_certified()    { awk '{print $1}' "$LANDSTATE/${1:-}" 2>/dev/null | grep -q '^CERTIFIED$'; }
is_ejected()      { awk '{print $1}' "$LANDSTATE/${1:-}" 2>/dev/null | grep -q '^EJECTED$'; }
gate_n()          { wc -l < "$GATE_COUNT" 2>/dev/null | tr -d ' ' || printf '0'; }
landing_log()     { cat "$RUN/landing.log" 2>/dev/null || true; }

# clean_case — remove all landstate entries and spira/* branches between test cases
clean_case() {
    rm -f "$QUEUEDIR/$REPONAME/open"
    rm -f "$QUEUEDIR/$REPONAME/bisect"
    rm -f "$RUN/landing.log"
    : > "$FORGE_LOG"
    : > "$BODY_LOG"
    : > "$TITLE_LOG"
    : > "$COMMENT_LOG"
    : > "$GATE_COUNT"
    : > "$REPRO_FAIL_FILE"
    : > "$REPRO_CALL_LOG"
    find "$LANDSTATE" -maxdepth 1 -type f 2>/dev/null -delete
    # Remove the batch worktree cleanly first (unregisters AND deletes the dir).
    local wt="$RUN/worktree/.batch-$(basename "$REPO")"
    if [ -d "$wt" ]; then
        git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    fi
    # Remove ALL remaining worktree dirs before pruning — git branch -D only succeeds
    # once the worktree registration is gone, and worktree prune only prunes entries
    # whose directory no longer exists.
    rm -rf "$RUN/worktree" && mkdir -p "$RUN/worktree"
    git -C "$REPO" worktree prune 2>/dev/null || true
    # Now all linked-worktree registrations are gone; branches are deletable.
    git -C "$REPO" for-each-ref --format='%(refname:short)' 'refs/heads/spira/*' 2>/dev/null \
        | while read -r br; do
            git -C "$REPO" branch -D "$br" 2>/dev/null || true
        done
}

echo "test-batch.sh"

# =============================================================================
# POSITIVE CONTROL: the forge is reached. Without this, "no batch opened" passes
# just as well against a batch.sh that silently returns before calling the forge.
# =============================================================================
seed
for i in $(seq 1 8); do branch "sp-bt1-$i"; done
out="$(batch "$REPONAME")"
is   "8 certified: PR opened"      "1"  "$(batch_pr)"
is   "8 certified: all BATCHED"    "8"  \
     "$(for i in $(seq 1 8); do is_batched "sp-bt1-$i" && echo y; done | grep -c y)"
want "8 certified: batch reported" "PR 1 opened" "$out"
clean_case

# =============================================================================
# 2. FEWER THAN MAX: no batch when too new; batch when oldest is old enough.
# =============================================================================
seed
NOW="$(date +%s)"
NEW_EPOCH="$NOW"
OLD_EPOCH=$(( NOW - 1800 - 1 ))   # 30 min + 1 s — past the wait threshold

for i in 1 2 3; do branch "sp-bt2n-$i" "$NEW_EPOCH"; done
batch "$REPONAME" > /dev/null
is "3 new: no batch opens" "0" "$([ -f "$(open_batch_file)" ] && echo 1 || echo 0)"

# Age the landstate entries.
for i in 1 2 3; do
    tip="$(git -C "$REPO" rev-parse "spira/sp-bt2n-$i")"
    printf 'CERTIFIED %s %s\n' "$tip" "$OLD_EPOCH" > "$LANDSTATE/sp-bt2n-$i"
done
batch "$REPONAME" > /dev/null
is "3 old: batch opens" "1" "$([ -f "$(open_batch_file)" ] && echo 1 || echo 0)"
clean_case

# =============================================================================
# 3. TRANSITION ORDERING: a suite-state transition branch goes before a branch
#    certified earlier.
# =============================================================================
seed
NOW="$(date +%s)"
# Age both past the 30-min wait so the batch triggers with just 2 branches.
OLD3=$(( NOW - 1800 - 2 ))

# Regular branch: certified first (older epoch).
branch "sp-bt3-reg" $(( OLD3 - 1 ))

# Transition branch: certified second (newer but still old); modifies suite-state.
git -C "$REPO" worktree add -q -b "spira/sp-bt3-trans" \
    "$RUN/worktree/sp-bt3-trans" main 2>/dev/null || true
mkdir -p "$RUN/worktree/sp-bt3-trans/spira"
printf 'test-something.sh | quarantined | 2026-09-16T00:00:00Z | sp-xxx | flaky\n' \
    > "$RUN/worktree/sp-bt3-trans/spira/suite-state"
git -C "$RUN/worktree/sp-bt3-trans" add -A
git -C "$RUN/worktree/sp-bt3-trans" commit -q -m "sp-bt3-trans: quarantine suite"
trans_tip="$(git -C "$REPO" rev-parse "spira/sp-bt3-trans")"
printf 'CERTIFIED %s %s\n' "$trans_tip" "$OLD3" > "$LANDSTATE/sp-bt3-trans"
plant_bead "sp-bt3-trans"

batch "$REPONAME" > /dev/null
batch_br="$(git -C "$REPO" for-each-ref --format='%(refname:short)' \
    'refs/heads/spira/queue/*' 2>/dev/null | tail -1)"
is "transition: batch opened" "1" "$([ -n "$batch_br" ] && echo 1 || echo 0)"
# The first merge commit in the batch branch should be the transition branch.
first_landed="$(git -C "$REPO" log --format='%s' "origin/main..$batch_br" \
    | grep 'spira: land' | tail -1)"
want "transition goes first" "sp-bt3-trans" "$first_landed"
clean_case

# =============================================================================
# 4. CONFLICTING PAIR: the second branch (same file, different content) is skipped.
# =============================================================================
seed
NOW="$(date +%s)"
# Age both branches past the 30-minute wait threshold so the batch triggers.
OLD4=$(( NOW - 1800 - 2 ))

# Branch A: creates shared.txt = version-a (certified first).
git -C "$REPO" worktree add -q -b "spira/sp-bt4-a" \
    "$RUN/worktree/sp-bt4-a" main 2>/dev/null || true
printf 'version-a\n' > "$RUN/worktree/sp-bt4-a/shared.txt"
git -C "$RUN/worktree/sp-bt4-a" add -A
git -C "$RUN/worktree/sp-bt4-a" commit -q -m "sp-bt4-a: version a"
tip_a="$(git -C "$REPO" rev-parse "spira/sp-bt4-a")"
printf 'CERTIFIED %s %s\n' "$tip_a" $(( OLD4 - 1 )) > "$LANDSTATE/sp-bt4-a"
plant_bead "sp-bt4-a"

# Branch B: creates shared.txt = version-b (certified after A — goes second).
git -C "$REPO" worktree add -q -b "spira/sp-bt4-b" \
    "$RUN/worktree/sp-bt4-b" main 2>/dev/null || true
printf 'version-b\n' > "$RUN/worktree/sp-bt4-b/shared.txt"
git -C "$RUN/worktree/sp-bt4-b" add -A
git -C "$RUN/worktree/sp-bt4-b" commit -q -m "sp-bt4-b: version b"
tip_b="$(git -C "$REPO" rev-parse "spira/sp-bt4-b")"
printf 'CERTIFIED %s %s\n' "$tip_b" "$OLD4" > "$LANDSTATE/sp-bt4-b"
plant_bead "sp-bt4-b"

batch "$REPONAME" > /dev/null
is      "conflict: A is BATCHED"          "1" "$(is_batched "sp-bt4-a" && echo 1 || echo 0)"
is      "conflict: B stays CERTIFIED"     "1" "$(is_certified "sp-bt4-b" && echo 1 || echo 0)"
clean_case

# =============================================================================
# 5. OPEN BATCH BLOCKS SECOND: a planted batch record prevents another opening.
# =============================================================================
seed
for i in $(seq 1 8); do branch "sp-bt5-$i"; done
# Plant a fake open batch record.
printf 'pr=99\nhead=abc\nbase=%s\nmembers=\nopened=0\nbranch=spira/queue/fake\n' \
    "$(git -C "$REPO" rev-parse origin/main)" > "$(open_batch_file)"

batch "$REPONAME" > /dev/null
is "open batch blocks second" "99" "$(batch_pr)"
clean_case

# =============================================================================
# b. LOCAL GATE GREEN: gate runs exactly once on the combined batch; PR opens.
# =============================================================================
seed
NOW="$(date +%s)"; OLD_B=$(( NOW - 1800 - 1 ))
for i in 1 2 3; do branch "sp-btb-$i" "$OLD_B"; done
out="$(batch "$REPONAME")"
is   "b: gate called exactly once"   "1"  "$(gate_n)"
is   "b: PR opened"                  "1"  "$(batch_pr)"
want "b: QUEUE BATCH meter written"  "QUEUE BATCH"   "$(landing_log)"
want "b: meter verdict=green"        "verdict=green" "$(landing_log)"
want "b: meter members=3"            "members=3"     "$(landing_log)"
clean_case

# =============================================================================
# c. LOCAL GATE RED WITH ONE REPRODUCER: ejected, rebuilt batch PR opened.
#
# Gate fails on its first call (batch of 3), passes on its second (rebuilt batch
# of 2). The repro stub identifies sp-btc-red as the reproducing member.
# =============================================================================
seed
NOW="$(date +%s)"; OLD_C=$(( NOW - 1800 - 1 ))
for id in sp-btc-1 sp-btc-2 sp-btc-red; do branch "$id" "$OLD_C"; done

# Gate stub: fails on first call, passes thereafter.
cat > "$SH/gate.sh" <<GSTUB2
#!/usr/bin/env bash
n=\$(wc -l < "$GATE_COUNT" 2>/dev/null | tr -d ' ' || printf 0)
printf '%s\n' "\$1" >> "$GATE_COUNT"
if [ "\${n:-0}" -eq 0 ]; then
    printf 'gate: VERDICT=FAIL reason=red test-btc.sh FAILED branch=%s repo=%s suite=test-btc.sh\n' "\$1" "\${2:-?}" >&2
    exit 1
fi
printf 'gate: VERDICT=PASS reason=stub branch=%s repo=%s suite=none\n' "\$1" "\${2:-?}" >&2
exit 0
GSTUB2
chmod +x "$SH/gate.sh"

# Repro stub: sp-btc-red reproduces; sp-btc-1 and sp-btc-2 do not.
printf 'spira/sp-btc-red\n' > "$REPRO_FAIL_FILE"

out="$(batch "$REPONAME")"
is   "c: gate called twice (full + rebuilt)"  "2"  "$(gate_n)"
is   "c: PR opened (rebuilt batch)"           "1"  "$(batch_pr)"
is   "c: sp-btc-red ejected"    "1" "$(is_ejected "sp-btc-red" && echo 1 || echo 0)"
is   "c: sp-btc-1 batched"      "1" "$(is_batched "sp-btc-1"   && echo 1 || echo 0)"
is   "c: sp-btc-2 batched"      "1" "$(is_batched "sp-btc-2"   && echo 1 || echo 0)"
want "c: CAUGHT line for ejected"  "QUEUE CAUGHT"    "$(landing_log)"
want "c: branch=sp-btc-red in CAUGHT" "branch=sp-btc-red" "$(landing_log)"
want "c: red BATCH meter line"   "verdict=red"      "$(landing_log)"
want "c: green BATCH meter line" "verdict=green"    "$(landing_log)"
want "c: ejected field in log"   "ejected=sp-btc-red" "$(landing_log)"

# Restore default always-pass gate stub.
cat > "$SH/gate.sh" <<GSTUB
#!/usr/bin/env bash
printf '%s\n' "\$1" >> "$GATE_COUNT"
printf 'gate: VERDICT=PASS reason=stub branch=%s repo=%s suite=none\n' "\$1" "\${2:-?}" >&2
exit 0
GSTUB
chmod +x "$SH/gate.sh"
clean_case

# =============================================================================
# e. UNATTRIBUTABLE LOCAL-GATE RED: gate red, no member reproduces —
#    PR IS opened; CI adjudicates (law-local-gates-buy-latency-not-coverage).
#
# POSITIVE CONTROL: gate_n starts at 0 before this case (clean_case zeros it);
# batch_pr reads from the forge fixture, which is only written when pr-create is
# called — an empty return proves the forge was never reached.
#
# Against pre-fix batch.sh this case fails: no PR is opened and the batch
# rebuilds the same tree, running the gate a second time (gate_n would be 2).
# =============================================================================
seed
NOW="$(date +%s)"; OLD_E=$(( NOW - 1800 - 1 ))
for id in sp-bte-1 sp-bte-2 sp-bte-3; do branch "$id" "$OLD_E"; done

# Gate red on first call; would pass on a second call — but with the fix, the
# second call never happens when ejected_arr is empty.
cat > "$SH/gate.sh" <<GSTUB_E
#!/usr/bin/env bash
n=\$(wc -l < "$GATE_COUNT" 2>/dev/null | tr -d ' ' || printf 0)
printf '%s\n' "\$1" >> "$GATE_COUNT"
if [ "\${n:-0}" -eq 0 ]; then
    printf 'gate: VERDICT=FAIL reason=red test-bte.sh FAILED branch=%s repo=%s suite=test-bte.sh\n' "\$1" "\${2:-?}" >&2
    exit 1
fi
printf 'gate: VERDICT=PASS reason=stub branch=%s repo=%s suite=none\n' "\$1" "\${2:-?}" >&2
exit 0
GSTUB_E
chmod +x "$SH/gate.sh"

# No member reproduces (repro stub default: all green).
: > "$REPRO_FAIL_FILE"

out_e="$(batch "$REPONAME")"
is   "e: gate called exactly once (no re-gate)"   "1"  "$(gate_n)"
is   "e: PR IS opened (unattributable red)"        "1"  "$(batch_pr)"
want "e: no-member-reproduced log line"            "no member reproduced" "$out_e"
want "e: ejected=- in landing log"                 "ejected=-"            "$(landing_log)"

# Restore default always-pass gate stub.
cat > "$SH/gate.sh" <<GSTUB
#!/usr/bin/env bash
printf '%s\n' "\$1" >> "$GATE_COUNT"
printf 'gate: VERDICT=PASS reason=stub branch=%s repo=%s suite=none\n' "\$1" "\${2:-?}" >&2
exit 0
GSTUB
chmod +x "$SH/gate.sh"
clean_case

# =============================================================================
# f. GENUINE EJECTION STILL REBUILDS: gate red, one member reproduces —
#    ejected, rebuilt batch opened; the unattributable-red path is NOT taken.
#
# This guards against an over-broad fix that always falls through to open the
# PR regardless of ejections. With such a fix: gate_n would be 1 (re-gate
# skipped), sp-btf-red would not be ejected, and the batch would contain all
# three members — the three assertions below would all fail.
# =============================================================================
seed
NOW="$(date +%s)"; OLD_F=$(( NOW - 1800 - 1 ))
for id in sp-btf-1 sp-btf-2 sp-btf-red; do branch "$id" "$OLD_F"; done

# Gate fails on first call (batch of 3), passes on second (rebuilt batch of 2).
cat > "$SH/gate.sh" <<GSTUB_F
#!/usr/bin/env bash
n=\$(wc -l < "$GATE_COUNT" 2>/dev/null | tr -d ' ' || printf 0)
printf '%s\n' "\$1" >> "$GATE_COUNT"
if [ "\${n:-0}" -eq 0 ]; then
    printf 'gate: VERDICT=FAIL reason=red test-btf.sh FAILED branch=%s repo=%s suite=test-btf.sh\n' "\$1" "\${2:-?}" >&2
    exit 1
fi
printf 'gate: VERDICT=PASS reason=stub branch=%s repo=%s suite=none\n' "\$1" "\${2:-?}" >&2
exit 0
GSTUB_F
chmod +x "$SH/gate.sh"

# sp-btf-red reproduces; sp-btf-1 and sp-btf-2 do not.
printf 'spira/sp-btf-red\n' > "$REPRO_FAIL_FILE"

batch "$REPONAME" > /dev/null
is   "f: gate called twice (full + rebuilt)"   "2"  "$(gate_n)"
is   "f: sp-btf-red ejected"                   "1"  "$(is_ejected "sp-btf-red" && echo 1 || echo 0)"
is   "f: PR IS opened (rebuilt batch)"         "1"  "$(batch_pr)"
_f_members="$(grep '^members=' "$(open_batch_file)" 2>/dev/null | cut -d= -f2)"
nowant "f: sp-btf-red not in batch members"   "sp-btf-red" "$_f_members"

# Restore default always-pass gate stub.
cat > "$SH/gate.sh" <<GSTUB
#!/usr/bin/env bash
printf '%s\n' "\$1" >> "$GATE_COUNT"
printf 'gate: VERDICT=PASS reason=stub branch=%s repo=%s suite=none\n' "\$1" "\${2:-?}" >&2
exit 0
GSTUB
chmod +x "$SH/gate.sh"
: > "$REPRO_FAIL_FILE"
clean_case

# =============================================================================
# d. METER STATS: queue.sh stats reads local_red_rate and cost from BATCH lines.
#    Plant one green BATCH line and one red BATCH line in landing.log.
# =============================================================================
seed
NOW="$(date +%s)"
# Green batch: members=3, gate_seconds=5, verdict=green.
printf 'QUEUE BATCH %s repo=%s members=3 gate_seconds=5 verdict=green\n' \
    "$NOW" "$REPONAME" >> "$RUN/landing.log"
# Red batch: members=3, gate_seconds=8, verdict=red, attr_seconds=2, ejected=sp-d1.
printf 'QUEUE BATCH %s repo=%s members=3 gate_seconds=8 verdict=red attr_seconds=2 ejected=sp-d1\n' \
    "$NOW" "$REPONAME" >> "$RUN/landing.log"

stats_out="$(SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_REPO_MAP="$SH/repo-map" \
    bash "$SH/queue.sh" stats 2>&1)"
want "d: local_red_rate 1/2 in stats" "1/2"   "$stats_out"
want "d: batches=2 in stats"          "batches:         2" "$stats_out"
want "d: members=6 in stats"          "6 members"       "$stats_out"
# cost = (5+8)/6 = 2s avg
want "d: cost per branch in stats"    "cost:"            "$stats_out"
clean_case

# =============================================================================
# g. BASE-CONFLICT STAMP: a branch that conflicts with origin/main is reopened
#    AND stamped merge-conflict via bump_requeue, matching landing.sh:624/922.
#
#    POSITIVE CONTROL: on the pre-fix tree, bump_requeue is never called from
#    the base-conflict branch in batch.sh; the spy file stays empty and the
#    assertion reads "1" against "0".
# =============================================================================
clean_case
seed
NOW="$(date +%s)"; OLD_G=$(( NOW - 1800 - 1 ))

# Branch spira/sp-btg: edits conflict.txt = "branch-ver" on top of current main.
git -C "$REPO" worktree add -q -b "spira/sp-btg" \
    "$RUN/worktree/sp-btg" main 2>/dev/null || true
printf 'branch-ver\n' > "$RUN/worktree/sp-btg/conflict.txt"
git -C "$RUN/worktree/sp-btg" add -A
git -C "$RUN/worktree/sp-btg" commit -q -m "sp-btg: work"
tip_g="$(git -C "$REPO" rev-parse "spira/sp-btg")"
printf 'CERTIFIED %s %s\n' "$tip_g" "$OLD_G" > "$LANDSTATE/sp-btg"
plant_bead "sp-btg"

# Push conflict.txt = "main-ver" to origin/main after the branch exists;
# spira/sp-btg now conflicts with the new base.
printf 'main-ver\n' > "$REPO/conflict.txt"
git -C "$REPO" add conflict.txt
git -C "$REPO" commit -q -m "main: set conflict.txt"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

# Spy: append a bump_requeue override to the lib.sh copy; bash uses the last
# definition, so this replaces the original for this batch() call.
# The path to REQUEUE_SPY_G is expanded now; ${1:-} etc. are escaped for runtime.
REQUEUE_SPY_G="$TMP/rq-spy-g"
: > "$REQUEUE_SPY_G"
cat >> "$SH/lib.sh" << LIBSPY

bump_requeue() {
    printf '%s %s\n' "\${1:-}" "\${2:-}" >> "$REQUEUE_SPY_G"
    _bump_write_event "\${1:-}" requeued "\${2:-unrecorded}"
}
LIBSPY

out_g="$(batch "$REPONAME" 2>&1)"
want "g: base-conflict: reopened message"            "conflicts with"   "$out_g"
is   "g: base-conflict: bump_requeue merge-conflict" "1" \
    "$(grep -c "^sp-btg merge-conflict$" "$REQUEUE_SPY_G" 2>/dev/null || echo 0)"

cp "$HERE/lib.sh" "$SH/lib.sh"
clean_case

# =============================================================================
# h. REBASE-CLEAN: a CERTIFIED branch whose base moved with a non-overlapping
#    change is batched without a reopen.
# =============================================================================
clean_case
seed
NOW="$(date +%s)"; OLD_H=$(( NOW - 1800 - 1 ))

# Branch sp-bth: edits clean.txt (no other branch touches this file).
git -C "$REPO" worktree add -q -b "spira/sp-bth" \
    "$RUN/worktree/sp-bth" main 2>/dev/null || true
printf 'branch-value\n' > "$RUN/worktree/sp-bth/clean.txt"
git -C "$RUN/worktree/sp-bth" add -A
git -C "$RUN/worktree/sp-bth" commit -q -m "sp-bth: add clean.txt"
tip_h="$(git -C "$REPO" rev-parse "spira/sp-bth")"
printf 'CERTIFIED %s %s\n' "$tip_h" "$OLD_H" > "$LANDSTATE/sp-bth"
plant_bead "sp-bth"

# Advance main with a non-overlapping change (other.txt, not clean.txt).
printf 'main-value\n' > "$REPO/other.txt"
git -C "$REPO" add other.txt
git -C "$REPO" commit -q -m "main: add other.txt"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

batch "$REPONAME" >/dev/null 2>&1
is   "h. rebase-clean: sp-bth is BATCHED"       "1" "$(is_batched "sp-bth" && echo 1 || echo 0)"
is   "h. rebase-clean: not reopened (no RED)"    "0" \
    "$([ "$(awk '{print $1}' "$LANDSTATE/sp-bth" 2>/dev/null)" = "RED" ] && echo 1 || echo 0)"
is   "h. rebase-clean: PR opened"                "1" "$(batch_pr)"
clean_case

# =============================================================================
# i. REBASE-CONFLICT: a CERTIFIED branch with a true conflict against the new
#    base is still reopened after a failed rebase attempt.
#
#    POSITIVE CONTROL: the spy file is empty before the run; the test would
#    produce "0" against "1" if bump_requeue is never called.
# =============================================================================
clean_case
seed
NOW="$(date +%s)"; OLD_I=$(( NOW - 1800 - 1 ))

# Branch sp-bti: edits conflict2.txt = "branch-ver".
git -C "$REPO" worktree add -q -b "spira/sp-bti" \
    "$RUN/worktree/sp-bti" main 2>/dev/null || true
printf 'branch-ver\n' > "$RUN/worktree/sp-bti/conflict2.txt"
git -C "$RUN/worktree/sp-bti" add -A
git -C "$RUN/worktree/sp-bti" commit -q -m "sp-bti: set conflict2.txt"
tip_i="$(git -C "$REPO" rev-parse "spira/sp-bti")"
printf 'CERTIFIED %s %s\n' "$tip_i" "$OLD_I" > "$LANDSTATE/sp-bti"
plant_bead "sp-bti"

# Advance main with a conflicting change (same file, different content).
printf 'main-ver\n' > "$REPO/conflict2.txt"
git -C "$REPO" add conflict2.txt
git -C "$REPO" commit -q -m "main: set conflict2.txt"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

REQUEUE_SPY_I="$TMP/rq-spy-i"
: > "$REQUEUE_SPY_I"
cat >> "$SH/lib.sh" << LIBSPY_I

bump_requeue() {
    printf '%s %s\n' "\${1:-}" "\${2:-}" >> "$REQUEUE_SPY_I"
    _bump_write_event "\${1:-}" requeued "\${2:-unrecorded}"
}
LIBSPY_I

out_i="$(batch "$REPONAME" 2>&1)"
is   "i. rebase-conflict: sp-bti reopened (RED)"    "1" \
    "$([ "$(awk '{print $1}' "$LANDSTATE/sp-bti" 2>/dev/null)" = "RED" ] && echo 1 || echo 0)"
want "i. rebase-conflict: reopened message"          "conflicts with" "$out_i"
is   "i. rebase-conflict: bump_requeue called"       "1" \
    "$(grep -c "^sp-bti merge-conflict$" "$REQUEUE_SPY_I" 2>/dev/null || echo 0)"

cp "$HERE/lib.sh" "$SH/lib.sh"
clean_case

# =============================================================================
# j. PR BODY CARRIES TITLES: the PR body contains "id — title" for every
#    member; the PR title says "beads for <repo>", not a list of bead IDs.
#
#    POSITIVE CONTROL: BODY_LOG is empty before the run (clean_case zeroed it);
#    a forge that is never reached cannot have written the titles.
# =============================================================================
clean_case
seed
NOW="$(date +%s)"; OLD_J=$(( NOW - 1800 - 1 ))
branch_t "sp-btj-1" "Cache invalidation breaks on empty key"     "$OLD_J"
branch_t "sp-btj-2" "Retry loop exceeds configured max attempts" "$OLD_J"
branch_t "sp-btj-3" "Scope label defaults to literal spira"      "$OLD_J"

is "j. positive-control: body empty before run" "0" \
    "$([ -s "$BODY_LOG" ] && echo 1 || echo 0)"

batch "$REPONAME" >/dev/null 2>&1

want "j. body: sp-btj-1 with title" "sp-btj-1 — Cache invalidation breaks on empty key" \
    "$(cat "$BODY_LOG" 2>/dev/null)"
want "j. body: sp-btj-2 with title" "sp-btj-2 — Retry loop exceeds configured max attempts" \
    "$(cat "$BODY_LOG" 2>/dev/null)"
want "j. body: sp-btj-3 with title" "sp-btj-3 — Scope label defaults to literal spira" \
    "$(cat "$BODY_LOG" 2>/dev/null)"
want "j. title: contains 'beads for'" "beads for" \
    "$(cat "$TITLE_LOG" 2>/dev/null)"
nowant "j. title: no bead id in title" "sp-btj" \
    "$(cat "$TITLE_LOG" 2>/dev/null)"
clean_case

# =============================================================================
# k. TITLE UNAVAILABLE: a member not in the bead database still opens the PR;
#    its body line shows "(title unavailable)".
#
#    POSITIVE CONTROL: the member WITH a known title must appear with that title
#    (if all titles show unavailable, the lookup is simply broken, not graceful).
# =============================================================================
clean_case
seed
NOW="$(date +%s)"; OLD_K=$(( NOW - 1800 - 1 ))
branch_t "sp-btk-known" "Title that is present in the DB" "$OLD_K"

# sp-btk-missing: branch exists, CERTIFIED, but bead not in DB.
wt_k="$RUN/worktree/sp-btk-missing"
git -C "$REPO" worktree add -q -b "spira/sp-btk-missing" "$wt_k" main 2>/dev/null || true
printf 'missing\n' > "$wt_k/sp-btk-missing.txt"
git -C "$wt_k" add -A
git -C "$wt_k" commit -q -m "sp-btk-missing: work"
tip_km="$(git -C "$REPO" rev-parse "spira/sp-btk-missing")"
printf 'CERTIFIED %s %s\n' "$tip_km" "$OLD_K" > "$LANDSTATE/sp-btk-missing"

batch "$REPONAME" >/dev/null 2>&1

want "k. known title present" "sp-btk-known — Title that is present in the DB" \
    "$(cat "$BODY_LOG" 2>/dev/null)"
want "k. unavailable for missing" "(title unavailable)" \
    "$(cat "$BODY_LOG" 2>/dev/null)"
is "k. PR opened despite missing title" "1" "$(batch_pr)"
clean_case

# =============================================================================
# l. EJECT COMMENT: when a member is ejected, a comment on the new PR names
#    the ejected id, its title, and the failing suite, with remaining count.
#
#    POSITIVE CONTROL: COMMENT_LOG is empty before the run (clean_case zeroed
#    it); a comment only appears when the forge pr-comment seam is called.
# =============================================================================
clean_case
seed
NOW="$(date +%s)"; OLD_L=$(( NOW - 1800 - 1 ))
branch_t "sp-btl-1"   "Survivor bead that stays in the batch"    "$OLD_L"
branch_t "sp-btl-2"   "Second survivor in the rebuilt batch"     "$OLD_L"
branch_t "sp-btl-red" "Bead that fails the local gate suite"     "$OLD_L"

# Gate fails on first call (full batch of 3), passes on second (rebuilt batch of 2).
cat > "$SH/gate.sh" <<GSTUB_L
#!/usr/bin/env bash
n=\$(wc -l < "$GATE_COUNT" 2>/dev/null | tr -d ' ' || printf 0)
printf '%s\n' "\$1" >> "$GATE_COUNT"
if [ "\${n:-0}" -eq 0 ]; then
    printf 'gate: VERDICT=FAIL reason=red test-btl.sh FAILED branch=%s repo=%s suite=test-btl.sh\n' "\$1" "\${2:-?}" >&2
    exit 1
fi
printf 'gate: VERDICT=PASS reason=stub branch=%s repo=%s suite=none\n' "\$1" "\${2:-?}" >&2
exit 0
GSTUB_L
chmod +x "$SH/gate.sh"

printf 'spira/sp-btl-red\n' > "$REPRO_FAIL_FILE"

is "l. positive-control: comment log empty before run" "0" \
    "$([ -s "$COMMENT_LOG" ] && echo 1 || echo 0)"

batch "$REPONAME" >/dev/null 2>&1

want "l. eject comment: names ejected id"    "sp-btl-red"                   "$(cat "$COMMENT_LOG" 2>/dev/null)"
want "l. eject comment: names title"         "Bead that fails the local gate suite" "$(cat "$COMMENT_LOG" 2>/dev/null)"
want "l. eject comment: names suite"         "test-btl.sh"                  "$(cat "$COMMENT_LOG" 2>/dev/null)"
want "l. eject comment: shows remain count"  "2 remain"                     "$(cat "$COMMENT_LOG" 2>/dev/null)"

# Restore default always-pass gate stub.
cat > "$SH/gate.sh" <<GSTUB
#!/usr/bin/env bash
printf '%s\n' "\$1" >> "$GATE_COUNT"
printf 'gate: VERDICT=PASS reason=stub branch=%s repo=%s suite=none\n' "\$1" "\${2:-?}" >&2
exit 0
GSTUB
chmod +x "$SH/gate.sh"
: > "$REPRO_FAIL_FILE"
clean_case

# =============================================================================
# m. CONFLICT-NOTE PATHS: a CERTIFIED branch that conflicts with the new base
#    is reopened with a note naming the specific conflicting file, not a bare
#    "conflicts with $base" sentence.
#
#    POSITIVE CONTROL: the note is read from the bead database after the run;
#    a batch that never called bead_reopen produces an empty note, which
#    would fail the "want" check rather than passing vacuously.
#
#    This must be seen to fail on the UNFIXED tree (batch.sh drops the
#    conflict path list before writing the reopen note).
# =============================================================================
clean_case
seed
NOW="$(date +%s)"; OLD_M=$(( NOW - 1800 - 1 ))

git -C "$REPO" worktree add -q -b "spira/sp-btm" \
    "$RUN/worktree/sp-btm" main 2>/dev/null || true
printf 'branch-ver\n' > "$RUN/worktree/sp-btm/conflict3.txt"
git -C "$RUN/worktree/sp-btm" add -A
git -C "$RUN/worktree/sp-btm" commit -q -m "sp-btm: set conflict3.txt"
tip_m="$(git -C "$REPO" rev-parse "spira/sp-btm")"
printf 'CERTIFIED %s %s\n' "$tip_m" "$OLD_M" > "$LANDSTATE/sp-btm"
plant_bead "sp-btm"

printf 'main-ver\n' > "$REPO/conflict3.txt"
git -C "$REPO" add conflict3.txt
git -C "$REPO" commit -q -m "main: set conflict3.txt"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

batch "$REPONAME" >/dev/null 2>&1

note_m="$(B show sp-btm --json 2>/dev/null \
    | python3 -c "import json,sys;d=json.load(sys.stdin);d=d[0] if isinstance(d,list) else d;print(d.get('notes') or '')" \
    2>/dev/null || true)"
is   "m. positive-control: sp-btm reopened (RED)" "1" \
    "$([ "$(awk '{print $1}' "$LANDSTATE/sp-btm" 2>/dev/null)" = "RED" ] && echo 1 || echo 0)"
want "m. conflict-note: names conflicting file" "conflict3.txt" "$note_m"
clean_case

# =============================================================================
# n. FORMAT BATCH: when the repo declares a formatter, the assembled batch
#    receives a "spira: format batch" commit before the PR opens.
#
#    POSITIVE CONTROL: the format commit must NOT appear on origin/main (only
#    on the batch branch). An empty formatter log means the formatter was
#    bypassed entirely; the grep-c assertion would fail against 0.
# =============================================================================
clean_case
seed
NOW="$(date +%s)"; OLD_N=$(( NOW - 1800 - 1 ))
branch "sp-btn-a" "$OLD_N"
branch "sp-btn-b" "$OLD_N"

FMT_SCRIPT_N="$TMP/fmt-n.sh"
cat > "$FMT_SCRIPT_N" << 'FMTN'
#!/usr/bin/env bash
for f in *.txt; do [ -f "$f" ] && printf 'fmt\n' >> "$f"; done
FMTN
chmod +x "$FMT_SCRIPT_N"

cat > "$SH/repo-map" << RMAP_N
$REPONAME | $REPO | queue | origin/main | bash $FMT_SCRIPT_N | |
RMAP_N

batch "$REPONAME" > /dev/null

batch_br_n="$(git -C "$REPO" for-each-ref --format='%(refname:short)' \
    'refs/heads/spira/queue/*' 2>/dev/null | tail -1)"
is "n. format batch: format commit present" "1" \
    "$(git -C "$REPO" log --format='%s' "origin/main..$batch_br_n" 2>/dev/null \
       | grep -c 'spira: format batch' || true)"
is "n. format batch: PR opened" "1" "$(batch_pr)"
is "n. format batch: no format commit on main" "0" \
    "$(git -C "$REPO" log --format='%s' origin/main 2>/dev/null \
       | grep -c 'spira: format batch' || true)"

cat > "$SH/repo-map" << RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP
clean_case

# =============================================================================
# o. FORMATTER FAILURE: when the formatter exits non-zero, the batch opens
#    without a format commit and nothing is changed.
#
#    POSITIVE CONTROL: a formatter that exits 0 but makes no changes would
#    also produce no format commit; the test distinguishes the two paths by
#    asserting the PR IS opened (the batch was not aborted on formatter failure).
# =============================================================================
clean_case
seed
NOW="$(date +%s)"; OLD_O=$(( NOW - 1800 - 1 ))
branch "sp-bto-a" "$OLD_O"

FMT_FAIL_N="$TMP/fmt-fail-o.sh"
printf '#!/usr/bin/env bash\nexit 1\n' > "$FMT_FAIL_N"
chmod +x "$FMT_FAIL_N"

cat > "$SH/repo-map" << RMAP_O
$REPONAME | $REPO | queue | origin/main | bash $FMT_FAIL_N | |
RMAP_O

batch "$REPONAME" > /dev/null

batch_br_o="$(git -C "$REPO" for-each-ref --format='%(refname:short)' \
    'refs/heads/spira/queue/*' 2>/dev/null | tail -1)"
is "o. formatter fail: PR still opened"          "1" "$(batch_pr)"
is "o. formatter fail: no format commit present" "0" \
    "$(git -C "$REPO" log --format='%s' "origin/main..$batch_br_o" 2>/dev/null \
       | grep -c 'spira: format batch' || true)"

cat > "$SH/repo-map" << RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP
clean_case

# =============================================================================
# p. BISECT FORCED CUT (sp-y931m): a bisect state file records the half a red,
#    unattributable batch (e.g. a build failure) narrowed to. The next cut must
#    be exactly that half, even though an unrelated P0 branch is also certified
#    and would otherwise sort first — a fresh priority cut re-admitting the
#    branch under isolation is exactly how PRs 302-304 looped forever.
#
#    POSITIVE CONTROL: without the fix, queue_sort_rows always wins and the P0
#    branch (sp-btp-hi) is cut instead, so this assertion fails against
#    unfixed batch.sh rather than passing vacuously.
# =============================================================================
clean_case
seed
NOW="$(date +%s)"
branch_p "sp-btp-hi" 0 "$NOW"
branch_p "sp-btp-lo" 4 "$NOW"
lo_tip="$(tip_of sp-btp-lo)"
mkdir -p "$QUEUEDIR/$REPONAME"
printf '%s:%s\n' "sp-btp-lo" "$lo_tip" > "$(bisect_file)"

out_p="$(batch "$REPONAME")"
is   "p. bisect forced: PR opened"          "1" "$(batch_pr)"
is   "p. bisect forced: lo is BATCHED"      "1" "$(is_batched sp-btp-lo && echo 1 || echo 0)"
is   "p. bisect forced: hi stays CERTIFIED" "1" "$(is_certified sp-btp-hi && echo 1 || echo 0)"
want "p. bisect forced: log names the forced cut" "forcing cut to recorded half" "$out_p"
clean_case

# =============================================================================
# q. STALE BISECT GROUP: the recorded group names a member that is no longer
#    CERTIFIED (ejected or landed by some other path). The forced cut is empty,
#    so batch.sh drops the state and falls through to a normal priority cut
#    instead of forcing an empty or stale batch forever.
# =============================================================================
clean_case
seed
NOW="$(date +%s)"; OLD_Q=$(( NOW - 1800 - 1 ))
branch_p "sp-btq-a" 4 "$OLD_Q"
mkdir -p "$QUEUEDIR/$REPONAME"
printf '%s:%s\n' "sp-btq-ghost" "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef" > "$(bisect_file)"

out_q="$(batch "$REPONAME")"
is   "q. stale bisect: state file dropped" "0" "$([ -f "$(bisect_file)" ] && echo 1 || echo 0)"
is   "q. stale bisect: real certified branch still batched" "1" "$(batch_pr)"
want "q. stale bisect: log reports advance" "bisect group already resolved elsewhere" "$out_q"
clean_case

# =============================================================================
# r. REBASE HANDED TO LANDING PASS (sp-5yw3b): a CERTIFIED branch whose tip no
#    longer merges onto the moved base, but whose commits rebase cleanly onto
#    it, is never suite-gated inline under the queue lock. batch.sh moves the
#    ref to the rebased tip, records RED at the OLD tip (conflicts-with-base),
#    and leaves it for the landing pass — which treats a stale RED record as
#    never-gated (spira/landing.sh CHECK6) and re-certifies it in parallel.
#
#    The certified tip carries two commits: the first duplicates a change main
#    also picked up independently (same file, same final content, different
#    commit), so a plain merge --no-ff conflicts; git rebase drops that first
#    commit as already-upstream and applies the second cleanly. That is a real
#    git divergence between the two operations, not a stubbed one.
#
#    POSITIVE CONTROL for "repro seam never called": the log is primed with a
#    direct stub invocation first, proving it records calls when they happen,
#    then cleared — so a silent log after the real run means nothing ran, not
#    that nothing could have been recorded.
#
#    sp-btr's own worktree (created above to build its commits) is left
#    checked out on spira/sp-btr for the rest of this case — the same shape
#    as an aeon's live worktree for the bead being batched. `git branch -f`
#    refuses to move a ref checked out anywhere, so this branch also proves
#    the ref-move survives that.
# =============================================================================
clean_case
seed
NOW="$(date +%s)"; OLD_R=$(( NOW - 1800 - 1 ))

# shared.txt exists before the branch forks, so both sides modify one blob
# instead of independently adding it.
printf 'base\n' > "$REPO/shared.txt"
git -C "$REPO" add shared.txt
git -C "$REPO" commit -q -m "main: add shared.txt"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

git -C "$REPO" worktree add -q -b "spira/sp-btr" \
    "$RUN/worktree/sp-btr" main 2>/dev/null || true
printf 'base\nshared\n' > "$RUN/worktree/sp-btr/shared.txt"
git -C "$RUN/worktree/sp-btr" add -A
git -C "$RUN/worktree/sp-btr" commit -q -m "sp-btr: add shared line"
printf 'base\nshared\nbranch-only\n' > "$RUN/worktree/sp-btr/shared.txt"
git -C "$RUN/worktree/sp-btr" add -A
git -C "$RUN/worktree/sp-btr" commit -q -m "sp-btr: add branch-only line"
tip_r="$(git -C "$REPO" rev-parse "spira/sp-btr")"
printf 'CERTIFIED %s %s\n' "$tip_r" "$OLD_R" > "$LANDSTATE/sp-btr"
plant_bead "sp-btr"

# Advance main with sp-btr's first commit's exact change, made independently
# (e.g. another bead touched the same shared line).
printf 'base\nshared\n' > "$REPO/shared.txt"
git -C "$REPO" add shared.txt
git -C "$REPO" commit -q -m "main: add shared line independently"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

bash "$SH/repro-stub.sh" "spira/sp-btr" >/dev/null 2>&1 || true
is   "r. positive control: repro log records a direct call" "1" \
    "$(grep -c '^spira/sp-btr$' "$REPRO_CALL_LOG" 2>/dev/null || echo 0)"
: > "$REPRO_CALL_LOG"

out_r="$(batch "$REPONAME" 2>&1)"

is   "r. rebase-handoff: repro seam never called"      "0" \
    "$(grep -c '^spira/sp-btr$' "$REPRO_CALL_LOG" 2>/dev/null)"
is   "r. rebase-handoff: landstate is RED"             "RED" \
    "$(awk '{print $1}' "$LANDSTATE/sp-btr" 2>/dev/null)"
is   "r. rebase-handoff: landstate tip is the OLD tip" "$tip_r" \
    "$(awk '{print $2}' "$LANDSTATE/sp-btr" 2>/dev/null)"
new_tip_r="$(git -C "$REPO" rev-parse "spira/sp-btr" 2>/dev/null)"
is   "r. rebase-handoff: branch ref moved to a new tip" "1" \
    "$([ -n "$new_tip_r" ] && [ "$new_tip_r" != "$tip_r" ] && echo 1 || echo 0)"
want "r. rebase-handoff: no PR opened"                 "QUEUE NOCUT" "$(landing_log)"
want "r. rebase-handoff: log names the handoff"        "handed to the landing pass" "$out_r"

# =============================================================================
# s. EXPRESS TAKES A SLOT (sp-ebx8b): BATCH_MAX older certified P0s plus one
#    newer express P0 — the cut must include the express bead rather than leave
#    it behind the cap on age alone.
#
#    POSITIVE CONTROL: the express branch has the NEWEST epoch of the nine,
#    tied on priority with the rest. Without the express-first sort key it is
#    the one branch age would exclude at BATCH_MAX; this assertion fails
#    against unfixed batch.sh rather than passing vacuously.
# =============================================================================
clean_case
seed
NOW="$(date +%s)"
for i in $(seq 1 8); do
    branch_p "sp-btr-old$i" 0 "$(( NOW - 900 + i * 100 ))"
done
branch_p_express "sp-btr-express" 0 "$NOW"

batch "$REPONAME" > /dev/null
is "s. express+cap: PR opened" "1" "$(batch_pr)"
is "s. express+cap: express branch BATCHED" "1" \
    "$(is_batched sp-btr-express && echo 1 || echo 0)"
is "s. express+cap: newest old branch stays CERTIFIED" "1" \
    "$(is_certified sp-btr-old8 && echo 1 || echo 0)"
is "s. express+cap: exactly 7 of the 8 old branches batched" "7" \
    "$(for i in $(seq 1 8); do is_batched "sp-btr-old$i" && echo x; done | grep -c x)"
clean_case

# =============================================================================
# t. EXPRESS GROUP KEEPS ITS OWN ORDER: two express beads still sort by the
#    existing key (priority, then epoch) between themselves — the express flag
#    only changes ranking against non-express beads, not the tie-break inside
#    the express set.
# =============================================================================
clean_case
seed
NOW="$(date +%s)"
branch_p_express "sp-bts-hi" 0 "$NOW"
branch_p_express "sp-bts-lo" 4 "$(( NOW - 100 ))"

batch "$REPONAME" > /dev/null
batch_br_s="$(git -C "$REPO" for-each-ref --format='%(refname:short)' \
    'refs/heads/spira/queue/*' 2>/dev/null | tail -1)"
is "t. two express: PR opened" "1" "$(batch_pr)"
first_merged_s="$(git -C "$REPO" log --format='%s' "origin/main..$batch_br_s" \
    | grep '^spira: land sp-bts-' | tail -1)"
want "t. higher-priority express lands before lower-priority express" \
    "sp-bts-hi" "$first_merged_s"
clean_case

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
