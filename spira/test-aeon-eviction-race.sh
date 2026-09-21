#!/usr/bin/env bash
#
# test-aeon-eviction-race.sh — aeon.sh reopens a bead closed while its landstate is
#   RED or EJECTED (the eviction-race shape).
#
# THE DEFECT THIS REPRODUCES (sp-htw4r). A batch eviction writes landstate=RED/EJECTED
# and calls bead_reopen. An aeon still in flight does not see the reopen — it closes the
# bead after the reopen (the close succeeds because the bead is now open). aeon.sh's exit
# checks run, read st=closed and committed=yes, all guards pass, and the aeon exits
# without reopening. The bead lands closed with a RED landstate: no queue mechanism
# retrieves it (queue_certified_list selects on CERTIFIED; bd list hides closed beads).
#
# THE RACE WINDOW: bead_reopen at 11:08:14Z, aeon re-closes at 11:12:31Z (+4m17s).
# Both bead and branch carry the correct commit; only the landstate says the work is stuck.
#
# POSITIVE CONTROLS (law-absence-needs-a-positive-control):
#   - closed+committed with NO landstate file → stays closed (proves the check does not
#     fire universally and would catch a version that always reopens)
#   - closed+committed with landstate=CERTIFIED → stays closed (proves the check reads
#     the state and only acts on RED/EJECTED)
#
# Driven through the REAL aeon.sh against a real bd on a throwaway fixture.
# Seen red without the fix: the eviction-race guard did not exist, so the bead stayed
# closed in all three cases. The first case (RED → reopened) is the fix; the others are
# guards against over-firing.
#
# defect: sp-htw4r
# covers: spira/aeon.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-eviction-race
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up aeonevictionrace || { echo "test-aeon-eviction-race: could not build fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
find "$HERE" -maxdepth 1 -name '*.sh' ! -name 'test-*.sh' -exec cp {} "$SPIRA_HOME/" \;
cp -r "$HERE/actors" "$SPIRA_HOME/" 2>/dev/null || true
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
bash -c ". \"$SPIRA_HOME/lib.sh\"" \
    || { printf 'test-aeon-eviction-race: fixture harness failed to source lib.sh\n' >&2; exit 1; }
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"
cat > "$SPIRA_HOME/chamber/builder.fayth" <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-aeon-eviction-race: aeon.sh has no SPIRA_AGENT injection point" >&2; exit 1; }

# The shim commits and closes, simulating the aeon session finishing its work. The
# eviction (landstate=RED) is written into $SPIRA_RUN/landstate/ before aeon.sh runs,
# simulating a batch eviction that raced with the session.
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
printf 'my work\n' >> f
git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":3}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

seed() {
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"%s","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "${2:-open}" "$_lbl" | testdb_seed
}
run_aeon() { rm -rf "$SPIRA_RUN/worktree"; "$SPIRA_HOME/aeon.sh" builder > "$TMP/out" 2>&1; }
field() { bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get(sys.argv[1]) or "")' "$2" 2>/dev/null; }
notes() { bd -C "$SPIRA_DB" show "$1" 2>/dev/null | tr '\n' ' '; }

mkdir -p "$SPIRA_RUN/landstate"

echo "test-aeon-eviction-race.sh"

# =============================================================================
echo
echo "closed+committed+landstate=RED — reopened (eviction race):"
# =============================================================================
# THE FIX (sp-htw4r). The batch eviction wrote RED and called bead_reopen; the aeon
# re-closed the bead while still in flight. aeon.sh must detect closed+RED and reopen.
testdb_reset; seed sp-er-1
printf 'RED faksha %s eviction-test\n' "$(date +%s)" > "$SPIRA_RUN/landstate/sp-er-1"
run_aeon
is   "bead is open after eviction-race detection"     open "$(field sp-er-1 status)"
is   "and the claim is released"                       ""   "$(field sp-er-1 assignee)"
want "aeon log shows eviction-race reopen"             "REOPENED — closed with landstate=RED" "$(cat "$TMP/out")"
want "and the reopen note names the cause"             "eviction-race" "$(notes sp-er-1)"
rm -f "$SPIRA_RUN/landstate/sp-er-1"

# =============================================================================
echo
echo "closed+committed+landstate=EJECTED — reopened (eviction race, automated shape):"
# =============================================================================
# EJECTED is the automated counterpart to RED: batch.sh sets it on local-gate failures.
# Both states mean the branch was evicted and must not stay closed.
testdb_reset; seed sp-er-2
printf 'EJECTED faksha %s batch-test\n' "$(date +%s)" > "$SPIRA_RUN/landstate/sp-er-2"
run_aeon
is   "bead is open after EJECTED detection"            open "$(field sp-er-2 status)"
want "aeon log shows eviction-race reopen"             "REOPENED — closed with landstate=EJECTED" "$(cat "$TMP/out")"
rm -f "$SPIRA_RUN/landstate/sp-er-2"

# =============================================================================
echo
echo "closed+committed, no landstate file — stays closed (positive control):"
# =============================================================================
# NO landstate file means the bead was never in a batch or was already cleaned up.
# The eviction-race guard must not fire — it would prevent legitimate closes.
testdb_reset; seed sp-er-3
rm -f "$SPIRA_RUN/landstate/sp-er-3"
run_aeon
is     "bead stays closed (no landstate)"              closed "$(field sp-er-3 status)"
nowant "no eviction-race reopen fired"                 "eviction-race" "$(cat "$TMP/out")"

# =============================================================================
echo
echo "closed+committed+landstate=CERTIFIED — stays closed (positive control):"
# =============================================================================
# CERTIFIED means the branch passed the gate and is queued. The aeon closed it with
# a commit — that is the expected outcome. The guard must not fire on CERTIFIED.
testdb_reset; seed sp-er-4
printf 'CERTIFIED faksha %s certified\n' "$(date +%s)" > "$SPIRA_RUN/landstate/sp-er-4"
run_aeon
is     "bead stays closed (landstate=CERTIFIED)"       closed "$(field sp-er-4 status)"
nowant "no eviction-race reopen fired"                 "eviction-race" "$(cat "$TMP/out")"
rm -f "$SPIRA_RUN/landstate/sp-er-4"

printf '\n%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
