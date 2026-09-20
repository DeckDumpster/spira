# Why the local landing gate's mean seconds per certification grew 12x in a day

Spike sp-kz6ml · 2026-09-20 · evidence in
`docs/spikes/why-the-local-landing-gate-slowed-evidence/` · proof of concept on
branch `spike/sp-kz6ml-poc`, unmerged and **local to this box's harness checkout** — aeons
carry no push credentials, so the branch was never sent to a remote. It is one commit on
top of `f4d9072`; if it has been reaped, the change is nine lines and is quoted in `sp-lewhk`.

## 1. The question

Spike sp-0pb8h measured the local landing gate's mean seconds per certification by hour on
2026-09-20 and found it rose from 86s at 00:00Z to 1031s at 17:00Z — a 12x slowdown — while
`spira/gate-suites` still named the same ten suites and had no commits since 2026-09-19. The
bead asks why, and names one explanation to rule out first: that the longest runs are reds,
which `gate-retry.sh` re-runs, so the rising mean may be composition rather than a slowdown.
An answer names the mechanism, says whether the gate really got slower, and — if it did not —
says what the day's gate time was actually spent on and what can be done about it.

**Short answer: the gate did not get slower, and the ten-suite list is not the list it runs.**
The landing gate derives its suite list from each branch's diff; the day's later branches
touched files that select ten to twenty times as many suites. Cost per certification tracks
suite count with R²=0.77, and the residual barely moves with the hour. The 86s the bead names
as the prize is the cost of certifying branches that touch almost nothing; it is not
recoverable. What *is* recoverable is 22% of the day's gate time, in two fixed costs paid on
every certification regardless of the branch. One of them is fixed on the POC branch.

## 2. What I found

### 2.1 The landing gate does not run `spira/gate-suites`

The bead's premise — "an unchanged ten-suite list" — does not describe this gate. The landing
gate runs the command in the repo map's `spira` row (observed,
`gate-phase-costs-20260920.txt`):

```
bash spira/inventory.sh && bash spira/literal-lint.sh && bash spira/scratch-fence.sh && {
    _s="$(bash spira/gate-touched.sh "$SPIRA_GATE_BASE" "$SPIRA_GATE_SELECT_HEAD")"
    [ -n "$_s" ] || exit 0
    bash spira/testenv-batch.sh --suites "${_s//$'\n'/,}" "$SPIRA_GATE_BRANCH"; }
```

`spira/gate-suites` is `gate-spira.sh`'s list, and `gate-spira.sh` is the scheduled runner
rather than the landing gate — its own header says so, and the repo map confirms it by never
naming it. The landing gate's list comes from `gate-touched.sh`, which calls
`select.sh --files <branch diff> --no-all-fallback`: every suite whose `# covers:` globs
match a file in the branch's diff, plus every suite that declares no `# covers:` line at all.

So the list is a property of the branch, and it is unbounded: `SPIRA_GATE_SELECT_CAP` exists
(`gate-touched.sh`, default `0` in `conf.sh:495`) and is not set on this box.

### 2.2 The reds explanation is refuted, and so is every explanation about the box

**Reds.** Restricting the hourly table to `pass` certifications leaves the rise intact:
79s at 00:00Z against 919s at 17:00Z (observed, columns 4–5 below, computed from
`gate-log-local-certifications-20260920-full.txt`). Composition by verdict is not the cause.

**Concurrency.** Mean concurrent aeon sessions per hour, reconstructed from
`$SPIRA_RUN/aeon-ledger.log` `done … wall_s=` records, is flat across the day and *lower* in
the slow hours than the fast ones: 2.00 at 00:00Z, 1.30 at 17:00Z, never above 2.9 in any
hour. The box was not busier when the gate was slow.

**The box itself.** 16 cores, load average 4.3–6.9, `vmstat` idle 67–84%, memory PSI
`avg300=0.07`. There is CPU stall pressure (`cpu some avg300=35.5`) but it comes from cgroup
quotas, not from saturation. Nothing here changes across a day.

### 2.3 What actually changed: the number of suites each branch selects

`testenv-batch.sh` leaves a per-suite record in `$SPIRA_RUN/batch-results/<batch>/<suite>.result`
— `<status> <epoch> <seconds> <fingerprint> <mode> <producer> <rc>`. Joining those records to
the gate log by time window gives, for 73 of the day's 155 certifications, the suite count the
gate actually ran (observed, `certification-vs-suite-count-20260920.tsv`).

| hour (Z) | certs | mean s | green certs | green mean s | certs with a suite record | mean suites selected |
|---|---|---|---|---|---|---|
| 00 | 14 | 87 | 10 | 79 | 4 | 2 |
| 01 | 9 | 91 | 7 | 96 | 6 | 4 |
| 02 | 14 | 70 | 13 | 75 | 1 | 4 |
| 03 | 8 | 147 | 7 | 102 | 2 | 6 |
| 04 | 10 | 99 | 8 | 102 | 5 | 4 |
| 08 | 9 | 180 | 5 | 103 | 5 | 5 |
| 09 | 7 | 271 | 6 | 182 | 3 | 64 |
| 10 | 14 | 240 | 10 | 175 | 3 | 44 |
| 11 | 17 | 128 | 12 | 146 | 5 | 12 |
| 12 | 8 | 374 | 4 | 409 | 6 | 43 |
| 13 | 4 | 631 | 4 | 631 | 4 | 57 |
| 14 | 6 | 485 | 4 | 167 | 6 | 27 |
| 15 | 6 | 656 | 5 | 681 | 6 | 62 |
| 16 | 7 | 451 | 6 | 245 | 7 | 34 |
| 17 | 4 | 1031 | 3 | 919 | 4 | 98 |
| 18 | 4 | 644 | 2 | 684 | 3 | 54 |
| 19 | 3 | 832 | 1 | 119 | 3 | 46 |

(Hours 05–07 had three, one and seven certifications and no surviving suite records.)

Fitting gate seconds on suite count across all 73 matched certifications:

```
ran = 147.4 + 9.31 * suites        R² = 0.769   (n = 73)
ran = 132.1 + 1.26 * batch_wall    R² = 0.826   (n = 73)
```

and the correlations that decide the bead's question:

```
corr(suite count, hour)          = +0.418
corr(regression residual, hour)  = +0.151
```

Mean residual is −15s for hours before 12:00Z and +13s for hours after — a 28s difference,
against a 945s swing in the raw hourly mean. **Once suite count is controlled for, the hour
of the day is worth about 28 seconds.** There is no slowdown to explain.

The bead's own counterexample resolves the same way: sp-kn2bj at 15:43Z ran 189s on 12
suites; sp-9c208 at 15:06Z ran 1837s on 221 suites. Both green, forty minutes apart, same
box, same gate — different branches.

### 2.4 Why afternoon branches select more: two files claimed by a quarter of the corpus

The corpus is 416 `spira/test-*.sh` files (origin/main at f4d9072), of which exactly one
declares no `# covers:` line — so the always-run floor is 1, not the driver. The driver is
fan-out. Counting suites whose `# covers:` globs match a single changed file (observed,
`covers-fan-out-20260920.txt`; matcher validated against the real `gate-touched.sh`, which
returned the identical 174 suites for spira/sp-5jsuu):

| changed file | suites selected |
|---|---|
| `spira/lib.sh` | 97 |
| `spira/conf.sh` | 92 |
| `spira/sentinel.sh` | 46 |
| `spira/cockpit.sh` | 43 |
| `spira/aeon.sh` | 40 |
| `spira/testenv-batch.sh` | 27 |
| `spira/landing.sh` | 27 |

A branch that touches `spira/lib.sh` pays roughly `147 + 9.31 × 97 ≈ 1050s`. A branch that
touches one leaf script pays about 190s. spira/sp-5jsuu's six-file diff — `bead.sh`,
`conf.sh`, `lib.sh`, `schema.sh` and two suites — selected 174 suites and cost 1522s.

The corpus grew 389 → 416 suites across the day and `lib.sh`'s fan-out grew 89 → 97, so the
selection *is* widening — but by 7–9% in a day, not by 12x. **The 12x is which branches came
up, not a change in the machinery.** That is a composition effect, just not the one the bead
proposed.

*Not established:* why the day's later branches clustered on `lib.sh`/`conf.sh`. Most of those
branches are already deleted (39 of 60 I tried no longer resolve), so I could not reconstruct
their diffs. It is plausibly just the work that was queued; I cannot show that.

### 2.5 Where the day's 40,088 gate seconds went

155 certifications, 40,088s (11.1 h) inside the local gate on 2026-09-20.

- **Suite execution: 56% of it is 21% of the certifications.** 15 of the 73 matched
  certifications selected ≥80 suites and account for 18,720s of the 33,390s those 73 cost.
- **Fixed per-certification overhead: ~120s, about 18,600s (5.2 h) a day.** Measured as gate
  `ran=` minus the batch's own wall: median 120s across 73 certifications. It decomposes
  (each measured directly in an `env -i` shell, `gate-phase-costs-20260920.txt`):

  | phase | cost |
  |---|---|
  | `spira/inventory.sh` | 36.3s |
  | `spira/gate-touched.sh` (6-file diff, 416-suite corpus) | 38.7s |
  | container start → first suite | 24s |
  | `spira/literal-lint.sh` | 5.7s |
  | `spira/scratch-fence.sh` | 0.07s |
  | *sum* | *~105s, against a measured median of 120s* |

- **Re-certification: 13,746s (34%).** 24 branches were certified more than once; all but the
  longest run of each totals 13,746s. This is mostly legitimate — a red that was fixed, or a
  rebase — and the verdict cache (`SPIRA_VERDICT_TTL=86400`) already returned 8 cached
  verdicts. I do not think there is a lever here, and I did not cost one.

### 2.6 The selector is quadratic in (corpus × diff), and the corpus is growing

`select.sh` calls `suite_covers_of` — which forks a `sed` — from inside the per-changed-file
loop, once per (changed file, suite) pair. On 416 suites that is 416 forks to find the
always-run set plus 416 more for every changed file. Measured against the unpatched selector
(`selector-benchmark-20260920.txt`):

| changed files | elapsed |
|---|---|
| 1 | 14.9s |
| 3 | 23.2s |
| 6 | 36.9s |
| 12 | 70.5s |

That is ~15s of floor plus ~4s per changed file today, and both terms scale with the corpus,
which gained 27 suites in a day.

### 2.7 The gate runs two suites at a time on a sixteen-core box, and that is deliberate

In-batch concurrency for batch `d80aab` (155 suites): 2 for 251 of 259 five-second samples,
1 for the other 8. `SPIRA_BATCH_MAXPAR=2` is set in the operator's `spira.conf`, with its
reason stated there: *"16 cores but 7 GiB of memory … 16 parallel test containers would
exhaust memory long before CPU. Memory is the binding constraint here."*

That reason checks out. The batch container's `memory.peak` was 835 MB at MAXPAR 2; the whole
user slice peaked at 6,961 MB of 7,419 MB with 3,897 MB of 4,095 MB swap already in use. The
container itself has no CPU limit (`cpu.max=max`) and used 2.05 cores over the batch, so the
suites are CPU-bound and would scale — **there is CPU headroom and no memory headroom.**

## 3. Options

Costs below are per day, against the observed 40,088s / 155 certifications, and assume that
profile repeats.

### Option A — build the covers map once (POC exists)

Read each suite's `# covers:` line once into an associative array instead of forking `sed`
per (file, suite) pair. Nine lines, one file.

- **Cost to do:** already done — `spike/sp-kz6ml-poc`, one commit, `spira/select.sh`,
  +9/−3 lines. Under an hour to review and land.
- **Measured benefit:** 38.7s → 15.4s median on a 6-file diff, over three alternating
  old/new pairs on the same box under the same load, returning the identical 174 suites each
  time. ~23s × 155 certifications = **~3,600s/day (9%)**.
- **Risk:** low. Uses a bash-4 associative array; the gate's selector runs on the host
  (bash 5.3.9 here) and select.sh already requires bash for `< <(…)`. The four suites that
  cover the selector — `test-select.sh`, `test-gate-touched.sh`, `test-covers-entries.sh`,
  `test-gate-base-selection.sh` — all pass through `testenv-batch.sh` against the POC branch.
  The residual risk is a box whose host bash is 3.x, where the map would silently be empty
  and every suite would look always-run — i.e. it would fail *expensive*, not *unsafe*.

### Option B — make `inventory.sh` cheaper

36.3s on every certification, 5,630s/day (14%), for a static scan of a tree that changed by a
handful of files. I did not profile it and do not know whether it is reducible; a scan
restricted to the branch's diff would be the obvious shape, and would need a positive control
proving it still catches an offender in an untouched file (or an argument for why it need not).

- **Cost to do:** unknown — call it a day of work, since the fence is load-bearing and any
  change to it needs its own red-then-green proof.
- **Benefit if it can be made diff-scoped:** up to ~5,000s/day (12%).
- **Risk:** this is the fence that keeps the operator's inventory out of a shared repository.
  Narrowing what it looks at is exactly the class of change that turns a fence into a
  decoration (law-a-control-that-cannot-check-must-refuse). Not to be done casually.

### Option C — raise `SPIRA_BATCH_MAXPAR`

Scaling the wall portion of each certification by `2/MAXPAR` against the day's log:

| MAXPAR | modelled day total | vs the MAXPAR=2 model (44,331s) |
|---|---|---|
| 3 | 35,754s | −19% |
| 4 | 31,466s | −29% |
| 6 | 27,177s | −39% |
| 8 | 25,033s | −38% |

- **Cost to do:** one line in `spira.conf`.
- **Cost to be able to do it:** memory the box does not have. At 835 MB peak for two
  concurrent suites, each additional slot is on the order of 250–300 MB (*inferred* — I did
  not measure the container's pre-suite baseline, so the per-slot increment is a division, not
  an observation). MAXPAR 6 needs roughly 1 GB more than exists, on a box already 3.9 GB into
  4 GB of swap.
- **Risk:** this is the exact shape of law-fence-loops-on-shared-hardware — the box also runs
  the CI runner, and the last time something here was left unbounded it drove load past 60 and
  got the runner killed. Raising MAXPAR without raising RAM converts a CPU-bound gate into a
  swapping box, which would slow *everything*, including the thing being measured.

### Option D — cap the selection with `SPIRA_GATE_SELECT_CAP`

The cap is already implemented and defaults to 0.

- **Cost to do:** one line. At cap 60 it would have truncated 15 of 73 matched certifications
  and saved ~5,350s across them (~16% of those certifications' gate seconds).
- **Risk:** a capped certification is a gate that silently did not run suites it had itself
  decided were needed, and the branch lands anyway. That is disarming a check to make a
  number go down (law-never-disarm-a-check-to-proceed). **Recommend against.**

### Option E — narrow the `# covers:` globs on the 97 suites that claim `lib.sh`

- **Cost to do:** ~97 files, each needing a judgement about what the suite really depends on.
  Days, and the judgements are not checkable by anything that exists.
- **Risk:** the wide default is deliberate (`CLAUDE.md`: "Err wide: a suite run needlessly
  costs seconds"). A glob narrowed wrongly makes a regression invisible, which is
  strictly worse than the wall clock it buys. **Recommend against.**

## 4. Recommendation

**Land Option A, then file Option B. Leave MAXPAR alone until the box has more RAM, and do
not cap the selection.**

A is measured, costs nine lines, changes no verdict, and gets more valuable every day the
corpus grows — it is the only option here where the benefit and the risk are both known. B is
the larger prize (14% against A's 9%) but is unscoped and touches a fence, so it belongs to a
bead with its own red-then-green proof rather than to this spike. Together they take the
fixed per-certification cost from ~120s to ~60s: **~8,600s/day, 22% of the local gate's time,
without running one fewer suite.**

C is the biggest lever on the list and is genuinely blocked, not deferred: the operator
already reasoned it out in `spira.conf` and the measurements agree. The decision it implies —
whether to give this box more memory — is the operator's, and it is worth about 3.6 hours a
day of landing-pass time at MAXPAR 6. I have not escalated it, because nothing is blocked on
the answer and the note in `spira.conf` shows it has already been considered.

**The load-bearing assumption is that the fixed ~120s overhead is paid per certification and
the certification count stays near 155/day.** If certifications became rarer and larger — say
40 a day averaging 200 suites — A and B would be worth a quarter of what they are worth here,
and C would be worth more.

### And the prize in the bead is not there

The bead computes that a pass certifying eight branches would spend 11 min instead of 137 min
if the gate returned to its 00:00Z mean of 86s. It would not. 86s is what it costs to certify
a branch that selects two suites. The eight branches in a 17:00Z pass selected 46–98 suites
each because of what they changed; certifying them for 86s each would mean not running the
suites their diffs select. The recoverable part is the ~120s of fixed cost on each, not the
suite time.

## 5. The falsifier

This document says the gate's *per-suite* cost was flat on 2026-09-20 and that all of the 12x
is suite count. It is wrong if any of these turns out to be true:

1. **Re-run the join and the regression on a later day** (the method is in
   `certification-vs-suite-count-20260920.tsv`'s header; the inputs are `$SPIRA_RUN/gate.log`
   and `$SPIRA_RUN/batch-results/*/*.result`). If `corr(residual, hour)` rises above ~0.4
   while `corr(suite count, hour)` falls, there is a real time-of-day slowdown and this
   conclusion does not hold.
2. **R² falls below ~0.5.** At 0.77 the model is the explanation; at 0.4 something else is
   most of the variance.
3. **The fixed overhead stops being ~120s.** Measure `inventory.sh`, `gate-touched.sh` and
   the container-start gap directly in an `env -i` shell, as in
   `gate-phase-costs-20260920.txt`. If the sum no longer approaches the measured
   `ran − batch_wall` median, the decomposition in §2.5 is wrong and Options A and B are
   costed against the wrong denominator.
4. **The always-run floor stops being 1.** `git grep -L -e '^# *covers:' origin/main -- 'spira/test-*.sh'`
   returns one file today. If suites start landing without declarations, the floor grows and
   every certification pays it — at which point the cheapest fix is a gate on the declaration,
   not anything in this document.

## 6. What I could not establish

- **Why the day's later branches clustered on `lib.sh` and `conf.sh`.** 39 of the 60 branches
  in the gate log no longer resolve, so their diffs are gone. Everything in §2.4 about
  fan-out is measured; the claim that afternoon branches happened to touch those files is
  read off the suite counts, not off the diffs.
- **The per-slot memory increment for Option C.** 835 MB peak at two concurrent slots is
  observed; "250–300 MB per additional slot" is that number divided, without a measurement of
  the container's pre-suite baseline. I did not run a higher-MAXPAR batch to find out, because
  the box was at 6,961 MB of 7,419 MB with swap 95% full and the landing pass was mid-flight
  (law-fence-loops-on-shared-hardware).
- **Whether `inventory.sh`'s 36s is reducible.** Measured, not profiled.
- **Early-hour suite counts.** `batch-results` directories are keyed by a content hash and are
  overwritten by a later run with the same key, so the 00:00–08:00Z rows in the join rest on
  1–6 surviving records each. The bias is toward *missing* rows rather than wrong ones, and
  the green-only hourly means in §2.2 — which do not depend on the join at all — carry the
  same conclusion on their own.

## 7. Filed

- `sp-lewhk` — select.sh: build the `# covers:` map once, not once per (changed file, suite).
  Option A, P2. Carries the POC branch and the measurements; a spike may not merge outside
  `docs/spikes`, so the fix lands under that bead.
- `sp-yj47l` — `spira/inventory.sh` costs 36s on every landing-gate certification. Option B,
  P3. Asks for a profile first and explicitly does not ask for a narrower fence.
