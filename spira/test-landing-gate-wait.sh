#!/usr/bin/env bash
#
# test-landing-gate-wait.sh — gate_lock_wait is derived from the gate timeout, not the pass
# budget.
#
#   ./test-landing-gate-wait.sh
#
# THE CASE: landing.sh always sets SPIRA_GATE_LOCK_WAIT when calling gate.sh. The old
# formula (LAND_GATE_RESERVE / 10) produced a wait far below gate.sh's documented minimum
# of 2 * SPIRA_GATE_TIMEOUT, causing rc=75 lock-timeout against healthy holders. The fix
# derives the wait from SPIRA_GATE_TIMEOUT, honors an explicit operator setting, and caps
# at remaining pass time with a warning when it cannot reach the minimum.
#
# defect: sp-3iinc
# covers: spira/landing.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-landing-gate-wait
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up landing-gate-wait || { echo "test-landing-gate-wait: could not build fixture"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH"

cp "$HERE/landing.sh" "$HERE/landing-lib.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }

WAIT_LOG="$RUN/received-lock-wait"
# GATE STUB: records the SPIRA_GATE_LOCK_WAIT it receives, then passes unconditionally.
# This is how the test reads what landing.sh computed without running a real gate.
stub gate.sh "
printf '%s\n' \"\${SPIRA_GATE_LOCK_WAIT:-unset}\" > '$WAIT_LOG'
echo \"gate: VERDICT=PASS reason=stub branch=\$1 repo=\${2:-?}\" >&2
exit 0"
stub confine.sh 'exit 0'
stub skew.sh    'exit 0'

printf '%s | %s | push | |\n' "$(basename "$REPO")" "$REPO" > "$SH/repo-map"

seed() {
    testdb_reset
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
}
branch() {
    local id="$1"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$id" > "$RUN/worktree/$id/$id.txt"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$id" "$id" "$id" | testdb_seed
}
drop_branch() {
    git -C "$REPO" worktree remove --force "$RUN/worktree/$1" >/dev/null 2>&1
    git -C "$REPO" branch -D "spira/$1" >/dev/null 2>&1
}

run_landing() {
    rm -f "$RUN/landing.progress" "$WAIT_LOG"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO="$REPO" \
    SPIRA_REPO_MAP="$SH/repo-map" \
        bash "$SH/landing.sh" 2>&1
}

echo "test-landing-gate-wait.sh"

# --------------------------------------------------------------------------------------
# POSITIVE CONTROL. The gate stub must be called, or every silence below proves nothing.
# A land is reported only if gate.sh was called and returned PASS.
# --------------------------------------------------------------------------------------
seed; branch sp-gwctrl
out="$(run_landing)"
want "control: gate stub was called and the branch landed" "landed spira/sp-gwctrl" "$out"
drop_branch sp-gwctrl

# --------------------------------------------------------------------------------------
# DEFAULT: wait is 2 * SPIRA_GATE_TIMEOUT, not LAND_GATE_RESERVE / 10.
#
# With SPIRA_GATE_TIMEOUT=100 the correct value is 200. The old formula would have
# produced LAND_GATE_RESERVE / 10 = 120 (at the 1200 default) — these are different
# numbers, so the assertion distinguishes them without extra machinery.
# --------------------------------------------------------------------------------------
seed; branch sp-gwdefault
export SPIRA_LAND_MAXSEC=0 SPIRA_GATE_TIMEOUT=100
out="$(run_landing)"
unset SPIRA_LAND_MAXSEC SPIRA_GATE_TIMEOUT
want "default: branch lands"             "landed spira/sp-gwdefault" "$out"
is   "default: wait is 2*GATE_TIMEOUT"   "200" "$(cat "$WAIT_LOG" 2>/dev/null)"
drop_branch sp-gwdefault

# --------------------------------------------------------------------------------------
# EXPLICIT: an operator-set SPIRA_GATE_LOCK_WAIT is passed to gate.sh unchanged.
# landing.sh must not override a value the operator set for a reason.
# --------------------------------------------------------------------------------------
seed; branch sp-gwexplicit
export SPIRA_LAND_MAXSEC=0 SPIRA_GATE_TIMEOUT=100 SPIRA_GATE_LOCK_WAIT=999
out="$(run_landing)"
unset SPIRA_LAND_MAXSEC SPIRA_GATE_TIMEOUT SPIRA_GATE_LOCK_WAIT
want "explicit: branch lands"            "landed spira/sp-gwexplicit" "$out"
is   "explicit: operator value honored"  "999" "$(cat "$WAIT_LOG" 2>/dev/null)"
drop_branch sp-gwexplicit

# --------------------------------------------------------------------------------------
# CLAMPED: when the pass budget is shorter than 2 * SPIRA_GATE_TIMEOUT, the wait is
# capped and a warning names rc=75 as contention.
#
# SPIRA_LAND_MAXSEC=300, SPIRA_GATE_TIMEOUT=200 → ideal=400 > budget → clamp.
# SPIRA_LAND_GATE_RESERVE=60 so gate_fits() passes despite the short budget.
# --------------------------------------------------------------------------------------
seed; branch sp-gwclamped
export SPIRA_LAND_MAXSEC=300 SPIRA_GATE_TIMEOUT=200 SPIRA_LAND_GATE_RESERVE=60
out="$(run_landing)"
unset SPIRA_LAND_MAXSEC SPIRA_GATE_TIMEOUT SPIRA_LAND_GATE_RESERVE
want "clamped: branch lands"             "landed spira/sp-gwclamped" "$out"
want "clamped: warning names capped"     "capped" "$out"
want "clamped: warning names rc=75"      "rc=75" "$out"
received="$(cat "$WAIT_LOG" 2>/dev/null)"
[ "${received:-0}" -lt 400 ] \
    && ok  "clamped: received wait is below the ideal (400)" \
    || bad "clamped: expected wait < 400, got $received"
[ "${received:-0}" -gt 0 ] \
    && ok  "clamped: received wait is positive" \
    || bad "clamped: expected wait > 0, got $received"
drop_branch sp-gwclamped

# --------------------------------------------------------------------------------------
# BUDGET CUT: gate_fits itself, not gate_lock_wait. LAND_MAXSEC=1 with RESERVE=2 makes
# gate_fits return false before the first gate call (1-0=1 < 2) — deterministic without
# wall-clock timing. The pass must log the cut, defer the branch, certify nothing, and
# never reach gate.sh at all: WAIT_LOG only exists if the stub ran, so its absence is the
# proof.
# --------------------------------------------------------------------------------------
seed; branch sp-gwcut
export SPIRA_LAND_MAXSEC=1 SPIRA_LAND_GATE_RESERVE=2
out="$(run_landing)"
unset SPIRA_LAND_MAXSEC SPIRA_LAND_GATE_RESERVE
want   "budget cut: pass logs the cut"      "budget cut at spira/sp-gwcut" "$out"
want   "budget cut: reports deferred count" "1 branch(es) deferred"       "$out"
nowant "budget cut: branch not certified"   "certified"                   "$out"
[ ! -e "$WAIT_LOG" ] \
    && ok  "budget cut: gate.sh never invoked" \
    || bad "budget cut: gate.sh never invoked" "found $WAIT_LOG"
drop_branch sp-gwcut
tl_summary
