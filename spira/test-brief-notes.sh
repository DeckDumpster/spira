#!/usr/bin/env bash
#
# test-brief-notes.sh — bound_bead_notes (lib.sh) caps what an aeon brief carries of a
#   recurring incident's notes.
#
# THE CASE THIS REPRODUCES (sp-n3m6k). incident.sh appends one "Recurrence N at
# <timestamp>." note per recurrence and never trims. sp-kogm reached 404 of them, ~216k
# tokens — over the context window of every aeon summoned to work it, so the bead could
# never be worked at all. bound_bead_notes keeps the newest few recurrences verbatim and
# folds everything older into a one-line count; a notes blob with no recurrence markers
# falls back to a plain tail-truncation so an ordinary bead's brief is never touched.
#
# PAIRS (law-absence-needs-a-positive-control): every "this is gone" assertion is paired
# with a "this is still here" one on the same input, and T1/T5 first prove the input
# actually exceeds the bound before trusting that the output does not.
#
# defect: sp-n3m6k
# tier: T1
# covers: spira/lib.sh spira/aeon.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# bbn <keep> <max-chars> <text> -> bound_bead_notes's own stdout, in a minimal environment.
# <text> goes through a file, not argv or a here-string: a real bd show of a
# thousand-note bead is well past Linux's ~128KB single-argument limit (T5 hit exactly
# this as "Argument list too long"), and a here-string appends a newline the input may
# not have had, which a byte-for-byte pass-through assertion (T3, T4) would then fail on.
bbn() {
    local keep="$1" max="$2" f="$TMP/bbn-in-$$-$RANDOM.txt"
    printf '%s' "$3" > "$f"
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_RUN="$TMP/run" \
        bash -c '. "$1"/lib.sh; bound_bead_notes "$2" "$3" < "$4"' \
        _ "$HERE" "$keep" "$max" "$f"
}

# A synthetic bd-show-shaped blob: N "Recurrence <i> at ..." notes between a NOTES header
# and a LABELS: line, matching what `bd show` actually renders (verified against a real
# fixture bead below in T5).
synth_notes() {   # synth_notes <n> -> full bd-show-shaped text
    local n="$1" i body="DESCRIPTION
  (none)

NOTES
"
    for i in $(seq 1 "$n"); do
        body+="  Recurrence $i at 2026-09-15T00:00:00Z.
  Oldest unsent branch: ${i}h — threshold is 24h
"
    done
    body+="
LABELS: spira, incident
"
    printf '%s' "$body"
}

# ===========================================================================================
echo
echo "T1: 1000 recurrence notes — non-default keep/max, pinned so the shipped defaults"
echo "    could not pass this by accident"
# ===========================================================================================

RAW="$(synth_notes 1000)"
want  "T1 setup: the raw input actually exceeds the bound (positive control)" \
      "Recurrence 1 at" "$RAW"
[ "${#RAW}" -gt 3000 ] && ok "T1 setup: raw input is over 3000 chars" \
    || bad "T1 setup: raw input is over 3000 chars" "len=${#RAW}"

OUT="$(bbn 3 3000 "$RAW")"
[ "${#OUT}" -le 3000 ] && ok "T1: bounded output is under the configured 3000-char cap" \
    || bad "T1: bounded output is under the configured 3000-char cap" "len=${#OUT}"
want   "T1: the newest recurrence (1000) survives verbatim" "Recurrence 1000 at" "$OUT"
want   "T1: the 3rd-newest kept recurrence (998) survives"  "Recurrence 998 at"  "$OUT"
nowant "T1: the oldest recurrence (1) is gone, not merely reordered" "Recurrence 1 at 2026" "$OUT"
nowant "T1: a mid-range folded recurrence (500) is gone" "Recurrence 500 at" "$OUT"
want   "T1: the fold banner names the omitted range" "recurrences 1..997" "$OUT"

# ===========================================================================================
echo
echo "T2: no recurrence markers — falls back to plain tail-truncation"
# ===========================================================================================

PROSE_RAW="DESCRIPTION
  (none)

NOTES
  $(python3 -c "print('OLDEST-MARKER ' + 'x'*4000 + ' NEWEST-MARKER')")

LABELS: spira
"
OUT2="$(bbn 5 100 "$PROSE_RAW")"
[ "${#OUT2}" -le 200 ] && ok "T2: ordinary prose notes are truncated to the char cap" \
    || bad "T2: ordinary prose notes are truncated to the char cap" "len=${#OUT2}"
want   "T2: the tail (newest content) survives truncation" "NEWEST-MARKER" "$OUT2"
nowant "T2: the head (oldest content) is cut" "OLDEST-MARKER" "$OUT2"

# ===========================================================================================
echo
echo "T3: notes already under both thresholds pass through untouched"
# ===========================================================================================

SMALL="$(synth_notes 2)"
OUT3="$(bbn 5 8000 "$SMALL")"
is "T3: a small recurring bead's notes are not rewritten at all" "$SMALL" "$OUT3"

# ===========================================================================================
echo
echo "T4: a bead with no NOTES section at all is passed through unchanged"
# ===========================================================================================

NONOTES="DESCRIPTION
  (none)

LABELS: spira"
OUT4="$(bbn 5 10 "$NONOTES")"
is "T4: text with no NOTES header is returned as-is" "$NONOTES" "$OUT4"

# ===========================================================================================
echo
echo "T5: against a real bd fixture (spira/testdb.sh), the bead's own acceptance test —"
echo "    1000 recurrence notes render a brief under a fixed size, using the shipped defaults"
# ===========================================================================================

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
# EXITS 77 FOR THE WHOLE SUITE WHEN NO bd ENGINE IS USABLE HERE (its own diagnostic on
# stderr) — the same convention test-attempts.sh and others use after cases already ran;
# T1-T4 above needed no database and already recorded their own verdicts either way.
testdb_require test-brief-notes
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up briefnotes || bail "T5: could not build the fixture database"

ID="$(bd -C "$SPIRA_DB" create "recurrence fixture" -t task --json 2>/dev/null \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
[ -n "$ID" ] || bail "T5: fixture bead was not created"

python3 -c '
n = 1000
parts = []
for i in range(1, n + 1):
    parts.append("Recurrence %d at 2026-09-15T00:00:00Z.\nOldest unsent branch: %dh — threshold is 24h\n" % (i, i))
print("".join(parts), end="")
' > "$TMP/notes1000.txt"
bd -C "$SPIRA_DB" update "$ID" --notes "$(cat "$TMP/notes1000.txt")" >/dev/null 2>&1

RAW5="$(bd -C "$SPIRA_DB" show "$ID" 2>/dev/null)"
[ "${#RAW5}" -gt 50000 ] && ok "T5 setup: the real fixture's raw brief is itself huge" \
    || bad "T5 setup: the real fixture's raw brief is itself huge" "len=${#RAW5}"

# The shipped defaults (conf.sh), not test-chosen numbers — this is the bead's own
# done-when: a bead carrying 1000 recurrence notes renders a brief under a fixed size.
# RAW5 goes through a file, not argv: it is well past Linux's ~128KB single-argument
# limit ("Argument list too long" is exactly what passing it as "$2" produces).
printf '%s' "$RAW5" > "$TMP/raw5.txt"
OUT5="$(env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_RUN="$TMP/run" \
    bash -c '. "$1"/conf.sh; . "$1"/lib.sh; bound_bead_notes "$SPIRA_BRIEF_KEEP_RECURRENCES" "$SPIRA_BRIEF_NOTES_MAX_CHARS" < "$2"' \
    _ "$HERE" "$TMP/raw5.txt")"

[ "${#OUT5}" -lt 20000 ] && ok "T5: a real 1000-recurrence bead's brief is under 20000 chars" \
    || bad "T5: a real 1000-recurrence bead's brief is under 20000 chars" "len=${#OUT5}"
want   "T5: the newest recurrence still reaches the aeon" "Recurrence 1000 at" "$OUT5"
nowant "T5: the first recurrence's prose does not"        "Oldest unsent branch: 1h "  "$OUT5"

# ===========================================================================================
echo
echo "T6: aeon.sh actually calls bound_bead_notes when it builds BEAD_BODY (delivery fence)"
# ===========================================================================================

want "T6: aeon.sh's BEAD_BODY is passed through bound_bead_notes" \
     'BEAD_BODY="$(bound_bead_notes' "$(cat "$HERE/aeon.sh")"

tl_summary
