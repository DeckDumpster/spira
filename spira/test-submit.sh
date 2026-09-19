#!/usr/bin/env bash
# test-submit.sh — queue.sh submit: certify any branch; suites.sh transitions call it.
#
# Six cases:
#   1. positive control: gate is invoked for every submit
#   2. bead-less branch is certified in queue mode
#   3. red branch fails submission (no landstate written)
#   4. suites.sh quarantine submits its transition branch
#   5. suites.sh disable submits its transition branch
#   6. suites.sh activate submits its transition branch
#
# covers: spira/queue.sh spira/suites.sh spira/conf.sh
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
is()   { [ "$2" = "$3" ]        && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-submit
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up submit || { echo "test-submit: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# SH is INSIDE REPO so that HERE/.. resolves to REPO from within suites.sh.
REPO="$TMP/repo"
SH="$REPO/spira"
REMOTE="$TMP/remote.git"
RUN="$TMP/run"
REPONAME=test-submit-fixture
GATE_COUNT="$TMP/gate-count"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"

# Copy all harness scripts into REPO/spira (the fixture's spira directory).
mkdir -p "$SH" "$RUN/worktree"
cp "$HERE"/*.sh "$SH/"

# Three stub suites — one per transition test — so each gets a distinct branch name
# regardless of when within the same second the tests run.
_mksuite() { printf '#!/usr/bin/env bash\n# covers: spira/queue.sh\nset -uo pipefail\nprintf ok\n' \
    > "$SH/$1"; chmod +x "$SH/$1"; }
_mksuite test-q.sh   # quarantine test
_mksuite test-d.sh   # disable test
_mksuite test-a.sh   # activate test
: > "$SH/suite-state"

stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub confine.sh 'exit 0'
stub gh 'exit 1'

# THE GATE IS THE POSITIVE CONTROL. Each call appends so we can count invocations.
stub gate.sh '
printf "%s\n" "$1" >> "'"$GATE_COUNT"'"
printf "gate: VERDICT=PASS reason=stub branch=%s repo=%s\n" "$1" "${2:-?}" >&2
exit 0'

# QUEUE-MODE repo-map: pinned to a non-default mode.
cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

# Commit the fixture files so git operations inside suites.sh work.
git -C "$REPO" add -A
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

_env() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_CONF=/nonexistent \
        "$@"
}

submit() {
    _env bash "$SH/queue.sh" submit "$1" 2>&1
}

transition() {
    _env bash "$SH/suites.sh" "$@" 2>&1
}

landstate() { cat "$RUN/landstate/${1:-}" 2>/dev/null; }
queue_rec()  { cat "$RUN/queue/${1:-}" 2>/dev/null; }
gate_n()     { [ -f "$GATE_COUNT" ] && wc -l < "$GATE_COUNT" | tr -d ' ' || printf '0'; }

echo "test-submit.sh"

# ----------------------------------------------------------------
# POSITIVE CONTROL: gate is invoked on submit.
# Without this, assertions about "no landstate written" pass just
# as well if queue.sh never runs the gate at all.
# ----------------------------------------------------------------
rm -f "$GATE_COUNT"
git -C "$REPO" worktree add -q -b "spira/sub-green" "$RUN/worktree/sub-green" main
printf 'green\n' > "$RUN/worktree/sub-green/change.txt"
git -C "$RUN/worktree/sub-green" add -A
git -C "$RUN/worktree/sub-green" commit -q -m "sub-green: test sp-o3ami"

out="$(submit spira/sub-green)"
is "gate was invoked for submit" "1" "$(gate_n)"

# ----------------------------------------------------------------
# 1. Bead-less branch is certified in queue mode.
# The branch has no bead — it still becomes CERTIFIED.
# ----------------------------------------------------------------
want "certified message in output" "certified" "$out"
_id="${SPIRA_ID_PREFIX:-sp}-" 2>/dev/null || _id=""  # not needed; id is the branch basename
case "$(landstate sub-green)" in
    CERTIFIED*) ok "bead-less: landstate is CERTIFIED" ;;
    *)          bad "bead-less: landstate is CERTIFIED" "got: [$(landstate sub-green)]" ;;
esac
[ -f "$RUN/queue/sub-green" ] && ok "bead-less: queue record written" \
    || bad "bead-less: queue record written" "file missing: $RUN/queue/sub-green"

# ----------------------------------------------------------------
# 2. Red branch fails submission; no landstate written.
# ----------------------------------------------------------------
stub gate.sh '
printf "%s\n" "$1" >> "'"$GATE_COUNT"'"
printf "gate: VERDICT=FAIL reason=stub-fail branch=%s repo=%s\n" "$1" "${2:-?}" >&2
exit 1'

git -C "$REPO" worktree add -q -b "spira/sub-red" "$RUN/worktree/sub-red" main
printf 'red\n' > "$RUN/worktree/sub-red/change.txt"
git -C "$RUN/worktree/sub-red" add -A
git -C "$RUN/worktree/sub-red" commit -q -m "sub-red: test sp-o3ami"

rm -f "$GATE_COUNT"
out="$(submit spira/sub-red 2>&1 || true)"
want "red branch: failure reported" "failed the gate" "$out"
is   "red branch: no landstate" "" "$(landstate sub-red)"

# Restore passing gate.
stub gate.sh '
printf "%s\n" "$1" >> "'"$GATE_COUNT"'"
printf "gate: VERDICT=PASS reason=stub branch=%s repo=%s\n" "$1" "${2:-?}" >&2
exit 0'

# ----------------------------------------------------------------
# 3–5. suites.sh transitions call queue.sh submit.
#
# Need to check out back to main between transitions because
# _sts_transition leaves HEAD on the transition branch.
# ----------------------------------------------------------------

# 3. quarantine: creates branch, commits, certifies.
rm -f "$GATE_COUNT"
git -C "$REPO" checkout -q main 2>/dev/null || true
out="$(transition quarantine test-q.sh sp-xyz "flaky test")"
is "quarantine: gate was called" "1" "$(gate_n)"
_qbr="$(printf '%s' "$out" | tail -1 | tr -d '[:space:]')"
# Output is the branch name.
case "$_qbr" in
    spira/suite-state/*) ok "quarantine: branch name on stdout" ;;
    *) bad "quarantine: branch name on stdout" "got: [$_qbr]" ;;
esac
_qid="${_qbr#spira/}"
case "$(landstate "$_qid")" in
    CERTIFIED*) ok "quarantine: transition branch certified" ;;
    *)          bad "quarantine: transition branch certified" "got: [$(landstate "$_qid")]" ;;
esac
[ -f "$RUN/queue/$_qid" ] && ok "quarantine: queue record written" \
    || bad "quarantine: queue record written" "file missing: $RUN/queue/$_qid"

# 4. disable: same pattern.
rm -f "$GATE_COUNT"
git -C "$REPO" checkout -q main 2>/dev/null || true
out="$(transition disable test-d.sh "unsafe in CI")"
is "disable: gate was called" "1" "$(gate_n)"
_dbr="$(printf '%s' "$out" | tail -1 | tr -d '[:space:]')"
case "$_dbr" in
    spira/suite-state/*) ok "disable: branch name on stdout" ;;
    *) bad "disable: branch name on stdout" "got: [$_dbr]" ;;
esac
_did="${_dbr#spira/}"
case "$(landstate "$_did")" in
    CERTIFIED*) ok "disable: transition branch certified" ;;
    *)          bad "disable: transition branch certified" "got: [$(landstate "$_did")]" ;;
esac
[ -f "$RUN/queue/$_did" ] && ok "disable: queue record written" \
    || bad "disable: queue record written" "file missing: $RUN/queue/$_did"

# 5. activate: same pattern.
# Pre-populate a disabled entry so activate has something to clear and commit.
git -C "$REPO" checkout -q main 2>/dev/null || true
printf 'test-a.sh | disabled | 2026-01-01T00:00:00Z | | fixture\n' >> "$SH/suite-state"
git -C "$REPO" add "$SH/suite-state"
git -C "$REPO" commit -q --no-gpg-sign -m "fixture: disable test-a.sh for activate test"
rm -f "$GATE_COUNT"
out="$(transition activate test-a.sh)"
is "activate: gate was called" "1" "$(gate_n)"
_abr="$(printf '%s' "$out" | tail -1 | tr -d '[:space:]')"
case "$_abr" in
    spira/suite-state/*) ok "activate: branch name on stdout" ;;
    *) bad "activate: branch name on stdout" "got: [$_abr]" ;;
esac
_aid="${_abr#spira/}"
case "$(landstate "$_aid")" in
    CERTIFIED*) ok "activate: transition branch certified" ;;
    *)          bad "activate: transition branch certified" "got: [$(landstate "$_aid")]" ;;
esac
[ -f "$RUN/queue/$_aid" ] && ok "activate: queue record written" \
    || bad "activate: queue record written" "file missing: $RUN/queue/$_aid"

printf '\nASSERTIONS %s\n' "$((pass + fail))"
printf '%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
