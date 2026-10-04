#!/usr/bin/env bash
#
# test-landing-modes.sh — the same closed bead, one repository per land mode (push, queue, pr):
# each mode does exactly its own thing to the base and to the bead, and none does another's.
#
#   push   lands the branch on the base and records bead.landed.
#   queue  certifies the branch; the base is untouched and the bead stays closed.
#   pr     pushes the branch and opens one pull request; the base is untouched, a second
#          pass opens no second pull request.
#
# Each mode's scenario ends with the cross-mode check that makes its silence mean something:
# the base moved in push mode (so "the base did not move" in the others is observable), and
# the forge stub recorded a create in pr mode (so "no create" in the others is observable).
#
# tier: T3
# covers: landing-pass/* spira/lib.sh spira/conf.sh
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-landing-modes
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up landing-modes || {
    printf 'SKIP test-landing-modes: testdb not available\n' >&2
    exit 77
}
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

RUN="$TMP/run"; SH="$TMP/spira"
mkdir -p "$RUN/worktree" "$SH"
cp "$HERE"/*.sh "$HERE"/*.py "$SH/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
export EMITTED="$TMP/events"; : > "$EMITTED"
export FORGE_LOG="$TMP/forge.log"; export FORGE_PRS="$TMP/forge.prs"
stub confine.sh 'exit 0'
stub mail '[ "${1:-}" = send ] || exit 0; printf "%s\n" "$*" >> "$EMITTED"; cat >> "$EMITTED"; printf "\n" >> "$EMITTED"'
stub gh 'exit 1'
stub gate.sh 'echo "gate: VERDICT=PASS reason=stub branch=$1 repo=${2:-?}" >&2; exit 0'
stub queue 'exit 0'
# A forge with just enough memory to tell a first pass from a second: pr-create records a
# PR, pr-list-open and pr-state read the record back.
stub forge 'printf "%s\n" "$*" >> "$FORGE_LOG"
case "${1:-}" in
    pr-create) cat >/dev/null; n=$(( $(wc -l < "$FORGE_PRS" 2>/dev/null || echo 0) + 41 )); printf "%s %s\n" "$n" "$3" >> "$FORGE_PRS"; echo "$n" ;;
    pr-list-open) cat "$FORGE_PRS" 2>/dev/null ;;
    pr-state) echo open ;;
    pr-automerge) exit 0 ;;
    *) exit 1 ;;
esac'

B() { bd -C "$SPIRA_DB" "$@"; }
status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }

# fixture <mode> — a repository named <mode>-repo with a real bare remote, a one-row land map and
# an empty ledger; sets REPO, REMOTE, REPONAME.
fixture() {
    local mode="$1"
    REPONAME="$mode-repo"; REPO="$TMP/$mode/repo"; REMOTE="$TMP/$mode/remote.git"
    mkdir -p "$TMP/$mode"
    git init -q --bare -b main "$REMOTE"
    git init -q -b main "$REPO"
    git -C "$REPO" commit -q --allow-empty -m base
    git -C "$REPO" remote add origin "$REMOTE"
    git -C "$REPO" push -q origin main
    git -C "$REPO" fetch -q origin
    printf '%s | %s | %s | origin/main | | |\n' "$REPONAME" "$REPO" "$mode" > "$SH/land-map"
    rm -rf "$RUN/submitted" "$RUN/landstate" "$RUN/tip-at-gate"
    : > "$EMITTED"; : > "$FORGE_LOG"; : > "$FORGE_PRS"
    testdb_reset
    testdb_seed <<'JSONL'
{"id":"sp-epic","title":"epic","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-01T00:00:00Z"}
JSONL
}

# closed_branch <id> — a closed bead labelled for the fixture repository, with one commit.
closed_branch() {
    local id="$1"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf 'work for %s\n' "$id" > "$RUN/worktree/$id/$id.txt"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id — work"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":["repo:%s"],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-epic","type":"parent-child"}]}\n' \
        "$id" "$id" "$REPONAME" "$id" | testdb_seed
}

pass() {  # pass <land|pr>
    rm -f "$RUN/landing.progress"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" SPIRA_ID_PREFIX=sp \
    SPIRA_REPO_MAP="$SH/land-map" SPIRA_GH="$SH/gh" \
        SPIRA_GATE_WORKER=0 PATH="$SH:$PATH" landing-pass "$1" 2>&1
}

remote_main() { git -C "$REMOTE" rev-parse main; }
on_remote_main() { git -C "$REMOTE" log --format=%s main | grep -q "$1"; }
events() { cat "$EMITTED"; }

echo "test-landing-modes.sh"

# ========================================================================================
echo
echo "push mode: the branch lands on the base"
# ========================================================================================
fixture push
closed_branch sp-pushed
base0="$(remote_main)"
out="$(pass land)"
want    "push: the land is reported"            "landed spira/sp-pushed" "$out"
if on_remote_main "sp-pushed"; then ok "push: the work is on the remote base"; else bad "push: the work is on the remote base" "$out"; fi
[ "$(remote_main)" != "$base0" ] && ok "push: the base moved" || bad "push: the base moved" "unchanged"
want    "push: the land is recorded as an event" "kind: bead.landed" "$(events)"
nowant  "push: no certification record"         "certified spira/sp-pushed" "$out"
nowant  "push: no pull request opened"          "pr-create" "$(cat "$FORGE_LOG")"

# ========================================================================================
echo
echo "queue mode: the branch is certified and the base is left for the round"
# ========================================================================================
fixture queue
closed_branch sp-queued
base0="$(remote_main)"
out="$(pass land)"
want    "queue: the branch is certified"        "certified spira/sp-queued" "$out"
nowant  "queue: it is not reported landed"      "landed spira/sp-queued" "$out"
is      "queue: the base did not move"          "$base0" "$(remote_main)"
is      "queue: the bead stays closed"          closed "$(status_of sp-queued)"
nowant  "queue: no land event"                  "kind: bead.landed" "$(events)"
nowant  "queue: no pull request opened"         "pr-create" "$(cat "$FORGE_LOG")"
want    "queue: the landstate record says CERTIFIED" "CERTIFIED" "$(cat "$RUN"/landstate/sp-queued* 2>/dev/null)"

# ========================================================================================
echo
echo "pr mode: one pull request is opened, the base is left to the forge"
# ========================================================================================
fixture pr
closed_branch sp-prd
base0="$(remote_main)"
out="$(pass pr)"
want    "pr: a pull request is opened"          "opened a pull request for spira/sp-prd" "$out"
is      "pr: the forge was asked to create exactly one" 1 "$(grep -c '^pr-create' "$FORGE_LOG")"
is      "pr: the base did not move"             "$base0" "$(remote_main)"
git -C "$REMOTE" rev-parse --verify -q refs/heads/spira/sp-prd >/dev/null \
    && ok "pr: the branch was pushed to the remote" || bad "pr: the branch was pushed to the remote" "$out"
is      "pr: the bead stays closed until the merge" closed "$(status_of sp-prd)"
nowant  "pr: no land event"                     "kind: bead.landed" "$(events)"

out="$(pass pr)"
is      "pr: a second pass opens no second pull request" 1 "$(grep -c '^pr-create' "$FORGE_LOG")"
nowant  "pr: the second pass does not report opening one" "opened a pull request" "$out"

tl_summary
