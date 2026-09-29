#!/usr/bin/env bash
#
# test-rapid-recur.sh — rapid_recur_streak / rapid_recur_check (lib.sh): three consecutive
# sub-10s aeon summons on the same bead are a setup loop that recurs identically on every
# retry (law-a-retry-must-change-an-input), so the bead is parked with SPIRA_ASK_LABEL and
# overseer instead of re-summoned forever.
#
# Rehomed from test-aeon-teardown-e2e.sh (sp-5t53s), cut there for the area's 60s cap. The
# case never needed a live aeon.sh session — only a ledger file and one real bd bead for
# the label/note side effects — so it runs here without git, a claude shim or the shared
# full-aeon fixture.
#
# defect: sp-fmvtv
# tier: T1
# covers: spira/lib.sh aeon/src/* UC-aeon-execution-11
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-rapid-recur
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up rapidrecur || {
    printf 'SKIP test-rapid-recur: could not build fixture database\n' >&2
    exit 77
}

# Non-default, explicit runtime root (law-probe-a-fixture-not-production): lib.sh writes
# here on source, and this must never be the real installed $SPIRA_RUN.
export SPIRA_INSTANCE=rapidrecur SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
. "$HERE/lib.sh"

seed() {   # seed <id>
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z"}\n' "$1" | testdb_seed
}
labels() { bd -C "$SPIRA_DB" label list "$1" 2>/dev/null | tr '\n' ' '; }
notes()  { bd -C "$SPIRA_DB" show "$1" 2>/dev/null | tr '\n' ' '; }
done_line() {   # done_line <fayth> <bead> <wall_s>
    printf '2026-09-27T00:00:00Z done %s %s rc=0 status=unlanded wall_s=%s api_s=1 turns=1 in_tok=1 cache_read_tok=0 out_tok=1 think_tok=0 cost_usd=0.01\n' \
        "$1" "$2" "$3"
}

echo "test-rapid-recur.sh"

# ======================================================================================
echo
echo "rapid_recur_streak — pure arithmetic, no ledger file, no bd:"
# ======================================================================================

streak() { printf '%s\n' "$@" | rapid_recur_streak; }

is "three consecutive sub-10s lines streak to 3" "3" \
    "$(streak "$(done_line f b 1)" "$(done_line f b 2.5)" "$(done_line f b 9.9)")"

is "a wall_s=? line still counts (missing spend, never a free pass)" "2" \
    "$(streak "$(done_line f b '?')" "$(done_line f b '?')")"

is "a real run (wall_s>=10) resets the streak to 0" "0" \
    "$(streak "$(done_line f b 1)" "$(done_line f b 90)")"

is "the streak resets AFTER the real run, not before it" "1" \
    "$(streak "$(done_line f b 90)" "$(done_line f b 1)")"

# ======================================================================================
echo
echo "rapid_recur_check — the ledger + bd wiring around that arithmetic:"
# ======================================================================================

testdb_reset
seed sp-rr1
LEDGER="$TMP/aeon-ledger.log"; > "$LEDGER"
FAYTH=testfayth

done_line "$FAYTH" sp-rr1 1 >> "$LEDGER"
done_line "$FAYTH" sp-rr1 2 >> "$LEDGER"
BEAD_ID=sp-rr1 LEDGER="$LEDGER" rapid_recur_check
nowant "after only 2 sub-10s runs, not parked" "${SPIRA_ASK_LABEL}" "$(labels sp-rr1)"

done_line "$FAYTH" sp-rr1 3 >> "$LEDGER"
BEAD_ID=sp-rr1 LEDGER="$LEDGER" rapid_recur_check
want "the third sub-10s run parks with the ask label" "${SPIRA_ASK_LABEL}" "$(labels sp-rr1)"
want "and with overseer"                               "overseer"          "$(labels sp-rr1)"
want "and the note names RAPID-RECUR"                   "RAPID-RECUR"       "$(notes sp-rr1)"

# Idempotent: builder.fayth excludes SPIRA_ASK_LABEL, so a fourth summon must not re-note —
# an annotation left dispatch free to keep re-summoning into the same fault, re-appending
# the same note forever.
done_line "$FAYTH" sp-rr1 1 >> "$LEDGER"
BEAD_ID=sp-rr1 LEDGER="$LEDGER" rapid_recur_check
_n_notes="$(notes sp-rr1 | grep -oc 'RAPID-RECUR' || true)"
is "the note is not re-appended once parked" "1" "${_n_notes:-0}"

# Positive control: a bead that never streaks sub-10s runs is never parked.
seed sp-rr2
done_line "$FAYTH" sp-rr2 90 >> "$LEDGER"
done_line "$FAYTH" sp-rr2 90 >> "$LEDGER"
done_line "$FAYTH" sp-rr2 90 >> "$LEDGER"
BEAD_ID=sp-rr2 LEDGER="$LEDGER" rapid_recur_check
nowant "a bead with three real (wall_s>=10) runs is never parked" "${SPIRA_ASK_LABEL}" "$(labels sp-rr2)"

tl_summary
