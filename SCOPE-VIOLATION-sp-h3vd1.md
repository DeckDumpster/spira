# sp-h3vd1: Scope Violation — Misfiled Ops Incident

**Date**: 2026-09-23  
**Bead**: sp-h3vd1  
**Status**: OUT OF SCOPE for Ops  
**Action**: Reassign to code owner  

## Finding

sp-h3vd1 was filed as an operational incident but should not be in the Ops queue. The incident is a code/test failure on uncommitted changes, which is developer responsibility, not an operational incident.

## Evidence

The incident description explicitly states:

> "This is a code/test issue, not an operational incident."  
> "The test failure needs investigation by the code owner."  
> "This bead should be claimed by the builder/code owner of sp-npggh."

**Technical Details**:
- Branch: `spira/sp-npggh` 
- Commits modifying landing.sh: e1952a2a, c3e6d4d2
- Failing test: `test-landing-phase2-order.sh`
- Status: Uncommitted code changes; test failure needs code owner fix

## Why Prior Sessions Failed

Prior Ops sessions tried to close this with `delivers:note:` label, expecting an SOP to apply. This caused repeated reopens (timing mismatches on the delivers: record) because:

1. **This is not Ops work** — Ops's role is to handle production incidents, not code failures
2. **The delivers: criterion cannot be satisfied** — Ops should not be satisfying delivery labels for code/test work
3. **The reopens are correct** — They flag that this work doesn't belong in the Ops queue

## Precedent

Similar to **sp-wbany** (documented in sp-wbany-scope.md): a feature task that was misfiled as Ops work. The pattern is: some work gets filed as an incident/Ops task when it should go through the normal developer queue.

## Resolution

**Reassign to the builder/code owner of sp-npggh** with the following task:

- Investigate `test-landing-phase2-order.sh` failure on landing.sh changes (commits e1952a2a, c3e6d4d2)
- Either fix the code to match test expectations, OR update test expectations if the change is correct
- Land the fix through normal queue process

This is not Ops's responsibility. The work should be handled by the code team through the regular development/landing process.
