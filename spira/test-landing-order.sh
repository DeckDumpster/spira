#!/usr/bin/env bash
#
# test-landing-order.sh — the landing pass certifies closed branches in
# (priority ASC, closed_at ASC) order, not refname order, so late-alphabet
# high-priority branches cannot starve when a budget cut ends the pass early.
#
# THREE PROPERTIES UNDER TEST:
#
#   1. ORDER. With four closed branches spanning two priority tiers and two
#      close dates each, a pass with no budget constraint certifies them
#      oldest-first within each tier: P1-oldest, P1-newer, P2-oldest, P2-newer.
#
#   2. BUDGET CUT LOG. With a budget that expires before any gate runs
#      (LAND_MAXSEC=1, RESERVE=2), the pass logs the cut once, names the
#      first deferred branch, and reports the count of unvisited branches.
#      The positive control: with no budget constraint the same branches
#      all certify and no cut message appears.
#
#   3. DISJOINT PASSES. After pass 1 certifies the two P1 branches,
#      pass 2 (with the P1 branches now submitted/skipped) certifies
#      the two P2 branches — the passes cover disjoint sets.
#
# The gate stub is instant (no sleep). Budget tests use LAND_MAXSEC=1 with
# RESERVE=2, which makes gate_fits return false before the very first gate
# call (1-0=1 < 2). This is deterministic without wall-clock timing.
#
# defect: sp-fm3rl
# covers: spira/landing.sh spira/lib.sh
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
before() {  # before <label> <a> <b> <text> — verify <a> appears before <b>
    local la lb
    la=$(printf '%s\n' "$4" | grep -n "$2" | head -1 | cut -d: -f1)
    lb=$(printf '%s\n' "$4" | grep -n "$3" | head -1 | cut -d: -f1)
    [ -n "$la" ] && [ -n "$lb" ] && [ "$la" -lt "$lb" ] \
        && ok "$1" \
        || bad "$1" "[$2] (line ${la:--}) not before [$3] (line ${lb:--})"
}

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-landing-order
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
export SPIRA_TESTDB_MODE=server
testdb_up landing-order || {
    printf 'SKIP test-landing-order: server testdb not available\n' >&2
    exit 77
}
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"
REPONAME=fixture-repo
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH"

cp "$HERE"/*.sh "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub confine.sh 'exit 0'
stub mail.sh '[ "${1:-}" = send ] || exit 0; printf "%s\n" "$*" >> "$EMITTED"; cat >> "$EMITTED"; printf "\n" >> "$EMITTED"'
export EMITTED="$TMP/events"; : > "$EMITTED"
stub gate.sh 'echo "gate: VERDICT=PASS reason=stub branch=$1 repo=${2:-?}" >&2; exit 0'
stub gh 'exit 1'
stub queue.sh 'exit 0'

cat > "$SH/repo-map" <<MAP
$REPONAME | $REPO | queue | |
MAP

B()  { bd -C "$SPIRA_DB" "$@"; }
status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }

landing() {
    rm -f "$RUN/landing.progress"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" SPIRA_ID_PREFIX=sp \
    SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/gh" \
        bash "$SH/landing.sh" 2>&1
}

landing_tight() {
    rm -f "$RUN/landing.progress"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" SPIRA_ID_PREFIX=sp \
    SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/gh" \
    SPIRA_LAND_MAXSEC=1 SPIRA_LAND_GATE_RESERVE=2 \
        bash "$SH/landing.sh" 2>&1
}

# branch_at <id> <priority> <closed_at> — a closed bead with a git branch
branch_at() {
    local id="$1" pri="$2" cat="$3"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$id" > "$RUN/worktree/$id/$id.txt"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id — work"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","priority":%d,"labels":[],"updated_at":"%s","closed_at":"%s","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$id" "$id" "$pri" "$cat" "$cat" "$id" | testdb_seed
}

drop_branch() {
    local id="$1"
    git -C "$REPO" worktree remove --force "$RUN/worktree/$id" >/dev/null 2>&1 || true
    git -C "$REPO" branch -D "spira/$id" >/dev/null 2>&1 || true
}

seed_order() {
    testdb_reset
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-01T00:00:00Z"}
JSONL
    # Four branches: two P1 and two P2, each pair spanning two close dates.
    # Expected sort: sp-ord-d (P1,oldest), sp-ord-b (P1,newer),
    #                sp-ord-a (P2,oldest), sp-ord-c (P2,newest)
    # Refname order would be: sp-ord-a, sp-ord-b, sp-ord-c, sp-ord-d (alphabetical)
    branch_at sp-ord-d 1 "2026-09-01T00:00:00Z"   # P1, oldest
    branch_at sp-ord-b 1 "2026-09-02T00:00:00Z"   # P1, newer
    branch_at sp-ord-a 2 "2026-09-01T00:00:00Z"   # P2, oldest
    branch_at sp-ord-c 2 "2026-09-03T00:00:00Z"   # P2, newest
}

echo "test-landing-order.sh"

# -----------------------------------------------------------------------
# POSITIVE CONTROL: with no budget constraint, all four branches certify
# in one pass. Without this, every silence below is vacuous.
# -----------------------------------------------------------------------
echo
echo "order and full-budget tests:"
seed_order
out="$(landing)"
want "pass certifies oldest P1 first (sp-ord-d)" "certified spira/sp-ord-d" "$out"
want "pass certifies newer P1 second (sp-ord-b)" "certified spira/sp-ord-b" "$out"
want "pass certifies oldest P2 third (sp-ord-a)"  "certified spira/sp-ord-a"  "$out"
want "pass certifies newest P2 last (sp-ord-c)"  "certified spira/sp-ord-c"  "$out"

# ORDER: the four branches must appear in (priority ASC, closed_at ASC) order,
# not refname order. refname order would put sp-ord-a before sp-ord-d.
before "P1-oldest before P1-newer"   "certified spira/sp-ord-d" "certified spira/sp-ord-b" "$out"
before "P1-newer before P2-oldest"   "certified spira/sp-ord-b" "certified spira/sp-ord-a" "$out"
before "P2-oldest before P2-newest"  "certified spira/sp-ord-a" "certified spira/sp-ord-c" "$out"
# Discriminating test: sp-ord-d (P1) must come before sp-ord-a (P2) even though
# 'a' < 'd' in refname order. This is the case that proves sorting beats alphabet.
before "P1 branch before same-date P2 branch" "certified spira/sp-ord-d" "certified spira/sp-ord-a" "$out"

nowant "no budget-cut message on a full pass" "budget cut" "$out"

drop_branch sp-ord-d; drop_branch sp-ord-b; drop_branch sp-ord-a; drop_branch sp-ord-c

# -----------------------------------------------------------------------
# BUDGET CUT: with LAND_MAXSEC=1 and RESERVE=2, gate_fits returns false
# before the first gate call (1-0=1 < 2). The pass must log the cut once,
# name the first branch in sorted order (sp-ord-d), and count all four
# as unvisited. Positive control: the full-budget test above showed all
# four certify when budget is not tight.
# -----------------------------------------------------------------------
echo
echo "budget cut test:"
seed_order
out="$(landing_tight)"
want "tight-budget pass logs a cut" "budget cut at" "$out"
want "cut names the first sorted branch" "budget cut at spira/sp-ord-d" "$out"
want "cut reports unvisited count" "4 branch(es) deferred" "$out"
nowant "tight-budget pass does not certify any branch" "certified" "$out"

drop_branch sp-ord-d; drop_branch sp-ord-b; drop_branch sp-ord-a; drop_branch sp-ord-c

# -----------------------------------------------------------------------
# DISJOINT PASSES: pass 1 (no budget limit) certifies only the two P1
# branches; pass 2 certifies the two P2 branches. The disjoint property
# is tested by running pass 1 on all four, then removing the two P1 beads
# from the pool (drop their branches — they are now "certified" and
# their submitted markers cause the loop to skip them) and running pass 2.
#
# This mirrors the real invariant: a branch certified in pass 1 does not
# occupy budget in pass 2, so the next-oldest set always gets its turn.
# -----------------------------------------------------------------------
echo
echo "disjoint-passes test:"
seed_order
# Pass 1: certify only P1 branches by withholding the gate on P2.
# The gate stub returns PASS for all; the submitted-file mechanism is what
# prevents recertification. We run pass 1 with budget that fits only 2 gates.
# Rather than timing, we run two separate landing() calls — first with only
# the P1 branches present, then add P2 branches for pass 2.
# This directly tests the "each pass picks up where the last left off" shape.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-01T00:00:00Z"}
JSONL
branch_at sp-ord-d 1 "2026-09-01T00:00:00Z"
branch_at sp-ord-b 1 "2026-09-02T00:00:00Z"
out1="$(landing)"
want "pass 1 certifies sp-ord-d (P1 oldest)" "certified spira/sp-ord-d" "$out1"
want "pass 1 certifies sp-ord-b (P1 newer)"  "certified spira/sp-ord-b" "$out1"

# Add P2 branches; their beads are closed so pass 2 will pick them up.
branch_at sp-ord-a 2 "2026-09-01T00:00:00Z"
branch_at sp-ord-c 2 "2026-09-03T00:00:00Z"
out2="$(landing)"
want "pass 2 certifies sp-ord-a (P2 oldest)" "certified spira/sp-ord-a" "$out2"
want "pass 2 certifies sp-ord-c (P2 newest)" "certified spira/sp-ord-c" "$out2"
# sp-ord-d and sp-ord-b are submitted; pass 2 skips them (submitted check).
nowant "pass 2 does not re-certify sp-ord-d" "certified spira/sp-ord-d" "$out2"
nowant "pass 2 does not re-certify sp-ord-b" "certified spira/sp-ord-b" "$out2"
before "pass 2 certifies P2-oldest before P2-newest" "certified spira/sp-ord-a" "certified spira/sp-ord-c" "$out2"

drop_branch sp-ord-d; drop_branch sp-ord-b; drop_branch sp-ord-a; drop_branch sp-ord-c

echo
printf 'results: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
