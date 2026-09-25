#!/usr/bin/env bash
#
# test-cockpit-strand-keys.sh — strand_keys: SP_STRANDS, SP_STRANDS_ESCALATED,
# SP_STRAND_GHOST and SP_STRAND_OTHER, derived from $SPIRA_RUN/strands.json.
#
# Before this suite, gap #2 of docs/test-plan/cockpit-observability.md: these four keys
# were only ever SEEDED into fixtures (test-cockpit-probe-fault.sh, watchtower.sh's own
# suite) — nothing derived them from a strands.json shaped the way strand.sh actually
# writes it (partition:kind:id -> {first, acted, escalated}), and nothing checked the
# collector's own honest-unknown branches.
#
# Runs the real `cockpit.sh strands` subcommand (registered in collect.sh's PROBES),
# the same seam czar_triggers uses.
#
# tier: T1
# covers: spira/cockpit.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
RUN="$TMP/run"; mkdir -p "$RUN"
BASE_PATH="$PATH"

run_strands() {
    env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
        bash "$HERE/cockpit.sh" strands 2>/dev/null
}

field() { printf '%s\n' "$1" | grep "^$2=" | sed "s/^$2=//"; }

# =============================================================================
# NO FILE AT ALL — strand.sh has never run. Distinct from a clean, empty ledger
# (law-absence-needs-a-positive-control): "0 strands" and "the detector never ran" must
# not read the same.
# =============================================================================
echo "no strands.json: detector has not run -> ? for every key"

rm -f "$RUN/strands.json"
out="$(run_strands)"
is "SP_STRANDS=?"           "?" "$(field "$out" SP_STRANDS)"
is "SP_STRANDS_ESCALATED=?" "?" "$(field "$out" SP_STRANDS_ESCALATED)"
is "SP_STRAND_GHOST=?"      "?" "$(field "$out" SP_STRAND_GHOST)"
is "SP_STRAND_OTHER=?"      "?" "$(field "$out" SP_STRAND_OTHER)"

# =============================================================================
# MALFORMED FILE — present but not parseable JSON. Same honest-unknown answer as no file,
# never a confident zero.
# =============================================================================
echo ""
echo "malformed strands.json: ? for every key, not 0"

printf '{not json' > "$RUN/strands.json"
out="$(run_strands)"
is "SP_STRANDS=? on malformed file"      "?" "$(field "$out" SP_STRANDS)"
is "SP_STRAND_GHOST=? on malformed file" "?" "$(field "$out" SP_STRAND_GHOST)"

# =============================================================================
# POSITIVE CONTROL — a real, clean ledger with no strands. This must read as an honest
# ZERO, not '?': the file exists and parsed, so the detector genuinely found nothing.
# Without this the two '?' cases above could just mean the parser always fails.
# =============================================================================
echo ""
echo "positive control: empty-but-valid ledger -> real 0, not ?"

printf '{}' > "$RUN/strands.json"
out="$(run_strands)"
is "SP_STRANDS=0"           "0"    "$(field "$out" SP_STRANDS)"
is "SP_STRANDS_ESCALATED=0" "0"    "$(field "$out" SP_STRANDS_ESCALATED)"
is "SP_STRAND_GHOST=0"      "0"    "$(field "$out" SP_STRAND_GHOST)"
is "SP_STRAND_OTHER=none"   "none" "$(field "$out" SP_STRAND_OTHER)"

# =============================================================================
# A REAL LEDGER — one ghost (ident escalated=0, so no recurrence has been raised) and one
# starved-queue strand (ident "-", the queue case, escalated at a nonzero timestamp).
# =============================================================================
echo ""
echo "one ghost, one escalated starved queue"

cat > "$RUN/strands.json" <<'JSON'
{
  "spira:ghost:sp-x1": {"first": 1700000000, "acted": 0, "escalated": 0},
  "spira:starved:-":   {"first": 1700000000, "acted": 0, "escalated": 1700000500}
}
JSON
out="$(run_strands)"
is "SP_STRANDS=2"                "2" "$(field "$out" SP_STRANDS)"
is "SP_STRANDS_ESCALATED=1"      "1" "$(field "$out" SP_STRANDS_ESCALATED)"
is "SP_STRAND_GHOST=1"           "1" "$(field "$out" SP_STRAND_GHOST)"
is "SP_STRAND_OTHER=starved=1"   "starved=1" "$(field "$out" SP_STRAND_OTHER)"

# =============================================================================
# UNCLASSIFIABLE KEY — a key with fewer than the 3 colon-separated parts strand.sh always
# writes. THE GHOST COUNT MUST GO TO ? HERE, NEVER STAY A CONFIDENT 0 (the code's own
# comment: "the entry that could not be classified may well be a ghost"). This is the
# defect law-absence-needs-a-positive-control exists to catch, so it is the case most
# worth a named assertion.
# =============================================================================
echo ""
echo "a key with no kind separator makes the ghost count ?, not 0"

cat > "$RUN/strands.json" <<'JSON'
{
  "malformed-key-no-colons": {"first": 1700000000, "acted": 0, "escalated": 0}
}
JSON
out="$(run_strands)"
is "SP_STRANDS=1 (the row is still counted)" "1" "$(field "$out" SP_STRANDS)"
is "SP_STRAND_GHOST=? (unclassifiable key)"  "?" "$(field "$out" SP_STRAND_GHOST)"
want_other="$(field "$out" SP_STRAND_OTHER)"
is "SP_STRAND_OTHER reports the unclassified count" "unclassified=1" "$want_other"

printf '\n  %d ok, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
