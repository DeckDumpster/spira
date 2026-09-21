#!/usr/bin/env bash
#
# test-batch-certified-landed.sh — batch.sh reconciles CERTIFIED+tip-in-base to LANDED
#   even while a batch PR is open.
#
# THE DEFECT (sp-dtpc8). batch.sh's reconciliation loop (the one that runs before the
# open-batch guard) handled the branch-gone case but not the branch-still-exists case.
# When a CERTIFIED branch's tip landed in origin/main through another path while a batch
# PR was pending, the CERTIFIED record persisted: `_batch_is_open` returned early and the
# _certified_list already-in-base check never ran.
#
# TWO CASES, TWO OUTCOMES:
#
#   sp-landed: CERTIFIED landstate, branch exists, tip IS in origin/main (merged).
#     batch.sh must mark LANDED even while a batch is open.
#
#   sp-pending: CERTIFIED landstate, branch exists, tip is NOT in origin/main.
#     batch.sh must leave it CERTIFIED (it still needs batching).
#
# SEEN RED WITHOUT THE FIX. Removing the branch-exists ancestry check from batch.sh
# causes the `[ "$_anyrn" = 1 ] && continue` path to skip sp-landed without inspection;
# it stays CERTIFIED and the "sp-landed: LANDED" assertion below fails.
#
# defect: sp-dtpc8
# covers: spira/batch.sh spira/conf.sh spira/landing.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-batch-certified-landed
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up batchcertlanded || { echo "test-batch-certified-landed: could not build fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"
REMOTE="$TMP/remote.git"
RUN="$TMP/run"
SH="$TMP/spira"
REPONAME=fixture-repo
LANDSTATE="$RUN/landstate"
QUEUEDIR="$RUN/queue"
MAIL_LOG="$TMP/mail-log"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/$REPONAME"

cp "$HERE"/*.sh "$SH/"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

cat > "$SH/mail.sh" <<MAIL
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$MAIL_LOG"
MAIL
chmod +x "$SH/mail.sh"
: > "$MAIL_LOG"

batch() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_MAX=8 \
    SPIRA_QUEUE_BATCH_WAIT=0 \
        bash "$SH/batch.sh" "$@" 2>&1
}

testdb_reset
testdb_seed <<JSONL
{"id":"sp-landed","title":"tip already in main","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-19T00:00:00Z"}
{"id":"sp-pending","title":"tip not yet in main","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-19T00:00:00Z"}
JSONL

# ─── sp-landed: branch exists, tip IS already in origin/main ──────────────────
git -C "$REPO" checkout -q -b spira/sp-landed main
printf 'landed\n' > "$REPO/sp-landed.txt"
git -C "$REPO" add sp-landed.txt && git -C "$REPO" commit -q -m "sp-landed: work"
_landed_tip="$(git -C "$REPO" rev-parse spira/sp-landed)"
git -C "$REPO" checkout -q main
# Fast-forward main to include sp-landed (simulates the batch PR landing externally).
git -C "$REPO" merge -q --no-edit --ff-only spira/sp-landed
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
# No trailing newline — matches land_mark in lib.sh.  A reader using "|| continue"
# would skip this record; that was the defect.
printf 'CERTIFIED %s %s' "$_landed_tip" "$(date +%s)" > "$LANDSTATE/sp-landed"

# ─── sp-pending: branch exists, tip NOT in origin/main ────────────────────────
git -C "$REPO" checkout -q -b spira/sp-pending main
printf 'pending\n' > "$REPO/sp-pending.txt"
git -C "$REPO" add sp-pending.txt && git -C "$REPO" commit -q -m "sp-pending: work"
_pending_tip="$(git -C "$REPO" rev-parse spira/sp-pending)"
git -C "$REPO" checkout -q main
printf 'CERTIFIED %s %s' "$_pending_tip" "$(date +%s)" > "$LANDSTATE/sp-pending"

# ─── Open batch: simulates a batch PR currently waiting for CI ────────────────
# With an open batch, _batch_is_open returns true and the normal certified-list
# reconciliation is skipped. The pre-guard loop must do the work instead.
cat > "$QUEUEDIR/$REPONAME/open" <<BATCH
pr=42
head=deadbeefdeadbeefdeadbeefdeadbeefdeadbeef
base=$(git -C "$REPO" rev-parse origin/main)
branch=spira/queue/batch-42
members=sp-other:deadbeefdeadbeefdeadbeefdeadbeefdeadbeef
opened=$(date +%s)
retries=0
BATCH

echo "test-batch-certified-landed.sh"

# =============================================================================
# RECONCILE WHILE BATCH OPEN — sp-landed tip is in main, should become LANDED.
# =============================================================================
echo
echo "reconcile while batch open:"

out="$(batch "$REPONAME")"
rc=$?
is  "batch exits 0"                               0 "$rc"
want "batch reports open-batch skip"              "open batch" "$out"

case "$(cat "$LANDSTATE/sp-landed" 2>/dev/null)" in
    LANDED*)
        ok "sp-landed: CERTIFIED-to-LANDED when tip is in main (batch open)" ;;
    *)
        bad "sp-landed: CERTIFIED-to-LANDED when tip is in main (batch open)" \
            "got: $(cat "$LANDSTATE/sp-landed" 2>/dev/null)" ;;
esac

want "batch logs LANDED for sp-landed"            "sp-landed tip already in" "$out"

# =============================================================================
# POSITIVE CONTROL — sp-pending tip is NOT in main, must stay CERTIFIED.
#   Removing the is-ancestor guard would wrongly mark this LANDED.
# =============================================================================
echo
echo "positive control — sp-pending stays CERTIFIED:"

case "$(cat "$LANDSTATE/sp-pending" 2>/dev/null)" in
    CERTIFIED*)
        ok "sp-pending: stays CERTIFIED when tip not in main" ;;
    *)
        bad "sp-pending: stays CERTIFIED when tip not in main" \
            "got: $(cat "$LANDSTATE/sp-pending" 2>/dev/null)" ;;
esac

nowant "sp-pending not in batch log as LANDED" "sp-pending tip already in" "$out"

printf '\n%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
