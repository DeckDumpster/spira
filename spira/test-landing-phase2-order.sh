#!/usr/bin/env bash
#
# test-landing-phase2-order.sh — Phase 2 tier ordering and RED-skip.
#
# PROPERTIES UNDER TEST:
#
#   1. TIER ORDER. With three closed branches — one never-gated (no prior landstate),
#      one RED with an unchanged tip, one RED with a moved tip — Phase 2 certifies both
#      the never-gated branch and the moved-tip RED branch first (both are tier 0:
#      stale RED is treated as never-gated), and skips the same-tip RED branch entirely.
#      This holds even when the same-tip RED branch would sort first by priority
#      (it is P1; the others are P2).
#
#   2. SKIP LOG. The same-tip RED branch produces a CHECK6 line naming the RED record
#      and the branch is not dispatched to the gate.
#
#   3. POSITIVE CONTROL. With the RED tip moved (add a commit to the previously-skipped
#      branch), Phase 2 dispatches it.
#
#   4. CONFLICTS-WITH-BASE STALE BASE. A RED conflicts-with-base record whose base has
#      advanced since the record was written is treated as never-gated (the record
#      describes a pair, and either side moving makes it stale).
#
# The gate stub is instant. SPIRA_CERTIFY_PAR=2 forces the parallel path (Phase 2)
# even on a single-core host.
#
# defect: sp-fpf62 sp-npggh
# covers: spira/landing.sh spira/lib.sh
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
before() {
    local la lb
    la=$(printf '%s\n' "$4" | grep -n "$2" | head -1 | cut -d: -f1)
    lb=$(printf '%s\n' "$4" | grep -n "$3" | head -1 | cut -d: -f1)
    [ -n "$la" ] && [ -n "$lb" ] && [ "$la" -lt "$lb" ] \
        && ok "$1" \
        || bad "$1" "[$2] (line ${la:--}) not before [$3] (line ${lb:--})"
}

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-landing-phase2-order
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up landing-phase2-order || {
    printf 'SKIP test-landing-phase2-order: no testdb available\n' >&2
    exit 77
}
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"
LANDSTATE="$RUN/landstate"
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

landing() {
    rm -f "$RUN/landing.progress"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$REPONAME" SPIRA_ID_PREFIX=sp \
    SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/gh" \
    SPIRA_CERTIFY_PAR=2 \
        bash "$SH/landing.sh" 2>&1
}

seed_base() {
    testdb_reset
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-01T00:00:00Z"}
JSONL
}

branch_at() {   # branch_at <id> <priority> <closed_at>
    local id="$1" pri="$2" cat="$3"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$id" > "$RUN/worktree/$id/$id.txt"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","priority":%d,"labels":[],"updated_at":"%s","closed_at":"%s","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$id" "$id" "$pri" "$cat" "$cat" "$id" | testdb_seed
}

drop_branch() {
    local id="$1"
    git -C "$REPO" worktree remove --force "$RUN/worktree/$id" >/dev/null 2>&1 || true
    git -C "$REPO" branch -D "spira/$id" >/dev/null 2>&1 || true
}

plant_red() {   # plant_red <id> <tip> <reason>
    mkdir -p "$LANDSTATE"
    printf 'RED %s %s %s' "$2" "$(date +%s)" "$3" > "$LANDSTATE/$1"
}

echo "test-landing-phase2-order.sh"

# -----------------------------------------------------------------------
# SETUP. Three branches:
#   sp-p2-ng  P2 never-gated  (no landstate)
#   sp-p2-rs  P1 RED same-tip (tip will not change during rebase)
#   sp-p2-rm  P2 RED moved-tip (landstate records an older SHA)
#
# Without tier ordering, brs sort gives: sp-p2-rs (P1) first, then
# sp-p2-ng and sp-p2-rm (P2, oldest first). The same-tip RED would
# consume the first slot. With tier ordering, sp-p2-ng and sp-p2-rm
# are both treated as never-gated (ng has no record; rm has a stale
# record because its tip moved), sp-p2-rs is skipped. Within tier 0,
# ng (closed 2026-09-01) sorts before rm (closed 2026-09-02).
# -----------------------------------------------------------------------

echo
echo "tier-order and RED-skip tests:"
seed_base
branch_at sp-p2-ng 2 "2026-09-01T00:00:00Z"
branch_at sp-p2-rs 1 "2026-09-01T00:00:00Z"
branch_at sp-p2-rm 2 "2026-09-02T00:00:00Z"

# sp-p2-rs: plant RED with current tip (rebase is a no-op, tip stays same)
tip_rs="$(git -C "$REPO" rev-parse "spira/sp-p2-rs")"
plant_red sp-p2-rs "$tip_rs" gate

# sp-p2-rm: plant RED with a stale SHA (tip has "moved" from landstate's view)
plant_red sp-p2-rm "0000000000000000000000000000000000000000" gate

out="$(landing)"

# Never-gated branch certifies.
want "never-gated branch certifies" "certified spira/sp-p2-ng" "$out"

# Same-tip RED branch is skipped with CHECK6 line.
want "same-tip RED produces CHECK6 skip line" "CHECK6 sp-p2-rs: tip unchanged since RED mark" "$out"
nowant "same-tip RED branch not certified"    "certified spira/sp-p2-ng\|certified spira/sp-p2-rs" "$out"
# (avoid literal pipe in test — separate assertion)
nowant "same-tip RED branch not certified (direct)" "certified spira/sp-p2-rs" "$out"

# Moved-tip RED branch is promoted to never-gated and certifies.
want "moved-tip RED branch certifies" "certified spira/sp-p2-rm" "$out"
want "stale-tip RED produces promotion log" "RED record tip stale" "$out"

# Tier log line is present and names the correct counts.
# Both ng (no landstate) and rm (stale RED) are tier 0.
want "tier log line present" "certify phase 2 tiers in $REPONAME" "$out"
want "tier log shows never-gated=2" "never-gated=2" "$out"
want "tier log shows skipped=1" "skipped=1" "$out"

# Order: never-gated before moved-tip-RED (both tier 0; ng closed 2026-09-01 < rm 2026-09-02).
before "never-gated certifies before moved-tip RED" \
    "certified spira/sp-p2-ng" "certified spira/sp-p2-rm" "$out"

drop_branch sp-p2-ng; drop_branch sp-p2-rs; drop_branch sp-p2-rm
rm -f "$LANDSTATE/sp-p2-ng" "$LANDSTATE/sp-p2-rs" "$LANDSTATE/sp-p2-rm"

# -----------------------------------------------------------------------
# POSITIVE CONTROL: moving the tip of the previously-skipped branch
# makes it eligible for dispatch again.
# -----------------------------------------------------------------------
echo
echo "positive control: moved tip dispatched:"
seed_base
branch_at sp-p2-ng 2 "2026-09-01T00:00:00Z"
branch_at sp-p2-rs 1 "2026-09-01T00:00:00Z"

# Plant RED with current tip, then add a commit (tip moves).
tip_rs_old="$(git -C "$REPO" rev-parse "spira/sp-p2-rs")"
plant_red sp-p2-rs "$tip_rs_old" gate
printf 'fix: sp-p2-rs retry\n' > "$RUN/worktree/sp-p2-rs/retry.txt"
git -C "$RUN/worktree/sp-p2-rs" add -A
git -C "$RUN/worktree/sp-p2-rs" commit -q -m "fix: sp-p2-rs retry"

out2="$(landing)"
want "moved tip is dispatched (certifies)"       "certified spira/sp-p2-rs" "$out2"
want "stale-tip promotion log present"           "RED record tip stale" "$out2"
nowant "skip line absent after tip moved"        "tip unchanged since RED mark" "$out2"

drop_branch sp-p2-ng; drop_branch sp-p2-rs
rm -f "$LANDSTATE/sp-p2-ng" "$LANDSTATE/sp-p2-rs"

# -----------------------------------------------------------------------
# CONFLICTS-WITH-BASE STALE BASE. A RED conflicts-with-base record whose
# timestamp is older than the latest commit on the base is stale: main has
# moved after the conflict was recorded, so the record no longer describes
# the current pair. Treat the branch as never-gated.
# -----------------------------------------------------------------------
echo
echo "conflicts-with-base stale-base tests:"
seed_base
branch_at sp-p2-cwb 2 "2026-09-01T00:00:00Z"

# Plant a conflicts-with-base RED with a timestamp in the past, before
# adding a new commit to main — so the base commit is newer than the record.
tip_cwb="$(git -C "$REPO" rev-parse "spira/sp-p2-cwb")"
past_epoch=$(( $(date +%s) - 3600 ))
mkdir -p "$LANDSTATE"
printf 'RED %s %s conflicts-with-base' "$tip_cwb" "$past_epoch" > "$LANDSTATE/sp-p2-cwb"

# Advance the base (main) past the record timestamp.
git -C "$REPO" checkout -q main
git -C "$REPO" commit -q --allow-empty -m "advance main"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

out3="$(landing)"
want "cwb stale-base branch certifies" "certified spira/sp-p2-cwb" "$out3"
want "cwb stale-base produces promotion log" "RED conflicts-with-base stale" "$out3"
nowant "cwb stale-base no skip line" "tip unchanged since RED mark" "$out3"

# -----------------------------------------------------------------------
# CONFLICTS-WITH-BASE CURRENT BASE. Same-tip conflicts-with-base RED whose
# base has NOT advanced since the record is still current — skip it.
# -----------------------------------------------------------------------
echo
echo "conflicts-with-base current-base (skip) tests:"
seed_base
branch_at sp-p2-cwb2 2 "2026-09-01T00:00:00Z"

tip_cwb2="$(git -C "$REPO" rev-parse "spira/sp-p2-cwb2")"
# Timestamp in the future relative to the base's latest commit — base has not advanced.
future_epoch=$(( $(date +%s) + 3600 ))
printf 'RED %s %s conflicts-with-base' "$tip_cwb2" "$future_epoch" > "$LANDSTATE/sp-p2-cwb2"

out4="$(landing)"
nowant "cwb current-base branch not certified" "certified spira/sp-p2-cwb2" "$out4"
want "cwb current-base produces skip line" "tip unchanged since RED mark" "$out4"

drop_branch sp-p2-cwb; drop_branch sp-p2-cwb2
rm -f "$LANDSTATE/sp-p2-cwb" "$LANDSTATE/sp-p2-cwb2"

echo
printf 'results: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
