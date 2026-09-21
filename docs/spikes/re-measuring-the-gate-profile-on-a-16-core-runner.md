# Re-measuring the gate profile on a 16-core runner

Spike for sp-0pb8h. This document records the baseline and the first 16-core data point.
Full analysis is in `wiki/notes/where-the-gate-time-goes-2026-09-20.md` (brain wiki).

## The question

Ryan doubled the CI runner template from 8 to 16 cores and raised `SPIRA_BATCH_MAXPAR` to
match. How much did the gate wall drop, and what is the constraint going forward?

## Baseline: 8-core runner, SPIRA_BATCH_MAXPAR 8

PR 165, run 35522175170. Opened 2026-09-20T15:15:54Z, landed 16:11:25Z (55.5-minute cycle).

| step | wall |
|---|---|
| provision + lint | ~1.5 min |
| suites | 1026s (~17 min) |
| teardown | <1 min |
| **gate total** | **~18 min** |

392 suites, 7294s summed, 8 workers → 7.1× parallelism. Container: ~5.8 of 8 cores busy.

*Note on timestamps.* sp-fxm90 records this baseline as "16:16Z" — that is when PR 167 was
opened, not when the PR 165 run occurred. The run completed before 16:11:25Z.

## First 16-core measurement

PR 169, run 35525243375, confirmed `maxpar 16` in the batch header, 413 suites.

| measure | 8-core | 16-core | delta |
|---|---|---|---|
| suites step | 1026s | 859s | −16% |
| gate wall | ~18 min | 22m49s | +5 min (provision slower) |
| cores busy, average | 5.8 of 8 | 7.9 of 16 | — |
| test-landing | 327s | 414s | longer under contention |
| test-poison | 340s | 403s | longer under contention |
| test-requeue-cap | 216s | 284s | longer under contention |

## Finding

Doubling the workers bought 16%, not the ~45% an unbound scale would give. The three longest
suites (test-landing, test-poison, test-requeue-cap) are each a full sentinel or landing pass
over a fixture database — they got *slower* under 16-way contention. They are now the
critical path and no worker count helps until they are split.

The levers that remain: per-suite `bd` cost (sp-czz4i, sp-yty8w) and splitting those three
suites. Adding more workers past this point manufactures flakes (three reds on re-run at 16
that were zero at 8) without shortening the wall.

## Related

- `wiki/notes/where-the-gate-time-goes-2026-09-20.md` — full profile with bd-cost breakdown
- sp-0pb8h — the re-measurement bead; uses this document as its method record
- sp-fxm90 — open bead to fill this table again after sp-czz4i and sp-xevus land
