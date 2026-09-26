#!/usr/bin/env bash
#
# test-batch-bisect-expiry.sh — a bisect group only forces a cut while it still
#   describes the commits it was split from (sp-55j4m).
#
# THE PROPERTY UNDER TEST. A bisect record named its members by bead id only, with
# no way to tell whether the split it recorded still described anything real. A
# group split 13 hours earlier, left over from a landed PR, forced a stale cut
# over nine certified P0s (batch 327, 2026-09-25) — nothing downstream re-checked
# that the recorded half was still worth cutting.
#
# THREE CASES:
#
#   p. fresh group      base and every member tip still match what was recorded
#                        → the group still forces its cut (sp-y931m not regressed)
#   u. base moved        an unrelated batch landed since the split → group
#                        discarded, priority winner cut instead, reason logged
#   v. member tip moved  a recorded member was rebuilt and re-certified since the
#                        split → group discarded, priority winner cut instead,
#                        reason logged
#
# POSITIVE CONTROL is case p: without it, an assertion that the stale group in
# u/v gets discarded would pass just as well against a batch.sh that ignores
# every bisect record regardless of content.
#
# tier: T1
# covers: spira/batch.sh spira/lib.sh spira/verdict.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-batch-bisect-expiry
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up bisectexpiry || { echo "test-batch-bisect-expiry: could not build fixture database"; exit 1; }
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

cat > "$SH/mail.sh" <<'MAIL'
#!/usr/bin/env bash
exit 0
MAIL
chmod +x "$SH/mail.sh"

# Forge fixture: log every pr-create call, return incrementing PR numbers. An
# unrecognised command (e.g. pr-list-queue) exits 1 with empty stdout, which
# every caller here already treats as "nothing".
FORGE_LOG="$TMP/forge-log"
cat > "$SH/forge-fixture.sh" << FORGE
#!/usr/bin/env bash
cmd="\${1:-}"; shift; repo="\${1:-}"; shift
case "\$cmd" in
    main-gate-status) printf 'green deadbeef\n' ;;
    pr-create)
        n=\$(( \$(wc -l < "$FORGE_LOG" 2>/dev/null || echo 0) + 1 ))
        cat >/dev/null
        printf '%s\n' "\$n" >> "$FORGE_LOG"
        printf '%s\n' "\$n"
        ;;
    *) printf 'forge-fixture: unknown command: %s\n' "\$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"
: > "$FORGE_LOG"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

batch() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
    SPIRA_QUEUE_LOCAL_GATE=0 \
        bash "$SH/batch.sh" "$@" 2>&1
}

seed() {
    testdb_reset
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
}

# branch_p <id> <priority> [epoch] — branch with an explicit bead priority, so
# a test can prove the priority cut, not the bisect group, made the choice.
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

tip_of()       { git -C "$REPO" rev-parse "spira/$1" 2>/dev/null; }
bisect_file()  { printf '%s/%s/bisect' "$QUEUEDIR" "$REPONAME"; }
batch_pr()     { grep '^pr=' "$QUEUEDIR/$REPONAME/open" 2>/dev/null | cut -d= -f2; }
is_batched()   { grep -q '^BATCHED' "$LANDSTATE/${1:-}" 2>/dev/null; }
is_certified() { awk '{print $1}' "$LANDSTATE/${1:-}" 2>/dev/null | grep -q '^CERTIFIED$'; }

clean_case() {
    rm -f "$QUEUEDIR/$REPONAME/open" "$QUEUEDIR/$REPONAME/bisect"
    rm -f "$RUN/landing.log"
    : > "$FORGE_LOG"
    find "$LANDSTATE" -maxdepth 1 -type f 2>/dev/null -delete
    local wt="$RUN/worktree/.batch-$(basename "$REPO")"
    [ -d "$wt" ] && git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    rm -rf "$RUN/worktree" && mkdir -p "$RUN/worktree"
    git -C "$REPO" worktree prune 2>/dev/null || true
    git -C "$REPO" for-each-ref --format='%(refname:short)' 'refs/heads/spira/*' 2>/dev/null \
        | while read -r br; do git -C "$REPO" branch -D "$br" 2>/dev/null || true; done
}

echo "test-batch-bisect-expiry.sh"

# =============================================================================
# p. FRESH GROUP STILL FORCES ITS CUT (sp-y931m, positive control): base and
#    member tip both still match what queue_bisect_split recorded, so the
#    recorded half is cut even though an unrelated P0 branch is also certified
#    and would otherwise sort first.
# =============================================================================
seed
NOW="$(date +%s)"
branch_p "sp-btp-hi" 0 "$NOW"
branch_p "sp-btp-lo" 4 "$NOW"
lo_tip_p="$(tip_of sp-btp-lo)"
base_sha_p="$(git -C "$REPO" rev-parse origin/main)"
printf '%s %s:%s\n' "$base_sha_p" "sp-btp-lo" "$lo_tip_p" > "$(bisect_file)"

out_p="$(batch "$REPONAME")"
is   "p. fresh group: PR opened"           "1" "$(batch_pr)"
is   "p. fresh group: lo is BATCHED"       "1" "$(is_batched sp-btp-lo && echo 1 || echo 0)"
is   "p. fresh group: hi stays CERTIFIED"  "1" "$(is_certified sp-btp-hi && echo 1 || echo 0)"
want "p. fresh group: log names the forced cut" "forcing cut to recorded half" "$out_p"
clean_case

# =============================================================================
# u. GROUP EXPIRES WHEN THE BASE MOVES: an unrelated batch lands since the
#    split was written, so the recorded base no longer names the branch the
#    next cut would land onto. The group must be discarded before it can force
#    a cut, its discard logged, and the priority winner cut in its place.
# =============================================================================
seed
NOW="$(date +%s)"; OLD_U=$(( NOW - 1800 - 1 ))
branch_p "sp-btu-hi" 0 "$OLD_U"
branch_p "sp-btu-lo" 4 "$OLD_U"
lo_tip_u="$(tip_of sp-btu-lo)"
stale_base_u="$(git -C "$REPO" rev-parse origin/main)"
printf '%s %s:%s\n' "$stale_base_u" "sp-btu-lo" "$lo_tip_u" > "$(bisect_file)"

git -C "$REPO" commit -q --allow-empty -m "unrelated landing"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

out_u="$(batch "$REPONAME")"
is   "u. base moved: state file dropped" "0" "$([ -f "$(bisect_file)" ] && echo 1 || echo 0)"
is   "u. base moved: priority winner batched instead of the stale group" "1" \
    "$(is_batched sp-btu-hi && echo 1 || echo 0)"
want "u. base moved: discard is logged with its reason" "base moved" "$out_u"
clean_case
git -C "$REPO" fetch -q origin

# =============================================================================
# v. GROUP EXPIRES WHEN A MEMBER'S TIP MOVES: the recorded member was rebuilt
#    (a new commit, re-certified) since the split. The record no longer names
#    the commit the bisect reasoned about, so forcing it would re-test
#    something unrelated to the original break (exactly what cost a full CI
#    run on two P3s in sp-55j4m).
# =============================================================================
seed
NOW="$(date +%s)"; OLD_V=$(( NOW - 1800 - 1 ))
branch_p "sp-btv-hi" 0 "$OLD_V"
branch_p "sp-btv-lo" 4 "$OLD_V"
stale_tip_v="$(tip_of sp-btv-lo)"
base_sha_v="$(git -C "$REPO" rev-parse origin/main)"
printf '%s %s:%s\n' "$base_sha_v" "sp-btv-lo" "$stale_tip_v" > "$(bisect_file)"

# branch_p already checked sp-btv-lo out at $RUN/worktree/sp-btv-lo; commit
# there directly rather than adding a second worktree for the same branch,
# which git refuses.
printf 'rebuild\n' > "$RUN/worktree/sp-btv-lo/rebuild.txt"
git -C "$RUN/worktree/sp-btv-lo" add -A
git -C "$RUN/worktree/sp-btv-lo" commit -q -m "sp-btv-lo: rebuild after split"
new_tip_v="$(tip_of sp-btv-lo)"
printf 'CERTIFIED %s %s\n' "$new_tip_v" "$NOW" > "$LANDSTATE/sp-btv-lo"

out_v="$(batch "$REPONAME")"
is   "v. tip moved: state file dropped" "0" "$([ -f "$(bisect_file)" ] && echo 1 || echo 0)"
is   "v. tip moved: priority winner batched instead of the stale group" "1" \
    "$(is_batched sp-btv-hi && echo 1 || echo 0)"
want "v. tip moved: discard is logged with its reason" "tip changed" "$out_v"
clean_case
tl_summary
