# gate-check — design

Replaces the logic of `spira/gate-check.sh` (271 lines) with one Rust binary, `gate-check`.
`spira/gate-check.sh` stays as the one entry point every caller already names —
`systemd/spira-gate-check.service`'s `ExecStart`, and the suites that test it — the same shim
pattern `gate/DESIGN.md` established for `gate.sh`, resolving `SPIRA_GATE_CHECK_BIN` through
`conf.sh` and `exec -a "$0"`-ing it. Bead: sp-ubw2o (epic sp-8m1at, wave 1 of the Rust
rewrite order).

## Intent

`gate-check` is the two-minute timer's leg that resolves `gh:run` gates: it discovers each
open gate's CI run, checks whether that run finished, and unblocks the bead either way (a
green run resolves the gate; a red or cancelled one is resolved too, so the bead re-enters
the queue instead of sitting behind a gate `bd` will never close on its own). Two more legs
piggyback on the same two-minute cadence because they need the same `gh` round-trips: filing
a bead for a suite GitHub Actions annotated as flaky, and raising one to P1 when a suite is
red twice in a row on `main`. A last leg pulls each recently completed run's TSD rows into
the local `run/tsd/` tree.

**Every JSON payload this binary reads — `bd gate list --json`, `bd show --json`, `gh api
.../annotations`, `gh run list --json` — is parsed with `serde_json`, not `python3`.** The
bash shelled to five separate inline `python3 -c` scripts for exactly this; this binary reads
the same fields with the same defaults (a field that is not present or not the expected shape
reads as absent, never a hard error — DESIGN.md "Non-goals" explains why that stays true even
here) and removes `python3` from this timer's dependency list entirely.

## Contract (unchanged from the bash)

```
gate-check.sh          # every caller, unchanged — systemd, the suites
gate-check [--home <spira-dir>] [--db <path>]   # what gate-check.sh execs
```

Five legs, in this order, each best-effort (one leg's failure never stops the next):

1. **Discover.** For every `pr`-mode repository in the repo-map, for every distinct branch
   named in an open, unbound (`await_id` empty) `gh:run` gate, `bd gate discover --branch
   <branch>` from inside that repository's checkout. `push`/`hold`-mode repositories are
   skipped — they never open a pull request, so there is no CI run to discover.
2. **Check.** `bd gate check --type=gh:run`, captured and printed verbatim (the operator's
   own read of this timer's log is this text, unchanged).
3. **Stuck count.** Lines containing `no run ID specified` — gates `bd` cannot resolve and
   never will — are counted and named separately from `0 resolved`, which is otherwise
   ambiguous between "nothing to do" and "every gate is permanently wedged".
4. **Escalated runs.** Each `⚠ <gate-id>: ESCALATE` line: find the bead the gate's own
   description names as blocked (`blocking (<id>)`), `bd gate resolve <gate-id>` so it
   re-enters the queue, and emit `ci.failed`.
5. **Flaky-suite beads.** Every `flaky suite` warning annotation on a recently completed run
   in `SPIRA_FLAKY_GH_REPO`: one P2 bead per suite via `bead.sh`, skipped while an open one
   already exists for that suite.
6. **Red-twice beads.** Every `red-twice suite` failure annotation on a recently completed,
   failed `main`-branch run: one P1 bead via `bead.sh`, carrying the failing `FAIL` lines and
   the commit range since the last green `main` run; an existing lower-priority bead for the
   same suite is raised to P1 with a note instead of duplicated.
7. **TSD ingest.** Every recently completed run: `tsd-ingest.sh <repo> <run-id>` (idempotent
   per run id on its own — DESIGN.md "Non-goals").

### Environment (unchanged names)

`SPIRA_FLAKY_GH_REPO` (legs 5-7 are no-ops without it and without `gh` on PATH),
`SPIRA_BD`/`SPIRA_DB` (via the `lib.sh` seam, same as every other crate in this rewrite).

## Non-goals

* **Porting `bead.sh` or `tsd-ingest.sh`.** Both are called exactly as the bash called them —
  external programs, not logic this crate owns. `bead.sh` in particular carries its own
  contract (persona partitions, repo resolution) that belongs with the harness's filing tool,
  not duplicated here (`CLAUDE.md` "Filing work").
* **Changing what `bd gate check`/`bd gate discover`/`bd show` mean**, or the dedup rule
  (skip filing while an open bead already carries the exact title). Ported as specified.
* **A stricter JSON schema.** The bash's inline `python3` reached for a field with `.get(...,
  default)` and moved on when the shape was not what it expected (a `try/except:
  print(nothing)` around the whole script, several times over). This binary keeps that
  tolerance — a `gh api` response the endpoint changed out from under it, or a `bd --json`
  call that returned something unexpected, degrades to "found nothing this tick", not a
  crash that takes the timer down. This is not a general license to guess: a `bd`/`gh`
  invocation that cannot even run (bad credentials, `gh` missing) is failed closed exactly
  as it was — a leg that produced no output produces no beads and no resolutions, silently,
  which is the same shape the bash had and this binary does not widen it.

## Design

`engine.rs` holds every pure decision over already-fetched JSON/text: which branches need
discovery, which lines are stuck vs. escalated, which gate id names which blocked bead,
whether a suite's flaky/red-twice bead already exists, the exact bead bodies and titles.
`ports.rs` names the boundary (`bd`, `gh`, `git`, `bead.sh`, `tsd-ingest.sh`, the `lib.sh`
repo-map seam); `real.rs` implements it. `main.rs` sequences the five legs, each wrapped so
one's failure is logged and does not block the next — the same `|| true` shape the bash used
throughout.

## Decisions — accreted accidents dropped, not ported

* **`bd gate list --json` is fetched once per run, not once per pr-mode repository.** The
  bash's discover loop re-ran the same query inside its per-repo `while`, re-filtering an
  identical, unchanged snapshot of open gates for every repository — nothing between those
  calls could change what they returned. This binary fetches it once and reuses the branch
  list across repositories; the set of `(repo, branch)` discover attempts is identical.
* The `python3` removal (Intent, above) is a dependency change, not a behavior change: every
  default and fallback the inline scripts had is preserved in the Rust equivalents.

## Test strategy

* **Unit (`cargo test -p gate-check`):** each JSON parser against a fixture payload and
  against a payload missing/malformed in the way the bash's `try/except` already tolerated;
  the gate-id and blocked-bead-id extraction regexes; the dedup rule for flaky/red-twice
  titles; the red-twice bead body (FAIL lines, commit range) against a fixed input; the
  stuck-count line match.
* **Integration:** a fake `World` recording every `bd`/`gh`/`bead.sh` call this binary would
  make, driven through one full pass with a mix of pr/push/hold repos, an escalated gate, a
  flaky annotation and a red-twice annotation, asserting the exact sequence and arguments —
  the same shape `gate`'s and `rebase-stale`'s fakes use.
* **Parity:** stub `bd`/`gh` on `PATH` (the same technique `test-gate-check-*.sh` already
  used) and diff this binary's output and its `bd`/`gh` call log against the bash's, for a
  discover pass, an escalate pass, a flaky-annotation pass and a red-twice pass.
