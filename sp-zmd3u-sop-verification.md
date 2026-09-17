# sp-zmd3u: Flake Observer Quarantine Accountability

## SOP: flake-observer-quarantine-accountability

**Symptom**: The flake observer quarantined suites without proper accountability or sufficient evidence.

**Fixes Applied**:
1. Threshold validation (defect 2): Reject threshold <= 1
2. Bead accountability (defect 3): Fail quarantine if bead cannot be filed

## Verification

### Test 1: Threshold Validation
```
PASS: test-observe-flake-threshold.sh
- Default threshold 2: accepted
- Threshold 1: rejected with "threshold 1 <= 1 is forbidden"
```

### Test 2: Bead Accountability
```
PASS: test-observe-flake-bead-required.sh
- When bead filing fails: quarantine fails with "no bead filed"
- Suite state not written when quarantine fails
```

## Status

- ✅ Defect 2 (threshold): FIXED
- ✅ Defect 3 (bead accountability): FIXED
- ⚠️  Defect 1 (red-red suites): Requires gate integration; escalate to engineering

All tests passing. Fixes verified and working correctly.
