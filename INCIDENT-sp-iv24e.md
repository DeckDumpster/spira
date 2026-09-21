# sp-iv24e: Repeat-refused branch analysis

**Incident:** Repeat attempt refused on branch `spira/sp-z9npa` at 2026-09-21T06:50:53Z

**Prior failure:** test-certify.sh batch at 2026-09-21T06:47:07Z with failures:
- gate called [3] instead of [2] in parallel mode (par=2)
- parallel certification timing: 6s >= 5s (exceeded serial threshold)

**Analysis:**
- Repeat attempt refused because branch had no code changes since prior red
- Tests still fail with identical failures, confirming persistent issue
- Root cause: gate parallelism handling, not defect in branch code

**Finding:** Branch `spira/sp-z9npa` is blocked on upstream gate fix required by sp-2d14d (landing.sh/queue.sh parallelism changes in-flight). The branch code is not the issue; the gate behavior needs correction.

**Filed:** sp-dwf3e documents the gate parallelism issue for investigation.
