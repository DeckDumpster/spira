#!/usr/bin/env bash
#
# test-aeon-eviction-race.sh — aeon.sh reopens a bead closed while its landstate carries a
#   batch-eviction record, and does NOT reopen for a gate-red record or a stale tip.
#
# THE ORIGINAL DEFECT (sp-htw4r). A batch eviction writes landstate=RED/EJECTED and calls
# bead_reopen. An aeon still in flight does not see the reopen — it closes the bead after
# the reopen (the close succeeds because the bead is now open). aeon.sh's exit checks run,
# read st=closed and committed=yes, all guards pass, and the aeon exits without reopening.
# The bead lands closed with a RED landstate: no queue mechanism retrieves it.
#
# THE REGRESSION (sp-ygvu0). The guard fires on RED regardless of reason. When the landing
# pass writes RED reason=gate after a gate failure, fixes the code, and closes — the guard
# reopens it as an eviction. The fix: skip RED reason=gate (normal path); skip when the
# record tip is older than the current branch tip (session pushed past the eviction).
#
# FIXTURE CASES (sp-ygvu0):
#   (a) RED reason=gate   at current tip   → stays closed (gate-red, not eviction)
#   (b) RED reason=ejected at stale tip    → stays closed (session pushed past eviction)
#   (c) RED reason=ejected at current tip  → reopened     (eviction, tip matches)
#   EJECTED at current tip                 → reopened     (EJECTED is always batch eviction)
#
# THE UNBOUNDED LOOP (sp-r1501). The reopen at (c) charges no attempt (by design — the
# harness's own requeue must not poison finished work) but had no cap and no idempotence
# check either: a bead whose landstate record never gets recertified reopens on every pass,
# forever, each cycle spending a lane. Two further brakes:
#   - idempotence: the landstate's own tip+reason is compared to a sidecar
#     ($LANDSTATE/<id>.evict-seen) written at the last reopen/escalation. Unchanged means
#     this exact record was already acted on — skip, mirroring landing.sh CHECK6.
#   - cap: at SPIRA_EVICTION_ESCALATE_AT prior eviction-race requeues, label the bead
#     needs-operator instead of reopening again.
#
# POSITIVE CONTROLS (law-absence-needs-a-positive-control):
#   - closed+committed with NO landstate file → stays closed
#   - closed+committed with landstate=CERTIFIED → stays closed
#
# The shim writes the landstate after committing (so the recorded tip matches the real
# branch tip) when a per-bead control file exists. For the stale-tip case the landstate is
# written before the aeon runs, with a dummy tip, and the shim's commit makes it stale. The
# idempotence case also writes the landstate before the aeon runs, with a dummy tip that
# matches a pre-seeded sidecar exactly — the idempotence check compares those two records to
# each other, not to the real branch tip, so it fires before the stale-tip check ever runs.
#
# testdb-mode: server — the cap case seeds requeued events via bd sql, refused in embedded mode
#
# defect: sp-htw4r sp-ygvu0 sp-r1501
# covers: spira/aeon.sh spira/lib.sh spira/conf.sh
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
export SPIRA_TESTDB_MODE=server
testdb_up aeonevictionrace || {
    printf 'SKIP test-aeon-eviction-race: server testdb not available\n' >&2
    exit 77
}
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

# The shim commits and closes. If a per-bead control file exists under $TMP/evict-ctrl/<id>,
# it writes the landstate AFTER committing (so the recorded tip is the real branch tip).
# Modes: gate → RED reason=gate; ejected-cur → RED reason=ejected; ejected-stat → EJECTED.
# For the stale-tip case the landstate is written outside the shim (before run_aeon), and
# no control file is set so the shim leaves it alone.
mkdir -p "$TMP/evict-ctrl"
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
printf 'my work\n' >> f
git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
tip="$(git rev-parse HEAD)"
ctrl="$TMP/evict-ctrl/$id"
if [ -f "$ctrl" ]; then
    mode="$(cat "$ctrl")"
    epoch="$(date +%s)"
    case "$mode" in
        gate)         printf 'RED %s %s gate\n'    "$tip" "$epoch" > "$SPIRA_RUN/landstate/$id" ;;
        ejected-cur)  printf 'RED %s %s ejected\n' "$tip" "$epoch" > "$SPIRA_RUN/landstate/$id" ;;
        ejected-stat) printf 'EJECTED %s %s\n'     "$tip" "$epoch" > "$SPIRA_RUN/landstate/$id" ;;
    esac
fi
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
echo "(c) closed+committed+landstate=RED reason=ejected at current tip — reopened:"
# =============================================================================
# The shim writes RED reason=ejected with the real branch tip. The guard must reopen.
testdb_reset; seed sp-er-1
echo "ejected-cur" > "$TMP/evict-ctrl/sp-er-1"
run_aeon
is   "bead is open after eviction-race detection"     open "$(field sp-er-1 status)"
is   "and the claim is released"                       ""   "$(field sp-er-1 assignee)"
want "aeon log shows eviction-race reopen"             "REOPENED — closed with landstate=RED" "$(cat "$TMP/out")"
want "and the reopen note names the cause"             "eviction-race" "$(notes sp-er-1)"
rm -f "$TMP/evict-ctrl/sp-er-1" "$SPIRA_RUN/landstate/sp-er-1"

# =============================================================================
echo
echo "closed+committed+landstate=EJECTED at current tip — reopened:"
# =============================================================================
# EJECTED is written by batch.sh and is always a batch eviction; no reason check needed.
testdb_reset; seed sp-er-2
echo "ejected-stat" > "$TMP/evict-ctrl/sp-er-2"
run_aeon
is   "bead is open after EJECTED detection"            open "$(field sp-er-2 status)"
want "aeon log shows eviction-race reopen"             "REOPENED — closed with landstate=EJECTED" "$(cat "$TMP/out")"
rm -f "$TMP/evict-ctrl/sp-er-2" "$SPIRA_RUN/landstate/sp-er-2"

# =============================================================================
echo
echo "closed+committed, no landstate file — stays closed (positive control):"
# =============================================================================
testdb_reset; seed sp-er-3
rm -f "$SPIRA_RUN/landstate/sp-er-3"
run_aeon
is     "bead stays closed (no landstate)"              closed "$(field sp-er-3 status)"
nowant "no eviction-race reopen fired"                 "eviction-race" "$(cat "$TMP/out")"

# =============================================================================
echo
echo "closed+committed+landstate=CERTIFIED — stays closed (positive control):"
# =============================================================================
testdb_reset; seed sp-er-4
printf 'CERTIFIED faksha %s certified\n' "$(date +%s)" > "$SPIRA_RUN/landstate/sp-er-4"
run_aeon
is     "bead stays closed (landstate=CERTIFIED)"       closed "$(field sp-er-4 status)"
nowant "no eviction-race reopen fired"                 "eviction-race" "$(cat "$TMP/out")"
rm -f "$SPIRA_RUN/landstate/sp-er-4"

# =============================================================================
echo
echo "closed+committed+landstate=RED no-rebase@<sha> — stays closed (landing.sh owns this):"
# =============================================================================
# no-rebase@ is not in LAND_EVICTION_REASONS; the guard exits before checking the tip.
testdb_reset; seed sp-er-5
printf 'RED faksha %s no-rebase@deadbeef\n' "$(date +%s)" > "$SPIRA_RUN/landstate/sp-er-5"
run_aeon
is     "bead stays closed (no-rebase@ RED)"           closed "$(field sp-er-5 status)"
nowant "no eviction-race reopen fired"                 "eviction-race" "$(cat "$TMP/out")"
rm -f "$SPIRA_RUN/landstate/sp-er-5"

# =============================================================================
echo
echo "(a) closed+committed+landstate=RED reason=gate — stays closed:"
# =============================================================================
# gate is not in LAND_EVICTION_REASONS; same shape as no-rebase@.
testdb_reset; seed sp-er-6
printf 'RED faksha %s gate\n' "$(date +%s)" > "$SPIRA_RUN/landstate/sp-er-6"
run_aeon
is     "bead stays closed (gate RED)"                 closed "$(field sp-er-6 status)"
nowant "no eviction-race reopen fired"                 "eviction-race" "$(cat "$TMP/out")"
rm -f "$SPIRA_RUN/landstate/sp-er-6"

# =============================================================================
echo
echo "(b) closed+committed+landstate=RED reason=ejected at STALE tip — stays closed:"
# =============================================================================
# Landstate is written before the shim runs with a fake tip. The shim's commit changes the
# branch tip, making the record stale. Stale record → close stands.
testdb_reset; seed sp-er-7
printf 'RED faksha %s ejected\n' "$(date +%s)" > "$SPIRA_RUN/landstate/sp-er-7"
run_aeon
is     "bead stays closed (stale eviction record)"     closed "$(field sp-er-7 status)"
nowant "no eviction-race reopen fired"                 "eviction-race" "$(cat "$TMP/out")"
want   "aeon log mentions stale record"                "stale record" "$(cat "$TMP/out")"
rm -f "$SPIRA_RUN/landstate/sp-er-7"

# =============================================================================
echo
echo "closed+committed+landstate=RED, sidecar matches — skip (idempotence):"
# =============================================================================
# When tip+reason in the sidecar match the current landstate, the reopen is a
# duplicate — the condition has not changed since the last reopen. The second
# pass must not write another requeued event.
testdb_reset; seed sp-er-8
printf 'RED fakesha99 %s ejected\n' "$(date +%s)" > "$SPIRA_RUN/landstate/sp-er-8"
printf 'fakesha99 ejected' > "$SPIRA_RUN/landstate/sp-er-8.evict-seen"
run_aeon
is   "bead stays closed (idempotence)"              closed "$(field sp-er-8 status)"
_rq8="$(bd -C "$SPIRA_DB" sql "SELECT COUNT(*) FROM events WHERE issue_id='sp-er-8' AND event_type='requeued' AND new_value='eviction-race'" 2>/dev/null | sed -n '3p' | tr -d ' ')"
is   "no eviction-race requeue event written"       0 "${_rq8:-0}"
want "aeon log shows idempotence skip"              "tip+reason unchanged" "$(cat "$TMP/out")"
rm -f "$SPIRA_RUN/landstate/sp-er-8" "$SPIRA_RUN/landstate/sp-er-8.evict-seen"

# =============================================================================
echo
echo "closed+committed+landstate=RED, cap reached — escalate, no requeue:"
# =============================================================================
# At SPIRA_EVICTION_ESCALATE_AT requeues the guard must escalate to the operator
# instead of requeueing, so the bead does not loop indefinitely. The landstate is written
# by the shim AFTER it commits (ejected-cur), so the tip is current and the stale-tip
# check does not intercept this before the cap check runs.
testdb_reset; seed sp-er-9
echo "ejected-cur" > "$TMP/evict-ctrl/sp-er-9"
for _i in 1 2 3; do
    _uuid="$(python3 -c 'import uuid; print(str(uuid.uuid4()))')"
    bd -C "$SPIRA_DB" sql "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$_uuid', 'sp-er-9', 'requeued', 'harness', 'eviction-race', NOW())" >/dev/null 2>&1
done
run_aeon
is   "bead stays closed (cap)"                     closed "$(field sp-er-9 status)"
_rq9="$(bd -C "$SPIRA_DB" sql "SELECT COUNT(*) FROM events WHERE issue_id='sp-er-9' AND event_type='requeued' AND new_value='eviction-race'" 2>/dev/null | sed -n '3p' | tr -d ' ')"
is   "no new requeue event at cap"                 3 "${_rq9:-0}"
want "aeon log shows escalation"                   "eviction-race escalated" "$(cat "$TMP/out")"
rm -f "$SPIRA_RUN/landstate/sp-er-9" "$SPIRA_RUN/landstate/sp-er-9.evict-seen" "$TMP/evict-ctrl/sp-er-9"

printf '\n%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
