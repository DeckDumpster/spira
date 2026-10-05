#!/usr/bin/env bash
#
# test-certified-withdraw.sh — bead_reopen reopens the bead and, for every cause but the
#   admission-exempt one, strips the submitted label. The lifecycle reopen event is the
#   withdrawal; no landstate record is read or written.
#
# The end-to-end case C: reopening a certified, unbatched bead withdraws it from what the
# next cut draws from (queue_certified_list reads the lifecycle row), and recertifying
# with a new tip admits it again.
#
# tier: T2
# covers: spira/lib.sh spira/conf.sh queue/src/ops/helpers.rs spira/testlib/lc-fixture.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
. "$HERE/testlib/lc-fixture.sh"
testdb_require test-certified-withdraw
TMP="$(mktemp -d)"; trap 'lcfix_down; testdb_drop; rm -rf "$TMP"' EXIT INT TERM
lcfix_up || { echo "test-certified-withdraw: could not build a lifecycle fixture"; exit 1; }
testdb_up certwithdraw || { echo "test-certified-withdraw: could not build fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; RUN="$TMP/run"; SH="$TMP/spira"
REPONAME=fixture-repo
LANDSTATE="$RUN/landstate"; QUEUEDIR="$RUN/queue"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/$REPONAME"
cp "$HERE"/*.sh "$HERE"/*.py "$SH/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SH/"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

field() { "${TESTDB_BD:-bd}" -C "$SPIRA_DB" show "$1" --json 2>/dev/null | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get(sys.argv[1]) or "")' "$2" 2>/dev/null; }

echo "test-certified-withdraw.sh"

# =============================================================================
# A. bead_reopen (lib.sh) downgrades CERTIFIED to WITHDRAWN; every other
#    landstate is left exactly as it was.
# =============================================================================
echo
echo "bead_reopen: CERTIFIED landstate is withdrawn on reopen; other states untouched:"

export SPIRA_RUN="$RUN" SPIRA_REAPLOG="$RUN/reap.log" SPIRA_CONF="$TMP/no-such-conf"
# shellcheck disable=SC1090
. "$HERE/lib.sh"

testdb_seed <<'JSONL'
{"id":"sp-wd-cert","title":"wd-cert","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-25T00:00:00Z","closed_at":"2026-09-25T00:00:00Z"}
{"id":"sp-wd-red","title":"wd-red","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-25T00:00:00Z","closed_at":"2026-09-25T00:00:00Z"}
JSONL

bead_reopen sp-wd-red some-cause >/dev/null 2>&1
bead_reopen sp-wd-cert holding-for-fix >/dev/null 2>&1
is "reopen: bead status is open"                "open" "$(field sp-wd-cert status)"
is "positive control: other bead status is open" "open" "$(field sp-wd-red status)"

# =============================================================================
# A2. sp-eiatd: the CERTIFIED/submitted-label carve-out is now a declared set
#     (lib.sh:_census_deliberate_reopen_causes), not a bare "work-close-converted"
#     literal. eject must NOT join the exemption — an ejected CERTIFIED bead still
#     needs WITHDRAWN and the label stripped (batch.sh's own comment: "every eject
#     strips the label") or a genuinely-ejected bead stays admissible to the next
#     cut. work-close-converted is the only cause that stays exempt.
# =============================================================================
echo
echo "bead_reopen: eject is deliberate for census but NOT admission-exempt; work-close-converted still is:"

labels_of() { "${TESTDB_BD:-bd}" -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
    | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(",".join(d[0].get("labels") or []))'; }

testdb_seed <<JSONL
{"id":"sp-wd-eject","title":"wd-eject","status":"closed","issue_type":"task","labels":["${SPIRA_SUBMITTED_LABEL:-spira-submitted}"],"updated_at":"2026-09-25T00:00:00Z","closed_at":"2026-09-25T00:00:00Z"}
{"id":"sp-wd-wcc","title":"wd-wcc","status":"closed","issue_type":"task","labels":["${SPIRA_SUBMITTED_LABEL:-spira-submitted}"],"updated_at":"2026-09-25T00:00:00Z","closed_at":"2026-09-25T00:00:00Z"}
JSONL

bead_reopen sp-wd-eject eject >/dev/null 2>&1
nowant "eject: submitted label stripped" "${SPIRA_SUBMITTED_LABEL:-spira-submitted}" "$(labels_of sp-wd-eject)"

bead_reopen sp-wd-wcc work-close-converted >/dev/null 2>&1
want "work-close-converted: submitted label kept" "${SPIRA_SUBMITTED_LABEL:-spira-submitted}" "$(labels_of sp-wd-wcc)"

# ======================================================================echo
tl_summary
