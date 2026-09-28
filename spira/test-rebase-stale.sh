#!/usr/bin/env bash
# test-rebase-stale.sh — rebase-stale.sh rebases a submitted branch stale against its
# landing ref mechanically, so a bead reaches an aeon only for a genuine content conflict
# or a red gate at the rebased tip.
#
# Cases: an append-only key-history conflict resolves with no aeon involvement and is
# re-certified at its new tip (the plant proves the union resolver actually ran: both
# sides' appended lines survive); a real same-line code conflict aborts the rebase and
# reopens the bead with the conflicting hunk quoted, leaving the branch at its pre-rebase
# tip; a branch that rebases cleanly but fails its gate at the new tip is reopened and
# restored to its pre-rebase tip rather than left pointing at an uncertified rewrite.
#
# The gate is a stub counter whose verdict is steered by a control file, exactly as
# test-certify.sh's — a real gate is exercised elsewhere; this suite is about the rebase
# and certify plumbing, not suite content
# (law-a-check-that-finds-nothing-must-first-prove-it-could-have-found-something): the
# gate-red case is the plant that proves the counter and the restore both fire.
#
# confine.sh is a stub; the real db is testdb.sh with an embedded engine. The bare remote
# is real git so ancestry and rebase are real.
#
# covers: spira/rebase-stale.sh spira/mech-resolve.sh spira/keylist-union.py spira/comment-union.py
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-rebase-stale
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up rebasestale || { echo "test-rebase-stale: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"
REPONAME=fixture-repo
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
mkdir -p "$REPO/spira-config/schema"
printf 'a\nb\n' > "$REPO/spira-config/schema/spira-key-history.txt"
printf 'line1\n' > "$REPO/f.txt"
git -C "$REPO" add -A
git -C "$REPO" commit -q -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH"

cp "$HERE"/*.sh "$HERE"/*.py "$SH/" 2>/dev/null
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub confine.sh 'exit 0'
stub gh 'exit 1'

# THE GATE IS ALSO THE COUNTER. Its verdict per branch is steered by a control file — PASS
# unless a case wrote FAIL for that branch name into $TMP/gate-verdicts/spira/<id>.
GATE_COUNT="$TMP/gate-count"
mkdir -p "$TMP/gate-verdicts/spira"
stub gate.sh '
printf "%s\n" "$1" >> "'"$GATE_COUNT"'"
v="'"$TMP"'/gate-verdicts/$1"
mode="PASS"; [ -f "$v" ] && mode="$(cat "$v")"
case "$mode" in
FAIL)
    printf "gate: VERDICT=FAIL reason=suite-red suite=test-stub branch=%s repo=%s\n" "$1" "${2:-?}" >&2
    exit 1 ;;
*)
    printf "gate: VERDICT=PASS reason=stub branch=%s repo=%s\n" "$1" "${2:-?}" >&2
    exit 0 ;;
esac'

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

B() { bd -C "$SPIRA_DB" "$@"; }
status_of() {
    B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'
}
notes_of() {
    B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("notes") or "")'
}
landstate() { cat "$RUN/landstate/${1:-}" 2>/dev/null; }
gate_n()    { [ -f "$GATE_COUNT" ] && wc -l < "$GATE_COUNT" || echo 0; }
rslog()     { cat "$RUN/rebase-stale.log" 2>/dev/null; }

seed_bead() {
    testdb_seed <<JSONL
{"id":"$1","title":"$1","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z"}
JSONL
}

# make_branch <id> <start-point> — cuts spira/<id> from <start-point> through a throwaway
# worktree that is removed once the commit lands, so the branch is never left checked
# out (rebase-stale.sh refuses a branch a live worktree still holds). Caller populates
# the worktree's files before calling commit_branch.
new_branch_wt() {
    local id="$1" start="$2"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/mk-$id" "$start"
}
commit_branch() {
    local id="$1" msg="$2" wt="$RUN/worktree/mk-$id"
    git -C "$wt" add -A
    git -C "$wt" commit -q -m "$msg"
    git -C "$REPO" worktree remove --force "$wt"
}
advance_main() {
    local wt="$RUN/worktree/mk-main-$RANDOM"
    git -C "$REPO" worktree add -q "$wt" main
    "$@" "$wt"
    git -C "$wt" add -A
    git -C "$wt" commit -q -m "advance main"
    git -C "$wt" push -q origin main
    git -C "$REPO" worktree remove --force "$wt"
    git -C "$REPO" fetch -q origin
}

run_rebase_stale() {
    env SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
        SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$REPONAME" SPIRA_REPO_MAP="$SH/repo-map" \
        SPIRA_REBASE_STALE_LOG="$RUN/rebase-stale.log" \
        bash "$SH/rebase-stale.sh" "$1" "$REPONAME"
}

echo "test-rebase-stale.sh"

# -----------------------------------------------------------------------------------------
# CASE 1: append-only key-history conflict — resolved mechanically, no aeon.
# -----------------------------------------------------------------------------------------
testdb_reset
seed_bead sp-rbkh1
new_branch_wt sp-rbkh1 main
printf 'c-from-branch\n' >> "$RUN/worktree/mk-sp-rbkh1/spira-config/schema/spira-key-history.txt"
commit_branch sp-rbkh1 "feat: sp-rbkh1 — work"
advance_main bash -c 'printf "c-from-main\n" >> "$1/spira-config/schema/spira-key-history.txt"' _

rm -f "$GATE_COUNT" "$RUN/rebase-stale.log"
out="$(run_rebase_stale sp-rbkh1)"; rc=$?
is   "case1: exit 0"                        "0"          "$rc"
want "case1: reports mechanical"            "mechanical" "$out"
is   "case1: gate ran once (re-certify)"    "1"          "$(gate_n)"
is   "case1: branch now contains origin/main" "yes" \
     "$(git -C "$REPO" merge-base --is-ancestor origin/main spira/sp-rbkh1 2>/dev/null && echo yes || echo no)"
content="$(git -C "$REPO" show spira/sp-rbkh1:spira-config/schema/spira-key-history.txt 2>/dev/null)"
if printf '%s' "$content" | grep -qF 'c-from-branch' && printf '%s' "$content" | grep -qF 'c-from-main'; then
    ok "case1: union kept both appended lines"
else
    bad "case1: union kept both appended lines" "got: $content"
fi
is   "case1: bead stays closed"             "closed"     "$(status_of sp-rbkh1)"
want "case1: note records the mechanical rebase" "mechanical" "$(notes_of sp-rbkh1)"
case "$(landstate sp-rbkh1)" in
    CERTIFIED*) ok "case1: landstate CERTIFIED" ;;
    *)          bad "case1: landstate CERTIFIED" "got: $(landstate sp-rbkh1)" ;;
esac
want "case1: log records a mechanical outcome" "outcome=mechanical" "$(rslog)"

# -----------------------------------------------------------------------------------------
# CASE 2: a real same-line conflict — returned to an aeon with the hunk quoted.
# -----------------------------------------------------------------------------------------
testdb_reset
seed_bead sp-rbcf1
new_branch_wt sp-rbcf1 main
printf 'line1-branch\n' > "$RUN/worktree/mk-sp-rbcf1/f.txt"
commit_branch sp-rbcf1 "feat: sp-rbcf1 — work"
advance_main bash -c 'printf "line1-main\n" > "$1/f.txt"' _

old_tip="$(git -C "$REPO" rev-parse spira/sp-rbcf1)"
rm -f "$GATE_COUNT" "$RUN/rebase-stale.log"
out="$(run_rebase_stale sp-rbcf1)"; rc=$?
is "case2: exit 1"                       "1"       "$rc"
is "case2: branch left at its pre-rebase tip" "$old_tip" "$(git -C "$REPO" rev-parse spira/sp-rbcf1)"
is "case2: gate never ran"               "0"       "$(gate_n)"
is "case2: bead is no longer closed" "no" "$([ "$(status_of sp-rbcf1)" = closed ] && echo yes || echo no)"
notes="$(notes_of sp-rbcf1)"
hunk_ok=1
printf '%s' "$notes" | grep -qF '<<<<<<<' || hunk_ok=0
printf '%s' "$notes" | grep -qF 'line1-branch' || hunk_ok=0
printf '%s' "$notes" | grep -qF 'line1-main' || hunk_ok=0
if [ "$hunk_ok" = 1 ]; then ok "case2: note quotes the conflicting hunk"
else bad "case2: note quotes the conflicting hunk" "got: $notes"; fi
want "case2: log records the conflict outcome" "outcome=conflict" "$(rslog)"

# -----------------------------------------------------------------------------------------
# CASE 3: a clean rebase that fails its gate at the new tip — returned, not landed.
# -----------------------------------------------------------------------------------------
testdb_reset
seed_bead sp-rbgr1
new_branch_wt sp-rbgr1 main
printf 'branch-only\n' > "$RUN/worktree/mk-sp-rbgr1/branch-only.txt"
commit_branch sp-rbgr1 "feat: sp-rbgr1 — work"
advance_main bash -c 'printf "main-only\n" > "$1/main-only.txt"' _

old_tip="$(git -C "$REPO" rev-parse spira/sp-rbgr1)"
printf 'FAIL\n' > "$TMP/gate-verdicts/spira/sp-rbgr1"
rm -f "$GATE_COUNT" "$RUN/rebase-stale.log"
out="$(run_rebase_stale sp-rbgr1)"; rc=$?
is "case3: exit 2"                             "2"       "$rc"
is "case3: gate ran once"                      "1"       "$(gate_n)"
is "case3: branch restored to its pre-rebase tip" "$old_tip" "$(git -C "$REPO" rev-parse spira/sp-rbgr1)"
is "case3: bead is no longer closed" "no" "$([ "$(status_of sp-rbgr1)" = closed ] && echo yes || echo no)"
want "case3: note quotes the gate failure"      "VERDICT=FAIL" "$(notes_of sp-rbgr1)"
want "case3: log records the gate-red outcome"  "outcome=gate-red" "$(rslog)"

tl_summary
