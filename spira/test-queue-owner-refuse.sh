#!/usr/bin/env bash
# test-queue-owner-refuse.sh — one writer per open batch (sp-91hb5): an open batch record
# claimed by the concierge (owner=concierge) refuses every other mutator, and the refusal
# names the owner and the override. The record and the branch it names are unchanged by a
# refused call.
#
# POSITIVE CONTROL: the identical call, made AS the concierge (SPIRA_QUEUE_ACTOR=concierge),
# succeeds and does mutate — proving the refusal above is a real check, not a call that
# would have failed (or succeeded) regardless (law-a-pattern-match-is-not-an-identity-check;
# a check that finds nothing must first prove it could have found something).
#
# tier: T2
# covers: queue/src/* spira/lib.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
# The queue binary (queue/DESIGN.md §7.4), invoked by name: the tree under test's build is
# on the suite's PATH (sp-gypjk).

. "$HERE/testdb.sh"
testdb_require test-queue-owner-refuse
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up owner-refuse || { echo "test-queue-owner-refuse: could not build fixture database"; exit 1; }
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
cp "$HERE"/*.sh "$HERE"/*.py "$SH/" 2>/dev/null
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SH/"
# mail is a compiled binary now (sp-ooh1k), not a script beside these, and "$HERE/mail" is
# the pre-existing kinds/ directory (spira/mail/kinds), not the tool — symlink the real
# compiled binary in by name instead.
ln -sf "$(command -v mail)" "$SH/mail"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

FORGE_LOG="$TMP/forge-log"
cat > "$SH/forge-fixture.sh" <<FORGE
#!/usr/bin/env bash
cmd="\${1:-}"; shift
case "\$cmd" in
    pr-close) printf '%s\tclose\n' "\${2:-}" >> "$FORGE_LOG" ;;
    *) : ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"

MAIL="$RUN/mail"
batch_file() { printf '%s/%s/open' "$QUEUEDIR" "$REPONAME"; }
member_branch() { printf 'spira/sp-ownf1'; }

queue() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${TESTDB_BD:-bd}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
    SPIRA_MAIL="$MAIL" \
        SPIRA_HOME="$SH" command queue "$@"
}

echo "test-queue-owner-refuse.sh"

testdb_seed <<'JSONL'
{"id":"sp-ownf1","title":"owner-refuse-fixture","description":"fixture bead.","status":"closed","issue_type":"task","priority":2,"labels":[],"updated_at":"2026-09-24T00:00:00Z","closed_at":"2026-09-24T00:00:00Z"}
JSONL

setup_open_batch() {
    base_sha="$(git -C "$REPO" rev-parse origin/main)"
    bwt="$RUN/worktree/sp-ownf1"
    rm -rf "$bwt"
    git -C "$REPO" worktree add -q -b "$(member_branch)" "$bwt" origin/main 2>/dev/null
    printf 'sp-ownf1\n' > "$bwt/sp-ownf1.txt"
    git -C "$bwt" add -A
    git -C "$bwt" commit -q -m "sp-ownf1: work"
    tip="$(git -C "$REPO" rev-parse "$(member_branch)")"
    stamp="20260927T000000Z"
    batch_br="spira/queue/$stamp"
    git -C "$REPO" branch -f "$batch_br" "$tip" >/dev/null
    printf 'BATCHED %s %s\n' "$tip" "$(date +%s)" > "$LANDSTATE/sp-ownf1"
    {
        printf 'pr=77\nhead=%s\nbase=%s\nmembers=sp-ownf1:%s\nopened=%s\nbranch=%s\nowner=concierge\n' \
            "$tip" "$base_sha" "$tip" "$(date +%s)" "$batch_br"
    } > "$(batch_file)"
}

teardown_open_batch() {
    rm -f "$(batch_file)" "$LANDSTATE/sp-ownf1"
    git -C "$REPO" worktree remove -f "$bwt" 2>/dev/null || true
    git -C "$REPO" branch -D "$(member_branch)" "$batch_br" 2>/dev/null || true
    git -C "$REPO" worktree prune 2>/dev/null || true
}

# ==========================================================================
# 1. A non-owner eject is refused; the record and branch are unchanged.
# ==========================================================================
echo
echo "eject by a non-owner is refused while the batch is claimed by concierge:"

setup_open_batch
record_before="$(cat "$(batch_file)")"
branch_sha_before="$(git -C "$REPO" rev-parse "$batch_br")"

out="$(SPIRA_QUEUE_ACTOR=some-other-writer queue eject sp-ownf1 "$REPONAME" 2>&1)"
rc=$?

wantrc  "refused eject exits non-zero"                1 "$rc"
want    "refusal names the owner"                     "claimed by concierge" "$out"
want    "refusal names the override"                  "SPIRA_QUEUE_OWNER_OVERRIDE=1" "$out"
is      "open record is unchanged"                    "$record_before" "$(cat "$(batch_file)")"
is      "batch branch tip is unchanged"                "$branch_sha_before" "$(git -C "$REPO" rev-parse "$batch_br")"
is      "landstate is unchanged (still BATCHED)"       "BATCHED" "$(awk '{print $1; exit}' "$LANDSTATE/sp-ownf1")"

teardown_open_batch

# ==========================================================================
# 2. POSITIVE CONTROL: the concierge itself can still eject its own claim —
#    proving case 1's refusal is a real ownership check, not a call that
#    always fails.
# ==========================================================================
echo
echo "POSITIVE CONTROL: eject by the concierge itself (the owner) succeeds:"

setup_open_batch

out2="$(SPIRA_QUEUE_ACTOR=concierge queue eject sp-ownf1 "$REPONAME" 2>&1)"
rc2=$?

wantrc "owner eject exits zero"           0 "$rc2"
is     "open record is removed (last member ejected)" "0" "$([ -f "$(batch_file)" ] && echo 1 || echo 0)"
is     "landstate becomes RED"            "RED" "$(awk '{print $1; exit}' "$LANDSTATE/sp-ownf1")"

teardown_open_batch

# ==========================================================================
# 3. abandon is refused the same way.
# ==========================================================================
echo
echo "abandon by a non-owner is refused while the batch is claimed by concierge:"

setup_open_batch
record_before3="$(cat "$(batch_file)")"

out3="$(SPIRA_QUEUE_ACTOR=some-other-writer queue abandon "$REPONAME" --reason "trying to abandon" 2>&1)"
rc3=$?

wantrc "refused abandon exits non-zero"   1 "$rc3"
want   "abandon refusal names the owner"      "claimed by concierge" "$out3"
want   "abandon refusal names the override"   "SPIRA_QUEUE_OWNER_OVERRIDE=1" "$out3"
is     "open record is unchanged (abandon refused)" "$record_before3" "$(cat "$(batch_file)")"

teardown_open_batch

echo
tl_summary
