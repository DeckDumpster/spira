#!/usr/bin/env bash
#
# test-landing-rebase.sh — rebase_branch classification (no-branch, no-base, conflict,
# rebase-refused, identity fix, fence), the survivor sweep (rebased when a landing moves
# the base under a withheld branch), the loop-defer guard (live aeon holds the bead), and
# the checkout refresh (skew.sh keeps the operator's working copy current).
#
# Extracted from test-landing.sh to reduce the critical-path suite time.
#
# covers: spira/landing.sh spira/lib.sh spira/skew.sh
# timeout: 240
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-landing-rebase
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
export SPIRA_TESTDB_MODE=server
testdb_up landing-rebase || {
    printf 'SKIP test-landing-rebase: server testdb not available\n' >&2
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

cp "$HERE"/*.sh "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub confine.sh 'exit 0'
stub mail.sh '[ "${1:-}" = send ] || exit 0; printf "%s\n" "$*" >> "$EMITTED"; cat >> "$EMITTED"; printf "\n" >> "$EMITTED"'
export EMITTED="$TMP/events"; : > "$EMITTED"
stub gh 'exit 1'

stub gate.sh '
r="$SPIRA_RUN/reap-during-gate"
if [ -s "$r" ]; then
    while read -r id; do
        [ -n "$id" ] || continue
        git -C "'"$REPO"'" worktree remove --force "'"$RUN"'/worktree/$id" >/dev/null 2>&1
        git -C "'"$REPO"'" branch -D "spira/$id" >/dev/null 2>&1
    done < "$r"
    : > "$r"
fi
mkdir -p "$SPIRA_RUN/tip-at-gate"
git -C "'"$REPO"'" rev-parse "$1" > "$SPIRA_RUN/tip-at-gate/${1//\//-}" 2>/dev/null
w="$SPIRA_RUN/withhold-gate"
if [ -s "$w" ] && grep -qx "$1" "$w"; then
    echo "gate: VERDICT=NO_VERDICT reason=stub-busy branch=$1 repo=${2:-?}" >&2
    exit "${SPIRA_GATE_NOVERDICT:?the gate protocol constant is not in the environment}"
fi
c="$SPIRA_RUN/claim-during-gate"
if [ -s "$c" ]; then
    while read -r id pid; do
        [ -n "$id" ] || continue
        printf "%s\\n" "$pid" > "$SPIRA_RUN/aeon-builder-$id.pid"
    done < "$c"
    : > "$c"
fi
echo "gate: VERDICT=PASS reason=${GATE_REASON:-stub} branch=$1 repo=${2:-?}" >&2; exit 0'

cp "$SH/gate.sh" "$TMP/gate-full.sh"

cat > "$SH/repo-map" <<MAP
$REPONAME | $REPO | push | |
MAP

B() { bd -C "$SPIRA_DB" "$@"; }
status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }
notes_of() { B show "$1" 2>/dev/null; }

landing() {
    rm -f "$RUN/landing.progress"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" SPIRA_ID_PREFIX=sp \
    SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/gh" \
        bash "$SH/landing.sh" 2>&1
}

seed() {
    testdb_reset
    rm -rf "$RUN/tip-at-gate"; rm -f "$RUN/withhold-gate" "$RUN/claim-during-gate"
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
}

branch() {
    local id="$1" f="${2:-$1.txt}" c="${3:-$1}"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$c" > "$RUN/worktree/$id/$f"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id — work"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$id" "$id" "$id" | testdb_seed
}

drop_branch() {
    local id="$1"
    git -C "$REPO" worktree remove --force "$RUN/worktree/$id" >/dev/null 2>&1
    git -C "$REPO" branch -D "spira/$id" >/dev/null 2>&1
}

withhold_gate() { printf 'spira/%s\n' "$@" > "$RUN/withhold-gate"; }
claim_during_gate() { printf '%s %s\n' "$1" "$2" > "$RUN/claim-during-gate"; }
tip_at_gate()   { cat "$RUN/tip-at-gate/spira-$1" 2>/dev/null; }
tip_of()        { git -C "$REPO" rev-parse "spira/$1" 2>/dev/null; }
on_base()       { git -C "$REPO" merge-base --is-ancestor origin/main "spira/$1" 2>/dev/null && echo yes || echo no; }

echo "test-landing-rebase.sh"

# --------------------------------------------------------------------------------------
# THE CLASSIFICATION BOTH GUARDS REST ON. rebase_branch returns 1 four ways and only one of
# them is a fact about the branch; before it said which, every caller that reopens on a
# rebase failure reopened for all four. Asserted directly, because the pass can only be
# steered into two of these and a guard reading a value nothing pins is a guard on a comment.
# --------------------------------------------------------------------------------------
echo
classify() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO="$REPO" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    bash -c '. "$1/lib.sh" >/dev/null 2>&1
             if rebase_branch "$2" "$3" "$4" fixture >/dev/null 2>&1
             then printf clean; else printf "%s" "${REBASE_FAILURE:-unset}"; fi' \
        _ "$SH" "$1" "$2" "$REPO" 2>/dev/null
}
seed; branch sp-kind
is "a ref that is not there is named no-branch" no-branch "$(classify spira/sp-nothere origin/main)"
is "a base that does not resolve is named no-base" no-base "$(classify spira/sp-kind refs/heads/no-such-base)"
is "a branch that rebases cleanly records no failure" clean "$(classify spira/sp-kind origin/main)"
drop_branch sp-kind

seed; branch sp-kindclash shared.txt "from the branch"
printf '%s\n' "and the base disagrees" > "$REPO/shared.txt"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m "base writes shared.txt again"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
is "and a real disagreement is named conflict" conflict "$(classify spira/sp-kindclash origin/main)"
drop_branch sp-kindclash

# THE REFUSED CASE: git declines to rebase (untracked file would be overwritten) without
# leaving any unmerged file. A non-conflict rebase failure must not reopen a finished bead,
# so it needs a name that is not "conflict". The fix sets REBASE_FAILURE=rebase-refused and
# captures git's first stderr line in REBASE_REFUSED_REASON.
classify_ext() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO="$REPO" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    bash -c '. "$1/lib.sh" >/dev/null 2>&1
             rebase_branch "$2" "$3" "$4" fixture >/dev/null 2>&1
             printf "%s|%s|%s" \
                 "${REBASE_FAILURE:-unset}" \
                 "${REBASE_REFUSED_REASON:-}" \
                 "${REBASE_CONFLICTS:-}"' \
        _ "$SH" "$1" "$2" "$REPO" 2>/dev/null
}

seed; branch sp-refused
printf 'base version\n' > "$REPO/blocked.txt"
git -C "$REPO" add blocked.txt
git -C "$REPO" commit -q -m "base adds blocked.txt"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
printf 'untracked\n' > "$RUN/worktree/sp-refused/blocked.txt"
_ext="$(classify_ext spira/sp-refused origin/main)"
_ext_fail="${_ext%%|*}"; _ext_rest="${_ext#*|}"; _ext_reason="${_ext_rest%%|*}"; _ext_conflicts="${_ext_rest#*|}"
is   "a rebase blocked by an untracked file is named rebase-refused" rebase-refused "$_ext_fail"
want "and git's refusal message is captured in REBASE_REFUSED_REASON" "untracked" "$_ext_reason"
is   "and REBASE_CONFLICTS is empty for a non-conflict failure" "" "$_ext_conflicts"
drop_branch sp-refused

seed; branch sp-kindconflicts shared2.txt "from the branch"
printf '%s\n' "base disagrees" > "$REPO/shared2.txt"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m "base writes shared2.txt"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
_ext="$(classify_ext spira/sp-kindconflicts origin/main)"
_ext_fail="${_ext%%|*}"; _ext_rest="${_ext#*|}"; _ext_conflicts="${_ext_rest#*|}"
is   "a real content conflict still produces REBASE_FAILURE=conflict"    conflict "$(classify spira/sp-kindconflicts origin/main)"
want "and REBASE_CONFLICTS names the colliding file"                      "shared2.txt" "$_ext_conflicts"
drop_branch sp-kindconflicts

# THE IDENTITY FIX. The landing pass runs without an ambient git identity; git refuses any
# rebase that must replay a commit. After the fix, the harness passes -c user.name/user.email
# from SPIRA_GIT_NAME and SPIRA_GIT_EMAIL.
rebase_id_classify() {
    local _home; _home="$(mktemp -d)"
    local _out
    _out="$(env -i \
        HOME="$_home" \
        PATH="$PATH" \
        SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$_home/nodb" \
        SPIRA_REPO="$REPO" SPIRA_REPO_MAP="$SH/repo-map" \
        SPIRA_GIT_NAME=testharness SPIRA_GIT_EMAIL=testharness@test.invalid \
        bash -c '. "$1/lib.sh" >/dev/null 2>&1
                 rebase_branch "$2" "$3" "$4" fixture >/dev/null 2>&1; _rc=$?
                 _cn="$(git -C "$4" log -1 --format=%cn "$2" 2>/dev/null)"
                 _ce="$(git -C "$4" log -1 --format=%ce "$2" 2>/dev/null)"
                 printf "%s|%s|%s|%s" "$_rc" "$_cn" "$_ce" "${REBASE_FAILURE:-}"' \
        _ "$SH" "$1" "$2" "$REPO" 2>/dev/null)"
    rm -rf "$_home"
    printf '%s' "$_out"
}

seed; branch sp-ident
printf 'base step\n' > "$REPO/ident-base.txt"
git -C "$REPO" add ident-base.txt
git -C "$REPO" commit -q -m "base adds ident-base.txt"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
_ri="$(rebase_id_classify spira/sp-ident origin/main)"
_ri_rc="${_ri%%|*}"; _ri_rest="${_ri#*|}"; _ri_cn="${_ri_rest%%|*}"; _ri_rest2="${_ri_rest#*|}"; _ri_ce="${_ri_rest2%%|*}"
is   "rebase succeeds in a clean env when SPIRA_GIT_NAME and SPIRA_GIT_EMAIL are set" "0" "$_ri_rc"
is   "the rebased commit's committer name is taken from SPIRA_GIT_NAME"  "testharness" "$_ri_cn"
is   "the rebased commit's committer email is taken from SPIRA_GIT_EMAIL" "testharness@test.invalid" "$_ri_ce"
drop_branch sp-ident

seed; branch sp-ident-clash shared-ic.txt "branch content"
printf 'base content\n' > "$REPO/shared-ic.txt"
git -C "$REPO" add shared-ic.txt
git -C "$REPO" commit -q -m "base also writes shared-ic.txt"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
_ri_clash="$(rebase_id_classify spira/sp-ident-clash origin/main)"
_ri_clash_fail="${_ri_clash##*|}"
is "a real conflict returns REBASE_FAILURE=conflict even with identity set" "conflict" "$_ri_clash_fail"
drop_branch sp-ident-clash

# THE FENCE. Every route from a rebase failure to a reopen lives in landing.sh and must read
# the classification first.
arms() {
    awk '
        { c = $0; sub(/#.*/, "", c) }
        c ~ /^[ \t]*$/ { next }
        n { if (c ~ /REBASE_FAILURE/) guarded = 1
            if (++k >= 6) { if (!guarded) print "line " n; n = 0 } }
        c ~ /![ \t]*rebase_branch/ { n = NR; k = 0; guarded = 0 }
        END { if (n && !guarded) print "line " n }' "$1"
}
is "every rebase failure arm in landing.sh reads the kind" "" "$(arms "$HERE/landing.sh")"
printf '%s\n' 'if ! rebase_branch "$br" "$base"; then' '    bead_reopen "$id" "conflicts"' 'fi' > "$TMP/plant.sh"
want "and the fence can see an arm that does not" "line 1" "$(arms "$TMP/plant.sh")"

cp "$TMP/gate-full.sh" "$SH/gate.sh"

# --------------------------------------------------------------------------------------
# THE SURVIVORS ARE REBASED WHEN THE BASE MOVES, not when some later pass reaches them.
# --------------------------------------------------------------------------------------
echo
seed; branch sp-held held.txt; branch sp-lands lands.txt
withhold_gate sp-held
out="$(landing)"
want "the branch whose gate was withheld is not landed" "gate NO_VERDICT on spira/sp-held" "$out"
want "and the branch behind it lands"                   "landed spira/sp-lands" "$out"
want "the survivor is rebased onto the new base at once" \
     "rebased spira/sp-held onto origin/main after landing spira/sp-lands" "$out"
is   "and its tip moved after the loop had walked past it" \
     moved "$([ "$(tip_at_gate sp-held)" != "$(tip_of sp-held)" ] && echo moved || echo "same as at gate time")"
is   "so it now carries the commit that landed"         yes "$(on_base sp-held)"
nowant "and a clean rebase never reopens the bead"      "reopened sp-held" "$out"
is   "which stays closed"                               closed "$(status_of sp-held)"
want "and the pass counts what the sweep did"           "1 survivor(s) rebased after a landing, 0 conflicted" "$out"
drop_branch sp-held; drop_branch sp-lands

# THE SWEEP MUST STILL REOPEN A REAL DISAGREEMENT.
seed; branch sp-cheld shared.txt "from the held branch"
branch sp-clands shared.txt "from the branch that lands"
withhold_gate sp-cheld
out="$(landing)"
want "the branch behind it still lands"            "landed spira/sp-clands" "$out"
want "and the survivor that truly conflicts is reopened" "reopened sp-cheld" "$out"
is   "its bead goes back to open"                  open "$(status_of sp-cheld)"
want "the note names the file that collided"       "shared.txt" "$(notes_of sp-cheld)"
want "and names the landing that moved the base"   "after spira/sp-clands landed" "$(notes_of sp-cheld)"
want "and the pass counts the conflict separately" "0 survivor(s) rebased after a landing, 1 conflicted" "$out"
drop_branch sp-cheld; drop_branch sp-clands

# A BRANCH AN AEON TOOK WHILE THE PASS RAN IS NEVER REWRITTEN.
seed; branch sp-taken taken.txt; branch sp-tlands tlands.txt
withhold_gate sp-taken
printf '#!/usr/bin/env bash\nwhile :; do sleep 1; done\n' > "$TMP/aeon.sh"; chmod +x "$TMP/aeon.sh"
"$TMP/aeon.sh" >/dev/null 2>&1 & aeon_pid=$!
claim_during_gate sp-taken "$aeon_pid"
out="$(landing)"
claimed="$(cat "$RUN/aeon-builder-sp-taken.pid" 2>/dev/null)"
kill "$aeon_pid" 2>/dev/null; wait "$aeon_pid" 2>/dev/null
rm -f "$RUN/aeon-builder-sp-taken.pid"
is     "the fixture did claim it mid-pass"      "$aeon_pid" "$claimed"
want   "the pass says an aeon took it"          "an aeon took spira/sp-taken while this pass ran" "$out"
is     "and its tip is exactly as the loop left it" \
       "$(tip_at_gate sp-taken)" "$(tip_of sp-taken)"
nowant "and nothing rebased it"                 "rebased spira/sp-taken" "$out"
drop_branch sp-taken; drop_branch sp-tlands

# --------------------------------------------------------------------------------------
# THE LANDING LOOP DEFERS WHEN A LIVE AEON HOLDS THE BEAD
# --------------------------------------------------------------------------------------
seed; branch sp-loop-held loop-held.txt
"$TMP/aeon.sh" >/dev/null 2>&1 & loop_held_pid=$!
printf '%s\n' "$loop_held_pid" > "$RUN/aeon-builder-sp-loop-held.pid"
out="$(landing)"
kill "$loop_held_pid" 2>/dev/null; wait "$loop_held_pid" 2>/dev/null
rm -f "$RUN/aeon-builder-sp-loop-held.pid"
want   "the loop defers when a live aeon holds the bead" \
       "a live aeon still holds spira/sp-loop-held — deferring the land" "$out"
nowant "and the branch was not landed"                   "landed spira/sp-loop-held" "$out"
is     "and the bead stays closed"                       closed "$(status_of sp-loop-held)"
drop_branch sp-loop-held

# --------------------------------------------------------------------------------------
# THE CHECKOUT REFRESH — unconditional, not only when a branch merged this pass.
# --------------------------------------------------------------------------------------
echo

advance_base() {
    local prev tree tip
    prev="$(git -C "$REMOTE" rev-parse HEAD)"
    tree="$(git -C "$REMOTE" rev-parse HEAD^{tree})"
    tip="$(git -C "$REMOTE" commit-tree -p "$prev" -m "base moved" "$tree")"
    git -C "$REMOTE" update-ref refs/heads/main "$tip"
}
checkout_current() { git -C "$REPO" rev-parse HEAD 2>/dev/null; }
reset_repo() {
    git -C "$REPO" checkout -q main 2>/dev/null || true
    git -C "$REPO" reset -q --hard origin/main 2>/dev/null || true
    git -C "$REPO" clean -qfd 2>/dev/null || true
}

seed; reset_repo; advance_base
before="$(checkout_current)"
mkdir -p "$REPO/untracked-notes"
printf 'notes\n' > "$REPO/untracked-notes/scratch.txt"
out="$(landing)"
after="$(checkout_current)"
rm -rf "$REPO/untracked-notes"
[ "$before" != "$after" ] \
    && ok "an untracked file does not block the refresh" \
    || bad "an untracked file does not block the refresh" "checkout did not move"
want "and the refresh is reported" "skew: refreshed to" "$out"

seed; reset_repo
printf 'clean\n' > "$REPO/tracked.txt"
git -C "$REPO" add tracked.txt
git -C "$REPO" commit -q -m "add tracked file"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
advance_base
printf 'dirty\n' >> "$REPO/tracked.txt"
before="$(checkout_current)"
out="$(landing)"
after="$(checkout_current)"
git -C "$REPO" stash drop >/dev/null 2>&1 || true
[ "$before" != "$after" ] \
    && ok "a modified tracked file is stashed and the advance proceeds" \
    || bad "a modified tracked file is stashed and the advance proceeds" "checkout did not move"
want "and the refresh reports the stash" "stashed dirty tracked files" "$out"
want "and reports the refresh" "skew: refreshed to" "$out"

seed; reset_repo; advance_base
before="$(checkout_current)"
out="$(landing)"
after="$(checkout_current)"
[ "$before" != "$after" ] \
    && ok "a base that moved with no branch landing is picked up" \
    || bad "a base that moved with no branch landing is picked up" "checkout did not move"
want "and the refresh is reported" "skew: refreshed to" "$out"

seed; reset_repo; advance_base
git -C "$REPO" checkout -q -b detour 2>/dev/null
before="$(checkout_current)"
out="$(landing)"
after="$(checkout_current)"
git -C "$REPO" checkout -q main 2>/dev/null
git -C "$REPO" branch -q -D detour 2>/dev/null
[ "$before" = "$after" ] \
    && ok "a checkout not on the base branch is not clobbered" \
    || bad "a checkout not on the base branch is not clobbered" "checkout moved anyway"
want "and the decline names the branch" "not main" "$out"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
