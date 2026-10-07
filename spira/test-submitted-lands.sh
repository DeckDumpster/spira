#!/usr/bin/env bash
#
# test-submitted-lands.sh — a work bead the model submits (sp-qsona; `work submit` since
#   sp-v62vn) must, in a NON-QUEUE repository, still land and close: work submit ->
#   SUBMITTED -> landing pass gates and lands it -> LANDED, and close-on-land closes it.
#
# THE DEFECT THIS REPRODUCES. sp-qsona made spira-lc close-on-land the only thing that closes a
# work bead, called when the work reaches the base. Queue mode reaches it through the batch
# verdict. Every other mode reaches the base through landing.sh's CHECK 6 (push merges it,
# pr opens a pull request, hold gates it) — and CHECK 6 skipped, refused to certify and
# refused to land any branch whose bead was not `closed`. A submitted bead is open, so in
# push, pr and hold mode nothing ever landed and nothing ever closed. Release acceptance runs
# a push-mode scratch repo and failed phase A stage 5 ("no commit with bead id on origin/main
# after 121s") on every release from the one that shipped sp-qsona.
#
# CASES (law-absence-needs-a-positive-control), against a real lifecycle record (below):
#   1. END TO END, push mode: the real aeon runs a model stand-in that commits and finishes
#      with `work submit` -> the row is SUBMITTED at the branch tip; the real landing pass
#      gates it (the gate's GatePass: CERTIFIED), lands it on origin/main, push-delivered
#      moves the row to LANDED and close-on-land closes the bead in bd.
#   2. A submitted bead whose gate is RED goes back to the builders: the gate's GateRed
#      moves the row to REWORK and nothing lands — and the next aeon really claims it again
#      and re-submits. A red that left it unclaimable would strand it: open, excluded from
#      every claim, never landed.
#   3. pr/hold mode: the merge happens off-box, and the Sending is the first to see the work
#      on the base. A SUBMITTED bead whose branch is found landed there is closed.
#   4. push mode: a bead whose lifecycle row the push gate moved from
#      SUBMITTED to CERTIFIED is still landed by that same pass, and ends LANDED.
#
# sp-v62vn (lifecycle_enforce=off retired) changed what cases 1-3 are told in: the model
# no longer `bd close`s and the aeon no longer converts that close into open +
# spira-submitted, so the assertions that read the spira-submitted label (1, 2, the setup
# of 3) now read the lifecycle row it stood for; case 2's "reopened <id>" line is replaced
# by REWORK plus a real re-claim (see case 2's note).
#
# defect: sp-qsona (acceptance phase A stage 5)
# tier: T3
# covers: landing-pass/* sending/src/* spira/lib.sh aeon/src/* work/* spira-lc/*
# hermetic-ok: uses fixture databases (bd, and a private dolt sql-server for spira_lifecycle) and local git repos, no systemd or gh
# timeout: 240
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
# THE LANDING PASS IS THE landing-pass BINARY (landing-pass/DESIGN.md §7.4): `land` for a
# pass, `halt` to stop one. Resolved from this tree before any fixture repoints SPIRA_REPO.
#
# THE LIFECYCLE SERVICE IS REAL (sp-v62vn retired lifecycle_enforce=off). The aeon always
# runs the model restricted, and a model finishes only through the work broker: `work
# submit` -> `spira-lc serve` -> the bead machine. So this suite stands up a throwaway
# spira_lifecycle database (testlib/lc-fixture.sh) behind the tree's own `spira-lc serve`,
# and every actor — the aeon's claim, the model's submit, the gate stub's verdict, the
# landing pass's delivery, close-on-land — reads and writes real lifecycle rows. bd holds
# the bead's content only; no assertion here reads bd status as the bead's state, except
# the close that close-on-land still writes to bd when the work lands.

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
. "$HERE/testlib/lc-fixture.sh"
testdb_require test-submitted-lands
TMP="$(mktemp -d)"
SERVE_PID=""
trap '[ -n "$SERVE_PID" ] && kill "$SERVE_PID" >/dev/null 2>&1; lcfix_down; testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up submittedlands || { echo "test-submitted-lands: could not build a fixture database"; exit 1; }
lcfix_up || { echo "test-submitted-lands: could not build a lifecycle fixture"; exit 1; }
LC_SOCK="$TMP/lc.sock"
tl_config SPIRA_LC_SOCKET="$LC_SOCK"
spira-lc serve "$LC_SOCK" > "$TMP/serve.log" 2>&1 &
SERVE_PID=$!
for _ in $(seq 1 50); do [ -S "$LC_SOCK" ] && break; sleep 0.1; done
[ -S "$LC_SOCK" ] || bail "spira-lc serve never opened its socket: $(cat "$TMP/serve.log")"
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

echo "test-submitted-lands.sh"

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; timeout 5 git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; timeout 5 git -C "$REPO" push -q origin main 2>/dev/null
timeout 5 git -C "$REPO" fetch -q origin
git -C "$REPO" remote set-head origin main 2>/dev/null || true

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
tl_config SPIRA_CHAMBER="$SPIRA_HOME/chamber"
# conf.d IS COPIED IN (matching test-aeon-sweep.sh, test-aeon-world-stop.sh, ...): aeon's
# own in-process config registry (spira_config::resolve, aeon::conf::merge_resolved_config)
# derives conf.d from THIS --home and now REFUSES to start if it is missing (sp-1cdgq) --
# a --home with no conf.d used to resolve silently to nothing instead of refusing.
cp -r "$HERE/conf.d" "$SPIRA_HOME/"
find "$HERE" -maxdepth 1 \( -name '*.sh' -o -name '*.py' \) ! -name 'test-*.sh' -exec cp {} "$SPIRA_HOME/" \;
cp -r "$HERE/actors" "$SPIRA_HOME/" 2>/dev/null || true
SPIRA_RUN="$TMP/run"; export SPIRA_RUN; mkdir -p "$SPIRA_RUN/worktree"; tl_config SPIRA_RUN="$SPIRA_RUN"
SPIRA_REPO_MAP="$TMP/repo-map"; tl_config SPIRA_REPO_MAP="$SPIRA_REPO_MAP"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"

stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SPIRA_HOME/$1"; chmod +x "$SPIRA_HOME/$1"; }
stub confine.sh 'exit 0'
stub mail    'exit 0'
stub gh         'exit 1'
# THE GATE STUB RECORDS ITS VERDICT AS THE REAL GATE DOES: gate/src/engine.rs ends every
# judged run with `spira-lc certify <SPIRA_GATE_BEAD> <branch tip> pass|red <detail> gate`
# — GatePass moves a SUBMITTED row to CERTIFIED, GateRed to REWORK. The landing pass then
# re-reads the row it is about to land. $TMP/gate-state keeps the state the row read right
# after the verdict, for the sections that assert what the gate itself did.
gate_stub() {   # gate_stub pass|red
    local verdict=PASS rc=0 detail=stub
    [ "$1" = red ] && { verdict=FAIL; rc=1; detail=stub-fail; }
    stub gate.sh "tip=\"\$(git -C '$REPO' rev-parse \"\$1\" 2>/dev/null)\"
[ -n \"\${SPIRA_GATE_BEAD:-}\" ] && [ -n \"\$tip\" ] && spira-lc certify \"\$SPIRA_GATE_BEAD\" \"\$tip\" $1 $detail gate >/dev/null 2>&1
spira-lc show \"\${SPIRA_GATE_BEAD:-}\" 2>/dev/null | python3 -c 'import sys,json; print(json.load(sys.stdin)[\"bead\"][\"state\"])' > '$TMP/gate-state' 2>/dev/null
echo \"gate: VERDICT=$verdict reason=$detail branch=\$1 repo=\${2:-?}\" >&2; exit $rc"
}
gate_pass() { gate_stub pass; }
gate_fail() { gate_stub red; }
gate_pass
# The fixture home (and its stubs) is the harness in force: bare names resolve here first.
export PATH="$SPIRA_HOME:$PATH"

cat > "$SPIRA_HOME/chamber/builder.fayth" <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

# THE SHIM IS THE SESSION: commit, then finish with `work submit`, the builder's {{FINISH}}
# brief and the only exit a model has (sp-v62vn: the aeon always runs it restricted — no
# bd, no SPIRA_DB, no TMP; PATH is model-bin/ and the system dirs). It used to `bd close`
# and rely on the aeon converting that close into open + spira-submitted; there is no such
# conversion any more. conf.sh replaces PATH, so the model is injected through SPIRA_AGENT,
# and $TMP is baked in because the restricted environment does not carry it. The fixture
# ids are sp-sl1..sp-sl4, not sp-sl-1: `work` refuses a binding that is not sp-<alnum>.
BIN="$TMP/bin"; mkdir -p "$BIN"; tl_config SPIRA_AGENT="$BIN/claude"
command -v aeon >/dev/null 2>&1 \
    || { echo "test-submitted-lands: aeon is not on PATH" >&2; exit 1; }
cat > "$BIN/claude" <<SHIM
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="\$(sed -n 's/^work \\(sp-[a-z0-9-]*\\) .*/\\1/p' "$TMP/prompt" | head -1)"
printf '%s\\n' "\$id" >> "\$id.txt"
git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "\$id: the work"
work submit > "$TMP/submit.out" 2>&1
printf '%s' "\$?" > "$TMP/submit.rc"
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":3}\\n'
exit 0
SHIM
chmod +x "$BIN/claude"

B() { bd -C "$SPIRA_DB" "$@"; } # batch-job: fixture bd call against the suite's throwaway store
field() { B show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
v=d[0].get(sys.argv[1])
print(",".join(v) if isinstance(v,list) else (v or ""))' "$2" 2>/dev/null; }
seed() {   # seed <id> [lifecycle-state] [tip] — the bd content row plus its lifecycle row
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "$_lbl" | testdb_seed
    lcfix_seed "$1" "${2:-READY}" "${3:-}" || bail "could not seed $1's lifecycle row"
}
# A submit that did not apply says why, as TAP comments: the broker's answer, then the aeon's.
run_aeon() {
    rm -rf "$SPIRA_RUN/worktree" "$TMP/submit.rc" "$TMP/submit.out"
    aeon --home "$SPIRA_HOME" builder > "$TMP/aeon.out" 2>&1
    [ "$(cat "$TMP/submit.rc" 2>/dev/null)" = 0 ] && return 0
    { cat "$TMP/submit.out" 2>/dev/null; tail -n 15 "$TMP/aeon.out"; } | sed 's/^/# /'
}
tl_config SPIRA_HOME_REPO=fixture SPIRA_ID_PREFIX=sp SPIRA_GH="$SPIRA_HOME/gh"
landing() {
    rm -f "$SPIRA_RUN/landing.progress" "$TMP/gate-state"
    SPIRA_REPO="$REPO" \
        landing-pass land 2>&1
}
sending() {
    SPIRA_REPO="$REPO" \
        command sending 2>&1
}
on_base() { timeout 5 git -C "$REPO" fetch -q origin 2>/dev/null; git -C "$REPO" log --format=%s origin/main 2>/dev/null; }
branch_tip() { git -C "$REPO" rev-parse "spira/$1" 2>/dev/null; }

# ======================================================================================
echo
echo "1. END TO END, push mode — work submit -> SUBMITTED -> CERTIFIED -> LANDED -> closed:"
# ======================================================================================
testdb_reset; seed sp-sl1
run_aeon
is   "the model's work submit was applied by the broker"             0 "$(cat "$TMP/submit.rc" 2>/dev/null || echo missing)"
is   "after the aeon: the lifecycle row is SUBMITTED"                SUBMITTED "$(lcfix_state sp-sl1)"
is   "at the branch tip the model committed"                         "$(branch_tip sp-sl1)" "$(lcfix_tip sp-sl1)"
is   "bd's status was never touched (no close to convert)"           open "$(field sp-sl1 status)"
nowant "and nothing is on the base yet"                              "sp-sl1" "$(on_base)"

out="$(landing)"
is   "the gate's GatePass certified the row before the land"         CERTIFIED "$(cat "$TMP/gate-state" 2>/dev/null)"
want "the landing pass lands the submitted bead's branch"            "landed spira/sp-sl1" "$out"
want "its commit is on origin/main"                                  "sp-sl1: the work" "$(on_base)"
is   "the lifecycle row ends LANDED"                                 LANDED "$(lcfix_state sp-sl1)"
is   "and the bead is closed by the landing"                         closed "$(field sp-sl1 status)"
want "with the landed outcome as its close reason"                   "OUTCOME: landed" "$(field sp-sl1 close_reason)"
nowant "never the 'not landed — its bead is open' skip"              "its bead is open" "$out"

# ======================================================================================
echo
echo "2. a submitted bead whose gate is RED goes back to the builders — claimable again:"
# ======================================================================================
testdb_reset; seed sp-sl2
run_aeon
is   "setup: submitted" SUBMITTED "$(lcfix_state sp-sl2)"
gate_fail
out="$(landing)"
gate_pass
is     "the red gate moves the row to REWORK, which the ready set claims" REWORK "$(lcfix_state sp-sl2)"
nowant "the red branch is not landed"                                     "landed spira/sp-sl2" "$out"
nowant "and nothing of it reaches the base"                               "sp-sl2" "$(on_base)"
is     "the bead is open"                                                 open "$(field sp-sl2 status)"
# CLAIMABLE AGAIN, proven by claiming it: a REWORK row an aeon could not take would strand
# the bead — open, excluded from every claim, never landed.
run_aeon
is   "the next aeon claims it again and its submit applies"               0 "$(cat "$TMP/submit.rc" 2>/dev/null || echo missing)"
is   "the row is SUBMITTED once more"                                     SUBMITTED "$(lcfix_state sp-sl2)"
is   "at the new tip"                                                     "$(branch_tip sp-sl2)" "$(lcfix_tip sp-sl2)"

# ======================================================================================
echo
echo "3. pr/hold mode — a submitted bead whose branch the Sending finds on the base closes:"
# ======================================================================================
# The forge (or a human) merged the branch; nothing on this box landed it. Simulated with a
# merge made directly on origin/main, then the Sending sweep.
testdb_reset
git -C "$REPO" branch -q -f spira/sp-sl3 origin/main
git -C "$REPO" worktree add -q "$TMP/wt3" spira/sp-sl3
printf 'three\n' > "$TMP/wt3/three.txt"
git -C "$TMP/wt3" add -A; git -C "$TMP/wt3" commit -qm "sp-sl3: the work"
git -C "$REPO" worktree remove --force "$TMP/wt3"
seed sp-sl3 SUBMITTED "$(branch_tip sp-sl3)"
git -C "$REPO" checkout -q main 2>/dev/null; git -C "$REPO" reset -q --hard origin/main
git -C "$REPO" merge -q --no-ff -m "Merge pull request #3 from spira/sp-sl3" spira/sp-sl3
timeout 5 git -C "$REPO" push -q origin main 2>/dev/null
timeout 5 git -C "$REPO" fetch -q origin
is   "setup: open and SUBMITTED before the sweep" "open SUBMITTED" "$(field sp-sl3 status) $(lcfix_state sp-sl3)"
out="$(sending)"
want "the Sending sends the landed branch"             "sp-sl3" "$out"
is   "and closes the submitted bead"                   closed "$(field sp-sl3 status)"
want "with the landed outcome"                         "OUTCOME: landed" "$(field sp-sl3 close_reason)"

# ======================================================================================
echo
echo "4. push mode — a row the push gate moved SUBMITTED -> CERTIFIED at the tip still lands:"
# ======================================================================================
# Local acceptance on d40bbb589: the model finishes with `work submit` (no bd close), so the
# bead is open and only the lifecycle row says SUBMITTED. The push gate itself records
# GatePass (SUBMITTED -> CERTIFIED) before the land; the pass then re-read the bead, found
# no SUBMITTED row, logged "bead is now open (was closed at scan time) — not landing" and
# every later pass skipped it as "its bead is open": READY, WORKING, SUBMITTED, CERTIFIED
# and never LANDED. Here the row is a real one and the gate stub's `spira-lc certify` is the
# real GatePass, so the pass's re-read sees exactly what production's does.
testdb_reset
timeout 5 git -C "$REPO" fetch -q origin
timeout 5 git -C "$REPO" branch -q -f spira/sp-sl4 origin/main
timeout 5 git -C "$REPO" worktree add -q "$TMP/wt4" spira/sp-sl4
printf 'four\n' > "$TMP/wt4/four.txt"
timeout 5 git -C "$TMP/wt4" add -A; timeout 5 git -C "$TMP/wt4" commit -qm "sp-sl4: the work"
timeout 5 git -C "$REPO" worktree remove --force "$TMP/wt4"
tip4="$(timeout 5 git -C "$REPO" rev-parse spira/sp-sl4)"
seed sp-sl4 SUBMITTED "$tip4"
is   "setup: the bead is open and SUBMITTED at its tip" "open SUBMITTED $tip4" \
     "$(field sp-sl4 status) $(lcfix_state sp-sl4) $(lcfix_tip sp-sl4)"
out="$(landing)"
is     "the gate moved the row to CERTIFIED before the land"         CERTIFIED "$(cat "$TMP/gate-state" 2>/dev/null)"
nowant "the pass does not refuse the land right after its own gate" "was closed at scan time" "$out"
want   "the landing pass lands the certified bead's branch"         "landed spira/sp-sl4" "$out"
want   "its commit is on origin/main"                               "sp-sl4: the work" "$(on_base)"
is     "push-delivered moves the CERTIFIED row to LANDED"           LANDED "$(lcfix_state sp-sl4)"
is     "and close-on-land closes the bead"                          closed "$(field sp-sl4 status)"

tl_summary
