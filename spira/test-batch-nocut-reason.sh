#!/usr/bin/env bash
#
# test-batch-nocut-reason.sh — every no-cut exit writes a reason-bearing line.
#
# THE PROPERTY UNDER TEST. batch.sh had ~18 bare `return 0` exits and wrote
# nothing to any log on most of them. An invocation that acquired the lock,
# ran, and chose not to cut a batch was indistinguishable from one that was
# never attempted. The concierge session that observed this had to go through
# /proc/locks and ps to rule out a wedged process — the logs said nothing.
#
# THREE CASES:
#
#   zero certified    no branches in queue → "0 certified"
#   ci busy           2 branches, CI reports 3 active runs, neither wait trigger
#                     fires → "no cut — N certified, need MAX; oldest Xm, cuts at Ym; CI busy"
#   ci idle           2 branches, CI reports 0 active → batch IS cut, QUEUE BATCH line
#
# POSITIVE CONTROL. The no-trigger case (ci busy) produces no QUEUE NOCUT line
# on the unfixed tree. The `want` assertion below fails against an empty log,
# which is how it was seen red without the fix.
#
# covers: spira/batch.sh spira/conf.sh spira/landing.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-batch-nocut-reason
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up batchnocut || { echo "test-batch-nocut-reason: could not build fixture database"; exit 1; }

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
cp "$HERE"/*.sh "$SH/"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

cat > "$SH/mail.sh" <<'MAIL'
#!/usr/bin/env bash
exit 0
MAIL
chmod +x "$SH/mail.sh"

FORGE_LOG="$TMP/forge-log"
cat > "$SH/forge-fixture.sh" <<'FORGE'
#!/usr/bin/env bash
cmd="${1:-}"; shift; repo="${1:-}"; shift
case "$cmd" in
    runs-active) printf '%s\n' "${RUNS_ACTIVE:-?}" ;;
    pr-create)
        n=$(( $(wc -l < "$FORGE_LOG" 2>/dev/null || echo 0) + 1 ))
        printf '%s\n' "$n" >> "$FORGE_LOG"
        printf '%s\n' "$n"
        ;;
    *) printf 'forge-fixture: unknown: %s\n' "$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"
: > "$FORGE_LOG"

# MAX and WAIT are enormous so only the idle-cut trigger can fire.
batch() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_MAX=99 \
    SPIRA_QUEUE_BATCH_WAIT=999999 \
    RUNS_ACTIVE="${RUNS_ACTIVE:-?}" \
    FORGE_LOG="$FORGE_LOG" \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
        bash "$SH/batch.sh" "$@" 2>&1
}

landing_log() { cat "$RUN/landing.log" 2>/dev/null || true; }
clear_run()   { rm -f "$RUN/landing.log"; : > "$FORGE_LOG"; }

# Seed a closed bead, create a CERTIFIED landstate and a branch for it.
make_branch() {
    local id="$1"
    testdb_reset
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-23T00:00:00Z"}\n' \
        "$id" "$id" | testdb_seed
    local wt="$RUN/worktree/$id"
    git -C "$REPO" worktree add -q -b "spira/$id" "$wt" main 2>/dev/null || true
    printf '%s\n' "$id" > "$wt/$id.txt"
    git -C "$wt" add -A && git -C "$wt" commit -q -m "$id: work"
    local tip; tip="$(git -C "$REPO" rev-parse "spira/$id")"
    printf 'CERTIFIED %s %s\n' "$tip" "$(date +%s)" > "$LANDSTATE/$id"
}

make_two_branches() {
    testdb_reset
    for _id in sp-nc-1 sp-nc-2; do
        printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-23T00:00:00Z"}\n' \
            "$_id" "$_id" | testdb_seed
        local wt="$RUN/worktree/$_id"
        git -C "$REPO" worktree add -q -b "spira/$_id" "$wt" main 2>/dev/null || true
        printf '%s\n' "$_id" > "$wt/$_id.txt"
        git -C "$wt" add -A && git -C "$wt" commit -q -m "$_id: work"
        local tip; tip="$(git -C "$REPO" rev-parse "spira/$_id")"
        printf 'CERTIFIED %s %s\n' "$tip" "$(date +%s)" > "$LANDSTATE/$_id"
    done
}

clean_branches() {
    find "$LANDSTATE" -maxdepth 1 -type f -delete 2>/dev/null || true
    local wt="$RUN/worktree/.batch-$(basename "$REPO")"
    [ -d "$wt" ] && git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    rm -rf "$RUN/worktree" && mkdir -p "$RUN/worktree"
    git -C "$REPO" worktree prune 2>/dev/null || true
    git -C "$REPO" for-each-ref --format='%(refname:short)' 'refs/heads/spira/*' 2>/dev/null \
        | while read -r br; do git -C "$REPO" branch -D "$br" 2>/dev/null || true; done
}

echo "test-batch-nocut-reason.sh"

# =============================================================================
# (a) ZERO CERTIFIED: no branches → QUEUE NOCUT certified=0 in landing.log.
#
# POSITIVE CONTROL: the `want` assertion fails on unfixed batch.sh because the
# log stays empty — the return at `[ -z "${certs:-}" ]` wrote nothing.
# =============================================================================
testdb_reset
clear_run
out_a="$(batch "$REPONAME")"
want "a. zero certified: stdout says no cut"          "no cut"          "$out_a"
want "a. zero certified: stdout says 0 certified"     "0 certified"     "$out_a"
want "a. zero certified: QUEUE NOCUT in landing.log"  "QUEUE NOCUT"     "$(landing_log)"
want "a. zero certified: certified=0 in landing.log"  "certified=0"     "$(landing_log)"
nowant "a. zero certified: no QUEUE BATCH"            "QUEUE BATCH"     "$(landing_log)"

# =============================================================================
# (b) TWO CERTIFIED, CI BUSY: 2 branches waiting, neither wait trigger fires,
#     CI says 3 runs active → no batch, QUEUE NOCUT with reason in landing.log.
#
# This is the observed case from 2026-09-23 ~23:51Z: two certified beads, no
# open batch, batch.sh ran and exited silently. This is the POSITIVE CONTROL
# for that incident: landing.log was empty; the `want` below fails without the fix.
# =============================================================================
make_two_branches
clear_run
out_b="$(RUNS_ACTIVE=3 batch "$REPONAME")"
want  "b. ci busy: stdout says no cut"                 "no cut"          "$out_b"
want  "b. ci busy: stdout names certified count"       "2 certified"     "$out_b"
want  "b. ci busy: stdout names need"                  "need 99"         "$out_b"
want  "b. ci busy: stdout names CI status"             "CI busy"         "$out_b"
want  "b. ci busy: QUEUE NOCUT in landing.log"         "QUEUE NOCUT"     "$(landing_log)"
want  "b. ci busy: certified=2 in landing.log"         "certified=2"     "$(landing_log)"
nowant "b. ci busy: no QUEUE BATCH"                    "QUEUE BATCH"     "$(landing_log)"
is    "b. ci busy: no PR opened"                       "0" \
      "$(wc -l < "$FORGE_LOG" | tr -d ' ')"
is    "b. ci busy: landstate still CERTIFIED sp-nc-1"  "CERTIFIED" \
      "$(awk '{print $1}' "$LANDSTATE/sp-nc-1" 2>/dev/null)"

# =============================================================================
# (c) TWO CERTIFIED, CI IDLE: CI says 0 runs → batch IS cut immediately.
#     A reason-bearing QUEUE BATCH line (not NOCUT) lands in the log.
# =============================================================================
clear_run
out_c="$(RUNS_ACTIVE=0 batch "$REPONAME")"
want  "c. ci idle: PR opened"                          "PR 1 opened"     "$out_c"
want  "c. ci idle: QUEUE BATCH in landing.log"         "QUEUE BATCH"     "$(landing_log)"
nowant "c. ci idle: no QUEUE NOCUT"                    "QUEUE NOCUT"     "$(landing_log)"
is    "c. ci idle: landstate now BATCHED sp-nc-1"      "BATCHED" \
      "$(awk '{print $1}' "$LANDSTATE/sp-nc-1" 2>/dev/null)"

clean_branches

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
