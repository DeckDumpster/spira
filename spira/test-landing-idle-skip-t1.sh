#!/usr/bin/env bash
#
# test-landing-idle-skip-t1.sh — certify_needs_gate (landing-lib.sh, sp-a5jpo), the pure
# idle-skip decision land_repo makes for a queue-mode branch: skip the local certification
# gate only when the cert queue is empty AND CI is idle, because at batch size 1 the
# bisect argument for per-branch certification is vacuous. Every row here is a function
# call over already-fetched counts — no forge, no queue file, no landing pass. What
# test-certify needs a full pass (a stub queue dir, a stub forge, a stub gate) to exercise
# one branch of, this asserts directly.
#
# host-reason: sources landing-lib.sh only; no database, no systemd, no git
# tier: T1
# covers: spira/landing-lib.sh UC-landing-merge-queue-07
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
. "$HERE/landing-lib.sh"

echo "test-landing-idle-skip-t1.sh"

# 1. IDLE SKIP: cert queue empty (0) and CI idle (0) -> skip.
is "certify_needs_gate(): empty queue + idle CI -> skip" \
    "skip" "$(certify_needs_gate 1 0 0)"

# 2. BUSY: cert queue empty but CI has an active run -> gate (not the sole batch member
#    in spirit — something else is already in flight for this repo).
is "certify_needs_gate(): empty queue + busy CI -> gate" \
    "gate" "$(certify_needs_gate 1 0 3)"

# 3. NOT SOLE MEMBER: cert queue already has entries -> gate, regardless of CI state.
is "certify_needs_gate(): non-empty queue + idle CI -> gate" \
    "gate" "$(certify_needs_gate 1 2 0)"

# 4. PRECONDITION: the feature is disabled (SPIRA_CERT_IDLE_SKIP=0) -> always gate, even
#    when the queue is empty and CI is idle (the shape that skips when enabled).
is "certify_needs_gate(): disabled -> gate even when idle" \
    "gate" "$(certify_needs_gate 0 0 0)"

# POSITIVE CONTROL: the disabled row above and the busy row above must differ from the
# idle-skip row — three different inputs, three "gate" answers, one "skip" answer. A
# matcher that always prints "gate" would still pass rows 2-4; row 1 is what catches it.
nowant "positive control: the idle-skip row is not just another 'gate'" \
    "gate" "$(certify_needs_gate 1 0 0)"

tl_summary
