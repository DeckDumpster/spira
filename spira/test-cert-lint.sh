#!/usr/bin/env bash
# test-cert-lint.sh — scratch-fence failure at queue-mode certification
#
# A branch carrying a root-level aeon scratch file (sp-*) must be refused
# certification before a CERTIFIED record is written. The gate runs the real
# scratch-fence.sh against the branch; this test verifies the mechanism ends
# the certification and reopens the bead, naming the file.
#
# SEEN RED FIRST (law-a-regression-test-must-be-seen-to-fail)
# Before sp-hm2vw, queue-mode certification ran no gate. A branch carrying
# sp-zmd3u-sop-verification.md passed certification, joined a batch of six,
# and blocked all five innocent members when CI caught it eight minutes into
# an image build (PR 59, run 35272201543). The gate stub here is called once
# per branch; a stub that exited 0 unconditionally (no gate before sp-hm2vw)
# would certify the scratch branch — that is the red this test was written to
# catch.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control)
# The gate count proves the gate ran. Without the count, a certification that
# ran no gate and one that ran a passing gate are the same silence. Two
# branches are tested: one with a scratch file (gate must fail, branch
# reopened) and one without (gate must pass, branch certified). The clean
# branch proves the gate does not refuse everything.
#
# covers: spira/landing.sh spira/scratch-fence.sh
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-cert-lint
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up cert-lint || { echo "test-cert-lint: could not build a fixture database"; exit 1; }
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
stub gh 'exit 1'

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

GATE_COUNT="$TMP/gate-count"

# GATE STUB: runs the real scratch-fence.sh against the branch worktree.
# scratch-fence.sh locates the git root via its own script path ($0), so it
# is placed inside the worktree at .test-spira/ — that subdir resolves to the
# worktree root via `git rev-parse --show-toplevel`. The directory is untracked
# (created after the branch commit), so it does not appear in `git ls-files`.
stub gate.sh '
id="${1#spira/}"
tree="'"$RUN"'/worktree/$id"
printf "%s\n" "$1" >> "'"$GATE_COUNT"'"
scr_dir="$tree/.test-spira"
mkdir -p "$scr_dir"
cp "'"$SH"'/scratch-fence.sh" "$scr_dir/scratch-fence.sh"
if ! fence_out="$(bash "$scr_dir/scratch-fence.sh" 2>&1)"; then
    printf "%s\n" "$fence_out" >&2
    printf "gate: VERDICT=FAIL reason=scratch-fence branch=%s repo=%s suite=-\n" "$1" "${2:-?}" >&2
    exit 1
fi
printf "gate: VERDICT=PASS reason=stub branch=%s repo=%s suite=-\n" "$1" "${2:-?}" >&2
exit 0'

B() { bd -C "$SPIRA_DB" "$@"; }
status_of() {
    B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'
}

landing() {
    rm -f "$RUN/landing.progress" "$GATE_COUNT"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" \
    SPIRA_REPO_MAP="$SH/repo-map" \
        bash "$SH/landing.sh" 2>&1
}

seed() {
    testdb_reset
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
}

bead_for() {
    local id="$1"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$id" "$id" "$id" | testdb_seed
}

main_tip()  { git -C "$REMOTE" rev-parse main 2>/dev/null; }
landstate() { cat "$RUN/landstate/${1:-}" 2>/dev/null; }
gate_n()    { [ -f "$GATE_COUNT" ] && wc -l < "$GATE_COUNT" || echo 0; }

echo "test-cert-lint.sh"

# -------------------------------------------------------------------------------------
# SCRATCH FILE AT ROOT: certification refused, bead reopened, file named.
# Simulates sp-zmd3u-sop-verification.md in PR 59 (spira/sp-zmd3u before 773de98).
# -------------------------------------------------------------------------------------
seed
git -C "$REPO" worktree add -q -b "spira/sp-clnt-drt" "$RUN/worktree/sp-clnt-drt" main
printf 'real work\n' > "$RUN/worktree/sp-clnt-drt/work.txt"
printf 'aeon working note\n' > "$RUN/worktree/sp-clnt-drt/sp-clnt-drt.txt"
git -C "$RUN/worktree/sp-clnt-drt" add -A
git -C "$RUN/worktree/sp-clnt-drt" commit -q -m "sp-t8tmq sp-clnt-drt — adds scratch note at root"
bead_for sp-clnt-drt

before="$(main_tip)"
out="$(landing)"

is   "gate called for scratch-file branch"       "1"        "$(gate_n)"
want "scratch branch is reopened"                "reopened sp-clnt-drt" "$out"
is   "reopened bead status is open"              "open"     "$(status_of sp-clnt-drt)"
is   "remote main unchanged after refusal"       "$before"  "$(main_tip)"
case "$(landstate sp-clnt-drt)" in
    RED*) ok "landstate is RED for scratch-fence failure" ;;
    *)    bad "landstate is RED for scratch-fence failure" "got: $(landstate sp-clnt-drt)" ;;
esac
# The reopen note must name the scratch file so the next aeon knows what to remove.
note_out="$(B show sp-clnt-drt --json 2>/dev/null \
    | python3 -c '
import json, sys
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
notes = d[0].get("notes", []) if d else []
print(" ".join(str(n) for n in notes))' 2>/dev/null || true)"
want "reopen note names the scratch file"        "sp-clnt-drt.txt" "$note_out"

# -------------------------------------------------------------------------------------
# POSITIVE CONTROL: clean branch is certified.
# Proves the gate does not refuse branches that carry no scratch files.
# -------------------------------------------------------------------------------------
seed
git -C "$REPO" worktree add -q -b "spira/sp-clnt-cln" "$RUN/worktree/sp-clnt-cln" main
printf 'real work, no scratch file\n' > "$RUN/worktree/sp-clnt-cln/work.txt"
git -C "$RUN/worktree/sp-clnt-cln" add -A
git -C "$RUN/worktree/sp-clnt-cln" commit -q -m "sp-t8tmq sp-clnt-cln — clean branch"
bead_for sp-clnt-cln

out="$(landing)"
is   "gate called for clean branch"              "1"        "$(gate_n)"
want "clean branch is certified"                 "certified spira/sp-clnt-cln" "$out"
case "$(landstate sp-clnt-cln)" in
    CERTIFIED*) ok "landstate is CERTIFIED for clean branch" ;;
    *)          bad "landstate is CERTIFIED for clean branch" "got: $(landstate sp-clnt-cln)" ;;
esac

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
