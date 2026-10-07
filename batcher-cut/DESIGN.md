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
  spira.toml.
- **Out:** a pushed `spira/queue/<stamp>` branch and a PR (or a force-pushed stack onto the
  open one). Also the open-batch record `$SPIRA_QUEUE_DIR/<repo>/open` (key=value, read by
  verdict.sh and queue-watch), `land_mark BATCHED|RED`, and the local verdict. Events go to
  stdout.
- **Round certificate (queue/DESIGN.md §8 D12):** under queue.local, `finish_local_round`
  writes `verdict=GREEN source=round` (`gate::cert`) to
  `${SPIRA_VERDICTS:-$SPIRA_RUN/verdicts}/trees/<repo>/<tree>` for the tree of the head whose
  full corpus `stabilize_round` just ran green. It does this immediately before `queue
  land-local`, which lands only a certified tree. The head the corpus judged is carried in
  `StableRound.head`, so if the worktree has moved since, the certificate names the green
  tree and land-local refuses the other one. If the certificate cannot be written, the
  round is refused (`verdict=refused`, local verdict red). That is louder than landing on a
  certificate nobody can read back.
- **Exit:** 0 when the cut ran or had nothing to do; non-zero with a one-line reason on
  stderr.

## 2a. Rounds are the batcher's (sp-1oiokx)

The pool is every unheld SUBMITTED or CERTIFIED lifecycle row whose tip is its branch's live
tip: no gate verdict is required, because the round's full suite is the stronger trial. A
non-empty pool cuts at once (`should_cut`: no count, no idle wait; an open batch prepares the
next round). `select_round` is feature-first: the epic root (the id before its first `.`)
shared by the most members, at least two, with the members they stack on or that stack on
them and every express member; else a catch-all of the whole pool. A member that conflicts
on merge waits for the next round. Reds are attributed by §4 and ejected with quoted lines;
a SUBMITTED survivor is certified (`GatePass`, gate key `round:<key>`) just before
`land-local`, and a SUBMITTED owner is returned to REWORK by `GateRed`.

## 3. Lifecycle machine

There is no lifecycle switch: sp-v62vn retired `lifecycle_enforce`, and the machine is the only
record of bead and delivery state.

| | behaviour |
|---|---|
| before the lock | `lc_probe`: `spira-lc list --state IN_DELIVERY`, parsed. Unreachable, unset, or not JSON → the cut is refused, nothing changed, non-zero exit |
| new round (`cut_new_round`) | `create-bead` per member, then `cut`. On success `batch_id`/`version` go on the record. A CAS refusal is **best-effort additive** (sp-o7nbr.4's contract): a loud `LIFECYCLE:` line on stderr, and the round proceeds without them |
| stacked round (`stack_round`) | `stack` when the record carries them; a refusal is loud and drops them |
| member stacks in the certified pool | `spira-lc show <id>`'s `stack`. A failed or unparsed read is loud on stderr and reads as unstacked, because the probe already proved the machine reachable |
| `judgement-ci` | no lifecycle call |

**Tests** (`io::lifecycle_tests`).

## 4. Concurrent attribution (sp-hvtgs)

Design: gate-unit-round-integration, "The round attributes its own reds, while it runs",
work item 5.

### 4.1 Intent

A round attributes its own reds **while its corpus runs**, starting on the first streamed red
(an operator decision), so that when the corpus ends the round already has a decision: land,
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
     **unattributed**. If the base moved since the cut and no owner was found, the round is
     rebuilt on the new base and only the unattributed suites re-run, once
     (`RoundOps::base_moved`; an unmoved base changes no input, so there is no retry). Still
     unattributed → the round is blocked and a judgement bead is filed for the Judge.
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
  what the re-entry check reads); the reopen note names each suite and its first FAIL line
  (`io::suite_first_fail`); a member stacked on an owner leaves with it;
- **only the owners' red suites** are re-run on the survivors' tree (the release build of that
  tree, whose binaries land). Green → the survivors land. Red → that verification is the next
  iteration's corpus, attributed the same way;
- flaky and base reds do not block landing; a base red is filed for Ops (the old
  `file_local_red_incident(..., "base")` path, deduped by incident.sh);
- an unattributed red blocks the round and is filed as a judgement bead (`RoundOps::judge`).

**Record.** One TSD row per red, family `round-attribution`: `repo`, `round`, `iteration`,
`suite`, `outcome` (`owner|flaky|base|unattributed`), `owner` (comma list, empty unless owner),
`attribution_secs` (red landed → settled; empty, never `0`, for a red that never settled — `outcome` then reads `unsettled`), `reruns` (runs launched for S), `settled_before_corpus_end`
(`true|false`). Item 7 reads these (`intent-report`, sp-cln99); the fields are built by
`batcher::attrib::RedRecord::tsd_fields`, unit-tested there. The `batch-round` row keeps `attribution_seconds` (the
longest per-red attribution wall of the round) and `regreen_seconds`.

### 4.3 Where the code lives

| piece | crate | IO |
|---|---|---|
| `attrib::Attributor` — the per-suite state machine, slot arithmetic, suspect order, final decision | `batcher` (pure) | none; the clock is an argument |
| `attrib::suspect_order`, `attrib::case_glob` — `# covers:` matching with the selector's `case` semantics (`suite_select::glob`) (`*` crosses `/`; `file#fn` matches on the file) | `batcher` | none |
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

**Every round-vm exit is typed** (`vm::corpus_end`, sp-dp872): 0/1 ran, 4 workspace build;
2, 3, the wall-bound kill (124/137) and anything else are harness faults — the round is not
judged, no verdict is written. round-vm's stderr goes to `<run>/batch-results/<repo>-<round>/
round-vm.stderr`, and **every fault carries its last 40 lines into the round log**, so the log
says why and not only that.

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
