#!/usr/bin/env bash
# test-certify.sh — queue land mode: a closed branch is CERTIFIED immediately, with no
# local rebase and no local gate (law-a-round-takes-certified-tips). The round (batcher-cut)
# and CI are the only judges of a SUBMITTED branch now; this pass is pure bookkeeping so the
# round's own CERTIFIED-pool read finds a branch the moment its bead closes.
#
# Cases: a branch is CERTIFIED with zero gate.sh calls; a second pass at the same tip is a
# no-op; a tip move re-certifies the new tip, still with no gate call; ten branches in one
# pass all reach CERTIFIED with zero gate.sh invocations total; a push-mode fixture in the
# same repo-map still pushes and still calls the gate (positive control — proves the gate
# stub counter works and that push mode is unaffected).
#
# The gate is a stub counter. Every "zero calls" assertion below depends on the positive
# control showing the counter works: the push-mode fixture must increment it.
#
# confine.sh is a stub; the real db is testdb.sh with an embedded engine.
# The bare remote is real git so ancestry checks are real.
#
# covers: spira/landing.sh spira/conf.sh
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-certify
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up certify || { echo "test-certify: could not build a fixture database"; exit 1; }
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
stub queue.sh 'exit 0'
stub gh 'exit 1'

# THE GATE IS ALSO THE COUNTER. Each invocation appends the branch name so the suite can
# assert it was (or was not) called (law-absence-needs-a-positive-control).
GATE_COUNT="$TMP/gate-count"
stub gate.sh '
printf "%s\n" "$1" >> "'"$GATE_COUNT"'"
printf "gate: VERDICT=PASS reason=stub branch=%s repo=%s\n" "$1" "${2:-?}" >&2
exit 0'

B() { bd -C "$SPIRA_DB" "$@"; }
status_of() {
    B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'
}

# QUEUE-MODE repo-map. Pinned to a non-default mode so an assertion against the
# default "push" would fail rather than pass vacuously.
write_map() {
    cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP
}
write_map

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

branch() {
    local id="$1" f="${2:-$1.txt}" c="${3:-$1}"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$c" > "$RUN/worktree/$id/$f"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id sp-1fm88 — work"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$id" "$id" "$id" | testdb_seed
}

main_tip()     { git -C "$REMOTE" rev-parse main 2>/dev/null; }
landstate()    { cat "$RUN/landstate/${1:-}" 2>/dev/null; }
gate_n()       { [ -f "$GATE_COUNT" ] && wc -l < "$GATE_COUNT" || echo 0; }

echo "test-certify.sh"

# -----------------------------------------------------------------------------------------
# QUEUE-MODE ENTRY: a closed branch is CERTIFIED with zero gate calls.
# -----------------------------------------------------------------------------------------
seed; branch sp-cert-green
before="$(main_tip)"
out="$(landing)"
is   "gate NOT called at queue-mode entry"   "0"        "$(gate_n)"
want "certify is reported"                   "certified spira/sp-cert-green" "$out"
is   "remote main is unchanged"              "$before"  "$(main_tip)"
case "$(landstate sp-cert-green)" in
    CERTIFIED*) ok "landstate says CERTIFIED" ;;
    *)          bad "landstate says CERTIFIED" "got: $(landstate sp-cert-green)" ;;
esac
is   "bead stays closed after certify"       "closed"   "$(status_of sp-cert-green)"

# -----------------------------------------------------------------------------------------
# SECOND PASS ON THE SAME TIP: no-op — no gate call, no duplicate certify message.
# -----------------------------------------------------------------------------------------
out2="$(landing)"
is   "gate still not called on second pass"  "0"        "$(gate_n)"
nowant "no second certify message"           "certified spira/sp-cert-green" "$out2"

# -----------------------------------------------------------------------------------------
# TIP MOVE: moving the branch tip after certification re-certifies the new tip — still
# with zero gate calls.
# -----------------------------------------------------------------------------------------
seed; branch sp-cert-move
landing > /dev/null   # first pass: certify
printf 'v2\n' > "$RUN/worktree/sp-cert-move/sp-cert-move.txt"
git -C "$RUN/worktree/sp-cert-move" add -A
git -C "$RUN/worktree/sp-cert-move" commit -q -m "sp-cert-move sp-1fm88 — second commit"
new_tip="$(git -C "$REPO" rev-parse spira/sp-cert-move 2>/dev/null)"
out="$(landing)"   # second pass: re-certifies the new tip
is   "gate not called after tip move"  "0"  "$(gate_n)"
want "second certify reported"         "certified spira/sp-cert-move" "$out"
case "$(landstate sp-cert-move)" in
    *"$new_tip"*) ok "landstate updated to new tip" ;;
    *)            bad "landstate updated to new tip" "expected [$new_tip] in [$(landstate sp-cert-move)]" ;;
esac

# -----------------------------------------------------------------------------------------
# WITHDRAWN AT THE SAME TIP STAYS WITHDRAWN (sp-pedat): a bead ejected while certified
# stays closed in the store (queue.sh eject's mid-batch path writes RED/WITHDRAWN without
# reopening the bd issue), so this pass reaches it as an ordinary closed queue-mode bead.
# "not CERTIFIED" must not be read as "needs certifying" — a withdrawal at the current tip
# holds until the tip moves.
# -----------------------------------------------------------------------------------------
seed; branch sp-cert-withdrawn
landing > /dev/null   # first pass: certify
wd_tip="$(git -C "$REPO" rev-parse spira/sp-cert-withdrawn 2>/dev/null)"
printf 'WITHDRAWN %s %s eject\n' "$wd_tip" "$(date +%s)" > "$RUN/landstate/sp-cert-withdrawn"
rm -f "$GATE_COUNT" "$RUN/landing.progress"
out="$(landing)"
is   "gate not called on a withdrawn bead" "0" "$(gate_n)"
nowant "no re-certify message for the withdrawn bead" "certified spira/sp-cert-withdrawn" "$out"
case "$(landstate sp-cert-withdrawn)" in
    WITHDRAWN*) ok "landstate stays WITHDRAWN at the same tip" ;;
    *)          bad "landstate stays WITHDRAWN at the same tip" "got: $(landstate sp-cert-withdrawn)" ;;
esac

# A new tip (the aeon's rework) recertifies normally.
printf 'v2\n' > "$RUN/worktree/sp-cert-withdrawn/sp-cert-withdrawn.txt"
git -C "$RUN/worktree/sp-cert-withdrawn" add -A
git -C "$RUN/worktree/sp-cert-withdrawn" commit -q -m "sp-cert-withdrawn sp-1fm88 — rework"
wd_tip2="$(git -C "$REPO" rev-parse spira/sp-cert-withdrawn 2>/dev/null)"
out="$(landing)"
want "moved tip: certify is reported" "certified spira/sp-cert-withdrawn" "$out"
case "$(landstate sp-cert-withdrawn)" in
    *"$wd_tip2"*) ok "landstate certifies the new tip" ;;
    *)            bad "landstate certifies the new tip" "expected [$wd_tip2] in [$(landstate sp-cert-withdrawn)]" ;;
esac

# -----------------------------------------------------------------------------------------
# TEN BRANCHES, ONE PASS: every one reaches CERTIFIED and the gate is never invoked.
# The acceptance fixture for law-a-round-takes-certified-tips — must be seen to fail
# against a landing.sh that still gates queue-mode branches before certifying them.
# -----------------------------------------------------------------------------------------
seed
for i in 1 2 3 4 5 6 7 8 9 10; do
    branch "sp-ten-$i"
done
out="$(landing)"
is "ten branches: zero gate.sh invocations" "0" "$(gate_n)"
_certified_n=0
for i in 1 2 3 4 5 6 7 8 9 10; do
    case "$(landstate "sp-ten-$i")" in
        CERTIFIED*) _certified_n=$(( _certified_n + 1 )) ;;
        *) bad "sp-ten-$i reached CERTIFIED" "got: $(landstate "sp-ten-$i")" ;;
    esac
done
is "ten branches: all ten reached CERTIFIED in one pass" "10" "$_certified_n"

# -----------------------------------------------------------------------------------------
# PUSH MODE STILL PUSHES, AND STILL GATES: a push-mode repo in the same repo-map lands
# normally and still calls the gate — proves the counter is live and queue-mode's change
# does not touch push mode.
# -----------------------------------------------------------------------------------------
PUSHREMOTE="$TMP/push-remote.git"
git init -q --bare -b main "$PUSHREMOTE"

PUSHREPO="$TMP/push-repo"
git init -q -b main "$PUSHREPO"
git -C "$PUSHREPO" commit -q --allow-empty -m base
git -C "$PUSHREPO" remote add origin "$PUSHREMOTE"
git -C "$PUSHREPO" push -q origin main
git -C "$PUSHREPO" fetch -q origin
mkdir -p "$RUN/worktree-push"
git -C "$PUSHREPO" worktree add -q -b "spira/sp-push-a" "$RUN/worktree-push/sp-push-a" main
printf 'push-work\n' > "$RUN/worktree-push/sp-push-a/sp-push-a.txt"
git -C "$RUN/worktree-push/sp-push-a" add -A
git -C "$RUN/worktree-push/sp-push-a" commit -q -m "sp-push-a sp-1fm88 — push work"

cat > "$SH/repo-map" <<RMAP
$REPONAME    | $REPO     | queue | origin/main | | |
push-fixture | $PUSHREPO | push  | origin/main | | |
RMAP

testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-push-a","title":"push-a","status":"closed","issue_type":"task","labels":["repo:push-fixture"],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-push-a","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL

push_before="$(git -C "$PUSHREMOTE" rev-parse main 2>/dev/null)"
out="$(landing)"
push_after="$(git -C "$PUSHREMOTE" rev-parse main 2>/dev/null)"
want "push-mode branch reports landed"  "landed spira/sp-push-a"  "$out"
is   "push-mode remote actually moved"  "yes"  "$([ "$push_before" != "$push_after" ] && echo yes || echo no)"
is   "push-mode positive control: gate was called" "1" "$(gate_n)"

# -----------------------------------------------------------------------------------------
# QUEUE.LOCAL: the same certify-not-rebase arm (sp-xe12f) — a closed branch is CERTIFIED
# with zero gate calls and, unlike every other mode, is NEVER REBASED onto the (local) base
# even when it conflicts. Before sp-xe12f, landing.sh's queue-mode arm matched only the
# literal string "queue", so a queue.local repo fell through to the ordinary rebase path
# below it and this whole case reopened the bead on the planted conflict instead of
# certifying it — the positive control for that: local/main advances with a conflicting
# edit to the SAME file the branch touches, so a landing pass that attempts the rebase (the
# old behaviour) must hit that conflict and reopen the bead, while one that only certifies
# (the fixed behaviour) leaves the branch's own tip byte-identical and the bead closed.
# -----------------------------------------------------------------------------------------
LOCALREPO="$TMP/local-repo"
git init -q -b trunk "$LOCALREPO"
git -C "$LOCALREPO" commit -q --allow-empty -m base
git -C "$LOCALREPO" branch local/main trunk

testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-qlocal-cert","title":"qlocal","status":"closed","issue_type":"task","labels":["repo:local-fixture"],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-qlocal-cert","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL

mkdir -p "$RUN/worktree-local"
git -C "$LOCALREPO" worktree add -q -b "spira/sp-qlocal-cert" "$RUN/worktree-local/sp-qlocal-cert" local/main
printf 'branch-version\n' > "$RUN/worktree-local/sp-qlocal-cert/shared.txt"
git -C "$RUN/worktree-local/sp-qlocal-cert" add -A
git -C "$RUN/worktree-local/sp-qlocal-cert" commit -q -m "sp-qlocal-cert sp-1fm88 — the work"
qlocal_tip="$(git -C "$LOCALREPO" rev-parse spira/sp-qlocal-cert)"

# Advance local/main with a CONFLICTING edit to the same file, after the branch was cut.
git -C "$LOCALREPO" checkout -q local/main
printf 'base-version\n' > "$LOCALREPO/shared.txt"
git -C "$LOCALREPO" add -A
git -C "$LOCALREPO" commit -q -m "base moves on, conflicting"
git -C "$LOCALREPO" checkout -q trunk
localmain_before="$(git -C "$LOCALREPO" rev-parse local/main)"

cat > "$SH/repo-map" <<RMAP
$REPONAME     | $REPO      | queue       | origin/main | | |
local-fixture | $LOCALREPO | queue.local | local/main  | | |
RMAP
rm -f "$GATE_COUNT" "$RUN/landing.progress"
out="$(landing)"
is   "queue.local: gate NOT called"          "0" "$(gate_n)"
want "queue.local: certify is reported"      "certified spira/sp-qlocal-cert" "$out"
is   "queue.local: local/main is untouched"  "$localmain_before" "$(git -C "$LOCALREPO" rev-parse local/main)"
is   "queue.local: branch tip is byte-identical (never rebased)" \
    "$qlocal_tip" "$(git -C "$LOCALREPO" rev-parse spira/sp-qlocal-cert)"
case "$(landstate sp-qlocal-cert)" in
    CERTIFIED*) ok "queue.local: landstate says CERTIFIED" ;;
    *)          bad "queue.local: landstate says CERTIFIED" "got: $(landstate sp-qlocal-cert)" ;;
esac
is   "queue.local: bead stays closed"        "closed" "$(status_of sp-qlocal-cert)"
nowant "queue.local: no reopen for the planted conflict" "Reopened by sentinel" "$out"

tl_summary
