#!/usr/bin/env bash
# test-bead-lint.sh — bead.sh lint reads labels via bd show --json.
#
# covers: spira/bead.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is()     { [ "$2" = "$3" ]       && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-bead-lint
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up bead_lint || { printf 'test-bead-lint: could not build fixture database\n' >&2; exit 1; }

lint() {
    SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
        SPIRA_HOME="$HERE" SPIRA_CONF="$TMP/no.conf" \
        SPIRA_NO_LOOP_LABEL="no-loop" \
        bash "$HERE/bead.sh" lint "$@" 2>&1
}
lint_rc() {
    SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
        SPIRA_HOME="$HERE" SPIRA_CONF="$TMP/no.conf" \
        SPIRA_NO_LOOP_LABEL="no-loop" \
        bash "$HERE/bead.sh" lint "$@" >/dev/null 2>&1; echo $?
}

echo "test-bead-lint.sh"

# -----------------------------------------------------------------------------------------
# FIXTURE. One bead with a repo: label (good), one without (bad).
# -----------------------------------------------------------------------------------------
testdb_seed <<'JSONL'
{"id":"sp-lint-good","title":"good bead","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-09-16T00:00:00Z"}
{"id":"sp-lint-bad","title":"bad bead","status":"open","issue_type":"task","labels":["plan"],"updated_at":"2026-09-16T00:00:00Z"}
{"id":"sp-lint-ev.1","title":"State change: branch → spira/sp-lint-ev","status":"closed","issue_type":"event","labels":[],"updated_at":"2026-09-16T00:00:00Z"}
JSONL

# -----------------------------------------------------------------------------------------
# POSITIVE CONTROL: a bead WITH repo: and partition label passes.
# The suite depends on this to trust any silence from subsequent checks.
# -----------------------------------------------------------------------------------------
out="$(lint sp-lint-good)"
rc="$(lint_rc sp-lint-good)"
is   "good bead passes"            "0"               "$rc"
nowant "good bead not reported"    "no repo: label"  "$out"

# -----------------------------------------------------------------------------------------
# MISSING LABEL: a bead without repo: is reported and exits 1.
# -----------------------------------------------------------------------------------------
out="$(lint sp-lint-bad)"
rc="$(lint_rc sp-lint-bad)"
is   "bad bead exits 1"            "1"                         "$rc"
want "bad bead is reported"        "sp-lint-bad: no repo: label" "$out"

# -----------------------------------------------------------------------------------------
# NON-EXISTENT ID: reported as unreadable, distinctly from a missing label, exits 1.
# -----------------------------------------------------------------------------------------
out="$(lint sp-lint-nosuchbead)"
rc="$(lint_rc sp-lint-nosuchbead)"
is   "nonexistent bead exits 1"    "1"                                 "$rc"
want "nonexistent is unreadable"   "sp-lint-nosuchbead: unreadable"    "$out"
nowant "not reported as no-label"  "no repo: label"                    "$out"

# -----------------------------------------------------------------------------------------
# --all: lints every bead in the database, reports exactly the offender.
# -----------------------------------------------------------------------------------------
out="$(lint --all)"
rc="$(lint_rc --all)"
is   "--all exits 1 (offender present)"   "1"                        "$rc"
want "--all reports the bad bead"         "sp-lint-bad: no repo: label" "$out"
nowant "--all does not report the good one" "sp-lint-good"            "$out"

# -----------------------------------------------------------------------------------------
# EVENT BEAD: event type has no routing obligation; repo: is not required.
# The positive control for the repo check lives above: sp-lint-bad (type=task, no repo)
# still exits 1, proving the check is live.
# -----------------------------------------------------------------------------------------
out="$(lint sp-lint-ev.1)"
rc="$(lint_rc sp-lint-ev.1)"
is   "event bead passes lint"            "0"                              "$rc"
nowant "event bead not reported"         "sp-lint-ev.1: no repo: label"  "$out"

# -----------------------------------------------------------------------------------------
# EVENT BEAD: --all does not flag the event bead either.
# -----------------------------------------------------------------------------------------
out_all="$(lint --all)"
nowant "--all does not flag event bead"  "sp-lint-ev.1"                  "$out_all"

# ==========================================================================================
echo
echo "no-loop: bead with no-loop label passes even without a partition label"
# ==========================================================================================
# POSITIVE CONTROL: the scan must fire on a bead WITHOUT no-loop that lacks partition.
# Without this, a scanner that skips all partition checks reads as correct.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-lint-broken","title":"open work, no partition","status":"open","issue_type":"task","labels":["repo:spira"],"updated_at":"2026-09-16T00:00:00Z"}
JSONL
out="$(lint sp-lint-broken)"
rc="$(lint_rc sp-lint-broken)"
is   "positive control: open bead without partition exits 1"       "1" "$rc"
want "positive control: open bead without partition is reported"   "sp-lint-broken: no partition label" "$out"

# Now verify the intentionally-unclaimable bead is NOT flagged.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-lint-noloop","title":"intentionally unclaimable","status":"open","issue_type":"task","labels":["repo:spira","no-loop"],"updated_at":"2026-09-16T00:00:00Z"}
JSONL
out="$(lint sp-lint-noloop)"
rc="$(lint_rc sp-lint-noloop)"
is   "no-loop bead passes lint"              "0"            "$rc"
nowant "no-loop bead not reported as error"  "partition"    "$out"

# ==========================================================================================
echo
echo "no-loop: a claimable bead (has partition) and a no-loop bead pass; broken one fails"
# ==========================================================================================
# All three in one --all pass: only the broken bead appears in output.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-lint-mix-a","title":"claimable","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-09-16T00:00:00Z"}
{"id":"sp-lint-mix-b","title":"no-loop","status":"open","issue_type":"task","labels":["repo:spira","no-loop"],"updated_at":"2026-09-16T00:00:00Z"}
{"id":"sp-lint-mix-c","title":"broken unclaimable","status":"open","issue_type":"task","labels":["repo:spira"],"updated_at":"2026-09-16T00:00:00Z"}
JSONL
out="$(lint --all)"
rc="$(lint_rc --all)"
is     "mixed --all exits 1"                           "1"              "$rc"
nowant "claimable bead not reported"                   "sp-lint-mix-a"  "$out"
nowant "no-loop bead not reported"                     "sp-lint-mix-b"  "$out"
want   "broken bead reported as no partition"          "sp-lint-mix-c"  "$out"

# ==========================================================================================
echo
echo "zero-labels: open non-event bead with no labels is reported (sp-xrgae)"
# ==========================================================================================
# A bead with an empty label set is unclaimable and unreachable to detect_unclaimable_ready
# unless it appears in the ready set. bead.sh lint must catch it: both "no repo: label" and
# "no partition label" should be reported.
# POSITIVE CONTROL: a bead with proper labels passes to confirm the scanner is live.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-lint-zero","title":"zero labels: filed by bypass path","status":"open","issue_type":"task","labels":[],"updated_at":"2026-09-16T00:00:00Z"}
{"id":"sp-lint-ok2","title":"properly labelled","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-09-16T00:00:00Z"}
JSONL
out="$(lint sp-lint-zero)"
rc="$(lint_rc sp-lint-zero)"
is   "zero-label bead exits 1"               "1"                            "$rc"
want "zero-label bead: no repo: label"       "sp-lint-zero: no repo: label" "$out"
want "zero-label bead: no partition label"   "no partition label"           "$out"

out_ok="$(lint sp-lint-ok2)"
rc_ok="$(lint_rc sp-lint-ok2)"
is   "properly labelled bead passes (positive control)"   "0" "$rc_ok"
nowant "properly labelled bead not reported"              "sp-lint-ok2" "$out_ok"

# ==========================================================================================
echo
echo "branch: label naming ANOTHER bead is flagged; naming itself is not (sp-om71s)"
# ==========================================================================================
# `bd create --parent` copies every label from the parent onto a child, including branch: —
# a child that keeps it names its PARENT's worktree, not its own, and the resume preference
# reads that label first, so a mislabeled bead is claimed ahead of everything else ready.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-lint-br-sib","title":"sibling, owns its own branch","status":"open","issue_type":"task","labels":["repo:spira","plan","branch:spira/sp-lint-br-sib"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-lint-br-bad","title":"child, wrongly recorded onto the sibling's branch","status":"open","issue_type":"task","labels":["repo:spira","plan","branch:spira/sp-lint-br-sib"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-lint-br-good","title":"a bead whose branch: label names itself","status":"open","issue_type":"task","labels":["repo:spira","plan","branch:spira/sp-lint-br-good"],"updated_at":"2026-09-25T00:00:00Z"}
JSONL

# POSITIVE CONTROL: the sibling's own branch: label (naming itself) must NOT be flagged —
# otherwise every correctly-labelled bead in the database would fail lint.
out="$(lint sp-lint-br-sib)"
rc="$(lint_rc sp-lint-br-sib)"
is   "a branch: label naming itself passes (positive control)" "0" "$rc"
nowant "not reported as mislabeled" "branch: label names" "$out"

out="$(lint sp-lint-br-good)"
rc="$(lint_rc sp-lint-br-good)"
is   "another correctly self-named branch: passes"             "0" "$rc"

# THE OFFENDER: sp-lint-br-bad's branch: label names sp-lint-br-sib, an EXISTING different bead.
out="$(lint sp-lint-br-bad)"
rc="$(lint_rc sp-lint-br-bad)"
is   "branch: label naming another bead exits 1"        "1" "$rc"
want "branch: label naming another bead is reported"    "sp-lint-br-bad: branch: label names sp-lint-br-sib, not itself" "$out"

out="$(lint --all)"
nowant "--all does not flag the correctly self-named beads" "sp-lint-br-sib: branch:" "$out"
want   "--all flags the mislabeled child"                   "sp-lint-br-bad: branch: label names sp-lint-br-sib, not itself" "$out"

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
