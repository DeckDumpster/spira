#!/usr/bin/env bash
# test-certify.sh — queue land mode: certify without pushing.
#
# Four cases: a green branch is CERTIFIED and the remote's main is unchanged;
# a red branch is reopened; moving the tip clears the record so the next pass
# re-gates; a push-mode fixture in the same repo-map still pushes.
#
# gate.sh and confine.sh are stubs; the real db is testdb.sh with an embedded
# engine. The bare remote is real git so ancestry checks are real.
#
# covers: spira/landing.sh spira/conf.sh
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
is()     { [ "$2" = "$3" ]        && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
nowant() { [[ "$3" != *"$2"* ]]   && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

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

cp "$HERE"/*.sh "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub confine.sh 'exit 0'
stub ask.sh 'printf "%s\n" "$*" >> "$EMITTED"'
export EMITTED="$TMP/events"; : > "$EMITTED"

# THE GATE IS ALSO THE COUNTER. Each invocation appends the branch name so the
# suite can assert it was (or was not) called, which is the positive control every
# silence here depends on.
GATE_COUNT="$TMP/gate-count"
stub gate.sh '
printf "%s\n" "$1" >> "'"$GATE_COUNT"'"
printf "gate: VERDICT=PASS reason=stub branch=%s repo=%s\n" "$1" "${2:-?}" >&2
exit 0'

stub gh 'exit 1'

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
    SPIRA_NOTIFY="$SH/ask.sh" SPIRA_ASK="$SH/ask.sh" \
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

main_tip()  { git -C "$REMOTE" rev-parse main 2>/dev/null; }
landstate() { cat "$RUN/landstate/${1:-}" 2>/dev/null; }
gate_n()    { [ -f "$GATE_COUNT" ] && wc -l < "$GATE_COUNT" || echo 0; }

echo "test-certify.sh"

# -----------------------------------------------------------------------------------------
# POSITIVE CONTROL: the gate is reached. Without this every assertion about "nothing
# pushed" passes just as well against a landing.sh that drops into the queue arm without
# running the gate at all.
# -----------------------------------------------------------------------------------------
seed; branch sp-cert-green
before="$(main_tip)"
out="$(landing)"
is   "gate was invoked for green branch"     "1"        "$(gate_n)"
want "certify is reported"                   "certified spira/sp-cert-green" "$out"
is   "remote main is unchanged"              "$before"  "$(main_tip)"
case "$(landstate sp-cert-green)" in
    CERTIFIED*) ok "landstate says CERTIFIED" ;;
    *)          bad "landstate says CERTIFIED" "got: $(landstate sp-cert-green)" ;;
esac
is   "bead stays closed after certify"       "closed"   "$(status_of sp-cert-green)"

# -----------------------------------------------------------------------------------------
# SECOND PASS ON THE SAME TIP: the gate must NOT run again; the certification stands.
# -----------------------------------------------------------------------------------------
out2="$(landing)"
is   "gate skipped on second pass (same tip)" "0"        "$(gate_n)"
nowant "no second certify message"            "certified spira/sp-cert-green" "$out2"

# -----------------------------------------------------------------------------------------
# RED BRANCH: a failed gate reopens the bead, same as push mode.
# -----------------------------------------------------------------------------------------
stub gate.sh '
printf "%s\n" "$1" >> "'"$GATE_COUNT"'"
printf "gate: VERDICT=FAIL reason=stub-fail branch=%s repo=%s\n" "$1" "${2:-?}" >&2
exit 1'
seed; branch sp-cert-red
out="$(landing)"
want "red branch is reopened"        "reopened sp-cert-red — failed the gate" "$out"
is   "reopened bead is open"         "open"          "$(status_of sp-cert-red)"
# Restore passing gate.
stub gate.sh '
printf "%s\n" "$1" >> "'"$GATE_COUNT"'"
printf "gate: VERDICT=PASS reason=stub branch=%s repo=%s\n" "$1" "${2:-?}" >&2
exit 0'

# -----------------------------------------------------------------------------------------
# TIP MOVE: moving the branch tip after certification clears the record. The next pass
# must re-gate rather than reuse the old certification.
# -----------------------------------------------------------------------------------------
seed; branch sp-cert-move
landing > /dev/null   # first pass: certify
# Move the tip.
printf 'v2\n' > "$RUN/worktree/sp-cert-move/sp-cert-move.txt"
git -C "$RUN/worktree/sp-cert-move" add -A
git -C "$RUN/worktree/sp-cert-move" commit -q -m "sp-cert-move sp-1fm88 — second commit"
new_tip="$(git -C "$REPO" rev-parse spira/sp-cert-move 2>/dev/null)"
out="$(landing)"   # second pass: must re-gate the new tip
is   "gate re-run after tip move"  "1"  "$(gate_n)"
want "second certify reported"     "certified spira/sp-cert-move" "$out"
case "$(landstate sp-cert-move)" in
    *"$new_tip"*) ok "landstate updated to new tip" ;;
    *)            bad "landstate updated to new tip" "expected [$new_tip] in [$(landstate sp-cert-move)]" ;;
esac

# -----------------------------------------------------------------------------------------
# PUSH MODE STILL PUSHES: a push-mode repo in the same repo-map lands normally.
# This is the other half of the positive control: the queue arm does not break push.
# -----------------------------------------------------------------------------------------
PUSHREMOTE="$TMP/push-remote.git"
git init -q --bare -b main "$PUSHREMOTE"

# Reuse the same checked-out repo for the push fixture — a second remote, a second repo-map
# entry. The landing worktree is keyed by repo basename, so two repos with the same path
# would share it; use a separate push repo to keep them independent.
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

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
