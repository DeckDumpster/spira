#!/usr/bin/env bash
#
# test-landing-midpass-close.sh — a bead that closes DURING a landing pass, after the
# bulk bdjson show land_repo takes at the top of the function, is certified in the SAME
# pass rather than waiting a whole pass for the next snapshot (sp-ob7uq, sp-ceemq).
#
# sp-anchor is closed from the start, so it gates first in this test's serial path
# (SPIRA_CERTIFY_PAR=1, for a deterministic single-candidate-at-a-time dispatch order).
# The gate.sh stub closes sp-midp0 as a side effect of gating sp-anchor — standing in for
# "an aeon closes a P0 while the pass is already running" without needing two real
# processes racing each other. sp-midp0's own bead is OPEN at scan time, so without the
# fix it would be invisible for the rest of this pass; a second landing() call would be
# needed to certify it.
#
# POSITIVE CONTROL: a third bead, sp-neverclose, stays open for the whole pass (nothing
# closes it) and must NOT be certified — proving the admission path fires because bd
# reports the bead closed, not because every open branch gets certified regardless
# (law-absence-needs-a-positive-control).
#
# defect: sp-ob7uq
# covers: spira/landing.sh spira/landing-lib.sh
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-landing-midpass-close
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up landing-midpass-close || {
    printf 'SKIP test-landing-midpass-close: no testdb available\n' >&2
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

cp "$HERE"/*.sh "$HERE"/*.py "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub confine.sh 'exit 0'
stub mail.sh '[ "${1:-}" = send ] || exit 0; printf "%s\n" "$*" >> "$EMITTED"; cat >> "$EMITTED"; printf "\n" >> "$EMITTED"'
export EMITTED="$TMP/events"; : > "$EMITTED"
# Gating sp-anchor closes sp-midp0 as a side effect, standing in for a bead that closes
# while the pass this stub is running inside of is already under way.
stub gate.sh 'if [ "$1" = "spira/sp-anchor" ]; then
    bd -C "$SPIRA_DB" close sp-midp0 --reason "closed mid-pass by test stub" >/dev/null 2>&1 || true
fi
echo "gate: VERDICT=PASS reason=stub branch=$1 repo=${2:-?}" >&2
exit 0'
stub gh 'exit 1'
stub queue.sh 'exit 0'

cat > "$SH/repo-map" <<MAP
$REPONAME | $REPO | queue | |
MAP

B() { bd -C "$SPIRA_DB" "$@"; }

landing() {
    rm -f "$RUN/landing.progress"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" SPIRA_ID_PREFIX=sp SPIRA_CERTIFY_PAR=1 \
    SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/gh" \
        bash "$SH/landing.sh" 2>&1
}

drop_branch() {
    local id="$1"
    git -C "$REPO" worktree remove --force "$RUN/worktree/$id" >/dev/null 2>&1 || true
    git -C "$REPO" branch -D "spira/$id" >/dev/null 2>&1 || true
}

# branch_at <id> <status> [closed_at] — a bead with a git branch, either closed already
# (status=closed, closed_at required) or still open (status=in_progress, no closed_at).
branch_at() {
    local id="$1" st="$2" cat="${3:-}"
    drop_branch "$id"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$id" > "$RUN/worktree/$id/$id.txt"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id — work"
    if [ "$st" = closed ]; then
        printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","priority":1,"labels":[],"updated_at":"%s","closed_at":"%s","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
            "$id" "$id" "$cat" "$cat" "$id" | testdb_seed
    else
        printf '{"id":"%s","title":"%s","status":"in_progress","issue_type":"task","priority":1,"labels":[],"updated_at":"2026-09-01T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
            "$id" "$id" "$id" | testdb_seed
    fi
}

seed() {
    rm -rf "$RUN/submitted"
    testdb_reset
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-01T00:00:00Z"}
JSONL
    branch_at sp-anchor closed "2026-09-01T00:00:00Z"
    branch_at sp-midp0 in_progress
    branch_at sp-neverclose in_progress
}

echo "test-landing-midpass-close.sh"
seed
out="$(landing)"

# POSITIVE CONTROL: without the fix's admission path, nothing here would ever certify
# sp-midp0 in one pass — this line fails first if the fix is reverted, so a silent
# "everything certified" bug cannot pass unnoticed (law-a-regression-test-must-be-seen-to-fail).
want "sp-anchor (closed at scan time) certifies as an ordinary candidate" \
    "certified spira/sp-anchor" "$out"
want "sp-midp0 (closed mid-pass, after the scan) is admitted and certified in the SAME pass" \
    "certified spira/sp-midp0" "$out"
want "the admission is logged against the bead that closed mid-pass" \
    "CHECK6 sp-midp0: closed mid-pass — admitting spira/sp-midp0" "$out"

# POSITIVE CONTROL: sp-neverclose stays open for the whole pass. It must not be certified —
# proving admission is conditional on bd reporting the bead closed, not unconditional.
nowant "sp-neverclose (never closes) is NOT certified" "certified spira/sp-neverclose" "$out"

st_midp0="$(B show sp-midp0 --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")')"
is "sp-midp0's bead is closed (by the stub, not reopened by landing)" "closed" "$st_midp0"

drop_branch sp-anchor; drop_branch sp-midp0; drop_branch sp-neverclose

tl_summary
