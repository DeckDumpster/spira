#!/usr/bin/env bash
#
# test-unclaimable-worktree.sh — detect_unclaimable_ready uses production config, not the
#                                calling worktree's conf.sh
#
# THE DEFECT (sp-b0j0s / sp-recur-unclaimable). detect_unclaimable_ready derived fayth
# label requirements from whatever checkout sourced lib.sh, while filing incidents into the
# shared production database. An aeon working on conf.sh — renaming SPIRA_PLAN_LABEL's
# default from "plan" to "partition:plan" — caused the detector to flag 24 beads as
# UNCLAIMABLE (against the production checkout's 1) because no bead carried the new label.
# 40 P1 incidents were filed in ~2 minutes; ops aeons "fixed" 40 correct beads by adding
# a label no production code reads.
#
# THE FIX. detect_unclaimable_ready checks whether it was called from a non-main worktree
# (_spira_gitstore reveals the shared .git dir; its parent is the main checkout). If the
# caller's SPIRA_REPO differs from that main checkout, the function re-runs via the main
# checkout's lib.sh with partition-label env vars unset, so production conf.sh defaults
# take effect.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control). A genuinely unclaimable bead
# must still be flagged even when called from the worktree context. A detector that silences
# everything passes every negative assertion here.
#
# WORKTREE SETUP. A fresh git repo is created in TMP (the "production" harness) with the
# spira directory copied in. A worktree is then branched from it with a modified conf.sh.
# This avoids relying on the test environment's workspace git (which is itself a worktree
# with .git pointing to paths the container cannot see).
#
# covers: spira/lib.sh spira/conf.sh
# defect: sp-b0j0s
# hermetic-ok: fixture database; fresh git repo in TMP, no system git state
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "$2"; }
has()  { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
lacks(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-unclaimable-worktree
TMP="$(mktemp -d)"

# Build a self-contained git repo to serve as the "production" harness, then create a
# worktree of it with a modified conf.sh. This topology mirrors production without relying
# on the test environment's workspace git (which is a worktree referencing host paths the
# container cannot reach).
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# SPIRA_SCOPE_LABEL derives from the git repo's base name via SPIRA_HOME_REPO. Inside the
# test container the workspace git (a linked worktree) has a broken gitdir link, so the
# automatic derivation can produce "workspace" instead of "spira". Pin SPIRA_HOME_REPO
# here so every context—outer test, fake worktree, production re-run—agrees on "spira",
# and name PROD_ROOT to match so conf.sh derives the same value when re-run from it.
export SPIRA_HOME_REPO=spira

PROD_ROOT="$TMP/spira"
FAKE_WT="$TMP/fake-worktree"
mkdir -p "$PROD_ROOT"
git init -q -b main "$PROD_ROOT" 2>/dev/null || git init -q "$PROD_ROOT" 2>/dev/null
git -C "$PROD_ROOT" config user.email t@t
git -C "$PROD_ROOT" config user.name t
cp -r "$HERE" "$PROD_ROOT/spira"
git -C "$PROD_ROOT" add spira
git -C "$PROD_ROOT" commit -qm "seed"
git -C "$PROD_ROOT" worktree add --detach "$FAKE_WT" HEAD 2>/dev/null
# Patch the worktree's conf.sh to simulate an aeon branch that renamed the partition labels.
sed -i \
    -e 's/: "${SPIRA_PLAN_LABEL:=plan}"/: "${SPIRA_PLAN_LABEL:=partition:plan}"/' \
    -e 's/: "${SPIRA_INCIDENT_LABEL:=incident}"/: "${SPIRA_INCIDENT_LABEL:=partition:incident}"/' \
    "$FAKE_WT/spira/conf.sh"

cleanup() {
    testdb_drop
    git -C "$PROD_ROOT" worktree remove --force "$FAKE_WT" 2>/dev/null || true
    rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

testdb_up unclaimable_wt || { echo "test-unclaimable-worktree: could not build fixture database"; exit 1; }

# Source production lib.sh so detect_unclaimable_ready is available for case 3.
export SPIRA_HOME="$HERE"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_CONF="$TMP/no-such.conf"
acted=0; progressed=0
act()      { acted=$((acted+1)); }
progress() { progressed=$((progressed+1)); act "$@"; }
log()      { : ; }
# shellcheck disable=SC1090
. "$HERE/lib.sh"

echo "test-unclaimable-worktree.sh"

# Helper: run detect_unclaimable_ready from the fake worktree's lib.sh.
# Unsets SPIRA_HOME so conf.sh derives it from BASH_SOURCE (the fake lib.sh path),
# not from this test's exported SPIRA_HOME. Also unsets label vars so the modified
# conf.sh sets them to partition:plan/incident. Keeps SPIRA_DB for the fixture.
run_from_worktree() {
    env -u SPIRA_HOME \
        -u SPIRA_PLAN_LABEL -u SPIRA_INCIDENT_LABEL \
        -u SPIRA_SCOPE_LABEL -u SPIRA_CI_LABEL \
        -u SPIRA_ASK_LABEL -u SPIRA_NO_LOOP_LABEL \
        -u SPIRA_CZAR_LABEL -u SPIRA_GROOMER_LABEL \
        -u SPIRA_MAECHEN_LABEL -u SPIRA_SPIKE_LABEL \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_CONF="$TMP/no-such.conf" \
        bash -c ". \"$FAKE_WT/spira/lib.sh\"; detect_unclaimable_ready" 2>/dev/null
}

# ==========================================================================================
echo
echo "case 1 — worktree with wrong SPIRA_PLAN_LABEL does not flag a claimable bead (the fix)"
# ==========================================================================================
# A bead with label "plan" is claimable under the production config. Before this fix, called
# from a worktree expecting "partition:plan", it was wrongly flagged UNCLAIMABLE.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-wt1a","title":"claimable builder bead","status":"open","issue_type":"task","labels":["plan","repo:spira","${SPIRA_SCOPE_LABEL}"]}
JSONL

wt_out="$(run_from_worktree)"
lacks "claimable bead not flagged when sourced from worktree with partition: labels" \
    "sp-wt1a" "$wt_out"

# ==========================================================================================
echo
echo "case 2 — positive control: a truly unclaimable bead is still flagged from the worktree"
# ==========================================================================================
# The fix must not suppress legitimate unclaimable detection. A bead with only the scope
# label (no partition label) has no persona to claim it and must still produce UNCLAIMABLE.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-wt2a","title":"truly unclaimable: no partition label","status":"open","issue_type":"task","labels":["repo:spira","${SPIRA_SCOPE_LABEL}"]}
JSONL

wt_out2="$(run_from_worktree)"
has "truly unclaimable bead still flagged from worktree context" \
    "UNCLAIMABLE sp-wt2a" "$wt_out2"

# ==========================================================================================
echo
echo "case 3 — worktree and production checkout agree on a mixed queue"
# ==========================================================================================
# Both a claimable and an unclaimable bead are present. The worktree result must match
# the production checkout's result exactly — same bead flagged, same bead not flagged.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-wt3a","title":"claimable: plan label","status":"open","issue_type":"task","labels":["plan","repo:spira","${SPIRA_SCOPE_LABEL}"]}
{"id":"sp-wt3b","title":"unclaimable: no partition","status":"open","issue_type":"task","labels":["repo:spira","${SPIRA_SCOPE_LABEL}"]}
JSONL

prod_out="$(detect_unclaimable_ready 2>/dev/null)"
wt_out3="$(run_from_worktree)"

lacks "production: claimable bead not flagged"  "sp-wt3a" "$prod_out"
has   "production: unclaimable bead flagged"     "UNCLAIMABLE sp-wt3b" "$prod_out"
lacks "worktree: claimable bead not flagged"     "sp-wt3a" "$wt_out3"
has   "worktree: unclaimable bead flagged"       "UNCLAIMABLE sp-wt3b" "$wt_out3"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
