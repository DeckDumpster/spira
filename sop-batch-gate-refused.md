---
name: batch-gate-refused
description: merge queue batch pre-flight gate refuses red batch silently
metadata:
  type: sop
---

## Merge Queue Batch Gate Refusal — sp-2r8sa

**MATCH:** `batch spira.*: rebuilt batch.*red` OR `BATCH REFUSED.*repo=` in landing.log

**SYMPTOM:** Merge queue holds certified branches with no open batch PR. Pre-flight gate rejected a rebuilt batch; same gate re-runs every pass. Operator sees stall, not alert.

**CHECK:** 
```bash
grep -E "BATCH REFUSED|rebuilt batch.*red" "$SPIRA_RUN/landing.log" | head -1
```

**FIX:** batch.sh (spira/batch.sh lines 218-234, 396-417, 419-436) implements:

1. **Detect unchanged member set** (lines 218-234): If the same member set was refused on a prior pass, skip the gate and do not re-escalate. Saves ~14 minutes per loop.

2. **Record state** (lines 411-417): Store refused member set and red suites to `$SPIRA_RUN/batch-refused-<repo>.state` for detection on next pass.

3. **Escalate once** (lines 419-427): Mail operator once per unique refused member set using a flag file guard. Includes red suite names and batch member list so operator can identify which branch broke.

4. **Visible logging** (lines 407, 432-434): Write "BATCH REFUSED" line to landing.log before escalation attempt — visible even if mail.sh fails (law-total-blockage-announces-itself).

Operator action: Identify which member branch introduced the gate failure; that branch should be ejected from the queue or fixed in-place.

**ESCALATE:** If mail.sh is broken or operator cannot unblock the failing branch.

**REF:** spira/batch.sh; law-total-blockage-announces-itself; law-repeating-conditions-escalate-once
