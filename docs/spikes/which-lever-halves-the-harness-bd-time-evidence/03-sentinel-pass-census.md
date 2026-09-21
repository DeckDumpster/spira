# 3 — Every bd call in one real sentinel pass, by call site

This runs the falsifier `../why-a-bd-call-costs-250ms.md` named for its own recommendation:

> Shim `bd` inside one real sentinel pass on a fixture, tag each call with its call site,
> and count. If CHECK 4's `3 x N` is less than about 30% of the pass's bd calls, the
> recommendation is wrong.

`poc/sentinel-callcount.sh` builds a server-mode fixture through `testdb.sh`, seeds it with
N dispatchable beads, stubs the sub-programs the way `spira/test-poison.sh` already does, and
runs one real `sentinel.sh` under `bash -x` with
`PS4='@@${BASH_SOURCE##*/}:${LINENO}@@ '`. `poc/analyze-xtrace.py` attributes every bd call
to the `sentinel.sh` line that caused it. No production store is touched.

## Result — 60 dispatchable beads, every check enabled

    fixture: <sptest fixture> (mode=server)
    seeded: 60 open
    sentinel rc=0  wall=38.8s  xtrace lines=4423

    bd calls in the pass: 187

    by sentinel.sh call site:
          60   32.1%  sentinel.sh:318    [label x60]
          60   32.1%  sentinel.sh:319    [sql x60]
          60   32.1%  sentinel.sh:321    [sql x60]
           1    0.5%  sentinel.sh:24     [migrate schema x1]
           1    0.5%  sentinel.sh:87     [list x1]
           1    0.5%  sentinel.sh:103    [show x1]
           1    0.5%  sentinel.sh:125    [ready x1]
           1    0.5%  sentinel.sh:126    [list x1]
           1    0.5%  sentinel.sh:204    [reclaim x1]
           1    0.5%  sentinel.sh:1292   [ready x1]

    by subcommand:
         120   64.2%  sql
          60   32.1%  label
           2    1.1%  list
           2    1.1%  ready
           1    0.5%  migrate schema
           1    0.5%  show
           1    0.5%  reclaim

**180 of 187 calls — 96.3% — are CHECK 4's three per dispatchable bead.** The threshold was
30%. The seven remaining calls are the whole rest of the pass.

The three lines are `sentinel.sh:318` (`bdq label list "$id"`), `:319` (`attempts_of`) and
`:321` (`reopens_of`), the last two reaching `bd sql` directly in `lib.sh:1826` and
`lib.sh:1837`.

## The matcher had to be proved before its silence was believed

The first version of the analyzer counted `timeout <n> bd` exec lines and reported **1** call
in this pass. Two reasons, both of which silently undercount:

1. bash repeats PS4's first character once per call depth, so `@@lib.sh:60@@` arrives as
   `@@@@lib.sh:60@@` and a fixed-prefix match misses it.
2. xtrace writes to stderr, and CHECK 4 calls the wrapper as
   `_labels="$(bdq label list "$id" 2>/dev/null)"` — which discards the trace of the exec
   inside it. All 60 label calls were invisible to a matcher that reads exec lines.

The fixed matcher counts at the wrapper line and de-duplicates the wrapper's own exec. A
matcher that reports 1 where the truth is 187 reads as "this loop is not the problem", which
is the conclusion that stops anyone looking.

## What this run does NOT establish

The fixture had no in-progress beads, no stale leases, no closed-but-unlanded beads and one
empty branch. CHECK 2/2b/2c and CHECK 5 therefore ran with empty sets and contributed 5
calls between them; in production those loops have work and will contribute more. The exact
figure — 3 bd calls per dispatchable bead, 187 for a 60-bead backlog — holds regardless;
**96.3% is an upper bound on CHECK 4's share of a pass**, and the falsifier's 30% floor has
9x of headroom above it.

A pass with the skips `test-poison.sh` sets (`SPIRA_SKIP_RECLAIM=1`,
`SPIRA_SKIP_CLOSED_CHECK=1`) made 182 calls, 180 of them the same loop.
