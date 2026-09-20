# Why a bd call costs 250 ms, and which hot loops pay it most

Spike for sp-krxs8. Measured and written 2026-09-20. Every number below was taken on the
operator's box that day; the raw output is preserved verbatim in
`why-a-bd-call-costs-250ms-evidence/` and cited by file. Absolute paths are rewritten to
their config keys, because this repository ships and `spira/inventory.sh` gates it.

Proof-of-concept branch: `spike/sp-krxs8-poc`, unmerged. It carries the patched-bd
experiment of section 4 and the benchmark scripts.

## The question

From the bead, as the concierge session framed it after measuring:

> Find out why a single bd call costs ~250 ms at p50 and cut the hot loops that make that
> the dominant cost of the gate. About 80% of a suite's wall time is spent inside bd
> processes, not in bash.

What would count as an answer: the 250 ms decomposed into named parts with measurements —
how much is process start, how much is opening the store, how much is the query — a count
of where the calls come from, and a costed recommendation about a resident bd or a batched
query interface, *before* anyone builds one. The deliverable is a written finding, not a
refactor.

## The answer in one paragraph

**The 250 ms is not a bd number. It is a bd number divided by a CPUQuota.** A bd call
against the live store does about 110 ms of CPU work, and ~90 ms of that happens before bd
has looked at the store at all: it is the cost of running 689 packages' `init()` functions
in a 202 MB binary. The store work is the remaining 10–30 ms, and it is the same 10–30 ms
whether the query returns one row or all 3,064. Wall time is then that CPU divided by the
quota of the unit making the call — measured at 119 ms at 100%, 170 ms at 70%, **314 ms at
the 40% that `spira-sentinel.service` and `spira-suites.service` both set**, and 887 ms at
20%. The bead's observed p50 of 251 ms sits exactly where a harness whose passes run at
40% and whose aeons run at 70% should sit. I tried to cut the 90 ms and **failed, and the
failure is the most useful thing in this document**: the obvious culprit that profiling
pointed at turned out to be a profiling artifact, and a patched bd built to remove it was
indistinguishable from stock. There is no hot spot in bd's startup. What there *is* is a
harness that calls bd three times per bead in a loop that runs every two minutes —
`sentinel.sh` CHECK 4 — where **177 calls costing 29,731 ms are replaced by one GROUP BY
costing 178 ms, a 167× reduction**, measured, with the batched result checked bead-by-bead
against the per-bead path. Fix the caller, not the callee.

## What I found

### 1. Ninety milliseconds happen before bd looks at anything (OBSERVED)

`bd --version` opens no store, reads no config and runs no query. It costs **80–90 ms of
CPU** (`01-process-start.md`). That is not dynamic linking — `ldd` lists three objects and
`LD_DEBUG=statistics` puts the whole loader at ~30 microseconds. It is Go package
initialisation:

    $ GODEBUG=inittrace=1 bd --version | grep '^init ' | awk '{s+=$5} END{print s, NR}'
      108.4 ms across 689 packages

689 packages are linked into bd and every one of their `init()` functions runs before
`main()`, on every invocation, whatever the subcommand. `GOGC=off` removes about 10 ms of
it (~12%) and nothing else moves it. `GOMAXPROCS` is not implicated — the box reports 16
CPUs to Go inside a sub-one-CPU cgroup, which looked like a classic pathology, but pinning
it to 1 or 2 changes nothing measurable (`01-process-start.md`).

### 2. The query is not the cost — but which backend is (OBSERVED)

Against the live server-mode store, every command costs the same (`02-backend-comparison.md`):

| command (server mode, 3,064 issues) | CPU |
|---|---|
| `bd ping` | 100 ms |
| `bd count` | 100 ms |
| `bd label list <one id>` | 110 ms |
| `bd sql` (one-row aggregate) | 100 ms |
| `bd list --limit 0 --json` (3,064 rows) | 120 ms |

A one-row lookup and a full dump are within 20 ms of each other, and `bd --version` is
90 ms of that. **Roughly 80% of a server-mode bd call is startup.**

Embedded mode is a different animal, and conflating the two is how one number came to
describe two things. There is no resident process, so every invocation opens the store
itself, and that open scales with store size:

| `bd ping` (embedded) | CPU |
|---|---|
| empty fixture | 180 ms |
| 3,064-issue fixture | 570 ms |

`bd label list` for a *single* id against the 3,064-issue embedded fixture costs 1,580 ms
CPU, because the open dominates the lookup entirely. This is why suites are slow: a real
suite run under a counting shim spent **65% of its wall clock inside bd at 630 ms a call**
(`07-suite-shim.md`), against 167 ms for comparable server-mode work.

One caveat I could not remove: the 3,064-issue embedded fixture was built by importing a
live export, which commits in chunks of 250, so its Dolt history is not shaped like a
naturally-grown store's. The direction is solid; the magnitude is not verified against a
store that grew normally.

### 3. Wall time is CPU time divided by the quota (OBSERVED)

This is the measurement that actually answers the bead's question. The same `bd count`,
run through `systemd-run` so each batch sits in its own cgroup (`03-cpu-quota-scaling.md`):

| CPUQuota | wall per call | who runs there |
|---|---|---|
| 100% | 119 ms | — |
| 70% | 170 ms | aeon sessions |
| **40%** | **314 ms** | `spira-sentinel.service`, `spira-suites.service` |
| 20% | 887 ms | — |
| 10% | not measured; >1 s by extrapolation | `spira-gate-check.service` |

At 20% the cost is worse than linear because the CFS quota period is 100 ms and a call
needing 110 ms of CPU must wait out whole periods. This session's own cgroup was throttled
in 546 of 1,205 periods while measuring.

The consequence runs both ways, and it is the lever this whole spike turns on: **a
millisecond of bd CPU saved is 2.5 ms of sentinel wall, and a call deleted is worth 314 ms
there.** Deleting calls beats making them faster.

### 4. I tried to cut the 90 ms. It did not work, and the reason matters (OBSERVED)

`inittrace` pointed at something that looked like a gift: `olebedev/when/rules/nl` at
**53 ms** — the Dutch-language rules of a natural-language date parser — on the startup
path of every bd call. The source corroborated the mechanism. `when`'s own package `init()`
eagerly builds four parsers (English, Russian, Brazilian Portuguese, Dutch), and beads uses
none of them: `internal/timeparsing/parser.go` builds its own lazily via `when.New(nil)`.
beads already does the lazy thing; the library's package init defeats it.

So I built it (`05-poc-patched-bd-null-result.md`). Two binaries from the same v1.2.1
source and toolchain, one with a `go.mod` replace onto a `when` fork with those three rule
packages and their init blocks deleted. The patch demonstrably took effect — the packages
vanish from the trace. And:

    bd-baseline   total init = 108.0 ms across 689 packages
    bd-patched    total init = 109.3 ms across 686 packages

    bd-baseline --version   n=15  cpu_ms p50=90
    bd-patched  --version   n=15  cpu_ms p50=80

Nothing. Re-running `inittrace` on the *unmodified shipped* binary showed why: the ~50 ms
item **moves between packages run to run** — 53 ms on `when/rules/nl` cold, 59 ms on
`chroma/v2/styles` warm. `inittrace` reports wall clock per init, so it charges page-fault
time (a 202 MB binary being read in) and GC pauses to whatever package happens to be
running. Read as per-package CPU cost it is simply wrong, and wrong in the most inviting
direction: a large number attached to an obviously-useless package.

**The honest finding is a negative one.** bd's 90 ms is the diffuse cost of 689 `init()`
functions; the stable contributors are all small (chroma/lexers ~10 ms, go-mysql-server
`sql/variables` ~4.6 ms, `sql/information_schema` ~4.2 ms, `main` ~4 ms). There is no
surgical fix, only `GOGC=off` at ~10 ms and a dependency amputation that is an upstream
project. Had I estimated instead of built, this document would have promised ~55 ms per
call across the whole harness and been wrong.

### 5. The harness calls bd three times per bead, every two minutes (OBSERVED + INFERRED)

`sentinel.sh` CHECK 4 loops over the dispatchable set and makes three bd calls per bead
(`04-nplus1-sentinel-check4.md`):

    for id in $dispatchable; do
        _labels="$(bdq label list "$id" ...)"      # 1
        n="$(attempts_of "$id")"                   # 2 -> bd sql, one issue_id
        _requeues="$(reopens_of "$id")"            # 3 -> bd sql, one issue_id

Calls 2 and 3 are single-row aggregates keyed on one `issue_id`; one `GROUP BY issue_id`
serves the whole set. Call 1 is not merely batchable but **redundant**: `dispatchable_open`
already pulls each bead's full JSON and reads `i.get("labels")` to apply the partition
exclusions — then prints only `i["id"]`. The labels were in hand and thrown away.

Measured against the live store on a 59-bead set:

    177 calls (3 per bead)                     29,731 ms
    1 GROUP BY returning the same three columns   178 ms      ->  167x

The batched result was checked bead-by-bead against the per-bead path: 20 of 20 agree, and
`sp-kogm` returns `reopens=17` on both — so the batched query is demonstrably able to
return a non-zero value and is not an all-zeros false all-clear
(`law-absence-needs-a-positive-control`).

Sizing it at the day's real backlog: `dispatchable_open` returned **22** beads, so 66 calls
a pass. `spira-sentinel.timer` fires every 2 minutes at 40%, where a call is 314 ms:

    66 x 314 ms          =  20.7 s of every 120 s pass   (~17% of the interval)
    x 720 passes/day     =  4.1 h/day of wall, ~1.45 CPU-hours/day

and it grows linearly with the open-work backlog — the 59-bead set would be 55.6 s a pass.

**What is inferred, not observed:** CHECK 4's *share of a whole sentinel pass*. I drove the
loop's queries directly rather than instrumenting a real pass, because a real pass mutates
the production store. The 167× on the loop is measured; "this loop is the largest single
block of bd calls in the pass" is read from the code.

### 6. A resident bd already exists — and cannot serve the suites (OBSERVED)

bd v1.2.1 ships `bd serve`, whose own help names this exact use: *"the same work surface
the CLI answers, for automation clients that would otherwise fork a bd subprocess per
call."* It works, and it is fast (`06-resident-bd-serve.md`):

| | per call |
|---|---|
| CLI `bd show <id> --json` | 191 ms |
| HTTP `GET /v0/beads/issues/<id>`, fresh curl each time | 33 ms |
| …of which curl's own process start (`/healthz`, no DB touch) | 24 ms |
| → server-side cost of the lookup | **~9 ms** |
| CLI `bd list --limit 0 --json` (3,064 rows) | 202 ms |
| HTTP equivalent | 82 ms |

So 5.8× for a bash caller spawning curl, ~20× for a client that holds a connection.

But:

    $ bd-embedded serve
      Error: operation "serve" not supported by the embedded-dolt backend:
      bd serve requires a Dolt SQL server; this workspace uses embedded Dolt

**The resident option cannot touch the test suites**, which all run embedded fixtures and
are the 80%-of-wall case the bead leads with. It helps only the server-mode callers —
sentinel, landing, census, the aeons — which is precisely the population that option A
below removes the calls from.

## Options

Costs are wall-clock at the sentinel's own 40% fence unless stated, and engineering
estimates are my own, from having read the call sites.

### A. Batch the N+1 loops in the harness

Delete call 1 (labels are already in the pass); replace calls 2 and 3 with one `GROUP BY`
over the dispatchable id list, keeping the per-bead functions for their other callers.

- **Benefit:** 29,553 ms per CHECK 4 on a 59-bead set; ~20 s of every 120 s sentinel pass
  today; ~1.45 CPU-hours/day returned to a one-core-equivalent box. Measured, 167×.
- **Cost:** one file, ~40 lines of bash plus a regression test. 3–5 hours including
  seeing the test fail against the unbatched code first
  (`law-a-regression-test-must-be-seen-to-fail`). No new process, no new dependency, no bd
  change, nothing to supervise or fence.
- **Risk:** the attempts arithmetic (claims − closes − thrash/unjudged requeues) is
  load-bearing for the poison valve, and `_attempts_sql_query`'s three recorded constraints
  still hold. Getting it wrong poisons live beads or fails to poison stuck ones. This is
  contained by the positive control already demonstrated in section 5: the batched and
  per-bead paths must agree on a fixture containing a bead with a non-zero reopen count.
- **Reversibility:** a revert of one commit.

### B. Run `bd serve` and have the harness talk HTTP

- **Benefit:** ~158 ms saved per server-mode call (191 → 33), ~182 ms for a client holding
  a connection. Applies to every server-mode call in the harness, not just CHECK 4's.
- **Cost:** a systemd unit with a CPUQuota (`law-fence-loops-on-shared-hardware`), a
  readiness probe that is not `/healthz` (which stays green while the database is
  unreachable — its help says so), auth or a documented loopback trust boundary, and a
  rewrite of `bdq` to speak HTTP with a CLI fallback for everything the API's 38
  capabilities do not cover. There is no OpenAPI document served on this build, so routes
  must be pinned by probing. Estimate 3–5 days, and a permanent new failure mode: a dead
  or wedged resident makes every caller fail at once.
- **Risk:** highest of the four. A new always-on service on a box that has already had a
  polling loop starve production (`law-fence-loops-on-shared-hardware`), pinned to a
  version-specific HTTP surface on a dependency this project does not control and has
  already been burned by twice (see `spira/build-bd.sh` on v1.2.2).
- **Does nothing for the suites.** Refused in embedded mode.

### C. Do nothing; raise the quotas instead

- **Benefit:** `spira-sentinel.service` at 80% rather than 40% halves its wall time for a
  one-line change. Zero engineering.
- **Cost:** the CPU has to come from somewhere. Section 3's table is a description of a
  box that is already oversubscribed — load 4.6–6.1 throughout, 45% of this session's own
  periods throttled. The quotas are not arbitrary; they are what keeps a read-only
  convenience from starving production.
- **Risk:** this is exactly the move `law-alerts-must-be-actionable` warns about in another
  register — widening a budget rather than measuring the thing that matters. The N+1 stays,
  and it grows with the backlog, so the relief is temporary and the next backlog spike
  spends it.

### D. An in-process store client (the Rust roadmap option)

- **Benefit:** in principle the ~9 ms floor of option B without the separate process.
- **Cost:** could not be established honestly, and I am saying so rather than guessing. bd
  reaches embedded Dolt through cgo; a Rust client cannot link that and would have to speak
  MySQL to a dolt server — which makes it option B's client half without option B's server,
  and leaves embedded-mode suites exactly where they are. Costing it properly means
  building a Rust MySQL client against the beads schema and re-implementing the model
  invariants that `bd` enforces above SQL. Weeks, not days, and the bead's own framing
  already concedes the point: *"a Rust harness that still shells out to bd inherits all of
  it."*
- **Risk:** the schema is not a stable interface. `spira/build-bd.sh` records two outages
  caused by binary/schema skew; a second implementation of the model doubles that surface.

## Recommendation

**Do option A. Do not do B, C or D now.**

It is the only one of the four with a measured benefit, a cost in hours rather than days,
no new long-lived process, no new dependency on an interface this project does not control,
and a revert path of one commit. Filed as **sp-f1m7f** with the measurements and the shape
of the fix.

It also orders the decision correctly: A *removes* most of the calls that B would make
faster. Doing A first tells you whether B is still worth anything — if the sentinel's bd
call count drops from 66 a pass to single digits, B is buying 158 ms on a population that
no longer exists. Doing B first buys a permanent service to accelerate work that should not
happen at all.

**The one load-bearing assumption:** that `sentinel.sh` CHECK 4's `3 × N` calls are the
largest single block of bd calls in a sentinel pass. The 167× on the loop is measured; its
share of the pass is read from the code, because instrumenting a real pass means mutating
the production store.

Separately, and not part of this recommendation: the suites' cost is a *different* problem
with a different fix. Their 630 ms per call is embedded store-open, which neither batching
nor a resident bd addresses. That lever is fixture work — sp-nyfng — and this spike found
nothing that changes its priority.

## The falsifier

**Shim `bd` inside one real sentinel pass on a fixture, tag each call with its call site,
and count.** If CHECK 4's `3 × N` is less than about 30% of the pass's bd calls, the
recommendation is wrong: batching it would be a rounding error on the sentinel's cost, the
calls would be somewhere I did not look, and sp-f1m7f should be re-scoped to wherever they
actually are before anyone writes the GROUP BY.

Three more things that would each falsify a specific claim, cheapest first:

- **If `bd --version` ever costs much less than 80 ms of CPU**, section 1 has stopped
  holding — bd's dependency graph changed — and the arithmetic in section 3 needs redoing
  before it is quoted. One command, and `rerun.sh` prints it.
- **If `spira-sentinel.service`'s CPUQuota changes**, every wall-clock figure in this
  document is wrong by the ratio, including the 20.7 s per pass and the 4.1 h/day. The CPU
  figures survive; the wall figures do not. Re-derive, do not re-measure.
- **If a naturally-grown embedded store opens in ~180 ms rather than ~570 ms at 3,000
  issues**, the caveat in section 2 was load-bearing, the suites' 630 ms per call is
  something other than store-open scaling, and sp-nyfng is aimed at the wrong thing.

## What I could not establish

- **bd call counts for a full landing pass or a full sentinel pass.** Both mutate the
  production store. Section 5's figures come from driving the loop's queries directly.
- **The magnitude of embedded store-open scaling on a naturally-grown store** — see
  section 2's caveat.
- **`bd batch` / `issues.batchApply` (write batching in one transaction).** Not measured;
  every hot loop this spike found is reads.
- **`bd serve` under its own CPUQuota.** It was run in the foreground inside the aeon's 70%
  fence. A supervised one needs its own, and its cost there is unmeasured.
- **Option D's real cost**, for the reasons given under it.

## Found along the way, filed not fixed

Running the gate's own suites over this document's evidence turned up an unrelated defect,
filed as **sp-shhy5**. `SPIRA_SCOPE_LABEL` derives from `basename "$SPIRA_REPO"`, and
`SPIRA_REPO` is a fact about where `conf.sh` sits — so in a worktree it is the *worktree's*
directory name. Run from an aeon worktree under `env -i`, which is how a gate must run
anything, `schema.sh name scope` returns the bead id. `literal-lint.sh` builds its pattern
list from `schema.sh`, so it flagged every line of this spike's evidence that named its own
bead and reported it as a configured label name. The same file, same tree, passes with the
ambient environment and fails without it — the shape
`law-gates-run-in-a-clean-environment` exists to catch.

The evidence files here are `.md` rather than `.txt` as a result: the lint exempts prose,
these are prose, and the rename makes the branch independent of what the tree it is linted
in happens to be called.

## Related

- sp-f1m7f — the batching fix, filed by this spike. P1.
- sp-shhy5 — the scope-label derivation defect above, filed by this spike. P1.
- sp-nyfng — the fixture-baseline lever. Removes the 15 s `bd init`; this spike confirms it
  does not touch the per-call floor, and that the suites' floor is embedded store-open.
- `wiki/notes/where-the-gate-time-goes-2026-09-20.md` — the concierge session's measurement
  that produced this bead.
