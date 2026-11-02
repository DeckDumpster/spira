#!/usr/bin/env bash
#
# test-bead-lint-remedy.sh — the open-remedy check and filing a remedy under an incident
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

echo "test-bead-lint-remedy.sh"

# ===========================================================================================
echo
echo "T2: real bd show --json wiring — one fixture, one seed"
# ===========================================================================================
. "$HERE/testdb.sh"
testdb_require test-bead-lint-remedy
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up bead_lint_remedy || { printf 'test-bead-lint: could not build fixture database\n' >&2; exit 1; }

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
{"id":"sp-lint-rem-bad","title":"incident, remedy only related","status":"open","issue_type":"task","labels":["incident-test","spira","repo:spira","no-loop"],"updated_at":"2026-10-01T00:00:00Z","dependencies":[{"issue_id":"sp-lint-rem-bad","depends_on_id":"sp-lint-rem-fix1","type":"relates-to"}]}
{"id":"sp-lint-rem-fix1","title":"remedy one","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-10-01T00:00:00Z","description":"Why.\n\n## Build\n\nThe change, in spira/x.\n"}
{"id":"sp-lint-rem-good","title":"incident, remedy blocks","status":"open","issue_type":"task","labels":["incident-test","spira","repo:spira","no-loop"],"updated_at":"2026-10-01T00:00:00Z","dependencies":[{"issue_id":"sp-lint-rem-good","depends_on_id":"sp-lint-rem-fix2","type":"blocks"}]}
{"id":"sp-lint-rem-fix2","title":"remedy two","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-10-01T00:00:00Z","description":"Why.\n\n## Build\n\nThe change, in spira/x.\n"}
{"id":"sp-lint-rem-done","title":"incident, remedy landed","status":"open","issue_type":"task","labels":["incident-test","spira","repo:spira","no-loop"],"updated_at":"2026-10-01T00:00:00Z","dependencies":[{"issue_id":"sp-lint-rem-done","depends_on_id":"sp-lint-rem-fix3","type":"relates-to"}]}
{"id":"sp-lint-rem-fix3","title":"remedy three, closed","status":"closed","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-10-01T00:00:00Z"}
{"id":"sp-lint-fil-inc","title":"incident to remedy","status":"open","issue_type":"task","labels":["incident-test","spira","repo:spira","no-loop"],"updated_at":"2026-10-01T00:00:00Z"}
{"id":"sp-lint-fil-work","title":"ordinary parent","status":"open","issue_type":"task","labels":["spira","repo:spira","plan"],"updated_at":"2026-10-01T00:00:00Z","description":"Why.\n\n## Build\n\nThe change, in spira/x.\n"}
JSONL

# ===========================================================================================
echo
echo "T5: an incident whose open remedy is linked relates-to only (law-a-bug-with-a-fix-in-flight-depends-on-it)"
# ===========================================================================================

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

printf 'harness | /tmp/harness | push | origin/main | | true | plan\n' > "$TMP/repos.tbl"
file_under() {            # file_under <parent> -> FILE_OUT (new id), FILE_RC
    tl_config SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
        SPIRA_RUNES_DIR="$HERE/runes" SPIRA_INCIDENT_LABEL="incident-test" \
        SPIRA_REPO_MAP="$TMP/repos.tbl"
    printf 'Remedy.\n\n## Build\n\nThe fix, in spira/x.\n' > "$TMP/remedy.md"
    FILE_OUT="$(SPIRA_HOME="$HERE" SPIRA_CONF="$TMP/no.conf" SPIRA_BEAD_LANE_OVERRIDE=1 \
        bead.sh file "remedy for $1" --for builder --repo harness --parent "$1" --body-file "$TMP/remedy.md" 2>"$TMP/file.err")"
    FILE_RC=$?
}
blocks_of() {             # blocks_of <id> -> space-separated ids it blocks-depends on
    SPIRA_DB="$SPIRA_DB" timeout 5 "${SPIRA_BD:-$TESTDB_BD}" -C "$SPIRA_DB" dep list "$1" --type blocks --json 2>/dev/null \
        | python3 -c 'import json,sys; print(" ".join(d.get("depends_on_id") or d.get("id") for d in json.load(sys.stdin)))'
}

file_under sp-lint-fil-inc
wantrc "filing a remedy under an incident exits 0" "0" "$FILE_RC"
want   "the incident now blocks on the new remedy" "$(printf %s "$FILE_OUT" | head -n1)" "$(blocks_of sp-lint-fil-inc)"

file_under sp-lint-fil-work
wantrc "filing under an ordinary bead exits 0 (positive control)" "0" "$FILE_RC"
is     "an ordinary parent gets no blocks edge" "" "$(blocks_of sp-lint-fil-work)"

tl_summary
