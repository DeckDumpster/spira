#!/usr/bin/env bash
#
# test-batch-conflicting-pr.sh — the mergeability TRIGGER: batch.sh's open-batch check
#   calls forge pr-mergeability and branches on DIRTY vs CLEAN.
#
# What happens once DIRTY fires — the abandon mechanics themselves (members returned to
# CERTIFIED, PR closed, run cancelled, operator mailed) — is test-queue-ops.sh's "DIRTY
# PR" case, folded in from here (sp-s088v.16, duplicate cluster #6): this file now only
# proves the predicate that decides whether to fire.
#
# THE PROPERTY UNDER TEST (sp-7kcj2). When a batch PR is cut against a stale base and
# another branch lands in the same file, the PR becomes DIRTY (CONFLICTING). Before the
# original fix, batch.sh had no mergeability check at _batch_is_open and returned early,
# leaving members pinned at BATCHED indefinitely with no alarm.
#
# defect: sp-7kcj2
# covers: spira/batch.sh spira/forge.sh spira/conf.sh
# scar: batch.sh returned early on _batch_is_open with no mergeability check; a DIRTY PR
#       pinned members at BATCHED indefinitely with no alarm.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

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

# Forge fixture — pr-mergeability answers from MERGE_ANSWER; every other call is a no-op.
# The MAIL_LOG/CLOSE_LOG-style bookkeeping the DIRTY case needs lives with that case now,
# in test-queue-ops.sh — this file only needs to prove the predicate.
MERGE_ANSWER="$TMP/merge-answer"
cat > "$SH/forge-fixture.sh" <<FORGE
#!/usr/bin/env bash
cmd="\${1:-}"; shift; repo="\${1:-}"; shift
case "\$cmd" in
    pr-mergeability)
        pr_n="\${1:-}"
        answer="\$(cat "$MERGE_ANSWER" 2>/dev/null || printf UNKNOWN)"
        printf '%s\t%s\n' "\$pr_n" "\$answer" >> "$MERGE_LOG"
        printf '%s\n' "\$answer"
        ;;
    pr-list-queue|runs-queue-branches) : ;;
    *) printf 'forge-fixture: unknown command: %s\n' "\$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"
: > "$MERGE_LOG"

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
{"id":"sp-gama","title":"certified member of the open batch","status":"in_progress","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-23T00:00:00Z"}
JSONL

git -C "$REPO" checkout -q -b spira/sp-gama main
printf 'gama\n' > "$REPO/gama.txt"
git -C "$REPO" add gama.txt && git -C "$REPO" commit -q -m "sp-gama: work"
_gama_tip="$(git -C "$REPO" rev-parse spira/sp-gama)"
git -C "$REPO" checkout -q main

echo "test-batch-conflicting-pr.sh"

# =============================================================================
# POSITIVE CONTROL — forge mergeability fixture can return DIRTY.
# (Verifies the fixture is wired before we trust the batch output.)
# =============================================================================
echo
echo "positive control — forge-fixture pr-mergeability returns DIRTY:"

printf 'DIRTY\n' > "$MERGE_ANSWER"
_merge_out="$("$SH/forge-fixture.sh" pr-mergeability "$REPO" 259)"
is "forge fixture returns DIRTY" "DIRTY" "$_merge_out"

# =============================================================================
# CLEAN BATCH SKIPS — a CLEAN mergeability answer leaves the batch alone.
# =============================================================================
echo
echo "CLEAN batch — open batch with CLEAN mergeability is left alone:"

printf 'BATCHED %s %s' "$_gama_tip" "$(date +%s)" > "$LANDSTATE/sp-gama"
cat > "$QUEUEDIR/$REPONAME/open" <<OPEN
pr=260
head=bbbfakebbbfakebbbfake
base=$(git -C "$REPO" rev-parse origin/main)
members=sp-gama:${_gama_tip}
opened=$(date -u +%Y%m%dT%H%M%SZ)
branch=spira/queue/20260923T190000Z
OPEN
printf 'CLEAN\n' > "$MERGE_ANSWER"

out="$(batch "$REPONAME")"
rc=$?
is  "batch exits 0 (CLEAN case)"                 0 "$rc"
want "batch logs open batch exists (CLEAN case)"  "open batch exists" "$out"
want "forge pr-mergeability was queried" "260" "$(cat "$MERGE_LOG" 2>/dev/null)"

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
