#!/usr/bin/env bash
#
# test-config-compat-master-base.sh — gap G3 (docs/test-plan/landing-merge-queue.md
# section 6): no suite in this area ran with a `master` base branch, despite 3 of 7
# repos using it and this exact assumption ("the base branch is not always main")
# having been fixed four times before it held (CLAUDE.md). Every existing suite in
# this area hardcodes `-b main` for its fixture repo and `origin/main` in its
# repo-map row.
#
# THREE ROWS, one per land mode that actually advances a base: batch.sh's merge
# onto spira_landref, verdict.sh's fast-forward, and landing.sh push. (queue mode's
# certify step and pr mode never move a base branch themselves, so they carry no
# row here.) Each drives the real script against a repo whose base is `master`.
#
# tier: T2
# covers: spira/batch.sh spira/verdict.sh spira/landing.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-config-compat-master-base
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up cfgcompatmaster || skip "testdb not available"

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

echo "test-config-compat-master-base.sh"

# ============================================================================
echo
echo "batch.sh: a CERTIFIED branch is merged onto a MASTER-based spira_landref:"
# ============================================================================
B_REPONAME=fixture-batch
B_REPO="$TMP/batch-repo"; B_REMOTE="$TMP/batch-remote.git"
B_RUN="$TMP/batch-run"; B_SH="$TMP/batch-spira"
B_LANDSTATE="$B_RUN/landstate"; B_QUEUEDIR="$B_RUN/queue"

git init -q --bare -b master "$B_REMOTE"
git init -q -b master "$B_REPO"
git -C "$B_REPO" commit -q --allow-empty -m base
git -C "$B_REPO" remote add origin "$B_REMOTE"
git -C "$B_REPO" push -q origin master
git -C "$B_REPO" fetch -q origin
mkdir -p "$B_RUN/worktree" "$B_SH" "$B_LANDSTATE" "$B_QUEUEDIR/$B_REPONAME"
cp "$HERE"/*.sh "$B_SH/"

printf '#!/usr/bin/env bash\nexit 0\n' > "$B_SH/gate.sh"; chmod +x "$B_SH/gate.sh"
B_FORGE_LOG="$TMP/batch-forge-log"; B_BODY_LOG="$TMP/batch-body-log"
: > "$B_FORGE_LOG"; : > "$B_BODY_LOG"
cat > "$B_SH/forge-fixture.sh" <<FORGE
#!/usr/bin/env bash
cmd="\${1:-}"; shift; repo="\${1:-}"; shift
case "\$cmd" in
    main-gate-status) printf 'green deadbeef\n' ;;
    pr-create)
        n=\$(( \$(wc -l < "$B_FORGE_LOG" 2>/dev/null || echo 0) + 1 ))
        body="\$(cat)"
        printf '%s\n' "\$n" >> "$B_FORGE_LOG"
        printf '%s\n' "\$body" >> "$B_BODY_LOG"
        printf '%s\n' "\$n"
        ;;
    pr-number) grep "^\${1:-}	" "$B_FORGE_LOG" 2>/dev/null | tail -1 | cut -f2 ;;
    pr-comment|pr-list-queue) : ;;
    *) printf 'forge-fixture: unknown: %s\n' "\$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$B_SH/forge-fixture.sh"
printf '#!/usr/bin/env bash\ntrue\n' > "$B_SH/mail.sh"; chmod +x "$B_SH/mail.sh"

# THE ROW: base is origin/master, declared explicitly rather than left to auto-detect.
printf '%s | %s | queue | origin/master | | |\n' "$B_REPONAME" "$B_REPO" > "$B_SH/repo-map"

testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
printf '{"id":"sp-mbase","title":"master-base fix","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-mbase","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
    | testdb_seed

git -C "$B_REPO" worktree add -q -b spira/sp-mbase "$B_RUN/worktree/sp-mbase" master
printf 'sp-mbase\n' > "$B_RUN/worktree/sp-mbase/sp-mbase.txt"
git -C "$B_RUN/worktree/sp-mbase" add -A
git -C "$B_RUN/worktree/sp-mbase" commit -q -m "sp-mbase: work"
B_TIP="$(git -C "$B_REPO" rev-parse spira/sp-mbase)"
printf 'CERTIFIED %s %s\n' "$B_TIP" "$(date +%s)" > "$B_LANDSTATE/sp-mbase"

out="$(SPIRA_HOME="$B_SH" SPIRA_RUN="$B_RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO_MAP="$B_SH/repo-map" \
    SPIRA_QUEUE_DIR="$B_QUEUEDIR" SPIRA_QUEUE_BATCH_MAX=1 SPIRA_QUEUE_BATCH_WAIT=0 \
    SPIRA_FORGE="$B_SH/forge-fixture.sh" \
        bash "$B_SH/batch.sh" "$B_REPONAME" 2>&1)"

want "batch: a PR was opened for the master-based batch" "opened" "$out"
case "$(cat "$B_LANDSTATE/sp-mbase" 2>/dev/null)" in
    BATCHED*) ok "batch: sp-mbase is BATCHED, not left CERTIFIED" ;;
    *) bad "batch: sp-mbase is BATCHED, not left CERTIFIED" "got: $(cat "$B_LANDSTATE/sp-mbase" 2>/dev/null)" ;;
esac
B_BATCH_BRANCH="$(grep '^branch=' "$B_QUEUEDIR/$B_REPONAME/open" 2>/dev/null | cut -d= -f2)"
if [ -n "$B_BATCH_BRANCH" ] \
   && git -C "$B_REPO" merge-base --is-ancestor master "refs/heads/$B_BATCH_BRANCH" 2>/dev/null; then
    ok "batch: the batch branch was merged onto MASTER (spira_landref), not main"
else
    bad "batch: the batch branch was merged onto MASTER (spira_landref), not main" \
        "branch=$B_BATCH_BRANCH"
fi

# ============================================================================
echo
echo "verdict.sh: a green batch fast-forwards a MASTER-based remote:"
# ============================================================================
V_REPONAME=fixture-verdict
V_REPO="$TMP/verdict-repo"; V_REMOTE="$TMP/verdict-remote.git"
V_RUN="$TMP/verdict-run"; V_SH="$TMP/verdict-spira"
V_LANDSTATE="$V_RUN/landstate"; V_QUEUEDIR="$V_RUN/queue"

git init -q --bare -b master "$V_REMOTE"
git init -q -b master "$V_REPO"
git -C "$V_REPO" commit -q --allow-empty -m base
git -C "$V_REPO" remote add origin "$V_REMOTE"
git -C "$V_REPO" push -q origin master
git -C "$V_REPO" fetch -q origin
mkdir -p "$V_RUN/worktree" "$V_SH" "$V_LANDSTATE" "$V_QUEUEDIR/$V_REPONAME"
cp "$HERE"/*.sh "$V_SH/"

V_FORGE_STATUS_FILE="$TMP/verdict-forge-status"
cat > "$V_SH/forge-fixture.sh" <<FORGE
#!/usr/bin/env bash
cmd="\${1:-}"; shift
case "\$cmd" in
    check-status) cat "$V_FORGE_STATUS_FILE" 2>/dev/null || printf 'pending\n' ;;
    run-id)       printf 'run-1\n' ;;
    run-metadata) : ;;
    run-cancel|workflow-rerun|pr-close) : ;;
    *) printf 'forge-fixture: unknown: %s\n' "\$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$V_SH/forge-fixture.sh"
printf '#!/usr/bin/env bash\ntrue\n' > "$V_SH/mail.sh"; chmod +x "$V_SH/mail.sh"
printf '#!/usr/bin/env bash\ntrue\n' > "$V_SH/suites.sh"; chmod +x "$V_SH/suites.sh"
printf '%s | %s | queue | origin/master | | |\n' "$V_REPONAME" "$V_REPO" > "$V_SH/repo-map"

# Build one member branch and an open batch record whose local merge commit sits
# on top of the current origin/master — the same shape build_batch() constructs
# in test-verdict.sh, inlined here so this file has no dependency on that one.
V_BASE_SHA="$(git -C "$V_REPO" rev-parse origin/master)"
V_BWT="$V_RUN/worktree/.batch-build"
git -C "$V_REPO" worktree add -q --detach "$V_BWT" "$V_BASE_SHA"
git -C "$V_REPO" worktree add -q -b spira/sp-vbase "$V_RUN/worktree/sp-vbase" origin/master
printf 'sp-vbase\n' > "$V_RUN/worktree/sp-vbase/sp-vbase.txt"
git -C "$V_RUN/worktree/sp-vbase" add -A
git -C "$V_RUN/worktree/sp-vbase" commit -q -m "sp-vbase: work"
V_MEMBER_TIP="$(git -C "$V_REPO" rev-parse spira/sp-vbase)"
git -C "$V_BWT" merge -q --no-edit --no-ff -m "spira: land sp-vbase" "$V_MEMBER_TIP" >/dev/null 2>&1
V_BATCH_HEAD="$(git -C "$V_BWT" rev-parse HEAD)"
git -C "$V_REPO" worktree remove -f "$V_BWT" 2>/dev/null || true

{
    printf 'pr=7\n'
    printf 'head=%s\n' "$V_BATCH_HEAD"
    printf 'base=%s\n' "$V_BASE_SHA"
    printf 'members=sp-vbase:%s\n' "$V_MEMBER_TIP"
    printf 'opened=%s\n' "$(date +%s)"
    printf 'branch=spira/queue/vtest\n'
} > "$V_QUEUEDIR/$V_REPONAME/open"
printf 'BATCHED %s %s\n' "$V_MEMBER_TIP" "$(date +%s)" > "$V_LANDSTATE/sp-vbase"
printf 'green\nhead-sha: %s\n' "$V_BATCH_HEAD" > "$V_FORGE_STATUS_FILE"

out="$(SPIRA_HOME="$V_SH" SPIRA_RUN="$V_RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO_MAP="$V_SH/repo-map" \
    SPIRA_QUEUE_DIR="$V_QUEUEDIR" SPIRA_QUEUE_CI_MAXSEC=3600 SPIRA_QUEUE_CI_IDLE_SEC=600 \
    SPIRA_QUEUE_INFRA_RETRIES=2 SPIRA_FORGE="$V_SH/forge-fixture.sh" \
        bash "$V_SH/verdict.sh" "$V_REPONAME" 2>&1)"

is   "verdict: remote MASTER fast-forwards to the batch head" \
     "$V_BATCH_HEAD" "$(git -C "$V_REMOTE" rev-parse master 2>/dev/null)"
want "verdict: reports a fast-forward landing" "landed by fast-forward" "$out"
case "$(cat "$V_LANDSTATE/sp-vbase" 2>/dev/null)" in
    LANDED*) ok "verdict: sp-vbase LANDED on the master base" ;;
    *) bad "verdict: sp-vbase LANDED on the master base" "got: $(cat "$V_LANDSTATE/sp-vbase" 2>/dev/null)" ;;
esac

# ============================================================================
echo
echo "landing.sh: a closed bead's branch lands (push mode) onto a MASTER base:"
# ============================================================================
L_REPO="$TMP/landing-repo"; L_REMOTE="$TMP/landing-remote.git"
L_RUN="$TMP/landing-run"; L_SH="$TMP/landing-spira"
git init -q --bare -b master "$L_REMOTE"
git init -q -b master "$L_REPO"
git -C "$L_REPO" commit -q --allow-empty -m base
git -C "$L_REPO" remote add origin "$L_REMOTE"
git -C "$L_REPO" push -q origin master
git -C "$L_REPO" fetch -q origin
mkdir -p "$L_RUN/worktree" "$L_SH"
cp "$HERE/landing.sh" "$HERE/landing-lib.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/suite-covers.sh" "$L_SH/"
printf '#!/usr/bin/env bash\necho "gate: VERDICT=PASS reason=stub branch=$1 repo=${2:-?}" >&2\nexit 0\n' \
    > "$L_SH/gate.sh"; chmod +x "$L_SH/gate.sh"
printf '#!/usr/bin/env bash\nexit 0\n' > "$L_SH/confine.sh"; chmod +x "$L_SH/confine.sh"
printf '#!/usr/bin/env bash\nexit 0\n' > "$L_SH/skew.sh"; chmod +x "$L_SH/skew.sh"

testdb_seed <<'JSONL'
{"id":"sp-goal2","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
printf '{"id":"sp-lbase","title":"landing master-base","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-lbase","depends_on_id":"sp-goal2","type":"parent-child"}]}\n' \
    | testdb_seed

git -C "$L_REPO" worktree add -q -b spira/sp-lbase "$L_RUN/worktree/sp-lbase" master
printf 'sp-lbase\n' > "$L_RUN/worktree/sp-lbase/sp-lbase.txt"
git -C "$L_RUN/worktree/sp-lbase" add -A
git -C "$L_RUN/worktree/sp-lbase" commit -q -m "feat: sp-lbase — work"

out="$(SPIRA_HOME="$L_SH" SPIRA_RUN="$L_RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO="$L_REPO" \
    SPIRA_REPO_MAP="$L_SH/repo-map-does-not-exist" \
        bash "$L_SH/landing.sh" 2>&1)"

want "landing: reports landing the master-base branch" "landed spira/sp-lbase" "$out"
git -C "$L_REPO" fetch -q origin
if git -C "$L_REPO" merge-base --is-ancestor spira/sp-lbase origin/master 2>/dev/null; then
    ok "landing: the branch's own commit is an ancestor of origin/master"
else
    bad "landing: the branch's own commit is an ancestor of origin/master" \
        "spira/sp-lbase is not an ancestor of origin/master"
fi

status_of() { "${TESTDB_BD:-bd}" -C "$SPIRA_DB" show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }
is "landing: sp-lbase stays closed" "closed" "$(status_of sp-lbase)"

tl_summary
