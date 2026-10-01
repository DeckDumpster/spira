#!/usr/bin/env bash
#
# test-groomer-litter-predicate.sh — groomer's litter predicate: an unmapped-repo bead
#   with no description is litter (closeable); one with a description is left for the
#   model pass. Table-tested against canned bead JSON — no database.
#
#   ./test-groomer-litter-predicate.sh
#
# groomer-litter-predicate.py is gone: the predicate is now `groomer`'s own
# src/litter.rs, ported directly (pure JSON-in, judgement-out — no reason to keep it a
# subprocess). `groomer __litter` is the same stdin-in, two-line-stdout-out contract the
# Python had, kept as an internal CLI verb so this suite (and anyone debugging by hand)
# can still drive the predicate standalone rather than through the whole sweep.
#
# tier: T1
# covers: groomer/src/litter.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

predicate() {   # predicate <bead-json> -> "HAS_CONTENT <0|1>\nMETA <text>"
    printf '%s' "$1" | groomer __litter
}

echo "test-groomer-litter-predicate.sh"

# ======================================================================================
echo
echo "positive control — a bead with a real description is NOT litter:"
# ======================================================================================
out="$(predicate '{"description":"This bead has a real description.","created_at":"2026-09-10T00:00:00Z","assignee":"aeon-yojimbo"}')"
want "described bead: HAS_CONTENT 1" "HAS_CONTENT 1" "$out"

# ======================================================================================
echo
echo "no description at all IS litter:"
# ======================================================================================
out="$(predicate '{"created_at":"2026-09-10T00:00:00Z","assignee":"aeon-yojimbo"}')"
want "no description field: HAS_CONTENT 0" "HAS_CONTENT 0" "$out"

# ======================================================================================
echo
echo "an empty or whitespace-only description IS litter:"
# ======================================================================================
out="$(predicate '{"description":"   ","created_at":"2026-09-10T00:00:00Z"}')"
want "whitespace-only description: HAS_CONTENT 0" "HAS_CONTENT 0" "$out"

# ======================================================================================
echo
echo "META sanitizes created_at and assignee for the close reason:"
# ======================================================================================
out="$(predicate '{"created_at":"2026-09-10T00:00:00Z","assignee":"aeon-yojimbo"}')"
want "META carries the created_at" "created_at: 2026-09-10T00:00:00Z" "$out"
want "META carries the assignee"   "assignee: aeon-yojimbo"            "$out"

# ======================================================================================
echo
echo "a missing assignee/created_at falls back to 'unknown', not empty:"
# ======================================================================================
out="$(predicate '{}')"
want "no description at all: HAS_CONTENT 0" "HAS_CONTENT 0" "$out"
want "META defaults created_at to unknown" "created_at: unknown" "$out"
want "META defaults assignee to unknown"   "assignee: unknown"   "$out"

# ======================================================================================
echo
echo "a malformed payload does not crash — treated the same as no description:"
# ======================================================================================
out="$(predicate 'not json')"
want "malformed payload: HAS_CONTENT 0" "HAS_CONTENT 0" "$out"

tl_summary
