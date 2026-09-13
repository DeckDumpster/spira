# Alignment check — sp-t4t6

Epic sp-1krr Intent measured verbatim on 2026-09-13. Full report is in the design document:
`wiki/projects/spira/designs/container-first-suites-2026-09-12.md` (brain repo).

## Five outcomes

| # | Outcome | Measured | Target | Status |
|---|---|---|---|---|
| 1 | Undeclared host suites | 227 / 245 | 0 | NOT MET — work remaining (waves 2–4) |
| 2 | Fixture-drift defects | 0 open | 0 | MET |
| 3 | Image models dependencies | FAIL (cargo 1.75.0) on old image; new Containerfile not yet built | zero FATAL + zero un-waived WARN | NOT CONFIRMED — rebuild required; structural gap for cargo in doctor-check.sh |
| 4 | Fences deleted | both present on origin/main | binary gone | NOT MET — sp-1ctx gate-failed and OPEN |
| 5 | Gate timing | ~267s (est.) | < 300s budget | MET (estimated) |

## Key measurements (origin/main, 2026-09-13)

**Outcome 1:**
- Total suites: 245
- Container-resident: 12 (testenv_up/exec calls)
- Host-declared: 10 (# host-reason: lines)
- Undeclared: 227 (hermetic.sh --host-check count)

**Outcome 2:**
- Open fixture-fault beads: 0
- Closed: sp-i7u, sp-1rs, sp-mwf, sp-841s.44/.52/.97 and sp-vnxz children

**Outcome 3:**
- doctor-check.sh in image ecb25dd5d06e: 8 ok, 1 waived (claude), 0 FAIL
- Full doctor.sh in image ecb25dd5d06e: FAIL — cargo 1.75.0 < 1.78.0 minimum (sp-tst3)
- Correct image tag: 596c357e2831 (not built; Containerfile uses rustup 1.96.0 per sp-5ozg)
- Structural gap: cargo removed from WARN loop by sp-tst3, so doctor-check.sh no longer
  checks cargo presence or version
- Waiver: claude (1 program; reason on file in testenv/doctor-waivers)

**Outcome 4:**
- spira/hermetic.sh: exists on origin/main
- Fixture-contamination fence in gate-spira.sh (lines 188–232): exists on origin/main
- sp-1ctx: OPEN — gate returned UNKNOWN/branch-red at 2026-09-13T02:26Z; reopened by sentinel

**Outcome 5:**
- Gate suites on origin/main: 8 (test-soak removed by sp-jb7y)
- Pre-sp-jb7y: 357s (9 suites); sp-jb7y removed test-soak (~90s)
- Estimated post-sp-jb7y: ~267s (sp-jb7y commit message; test-gate-budget.sh 15/15)
- SPIRA_GATE_BUDGET: 300s

## Classification

Outcomes 1 and 4 are work remaining, not Intent wrong:
- Outcome 1: migration is in progress; waves 2–4 have not started. Expected mid-epic state.
- Outcome 4: the work was done (sp-1ctx commit aeb50fc4); the gate red is the blocker.

Outcome 3 has a structural gap (cargo removed from doctor-check.sh's purview) that is
separate from whether the image will pass when rebuilt. The gap means a future removal of
cargo from the Containerfile would not be caught by the build check.
