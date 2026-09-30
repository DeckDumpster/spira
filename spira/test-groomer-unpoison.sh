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
#   2. REFUSES a bead spira-claim does not clear (not poisoned: spira-claim prints SKIP, not
#      OK) — groomer exits 1.
#   3. On a poisoned bead with both flags: delegates to `spira-claim unpoison` with
#      --credit <cause>, --actor groomer and the evidence in --cause (spira-claim/DESIGN.md
#      §8.6 item 4). The write trail itself (unjudged credit, poison.cleared floor, note,
#      ask) is `cargo test -p spira-claim`.
#
# STUB spira-claim (SPIRA_CLAIM_BIN): records its argv and answers OK/SKIP by bead id, so
# the suite needs no database and no lifecycle machine. The refusal cases also prove the
# stub was never called.
#
# tier: T1
# covers: spira/groomer.sh spira/conf.sh spira-claim/*
# defect: sp-0qp7s
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
NONE="$T/none.conf"
RUN="$T/run"; mkdir -p "$RUN"

# Bead ids starting with "poisoned-" are poisoned; anything else is not. The stub bd records
# any call (none are expected: groomer delegates everything to spira-claim).
STUB_BD="$T/stub-bd"
BD_LOG="$T/bd.log"
cat > "$STUB_BD" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$BD_LOG_PATH"
exit 0
STUB
chmod +x "$STUB_BD"

STUB_DIR="$T/stubbin"; mkdir -p "$STUB_DIR"
STUB_CLAIM="$STUB_DIR/spira-claim"   # found by name, first on run_groomer's PATH (sp-gypjk)
CLAIM_LOG="$T/claim.log"
cat > "$STUB_CLAIM" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$CLAIM_LOG_PATH"
bead=""; while [ $# -gt 0 ]; do [ "$1" = "--bead" ] && bead="$2"; shift; done
case "$bead" in
    poisoned-*) printf 'OK   %s: cleared (stub)\n' "$bead" ;;
    *)          printf 'SKIP %s: not poisoned (stub)\n' "$bead" ;;
esac
exit 0
STUB
chmod +x "$STUB_CLAIM"

run_groomer() {
    env -i HOME="$T" PATH="$STUB_DIR:$HERE:/usr/bin:/bin" \
        SPIRA_CONF="$NONE" \
        SPIRA_BD="$STUB_BD" \
        BD_LOG_PATH="$BD_LOG" \
        CLAIM_LOG_PATH="$CLAIM_LOG" \
        SPIRA_DB="$T/fixture.db" \
        SPIRA_RUN="$RUN" \
        groomer.sh "$@" 2>&1
}

echo "test-groomer-unpoison.sh"

# ==========================================================================================
echo
echo "POSITIVE CONTROL: unpoison without --cause is refused (exits 1, bd never called)"
# ==========================================================================================
: > "$BD_LOG"; : > "$CLAIM_LOG"
out="$(run_groomer unpoison poisoned-1 --evidence 'pre-session death, no work attempted')"; rc=$?
is   "no --cause exits 1"          1        "$rc"
want "error mentions --cause"      "--cause" "$out"
is   "bd not called when --cause missing" "" "$(cat "$BD_LOG" 2>/dev/null)"
is   "spira-claim not called when --cause missing" "" "$(cat "$CLAIM_LOG" 2>/dev/null)"

# ==========================================================================================
echo
echo "unpoison without --evidence is refused (exits 1, bd never called)"
# ==========================================================================================
: > "$BD_LOG"; : > "$CLAIM_LOG"
out="$(run_groomer unpoison poisoned-1 --cause pre-session-death)"; rc=$?
is   "no --evidence exits 1"        1           "$rc"
want "error mentions --evidence"    "--evidence" "$out"
is   "bd not called when --evidence missing" "" "$(cat "$BD_LOG" 2>/dev/null)"
is   "spira-claim not called when --evidence missing" "" "$(cat "$CLAIM_LOG" 2>/dev/null)"

# ==========================================================================================
echo
echo "unpoison on a bead spira-claim does not clear is refused (exits 1)"
# ==========================================================================================
: > "$BD_LOG"; : > "$CLAIM_LOG"
out="$(run_groomer unpoison clean-bead --cause pre-session-death --evidence 'nothing to lift')"; rc=$?
is   "not-poisoned bead exits 1"          1                  "$rc"
want "error says it was not cleared"      "was not cleared"  "$out"
want "spira-claim was asked"              "unpoison --bead clean-bead" "$(cat "$CLAIM_LOG")"
nowant "no UNPOISONED line"               "UNPOISONED"       "$out"

# ==========================================================================================
echo
echo "unpoison on a poisoned bead with --cause and --evidence: delegates to spira-claim"
# ==========================================================================================
: > "$BD_LOG"; : > "$CLAIM_LOG"
out="$(run_groomer unpoison poisoned-1 --cause yield-headless --evidence 'the session backgrounded a test batch and ended its turn; the harness, not the work, ended the attempt')"; rc=$?
log="$(cat "$CLAIM_LOG")"

is   "unpoison exits 0"                       0                 "$rc"
want "output confirms the lift"               "UNPOISONED poisoned-1 cause=yield-headless" "$out"
want "spira-claim unpoison names the bead"    "unpoison --bead poisoned-1" "$log"
want "the cause is credited"                  "--credit yield-headless" "$log"
want "the actor is groomer"                   "--actor groomer" "$log"
want "--cause carries the cause"              "--cause yield-headless:" "$log"
want "--cause carries the evidence"           "backgrounded a test batch" "$log"
is   "groomer itself writes nothing to bd"    "" "$(cat "$BD_LOG")"

echo
tl_summary
