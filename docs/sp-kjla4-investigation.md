# sp-kjla4: test-doctor-enable-drift investigation (Ops conclusion)

## Investigation Summary

This incident was delegated to builder task sp-top9m. Investigation findings:

**Bug Location**: doctor.sh lines 1078-1085 (enabled units section)

**Root Cause**: doctor.sh was silently continuing when `systemctl is-enabled` returned no output, treating it as "unit-not-found". This is incorrect—empty output when the unit file exists indicates a genuine failure (manager unreachable), not a valid state.

**Fix**: Commit d44c6c79 on sp-top9m. The fix explicitly checks for empty output from systemctl:
- If the unit file doesn't exist: treat as "not-found" (continue)
- If the unit file exists: report FAIL with diagnostic guidance

**Test Status**: test-doctor-enable-drift.sh passes all 31 test cases with the fix. Test is deterministic with comprehensive mocks, confirming this is a real code defect (per law-absence-needs-a-positive-control).

**Evidence**: Test explicitly includes positive controls that verify disabled units ARE reported correctly, ruling out environmental flakiness.

## Outcome

sp-top9m is closed with fix ready to land. Rebase complete. Landing gate will judge the rebased tree.
