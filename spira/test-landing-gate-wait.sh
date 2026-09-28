#!/usr/bin/env bash
#
# test-landing-gate-wait.sh — gate_lock_wait is a short, bounded default that does not
# head-of-line block the rest of a pass behind one contended gate lock.
#
#   ./test-landing-gate-wait.sh
#
# THE CASE: landing.sh always sets SPIRA_GATE_LOCK_WAIT when calling gate.sh, and the
# default is short (120s) and does NOT scale with SPIRA_GATE_TIMEOUT. The lock's usual
# contender is the branch's own aeon, mid-gate on the same tree, and a wait long enough to
# outlast a healthy holder (once 2 * SPIRA_GATE_TIMEOUT) head-of-line blocked every other
# candidate behind it in the same pass — 44 of 60 minutes lost to two such waits on one
# host (sp-u7wrz). A short wait that gives up costs little: the gate returns NO_VERDICT,
# not a branch fault, and the holder's own verdict is cached and reused next pass. An
# explicit operator setting is still honored unchanged, and a tight pass budget can still
# cap the wait further.
#
# defect: sp-3iinc sp-u7wrz
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
HELD_BRANCH_FILE="$RUN/held-branch"
# GATE STUB: appends the SPIRA_GATE_LOCK_WAIT it receives, then passes — unless the branch
# named in HELD_BRANCH_FILE matches, in which case it returns NO_VERDICT/lock-timeout, the
# same shape gate.sh's own flock -w timeout returns (spira/gate.sh, verified against a real
# lock in test-gate-tree.sh). This is how the test reads what landing.sh computed, and how
# it simulates another process already holding a branch's gate lock, without paying for a
# real gate trial.
stub gate.sh "
printf '%s\n' \"\${SPIRA_GATE_LOCK_WAIT:-unset}\" >> '$WAIT_LOG'
if [ -f '$HELD_BRANCH_FILE' ] && [ \"\$(cat '$HELD_BRANCH_FILE')\" = \"\$1\" ]; then
    echo \"gate: VERDICT=NO_VERDICT reason=lock-timeout branch=\$1 repo=\${2:-?} suite=-\" >&2
    exit 75
fi
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
# DEFAULT: wait is a short, bounded 120s, and does NOT scale with SPIRA_GATE_TIMEOUT.
#
# SPIRA_GATE_TIMEOUT=2700 (the production default) would have produced 5400 under the old
# 2*timeout formula — the assertion distinguishes the two without extra machinery.
# --------------------------------------------------------------------------------------
seed; branch sp-gwdefault
export SPIRA_LAND_MAXSEC=0
out="$(run_landing)"
unset SPIRA_LAND_MAXSEC
want "default: branch lands"        "landed spira/sp-gwdefault" "$out"
is   "default: wait is bounded (120), not 2*GATE_TIMEOUT" "120" "$(cat "$WAIT_LOG" 2>/dev/null)"
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
# CLAMPED: when the pass budget is shorter than the 120s ideal, the wait is capped and a
# warning names rc=75 as contention.
#
# SPIRA_LAND_MAXSEC=90 → remaining ~90 < ideal (120) → clamp.
# SPIRA_LAND_GATE_RESERVE=10 so gate_fits() passes despite the short budget.
# --------------------------------------------------------------------------------------
seed; branch sp-gwclamped
export SPIRA_LAND_MAXSEC=90 SPIRA_LAND_GATE_RESERVE=10
out="$(run_landing)"
unset SPIRA_LAND_MAXSEC SPIRA_LAND_GATE_RESERVE
want "clamped: branch lands"             "landed spira/sp-gwclamped" "$out"
want "clamped: warning names capped"     "capped" "$out"
want "clamped: warning names rc=75"      "rc=75" "$out"
received="$(cat "$WAIT_LOG" 2>/dev/null)"
[ "${received:-0}" -lt 120 ] \
    && ok  "clamped: received wait is below the ideal (120)" \
    || bad "clamped: expected wait < 120, got $received"
[ "${received:-0}" -gt 0 ] \
    && ok  "clamped: received wait is positive" \
    || bad "clamped: expected wait > 0, got $received"
drop_branch sp-gwclamped

# --------------------------------------------------------------------------------------
# HELD LOCK, TWO CANDIDATES: a branch whose gate lock is held by another process (the
# HELD_BRANCH_FILE stub above, standing in for gate.sh's own real flock -w timeout) must
# not head-of-line block the other candidate in the same pass — that candidate lands
# without waiting behind it, and the held one is deferred (NO_VERDICT), not reopened.
#
# THE DISCRIMINATING FACT: the wait recorded for the held branch is bounded (<=120), which
# fails on today's landing.sh (SPIRA_GATE_TIMEOUT defaults to 2700, so the old formula
# would send 5400 — a wait long enough to actually hold the pass hostage against a real
# lock, rather than a stub that gives up instantly).
# --------------------------------------------------------------------------------------
seed; branch sp-gwheld; branch sp-gwfree
printf 'spira/sp-gwheld\n' > "$HELD_BRANCH_FILE"
export SPIRA_LAND_MAXSEC=0
out="$(run_landing)"
unset SPIRA_LAND_MAXSEC
rm -f "$HELD_BRANCH_FILE"
want   "held lock: the free candidate lands in the same pass" "landed spira/sp-gwfree" "$out"
nowant "held lock: the held candidate does not land"          "landed spira/sp-gwheld" "$out"
nowant "held lock: the held candidate is not reopened"        "reopened sp-gwheld"     "$out"
want   "held lock: the held candidate is logged as NO_VERDICT" "gate NO_VERDICT on spira/sp-gwheld" "$out"
maxwait="$(sort -n "$WAIT_LOG" 2>/dev/null | tail -1)"
[ "${maxwait:-99999}" -le 120 ] \
    && ok  "held lock: the wait passed to gate.sh is bounded (<=120), not 2*GATE_TIMEOUT (5400)" \
    || bad "held lock: wait passed to gate.sh is bounded (<=120)" "got $maxwait"
is "held lock: gate.sh was called for both candidates" "2" "$(grep -c . "$WAIT_LOG" 2>/dev/null)"
drop_branch sp-gwheld; drop_branch sp-gwfree

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
# --------------------------------------------------------------------------------------
# SPIRA_GATE_LOCK_WAIT and SPIRA_LAND_CPU_QUOTA are in conf.sh's allowlist, so an operator
# can set either from spira.toml without a code override.
# --------------------------------------------------------------------------------------
echo
echo "conf keys: SPIRA_GATE_LOCK_WAIT and SPIRA_LAND_CPU_QUOTA are settable"
_conf_keys="$(SPIRA_HOME="$HERE" SPIRA_CONF=/nonexistent bash -c ". '$HERE/conf.sh'; printf '%s' \"\$SPIRA_CONF_KEYS\"")"
want "SPIRA_GATE_LOCK_WAIT is in SPIRA_CONF_KEYS" "SPIRA_GATE_LOCK_WAIT" "$_conf_keys"
want "SPIRA_LAND_CPU_QUOTA is in SPIRA_CONF_KEYS" "SPIRA_LAND_CPU_QUOTA" "$_conf_keys"
nowant "positive control: a made-up key is absent" "SPIRA_GATE_LOCK_WAIT_NONEXISTENT" "$_conf_keys"

tl_summary
