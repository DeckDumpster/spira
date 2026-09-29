# batcher-cut — the merge-queue round cutter's IO seam

## 1. Intent

`batcher cut <repo>` replaces batch.sh's cut (sp-jzfog). The pure `batcher` crate decides
what a round is. This crate gathers that core's inputs and carries out its decision. The
inputs come from landstate, git, testenv-batch and round-vm, the forge and the bead store.
Carrying it out means merges, the round's suites, the PR, the open-batch record and
`land_mark`. `batcher judgement-ci` is verdict.sh's CI-only attribution producer (sp-lomk3).

## 2. Contract

- **In:** `SPIRA_HOME` (lib.sh, forge.sh), `SPIRA_RUN` (landstate, queue dir), `SPIRA_DB`,
  and the repo map through lib.sh (`repo_land`, `repo_root`, `spira_landref`), never
  spira.toml. The one spira.toml key it reads is `spira.lifecycle_enforce` (§3), through
  spira-config, and only when the environment does not pin it.
- **Out:** a pushed `spira/queue/<stamp>` branch and a PR (or a force-pushed stack onto the
  open one). Also the open-batch record `$SPIRA_QUEUE_DIR/<repo>/open` (key=value, read by
  verdict.sh and queue-watch), `land_mark BATCHED|RED`, and the local verdict. Events go to
  stdout.
- **Exit:** 0 when the cut ran or had nothing to do; non-zero with a one-line reason on
  stderr.

## 3. Lifecycle switch

**Finding (operator, 2026-09-28):** the lifecycle machine was never deployed on this host.
There is no `spira_lifecycle` database, no `spira_lc` grant, and no service or socket.
**Decision:** `lifecycle_enforce` is THE switch for everything that touches the lifecycle
machine.

**Resolution** (`spira_config::lifecycle_enforce`, the aeon crate's rule):
- `SPIRA_LIFECYCLE_ENFORCE` wins: `1`/`true` is on, and anything else, including empty, is off.
- Else the typed `spira.lifecycle_enforce`.
- Else **off**.

`SPIRA_LC_BIN` existing on disk never turns it on. `Env::lc_enforce` carries the result.
`lcq` itself refuses when it is off, so a forgotten call site still cannot reach the binary.

| | **off** (production today) | **on** |
|---|---|---|
| spira-lc | **never run**: no probe, no `create-bead`, `cut`, `stack` or `show` | run |
| before the lock | nothing | `lc_probe`: `spira-lc list --state IN_DELIVERY`, parsed. Unreachable, unset, or not JSON → `batcher cut <repo>: lifecycle_enforce is on and spira-lc is unreachable (…) — refused, nothing changed; …`, non-zero exit |
| new round (`cut_new_round`) | open-batch record **without** `batch_id`/`version`: the pre-sp-o7nbr.4 record | `create-bead` per member, then `cut`. On success `batch_id`/`version` go on the record. A CAS refusal is **best-effort additive** (sp-o7nbr.4's contract): a loud `LIFECYCLE:` line on stderr, and the round proceeds without them |
| stacked round (`stack_round`) | no `stack`; the record is rewritten without `batch_id`/`version` | `stack` when the record carries them; a refusal is loud and drops them, as today |
| member stacks in the certified pool | unstacked. Stacking exists only in the lifecycle machine; there is no legacy record of it | `spira-lc show <id>`'s `stack`. A failed or unparsed read is loud on stderr and reads as unstacked, because the probe already proved the machine reachable |
| `judgement-ci` | no lifecycle call in any era | same |

verdict.sh's `_lc_land_batch`/`_lc_settle_batch` read `batch_id`/`version` off the record. It
is off, so they are absent and those calls have nothing to CAS against. verdict.sh must
honour the same switch in its own rewrite.

**Tests** (`io::lifecycle_tests`):
- `off_never_runs_spira_lc_and_records_no_batch_id`: an executable recording spira-lc must
  never be called, and the written record has no `batch_id=`/`version=`.
- `on_runs_spira_lc_and_records_the_batch`
- `on_reads_the_stack_off_the_machine`
- `on_unreachable_machine_fails_the_probe`

**Cutover addition:** none for off, because the unset default is off and conf.sh exports
`SPIRA_LIFECYCLE_ENFORCE=0`. The queue unit or queue.sh must pass that variable through to
`batcher` unchanged.
