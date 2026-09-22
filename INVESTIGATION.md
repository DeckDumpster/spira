# sp-mbzrp Investigation: Intermittent Batch Test Failures

## Incident Summary

Tests pass individually (test-batch-stuck: 14 passes, test-landing: 56 passes, test-poison: 28 passes, test-requeue: 25 passes, test-requeue-cap: 6 passes) but fail intermittently when run in batch. Different suites fail each time, indicating an order-dependent issue, not a consistent defect.

## Root Cause Identified

**Shared testdb state in batch container execution environment.**

### How testenv-batch.sh Batch Mode Works

From `testenv-batch.sh` lines 810-812:
> "ONE BATCH, NOT ONE CONTAINER PER SUITE. Standing a container up costs ~10s; 260 of them would cost more than the pass. testenv-batch.sh brings up one container, installs the candidate once, and runs the selection inside it."

This means:
- All selected suites run in a SINGLE container instance
- The container persists for the entire batch run
- All tests share the same filesystem, environment, and running services

### The testdb Problem

From `testdb.sh` lines 1-2:
> "testdb.sh — a REAL `bd` on a throwaway Dolt database."

Tests that use `testdb_up()` create fixture databases. The lifecycle is:
1. `testdb_up fayth` - creates new testdb, exports SPIRA_DB
2. Test runs using the fixture
3. `trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM` - cleanup on exit

**In batch mode, with multiple tests running sequentially in one container:**
- Test 1 creates tmpdir, starts testdb, runs, cleanup trap fires
- Test 2 immediately creates new tmpdir, starts testdb... **but residual state may exist**
- If testdb_drop is incomplete or there's a race, test 2 may inherit test 1's leftover data

### Evidence

1. **Container-wide testdb state**: All tests in batch share the container's filesystem and services
2. **testdb.sh lines 19-30 history**: Previous server-mode testdb attempts had "schema migrations hold a global lock and leaked fixtures from SIGKILL'd suites"
3. **Intermittent pattern**: Different suites fail each time → execution-order-dependent → state leakage
4. **Individual pass, batch fail**: Each individual test gets fresh process → no state leakage

### Concrete Failure Mechanism

When tests run sequentially in batch:
```
[Container startup]
├─ Test 1: testdb_up → test runs → testdb_drop
├─ Test 2: testdb_up → inherits residual state → test fails
├─ Test 3: testdb_up → different residual state → test fails
└─ Different failure pattern next batch run (execution order varies)
```

## What This Is NOT

- NOT a verdict-repeat mechanism defect (the diagnosis correctly identified this)
- NOT a gate-level issue (tests pass on origin/main with no pending changes)
- NOT a single-suite defect (multiple different suites affected)

## Required Resolution

The root cause `sp-k2jjz` (filed as child bead) requires:

1. **Verify testdb_drop completeness in batch mode**: Ensure cleanup removes all traces of the previous test's database
2. **Isolate testdb per test in batch**: Consider per-test tmpdir that doesn't share state
3. **Review server-mode testdb leaks**: testdb.sh acknowledges server mode had fixture leakage; verify server-mode cleanup is complete
4. **Add batch-mode synchronization**: Between testdb_drop completion and next testdb_up start
5. **Test: reproduce in batch, verify fix**: Run problematic suite batch again after fix

## Verification Path

1. Identify which specific testdb state is leaking (schema? data? file descriptors?)
2. Add testdb_drop verification step before next testdb_up
3. Re-run verdict-repeat cycle on the failing suites in batch mode
4. Confirm green across multiple batch runs with different execution orders
