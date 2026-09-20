# SOP: Batch pre-flight gate latency

**Incident:** sp-hsxk8

**Decision:** Operator decision 2026-09-17 20:11 — keep pre-flight gate OFF THE CRITICAL PATH and NARROWED to marginal risk.

## Symptom

Landing loop frozen for 14-90 minutes by batch pre-flight gate. A single landing pass cannot complete during a batch gate run. The mutex held by landing.sh blocks all verdict settlement and certification, even for green batch PRs that are ready to land.

Observed: 19:09Z pass held for 90 minutes, killed by RuntimeMaxSec cap at 20:39Z, producing zero output. No landing pass ran between 18:54:03 and 19:08:24 (14m 21s freeze).

## Check

Verify gate is async (Phase 1) and marginal-risk narrowed (Phase 2):

```bash
# Phase 1: gate is async, spawn-and-return
git log --oneline origin/main | grep sp-c8w16

# Phase 2: gate narrowed to marginal risk
git log --oneline origin/main | grep sp-74gwk
```

Expected: both commits present on origin/main.

## Fix

**Phase 1 (sp-c8w16):** Decouple gate from critical path
- Gate spawn is async via background job or deferred execution
- Landing pass records gate-in-flight and returns immediately
- Subsequent pass collects result
- Verdict settlement and certification continue at normal cadence throughout

**Phase 2 (sp-74gwk):** Narrow to marginal risk
- Run only suites the batch's diff selects
- Run only those suites not already green on every member individually
- Skip full suite set that CI will also run

Acceptance:
- Landing pass latency is bounded and measurable (publish before-and-after)
- Green batch PR settled within one pass cadence regardless of other gates in flight

## Watcher False-Positive Pattern (sp-4hbuj)

While this phase is in progress (async gate commits not yet on main), the landing loop **legitimately freezes** pending implementation — this is not a gate bug. However, watchtower.sh --throttle-check cannot distinguish this expected state from real gate failure and fires recurring "deep+stalled" queue incidents (sp-pw5qw pattern).

**Fix (filed as sp-f2h4h):** watchtower.sh --throttle-check should verify async gate implementation status before escalating:
```bash
git log --oneline origin/main | grep -E '(sp-c8w16|sp-74gwk)' | wc -l | grep -qE '^2$'
```
If both commits NOT on main, the stall is deliberate—suppress alert.

## Reference

- law-local-gates-buy-latency-not-coverage — local gates buy latency, CI buys coverage
- law-gate-earns-its-place — record what the gate catches or delete it
- law-arm-before-you-retire — decouple before retiring old mechanism
- Child beads: sp-c8w16 (Phase 1), sp-74gwk (Phase 2)
- Decision: operator 2026-09-17 20:11, documented in sp-hsxk8 incident notes
- Watcher fix: sp-f2h4h

## Tags

batch-gate, landing-loop, latency, critical-path, marginal-risk, decision
