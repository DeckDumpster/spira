#!/usr/bin/env bash
#
# test-bead-lint.sh — bead.sh lint: the pure label/status/type judgement
# (_bead_lint_judge, T1 over canned rows) and the real `bd show --json` wiring around it
# (--all enumeration, unreadable ids, the branch: label existence check — T2, one fixture).
#
# tier: T2
# covers: spira/bead.sh UC-dispatch-04
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

export SPIRA_NO_LOOP_LABEL="no-loop"
# shellcheck disable=SC1090
. "$HERE/bead.sh"

echo "test-bead-lint.sh"

# ===========================================================================================
echo
echo "T1: _bead_lint_judge over canned labels/status/type/partitions (no bd, no Dolt)"
# ===========================================================================================
# THE POSITIVE CONTROL IS FIRST (law-absence-needs-a-positive-control): a judge that never
# fires would pass every "not reported" row below for the wrong reason.
PART="plan maechen-sweep spike czar-trigger incident groom"

# Row format: name|labels|status|type|partitions|want_rc|want_out ("\n" marks a line break
# in a multi-line expected output; EMPTY marks an empty labels string).
ROWS=(
    "positive control: repo: + partition passes|repo:spira plan|open|task|${PART}|0|"
    "missing repo: is reported|plan|open|task|${PART}|1|no repo: label"
    "missing partition is reported|repo:spira|open|task|${PART}|1|no partition label; add one or mark no-loop"
    "no-loop bypasses the partition check|repo:spira no-loop|open|task|${PART}|0|"
    "event type is exempt from both checks|EMPTY|open|event|${PART}|0|"
    "epic needs repo: but not a partition|repo:spira|open|epic|${PART}|0|"
    "closed status skips the partition check|repo:spira|closed|task|${PART}|0|"
    "zero labels reports both defects|EMPTY|open|task|${PART}|2|no repo: label\nno partition label; add one or mark no-loop"
    "the check generalises across claimable types (bug)|repo:spira|open|bug|${PART}|1|no partition label; add one or mark no-loop"
)

for row in "${ROWS[@]}"; do
    IFS='|' read -r name labels status type partitions want_rc want_out <<<"$row"
    [ "$labels" = "EMPTY" ] && labels=""
    want_out="${want_out//\\n/$'\n'}"
    out="$(_bead_lint_judge "$labels" "$status" "$type" "$partitions")"
    rc=$?
    wantrc "$name (rc)"     "$want_rc"  "$rc"
    is     "$name (output)" "$want_out" "$out"
done

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
        bash "$HERE/bead.sh" lint "$@" 2>&1)"
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

tl_summary
