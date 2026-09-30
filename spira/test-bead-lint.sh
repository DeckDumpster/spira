#!/usr/bin/env bash
#
# test-bead-lint.sh — bead.sh lint: the real `bd show --json` wiring around the label/
# status/type judgement (--all enumeration, unreadable ids, the branch: label existence
# check — T2, one fixture).
#
# T1 (the pure judgement, `_bead_lint_judge` over canned label/status/type/partition rows,
# no bd, no Dolt) moved to the Rust `bead` crate's own unit tests
# (bead/src/lib.rs::tests::lint_judge_rows_match_the_bash_suite_table, the same table,
# row for row) when `bead.sh`'s logic moved into the `bead` binary and `bead.sh` itself
# became a shim with no `_bead_lint_judge` function left to source (sp-g9mhe). Retiring it
# here rather than leaving a source-and-call that can never work again.
#
# tier: T2
# covers: spira/bead.sh UC-dispatch-04
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

export SPIRA_NO_LOOP_LABEL="no-loop"

echo "test-bead-lint.sh"

# ===========================================================================================
echo
echo "T2: real bd show --json wiring — one fixture, one seed"
# ===========================================================================================
. "$HERE/testdb.sh"
testdb_require test-bead-lint
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up bead_lint || { printf 'test-bead-lint: could not build fixture database\n' >&2; exit 1; }

run_lint() {              # run_lint <args...> -> sets LINT_OUT and LINT_RC from ONE call
    LINT_OUT="$(SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
        SPIRA_HOME="$HERE" SPIRA_CONF="$TMP/no.conf" \
        SPIRA_NO_LOOP_LABEL="no-loop" SPIRA_ASK_LABEL="needs-op-test" \
        SPIRA_INCIDENT_LABEL="incident-test" \
        bead.sh lint "$@" 2>&1)"
    LINT_RC=$?
}

testdb_seed <<'JSONL'
{"id":"sp-lint-mix-claim","title":"claimable","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-lint-mix-noloop","title":"no-loop","status":"open","issue_type":"task","labels":["repo:spira","no-loop"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-lint-mix-nopart","title":"broken: no partition","status":"open","issue_type":"task","labels":["repo:spira"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-lint-mix-norepo","title":"broken: no repo","status":"open","issue_type":"task","labels":["plan"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-lint-mix-ev.1","title":"State change: branch → spira/sp-lint-mix-ev","status":"closed","issue_type":"event","labels":[],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-lint-mix-brgood","title":"branch label names itself","status":"open","issue_type":"task","labels":["repo:spira","plan","branch:spira/sp-lint-mix-brgood"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-lint-mix-brbad","title":"branch label names another bead","status":"open","issue_type":"task","labels":["repo:spira","plan","branch:spira/sp-lint-mix-brgood"],"updated_at":"2026-09-25T00:00:00Z"}
JSONL

run_lint --all
wantrc "mixed --all exits 1 (offenders present)"         "1" "$LINT_RC"
nowant "--all does not report the claimable bead"        "sp-lint-mix-claim"  "$LINT_OUT"
nowant "--all does not report the no-loop bead"           "sp-lint-mix-noloop"  "$LINT_OUT"
want   "--all reports the no-partition bead"               "sp-lint-mix-nopart: no partition label" "$LINT_OUT"
want   "--all reports the no-repo bead"                     "sp-lint-mix-norepo: no repo: label"     "$LINT_OUT"
nowant "--all does not report the event bead"              "sp-lint-mix-ev" "$LINT_OUT"
nowant "--all does not report the self-named branch label" "sp-lint-mix-brgood: branch:" "$LINT_OUT"
want   "--all reports the branch label naming another bead" \
       "sp-lint-mix-brbad: branch: label names sp-lint-mix-brgood, not itself" "$LINT_OUT"

run_lint sp-lint-mix-nonexistent
wantrc "nonexistent id exits 1"    "1" "$LINT_RC"
want   "nonexistent id is unreadable, not a label defect" \
       "sp-lint-mix-nonexistent: unreadable" "$LINT_OUT"
nowant "nonexistent id not reported as no-label" "no repo: label" "$LINT_OUT"

run_lint sp-lint-mix-brgood
wantrc "self-named branch: label passes (positive control)" "0" "$LINT_RC"

run_lint sp-lint-mix-brbad
wantrc "branch: label naming another bead exits 1" "1" "$LINT_RC"
want   "branch: label naming another bead is reported" \
       "sp-lint-mix-brbad: branch: label names sp-lint-mix-brgood, not itself" "$LINT_OUT"

# ===========================================================================================
echo
echo "T3: two ask-labelled beads sharing a blocks edge (sp-z6m7y)"
# ===========================================================================================
# THE POSITIVE CONTROL IS FIRST: two ask-labelled beads wired with a plain blocks edge must
# be caught, before checking the shapes that must pass it through.
testdb_seed <<'JSONL'
{"id":"sp-lint-ask-a","title":"ask a","status":"open","issue_type":"decision","labels":["needs-op-test","overseer"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-lint-ask-b","title":"ask b","status":"open","issue_type":"decision","labels":["needs-op-test","overseer"],"updated_at":"2026-09-25T00:00:00Z","dependencies":[{"issue_id":"sp-lint-ask-b","depends_on_id":"sp-lint-ask-a","type":"blocks"}]}
{"id":"sp-lint-ask-rel-a","title":"ask rel a","status":"open","issue_type":"decision","labels":["needs-op-test","overseer"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-lint-ask-rel-b","title":"ask rel b","status":"open","issue_type":"decision","labels":["needs-op-test","overseer"],"updated_at":"2026-09-25T00:00:00Z","dependencies":[{"issue_id":"sp-lint-ask-rel-b","depends_on_id":"sp-lint-ask-rel-a","type":"relates-to"}]}
{"id":"sp-lint-ask-work","title":"work bead deliberately blocking an ask","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-lint-ask-target","title":"ask blocked by a work bead","status":"open","issue_type":"decision","labels":["needs-op-test","overseer"],"updated_at":"2026-09-25T00:00:00Z","dependencies":[{"issue_id":"sp-lint-ask-target","depends_on_id":"sp-lint-ask-work","type":"blocks"}]}
JSONL

run_lint --all
wantrc "ask-edge fixture --all exits 1 (offender present)" "1" "$LINT_RC"
want   "blocks edge between two ask-labelled beads is flagged" \
       "sp-lint-ask-b: blocks edge to ask-labelled sp-lint-ask-a" "$LINT_OUT"
nowant "relates-to edge between two ask-labelled beads is not flagged" \
       "sp-lint-ask-rel-b: blocks edge" "$LINT_OUT"
nowant "a work bead's deliberate block onto an ask is not flagged" \
       "sp-lint-ask-target: blocks edge" "$LINT_OUT"

run_lint sp-lint-ask-a
wantrc "the non-blocking end of the flagged edge passes alone" "0" "$LINT_RC"

# ===========================================================================================
echo
echo "T4: a work bead blocks-dependent on an incident/alarm bead (sp-3bc6t, sp-ivlj4)"
# ===========================================================================================
# THE POSITIVE CONTROL IS FIRST: a plain work bead wired to block on an incident-labelled
# bead — exactly the sp-pyowh/sp-kogm shape — must be caught before checking the shapes
# that must pass it through. SPIRA_INCIDENT_LABEL is pinned to a non-default
# ("incident-test") by run_lint so this proves the check reads the configured key rather
# than a literal "incident".
testdb_seed <<'JSONL'
{"id":"sp-lint-inc-alarm","title":"recurring alarm","status":"open","issue_type":"task","labels":["incident-test","spira","repo:spira","no-loop"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-lint-inc-work","title":"work bead wrongly blocked on the alarm","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-09-25T00:00:00Z","dependencies":[{"issue_id":"sp-lint-inc-work","depends_on_id":"sp-lint-inc-alarm","type":"blocks"}]}
{"id":"sp-lint-inc-rel","title":"work bead related to the alarm, not blocked","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-09-25T00:00:00Z","dependencies":[{"issue_id":"sp-lint-inc-rel","depends_on_id":"sp-lint-inc-alarm","type":"relates-to"}]}
{"id":"sp-lint-work-a","title":"ordinary work a","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-lint-work-b","title":"ordinary work b, blocked on work a","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-09-25T00:00:00Z","dependencies":[{"issue_id":"sp-lint-work-b","depends_on_id":"sp-lint-work-a","type":"blocks"}]}
JSONL

run_lint --all
wantrc "incident-edge fixture --all exits 1 (offender present)" "1" "$LINT_RC"
want   "blocks edge onto an incident-labelled bead is flagged" \
       "sp-lint-inc-work: blocks edge to incident-labelled sp-lint-inc-alarm" "$LINT_OUT"
nowant "relates-to edge onto the same alarm is not flagged" \
       "sp-lint-inc-rel: blocks edge" "$LINT_OUT"
nowant "a blocks edge between two ordinary work beads is not flagged" \
       "sp-lint-work-b: blocks edge" "$LINT_OUT"

run_lint sp-lint-inc-alarm
wantrc "the alarm itself, with no outgoing blocks edge, passes alone" "0" "$LINT_RC"

run_lint sp-lint-work-b
wantrc "a work-onto-work blocks edge passes alone (positive control for the accept path)" "0" "$LINT_RC"

tl_summary
