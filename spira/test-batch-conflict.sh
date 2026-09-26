#!/usr/bin/env bash
#
# test-batch-conflict.sh — one conflict table for batch.sh: what happens to a
#   CERTIFIED branch when the base moves out from under it. Merged (sp-s088v.16):
#   test-batch-cited-commit.sh's integration cases 7-9 (bare-sha vs declared vs
#   named citation deciding reopen-vs-landed) plus test-batch.sh's g/h/i/m
#   (conflict reopen stamps bump_requeue, a non-conflicting rebase is silent,
#   a true rebase conflict still reopens, and the reopen note names the file).
#   The six bead_cited_commit_on_base unit cases stayed behind in
#   test-batch-cited-commit.sh, demoted to T2 (no batch.sh, no git conflict,
#   no bd needed for those — just the function).
#
# defect: sp-c9d41 sp-jjnmc
# tier: T1
# covers: spira/batch.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-batch-conflict
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up batchconflict || { echo "test-batch-conflict: could not build fixture database"; exit 1; }

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

cp "$HERE"/*.sh "$HERE"/*.py "$SH/"

GATE_COUNT="$TMP/gate-count"
: > "$GATE_COUNT"
cat > "$SH/gate.sh" <<GSTUB
#!/usr/bin/env bash
printf '%s\n' "\$1" >> "$GATE_COUNT"
exit 0
GSTUB
chmod +x "$SH/gate.sh"

FORGE_LOG="$TMP/forge-log"
BODY_LOG="$TMP/body-log"
cat > "$SH/forge-fixture.sh" << FORGE
#!/usr/bin/env bash
cmd="\${1:-}"; shift; repo="\${1:-}"; shift
case "\$cmd" in
    main-gate-status) printf 'green deadbeef\n' ;;
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
: > "$FORGE_LOG"; : > "$BODY_LOG"

printf '#!/usr/bin/env bash\ntrue\n' > "$SH/mail.sh"; chmod +x "$SH/mail.sh"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

B() { "${TESTDB_BD:-bd}" -C "$SPIRA_DB" "$@"; }

batch() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_MAX=8 \
    SPIRA_QUEUE_BATCH_WAIT=0 \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
        bash "$SH/batch.sh" "$@" 2>&1
}

seed() {
    testdb_reset
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
}

plant_bead() {   # plant_bead <id> [<title>]
    printf \
        '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$1" "${2:-$1}" "$1" | testdb_seed
}

status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status",""))' 2>/dev/null; }

branch_exists() { git -C "$REPO" show-ref --verify -q "refs/heads/spira/$1" 2>/dev/null; }
open_batch_file() { printf '%s/%s/open' "$QUEUEDIR" "$REPONAME"; }
batch_pr() { grep '^pr=' "$(open_batch_file)" 2>/dev/null | cut -d= -f2; }

clean_case() {
    rm -f "$QUEUEDIR/$REPONAME/open"
    : > "$FORGE_LOG"; : > "$BODY_LOG"; : > "$GATE_COUNT"
    find "$LANDSTATE" -maxdepth 1 -type f -delete 2>/dev/null
    local wt
    for wt in "$RUN/worktree"/sp-*; do
        [ -d "$wt" ] && git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    done
    rm -rf "$RUN/worktree" && mkdir -p "$RUN/worktree"
    git -C "$REPO" worktree prune 2>/dev/null || true
    git -C "$REPO" for-each-ref --format='%(refname:short)' 'refs/heads/spira/*' 2>/dev/null \
        | while read -r br; do git -C "$REPO" branch -D "$br" 2>/dev/null || true; done
    cp "$HERE/lib.sh" "$SH/lib.sh"
}

echo "test-batch-conflict.sh"

# =============================================================================
# CASE 7 (integration control) — conflicting branch, no valid citation → the
# bead is reopened and the branch survives. Proves the reopen path fires before
# trusting any "no reopen" assertion in cases 8/9.
# =============================================================================
echo
echo "case 7 — conflicting branch, no citation: reopened, branch survives:"
seed
git -C "$REPO" worktree add -q -b "spira/sp-ctrl" "$RUN/worktree/sp-ctrl" main
printf 'aeon-version\n' > "$RUN/worktree/sp-ctrl/f.txt"
git -C "$RUN/worktree/sp-ctrl" add -A
git -C "$RUN/worktree/sp-ctrl" commit -q -m "sp-ctrl: fix via aeon"
_ctrl_tip="$(git -C "$REPO" rev-parse spira/sp-ctrl)"
printf 'CERTIFIED %s %s\n' "$_ctrl_tip" "$(date +%s)" > "$LANDSTATE/sp-ctrl"
plant_bead sp-ctrl
printf 'main-ver\n' > "$REPO/f.txt"
git -C "$REPO" add f.txt && git -C "$REPO" commit -q -m "main: set f.txt"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

out="$(batch "$REPONAME")"
want  "case 7: batch reports conflict" "conflicts with" "$out"
want  "case 7: batch reports reopen"   "reopened"       "$out"
nowant "case 7: batch did not cite"    "notes cite"      "$out"
is    "case 7: sp-ctrl is reopened"    open              "$(status_of sp-ctrl)"
if branch_exists sp-ctrl; then ok "case 7: sp-ctrl branch survives reopen"
else bad "case 7: sp-ctrl branch survives reopen" "branch was deleted"; fi
clean_case

# =============================================================================
# CASE 8 (sp-jjnmc) — PARTIAL CITATION: a naming commit is on base, but the
# branch has one more commit not on base that conflicts. batch must reopen,
# not mark landed — a partial landing is not a full landing.
# CASE 9 — FULL CITATION (positive control): tip already an ancestor of base
# → LANDED (already-in-base). Proves the unlanded check does not also break
# the case where the branch genuinely finished.
# =============================================================================
echo
echo "case 8 — partial citation (extra unlanded commit) → reopened, not landed:"
echo "case 9 — full citation (tip already on base) → landed (positive control):"
seed
printf 'partial8-first-part\n' > "$REPO/p8a.txt"
git -C "$REPO" add p8a.txt
git -C "$REPO" commit -q -m "sp-partial8: first part of the work"
PARTIAL8_NAMING_SHA="$(git -C "$REPO" rev-parse HEAD)"
printf 'main-advance\n' > "$REPO/p8conflict.txt"
git -C "$REPO" add p8conflict.txt
git -C "$REPO" commit -q -m "main: advance after sp-partial8 naming commit"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

git -C "$REPO" branch "spira/sp-partial8" "$PARTIAL8_NAMING_SHA"
git -C "$REPO" worktree add -q "$RUN/worktree/sp-partial8" "spira/sp-partial8"
printf 'partial8-version\n' > "$RUN/worktree/sp-partial8/p8conflict.txt"
git -C "$RUN/worktree/sp-partial8" add p8conflict.txt
git -C "$RUN/worktree/sp-partial8" commit -q -m "sp-partial8: second part (extra work not on main)"
PARTIAL8_TIP="$(git -C "$REPO" rev-parse spira/sp-partial8)"

git -C "$REPO" branch "spira/sp-full9" "$PARTIAL8_NAMING_SHA"

plant_bead sp-partial8
plant_bead sp-full9
B note sp-partial8 "partial fix applied at $PARTIAL8_NAMING_SHA, streamer removal pending" >/dev/null 2>&1 || true
B note sp-full9    "all work done; naming commit $PARTIAL8_NAMING_SHA is on main"          >/dev/null 2>&1 || true

printf 'CERTIFIED %s %s\n' "$PARTIAL8_TIP"        "$(date +%s)" > "$LANDSTATE/sp-partial8"
printf 'CERTIFIED %s %s\n' "$PARTIAL8_NAMING_SHA"  "$(date +%s)" > "$LANDSTATE/sp-full9"

out89="$(batch "$REPONAME")"
want  "case 8: batch notes partial citation"   "unlanded commits"  "$out89"
want  "case 8: batch reopens sp-partial8"      "reopened"          "$out89"
nowant "case 8: batch did not mark p8 landed"  "marked landed"     "$out89"
is    "case 8: sp-partial8 is reopened"        open                "$(status_of sp-partial8)"
if branch_exists sp-partial8; then ok "case 8: sp-partial8 branch survives reopen"
else bad "case 8: sp-partial8 branch survives reopen" "branch was deleted"; fi

want  "case 9: batch marks sp-full9 landed"    "LANDED"            "$out89"
want  "case 9: batch notes already-in-base"    "already in"        "$out89"
_full9_ls="$(cat "$LANDSTATE/sp-full9" 2>/dev/null || true)"
want  "case 9: sp-full9 landstate is LANDED"   "LANDED"            "$_full9_ls"
clean_case

# =============================================================================
# g. BASE-CONFLICT STAMP: a branch that conflicts with origin/main is reopened
#    AND stamped merge-conflict via bump_requeue, matching landing.sh:624/922.
#    POSITIVE CONTROL: the spy file is empty before the run.
# =============================================================================
echo
echo "g. base-conflict: reopened AND stamped merge-conflict:"
seed
NOW="$(date +%s)"; OLD_G=$(( NOW - 1800 - 1 ))
git -C "$REPO" worktree add -q -b "spira/sp-btg" "$RUN/worktree/sp-btg" main 2>/dev/null || true
printf 'branch-ver\n' > "$RUN/worktree/sp-btg/conflict.txt"
git -C "$RUN/worktree/sp-btg" add -A
git -C "$RUN/worktree/sp-btg" commit -q -m "sp-btg: work"
tip_g="$(git -C "$REPO" rev-parse "spira/sp-btg")"
printf 'CERTIFIED %s %s\n' "$tip_g" "$OLD_G" > "$LANDSTATE/sp-btg"
plant_bead "sp-btg"

printf 'main-ver\n' > "$REPO/conflict.txt"
git -C "$REPO" add conflict.txt
git -C "$REPO" commit -q -m "main: set conflict.txt"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

REQUEUE_SPY_G="$TMP/rq-spy-g"
: > "$REQUEUE_SPY_G"
cat >> "$SH/lib.sh" << LIBSPY
bump_requeue() {
    printf '%s %s\n' "\${1:-}" "\${2:-}" >> "$REQUEUE_SPY_G"
    _bump_write_event "\${1:-}" requeued "\${2:-unrecorded}"
}
LIBSPY

out_g="$(batch "$REPONAME" 2>&1)"
want "g. base-conflict: reopened message"            "conflicts with"   "$out_g"
is   "g. base-conflict: bump_requeue merge-conflict" "1" \
    "$(grep -c "^sp-btg merge-conflict$" "$REQUEUE_SPY_G" 2>/dev/null || echo 0)"
clean_case

# =============================================================================
# h. REBASE-CLEAN: a CERTIFIED branch whose base moved with a non-overlapping
#    change is batched without a reopen.
# =============================================================================
echo
echo "h. rebase-clean: no reopen when the base's change does not overlap:"
seed
NOW="$(date +%s)"; OLD_H=$(( NOW - 1800 - 1 ))
git -C "$REPO" worktree add -q -b "spira/sp-bth" "$RUN/worktree/sp-bth" main 2>/dev/null || true
printf 'branch-value\n' > "$RUN/worktree/sp-bth/clean.txt"
git -C "$RUN/worktree/sp-bth" add -A
git -C "$RUN/worktree/sp-bth" commit -q -m "sp-bth: add clean.txt"
tip_h="$(git -C "$REPO" rev-parse "spira/sp-bth")"
printf 'CERTIFIED %s %s\n' "$tip_h" "$OLD_H" > "$LANDSTATE/sp-bth"
plant_bead "sp-bth"

printf 'main-value\n' > "$REPO/other.txt"
git -C "$REPO" add other.txt
git -C "$REPO" commit -q -m "main: add other.txt"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

batch "$REPONAME" >/dev/null 2>&1
is   "h. rebase-clean: sp-bth is BATCHED"       "1" \
    "$(grep -q '^BATCHED' "$LANDSTATE/sp-bth" 2>/dev/null && echo 1 || echo 0)"
is   "h. rebase-clean: not reopened (no RED)"    "0" \
    "$([ "$(awk '{print $1}' "$LANDSTATE/sp-bth" 2>/dev/null)" = "RED" ] && echo 1 || echo 0)"
is   "h. rebase-clean: PR opened"                "1" "$(batch_pr)"
clean_case

# =============================================================================
# i. REBASE-CONFLICT: a CERTIFIED branch with a true conflict against the new
#    base is still reopened after a failed rebase attempt.
#    POSITIVE CONTROL: the spy file is empty before the run.
# =============================================================================
echo
echo "i. rebase-conflict: a true conflict still reopens after the rebase attempt:"
seed
NOW="$(date +%s)"; OLD_I=$(( NOW - 1800 - 1 ))
git -C "$REPO" worktree add -q -b "spira/sp-bti" "$RUN/worktree/sp-bti" main 2>/dev/null || true
printf 'branch-ver\n' > "$RUN/worktree/sp-bti/conflict2.txt"
git -C "$RUN/worktree/sp-bti" add -A
git -C "$RUN/worktree/sp-bti" commit -q -m "sp-bti: set conflict2.txt"
tip_i="$(git -C "$REPO" rev-parse "spira/sp-bti")"
printf 'CERTIFIED %s %s\n' "$tip_i" "$OLD_I" > "$LANDSTATE/sp-bti"
plant_bead "sp-bti"

printf 'main-ver\n' > "$REPO/conflict2.txt"
git -C "$REPO" add conflict2.txt
git -C "$REPO" commit -q -m "main: set conflict2.txt"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

REQUEUE_SPY_I="$TMP/rq-spy-i"
: > "$REQUEUE_SPY_I"
cat >> "$SH/lib.sh" << LIBSPY_I
bump_requeue() {
    printf '%s %s\n' "\${1:-}" "\${2:-}" >> "$REQUEUE_SPY_I"
    _bump_write_event "\${1:-}" requeued "\${2:-unrecorded}"
}
LIBSPY_I

out_i="$(batch "$REPONAME" 2>&1)"
is   "i. rebase-conflict: sp-bti reopened (RED)"    "1" \
    "$([ "$(awk '{print $1}' "$LANDSTATE/sp-bti" 2>/dev/null)" = "RED" ] && echo 1 || echo 0)"
want "i. rebase-conflict: reopened message"          "conflicts with" "$out_i"
is   "i. rebase-conflict: bump_requeue called"       "1" \
    "$(grep -c "^sp-bti merge-conflict$" "$REQUEUE_SPY_I" 2>/dev/null || echo 0)"
clean_case

# =============================================================================
# m. CONFLICT-NOTE PATHS: a CERTIFIED branch that conflicts with the new base
#    is reopened with a note naming the specific conflicting file, not a bare
#    "conflicts with $base" sentence.
#    POSITIVE CONTROL: the note is read back from bd after the run; a batch
#    that never called bead_reopen produces an empty note.
# =============================================================================
echo
echo "m. conflict-note: the reopen note names the conflicting file:"
seed
NOW="$(date +%s)"; OLD_M=$(( NOW - 1800 - 1 ))
git -C "$REPO" worktree add -q -b "spira/sp-btm" "$RUN/worktree/sp-btm" main 2>/dev/null || true
printf 'branch-ver\n' > "$RUN/worktree/sp-btm/conflict3.txt"
git -C "$RUN/worktree/sp-btm" add -A
git -C "$RUN/worktree/sp-btm" commit -q -m "sp-btm: set conflict3.txt"
tip_m="$(git -C "$REPO" rev-parse "spira/sp-btm")"
printf 'CERTIFIED %s %s\n' "$tip_m" "$OLD_M" > "$LANDSTATE/sp-btm"
plant_bead "sp-btm"

printf 'main-ver\n' > "$REPO/conflict3.txt"
git -C "$REPO" add conflict3.txt
git -C "$REPO" commit -q -m "main: set conflict3.txt"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

batch "$REPONAME" >/dev/null 2>&1

note_m="$(B show sp-btm --json 2>/dev/null \
    | python3 -c "import json,sys;d=json.load(sys.stdin);d=d[0] if isinstance(d,list) else d;print(d.get('notes') or '')" \
    2>/dev/null || true)"
is   "m. positive-control: sp-btm reopened (RED)" "1" \
    "$([ "$(awk '{print $1}' "$LANDSTATE/sp-btm" 2>/dev/null)" = "RED" ] && echo 1 || echo 0)"
want "m. conflict-note: names conflicting file" "conflict3.txt" "$note_m"
clean_case
tl_summary
