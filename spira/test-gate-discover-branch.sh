#!/usr/bin/env bash
#
# test-gate-discover-branch.sh — gate-check calls discover with the gate's own branch,
# not the repository's current branch.
#
#   ./test-gate-discover-branch.sh
#
# THE DEFECT THIS SUITE EXISTS FOR.
#
# gate-check called `bd gate discover` with no --branch, so it defaulted to the
# repository's current branch (the base branch, usually main). A deployment run on
# main started seconds after two PRs merged; its time proximity to the open gates
# made bd gate discover bind all four gates to it. A gate created for a PR must only
# be satisfiable by a run on that PR's branch — a deployment run on the base branch
# cannot stand in.
#
# The fix: each gate stores metadata.branch at creation. gate-check collects unique
# branches from open unbound gates and calls `discover --branch <branch>` once per
# branch instead of once per repo.
#
# THREE THINGS VERIFIED.
#
# 1. POSITIVE CONTROL: a gate with metadata.branch set → gate-check calls
#    `discover --branch <branch>`. A discover call that ignores metadata.branch
#    would fail this assertion.
#
# 2. NO DEFAULT-BRANCH DISCOVER: gate-check never calls `discover` without --branch.
#    The old behaviour called `discover` (no flag), which is the bug. A regression
#    would surface here.
#
# 3. NO DISCOVER WHEN NO BRANCHES: when no open unbound gates have metadata.branch,
#    gate-check emits no discover call. This prevents a fallback to the default-branch
#    query that is the root of the bug.
#
# A STUB bd IS CORRECT HERE. What is being tested is gate-check.sh's argument-passing
# logic — specifically that it reads metadata.branch and passes it as --branch. That
# is a text-processing question about gate-check.sh, not a database-state question,
# so a stub that emits controlled JSON is the right dependency
# (law-prefer-the-real-dependency: use a real dependency for database-state questions;
# a stub for output-parsing questions).
#
# covers: spira/gate-check.sh spira/aeon.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

SH="$TMP/spira"; mkdir -p "$SH"
cp "$HERE/gate-check.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$SH/"

BD_LOG="$TMP/bd-calls.log"
export BD_LOG
mkdir -p "$TMP/sbin" "$TMP/run/events"

PR_REPO="$TMP/pr-repo"; mkdir -p "$PR_REPO"
git init -q -b main "$PR_REPO" 2>/dev/null
git -C "$PR_REPO" commit -q --allow-empty -m init 2>/dev/null

MAP="$TMP/repo-map"
printf 'prerepo | %s | pr | origin/main | |\n' "$PR_REPO" > "$MAP"

run_gate_check() {
    SPIRA_HOME="$SH" SPIRA_REPO="$PR_REPO" SPIRA_RUN="$TMP/run" SPIRA_DB="$TMP/fake-db" \
    SPIRA_REPO_MAP="$MAP" SPIRA_CONF="$TMP/no.conf" SPIRA_BD="$TMP/sbin/bd" \
        bash "$SH/gate-check.sh" 2>/dev/null
}

# ======================================================================================
# PART 1: POSITIVE CONTROL — gate with metadata.branch causes discover --branch call.
#
# The stub returns one open unbound gh:run gate with metadata.branch=spira/pr-branch.
# gate-check must call `discover --branch spira/pr-branch`, not `discover` alone.
# A discover call with the wrong branch or no branch would fail this assertion.
# ======================================================================================
echo "discover is called with the gate's branch:"

cat > "$TMP/sbin/bd" <<'BDSTUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$BD_LOG"
case "$*" in
    *"gate list"*"--json"*|*"gate list --json"*)
        printf '[{"id":"sp-g1","await_type":"gh:run","metadata":{"branch":"spira/pr-branch","repo":"org/repo"}}]\n' ;;
esac
BDSTUB
chmod +x "$TMP/sbin/bd"

: > "$BD_LOG"
run_gate_check

bd_calls="$(cat "$BD_LOG" 2>/dev/null)"
want   "discover called with correct branch"  "gate discover --branch spira/pr-branch" "$bd_calls"
want   "gate check is still called"           "gate check --type=gh:run"               "$bd_calls"

# ======================================================================================
# PART 2: NO DEFAULT-BRANCH DISCOVER — discover is never called without --branch.
#
# The old code called `gate discover` (no --branch), which queried the repository's
# current branch. The new code must never do that — the only discover calls allowed
# are those with an explicit --branch matching a gate's stored branch.
# ======================================================================================
echo
echo "discover is never called without --branch:"

# Same stub as above: the call log from PART 1 is still valid for this check.
nowant "no bare gate discover call" "gate discover" "$(grep 'gate discover$' "$BD_LOG" 2>/dev/null || true)"

# ======================================================================================
# PART 3: NO DISCOVER WHEN NO BRANCHES — when no unbound gates have metadata.branch,
# gate-check emits no discover call.
#
# THE POSITIVE CONTROL: first assert that discover WAS called in PART 1 (proving this
# stub and mechanism can produce a discover call). Then show that with an empty list
# it is not called. Silently passing with zero calls would also pass a broken suite
# that never called discover in PART 1 either.
# ======================================================================================
echo
echo "no discover call when no unbound gates have a branch:"

want "positive control: part 1 DID call discover" \
    "gate discover" "$(cat "$BD_LOG" 2>/dev/null)"

cat > "$TMP/sbin/bd" <<'BDSTUB2'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$BD_LOG"
case "$*" in
    *"gate list"*"--json"*|*"gate list --json"*)
        # A bound gate (has await_id) and a gate with no branch metadata — neither
        # should trigger a discover call.
        printf '[{"id":"sp-g2","await_type":"gh:run","await_id":"12345","metadata":{"repo":"org/repo"}},{"id":"sp-g3","await_type":"gh:run","metadata":{"repo":"org/repo"}}]\n' ;;
esac
BDSTUB2
chmod +x "$TMP/sbin/bd"

: > "$BD_LOG"
run_gate_check

bd_calls2="$(cat "$BD_LOG" 2>/dev/null)"
nowant "no discover call when no branches" "gate discover" "$bd_calls2"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
