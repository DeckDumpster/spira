#!/usr/bin/env bash
# timeout: 150
#
# test-groomer-incident-reroute.sh — groomer sweep reroutes an incident bead whose branch
#   already carries a commit to the builders' partition (sp-18v9k).
#
# THE ROOT CAUSE. "sp-7zg1j is filed as an Ops incident, but what's left is a code change" was
# said by a Concierge session reading the bead by hand, three separate times before this
# (sp-awm1q, sp-qlcvc, sp-c1ot2). Ops (ops.fayth) has no Edit or Write tool, so it can never
# be the one who put that commit there — the commit's presence on an incident bead's own
# branch is a fully computable fact, not a judgment call.
#
# TWO PAIRS, each proving the predicate needs the COMMIT, not merely the branch: label
# (law-absence-needs-a-positive-control):
#   sp-ir-code     — incident, branch: label names a ref with a real commit ahead of base
#                    → REROUTED: incident label removed, plan label added, noted.
#   sp-ir-nocommit — incident, branch: label names a real ref, but it IS the base (no commit)
#                    → left alone: still incident, no plan label.
#   sp-ir-nobranch — incident, no branch: label at all → left alone.
#
# REAL GIT REPO, REAL TESTDB (law-prefer-the-real-dependency): the predicate reads git
# ancestry (rev-list --count), which a stub cannot stand in for.
#
# tier: T2
# defect: sp-18v9k
# covers: groomer/src/* spira/lib.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testdb.sh"
testdb_require test-groomer-incident-reroute
TMP="$(mktemp -d)"
testdb_up increroute || { echo "test-groomer-incident-reroute: could not build fixture database"; exit 1; }
trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'exit 143' INT TERM

. "$HERE/testlib.sh"

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; RUN="$TMP/run"; mkdir -p "$RUN"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
git -C "$REPO" remote set-head origin main

# PINNED TO A NON-DEFAULT NAME so the suite cannot pass on an accidentally matching literal.
REPONAME=increrouterepo
printf '%s | %s | push | main | |\n' "$REPONAME" "$REPO" > "$TMP/repo-map"

run_sweep() {
    env -i PATH="$PATH" HOME="$HOME" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" \
        SPIRA_HOME="$HERE" SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$REPONAME" \
        SPIRA_BD="${SPIRA_BD:-bd}" \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_RUN="$RUN" \
        SPIRA_REPO_MAP="$TMP/repo-map" \
        SPIRA_ASK_LABEL=needs-ryan \
        SPIRA_CI_LABEL=awaiting-ci \
        SPIRA_SPIKE_LABEL=spike \
        SPIRA_SCOPE_LABEL=spira \
        groomer sweep "$@" 2>&1
}

labels_of() { bd -C "$SPIRA_DB" label list "$1" 2>/dev/null | tr '\n' ' '; }
notes_of()  { bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | python3 -c '
import json,sys; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
print(d[0].get("notes") or "")' 2>/dev/null; }

echo "test-groomer-incident-reroute.sh"

# ==========================================================================================
echo
echo "FIXTURE — a branch with a real commit, a branch that is the base, and no branch at all"
# ==========================================================================================
testdb_reset

BASE_SHA="$(git -C "$REPO" rev-parse origin/main)"

# sp-ir-code: branch cut from base, one real commit on top.
git -C "$REPO" branch -q sp-ir-code-branch "$BASE_SHA"
git -C "$REPO" worktree add -q "$TMP/wt-code" sp-ir-code-branch
printf 'fix\n' > "$TMP/wt-code/FIX_FILE"
git -C "$TMP/wt-code" add FIX_FILE
git -C "$TMP/wt-code" commit -q -m "sp-ir-code: the actual fix"
git -C "$REPO" worktree remove --force "$TMP/wt-code"

# sp-ir-nocommit: branch cut from base, no commits of its own — a claim assigned the bead
# a branch: label (every aeon.sh claim does) but nobody has written anything yet.
git -C "$REPO" branch -q sp-ir-empty-branch "$BASE_SHA"

testdb_seed <<JSONL
{"id":"sp-epic","title":"epic","status":"open","issue_type":"epic","labels":["spira"]}
{"id":"sp-ir-code","title":"incident with a real fix already committed","status":"open","issue_type":"task","labels":["spira","incident","repo:$REPONAME","branch:sp-ir-code-branch"],"dependencies":[{"issue_id":"sp-ir-code","depends_on_id":"sp-epic","type":"parent-child"}]}
{"id":"sp-ir-nocommit","title":"incident claimed but nothing written yet","status":"open","issue_type":"task","labels":["spira","incident","repo:$REPONAME","branch:sp-ir-empty-branch"],"dependencies":[{"issue_id":"sp-ir-nocommit","depends_on_id":"sp-epic","type":"parent-child"}]}
{"id":"sp-ir-nobranch","title":"incident never claimed, no branch label","status":"open","issue_type":"task","labels":["spira","incident","repo:$REPONAME"],"dependencies":[{"issue_id":"sp-ir-nobranch","depends_on_id":"sp-epic","type":"parent-child"}]}
JSONL

# ==========================================================================================
echo
echo "DRY-RUN — names the reroute, changes nothing"
# ==========================================================================================
: > "$RUN/groom.log"
out="$(run_sweep --dry-run)"
is "dry-run: exits 0" 0 "$?"
want   "dry-run: names sp-ir-code as incident-is-code" "incident-is-code"     "$out"
want   "dry-run: REROUTED sp-ir-code"                  "REROUTED sp-ir-code" "$out"
nowant "dry-run: sp-ir-nocommit not named"              "sp-ir-nocommit"      "$out"
nowant "dry-run: sp-ir-nobranch not named"              "sp-ir-nobranch"      "$out"

want  "dry-run: sp-ir-code still incident (no change)" "incident" "$(labels_of sp-ir-code)"
nowant "dry-run: sp-ir-code not yet plan"              "plan"     "$(labels_of sp-ir-code)"

# ==========================================================================================
echo
echo "REAL RUN — sweep reroutes the coded incident, leaves the other two alone"
# ==========================================================================================
: > "$RUN/groom.log"
out="$(run_sweep)"
is "real run: exits 0" 0 "$?"

# ---- positive: sp-ir-code moved from incident to plan ----
nowant "sp-ir-code: incident label removed" "incident" "$(labels_of sp-ir-code)"
want   "sp-ir-code: plan label added"       "plan"      "$(labels_of sp-ir-code)"
want   "sp-ir-code: note explains the move" "incident -> plan" "$(notes_of sp-ir-code)"
want   "sp-ir-code: note names the commit count" "1 commit" "$(notes_of sp-ir-code)"

# ---- negative control 1: branch exists, no commit ahead — stays incident ----
want   "sp-ir-nocommit: still incident" "incident" "$(labels_of sp-ir-nocommit)"
nowant "sp-ir-nocommit: not moved to plan" "plan"   "$(labels_of sp-ir-nocommit)"

# ---- negative control 2: no branch: label at all — stays incident ----
want   "sp-ir-nobranch: still incident" "incident" "$(labels_of sp-ir-nobranch)"
nowant "sp-ir-nobranch: not moved to plan" "plan"   "$(labels_of sp-ir-nobranch)"

# ---- groom.log carries the reroute ----
groom_log="$(cat "$RUN/groom.log" 2>/dev/null)"
want "groom.log names the reroute" "REROUTED sp-ir-code" "$groom_log"

echo
tl_summary
