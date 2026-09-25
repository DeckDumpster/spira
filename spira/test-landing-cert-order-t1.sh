#!/usr/bin/env bash
#
# test-landing-cert-order-t1.sh — certify_order and certify_tier (landing-lib.sh), the
# pure classifier land_repo calls to decide which closed branch is certified next in a
# queue-mode pass. Every row here is a function call over synthetic rows — no git, no
# testdb, no gate.sh trial, no landing pass. What test-landing-order.sh and
# test-landing-phase2-order.sh need a full pass (worktrees, a stub gate, a bead per row)
# to exercise one branch of, this asserts directly.
#
# host-reason: sources landing-lib.sh only; no database, no systemd, no git
# tier: T1
# covers: spira/landing-lib.sh UC-landing-merge-queue-02 UC-landing-merge-queue-04
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
. "$HERE/landing-lib.sh"

echo "test-landing-cert-order-t1.sh"

# --- certify_order: three buckets (base-fix, express, tail), each sorted by --------------
# (priority ASC, closed_at ASC) — UC-landing-merge-queue-04 ------------------------------

rows() { printf '%s\n' "$@"; }   # one row per arg, already tab-joined by the caller

# 1. PLAIN PRIORITY/CLOSED_AT ORDER, no fix or express branches (mirrors
#    test-landing-order.sh's "order" property): P1-oldest, P1-newer, P2-oldest, P2-newest,
#    not refname order (refname order would be a, b, c, d).
out="$(rows \
    $'sp-ord-a\tspira/sp-ord-a\t2\t2026-09-01T00:00:00Z\t-\t0' \
    $'sp-ord-b\tspira/sp-ord-b\t1\t2026-09-02T00:00:00Z\t-\t0' \
    $'sp-ord-c\tspira/sp-ord-c\t2\t2026-09-03T00:00:00Z\t-\t0' \
    $'sp-ord-d\tspira/sp-ord-d\t1\t2026-09-01T00:00:00Z\t-\t0' \
    | certify_order fixture-repo)"
is "certify_order(): priority/closed_at order, not refname order" \
    "$(printf 'spira/sp-ord-d\nspira/sp-ord-b\nspira/sp-ord-a\nspira/sp-ord-c')" "$out"

# 2. BASE-FIX FIRST, regardless of priority: a P2 fix branch sorts before a P1 plain one.
out="$(rows \
    $'sp-plain\tspira/sp-plain\t1\t2026-09-01T00:00:00Z\t-\t0' \
    $'sp-fix\tspira/sp-fix\t2\t2026-09-02T00:00:00Z\tbasefail:fixture-repo:test-x.sh\t0' \
    | certify_order fixture-repo)"
is "certify_order(): base-fix branch sorts first despite lower priority" \
    "$(printf 'spira/sp-fix\nspira/sp-plain')" "$out"

# 3. EXPRESS SECOND: an express-labelled P2 branch sorts before a plain P1 branch, but
#    after a base-fix branch (three-bucket partition, not a plain priority sort).
out="$(rows \
    $'sp-plain\tspira/sp-plain\t1\t2026-09-01T00:00:00Z\t-\t0' \
    $'sp-expr\tspira/sp-expr\t2\t2026-09-02T00:00:00Z\t-\t1' \
    $'sp-fix\tspira/sp-fix\t3\t2026-09-03T00:00:00Z\tbasefail:fixture-repo:test-x.sh\t0' \
    | certify_order fixture-repo)"
is "certify_order(): fix, then express, then plain (three-bucket partition)" \
    "$(printf 'spira/sp-fix\nspira/sp-expr\nspira/sp-plain')" "$out"

# 4. POSITIVE CONTROL: a basefail external_ref for a DIFFERENT repository is not treated
#    as this repository's fix — the match is scoped to the repo name argument.
out="$(rows \
    $'sp-other-repo\tspira/sp-other-repo\t1\t2026-09-01T00:00:00Z\tbasefail:another-repo:test-x.sh\t0' \
    $'sp-plain\tspira/sp-plain\t2\t2026-09-01T00:00:00Z\t-\t0' \
    | certify_order fixture-repo)"
is "certify_order(): a fix ref for a different repo does not jump the queue" \
    "$(printf 'spira/sp-other-repo\nspira/sp-plain')" "$out"

# --- certify_tier: phase-2 classification of one closed branch --------------------------
# UC-landing-merge-queue-02 ---------------------------------------------------------------

# 1. Never-gated: no landstate record at all.
out="$(certify_tier "" "" "" "" abc123 1000)"
is "certify_tier(): no record -> tier 0, never-gated" "tier=0 reason=never-gated" "$out"

# 2. RED, tip has moved since the record -> promoted to tier 0 (treated as never-gated).
out="$(certify_tier RED oldtip 500 gate newtip 1000)"
is "certify_tier(): RED with a stale tip -> tier 0, tip-stale" "tier=0 reason=tip-stale" "$out"

# 3. RED, tip unchanged, reason is not base-red -> skip (re-gating would repeat itself).
out="$(certify_tier RED sametip 500 gate sametip 1000)"
is "certify_tier(): RED, current tip, non-base-red -> skip" "tier=skip reason=tip-current" "$out"

# POSITIVE CONTROL for case 3: the same current-tip RED, but reason=base-red, is NOT
# skipped — the base may have recovered, so it still gets a tier (last, tier 2).
out="$(certify_tier RED sametip 500 base-red sametip 1000)"
is "certify_tier(): RED, current tip, base-red -> tier 2, not skipped" "tier=2 reason=red" "$out"

# 4. conflicts-with-base, base has advanced past the record -> promoted to tier 0.
out="$(certify_tier RED sametip 500 conflicts-with-base sametip 1000)"
is "certify_tier(): cwb record with an advanced base -> tier 0, cwb-stale-base" \
    "tier=0 reason=cwb-stale-base" "$out"

# 5. conflicts-with-base, base has NOT advanced, tip unchanged -> skip.
out="$(certify_tier RED sametip 1000 conflicts-with-base sametip 500)"
is "certify_tier(): cwb record with a current base and tip -> skip" \
    "tier=skip reason=cwb-current" "$out"

# 6. conflicts-with-base, base has NOT advanced, but the tip HAS moved -> tier 0
#    (tip-stale takes the promotion even though the base side of the pair is current).
out="$(certify_tier RED oldtip 1000 conflicts-with-base newtip 500)"
is "certify_tier(): cwb record, current base but stale tip -> tier 0, tip-stale" \
    "tier=0 reason=tip-stale" "$out"

# 7. A non-RED record (e.g. GATED, CONTENT) -> tier 1, ordinary re-gate.
out="$(certify_tier GATED sometip 500 gate-red sometip 1000)"
is "certify_tier(): non-RED record -> tier 1, non-red" "tier=1 reason=non-red" "$out"

tl_summary
