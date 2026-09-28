#!/usr/bin/env bash
#
# test-landing-cutover-round.sh — a bead labelled cutover-round lands only in the cutover
# round, assembled by hand, never through the ordinary landing pass.
#
# WHY THIS EXISTS. The landing pass kept re-judging finished cutover branches, marking them
# RED no-rebase whenever the base moved and then filing repeat-refused asks about work that
# was never going to land this way. Every one of those runs was a certify slot and an ask
# spent on a branch this pass could not have merged in the first place.
#
# TWO PROPERTIES UNDER TEST, each with its positive control (a check that finds nothing must
# first prove it could have found something):
#
#   1. CERTIFICATION. A clean, closed branch whose bead is labelled cutover-round is never
#      certified; an otherwise-identical unlabelled branch in the same pass is. Removing the
#      label from the same fixture then certifies it too — proving the skip is keyed to the
#      label and not to anything else about the branch.
#
#   2. REBASE/REPEAT MACHINERY. A closed branch that cannot rebase onto a moved base is never
#      reopened, never RED-marked and never the subject of an ask when its bead carries the
#      label — across two passes, the shape that used to escalate. An unlabelled twin in the
#      identical conflict shape reopens on the first pass and escalates (files an ask) on the
#      second, proving the harness really would have done this had the label been absent.
#
# defect: sp-umcjk
# covers: spira/landing.sh spira/conf.sh spira/lib.sh
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-landing-cutover-round
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up landing-cutover-round || {
    printf 'SKIP test-landing-cutover-round: testdb not available\n' >&2
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
stub gh 'exit 1'
stub gate.sh 'echo "gate: VERDICT=PASS reason=stub branch=$1 repo=${2:-?}" >&2; exit 0'
stub queue.sh 'exit 0'

# A NON-DEFAULT LABEL, PINNED. Asserting against the shipped default ("cutover-round") would
# pass just as well if landing.sh had that string written in literally rather than reading
# SPIRA_CUTOVER_ROUND_LABEL — which is exactly what pinning a non-default value here is for.
CUTOVER_LABEL=fixture-cutover-round

cat > "$SH/repo-map" <<MAP
$REPONAME | $REPO | queue | |
MAP

B() { bd -C "$SPIRA_DB" "$@"; }
status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }

landing() {
    rm -f "$RUN/landing.progress"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" SPIRA_ID_PREFIX=sp \
    SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/gh" \
    SPIRA_CERT_IDLE_SKIP=0 SPIRA_CUTOVER_ROUND_LABEL="$CUTOVER_LABEL" \
        bash "$SH/landing.sh" 2>&1
}

# branch <id> <file> <content> [labels-json] — a closed bead with a clean git branch.
branch() {
    local id="$1" f="$2" c="$3" labels="${4:-[]}"
    drop_branch "$id"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$c" > "$RUN/worktree/$id/$f"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id — work"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":%s,"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$id" "$id" "$labels" "$id" | testdb_seed
}

drop_branch() {
    git -C "$REPO" worktree remove --force "$RUN/worktree/$1" >/dev/null 2>&1 || true
    git -C "$REPO" branch -D "spira/$1" >/dev/null 2>&1 || true
}

advance_base() {
    printf '%s\n' "base step $1" > "$REPO/base-step.txt"
    git -C "$REPO" add base-step.txt
    git -C "$REPO" commit -q -m "base step $1"
    git -C "$REPO" push -q origin main
    git -C "$REPO" fetch -q origin
}

seed() {
    rm -rf "$RUN/submitted" "$RUN/landstate" "$RUN/tip-at-gate"
    testdb_reset
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-01T00:00:00Z"}
JSONL
}

echo "test-landing-cutover-round.sh"

# ========================================================================================
# PART 1: CERTIFICATION. Two clean branches, one labelled, in the same pass.
# ========================================================================================
echo
echo "certification skips the labelled branch, certifies the plain one:"
seed
branch sp-cut cut.txt "cutover work" "[\"$CUTOVER_LABEL\"]"
branch sp-ord ord.txt "ordinary work"

out="$(landing)"
want   "plain branch certifies"                "certified spira/sp-ord" "$out"
nowant "labelled branch is not certified"       "certified spira/sp-cut" "$out"
nowant "labelled branch is not gated"           "gate: VERDICT" "$out"
want   "log names why it was skipped"           "cutover round" "$out"
is     "labelled bead stays closed, untouched"  closed "$(status_of sp-cut)"
nowant "nothing is filed about the labelled bead" "sp-cut" "$(cat "$EMITTED")"

drop_branch sp-ord

# POSITIVE CONTROL: the same branch, same bead, minus the label, certifies. This proves the
# skip above was keyed to the label — not to the branch's id, its content, or anything else
# about the fixture.
echo
echo "positive control: the same branch without the label certifies:"
B label remove sp-cut "$CUTOVER_LABEL" >/dev/null 2>&1
out="$(landing)"
want "unlabelled branch now certifies" "certified spira/sp-cut" "$out"

drop_branch sp-cut

# ========================================================================================
# PART 2: REBASE/REPEAT MACHINERY. Two branches that cannot rebase onto a moved base — one
# labelled, one not — across two passes.
# ========================================================================================
echo
echo "rebase/repeat machinery never touches the labelled bead:"
seed
branch sp-cut2  shared-cut.txt  "from the cutover branch"  "[\"$CUTOVER_LABEL\"]"
branch sp-plain shared-plain.txt "from the plain branch"

# The base conflict that makes both branches permanently unable to rebase.
printf '%s\n' "base owns shared-cut.txt"   > "$REPO/shared-cut.txt"
printf '%s\n' "base owns shared-plain.txt" > "$REPO/shared-plain.txt"
git -C "$REPO" add -A
git -C "$REPO" commit -q -m "base: take both shared files"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

echo
echo "pass 1 — positive control fires on the plain bead, never on the labelled one:"
out="$(landing)"
want   "plain bead reopens on first RED"         "reopened sp-plain" "$out"
is     "plain bead is open after first RED"      open   "$(status_of sp-plain)"
nowant "labelled bead is never reopened"         "reopened sp-cut2"  "$out"
nowant "labelled bead is never RED-gated"        "gate: VERDICT"     "$out"
is     "labelled bead stays closed"              closed "$(status_of sp-cut2)"

echo
echo "pass 2 — plain bead escalates (recurring RED); labelled bead is still untouched:"
B close sp-plain --reason "aeon tried" >/dev/null 2>&1
git -C "$RUN/worktree/sp-plain" commit -q --allow-empty -m "sp-plain: aeon attempt 1"
advance_base 1

: > "$EMITTED"
out="$(landing)"
want   "plain bead escalates on second RED"      "escalated sp-plain" "$out"
want   "escalation for the plain bead reaches the operator" "red recurring" "$(cat "$EMITTED")"
nowant "labelled bead is never reopened"         "reopened sp-cut2"   "$out"
nowant "labelled bead is never escalated"        "escalated sp-cut2"  "$out"
is     "labelled bead is still closed after two passes" closed "$(status_of sp-cut2)"
nowant "no ask is ever filed for the labelled bead" "sp-cut2" "$(cat "$EMITTED")"

drop_branch sp-cut2
drop_branch sp-plain

tl_summary
