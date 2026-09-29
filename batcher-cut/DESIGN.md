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

## 4. Concurrent attribution (sp-hvtgs)

Design: brain `wiki/projects/spira/designs/gate-unit-round-integration-2026-09-29.md`, "The
round attributes its own reds, while it runs", work item 5.

### 4.1 Intent

A round attributes its own reds **while its corpus runs**, starting on the first streamed red
(per Ryan, 2026-09-29), so that when the corpus ends the round already has a decision: land,
eject the owners, or block. It replaces the old loop (full corpus → `attribute.sh` → full
corpus again on the reduced membership), which cost a full corpus per iteration. The measured
hand technique it automates (rounds 105–123) was a targeted rerun of the red suite without one
member, median 165 s.

### 4.2 Contract

**Per red suite S, as soon as its result lands** (testenv writes `<S>.result` after `<S>.out`,
so a present, parseable result is final):

1. **Plain rerun** — S alone on the round tree with every member. Green → S is **flaky**:
   recorded, charged to nobody, nothing more runs for S.
2. Red → S's **attribution reruns** are queued: the **base** run (S with every member removed),
   then one run **without each suspect X** (X's removal set is X plus every round member stacked
   on X, since their tips carry X's commits). Suspects are ordered: members whose diff touches
   S's covered paths first, then the rest, each group in round order.
3. Outcomes:
   - S green without X → **X owns S**. Queued (not yet started) runs for S are cancelled. A run
     already in flight that also comes back green names a co-owner, unless its removal set
     strictly contains an owner's (a prerequisite whose dependent is the owner is not blamed).
   - Base run red → S is a **base red**: charged to nobody.
   - Every run red but the base green (two members each break S alone) → S is
     **unattributed**: the round is blocked and filed for Ops, as before.
   - A rerun that faulted (the harness, not the suite) is retried once; a second fault makes S
     unattributed. A fault never reads as green or red.

**Slot budget.** A round has `slots` = `SPIRA_BATCHER_ROUND_SLOTS` (default `maxpar + 4`). The
corpus always has its `maxpar`: while it runs, attribution may hold `slots − min(maxpar,
corpus suites not yet finished) − attribution runs in flight`. So attribution never takes a slot
from the corpus, uses the 4 extra slots from the first red, and inherits every corpus slot the
corpus tail frees.

**At corpus end**, once every red is settled:
- every owner is ejected with the suites named against it through the existing eject path
  (`eject_member`: bead reopened `queue-eject-local`, `land_mark EJECTED <tip> <suites>` —
  what the re-entry check reads); a member stacked on an owner leaves with it;
- **only the owners' red suites** are re-run on the survivors' tree (the release build of that
  tree, whose binaries land). Green → the survivors land. Red → that verification is the next
  iteration's corpus, attributed the same way;
- flaky and base reds do not block landing; a base red is filed for Ops (the old
  `file_local_red_incident(..., "base")` path, deduped by incident.sh);
- an unattributed red blocks the round and is filed for Ops (`"unattributed"`).

**Record.** One TSD row per red, family `round-attribution`: `repo`, `round`, `iteration`,
`suite`, `outcome` (`owner|flaky|base|unattributed`), `owner` (comma list, empty unless owner),
`attribution_secs` (red landed → settled), `reruns` (runs launched for S), `settled_before_corpus_end`
(`true|false`). Item 7 reads these. The `batch-round` row keeps `attribution_seconds` (the
longest per-red attribution wall of the round) and `regreen_seconds`.

### 4.3 Where the code lives

| piece | crate | IO |
|---|---|---|
| `attrib::Attributor` — the per-suite state machine, slot arithmetic, suspect order, final decision | `batcher` (pure) | none; the clock is an argument |
| `attrib::suspect_order`, `attrib::case_glob` — `# covers:` matching with select.sh's `case` semantics (`*` crosses `/`; `file#fn` matches on the file) | `batcher` | none |
| `drive::drive` — the loop: stream main results in, launch jobs out, until settled | `batcher-cut` | through the `RoundRunner` trait only |
| `vm::VmRunner` — `RoundRunner` over `round-vm run --attr-spool` | `batcher-cut` | git, round-vm, the spool files |

```rust
pub trait RoundRunner {
    fn start_main(&mut self, suites: &[String]) -> Result<(), String>;
    fn poll_main(&mut self) -> Result<MainPoll, String>;       // newly final results; done(rc)
    fn launch(&mut self, job: &Job) -> Result<(), String>;      // build the tree, submit
    fn poll_jobs(&mut self) -> Vec<(u64, JobResult)>;           // Green | Red | Fault
    fn now(&self) -> u64;                                       // seconds
    fn wait(&mut self);                                         // one poll interval
}
```

**Round-vm without the spool.** If round-vm exits without writing `corpus.done` (one that
predates §2.2a, or a stand-in), its exit code stands for the corpus's; the reruns it never
answers fault, so any red is unattributed and blocks the round — never a silent green.

**Trees.** A job's tree is the round's base with every member *not* in its removal set merged in
round order (`merge_member`, the round's own commit subject), committed on
`spira/batcher-attr/<repo>-<round>-j<job>`. The build a job asks round-vm for is `artifacts`
(reuse the round's release binaries) when no removed member changed anything but `*.sh`/`*.md`
files, else `aeon` (a fresh debug build of that tree); the survivors' verification is `round`.

### 4.4 Tests (`cargo test -p batcher -p batcher-cut`, each well under a second)

Pure core (`batcher::attrib::tests`): suspect order by covers; flaky on plain green; owner;
base; unattributed; co-owners and the stacked-prerequisite rule; cancellation of queued runs;
fault retry; slot arithmetic (never above the budget, none taken from the corpus). Driver with a
fake runner (`batcher-cut::drive::tests`): attribution starts on the first streamed red before
the corpus ends; the slot budget holds at every tick; owner/flake/base classification; multiple
owners all ejected; survivors re-run only the red suites; nothing ejected when every red is a
flake or base red. The end-to-end proof (`e2e`, `#[ignore]`: `cargo test -p batcher-cut --bin batcher e2e --
--ignored --nocapture`) runs a real fixture repo with three members, real git trees, the real
`VmRunner` and spool, and a stub round-vm that runs tiny fixture suites locally.
