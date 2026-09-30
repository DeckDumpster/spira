#!/usr/bin/env bash
#
# test-groomer-poison-triage.sh — groomer.sh triage-poison: the other side of poison
#   triage from `unpoison`, for the charge that WAS the work's fault.
#
# WHAT THIS SUITE IS GUARDING (sp-iruqq)
# ----------------------------------------------
# A poisoned bead admits no claim (aeon.sh refuses it at claim time). Before this suite,
# a WORK'S-FAULT poison verdict had no tool of its own: the groomer's only options were
# `unpoison` (which credits the harness — wrong when the charge really was the work's
# fault) or leaving a bare note ("poison stands, next claim must fix X") while the label
# stayed on. That note strands the bead forever, because nothing can ever BE the next
# claim while poison forbids every claim.
#
# `triage-poison --verdict work-fault` is the fix: a poisoned bead run through it ends
# claimable, with the triage recorded as the reason the next claim must act on. A poisoned
# bead run through `--verdict drop` ends dropped instead — the one shape where leaving
# poison in place is fine, because the bead is closed and will never be claimed again.
#
# STUB BD (law-gates-run-in-a-clean-environment): triage-poison's job is to make specific
# bd calls with the right arguments; bump_poison_cleared/bump_requeue are already tested
# against a real store (test-attempts.sh, test-poison.sh).
#
# tier: T1
# covers: spira/groomer.sh spira/lib.sh spira/conf.sh spira/chamber/groomer.md
# defect: sp-iruqq
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
NONE="$T/none.conf"
RUN="$T/run"; mkdir -p "$RUN"

# Bead ids starting with "poisoned-" carry spira-poison; anything else does not.
STUB_BD="$T/stub-bd"
BD_LOG="$T/bd.log"
cat > "$STUB_BD" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$BD_LOG_PATH"
if [ "$1" = "-C" ]; then shift 2; fi
if [ "$1" = "label" ] && [ "$2" = "list" ]; then
    bead_id="$3"
    case "$bead_id" in
        poisoned-*) printf '  - plan\n  - spira-poison\n' ;;
        *)          printf '  - plan\n' ;;
    esac
    exit 0
fi
exit 0
STUB
chmod +x "$STUB_BD"

run_groomer() {
    env -i HOME="$T" PATH="$HERE:/usr/bin:/bin" \
        SPIRA_CONF="$NONE" \
        SPIRA_BD="$STUB_BD" \
        BD_LOG_PATH="$BD_LOG" \
        SPIRA_DB="$T/fixture.db" \
        SPIRA_RUN="$RUN" \
        groomer.sh "$@" 2>&1
}

echo "test-groomer-poison-triage.sh"

# ==========================================================================================
echo
echo "triage-poison refuses without --verdict"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_groomer triage-poison poisoned-1 --evidence 'too large, split in two')"; rc=$?
is   "no --verdict exits 1"        1          "$rc"
want "error mentions --verdict"    "--verdict" "$out"

# ==========================================================================================
echo
echo "triage-poison refuses an unknown --verdict"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_groomer triage-poison poisoned-1 --verdict maybe --evidence 'unsure')"; rc=$?
is   "unknown verdict exits 1"     1              "$rc"
want "error names work-fault/drop" "work-fault or drop" "$out"

# ==========================================================================================
echo
echo "triage-poison refuses without --evidence"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_groomer triage-poison poisoned-1 --verdict work-fault)"; rc=$?
is   "no --evidence exits 1"       1           "$rc"
want "error mentions --evidence"   "--evidence" "$out"
is   "bd not called when --evidence missing" "" "$(cat "$BD_LOG" 2>/dev/null)"

# ==========================================================================================
echo
echo "triage-poison --verdict work-fault on a bead that does not carry spira-poison is refused"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_groomer triage-poison clean-bead --verdict work-fault --evidence 'nothing to triage')"; rc=$?
is     "not-poisoned bead exits 1"   1              "$rc"
want   "error mentions spira-poison" "spira-poison" "$out"
nowant "no label remove issued"      "label remove" "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "triage-poison --verdict work-fault on a poisoned bead: lifts poison, does not credit"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_groomer triage-poison poisoned-strand --verdict work-fault --evidence 'too large — split into the parse half and the render half; the fix is splitting it')"; rc=$?
log="$(cat "$BD_LOG")"

is     "triage-poison exits 0"                  0                  "$rc"
want   "output confirms the triage"             "TRIAGED poisoned-strand verdict=work-fault" "$out"
want   "a poison.cleared event floors attempts" "poison.cleared"   "$log"
want   "spira-poison label is removed"          "label remove poisoned-strand spira-poison" "$log"
nowant "no unjudged credit is written (it WAS the work's fault)" "unjudged" "$log"
want   "a note names the triage"                "note poisoned-strand" "$log"
want   "the note says WORK'S FAULT"             "WORK'S FAULT"     "$log"
want   "the note carries the fix as the cause"  "split into the parse half" "$log"
want   "the note says the fix is the next claim" "the next claim"  "$log"

# The bead this ran against is now claimable: nothing in this run re-adds spira-poison.
nowant "poison is never re-added" "label add poisoned-strand spira-poison" "$log"

# ==========================================================================================
echo
echo "triage-poison --verdict drop: closes the bead and labels it spira-dropped"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_groomer triage-poison poisoned-litter --verdict drop --evidence 'fixture-shaped, filed by mistake, not worth fixing')"; rc=$?
log="$(cat "$BD_LOG")"

is   "triage-poison drop exits 0"        0                      "$rc"
want "output confirms DROPPED"           "DROPPED poisoned-litter" "$out"
want "the bead is closed"                "close poisoned-litter"   "$log"
want "the bead is labeled spira-dropped" "label add poisoned-litter spira-dropped" "$log"

echo
tl_summary
