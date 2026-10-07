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
tl_config SPIRA_NO_LOOP_LABEL="no-loop"

echo "test-bead-lint.sh"

# ===========================================================================================
echo
echo "T2: real bd show --json wiring — one fixture, one seed"
# ===========================================================================================
. "$HERE/testdb.sh"
testdb_require test-bead-lint
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up bead_lint || { printf 'test-bead-lint: could not build fixture database\n' >&2; exit 1; }

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
    # round 4 fix (pattern 6): SPIRA_CHAMBER no longer derives from SPIRA_HOME even when
    # SPIRA_HOME is the real repo — without it, bead lint's partition check cannot read
    # chamber/*.fayth at all, so it never recognises a valid partition label.
    # The incident-edge check (bead/src/main.rs::cfg_label) reads SPIRA_INCIDENT_LABEL, not
    # SPIRA_ALARM_LABEL — that name was never a real key, even before the migration; pin it
    # to a non-default so this proves the check reads the configured key, not a literal
    # "incident".
    tl_config SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
        SPIRA_NO_LOOP_LABEL="no-loop" SPIRA_ASK_LABEL="needs-op-test" SPIRA_SCOPE_LABEL="" \
        SPIRA_CHAMBER="$HERE/chamber" SPIRA_INCIDENT_LABEL="incident-test"
    LINT_OUT="$(SPIRA_LC_BIN="$SPIRA_LC_BIN" \
        SPIRA_HOME="$HERE" SPIRA_CONF="$TMP/no.conf" \
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

# ===========================================================================================
echo
echo "T5: an incident whose open remedy is linked relates-to only (law-a-bug-with-a-fix-in-flight-depends-on-it)"
# ===========================================================================================
testdb_seed <<'JSONL'
{"id":"sp-lint-rem-bad","title":"incident, remedy only related","status":"open","issue_type":"task","labels":["incident-test","spira","repo:spira","no-loop"],"updated_at":"2026-10-01T00:00:00Z","dependencies":[{"issue_id":"sp-lint-rem-bad","depends_on_id":"sp-lint-rem-fix1","type":"relates-to"}]}
{"id":"sp-lint-rem-fix1","title":"remedy one","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-10-01T00:00:00Z"}
{"id":"sp-lint-rem-good","title":"incident, remedy blocks","status":"open","issue_type":"task","labels":["incident-test","spira","repo:spira","no-loop"],"updated_at":"2026-10-01T00:00:00Z","dependencies":[{"issue_id":"sp-lint-rem-good","depends_on_id":"sp-lint-rem-fix2","type":"blocks"}]}
{"id":"sp-lint-rem-fix2","title":"remedy two","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-10-01T00:00:00Z"}
{"id":"sp-lint-rem-done","title":"incident, remedy landed","status":"open","issue_type":"task","labels":["incident-test","spira","repo:spira","no-loop"],"updated_at":"2026-10-01T00:00:00Z","dependencies":[{"issue_id":"sp-lint-rem-done","depends_on_id":"sp-lint-rem-fix3","type":"relates-to"}]}
{"id":"sp-lint-rem-fix3","title":"remedy three, closed","status":"closed","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-10-01T00:00:00Z"}
JSONL

run_lint sp-lint-rem-bad
wantrc "relates-to-only open remedy exits 1" "1" "$LINT_RC"
want   "relates-to-only open remedy is flagged" \
       "sp-lint-rem-bad: open remedy sp-lint-rem-fix1 is linked relates-to only" "$LINT_OUT"

run_lint sp-lint-rem-good
wantrc "a blocks-linked open remedy passes" "0" "$LINT_RC"

run_lint sp-lint-rem-done
wantrc "a relates-to remedy that already closed passes" "0" "$LINT_RC"

echo
echo "T5b: filing a remedy under an incident adds the blocks edge in the same step"
testdb_seed <<'JSONL'
{"id":"sp-lint-fil-inc","title":"incident to remedy","status":"open","issue_type":"task","labels":["incident-test","spira","repo:spira","no-loop"],"updated_at":"2026-10-01T00:00:00Z"}
{"id":"sp-lint-fil-work","title":"ordinary parent","status":"open","issue_type":"task","labels":["spira","repo:spira","plan"],"updated_at":"2026-10-01T00:00:00Z"}
JSONL

file_under() {            # file_under <parent> -> FILE_OUT (new id), FILE_RC
    FILE_OUT="$(SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
        SPIRA_HOME="$HERE" SPIRA_CONF="$TMP/no.conf" SPIRA_BEAD_LANE_OVERRIDE=1 \
        SPIRA_ALARM_LABEL="incident-test" \
        bead.sh file "remedy for $1" --for builder --repo harness --parent "$1" 2>"$TMP/file.err")"
    FILE_RC=$?
}
blocks_of() {             # blocks_of <id> -> space-separated ids it blocks-depends on
    SPIRA_DB="$SPIRA_DB" timeout 5 "${SPIRA_BD:-$TESTDB_BD}" -C "$SPIRA_DB" dep list "$1" --type blocks --json 2>/dev/null \
        | python3 -c 'import json,sys; print(" ".join(d.get("depends_on_id") or d.get("id") for d in json.load(sys.stdin)))'
}

file_under sp-lint-fil-inc
wantrc "filing a remedy under an incident exits 0" "0" "$FILE_RC"; cat "$TMP/file.err" | sed "s/^/# err: /"
want   "the incident now blocks on the new remedy" "$(printf %s "$FILE_OUT" | head -n1)" "$(blocks_of sp-lint-fil-inc)"

file_under sp-lint-fil-work
wantrc "filing under an ordinary bead exits 0 (positive control)" "0" "$FILE_RC"
is     "an ordinary parent gets no blocks edge" "" "$(blocks_of sp-lint-fil-work)"

tl_summary
