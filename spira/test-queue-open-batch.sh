#!/usr/bin/env bash
# test-queue-open-batch.sh — queue.sh open-batch: assemble, push, open the PR and write
#   the open record in one hand-invoked command (sp-214zs).
#
# Exercises real git merges and conflicts against a fixture repo/remote, a real bd store
# (testdb.sh) and forge/gate fixtures that log every call. open-batch's assembly primitives
# (base_conflict, format_batch, pf_gate) are the same bash bodies batch.sh's own
# _base_conflict/format_batch/_pf_gate once were — inlined straight into queue/src/seam.rs
# (sp-uwhx0, batch.sh deleted) rather than sourced from it; this suite is unchanged by that,
# since it only ever called `queue open-batch`, never batch.sh itself.
#
# tier: T1
# covers: queue/src/* spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
# The queue binary (queue/DESIGN.md §7.4), invoked by name: the tree under test's build is
# on the suite's PATH (sp-gypjk).

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-queue-open-batch
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up openbatch || { echo "test-queue-open-batch: could not build fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"
REMOTE="$TMP/remote.git"
RUN="$TMP/run"
SH="$TMP/spira"
REPONAME=fixture-repo
LANDSTATE="$RUN/landstate"
QUEUEDIR="$RUN/queue"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/$REPONAME"

cp "$HERE"/*.sh "$HERE"/*.py "$SH/" 2>/dev/null
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SH/"

GATE_COUNT="$TMP/gate-count"
GATE_RC_FILE="$TMP/gate-rc"
: > "$GATE_COUNT"; printf '0\n' > "$GATE_RC_FILE"
cat > "$SH/gate.sh" <<GSTUB
#!/usr/bin/env bash
printf '%s\n' "\$1" >> "$GATE_COUNT"
exit "\$(cat "$GATE_RC_FILE" 2>/dev/null || echo 0)"
GSTUB
chmod +x "$SH/gate.sh"

FORGE_LOG="$TMP/forge-log"
BODY_LOG="$TMP/body-log"
cat > "$SH/forge-fixture.sh" << FORGE
#!/usr/bin/env bash
cmd="\${1:-}"; shift; repo="\${1:-}"; shift
case "\$cmd" in
    pr-create)
        n=\$(( \$(wc -l < "$FORGE_LOG" 2>/dev/null || echo 0) + 1 ))
        body="\$(cat)"
        printf '%s\n' "\$n" >> "$FORGE_LOG"
        printf '%s\n' "\$body" >> "$BODY_LOG"
        printf '%s\n' "\$n"
        ;;
    pr-number) grep "^\${1:-}	" "$FORGE_LOG" 2>/dev/null | tail -1 | cut -f2 ;;
    pr-comment|pr-list-queue) : ;;
    *) printf 'forge-fixture: unknown: %s\n' "\$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"

printf '#!/usr/bin/env bash\ntrue\n' > "$SH/mail"; chmod +x "$SH/mail"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
notq | $REPO | push | origin/main | | |
RMAP

B() { "${TESTDB_BD:-bd}" -C "$SPIRA_DB" "$@"; }

openbatch() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
    SPIRA_PREFLIGHT_WALL_SECS=60 \
        PATH="$SH:$PATH" SPIRA_HOME="$SH" queue open-batch "$@" 2>&1
}

seed() {
    testdb_reset
    testdb_seed <<'JSONL'
{"id":"sp-epic","title":"epic","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
}

plant_bead() {   # plant_bead <id> [<title>]
    printf \
        '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-epic","type":"parent-child"}]}\n' \
        "$1" "${2:-$1}" "$1" | testdb_seed
}

# certify <id> <path-in-repo> <content> — a CERTIFIED branch spira/<id> off main,
# adding/overwriting <path> with <content>, and its own bead + landstate record.
certify() {
    local id="$1" path="$2" content="$3"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" origin/main
    printf '%s\n' "$content" > "$RUN/worktree/$id/$path"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "$id: work"
    local tip; tip="$(git -C "$REPO" rev-parse "spira/$id")"
    printf 'CERTIFIED %s %s\n' "$tip" "$(date +%s)" > "$LANDSTATE/$id"
    plant_bead "$id" "bead for $id"
}

landstate_of() { { read -r st _ < "$LANDSTATE/$1"; } 2>/dev/null; printf '%s' "${st:-}"; }
branch_exists() { git -C "$REPO" show-ref --verify -q "refs/heads/spira/$1" 2>/dev/null; }
open_batch_file() { printf '%s/%s/open' "$QUEUEDIR" "$REPONAME"; }
remote_queue_branches() { git --git-dir="$REMOTE" for-each-ref --format='%(refname:short)' 'refs/heads/spira/queue/*' 2>/dev/null; }

clean_case() {
    rm -f "$(open_batch_file)"
    : > "$FORGE_LOG"; : > "$BODY_LOG"; : > "$GATE_COUNT"; printf '0\n' > "$GATE_RC_FILE"
    find "$LANDSTATE" -maxdepth 1 -type f -delete 2>/dev/null
    local wt
    for wt in "$RUN/worktree"/sp-* "$RUN/worktree"/.open-batch-*; do
        [ -e "$wt" ] && git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    done
    rm -rf "$RUN/worktree" && mkdir -p "$RUN/worktree"
    git -C "$REPO" worktree prune 2>/dev/null || true
    git -C "$REPO" for-each-ref --format='%(refname:short)' 'refs/heads/spira/*' 2>/dev/null \
        | while read -r br; do git -C "$REPO" branch -D "$br" 2>/dev/null || true; done
    git --git-dir="$REMOTE" for-each-ref --format='%(refname:short)' 'refs/heads/spira/queue/*' 2>/dev/null \
        | while read -r br; do git --git-dir="$REMOTE" branch -D "$br" 2>/dev/null || true; done
}

echo "test-queue-open-batch.sh"

# =============================================================================
# 1. Positive control: two independent certified branches, no conflicts, gate
#    green — assembled, pushed, PR opened, record written, members BATCHED.
# =============================================================================
echo
echo "1. assembles both members, opens the PR, writes the record, marks BATCHED:"
seed
certify sp-a fa.txt "a-content"
certify sp-b fb.txt "b-content"

out1="$(openbatch fixture-repo)"
want "1. reports PR opened, 2 branches" "PR 1 opened — 2 branches" "$out1"
is   "1. one PR created"                "1"           "$(wc -l < "$FORGE_LOG")"
body1="$(cat "$BODY_LOG")"
want "1. body lists sp-a"  "sp-a" "$body1"
want "1. body lists sp-b"  "sp-b" "$body1"
rec1="$(cat "$(open_batch_file)" 2>/dev/null)"
want "1. record names pr=1"       "pr=1"     "$rec1"
want "1. record lists sp-a:"      "sp-a:"    "$rec1"
want "1. record lists sp-b:"      "sp-b:"    "$rec1"
branch1="$(printf '%s\n' "$rec1" | sed -n 's/^branch=//p')"
want "1. commit message names sp-a with its own title" "spira: land sp-a — bead for sp-a" \
    "$(git -C "$REPO" log --format=%s "$branch1" -n 5 2>/dev/null)"
is   "1. sp-a is BATCHED"  "BATCHED" "$(landstate_of sp-a)"
is   "1. sp-b is BATCHED"  "BATCHED" "$(landstate_of sp-b)"
want "1. landing.log records the batch" "verdict=green source=open-batch" \
    "$(cat "$RUN/landing.log" 2>/dev/null)"

echo
echo "2. refuses a second batch while one is open:"
out2="$(openbatch fixture-repo)"; rc2=$?
want "2. names already-open"  "already open" "$out2"
is   "2. still one PR total"  "1"            "$(wc -l < "$FORGE_LOG")"
[ "$rc2" -ne 0 ] && ok "2. non-zero exit" || bad "2. non-zero exit" "rc=$rc2"
clean_case

# =============================================================================
# 3. --dry-run: reports members, merge head and the would-be record; no branch,
#    no push, no PR, no landstate change.
# =============================================================================
echo
echo "3. --dry-run makes no changes:"
seed
certify sp-c fc.txt "c-content"
certify sp-d fd.txt "d-content"

out3="$(openbatch fixture-repo --dry-run)"
want "3. announces dry-run"        "dry-run"       "$out3"
want "3. lists sp-c"               "sp-c:"         "$out3"
want "3. lists sp-d"               "sp-d:"         "$out3"
want "3. shows merge head"         "merge head:"   "$out3"
want "3. shows would-be record"    "would write open record" "$out3"
is   "3. no PR created"            "0"             "$(wc -l < "$FORGE_LOG")"
[ -f "$(open_batch_file)" ] && bad "3. no open record written" "file exists" \
    || ok "3. no open record written"
is   "3. sp-c still CERTIFIED"     "CERTIFIED"     "$(landstate_of sp-c)"
is   "3. sp-d still CERTIFIED"     "CERTIFIED"     "$(landstate_of sp-d)"
qb3="$(remote_queue_branches)"
[ -z "$qb3" ] && ok "3. no queue branch pushed" || bad "3. no queue branch pushed" "$qb3"
clean_case

# =============================================================================
# 4. --members overrides selection, and a member conflicting with the
#    accumulating batch is skipped with a reason, not reopened.
# =============================================================================
echo
echo "4. --members overrides selection; a batch conflict is skipped, not reopened:"
seed
certify sp-e shared.txt "e-content"
certify sp-f shared.txt "f-content"
certify sp-g fg.txt     "g-content"

out4="$(openbatch fixture-repo --members "sp-e sp-f sp-g")"
want  "4. reports PR opened, 2 branches" "PR 1 opened — 2 branches" "$out4"
want  "4. names the skip and its reason" "sp-f: conflicts with batch" "$out4"
rec4="$(cat "$(open_batch_file)" 2>/dev/null)"
want  "4. record includes sp-e"   "sp-e:" "$rec4"
want  "4. record includes sp-g"   "sp-g:" "$rec4"
nowant "4. record excludes sp-f"  "sp-f:" "$rec4"
is    "4. sp-f is still CERTIFIED (not reopened)" "CERTIFIED" "$(landstate_of sp-f)"
if branch_exists sp-f; then ok "4. sp-f branch untouched"
else bad "4. sp-f branch untouched" "branch missing"; fi
clean_case

# =============================================================================
# 5. A member that conflicts with the base itself is skipped with its own
#    reason, distinct from a batch-only conflict.
# =============================================================================
echo
echo "5. base conflict is skipped and named distinctly from a batch conflict:"
seed
certify sp-h base.txt "h-content"
# advance main under sp-h with a conflicting change, then fetch so base moved.
printf 'main-advance\n' > "$REPO/base.txt"
git -C "$REPO" add base.txt && git -C "$REPO" commit -q -m "main: advance base.txt"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

out5="$(openbatch fixture-repo)"
want "5. names base conflict" "sp-h: conflicts with base" "$out5"
want "5. no cut — nothing admissible" "no cut" "$out5"
is   "5. sp-h is still CERTIFIED" "CERTIFIED" "$(landstate_of sp-h)"
[ -f "$(open_batch_file)" ] && bad "5. no open record written" "file exists" \
    || ok "5. no open record written"
clean_case

# =============================================================================
# 6. --skip-pregate opens the PR without ever calling the gate, and says so
#    in the PR body.
# =============================================================================
echo
echo "6. --skip-pregate skips the gate and says so in the PR body:"
seed
certify sp-i fi.txt "i-content"
printf '1\n' > "$GATE_RC_FILE"    # gate would fail if it ran

out6="$(openbatch fixture-repo --skip-pregate)"
want "6. names skip-pregate"        "pre-flight gate skipped" "$out6"
want "6. opens the PR anyway"       "PR 1 opened"              "$out6"
is   "6. gate never invoked"        "0" "$(wc -l < "$GATE_COUNT")"
want "6. PR body notes the skip"    "Opened with --skip-pregate" "$(cat "$BODY_LOG")"
clean_case

# =============================================================================
# 7. Without --skip-pregate, a red pre-flight gate blocks the open: no PR, no
#    record, the batch branch is not left behind, and members stay CERTIFIED.
# =============================================================================
echo
echo "7. a red pre-flight gate blocks the open (no PR, no record):"
seed
certify sp-j fj.txt "j-content"
printf '1\n' > "$GATE_RC_FILE"

out7="$(openbatch fixture-repo)"; rc7=$?
want "7. names the gate failure" "pre-flight gate failed" "$out7"
is   "7. gate was called once"   "1" "$(wc -l < "$GATE_COUNT")"
is   "7. no PR created"          "0" "$(wc -l < "$FORGE_LOG")"
[ "$rc7" -ne 0 ] && ok "7. non-zero exit" || bad "7. non-zero exit" "rc=$rc7"
is   "7. sp-j is still CERTIFIED" "CERTIFIED" "$(landstate_of sp-j)"
[ -f "$(open_batch_file)" ] && bad "7. no open record written" "file exists" \
    || ok "7. no open record written"
qb7="$(remote_queue_branches)"
[ -z "$qb7" ] && ok "7. no queue branch left pushed" || bad "7. no queue branch left pushed" "$qb7"
clean_case

# =============================================================================
# 8. Nothing admissible (empty --members) never opens a PR with an empty
#    member list — the real defect this command replaces by hand.
# =============================================================================
echo
echo "8. nothing admissible — refuses rather than open an empty-list PR:"
seed
out8="$(openbatch fixture-repo --members "sp-nosuchbead")"; rc8=$?
want "8. names the bead as not certified" "sp-nosuchbead: not CERTIFIED" "$out8"
want "8. reports no cut"                  "no cut"                       "$out8"
is   "8. no PR created"                   "0" "$(wc -l < "$FORGE_LOG")"
[ "$rc8" -ne 0 ] && ok "8. non-zero exit" || bad "8. non-zero exit" "rc=$rc8"
clean_case

# =============================================================================
# 9. Cheap refusals: unknown repo, and a repo not in queue mode.
# =============================================================================
echo
echo "9. refuses an unknown repo and a non-queue-mode repo:"
out9a="$(openbatch nosuchrepo)"
want "9. names the repo" "no such repo" "$out9a"
out9b="$(openbatch notq)"
want "9. names queue mode" "not in queue mode" "$out9b"

echo
tl_summary
