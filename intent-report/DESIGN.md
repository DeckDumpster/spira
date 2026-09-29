# intent-report — design

Bead sp-cln99 (epic sp-8m1at, design `gate-unit-round-integration-2026-09-29`, item 7).

## Intent

The epic states its Intent as numbers: **median certification ≤ 3 min for a branch that
touches only Rust**, **no-verdict gates < 5 % of gate runs** (23 % when the design was
approved), **≤ 15 min of round time per member defect**, **no suite waiting on a shared test
database**, and certified → landed latency before and after. This command prints those
numbers for a time window, from rows the owning components write, so "did the epic work?" is
one command rather than an afternoon of `awk` over gate.log.

It is read-only and engine-free: it reads JSONL files under `run/tsd/` (and gate.log for
history) and prints. It never writes, never calls `bd`, never touches Dolt.

## Non-goals

* Writing any row. Each measure is produced by the component that owns the event (below).
* A query language, a dashboard, or a daemon. One window, one report.
* Judging a single branch. The numbers are distributions over a window.

## Contract

```
intent-report [--run <dir>] [--since <dur|ISO>] [--until <dur|ISO>] [--no-backfill]
```

* `--run` defaults to `$SPIRA_RUN`.
* `--since` defaults to `24h`. A duration is `<n>s|m|h|d` before now; otherwise an ISO
  timestamp (`YYYY-MM-DDTHH:MM:SSZ` or `YYYY-MM-DD`). `--until` defaults to now.
* Exit 0 with the report on stdout; exit 2 on a usage error or an unreadable `--run`.
* A measure with no rows in the window prints `?` (law: a probe that could not read renders
  `?`, never 0) and its target line reads `NO DATA`, not MET.

## The rows it reads

| measure | family (file) | producer | fields used |
|---|---|---|---|
| gate wall by branch type, no-verdict share | `tsd/gate-run.jsonl` | `gate` (`gate/src/telemetry.rs`) | `ts`, `status`, `reason`, `waited_secs`, `ran_secs`, `wall_secs`, `gate_mode`, `compose`, `branch_type` |
| (history, same measures) | `gate.log` | `gate` (and the bash gate before it) | the meter line, see below |
| attribution time per member defect | `tsd/round-attribution.jsonl` | `batcher` (`RedRecord::tsd_fields`, sp-hvtgs) | `ts`, `outcome`, `owner`, `attribution_secs` |
| bd / Dolt wait per suite | `tsd/suite-timing.jsonl` | `testenv` (+ `bd-meter`) | `ts`, `suite`, `wall_secs`, `bd_calls`, `bd_ms` |
| certified → landed | `tsd/landing-event.jsonl` | landing pass | `ts`, `bead`, `state` (`CERTIFIED`, `LANDED`) |

Unparseable lines and rows without a parseable `ts` are skipped, never fatal. Numeric fields
are accepted as JSON numbers or numeric strings (the batcher writes through `tsd-write
--field-str`); an empty string is "not measured", not 0.

**gate.log backfill.** gate.log lines are
`<ts> <repo> <branch> waited=<n>s ran=<n>s rc=<n>[ <reason>][ compose=<label> phases=…]`.
They are read only for times **before the first `gate-run` row** (no double counting once the
row exists). Status comes from `rc` (0 PASS, 75 NO_VERDICT, 76 BASE_FAIL, else FAIL);
`branch_type` from `compose` (`unit` → rust-only, `fences` → nothing-buildable,
`suites(script)` → bash-touching, otherwise or absent → unknown); `gate_mode` from `compose`
(`suites(mode)` → suites; any other label → unit, since only unit mode composes anything else; absent → unknown). `--no-backfill` skips it.

## Definitions

* **Trial**: one gate-run row (or backfilled line). **Cached** trials (`reason=cached`) are
  counted in the no-verdict denominator but excluded from wall statistics (they did no work).
* **Certification wall**: `waited + ran` of a non-cached trial that ended PASS. The headline
  compares the **median certification wall of rust-only trials** with 180 s.
* **No-verdict share**: NO_VERDICT trials / all trials, against 5 %. The top reasons follow.
* **Attribution per member defect**: `attribution_secs` of `round-attribution` rows with
  `outcome=owner`, against 900 s (median and max). Flaky, base, unattributed and unsettled
  reds are counted beside it.
* **bd wait**: over `suite-timing` rows except `__batch__`: the metered share (rows with
  `bd_calls > 0`), total `bd_ms` against the metered runs' suite wall, and the suites with the most
  `bd_ms`. The Intent's "zero suites waiting on a shared test database" is read here.
* **Certified → landed**: for each bead's first `LANDED` in the window, its latest
  `CERTIFIED` at or before it; median and p90.
* Medians and p90 are interpolated (DuckDB `quantile_cont`), as testenv's `--report`.

## Decisions

* **A separate crate**, not a mode of `gate` or `tsd`: it reads four producers' families, and
  a report inside any one of them would make that crate depend on the others' vocabularies.
* **gate.log is history, the TSD row is the record.** gate.log's format is frozen (other
  programs parse it); a typed row is what new readers read.
