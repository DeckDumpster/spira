#!/usr/bin/env bash
#
# test-landing-red-recurring.sh — a branch that goes RED twice with the same reason class
# escalates on the second RED instead of reopening, even when the tip and base both changed.
#
# WHY THIS EXISTS. sp-m6jhx was reopened four times because the duplicate-RED guard keys on
# (tip, base) identity, which the loop changes on every cycle: the aeon commits new work
# (tip moves) and main lands other beads (base moves), so every RED looks new to the guard.
# The fix keys on the reason CLASS instead — "no-rebase" from "no-rebase@<sha>" — which
# survives any sha change.
#
# THREE THINGS VERIFIED.
#
# 1. POSITIVE CONTROL: first RED reopens — the single allowed reopen.
#
# 2. RECURRING RED: second RED with different tip AND base escalates instead of reopening.
#    This is the regression the bead addresses; it must be seen to fail before the fix.
#
# 3. IDEMPOTENT GUARD: an identical re-mark (same tip AND base) is still silently skipped.
#    The coarser rule must not have swallowed the finer one.
#
# defect: sp-4rjxl
# covers: spira/landing.sh spira/lib.sh
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-landing-red-recurring
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up landing-red-recurring || {
    printf 'SKIP test-landing-red-recurring: testdb not available\n' >&2
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

cat > "$SH/repo-map" <<MAP
$REPONAME | $REPO | push | |
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
        bash "$SH/landing.sh" 2>&1
}

advance_base() {
    printf '%s\n' "base step $1" > "$REPO/base-step.txt"
    git -C "$REPO" add base-step.txt
    git -C "$REPO" commit -q -m "base step $1"
    git -C "$REPO" push -q origin main
    git -C "$REPO" fetch -q origin
}

seed() {
    testdb_reset
    rm -rf "$RUN/tip-at-gate" "$RUN/landstate"
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
}

branch() {
    local id="$1" f="${2:-$1.txt}" c="${3:-$1}"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$c" > "$RUN/worktree/$id/$f"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id — work"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$id" "$id" "$id" | testdb_seed
}

drop_branch() {
    git -C "$REPO" worktree remove --force "$RUN/worktree/$1" >/dev/null 2>&1
    git -C "$REPO" branch -D "spira/$1" >/dev/null 2>&1
}

echo "test-landing-red-recurring.sh"

# --------------------------------------------------------------------------------------
# SET UP: sp-recur writes to shared.txt; base also writes it, causing a conflict.
# --------------------------------------------------------------------------------------
seed
branch sp-recur shared.txt "from the branch"
# The base conflict that makes this branch permanently unable to rebase.
printf '%s\n' "base owns shared.txt" > "$REPO/shared.txt"
git -C "$REPO" add -A
git -C "$REPO" commit -q -m "base: take shared.txt"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

# --------------------------------------------------------------------------------------
# PART 1: POSITIVE CONTROL — first RED reopens.
# --------------------------------------------------------------------------------------
echo
echo "first RED reopens — positive control:"
out="$(landing)"
want "first RED reopens the bead"        "reopened sp-recur" "$out"
is   "bead is open after first RED"      open "$(status_of sp-recur)"

# --------------------------------------------------------------------------------------
# PART 2: RECURRING RED — tip AND base both changed; second RED must escalate.
#
# Close the bead (simulating aeon work), advance the tip (aeon committed something),
# and advance the base (another bead landed on main). Both tip and base now differ from
# the previous RED mark. This is the loop the fix must break.
# --------------------------------------------------------------------------------------
echo
echo "second RED with changed tip+base escalates, not reopens:"

B close sp-recur --reason "aeon tried" >/dev/null 2>&1
# Advance tip: aeon committed new work.
git -C "$RUN/worktree/sp-recur" commit -q --allow-empty -m "sp-recur: aeon attempt 1"
# Advance base: another bead landed; shared.txt is no longer in conflict (doesn't matter —
# the branch still conflicts because its own version of shared.txt disagrees with the base).
advance_base 1

out="$(landing)"
want "second RED escalates"              "escalated sp-recur" "$out"
nowant "second RED does not reopen"      "reopened sp-recur"  "$out"
is   "bead stays closed on second RED"   closed "$(status_of sp-recur)"
want "escalation reaches the operator"   "red recurring" "$(cat "$EMITTED")"

# --------------------------------------------------------------------------------------
# PART 3: IDEMPOTENT GUARD — identical re-mark (tip+base unchanged) is still skipped.
#
# The reason-class guard fires first now. To reach the idempotent guard, the reason class
# must not match, which is impossible for a single bead with the same cause. So we verify
# the idempotent guard indirectly: mark RED manually, then run landing without moving tip
# or base. The pass must not escalate or reopen — it must be silent.
# --------------------------------------------------------------------------------------
echo
echo "idempotent re-mark is skipped silently:"

seed
branch sp-idem shared.txt "from idem"
# Set up the conflict again.
printf '%s\n' "base owns shared.txt again" > "$REPO/shared.txt"
git -C "$REPO" add -A
git -C "$REPO" commit -q -m "base: shared.txt conflict for idem"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

# First pass: RED, bead reopens.
out="$(landing)"
want "first RED reopens sp-idem" "reopened sp-idem" "$out"
B close sp-idem --reason "aeon did nothing useful" >/dev/null 2>&1

# Run landing AGAIN with the SAME tip and base (nothing changed).
# The idempotent guard must fire: no escalation, no reopen.
: > "$EMITTED"
out2="$(landing)"
nowant "identical re-mark does not escalate" "escalated sp-idem" "$out2"
nowant "identical re-mark does not reopen"   "reopened sp-idem"  "$out2"
is   "bead stays closed on identical re-mark" closed "$(status_of sp-idem)"

drop_branch sp-recur 2>/dev/null || true
drop_branch sp-idem  2>/dev/null || true
tl_summary
