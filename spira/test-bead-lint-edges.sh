#!/usr/bin/env bash
#
# test-bead-lint-edges.sh — the ask/ask and work/incident blocks-edge checks in `bead lint --all`
#
# The real `bd show --json` wiring around the label/status/type judgement, T2, one small
# fixture. The pure judgement is the Rust `bead` crate's own unit test
# (bead/src/lib.rs::tests::lint_judge_rows_match_the_bash_suite_table). Each lint call costs
# about a second of real bd round trips per bead, so the fixture holds only the beads this
# file's assertions read; test-bead-lint.sh, test-bead-lint-edges.sh and
# test-bead-lint-remedy.sh together make the assertions the one suite made.
#
# tier: T2
# covers: spira/bead.sh UC-dispatch-04
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=180   # slow cases: real git/dolt on a loaded VM

export SPIRA_NO_LOOP_LABEL="no-loop"
tl_config SPIRA_NO_LOOP_LABEL="no-loop"

echo "test-bead-lint-edges.sh"

# ===========================================================================================
echo
echo "T2: real bd show --json wiring — one fixture, one seed"
# ===========================================================================================
. "$HERE/testdb.sh"
testdb_require test-bead-lint-edges
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up bead_lint_edges || { printf 'test-bead-lint: could not build fixture database\n' >&2; exit 1; }

# The partition check applies to a bead awaiting dispatch — its lifecycle state, not bd
# status (sp-mve9i): this world's machine mirrors the store (open READY, closed LANDED).
lc_mirror_bd "$TMP/lc"
run_lint() {              # run_lint <args...> -> sets LINT_OUT and LINT_RC from ONE call
    # SPIRA_DB/SPIRA_BD/SPIRA_NO_LOOP_LABEL/SPIRA_ASK_LABEL are registered keys (per Ryan
    # 2026-10-05, ONE SOURCE OF CONFIG): declare via tl_config, not the env prefix below,
    # which no process reads them from any more.
    # round 3 fix: the complete fixture declares scope_label="spira" as its base value,
    # so the partition check would require a bare "spira" label none of this suite's
    # fixture beads carry (they use "repo:spira", a different label). Declare the empty
    # scope this suite has always meant.
    # round 4 fix (pattern 6): SPIRA_RUNES_DIR no longer derives from SPIRA_HOME even when
    # SPIRA_HOME is the real repo — without it, bead lint's partition check cannot read
    # runes/*.rune at all, so it never recognises a valid partition label.
    # The incident-edge check (bead/src/main.rs::cfg_label) reads SPIRA_INCIDENT_LABEL, not
    # SPIRA_ALARM_LABEL — that name was never a real key, even before the migration; pin it
    # to a non-default so this proves the check reads the configured key, not a literal
    # "incident".
    tl_config SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
        SPIRA_NO_LOOP_LABEL="no-loop" SPIRA_ASK_LABEL="needs-op-test" SPIRA_SCOPE_LABEL="" \
        SPIRA_RUNES_DIR="$HERE/runes" SPIRA_INCIDENT_LABEL="incident-test"
    LINT_OUT="$(SPIRA_LC_BIN="$SPIRA_LC_BIN" \
        SPIRA_HOME="$HERE" SPIRA_CONF="$TMP/no.conf" \
        bead.sh lint "$@" 2>&1)"
    LINT_RC=$?
}


testdb_seed <<'JSONL'
{"id":"sp-lint-ask-a","title":"ask a","status":"open","issue_type":"decision","labels":["needs-op-test","overseer"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-lint-ask-b","title":"ask b","status":"open","issue_type":"decision","labels":["needs-op-test","overseer"],"updated_at":"2026-09-25T00:00:00Z","dependencies":[{"issue_id":"sp-lint-ask-b","depends_on_id":"sp-lint-ask-a","type":"blocks"}]}
{"id":"sp-lint-ask-rel-a","title":"ask rel a","status":"open","issue_type":"decision","labels":["needs-op-test","overseer"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-lint-ask-rel-b","title":"ask rel b","status":"open","issue_type":"decision","labels":["needs-op-test","overseer"],"updated_at":"2026-09-25T00:00:00Z","dependencies":[{"issue_id":"sp-lint-ask-rel-b","depends_on_id":"sp-lint-ask-rel-a","type":"relates-to"}]}
{"id":"sp-lint-ask-work","title":"work bead deliberately blocking an ask","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-09-25T00:00:00Z","description":"Why.\n\n## Build\n\nThe change, in spira/x.\n"}
{"id":"sp-lint-ask-target","title":"ask blocked by a work bead","status":"open","issue_type":"decision","labels":["needs-op-test","overseer"],"updated_at":"2026-09-25T00:00:00Z","dependencies":[{"issue_id":"sp-lint-ask-target","depends_on_id":"sp-lint-ask-work","type":"blocks"}]}
{"id":"sp-lint-inc-alarm","title":"recurring alarm","status":"open","issue_type":"task","labels":["incident-test","spira","repo:spira","no-loop"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-lint-inc-work","title":"work bead wrongly blocked on the alarm","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-09-25T00:00:00Z","dependencies":[{"issue_id":"sp-lint-inc-work","depends_on_id":"sp-lint-inc-alarm","type":"blocks"}],"description":"Why.\n\n## Build\n\nThe change, in spira/x.\n"}
{"id":"sp-lint-inc-rel","title":"work bead related to the alarm, not blocked","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-09-25T00:00:00Z","dependencies":[{"issue_id":"sp-lint-inc-rel","depends_on_id":"sp-lint-inc-alarm","type":"relates-to"}],"description":"Why.\n\n## Build\n\nThe change, in spira/x.\n"}
{"id":"sp-lint-work-a","title":"ordinary work a","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-09-25T00:00:00Z","description":"Why.\n\n## Build\n\nThe change, in spira/x.\n"}
{"id":"sp-lint-work-b","title":"ordinary work b, blocked on work a","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-09-25T00:00:00Z","dependencies":[{"issue_id":"sp-lint-work-b","depends_on_id":"sp-lint-work-a","type":"blocks"}],"description":"Why.\n\n## Build\n\nThe change, in spira/x.\n"}
JSONL

run_lint --all
ALL_OUT="$LINT_OUT"

# ===========================================================================================
echo
echo "T3: two ask-labelled beads sharing a blocks edge (sp-z6m7y)"
# ===========================================================================================
# THE POSITIVE CONTROL IS FIRST: two ask-labelled beads wired with a plain blocks edge must
# be caught, before checking the shapes that must pass it through.

want   "blocks edge between two ask-labelled beads is flagged" \
       "sp-lint-ask-b: blocks edge to ask-labelled sp-lint-ask-a" "$ALL_OUT"
nowant "relates-to edge between two ask-labelled beads is not flagged" \
       "sp-lint-ask-rel-b: blocks edge" "$ALL_OUT"
nowant "a work bead's deliberate block onto an ask is not flagged" \
       "sp-lint-ask-target: blocks edge" "$ALL_OUT"

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

want   "blocks edge onto an incident-labelled bead is flagged" \
       "sp-lint-inc-work: blocks edge to incident-labelled sp-lint-inc-alarm" "$ALL_OUT"
nowant "relates-to edge onto the same alarm is not flagged" \
       "sp-lint-inc-rel: blocks edge" "$ALL_OUT"
nowant "a blocks edge between two ordinary work beads is not flagged" \
       "sp-lint-work-b: blocks edge" "$ALL_OUT"

run_lint sp-lint-inc-alarm
wantrc "the alarm itself, with no outgoing blocks edge, passes alone" "0" "$LINT_RC"

run_lint sp-lint-work-b
wantrc "a work-onto-work blocks edge passes alone (positive control for the accept path)" "0" "$LINT_RC"
tl_summary
