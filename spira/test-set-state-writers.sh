#!/usr/bin/env bash
#
# test-set-state-writers.sh — no harness path writes a dimension label with bd label add
# or embeds one in SPIRA_INCIDENT_LABELS; bd set-state is the only writer.
#
# WHAT THIS GUARDS. bd set-state does three things that label add cannot do: it removes
# the previous value atomically (single-valuedness), it writes an event bead as the source
# of truth, and it provides a typed read-path through bd state. A writer that constructs
# "dim:val" and calls label add bypasses all three — two successive writes leave two labels,
# the event trail has a gap, and readers parsing the label list see an ambiguous result.
#
# THE STATIC SCAN PLANTS AN OFFENDER FIRST (law-absence-needs-a-positive-control). An empty
# result from a scanner that never finds anything looks identical to an empty result from a
# scanner that found nothing — the only distinguishing fact is a positive control you planted.
#
# bd set-state's own semantics (atomicity, event trail) are exercised on a real database in
# test-set-state-semantics.sh (T2), not here — this file is a pure lint and needs no fixture.
#
# tier: T0
# covers: spira/*.sh UC-safety-fences-29
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo "test-set-state-writers.sh"

# ---------------------------------------------------------------------------
# Static scan — no harness file may call `label add` with a dimension label,
# or embed a dimension label in SPIRA_INCIDENT_LABELS.
#
# DIM_RE: a dimension prefix followed by colon, inside a quoted argument.
# "label add" then anything on the same line, then a quote, then the prefix.
# `.*` is safe here — grep processes line-by-line, so it cannot cross newlines.
# ---------------------------------------------------------------------------
DIM_PREFIXES='repo|severity|branch|fayth|lane|gate'
LABEL_ADD_DIM_RE="label add.*[\"'](${DIM_PREFIXES}):"
INC_LABELS_DIM_RE="SPIRA_INCIDENT_LABELS[^=]*=.*,?(${DIM_PREFIXES}):"

echo
echo "positive control — scanner detects known-bad patterns in synthetic files:"

SYN_LA="$TMP/syn-la.sh"
printf 'bdq label add "$BEAD" "branch:$BR"\n' > "$SYN_LA"
hit="$(grep -E "$LABEL_ADD_DIM_RE" "$SYN_LA" 2>/dev/null || true)"
[ -n "$hit" ] && ok "scanner detects: label add with branch: argument" \
               || bad "scanner detects: label add with branch: argument" "no match in synthetic file"

SYN_LA2="$TMP/syn-la2.sh"
printf '"$BD" -C "$DB" label add "$id" "lane:fast"\n' > "$SYN_LA2"
hit2="$(grep -E "$LABEL_ADD_DIM_RE" "$SYN_LA2" 2>/dev/null || true)"
[ -n "$hit2" ] && ok "scanner detects: label add with lane: argument" \
                || bad "scanner detects: label add with lane: argument" "no match in synthetic file"

SYN_INC="$TMP/syn-inc.sh"
printf 'SPIRA_INCIDENT_LABELS="spira,plan,repo:$NAME"\n' > "$SYN_INC"
hit3="$(grep -E "$INC_LABELS_DIM_RE" "$SYN_INC" 2>/dev/null || true)"
[ -n "$hit3" ] && ok "scanner detects: SPIRA_INCIDENT_LABELS with embedded dimension" \
                || bad "scanner detects: SPIRA_INCIDENT_LABELS with embedded dimension" "no match in synthetic file"

echo
echo "no harness file uses label add for a declared dimension:"
la_offenders=0
while IFS= read -r f; do
    hits="$(grep -En "$LABEL_ADD_DIM_RE" "$f" 2>/dev/null || true)"
    if [ -n "$hits" ]; then
        printf '  OFFENDER %s:\n' "$(basename "$f")"
        printf '%s\n' "$hits" | head -5 | sed 's/^/    /'
        la_offenders=$((la_offenders+1))
    fi
done < <(find "$HERE" -maxdepth 1 -name '*.sh' ! -name 'test-set-state-writers.sh' | sort)
[ "$la_offenders" -eq 0 ] \
    && ok "no harness file calls label add with a dimension label" \
    || bad "no harness file calls label add with a dimension label" "$la_offenders offender(s) found (see above)"

echo
echo "no harness file embeds a dimension label in SPIRA_INCIDENT_LABELS:"
inc_offenders=0
while IFS= read -r f; do
    hits="$(grep -En "$INC_LABELS_DIM_RE" "$f" 2>/dev/null || true)"
    if [ -n "$hits" ]; then
        printf '  OFFENDER %s:\n' "$(basename "$f")"
        printf '%s\n' "$hits" | head -5 | sed 's/^/    /'
        inc_offenders=$((inc_offenders+1))
    fi
done < <(find "$HERE" -maxdepth 1 -name '*.sh' ! -name 'test-set-state-writers.sh' | sort)
[ "$inc_offenders" -eq 0 ] \
    && ok "no harness file embeds a dimension label in SPIRA_INCIDENT_LABELS" \
    || bad "no harness file embeds a dimension label in SPIRA_INCIDENT_LABELS" "$inc_offenders offender(s) found (see above)"

tl_summary
