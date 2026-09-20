# Which lever halves the harness's bd time: startup, a resident bd, batching, or an in-process store client

Spike for sp-yty8w. Measured and written 2026-09-20. Raw output is preserved verbatim in
`which-lever-halves-the-harness-bd-time-evidence/` and cited by file. The proof of concept —
a Go program that opens bd's store in-process, a four-way benchmark, and a traced sentinel
pass — is on branch `spike/sp-yty8w-poc` under `poc/`, unmerged and local (an aeon carries no
push credentials).

This spike is the second half of a pair. `why-a-bd-call-costs-250ms.md` (sp-krxs8) answered
*why* one call costs what it costs; it could not cost the in-process option and said so, and
it named a falsifier for its own recommendation that it could not run. **Both are done here.**
Read that document first; this one does not repeat it.

## The question

> Why does one bd call cost 250 ms at p50 and 560 ms at p90 on a fresh embedded fixture, and
> what would cut the harness's bd time by half: a faster bd startup, a resident bd, batching
> in the hot loops, or an in-process store client (Rust)? Answer with measurements, not a
> recommendation from first principles.

Behind it, from the bead: Ryan's hypothesis that migrating components to Rust is the biggest
available reduction, because the bash is thin and the cost is the process boundary plus a
Dolt open per call. What counts as an answer is a number per lever, taken the same way, and
one recommendation.

## The answer in one paragraph

**Batching. It is 85x, it is the only lever that helps the test suites, and an in-process
client is worth 2.4x *after* it and nothing like that before it.** One real sentinel pass
over a 60-bead backlog makes 187 bd calls and **180 of them — 96.3% — are one loop**,
CHECK 4's three-per-bead at `sentinel.sh:318/319/321`. Run that loop's work four ways on one
fixture in one minute: as it stands, 31,619 ms; batched into two calls, **371 ms**; through
an in-process Go client with the loop left in, 1,632 ms; in-process *and* batched, 152 ms.
The third row is the one the bead was cut for. **A harness rewritten around an in-process
store client, with the N+1 still in it, is 4.4x slower than a forty-line change to one bash
loop.** The in-process client is real and cheaper to build than anyone thought — bd ships a
public Go SDK, `beads.OpenBestAvailable`, that opens the same store with the same model
code, and a working probe against it took a day — but its payoff is 219 ms per sentinel pass
once batching has taken the calls away. The levers are not additive: batching deletes the
population the other three would accelerate.

## What I found

### 1. bd ships an in-process store client, and it is Go, not Rust (OBSERVED)

`beads_cgo.go:25` exports `OpenBestAvailable(ctx, beadsDir) (Storage, error)`, documented as
"the CONSUMER surface: it opens and uses bd's own storage". It dispatches on the same
`metadata.json` the CLI reads, so it serves embedded Dolt through cgo *and* a dolt sql-server,
and it carries bd's own model invariants because it is bd's own code
(`01-in-process-probe.md`).

This removes the cost sp-krxs8 could not establish and the one that made option D look like
weeks of work. A Rust client cannot link cgo and would have to speak MySQL to a dolt server
and re-implement the model above SQL; **a Go client links the model itself**. The probe is
133 lines, built in 4 m 53 s cold and 27 s warm, and the only friction was the SDK telling me
its own contract. What it cannot do: the public `Storage` surface exposes no raw SQL, so two
of CHECK 4's three calls have no in-process equivalent through it.

So the honest framing of lever D is *an in-process **Go** store client*, not a Rust one. The
language question and the store-access question are orthogonal, and only the second one is
worth money.

### 2. Where a call's time actually goes (OBSERVED)

Production store, server mode, 3,083 issues, same ids and same session
(`02-per-call-phases.md`):

| | CLI process | in-process, store held open |
|---|---|---|
| process start to first store call | 80 ms CPU (`bd --version`) | 48 ms CPU |
| open the store | — (inside the 90 ms below) | 33 ms wall / 5.8 ms CPU, **once** |
| `show <id>` | 100 ms CPU / 160 ms wall | 2.6 ms CPU / 12.6 ms wall |
| `count --status open` | 90 ms CPU / 150 ms wall | 0.4 ms CPU / 1.6 ms wall |
| `list --limit 0` (3,083 rows) | 320 ms CPU / 620 ms wall | 62.6 ms CPU / 217 ms wall |

Row counts were checked call by call against the CLI — 67, 3083, 33, 63, 67, 7 — so the
speedup is not the in-process path answering a smaller question. The first draft of this
table *was* that mistake: `ListRequest.Limit` nil is a default page of 50, not `--limit 0`,
and it flattered the SDK by 4.4x on one row until the row count exposed it.

Two things follow. **On a small server-mode call, roughly 80-90% of the CLI's CPU is process
start** — which is sp-krxs8's finding, independently reproduced. And **the ratio collapses as
the result grows**, from 220x on `count` to 2.9x on a 3,083-row dump, because rendering rows
is real work either way. The harness's calls are overwhelmingly the small kind.

Embedded mode is a different animal and the difference decides the suites' fate. In-process,
store already open, on a 500-issue embedded fixture: `show` costs **234 ms of CPU**, against
2.6 ms for the same call in-process against a dolt server. The CLI on the same fixture costs
610 ms, so the process boundary is worth 2.6x there — not 38x. **Holding the store open
removes the open; it does not make embedded Dolt fast.** Median CLI-over-in-process ratio
across twelve embedded calls: 3.8x.

### 3. One loop is 96.3% of a sentinel pass (OBSERVED)

sp-krxs8 could not instrument a real pass because a real pass mutates the production store,
and named this as the falsifier for its own recommendation. It runs fine on a fixture
(`03-sentinel-pass-census.md`): a server-mode fixture from `testdb.sh`, the sub-program stubs
`test-poison.sh` already uses, one real `sentinel.sh` under `bash -x`, and every bd call
attributed to the `sentinel.sh` line that caused it.

    bd calls in the pass: 187        (60 dispatchable beads, every check enabled, 38.8 s)
          60   32.1%  sentinel.sh:318    [label x60]
          60   32.1%  sentinel.sh:319    [sql x60]
          60   32.1%  sentinel.sh:321    [sql x60]
           7    3.7%  everything else in the pass

The threshold was 30%. The result is 96.3%, and sp-krxs8's "3 calls per bead" is confirmed
exactly rather than approximately.

The matcher that produced it had to be proved first. Its first version reported **1** call in
this pass, because bash repeats PS4's first character per call depth *and* because
`$(bdq label list "$id" 2>/dev/null)` throws away the xtrace of the exec inside it. "This
loop is not the problem" is what a broken matcher says, and it is the answer that stops
anyone looking (`law-absence-needs-a-positive-control`).

**Not established:** the fixture had no stale leases and no closed-but-unlanded beads, so
CHECK 2 and CHECK 5 ran over empty sets and contributed 5 calls between them. In production
those loops have work. 96.3% is therefore an upper bound on CHECK 4's share — with 9x of
headroom over the 30% the falsifier needed.

### 4. The four levers, on one workload, in one minute (OBSERVED)

`poc/lever-bench.sh` runs CHECK 4's real work over 60 beads four ways against one fixture
(`04-four-way-lever-bench.md`). Wall, at this session's 70% quota:

| | wall | vs. the code as it stands |
|---|---|---|
| **A** CLI, per bead — 180 processes | 31,619 ms | — |
| **B** CLI, batched — 2 processes | **371 ms** | **85x** |
| **C** in-process, per bead — 1 process, 60 calls | 1,632 ms | 19x |
| **D** in-process *and* batched — 1 process, 1 call | 152 ms | 208x |

and the three readings that matter:

- **A → B: 85x for a bash change.** No new process, no new dependency, nothing to supervise.
- **B → D: 2.4x, or 219 ms per pass**, for everything an in-process client costs to build and
  own.
- **C is 4.4x slower than B.** At this backlog size. C's fixed cost (≈50 ms start + ≈50 ms
  open) amortises over N, so at production's 22 dispatchable beads the gap narrows to about
  1.8x — still the wrong side of a forty-line diff.

That last row answers the bead's question directly, and it answers Ryan's hypothesis in the
negative. **The process boundary is not the harness's bd cost. The number of calls is.**

### 5. Startup and a resident bd, for completeness (OBSERVED, mostly by sp-krxs8)

- **Faster bd startup.** sp-krxs8 built the patch and got a null result: bd's ~90 ms is 689
  packages' `init()`, diffuse, with no hot spot, and the 53 ms item profiling pointed at was
  an `inittrace` artifact. One datum this spike adds: the probe, which links the same storage
  layer but none of bd's CLI surface, starts in **48-52 ms of CPU against bd's 80-90 ms**.
  Observed. That ~40 ms is *plausibly* the CLI shell — cobra, the syntax highlighter, the
  integrations — but I did not attribute it package by package, and sp-krxs8's null result is
  the reason not to trust an attribution that has not been built and measured.
- **A resident bd.** `bd serve` exists and sp-krxs8 measured it at 33 ms per call through
  curl, ~9 ms server-side, against the CLI's 191 ms. It is **refused in embedded mode** —
  *"bd serve requires a Dolt SQL server"* — so it cannot touch the suites, which are the
  80%-of-wall case the bead leads with. My in-process figure of 12.6 ms wall for the same
  `show` is the same lever without the second process.

### 6. What each lever is worth to the box, per day (INFERRED from the measurements above)

Rescaling by sp-krxs8's measured quota ratio (314/170 = 1.85 from this session's 70% to the
sentinel's 40%), at the 22 dispatchable beads sp-krxs8 measured as the day's backlog, over
`spira-sentinel.timer`'s 720 passes a day:

| | per pass | per day | saved vs. today |
|---|---|---|---|
| today (66 calls) | 21.4 s | **4.30 h** | — |
| after batching | 0.69 s | 8.2 min | 4.16 h/day |
| after batching + in-process | 0.28 s | 3.4 min | a further **4.8 min/day** |
| in-process without batching | ≈1.24 s | 14.9 min | 4.05 h/day |

The 21.4 s/pass and 4.30 h/day are derived by a different route from sp-krxs8's 20.7 s and
4.1 h/day, from a different measurement, and agree to within 5%. That is the strongest
evidence either document has that the figure is real.

**The suites.** They run embedded fixtures, so neither a resident bd (refused) nor server-mode
speedups apply, and an in-process client is worth only the 3.8x median of section 2 — *and
only to code that is rewritten to use it*, which the bash under test never will be. But
batching helps them by a route that needs no suite change at all: a suite that drives a
sentinel pass pays CHECK 4's loop. In this spike's own traced pass, **31.6 s of a 38.8 s
pass — 81% — was that one loop**, which is the same 81% the bead reports for `test-census`'s
time inside bd. Every suite that drives a sentinel pass gets most of that back for free.

## Options

Costs are wall at the sentinel's 40% fence where a unit is named; engineering estimates are
mine, from having built the probe and read the call sites.

### A. Batch CHECK 4's N+1 (sp-f1m7f, already filed by sp-krxs8)

- **Benefit:** 85x on the loop, measured four ways on one fixture. ≈4.16 h/day of sentinel
  wall, ≈81% off any suite that drives a pass. Grows with the backlog.
- **Cost:** ~40 lines in one file plus a regression test seen to fail first. **3-5 hours.**
- **Risk:** the attempts arithmetic drives the poison valve; getting it wrong poisons live
  beads or fails to poison stuck ones. Contained by the control sp-krxs8 demonstrated and
  this spike re-ran: the batched `GROUP BY` must return a non-zero count for a bead known to
  have events, and agree bead-by-bead with the per-bead path.
- **Reversibility:** one commit.

### B. An in-process Go store client for the harness

- **Benefit:** 2.4x on top of A — 219 ms per sentinel pass, ≈4.8 min/day. Without A, 19x,
  but still 4.4x worse than A alone.
- **Cost:** the probe was a day; a library the harness depends on is **a week at least**, and
  then the callers. Every harness script that reads bd would have to become a Go program or
  call one — that is not a refactor, it is a rewrite of the thing the bash *is*. Two
  specifics that are not estimates: the SDK exposes no raw SQL, so `bd sql` callers have no
  in-process path today; and a second binary linking bd's storage layer is a second thing
  that must be rebuilt in lockstep with the bd pin, which `spira/build-bd.sh` records as
  having caused two outages.
- **Risk:** high, and structurally worse than it looks: it makes the harness depend on bd's
  *library* API rather than its CLI. A CLI contract is the one upstream is careful with.
- **Does nothing for the suites** except by rewriting the code they test.

### C. A resident bd (`bd serve`)

- **Benefit:** 158 ms per server-mode call (sp-krxs8). Nothing after A, which deletes the
  calls.
- **Cost:** 3-5 days, a supervised service with a CPUQuota, a readiness probe that is not
  `/healthz`, and a rewrite of `bdq` to speak HTTP with a CLI fallback (sp-krxs8, option B).
- **Risk:** a new always-on process on a box that has already had a polling loop starve
  production (`law-fence-loops-on-shared-hardware`); pinned to a version-specific HTTP
  surface. **Refused in embedded mode**, so zero for the suites.

### D. Cut bd's startup

- **Benefit:** unknown and probably ~40 ms per call, from the probe's 48 ms against bd's
  80-90 ms. At 66 calls a pass that would be ≈4.3 s/pass, ≈52 min/day — *if* the calls still
  existed, which after A they do not.
- **Cost:** an upstream fork of bd's CLI surface. Not ours to make, and
  `law-never-file-upstream` says where that work does not go.
- **Risk:** sp-krxs8 already built one startup patch on a well-evidenced hypothesis and got
  a null result. Assume this one is a null result too until someone builds it.

## Recommendation

**Do A. Do not do B, C or D — and specifically, do not spend the Rust migration on this.**

A is the only lever with a measured benefit, a cost in hours, no new process, no new
dependency and a one-commit revert. It is already filed as **sp-f1m7f**; this spike adds the
falsifier that confirms it and the number that orders it against the alternatives.

On the framing the bead was cut with: **the measurements do not support "migrating components
to Rust is the biggest reduction" for this cost.** Not because Rust is wrong — because the
harness's bd time is a *call-count* problem, not a *per-call* problem, and the language a
caller is written in does not change how many times it asks. The in-process option is real,
is Go rather than Rust, and is worth about five minutes of box time a day once the loop is
fixed. If the Rust migration is worth doing it is worth doing for other reasons; this is not
one of them, and betting it on a 2.4x that arrives after a 85x would be paying weeks for the
smaller half.

**The one load-bearing assumption:** that CHECK 4's `3 x N` remains the dominant block of bd
calls in a sentinel pass as the production backlog grows. It is 96.3% on a 60-bead fixture
with CHECK 2 and CHECK 5 idle, and it scales linearly with the dispatchable set while the
other checks do not scale with it at all — so the share should rise, not fall. But it was
measured on a fixture whose other loops had nothing to do.

## The falsifier

**Trace one sentinel pass on a fixture that has stale leases, closed-but-unlanded beads and
branches — the state CHECK 2 and CHECK 5 exist for — and recount.** If CHECK 4's share falls
below about 50%, the other loops are the larger population, sp-f1m7f is a partial fix, and
the batching work should be scoped to whichever loop the recount names. `poc/sentinel-callcount.sh`
takes the recount; only the fixture needs enriching.

Three more, each falsifying one specific claim, cheapest first:

- **If `bd --version` costs much less than 80 ms of CPU**, bd's dependency graph changed, the
  CLI's per-call floor moved, and every ratio in section 2 needs re-deriving.
- **If a batched `GROUP BY` and the per-bead path ever disagree on a bead with a non-zero
  reopen count**, option A is not a pure optimisation and sp-f1m7f needs the arithmetic
  re-derived rather than re-expressed.
- **If the harness ever stops shelling out to bd for reads** — if it becomes a Go or Rust
  program holding the store — then section 4's row C is the relevant row and not row A, and
  the in-process lever's value goes from 2.4x-after-batching to the whole of it. That is a
  decision, not a discovery: this document says do not make it for this reason, not that it
  could never be made.

## What I could not establish

- **CHECK 4's share of a pass whose other loops have work.** See the falsifier. What is
  established is the absolute figure: 3 bd calls per dispatchable bead, 187 for a 60-bead
  backlog.
- **An in-process equivalent of `bd sql`.** The public `Storage` surface has no raw-SQL
  method, so rows C and D of the four-way benchmark cover the label half of CHECK 4's work
  only. The two `events` aggregates would need re-expressing against whatever the storage
  layer exposes, or the SDK extending. Unmeasured, and a real cost on option B.
- **Where bd's extra ~40 ms of startup goes**, beyond observing that a binary linking the
  storage layer without the CLI surface starts in 48 ms rather than 88 ms.
- **Embedded-mode scaling past 500 issues.** The embedded figures here are a 500-issue
  fixture; sp-krxs8 measured a 3,064-issue one at 570 ms for `bd ping` and flagged that it
  was built by chunked import rather than grown. Neither of us has measured a naturally-grown
  embedded store.
- **The suites' aggregate saving from A**, as a number. 81% of one traced sentinel pass was
  CHECK 4; how much of the ten slowest suites' wall is sentinel passes is read from the code,
  not counted.

## Related

- `why-a-bd-call-costs-250ms.md` — sp-krxs8, the twin spike. Decomposes the call; this one
  costs the levers and runs its falsifier. Its recommendation survives.
- sp-f1m7f — the batching fix. This spike is its second, independent justification.
- sp-nyfng — the fixture-baseline lever. Untouched by anything here: the suites' embedded
  floor is store-open plus an expensive per-query cost, and section 2 says an in-process
  client only moves the first of those.
