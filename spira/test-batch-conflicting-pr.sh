#!/usr/bin/env bash
#
# test-batch-conflicting-pr.sh — batch.sh detects a DIRTY open batch PR and abandons it,
#   returning members to CERTIFIED and mailing the operator.
#
# THE PROPERTY UNDER TEST (sp-7kcj2). When a batch PR is cut against a stale base and
# another branch lands in the same file, the PR becomes DIRTY (CONFLICTING). Before this
# fix, batch.sh had no mergeability check at _batch_is_open and returned early, leaving
# members pinned at BATCHED indefinitely with no alarm.
#
# THREE CASES, THREE OUTCOMES:
#
#   sp-alfa: BATCHED member of a DIRTY PR. Must be returned to CERTIFIED.
#   sp-beta: BATCHED member of a DIRTY PR, already RED. Must be left at RED (not CERTIFIED).
#   sp-gama: CERTIFIED, not in the DIRTY batch. Must not be disturbed.
#
# SEEN RED WITHOUT THE FIX. Removing the mergeability check leaves sp-alfa at BATCHED;
# the assertions below then fail.
#
# defect: sp-7kcj2
# covers: spira/batch.sh spira/forge.sh spira/conf.sh spira/landing.sh
# scar: batch.sh returned early on _batch_is_open with no mergeability check; a DIRTY PR
#       pinned members at BATCHED indefinitely with no alarm.
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
testdb_require test-batch-conflicting-pr
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up batchconflictingpr || { echo "test-batch-conflicting-pr: could not build fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"
REMOTE="$TMP/remote.git"
RUN="$TMP/run"
SH="$TMP/spira"
REPONAME=fixture-repo
LANDSTATE="$RUN/landstate"
QUEUEDIR="$RUN/queue"
MAIL_LOG="$TMP/mail-log"
FORGE_LOG="$TMP/forge-log"
CLOSE_LOG="$TMP/close-log"
COMMENT_LOG="$TMP/comment-log"
MERGE_LOG="$TMP/merge-log"

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

# mail.sh stub — records calls for assertion.
cat > "$SH/mail.sh" <<MAIL
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$MAIL_LOG"
MAIL
chmod +x "$SH/mail.sh"
: > "$MAIL_LOG"

# Forge fixture — pr-mergeability returns DIRTY; pr-close and pr-comment are recorded.
cat > "$SH/forge-fixture.sh" <<FORGE
#!/usr/bin/env bash
cmd="\${1:-}"; shift; repo="\${1:-}"; shift
case "\$cmd" in
    pr-mergeability)
        pr_n="\${1:-}"
        printf '%s\t%s\n' "\$pr_n" "DIRTY" >> "$MERGE_LOG"
        printf 'DIRTY\n'
        ;;
    pr-close)
        pr_n="\${1:-}"
        printf '%s\n' "\$pr_n" >> "$CLOSE_LOG"
        ;;
    pr-comment)
        pr_n="\${1:-}" body="\${2:-}"
        printf '%s\t%s\n' "\$pr_n" "\$body" >> "$COMMENT_LOG"
        ;;
    pr-list-queue)
        : ;;
    *) printf 'forge-fixture: unknown command: %s\n' "\$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"
: > "$FORGE_LOG"; : > "$CLOSE_LOG"; : > "$COMMENT_LOG"; : > "$MERGE_LOG"

batch() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_MAX=8 \
    SPIRA_QUEUE_BATCH_WAIT=0 \
    SPIRA_QUEUE_LOCAL_GATE=0 \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
        bash "$SH/batch.sh" "$@" 2>&1
}

testdb_reset
testdb_seed <<JSONL
{"id":"sp-alfa","title":"member that must return to CERTIFIED","status":"in_progress","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-23T00:00:00Z"}
{"id":"sp-beta","title":"member already RED — must stay RED","status":"in_progress","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-23T00:00:00Z"}
{"id":"sp-gama","title":"certified outside batch — must not be disturbed","status":"in_progress","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-23T00:00:00Z"}
JSONL

# ─── Fixture: create branches so refs exist ───────────────────────────────────
git -C "$REPO" checkout -q -b spira/sp-alfa main
printf 'alfa\n' > "$REPO/alfa.txt"
git -C "$REPO" add alfa.txt && git -C "$REPO" commit -q -m "sp-alfa: work"
_alfa_tip="$(git -C "$REPO" rev-parse spira/sp-alfa)"
git -C "$REPO" checkout -q main

git -C "$REPO" checkout -q -b spira/sp-beta main
printf 'beta\n' > "$REPO/beta.txt"
git -C "$REPO" add beta.txt && git -C "$REPO" commit -q -m "sp-beta: work"
_beta_tip="$(git -C "$REPO" rev-parse spira/sp-beta)"
git -C "$REPO" checkout -q main

git -C "$REPO" checkout -q -b spira/sp-gama main
printf 'gama\n' > "$REPO/gama.txt"
git -C "$REPO" add gama.txt && git -C "$REPO" commit -q -m "sp-gama: work"
_gama_tip="$(git -C "$REPO" rev-parse spira/sp-gama)"
git -C "$REPO" checkout -q main

# ─── Landstates: sp-alfa and sp-beta are BATCHED in an open PR; sp-gama CERTIFIED ───
_batch_epoch=$(( $(date +%s) - 120 ))
printf 'BATCHED %s %s' "$_alfa_tip" "$_batch_epoch" > "$LANDSTATE/sp-alfa"
printf 'RED %s %s' "$_beta_tip" "$_batch_epoch" > "$LANDSTATE/sp-beta"
printf 'CERTIFIED %s %s' "$_gama_tip" "$(date +%s)" > "$LANDSTATE/sp-gama"

# ─── Open batch file: simulates a PR that was cut and is now DIRTY ────────────
cat > "$QUEUEDIR/$REPONAME/open" <<OPEN
pr=259
head=aaafakeaaafakeaaafake
base=$(git -C "$REPO" rev-parse origin/main)
members=sp-alfa:${_alfa_tip} sp-beta:${_beta_tip}
opened=$(date -u +%Y%m%dT%H%M%SZ)
branch=spira/queue/20260923T182519Z
OPEN

echo "test-batch-conflicting-pr.sh"

# =============================================================================
# POSITIVE CONTROL — forge mergeability fixture can return DIRTY.
# (Verifies the fixture is wired before we trust the batch output.)
# =============================================================================
echo
echo "positive control — forge-fixture pr-mergeability returns DIRTY:"

_merge_out="$("$SH/forge-fixture.sh" pr-mergeability "$REPO" 259)"
is "forge fixture returns DIRTY" "DIRTY" "$_merge_out"

# =============================================================================
# MAIN CASE — batch.sh detects DIRTY PR and abandons the batch.
# =============================================================================
echo
echo "main case — batch.sh detects DIRTY PR and abandons:"

out="$(batch "$REPONAME")"
rc=$?
is  "batch exits 0"                               0 "$rc"
want "batch logs DIRTY detection"                 "DIRTY" "$out"
want "batch logs abandoning"                      "abandoning" "$out"

# sp-alfa must be returned to CERTIFIED.
case "$(cat "$LANDSTATE/sp-alfa" 2>/dev/null)" in CERTIFIED*)
    ok "sp-alfa: returned to CERTIFIED" ;;
    *) bad "sp-alfa: returned to CERTIFIED" \
           "got: $(cat "$LANDSTATE/sp-alfa" 2>/dev/null)" ;; esac

want "batch logs sp-alfa returned to CERTIFIED" \
    "sp-alfa returned to CERTIFIED" "$out"

# sp-beta must remain RED (not overwritten by the return-to-CERTIFIED path).
case "$(cat "$LANDSTATE/sp-beta" 2>/dev/null)" in RED*)
    ok "sp-beta: stays RED (not overwritten)" ;;
    *) bad "sp-beta: stays RED (not overwritten)" \
           "got: $(cat "$LANDSTATE/sp-beta" 2>/dev/null)" ;; esac

want "batch logs sp-beta left at RED" \
    "sp-beta left at RED" "$out"

# sp-gama must not be touched (CERTIFIED, outside the batch).
case "$(cat "$LANDSTATE/sp-gama" 2>/dev/null)" in CERTIFIED*)
    ok "sp-gama: CERTIFIED state preserved" ;;
    *) bad "sp-gama: CERTIFIED state preserved" \
           "got: $(cat "$LANDSTATE/sp-gama" 2>/dev/null)" ;; esac

# =============================================================================
# PR CLOSED — forge pr-close was called with the right PR number.
# =============================================================================
echo
echo "PR closed — forge pr-close called with PR 259:"

want "forge pr-close called with PR 259" "259" "$(cat "$CLOSE_LOG" 2>/dev/null)"

# =============================================================================
# OPERATOR MAILED — mail.sh was called with the conflict subject.
# =============================================================================
echo
echo "operator mailed — mail.sh called with conflict subject:"

want "mail.sh called with conflict subject" \
    "batch PR abandoned" "$(cat "$MAIL_LOG" 2>/dev/null)"

# =============================================================================
# OPEN FILE REMOVED — open batch is gone after abandon.
# =============================================================================
echo
echo "open file removed — no open batch after abandon:"

if [ ! -f "$QUEUEDIR/$REPONAME/open" ]; then
    ok "open batch file was removed"
else
    bad "open batch file was removed" "file still exists"
fi

# =============================================================================
# CLEAN BATCH SKIPS — a CLEAN mergeability answer leaves the batch alone.
# =============================================================================
echo
echo "CLEAN batch — open batch with CLEAN mergeability is left alone:"

# Restore open file, update forge to return CLEAN.
cat > "$QUEUEDIR/$REPONAME/open" <<OPEN2
pr=260
head=bbbfakebbbfakebbbfake
base=$(git -C "$REPO" rev-parse origin/main)
members=sp-gama:${_gama_tip}
opened=$(date -u +%Y%m%dT%H%M%SZ)
branch=spira/queue/20260923T190000Z
OPEN2

# Restore sp-gama to BATCHED for this sub-test.
printf 'BATCHED %s %s' "$_gama_tip" "$(date +%s)" > "$LANDSTATE/sp-gama"

cat > "$SH/forge-fixture.sh" <<FORGE2
#!/usr/bin/env bash
cmd="\${1:-}"; shift; repo="\${1:-}"; shift
case "\$cmd" in
    pr-mergeability)
        printf 'CLEAN\n' ;;
    pr-list-queue)
        : ;;
    *) printf 'forge-fixture: unknown command: %s\n' "\$cmd" >&2; exit 1 ;;
esac
FORGE2
chmod +x "$SH/forge-fixture.sh"

out2="$(batch "$REPONAME")"
rc2=$?
is  "batch exits 0 (CLEAN case)"                 0 "$rc2"
want "batch logs open batch exists (CLEAN case)"  "open batch exists" "$out2"

if [ -f "$QUEUEDIR/$REPONAME/open" ]; then
    ok "CLEAN: open batch file preserved"
else
    bad "CLEAN: open batch file preserved" "file was removed"
fi

case "$(cat "$LANDSTATE/sp-gama" 2>/dev/null)" in BATCHED*)
    ok "CLEAN: sp-gama remains BATCHED" ;;
    *) bad "CLEAN: sp-gama remains BATCHED" \
           "got: $(cat "$LANDSTATE/sp-gama" 2>/dev/null)" ;; esac

printf '\n%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
