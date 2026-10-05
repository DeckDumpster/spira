#!/usr/bin/env bash
#
# test-config-compat-master-base.sh — gap G3 (docs/test-plan/landing-merge-queue.md
# section 6): no suite in this area ran with a `master` base branch, despite 3 of 7
# repos using it and this exact assumption ("the base branch is not always main")
# having been fixed four times before it held (CLAUDE.md). Every existing suite in
# this area hardcodes `-b main` for its fixture repo and `origin/main` in its
# repo-map row.
#
# THREE ROWS, one per land mode that actually advances a base: the batcher's cut
# (batch.sh's own merge onto spira_landref, retired sp-vsob2), queue verdict's fast-forward,
# and landing.sh push. (queue mode's certify step and pr mode never move a base branch
# themselves, so they carry no row here.) Each drives the real script/binary against a
# repo whose base is `master`.
#
# tier: T2
# covers: batcher-cut/src/*.rs batcher/src/*.rs queue/src/* landing-pass/src/* spira/lib.sh spira/testlib/lc-fixture.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
# THE LANDING PASS IS THE landing-pass BINARY (landing-pass/DESIGN.md §7.4): `land` for a
# pass, `halt` to stop one. Invoked by name on this suite's PATH (sp-gypjk).

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
. "$HERE/testlib/lc-fixture.sh"
testdb_require test-config-compat-master-base
TMP="$(mktemp -d)"; trap 'lcfix_down; testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up cfgcompatmaster || skip "testdb not available"
lcfix_up || { echo "test-config-compat-master-base: could not build a lifecycle fixture"; exit 1; }

# The batcher is the tree's own build, by name on this suite's PATH (sp-gypjk).

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

echo "test-config-compat-master-base.sh"

# ============================================================================
echo
echo "batcher cut: a CERTIFIED branch is merged onto a MASTER-based spira_landref:"
# ============================================================================
B_REPONAME=fixture-batch
B_REPO="$TMP/batch-repo"; B_REMOTE="$TMP/batch-remote.git"
B_RUN="$TMP/batch-run"; B_SH="$TMP/batch-spira"
B_QUEUEDIR="$B_RUN/queue"

git init -q --bare -b master "$B_REMOTE"
git init -q -b master "$B_REPO"
mkdir -p "$B_REPO/spira"
: > "$B_REPO/spira/test-a.sh"
git -C "$B_REPO" add -A && git -C "$B_REPO" commit -q -m base
git -C "$B_REPO" remote add origin "$B_REMOTE"
git -C "$B_REPO" push -q origin master
git -C "$B_REPO" fetch -q origin
mkdir -p "$B_RUN/worktree" "$B_SH" "$B_QUEUEDIR/$B_REPONAME"
cp "$HERE"/*.sh "$HERE"/*.py "$B_SH/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$B_SH/"
printf '#!/usr/bin/env bash\nexit 0\n' > "$B_SH/mail"; chmod +x "$B_SH/mail"

# Stub round-vm: batcher-cut's corpus step runs `round-vm run <worktree> --suites CSV
# --maxpar N --toolchain V --results-dir DIR` (sp-o3o6z) and reads each suite's result
# protocol from DIR — green for every suite named.
cat > "$B_SH/round-vm-stub.sh" <<'STUB'
#!/usr/bin/env bash
[ "${1:-}" = run ] || { printf 'round-vm-stub: unexpected verb: %s\n' "${1:-}" >&2; exit 2; }
shift
suites_csv="" results=""
while [ $# -gt 0 ]; do
    case "$1" in
        --suites) suites_csv="${2:-}"; shift 2 ;;
        --results-dir) results="${2:-}"; shift 2 ;;
        --maxpar|--toolchain) shift 2 ;;
        *) shift ;;
    esac
done
: "${results:?round-vm-stub: --results-dir not given}"
mkdir -p "$results"
IFS=',' read -r -a suites <<< "$suites_csv"
for s in "${suites[@]:-}"; do
    [ -n "$s" ] || continue
    printf 'ok %s 1 - serial explicit 0\n' "$(date +%s)" > "$results/$s.result"
    : > "$results/$s.out"
done
exit 0
STUB
chmod +x "$B_SH/round-vm-stub.sh"

B_FORGE_LOG="$TMP/batch-forge-log"
: > "$B_FORGE_LOG"
cat > "$B_SH/forge-fixture.sh" <<FORGE
#!/usr/bin/env bash
cmd="\${1:-}"; shift; repo="\${1:-}"; shift
case "\$cmd" in
    pr-create)
        n=\$(( \$(wc -l < "$B_FORGE_LOG" 2>/dev/null || echo 0) + 1 ))
        cat >/dev/null
        printf '%s\n' "\$n" >> "$B_FORGE_LOG"
        printf '%s\n' "\$n"
        ;;
    *) printf 'forge-fixture: unknown: %s\n' "\$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$B_SH/forge-fixture.sh"

# THE ROW: base is origin/master, declared explicitly rather than left to auto-detect.
printf '%s | %s | queue | origin/master | | |\n' "$B_REPONAME" "$B_REPO" > "$B_SH/repo-map"

testdb_seed <<'JSONL'
{"id":"sp-epic","title":"epic","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
# A batch member is open and submitted (sp-1346p): a closed bead's CERTIFIED record is stale.
printf '{"id":"sp-mbase","title":"master-base fix","status":"open","issue_type":"task","labels":["express","spira-submitted"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-mbase","depends_on_id":"sp-epic","type":"parent-child"}]}\n' \
    | testdb_seed

git -C "$B_REPO" worktree add -q -b spira/sp-mbase "$B_RUN/worktree/sp-mbase" master
printf 'sp-mbase\n' > "$B_RUN/worktree/sp-mbase/sp-mbase.txt"
git -C "$B_RUN/worktree/sp-mbase" add -A
git -C "$B_RUN/worktree/sp-mbase" commit -q -m "sp-mbase: work"
B_TIP="$(git -C "$B_REPO" rev-parse spira/sp-mbase)"
git -C "$B_REPO" worktree remove -f "$B_RUN/worktree/sp-mbase"
lcfix_seed sp-mbase CERTIFIED "$B_TIP"

out="$(SPIRA_HOME="$B_SH" SPIRA_RUN="$B_RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO_MAP="$B_SH/repo-map" \
    SPIRA_QUEUE_DIR="$B_QUEUEDIR" SPIRA_QUEUE_BATCH_WAIT=999999 \
    SPIRA_FORGE="$B_SH/forge-fixture.sh" PATH="$B_SH:$PATH" \
        batcher cut "$B_REPONAME" --round-vm "$B_SH/round-vm-stub.sh" 2>&1)"

want "batcher cut: a PR was opened for the master-based batch" "opened" "$out"
is "batcher cut: sp-mbase is IN_DELIVERY, not left CERTIFIED" "IN_DELIVERY" "$(lcfix_state sp-mbase)"
B_BATCH_BRANCH="$(grep '^branch=' "$B_QUEUEDIR/$B_REPONAME/open" 2>/dev/null | cut -d= -f2)"
if [ -n "$B_BATCH_BRANCH" ] \
   && git -C "$B_REPO" merge-base --is-ancestor master "refs/heads/$B_BATCH_BRANCH" 2>/dev/null; then
    ok "batcher cut: the batch branch was merged onto MASTER (spira_landref), not main"
else
    bad "batcher cut: the batch branch was merged onto MASTER (spira_landref), not main" \
        "branch=$B_BATCH_BRANCH"
fi

# ============================================================================
echo
echo "queue verdict: a green batch fast-forwards a MASTER-based remote:"
# ============================================================================
V_REPONAME=fixture-verdict
V_REPO="$TMP/verdict-repo"; V_REMOTE="$TMP/verdict-remote.git"
V_RUN="$TMP/verdict-run"; V_SH="$TMP/verdict-spira"
V_QUEUEDIR="$V_RUN/queue"

git init -q --bare -b master "$V_REMOTE"
git init -q -b master "$V_REPO"
git -C "$V_REPO" commit -q --allow-empty -m base
git -C "$V_REPO" remote add origin "$V_REMOTE"
git -C "$V_REPO" push -q origin master
git -C "$V_REPO" fetch -q origin
mkdir -p "$V_RUN/worktree" "$V_SH" "$V_QUEUEDIR/$V_REPONAME"
cp "$HERE"/*.sh "$HERE"/*.py "$V_SH/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$V_SH/"

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
printf '#!/usr/bin/env bash\ntrue\n' > "$V_SH/mail"; chmod +x "$V_SH/mail"
# Stubs are injected by NAME: the fixture home $V_SH goes first on PATH (sp-gypjk).
printf '#!/usr/bin/env bash\ntrue\n' > "$V_SH/testenv"; chmod +x "$V_SH/testenv"
printf '%s | %s | queue | origin/master | | |\n' "$V_REPONAME" "$V_REPO" > "$V_SH/repo-map"

# Build one member branch and an open batch record whose local merge commit sits
# on top of the current origin/master — the same shape build_batch() constructs
# in the retired test-verdict.sh, inlined here.
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
lcfix_seed sp-vbase CERTIFIED "$V_MEMBER_TIP"
V_CUT="$(spira-lc cut vtest-1 --repo "$V_REPONAME" --head "$V_BATCH_HEAD" --base "$V_BASE_SHA" \
    --members "sp-vbase:$V_MEMBER_TIP" --actor queue.sh 2>&1)"
want "verdict fixture: the batch is cut on spira-lc" "vtest-1" "$V_CUT"
V_LC_VERSION="$(spira-lc show-batch vtest-1 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin).get("version",""))' 2>/dev/null)"
{ printf 'batch_id=vtest-1\n'; printf 'version=%s\n' "$V_LC_VERSION"; } >> "$V_QUEUEDIR/$V_REPONAME/open"
printf 'green\nhead-sha: %s\n' "$V_BATCH_HEAD" > "$V_FORGE_STATUS_FILE"

out="$(SPIRA_HOME="$V_SH" SPIRA_RUN="$V_RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO_MAP="$V_SH/repo-map" \
    SPIRA_QUEUE_DIR="$V_QUEUEDIR" SPIRA_QUEUE_CI_MAXSEC=3600 SPIRA_QUEUE_CI_IDLE_SEC=600 \
    SPIRA_QUEUE_INFRA_RETRIES=2 SPIRA_FORGE="$V_SH/forge-fixture.sh" PATH="$V_SH:$PATH" \
        queue verdict "$V_REPONAME" 2>&1)"

is   "verdict: remote MASTER fast-forwards to the batch head" \
     "$V_BATCH_HEAD" "$(git -C "$V_REMOTE" rev-parse master 2>/dev/null)"
want "verdict: reports a fast-forward landing" "landed by fast-forward" "$out"
nowant "verdict: spira-lc refused no step of the land walk" "refused" "$out"
is "verdict: the batch is LANDED on spira-lc" "LANDED" \
    "$(lcfix_sql -q "SELECT state FROM batch WHERE batch_id='vtest-1'" -r csv 2>/dev/null | sed -n 2p)"

# ============================================================================
echo
echo "landing-pass land: a closed bead's branch lands (push mode) onto a MASTER base:"
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
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/suite-covers.sh" "$L_SH/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$L_SH/"
printf '#!/usr/bin/env bash\necho "gate: VERDICT=PASS reason=stub branch=$1 repo=${2:-?}" >&2\nexit 0\n' \
    > "$L_SH/gate.sh"; chmod +x "$L_SH/gate.sh"
printf '#!/usr/bin/env bash\nexit 0\n' > "$L_SH/confine.sh"; chmod +x "$L_SH/confine.sh"
printf '#!/usr/bin/env bash\nexit 0\n' > "$L_SH/skew"; chmod +x "$L_SH/skew"
lc_path_stub "$L_SH" "$TMP/lcfix-l"

testdb_seed <<'JSONL'
{"id":"sp-epic2","title":"epic","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
printf '{"id":"sp-lbase","title":"landing master-base","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-lbase","depends_on_id":"sp-epic2","type":"parent-child"}]}\n' \
    | testdb_seed

git -C "$L_REPO" worktree add -q -b spira/sp-lbase "$L_RUN/worktree/sp-lbase" master
printf 'sp-lbase\n' > "$L_RUN/worktree/sp-lbase/sp-lbase.txt"
git -C "$L_RUN/worktree/sp-lbase" add -A
git -C "$L_RUN/worktree/sp-lbase" commit -q -m "feat: sp-lbase — work"
lc_bead SUBMITTED sp-lbase "$(git -C "$L_RUN/worktree/sp-lbase" rev-parse HEAD)" 0   # the hand-off is the lifecycle row (sp-mve9i)

out="$(SPIRA_HOME="$L_SH" SPIRA_RUN="$L_RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO="$L_REPO" \
    SPIRA_REPO_MAP="$L_SH/repo-map-does-not-exist" PATH="$L_SH:$PATH" \
        landing-pass land 2>&1)"

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
