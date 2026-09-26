#!/usr/bin/env bash
#
# test-groomer-unpoison.sh — groomer.sh unpoison: lift spira-poison when the charge was the
#   harness's fault, crediting the attempt instead of erasing it silently.
#
#   ./test-groomer-unpoison.sh
#
# WHAT THIS SUITE IS GUARDING (sp-0qp7s task 3)
# ----------------------------------------------
# The groomer needs one tool that turns "I judged this attempt was the harness's fault, not
# the work's" into a durable record — never a bare label removal, which is indistinguishable
# from an amnesty nobody can audit. Three properties, each a case below:
#
#   1. REFUSES without --cause and REFUSES without --evidence (positive control: a script
#      with no case handler for 'unpoison' would fall through to usage/exit 1 too, so the
#      refusal must ALSO prove the right bd calls never happened).
#   2. REFUSES a bead that does not carry spira-poison — nothing to lift.
#   3. On a poisoned bead with both flags: writes a requeued/unjudged-<cause> event (the
#      same vocabulary session_outcome already charges attempts through), removes
#      spira-poison, writes a poison.cleared event (bump_poison_cleared — sp-qd2ul: a clear
#      that leaves no trace is undone by the very next CHECK 4 pass reading the same count
#      against a bare label), and writes a note naming the cause and the evidence.
#
# STUB BD (law-gates-run-in-a-clean-environment): unpoison's job is to make specific bd
# calls with the right arguments in the right order; a real Dolt server would test bd's own
# correctness, already covered where bump_requeue/bump_poison_cleared are tested against a
# real store (test-attempts.sh, test-poison.sh). The stub also answers `label list` so the
# poisoned/not-poisoned branches are exercised without a database.
#
# tier: T1
# covers: spira/groomer.sh spira/lib.sh spira/conf.sh
# defect: sp-0qp7s
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

GROOMSH="$HERE/groomer.sh"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
NONE="$T/none.conf"
RUN="$T/run"; mkdir -p "$RUN"

# Bead ids starting with "poisoned-" carry spira-poison; anything else does not. The stub
# also answers `sql` (bump_requeue/bump_poison_cleared) and `note`/`label remove` by just
# recording them, same as test-groomer.sh's stub.
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
        bash "$GROOMSH" "$@" 2>&1
}

echo "test-groomer-unpoison.sh"

# ==========================================================================================
echo
echo "POSITIVE CONTROL: unpoison without --cause is refused (exits 1, bd never called)"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_groomer unpoison poisoned-1 --evidence 'pre-session death, no work attempted')"; rc=$?
is   "no --cause exits 1"          1        "$rc"
want "error mentions --cause"      "--cause" "$out"
is   "bd not called when --cause missing" "" "$(cat "$BD_LOG" 2>/dev/null)"

# ==========================================================================================
echo
echo "unpoison without --evidence is refused (exits 1, bd never called)"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_groomer unpoison poisoned-1 --cause pre-session-death)"; rc=$?
is   "no --evidence exits 1"        1           "$rc"
want "error mentions --evidence"    "--evidence" "$out"
is   "bd not called when --evidence missing" "" "$(cat "$BD_LOG" 2>/dev/null)"

# ==========================================================================================
echo
echo "unpoison on a bead that does not carry spira-poison is refused (exits 1)"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_groomer unpoison clean-bead --cause pre-session-death --evidence 'nothing to lift')"; rc=$?
is   "not-poisoned bead exits 1"          1                  "$rc"
want "error mentions spira-poison"        "spira-poison"     "$out"
# label list IS called (that is how it learned the bead is clean) but nothing else is.
nowant "no label remove issued"  "label remove"  "$(cat "$BD_LOG")"
nowant "no sql event issued"     " sql "         "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "unpoison on a poisoned bead with --cause and --evidence: full credit trail"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_groomer unpoison poisoned-1 --cause yield-headless --evidence 'the session backgrounded a test batch and ended its turn; the harness, not the work, ended the attempt')"; rc=$?
log="$(cat "$BD_LOG")"

is   "unpoison exits 0"                       0                 "$rc"
want "output confirms the lift"               "UNPOISONED poisoned-1 cause=yield-headless" "$out"
want "an events row credits unjudged-<cause>" "unjudged-yield-headless" "$log"
want "spira-poison label is removed"          "label remove poisoned-1 spira-poison" "$log"
want "a poison.cleared event is written"      "poison.cleared" "$log"
want "a note names the cause"                 "note poisoned-1" "$log"
want "the note text names the cause"          "yield-headless"  "$log"
want "the note carries the evidence"          "backgrounded a test batch" "$log"

echo
tl_summary
