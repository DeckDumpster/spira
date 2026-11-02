#!/usr/bin/env bash
#
# test-bead-lint.sh — --all enumeration, unreadable ids and the branch: label check
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
{"id":"sp-lint-mix-claim","title":"claimable","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-09-25T00:00:00Z","description":"Why.\n\n## Build\n\nThe change, in spira/x.\n"}
{"id":"sp-lint-mix-noloop","title":"no-loop","status":"open","issue_type":"task","labels":["repo:spira","no-loop"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-lint-mix-nopart","title":"broken: no partition","status":"open","issue_type":"task","labels":["repo:spira"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-lint-mix-norepo","title":"broken: no repo","status":"open","issue_type":"task","labels":["plan"],"updated_at":"2026-09-25T00:00:00Z","description":"Why.\n\n## Build\n\nThe change, in spira/x.\n"}
{"id":"sp-lint-mix-ev.1","title":"State change: branch → spira/sp-lint-mix-ev","status":"closed","issue_type":"event","labels":[],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-lint-mix-brgood","title":"branch label names itself","status":"open","issue_type":"task","labels":["repo:spira","plan","branch:spira/sp-lint-mix-brgood"],"updated_at":"2026-09-25T00:00:00Z","description":"Why.\n\n## Build\n\nThe change, in spira/x.\n"}
{"id":"sp-lint-mix-brbad","title":"branch label names another bead","status":"open","issue_type":"task","labels":["repo:spira","plan","branch:spira/sp-lint-mix-brgood"],"updated_at":"2026-09-25T00:00:00Z","description":"Why.\n\n## Build\n\nThe change, in spira/x.\n"}
{"id":"sp-lint-spec-none","title":"builder bead, outcome only","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-10-01T00:00:00Z","description":"A routine deploy causes no failed requests, whatever the mechanism."}
{"id":"sp-lint-spec-empty","title":"builder bead, empty Build section","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-10-01T00:00:00Z","description":"Why.\n\n## Build\n\n## Acceptance\nx\n"}
{"id":"sp-lint-spec-done","title":"builder bead already closed without a spec","status":"closed","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-10-01T00:00:00Z"}
{"id":"sp-lint-spec-noloop","title":"no-loop bead is not builder work","status":"open","issue_type":"task","labels":["repo:spira","no-loop"],"updated_at":"2026-10-01T00:00:00Z","description":"x"}
JSONL

run_lint --all
ALL_OUT="$LINT_OUT"
wantrc "mixed --all exits 1 (offenders present)"         "1" "$LINT_RC"
nowant "--all does not report the claimable bead"        "sp-lint-mix-claim"  "$LINT_OUT"
nowant "--all does not report the no-loop bead"           "sp-lint-mix-noloop"  "$LINT_OUT"
want   "--all reports the no-partition bead"               "sp-lint-mix-nopart: no partition label" "$LINT_OUT"
want   "--all reports the no-repo bead"                     "sp-lint-mix-norepo: no repo: label"     "$LINT_OUT"
nowant "--all does not report the event bead"              "sp-lint-mix-ev" "$LINT_OUT"
nowant "--all does not report the self-named branch label" "sp-lint-mix-brgood: branch:" "$LINT_OUT"
want   "--all reports the branch label naming another bead" \
       "sp-lint-mix-brbad: branch: label names sp-lint-mix-brgood, not itself" "$LINT_OUT"

want   "--all reports an outcome-only builder bead" "sp-lint-spec-none: no build spec" "$ALL_OUT"
want   "--all reports a builder bead whose Build section is empty" "sp-lint-spec-empty: no build spec" "$ALL_OUT"
nowant "--all does not report a builder bead that has a Build section" "sp-lint-mix-claim: no build spec" "$ALL_OUT"
nowant "--all does not report a closed spec-less bead" "sp-lint-spec-done: no build spec" "$ALL_OUT"
nowant "--all does not report a bead outside the builder partition" "sp-lint-spec-noloop: no build spec" "$ALL_OUT"

run_lint sp-lint-spec-none
wantrc "a spec-less builder bead exits 1 alone" "1" "$LINT_RC"
run_lint sp-lint-mix-claim
wantrc "a builder bead with a Build section passes alone (positive control)" "0" "$LINT_RC"

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

tl_summary
