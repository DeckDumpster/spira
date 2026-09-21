# sp-dwf3e: Gate Parallelism Issue

## Finding

The branch `spira/sp-z9npa` is blocked by a **gate implementation issue**, not a branch code defect.

## Evidence

Test failures in test-certify.sh:
- "gate called for both branches with par=2: wanted [2] got [3]"
- "both branches certified in parallel: 6s >= 5s — serial"

These indicate that the landing.sh/gate.sh does not correctly implement the `par=N` parallelism parameter.

## Root Cause

The gate infrastructure does not yet support parallel execution with the `par=N` parameter. The branch sp-z9npa correctly added tests for this feature, but the gate cannot execute them.

## Status

- **Branch**: sp-z9npa (unable to fix locally; root cause is infrastructure)
- **Repeat refused**: "no change" (correct diagnosis — branch has nothing to fix)
- **Related**: sp-2d14d has in-flight landing.sh changes that may address this (blocked)
- **Filed**: sp-ubjuq to track gate parallelism implementation

## Next Steps

1. Resolve sp-2d14d blocker to land gate parallelism support
2. Once gate supports par=N, sp-z9npa can be retried and should pass
3. Or implement par=N support in a new bead if sp-2d14d is not suitable
