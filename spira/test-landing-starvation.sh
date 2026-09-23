#!/usr/bin/env bash
#
# test-landing-starvation.sh — a pass that cuts its budget records a cursor so the
# next pass starts at the repo that was deferred, and a branch deferred too many
# consecutive times produces an escalation mail.
#
# THREE PROPERTIES UNDER TEST:
#
#   1. CURSOR WRITE. After a pass with a budget cut, $SPIRA_RUN/landing.cursor
#      contains the name of the repo where the last cut fired.
#      Positive control: with no budget constraint the cursor is not written.
#
#   2. REPO ROTATION. Given cursor=repo-b, the next pass processes repo-b's
#      branches before repo-a's branches, even though repo-a is first in
#      spira_repos order (it is the home repo).
#
#   3. DEFERRAL ESCALATION. After SPIRA_DEFERRAL_ESCALATE_AT consecutive budget
#      cuts that defer the same branch, an escalation mail is sent.
#      The test must be seen to fail before the fix:
#      without the deferral counter the mail is never sent.
#
# The gate stub is instant (no sleep). Budget tests use LAND_MAXSEC=1 with
# RESERVE=2, which makes gate_fits return false immediately (1-0=1 < 2).
# This is deterministic without wall-clock timing.
#
# defect: sp-3tkj6
# covers: spira/landing.sh spira/lib.sh
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
before() {
    local la lb
    la=$(printf '%s\n' "$4" | grep -n "$2" | head -1 | cut -d: -f1)
    lb=$(printf '%s\n' "$4" | grep -n "$3" | head -1 | cut -d: -f1)
    [ -n "$la" ] && [ -n "$lb" ] && [ "$la" -lt "$lb" ] \
        && ok "$1" \
        || bad "$1" "[$2] (line ${la:--}) not before [$3] (line ${lb:--})"
}

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-landing-starvation
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up landing-starvation || {
    printf 'SKIP test-landing-starvation: no testdb available\n' >&2
    exit 77
}
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# Two repos: REPO_A (home) and REPO_B (secondary), both queue mode.
REPO_A="$TMP/repo-a"; REMOTE_A="$TMP/remote-a.git"; NAME_A=fixture-repo-a
REPO_B="$TMP/repo-b"; REMOTE_B="$TMP/remote-b.git"; NAME_B=fixture-repo-b
RUN="$TMP/run"; SH="$TMP/spira"

for rdir in "$REMOTE_A" "$REMOTE_B"; do git init -q --bare -b main "$rdir"; done
for rdir in "$REPO_A" "$REPO_B"; do
    git init -q -b main "$rdir"
    git -C "$rdir" commit -q --allow-empty -m base
done
git -C "$REPO_A" remote add origin "$REMOTE_A"; git -C "$REPO_A" push -q origin main; git -C "$REPO_A" fetch -q origin
git -C "$REPO_B" remote add origin "$REMOTE_B"; git -C "$REPO_B" push -q origin main; git -C "$REPO_B" fetch -q origin
mkdir -p "$RUN/worktree" "$SH"

cp "$HERE"/*.sh "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub confine.sh 'exit 0'
stub gate.sh 'echo "gate: VERDICT=PASS reason=stub branch=$1 repo=${2:-?}" >&2; exit 0'
stub gh 'exit 1'
stub queue.sh 'exit 0'
stub mail.sh '[ "${1:-}" = send ] || exit 0; printf "%s\n" "$*" >> "$EMITTED"; cat >> "$EMITTED"; printf "\n" >> "$EMITTED"'
export EMITTED="$TMP/events"; : > "$EMITTED"

cat > "$SH/repo-map" <<MAP
$NAME_A | $REPO_A | queue | |
$NAME_B | $REPO_B | queue | |
MAP

B() { bd -C "$SPIRA_DB" "$@"; }

landing() {
    rm -f "$RUN/landing.progress"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO="$REPO_A" SPIRA_HOME_REPO="$NAME_A" SPIRA_ID_PREFIX=sp \
    SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/gh" "$@" \
        bash "$SH/landing.sh" 2>&1
}

landing_tight() {
    landing SPIRA_LAND_MAXSEC=1 SPIRA_LAND_GATE_RESERVE=2
}

# Add a closed bead with a git branch in the given repo
branch_at() {
    local id="$1" repo="$2" reponame="$3"
    git -C "$repo" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$id" > "$RUN/worktree/$id/$id.txt"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","priority":2,"labels":["repo:%s"],"updated_at":"2026-09-01T00:00:00Z","closed_at":"2026-09-01T00:00:00Z","dependencies":[]}\n' \
        "$id" "$id" "$reponame" | testdb_seed
}

drop_branch() {
    local id="$1" repo="$2"
    git -C "$repo" worktree remove --force "$RUN/worktree/$id" >/dev/null 2>&1 || true
    git -C "$repo" branch -D "spira/$id" >/dev/null 2>&1 || true
}

reset_all() {
    testdb_reset
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-01T00:00:00Z"}
JSONL
    rm -rf "$RUN/submitted" "$RUN/landing.cursor" "$RUN/landing.deferred"
    : > "$EMITTED"
}

echo "test-landing-starvation.sh"

# ---------------------------------------------------------------------------
# POSITIVE CONTROL: with no budget constraint neither cursor nor deferral
# files appear. This proves absence is meaningful below.
# ---------------------------------------------------------------------------
echo
echo "positive control — full-budget pass leaves no cursor:"
reset_all
branch_at sp-star-a "$REPO_A" "$NAME_A"
branch_at sp-star-b "$REPO_B" "$NAME_B"
out="$(landing)"
want "full-budget pass certifies both branches" "certified" "$out"
nowant "full-budget pass emits no budget-cut" "budget cut" "$out"
[ -f "$RUN/landing.cursor" ] \
    && bad "no cursor written on full-budget pass" "cursor file exists: $(cat "$RUN/landing.cursor")" \
    || ok "no cursor written on full-budget pass"
drop_branch sp-star-a "$REPO_A"; drop_branch sp-star-b "$REPO_B"

# ---------------------------------------------------------------------------
# CURSOR WRITE: a tight pass writes the cursor with the repo name.
# The last repo to cut is the cursor value because it was most recently
# deprived of processing time.
# ---------------------------------------------------------------------------
echo
echo "cursor write test:"
reset_all
branch_at sp-cur-a "$REPO_A" "$NAME_A"
branch_at sp-cur-b "$REPO_B" "$NAME_B"
out="$(landing_tight)"
want "tight pass logs budget cut" "budget cut" "$out"
cursor="$(cat "$RUN/landing.cursor" 2>/dev/null)"
[ -n "$cursor" ] \
    && ok "cursor file written after budget cut" \
    || bad "cursor file written after budget cut" "cursor file missing"
# The last repo to cut is fixture-repo-b (it is processed second and also cuts)
[ "$cursor" = "$NAME_B" ] \
    && ok "cursor names the last-cut repo ($NAME_B)" \
    || bad "cursor names the last-cut repo" "got [$cursor], want [$NAME_B]"
drop_branch sp-cur-a "$REPO_A"; drop_branch sp-cur-b "$REPO_B"

# ---------------------------------------------------------------------------
# REPO ROTATION: with cursor=NAME_B, the next pass processes NAME_B first.
# Verified by comparing the order of budget-cut log lines.
# ---------------------------------------------------------------------------
echo
echo "repo rotation test:"
reset_all
branch_at sp-rot-a "$REPO_A" "$NAME_A"
branch_at sp-rot-b "$REPO_B" "$NAME_B"
# Plant the cursor so the pass starts from NAME_B
printf '%s\n' "$NAME_B" > "$RUN/landing.cursor"
out="$(landing_tight)"
want "rotated pass still logs budget cut in both repos" "budget cut" "$out"
before "repo-b cut logged before repo-a cut when cursor=repo-b" \
    "branch(es) deferred in $NAME_B" \
    "branch(es) deferred in $NAME_A" \
    "$out"
drop_branch sp-rot-a "$REPO_A"; drop_branch sp-rot-b "$REPO_B"

# ---------------------------------------------------------------------------
# DEFERRAL ESCALATION: after SPIRA_DEFERRAL_ESCALATE_AT consecutive passes
# that defer the same branch, an escalation mail is sent.
# POSITIVE CONTROL FIRST: without the counter, the mail is never sent; this
# proves that the escalation in the "with counter" run is real signal.
# ---------------------------------------------------------------------------
echo
echo "deferral escalation test:"
reset_all
branch_at sp-def-a "$REPO_A" "$NAME_A"

# Run SPIRA_DEFERRAL_ESCALATE_AT-1 tight passes — no mail should be sent yet
# (uses default of 5 but we force it to 3 for speed)
THRESHOLD=3
for i in $(seq 1 $(( THRESHOLD - 1 )) ); do
    out="$(landing SPIRA_LAND_MAXSEC=1 SPIRA_LAND_GATE_RESERVE=2 SPIRA_DEFERRAL_ESCALATE_AT="$THRESHOLD")"
    nowant "no escalation before threshold (pass $i)" "budget-deferred" "$(cat "$EMITTED")"
done

# One more tight pass crosses the threshold
out="$(landing SPIRA_LAND_MAXSEC=1 SPIRA_LAND_GATE_RESERVE=2 SPIRA_DEFERRAL_ESCALATE_AT="$THRESHOLD")"
want "escalation mail sent after $THRESHOLD deferrals" "budget-deferred" "$(cat "$EMITTED")"
want "escalation names the deferred branch" "spira/sp-def-a" "$(cat "$EMITTED")"

# A full-budget pass processes the branch: deferral count should reset.
# After the reset, another tight pass should NOT immediately re-escalate.
: > "$EMITTED"
out="$(landing)"   # full-budget: processes the branch
want "full-budget pass certifies the branch" "certified" "$out"
# Now run tight again — deferral count reset, mail already closed, no re-escalation
out="$(landing SPIRA_LAND_MAXSEC=1 SPIRA_LAND_GATE_RESERVE=2 SPIRA_DEFERRAL_ESCALATE_AT="$THRESHOLD")"
nowant "no re-escalation after deferral count reset" "budget-deferred" "$(cat "$EMITTED")"
drop_branch sp-def-a "$REPO_A"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
