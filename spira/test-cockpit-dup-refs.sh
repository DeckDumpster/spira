#!/usr/bin/env bash
#
# test-cockpit-dup-refs.sh — the duplicate-ref meter measures what dedup missed.
#
#   ./test-cockpit-dup-refs.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# incident.sh trusts its own dedup; this is the check on the dedup. When dedup breaks, every
# pass files a fresh bead for the same external_ref, and the meter is the thing that notices
# before the operator's queue fills up again (the pattern that produced 38 surplus beads over
# two days, sp-2lfgn).
#
# The meter queries beads labelled spira,incident and groups by external_ref. Any ref that
# appears on more than one bead within the lookback window is a dedup failure; SP_DUP_REFS
# is the count of such refs and SP_DUP_BEADS is the total surplus.
#
# THE POSITIVE CONTROL COMES FIRST (law-absence-needs-a-positive-control). Before asserting
# that the meter reads 0 on a clean database, this suite proves the meter would have reported
# a known duplicate — because a meter that always reads 0 or ? cannot be distinguished from
# one that never reads anything.
#
# SPIRA_BDJSON_FIXTURE, NOT A REAL STORE (docs/test-plan/cockpit-observability.md coverage
# row 13). dup_refs_keys' only bd read is one `list --all --label spira,incident`; the query
# shape itself — that this label pair and the lookback filter reach the real store correctly
# — is covered once in test-cockpit-bd-contract.sh, against real bd.
#
# COVERS: spira/cockpit.sh spira/watchtower.sh spira/incident.sh
# tier: T2
# covers: spira/cockpit.sh spira/watchtower.sh spira/incident.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "test-cockpit-dup-refs.sh"

# Run cockpit.sh dup_refs — the probe's own subcommand, not a full `once` — with bd reads
# answered from a canned-JSON fixture rather than a live store.
dup_refs() {    # dup_refs <fixture-file>
    env -i PATH="$PATH" HOME="$HOME" \
        SPIRA_CONF=/nonexistent \
        SPIRA_DB="$TMP/nodb" \
        SPIRA_RUN="$TMP" \
        SPIRA_BDJSON_FIXTURE="$1" \
        bash "$HERE/cockpit.sh" dup_refs 2>/dev/null
}
key() { printf '%s\n' "$1" | sed -n "s/^$2=//p" | head -1; }

# ======================================================================================
echo
echo "the positive control — the meter detects a known duplicate:"
# ======================================================================================
# Two beads share an external_ref. Pre-known ids let us verify that SP_DUP_ROW0 names the
# specific beads planted — that the meter did not arrive at its count via a different path.
cat > "$TMP/dup.json" <<'JSON'
[
  {"id":"sp-dup1","title":"dup bead 1","status":"open","issue_type":"task","labels":["spira","incident"],"updated_at":"2026-09-10T00:00:00Z","external_ref":"dup-test-ref-1"},
  {"id":"sp-dup2","title":"dup bead 2","status":"open","issue_type":"task","labels":["spira","incident"],"updated_at":"2026-09-10T00:00:00Z","external_ref":"dup-test-ref-1"}
]
JSON

keys="$(dup_refs "$TMP/dup.json")"
is   "SP_DUP_REFS=1 (one duplicated ref)"    "1" "$(key "$keys" SP_DUP_REFS)"
is   "SP_DUP_BEADS=1 (one surplus bead)"     "1" "$(key "$keys" SP_DUP_BEADS)"
want "SP_DUP_ROW0 names the ref"             "dup-test-ref-1" "$keys"
want "and names the first bead id"           "sp-dup1"        "$keys"
want "and names the second bead id"          "sp-dup2"        "$keys"
is   "SP_DUP_N=1 (one row emitted)"          "1" "$(key "$keys" SP_DUP_N)"

# ======================================================================================
echo
echo "a clean database (no duplicate refs) renders 0, not ?:"
# ======================================================================================
# ZERO IS A VALID MEASUREMENT. Two open beads with DIFFERENT external_refs must not count
# as duplicates, and the meter must not emit ? just because the duplicate count is zero
# (law-alerts-must-be-actionable).
cat > "$TMP/clean.json" <<'JSON'
[
  {"id":"sp-clean1","title":"distinct ref bead 1","status":"open","issue_type":"task","labels":["spira","incident"],"updated_at":"2026-09-10T00:00:00Z","external_ref":"distinct-ref-alpha"},
  {"id":"sp-clean2","title":"distinct ref bead 2","status":"open","issue_type":"task","labels":["spira","incident"],"updated_at":"2026-09-10T00:00:00Z","external_ref":"distinct-ref-beta"}
]
JSON

keys_clean="$(dup_refs "$TMP/clean.json")"
is   "SP_DUP_REFS=0 (no duplicate refs)"    "0" "$(key "$keys_clean" SP_DUP_REFS)"
is   "SP_DUP_BEADS=0"                        "0" "$(key "$keys_clean" SP_DUP_BEADS)"
is   "SP_DUP_N=0 (no rows)"                  "0" "$(key "$keys_clean" SP_DUP_N)"
nowant "0 does not render as ?"              "SP_DUP_REFS=?" "$keys_clean"

# ======================================================================================
echo
echo "an unreadable store renders ? rather than 0:"
# ======================================================================================
# THE FAILURE THIS BEAD EXISTS TO PREVENT. A dedup meter that reads 0 because it cannot
# reach the database is indistinguishable from a system where dedup is healthy — and that
# indistinguishability is the exact defect the meter was built to close. The ? convention
# preserves the suspicion a broken probe must not displace (law-failed-probe-is-not-zero).
#
# A nonexistent fixture path makes bdsim.py exit 1 with no stdout — the same shape bdjson
# sees from a real bd that cannot reach its store.
keys_bad="$(dup_refs "$TMP/does-not-exist.json")"
is     "unreadable store: SP_DUP_REFS=?"            "?" "$(key "$keys_bad" SP_DUP_REFS)"
is     "unreadable store: SP_DUP_BEADS=?"           "?" "$(key "$keys_bad" SP_DUP_BEADS)"
nowant "unreadable store does not render 0"         "SP_DUP_REFS=0" "$keys_bad"

# ======================================================================================
echo
echo "multiple surplus beads on one ref are counted correctly:"
# ======================================================================================
# SP_DUP_BEADS is SURPLUS (total-1 per ref), not the total count. Three beads for one ref
# is 2 surplus, not 3. This matters because the correct reading of the meter is "how many
# beads can be collapsed", not "how many total beads are involved".
cat > "$TMP/triple.json" <<'JSON'
[
  {"id":"sp-tri1","title":"triple bead A","status":"open","issue_type":"task","labels":["spira","incident"],"updated_at":"2026-09-10T00:00:00Z","external_ref":"dup-triple-ref"},
  {"id":"sp-tri2","title":"triple bead B","status":"open","issue_type":"task","labels":["spira","incident"],"updated_at":"2026-09-10T00:00:00Z","external_ref":"dup-triple-ref"},
  {"id":"sp-tri3","title":"triple bead C","status":"open","issue_type":"task","labels":["spira","incident"],"updated_at":"2026-09-10T00:00:00Z","external_ref":"dup-triple-ref"}
]
JSON

keys_triple="$(dup_refs "$TMP/triple.json")"
is   "three beads on one ref: SP_DUP_REFS=1"  "1" "$(key "$keys_triple" SP_DUP_REFS)"
is   "SP_DUP_BEADS=2 (surplus, not total)"    "2" "$(key "$keys_triple" SP_DUP_BEADS)"

# ======================================================================================
echo
echo "beads without an external_ref are excluded:"
# ======================================================================================
# Not every bead has an external_ref. Beads without one must not be counted or grouped —
# otherwise a pair of beads with no ref would always count as a pair (grouped on "").
cat > "$TMP/noref.json" <<'JSON'
[
  {"id":"sp-noref1","title":"no external ref bead 1","status":"open","issue_type":"task","labels":["spira","incident"],"updated_at":"2026-09-10T00:00:00Z"},
  {"id":"sp-noref2","title":"no external ref bead 2","status":"open","issue_type":"task","labels":["spira","incident"],"updated_at":"2026-09-10T00:00:00Z"}
]
JSON

keys_noref="$(dup_refs "$TMP/noref.json")"
is   "beads with no external_ref are not counted as duplicates" "0" "$(key "$keys_noref" SP_DUP_REFS)"
is   "SP_DUP_BEADS=0"                                           "0" "$(key "$keys_noref" SP_DUP_BEADS)"

# ======================================================================================
echo
echo "closed beads outside the lookback window are excluded:"
# ======================================================================================
# The lookback window exists so that old noise does not inflate the current count. Beads
# closed long before the cutoff are already resolved (merged or abandoned) and should not
# count as current dedup failures. Here both beads share a ref but their closed_at is
# 2026-01-01 — well before the 7-day lookback from whenever this suite runs.
cat > "$TMP/old.json" <<'JSON'
[
  {"id":"sp-old1","title":"old closed bead A","status":"closed","issue_type":"task","labels":["spira","incident"],"updated_at":"2026-01-01T00:00:00Z","closed_at":"2026-01-01T00:00:00Z","external_ref":"old-dup-ref"},
  {"id":"sp-old2","title":"old closed bead B","status":"closed","issue_type":"task","labels":["spira","incident"],"updated_at":"2026-01-01T00:00:00Z","closed_at":"2026-01-01T00:00:00Z","external_ref":"old-dup-ref"}
]
JSON

keys_old="$(dup_refs "$TMP/old.json")"
is   "old closed beads outside 7-day window: SP_DUP_REFS=0" "0" "$(key "$keys_old" SP_DUP_REFS)"
is   "SP_DUP_BEADS=0"                                        "0" "$(key "$keys_old" SP_DUP_BEADS)"

echo
tl_summary
