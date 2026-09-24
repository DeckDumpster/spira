#!/usr/bin/env bash
# test-certify.sh — queue land mode: certify only after the gate passes.
#
# Cases: a branch is CERTIFIED after the gate passes; a branch whose gate fails is
# reopened; a branch whose rebase fails is reopened; moving the tip clears the
# record so the next pass re-certifies; a push-mode fixture in the same repo-map
# still pushes.
#
# The gate is a stub counter. Every silence below depends on the positive control
# showing the counter works: the push-mode fixture must increment it.
#
# confine.sh is a stub; the real db is testdb.sh with an embedded engine.
# The bare remote is real git so ancestry checks are real.
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
# QUEUE-MODE ENTRY: branch is CERTIFIED after the gate passes.
# -----------------------------------------------------------------------------------------
seed; branch sp-cert-green
before="$(main_tip)"
out="$(landing)"
is   "gate called at queue-mode entry"       "1"        "$(gate_n)"
want "certify is reported"                   "certified spira/sp-cert-green" "$out"
is   "remote main is unchanged"              "$before"  "$(main_tip)"
case "$(landstate sp-cert-green)" in
    CERTIFIED*) ok "landstate says CERTIFIED" ;;
    *)          bad "landstate says CERTIFIED" "got: $(landstate sp-cert-green)" ;;
esac
is   "bead stays closed after certify"       "closed"   "$(status_of sp-cert-green)"

# -----------------------------------------------------------------------------------------
# SECOND PASS ON THE SAME TIP: gate not called again; the certification stands.
# -----------------------------------------------------------------------------------------
out2="$(landing)"
is   "gate not called again on second pass"  "0"        "$(gate_n)"
nowant "no second certify message"           "certified spira/sp-cert-green" "$out2"

# -----------------------------------------------------------------------------------------
# GATE FAILURE AT CERTIFICATION: a branch whose gate fails is reopened, not certified.
# The gate stub is replaced with one that exits 1 (FAIL) for this branch, then restored.
# This is the positive control for sp-hm2vw: a fence violation caught at certification
# must reopen the bead and leave main unchanged.
# -----------------------------------------------------------------------------------------
seed; branch sp-cert-red
stub gate.sh '
printf "%s\n" "$1" >> "'"$GATE_COUNT"'"
if printf "%s" "$1" | grep -q "sp-cert-red"; then
    printf "gate: VERDICT=FAIL reason=inventory branch=%s repo=%s\n" "$1" "${2:-?}" >&2
    exit 1
fi
printf "gate: VERDICT=PASS reason=stub branch=%s repo=%s\n" "$1" "${2:-?}" >&2
exit 0'
before="$(main_tip)"
out="$(landing)"
is   "gate called for failing branch"        "1"        "$(gate_n)"
want "failing branch is reopened"            "reopened sp-cert-red" "$out"
is   "reopened bead is open"                 "open"     "$(status_of sp-cert-red)"
is   "remote main is unchanged"              "$before"  "$(main_tip)"
case "$(landstate sp-cert-red)" in
    RED*) ok "landstate says RED for gate failure" ;;
    *)    bad "landstate says RED for gate failure" "got: $(landstate sp-cert-red)" ;;
esac
# Restore the passing stub for subsequent tests.
stub gate.sh '
printf "%s\n" "$1" >> "'"$GATE_COUNT"'"
printf "gate: VERDICT=PASS reason=stub branch=%s repo=%s\n" "$1" "${2:-?}" >&2
exit 0'

# -----------------------------------------------------------------------------------------
# CONFLICT AT ENTRY: a branch that does not rebase onto base is reopened and not
# certified. This is the positive control: it proves landing.sh's rebase check is live.
# -----------------------------------------------------------------------------------------
seed; branch sp-cert-conflict
# Advance the base so sp-cert-conflict's file conflicts.
printf 'base-change\n' > "$REPO/sp-cert-conflict.txt"
git -C "$REPO" add -A
git -C "$REPO" commit -q -m "base: conflict with sp-cert-conflict"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
out="$(landing)"
want "conflicting branch is reopened"    "reopened sp-cert-conflict" "$out"
is   "reopened bead is open"             "open"   "$(status_of sp-cert-conflict)"
case "$(landstate sp-cert-conflict)" in
    RED*) ok "landstate says RED for conflict" ;;
    *)    bad "landstate says RED for conflict" "got: $(landstate sp-cert-conflict)" ;;
esac

# -----------------------------------------------------------------------------------------
# TIP MOVE: moving the branch tip after certification clears the record. The next pass
# must re-certify the new tip, calling the gate again.
# -----------------------------------------------------------------------------------------
seed; branch sp-cert-move
landing > /dev/null   # first pass: certify
# Move the tip.
printf 'v2\n' > "$RUN/worktree/sp-cert-move/sp-cert-move.txt"
git -C "$RUN/worktree/sp-cert-move" add -A
git -C "$RUN/worktree/sp-cert-move" commit -q -m "sp-cert-move sp-1fm88 — second commit"
new_tip="$(git -C "$REPO" rev-parse spira/sp-cert-move 2>/dev/null)"
out="$(landing)"   # second pass: re-certifies the new tip
is   "gate called after tip move"  "1"  "$(gate_n)"
want "second certify reported"     "certified spira/sp-cert-move" "$out"
case "$(landstate sp-cert-move)" in
    *"$new_tip"*) ok "landstate updated to new tip" ;;
    *)            bad "landstate updated to new tip" "expected [$new_tip] in [$(landstate sp-cert-move)]" ;;
esac

# -----------------------------------------------------------------------------------------
# PUSH MODE STILL PUSHES: a push-mode repo in the same repo-map lands normally.
# Queue-mode changes must not break the push path.
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

# -----------------------------------------------------------------------------------------
# PARALLEL CERTIFY: with SPIRA_CERTIFY_PAR=2 and a gate that sleeps 3s, two branches are
# certified in ~3s (parallel), not ~6s (serial). Both branches must reach CERTIFIED.
# -----------------------------------------------------------------------------------------
write_map  # restore queue-only map
GATE_SLEEP=3
stub gate.sh '
printf "%s %s\n" "$(date +%s)" "$1" >> "'"$GATE_COUNT"'"
sleep '"$GATE_SLEEP"'
printf "gate: VERDICT=PASS reason=stub branch=%s repo=%s\n" "$1" "${2:-?}" >&2
exit 0'

stub queue.sh 'exit 0'
rm -f "$GATE_COUNT"
seed; branch sp-par-a; branch sp-par-b
SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_CERTIFY_PAR=2 \
        bash "$SH/landing.sh" > /dev/null 2>&1
is "gate called for both branches with par=2" "2" "$(gate_n)"
_t0=$(awk 'NR==1{print $1}' "$GATE_COUNT" 2>/dev/null || echo 0)
_t1=$(awk 'END{print $1}' "$GATE_COUNT" 2>/dev/null || echo 0)
_spread=$(( _t1 - _t0 ))
[ "$_spread" -le 1 ] \
    && ok "both branches certified in parallel (start times ${_spread}s apart)" \
    || bad "both branches certified in parallel" "gates started ${_spread}s apart — serial"
case "$(landstate sp-par-a)" in CERTIFIED*) ok "sp-par-a certified" ;; *) bad "sp-par-a certified" "$(landstate sp-par-a)" ;; esac
case "$(landstate sp-par-b)" in CERTIFIED*) ok "sp-par-b certified" ;; *) bad "sp-par-b certified" "$(landstate sp-par-b)" ;; esac

# Restore the fast passing stub used by any future cases.
stub gate.sh '
printf "%s\n" "$1" >> "'"$GATE_COUNT"'"
printf "gate: VERDICT=PASS reason=stub branch=%s repo=%s\n" "$1" "${2:-?}" >&2
exit 0'

# -----------------------------------------------------------------------------------------
# COMPLETION-ORDER: a gate dispatched second is certified as soon as it finishes, without
# waiting for gates dispatched before it.
#
# Gate stub: branches containing "-slow" sleep 3s; "-fast" exits immediately.
# sp-co-slow has an earlier closed_at, so it sorts first and is dispatched first.
#
# par=2: both gates start together; sp-co-fast finishes first and must be certified
#        before sp-co-slow (which is still sleeping).
# par=1: serial dispatch — sp-co-slow runs to completion before sp-co-fast starts,
#        so sp-co-slow certifies first. This is the positive control proving the
#        par=2 reversal is real, not an artefact of output order.
# -----------------------------------------------------------------------------------------
stub gate.sh '
printf "%s %s\n" "$(date +%s)" "$1" >> "'"$GATE_COUNT"'"
if printf "%s" "$1" | grep -q "\-slow"; then sleep 3; fi
printf "gate: VERDICT=PASS reason=stub branch=%s repo=%s\n" "$1" "${2:-?}" >&2
exit 0'
stub queue.sh 'exit 0'

branch_with_date() {
    local id="$1" cat="$2"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$id" > "$RUN/worktree/$id/$id.txt"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id — work"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"%s","closed_at":"%s","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$id" "$id" "$cat" "$cat" "$id" | testdb_seed
}
drop_branch() {
    git -C "$REPO" worktree remove --force "$RUN/worktree/$1" >/dev/null 2>&1 || true
    git -C "$REPO" branch -D "spira/$1" >/dev/null 2>&1 || true
}

# par=2: fast gate (dispatched second) is certified before slow gate (dispatched first)
write_map
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-01T00:00:00Z"}
JSONL
rm -f "$GATE_COUNT"
branch_with_date sp-co-slow "2026-09-01T00:00:00Z"
branch_with_date sp-co-fast "2026-09-02T00:00:00Z"
co_out="$(SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_CERTIFY_PAR=2 bash "$SH/landing.sh" 2>&1)"
want "par=2: sp-co-fast certified"                      "certified spira/sp-co-fast" "$co_out"
want "par=2: sp-co-slow certified"                      "certified spira/sp-co-slow" "$co_out"
before "par=2: fast certified before slow" \
    "certified spira/sp-co-fast" "certified spira/sp-co-slow" "$co_out"

# par=1 positive control: slow dispatched first, certifies first
drop_branch sp-co-slow; drop_branch sp-co-fast
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-01T00:00:00Z"}
JSONL
rm -f "$GATE_COUNT"
branch_with_date sp-co-slow "2026-09-01T00:00:00Z"
branch_with_date sp-co-fast "2026-09-02T00:00:00Z"
co_out="$(SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_CERTIFY_PAR=1 bash "$SH/landing.sh" 2>&1)"
want "par=1: sp-co-slow certified"                      "certified spira/sp-co-slow" "$co_out"
want "par=1: sp-co-fast certified"                      "certified spira/sp-co-fast" "$co_out"
before "par=1: slow certified before fast (serial)" \
    "certified spira/sp-co-slow" "certified spira/sp-co-fast" "$co_out"

# -----------------------------------------------------------------------------------------
# IDLE-SKIP: when CI is idle (forge returns 0 for runs-active) and the cert queue is
# empty, landing.sh certifies without calling gate.sh.
#
# TWO CASES:
#   idle: forge returns 0 — gate must NOT be called; branch must be CERTIFIED.
#   busy: forge returns 1 — gate IS called normally (positive control).
#
# SEEN RED WITHOUT THE FIX: removing the SPIRA_CERT_IDLE_SKIP block causes the gate
# to be called even when CI is idle, so the "gate not called" assertion below fails.
# -----------------------------------------------------------------------------------------

# Restore normal (passing) gate and a counting queue stub.
stub gate.sh '
printf "%s\n" "$1" >> "'"$GATE_COUNT"'"
printf "gate: VERDICT=PASS reason=stub branch=%s repo=%s\n" "$1" "${2:-?}" >&2
exit 0'
stub queue.sh 'exit 0'

# idle case: forge reports 0 active runs → gate bypassed, branch certified directly.
stub forge.sh 'case "${1:-}" in runs-active) echo 0 ;; *) exit 0 ;; esac'

# Clean landstate from previous test sections so queue_certified_list returns 0,
# simulating an empty cert queue (the precondition for the idle-skip).
rm -f "$RUN/landstate/"*

write_map
seed
# AN EMPTY CERT QUEUE IS THE CASE'S PRECONDITION, so make it one. The par=1 case above leaves
# sp-co-slow and sp-co-fast CERTIFIED; queue_certified_list counted them, the skip correctly
# declined, and this case failed on every run once #283 put it on main (main gates
# 35940444737, 35940777548). Clear the landstate and assert the precondition.
rm -f "$RUN"/landstate/* "$RUN"/submitted/* 2>/dev/null || true
is "idle precondition: no CERTIFIED landstate left over" "0" \
    "$(grep -l '^CERTIFIED' "$RUN"/landstate/* 2>/dev/null | wc -l | tr -d ' ')"
branch sp-idle-skip
rm -f "$GATE_COUNT"
idle_out="$(SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_FORGE="$SH/forge.sh" \
        bash "$SH/landing.sh" 2>&1)"
is   "idle: gate not called"    "0"        "$(gate_n)"
want "idle: certify reported"   "certified spira/sp-idle-skip" "$idle_out"
want "idle: says sole batch"    "sole batch member" "$idle_out"
case "$(landstate sp-idle-skip)" in
    CERTIFIED*) ok "idle: landstate says CERTIFIED" ;;
    *)          bad "idle: landstate says CERTIFIED" "got: $(landstate sp-idle-skip)" ;;
esac

# busy case: forge reports 1 active run → gate IS called (positive control).
stub forge.sh 'case "${1:-}" in runs-active) echo 1 ;; *) exit 0 ;; esac'

# Clean previous landstate so the branch is re-evaluated.
rm -f "$RUN/landstate/sp-idle-skip" "$RUN/submitted/sp-idle-skip" 2>/dev/null || true
seed; branch sp-busy-gate
rm -f "$GATE_COUNT"
busy_out="$(SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_FORGE="$SH/forge.sh" \
        bash "$SH/landing.sh" 2>&1)"
is   "busy: gate called"        "1"        "$(gate_n)"
want "busy: certify reported"   "certified spira/sp-busy-gate" "$busy_out"

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
