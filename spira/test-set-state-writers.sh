#!/usr/bin/env bash
#
# test-set-state-writers.sh — Every harness path that sets a dimension writes it with
# bd set-state, not bd label add or a constructed dim:val string. Verified by static
# scan of source files and by live exercise of the set-state guarantee.
#
#   ./test-set-state-writers.sh
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
# THE FUNCTIONAL TEST calls set-state twice on the same dimension and asserts one label
# remains, not two, and that each call left an event bead. A test that passed on unfixed
# code with label add would have proved nothing about atomicity; this one would have shown
# two labels (the defect) where one was expected.
#
# covers: spira/*.sh
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "$2"; }
eq()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

. "$HERE/testdb.sh"
testdb_require test-set-state-writers
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'testdb_drop; rm -rf "$TMP"; exit 130' INT
trap 'testdb_drop; rm -rf "$TMP"; exit 143' TERM
testdb_up setstate || { echo "test-set-state-writers: could not build fixture database"; exit 1; }

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

# ---------------------------------------------------------------------------
# Functional — single-valuedness.
# bd set-state removes the previous dim:val label before adding the new one.
# Two successive calls must leave exactly one label for the dimension.
# ---------------------------------------------------------------------------
echo
echo "set-state: two calls to the same dimension leave exactly one label:"
b="$(bd -C "$SPIRA_DB" create "set-state atomicity test" -l plan --silent 2>/dev/null | tr -d '[:space:]')"
[ -n "$b" ] || { bad "fixture bead created" "bd create returned nothing"; \
                 printf 'test-set-state-writers.sh: %d passed, %d failed\n' "$pass" "$fail"; exit 1; }
ok "fixture bead created ($b)"

bd -C "$SPIRA_DB" set-state "$b" "branch=feat/first"  --reason "first"  >/dev/null 2>&1
v1="$(bd -C "$SPIRA_DB" state "$b" branch 2>/dev/null)"
eq "after first set-state: branch dimension is feat/first" "feat/first" "$v1"

bd -C "$SPIRA_DB" set-state "$b" "branch=feat/second" --reason "second" >/dev/null 2>&1
v2="$(bd -C "$SPIRA_DB" state "$b" branch 2>/dev/null)"
eq "after second set-state: branch dimension is feat/second" "feat/second" "$v2"

branch_count="$(bd -C "$SPIRA_DB" show "$b" --json 2>/dev/null \
    | python3 -c 'import json,sys
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
L=(d[0].get("labels") or []) if d else []
print(sum(1 for l in L if l.startswith("branch:")))')"
eq "exactly one branch: label after two set-state calls" "1" "$branch_count"

# ---------------------------------------------------------------------------
# Functional — event trail.
# bd set-state creates an event bead per call; its id is <parent>.<n>.
# Two calls on bead $b produce sp-<suffix>.1 and sp-<suffix>.2.
# ---------------------------------------------------------------------------
echo
echo "set-state: each call creates an event bead:"
event_count="$(bd -C "$SPIRA_DB" list --all --json 2>/dev/null \
    | python3 -c "import json,sys
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
parent='$b'
print(sum(1 for i in d if (i.get('id') or '').startswith(parent + '.')))")"
eq "two set-state calls on $b leave two event beads" "2" "$event_count"

echo
printf 'test-set-state-writers.sh: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
