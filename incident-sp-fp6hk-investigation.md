# sp-fp6hk: Incident Investigation

## Summary
sp-fp6hk reported that sp-val2d's fix (commit 6fc5c4f9) is incorrect and breaks the empty-branch test. Investigation confirms the diagnosis and identifies that the correct solution is already on origin/main.

## Verified Facts

### Incorrect Fix (Not on origin/main)
- **Commit**: 6fc5c4f9 `lib.sh: fix content_landed to check ancestry before rejecting ahead=0`
- **Author**: aeon-shiva
- **Status**: NOT an ancestor of origin/main
- **Problem**: Moving the ancestry check before the ahead check breaks the empty-branch test by returning 0 for empty branches

### Correct Fix (On origin/main)  
- **Commit**: c6dda065 `fix: content_landed must return non-zero for zero-ahead branches (sp-qc4kn)`
- **Status**: IS an ancestor of origin/main (verified with `git merge-base --is-ancestor`)
- **Solution approach**:
  1. content_landed returns non-zero for zero-ahead branches (cannot distinguish empty from pure-ancestor)
  2. sending.sh implements fast-forward-merged reap path: checks zero-ahead AND is-ancestor AND landed()
  3. Test: test-content-landed-empty-branch.sh passes on origin/main, fails with 6fc5c4f9

## Conclusion
The work described in sp-val2d (fixing the pure-ancestor issue) is **already completed and landed** on origin/main via c6dda065. The attempted fix by aeon-shiva (6fc5c4f9) is incomplete and was not merged to main. The correct approach already in production properly handles both empty branches and pure-ancestor branches without breaking the test suite.

## Action
sp-fp6hk is closed as the correct fix is verified on origin/main. sp-val2d should be marked as superseded by c6dda065 (reference sp-qc4kn).
