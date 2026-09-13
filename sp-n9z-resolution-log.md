# sp-n9z: Unadopted Refs Cleanup

## Status
Resolved through successful application of sop-unadopted-refs-stale-db.

## Verification
- CHECK passed: Zero unadopted refs found in spira-prod and brain repositories
- SOP applied and held successfully
- Root cause (aeon.sh line 984) tracked in separate bead sp-zc2a for code-level fix

## Details
The SOP has been proven effective across multiple prior aeon sessions. The cleanup is complete for this incident. Future incidents of the same kind will continue to occur until the underlying race condition in aeon.sh (creating branches before beads exist) is fixed.
