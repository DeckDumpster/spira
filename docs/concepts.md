# Core concepts and vocabulary

## Bead

A bead is one work item. Its content (title, body, labels, priority, dependency edges,
comments) lives in the beads database, read and written through the `bd` CLI. Its *state*
lives separately, in the lifecycle store `spira_lifecycle`, one row per bead, and is changed
only by sending an event to the lifecycle service (`spira-lc`). Content is free-form; state is
a machine that refuses illegal moves.

Every state change is a compare-and-swap. An event names the state it expects and the row
version it saw; if either is stale the machine refuses and the row is returned unchanged. A
refusal is a normal answer, not an error. Every applied *and* refused event is appended to an
immutable `event` table, which is the truth the rows are folded from.

`lifecycle/src/bead.rs` defines these states:

| State | Meaning |
|---|---|
| `READY` | Filed and waiting for a claim. A bead starts here. |
| `WORKING` | An aeon holds it under a lease. |
| `SUBMITTED` | The aeon handed in a branch tip; not yet certified. |
| `CERTIFIED` | A gate or round passed that exact tip. |
| `IN_DELIVERY` | Cut into a round (or a PR or push, in other land modes) and on its way to the landing ref. |
| `REWORK` | Sent back (red gate, ejected from a round, withdrawn base). Claimable again. |
| `LANDED` | Terminal. The work is on the landing ref. |
| `DONE` | Terminal. Closed with no branch to land (an aeon's `work done`, e.g. research or filing work). |
| `DROPPED` | Terminal. Closed without landing: no branch was ever committed, or the work was declared unwanted. |
| `SUPERSEDED` | Terminal. Replaced by another bead (also used for an identical patch). |
| `OPEN` | A container (an epic). Never claimable and never `READY`; closed by bd, not by this machine. |

Terminal means terminal: no event moves a row out of `LANDED`, `DONE`, `DROPPED` or
`SUPERSEDED`, for any actor. Two narrow exceptions are written into the machine: the
classifier may re-label a terminal row whose reason is its own no-evidence default, and the
`delivered-correction` actor may move a `DROPPED` row to `DONE`.

### Transitions

The machine enforces actors only where noted below; the last column says which component
normally sends the event.

| From | Event | To | Normally sent by |
|---|---|---|---|
| `READY` | `Claim` | `WORKING` (records holder, persona, lease, stack) | `aeon` |
| `REWORK` | `Claim` | `WORKING`; refused if the proposed stack equals the stack the red work was built on | `aeon` |
| `WORKING` | `Submit {tip}` | `SUBMITTED` | `work submit` (tip read from branch `spira/<id>`) |
| `WORKING`, `READY` | `Done` | `DONE` | `work done`; filing and grooming paths |
| `WORKING` | `Release` / `HolderDead` | `READY`, or `REWORK` if the claim was taken from `REWORK` | `sentinel` (lease expired), `aeon` teardown |
| `WORKING` | `Renew`, `Phase`, `Disposition` | `WORKING` | the holder only for `Renew`/`Phase` (phases `claimed`, `building`, `session`, `teardown`, forward only); teardown and watchdogs for `Disposition` (`lapsed` < `thrash` < `slain`; a weaker word never overwrites a stronger) |
| `SUBMITTED` | `GatePass {tip, gate_key}` | `CERTIFIED` | `landing-pass` / `gate-worker` (per-branch gate); `batcher` (round) |
| `SUBMITTED`, `CERTIFIED` | `GateRed {tip, reason}` | `REWORK` | gate, `sift`, round ejection |
| `SUBMITTED` | `GateInfra {tip}` | `SUBMITTED` (the infrastructure faulted, not the tree) | gate |
| `SUBMITTED`, `CERTIFIED` | `Deliver` | `IN_DELIVERY` | `landing-pass`, `batcher`, `queue` |
| `IN_DELIVERY` | `Delivered {merge_sha, proof}` | `LANDED` | `queue land-local`, `landing-pass` (pr/push) |
| `IN_DELIVERY` | `Returned {reason}` | `REWORK` | `batcher` (member ejected from a round), `queue` |
| `IN_DELIVERY` | `Requeued {tip}` | `SUBMITTED`, `CERTIFIED` (if certified at that tip), or `REWORK` (if the tip was ejected red) | `queue`, `spira-claim` |
| `SUBMITTED`, `CERTIFIED` | `Restack {tip, stack}` | `SUBMITTED` (certification and screen verdict dropped) | stacked-dependent cascade |
| `SUBMITTED`, `CERTIFIED` | `Sifted {tip}` | same state; records the tip the screen passed | `sift` |
| any non-terminal | `ContentOnBase {proof}` | `LANDED` (the content is already on the landing ref) | `sending`, content reconciliation |
| any non-terminal | `Supersede {by}` | `SUPERSEDED` | `groomer`, `sift` |
| any non-terminal | `Drop {reason}` | `DROPPED` (`closed-no-branch` or `unwanted`) | `groomer`, the operator |
| any non-terminal | `Hold`, `Unhold` | same state; adds or removes a hold | see Holds |
| any non-terminal | `Express`, `Unexpress` | same state; sets or clears the jump-the-line flag | the operator or Concierge |
| any | `BaseWithdrawn {prereq, tip}` | `READY` gains a `wait` hold; `WORKING`, `IN_DELIVERY`, `REWORK` note it and stay; `SUBMITTED`, `CERTIFIED` go to `REWORK` | machine cascade when a prerequisite moves backward |
| any | `PrereqLanded {prereq}` | same state; the prerequisite leaves `stack` | machine cascade |
| non-`WORKING`, non-terminal | `Reclassify {state, rule}` | the named state | actor `classifier` only |
| `REWORK` (manual-held) | `Resubmit {tip}` | `SUBMITTED` | break-glass only |
| non-terminal | `Absolve {tip}` | clears a red recorded against `tip` | break-glass only |

Rules the machine enforces itself:

- **Tip invariant.** A certification is a claim about one exact commit. `Submit` and
  `Resubmit` require a full 40-hex object name; `GatePass`, `GateRed`, `GateInfra` and `Sifted`
  must name the row's current tip or are refused. A new tip on a `CERTIFIED` row drops it back
  to `SUBMITTED`.
- **A red tip cannot be resubmitted.** After a gate or round ejects a tip as red (any reason
  except `no-rebase` and `timeout`, which say nothing about the tree), `Submit` and `Deliver`
  of that same tip are refused; the aeon must change the tree.
- **Holder-only events.** `Renew` and `Phase` from anyone but the current holder are refused,
  so a reaped aeon's late renewal cannot extend its successor's lease.
- **Stacks.** A `Claim` records the certified tips of the bead's unlanded prerequisites
  (`stack`, with `stack_depth`); a depth over the configured cap is refused, and a `Submit`
  while a prerequisite tip has been withdrawn is refused.
- **Break-glass moves** (`Resubmit`, `Absolve`, and `Unhold` of a manual hold on a red-ejected
  `REWORK` row) are refused unless they carry a reason, the evidence (command and result) and
  an evidence tip equal to the tip admitted.

Gate-red reasons are `suites-failed`, `syntax`, `policy-violation`, `no-rebase`, `timeout`, `confine` and
`phantom-bead-id`. Return reasons are `pr-closed-unmerged`, `pr-changes-requested`, `push-rejected`,
`batch-ejected`, `batch-ejected-red` and `base-withdrawn`.

### Holds

Holds are orthogonal to state: they suspend any non-terminal state without losing it. A row
carries a set of them, and the claim selector (`spira-claim`) offers only `READY` and
`REWORK` rows whose holds are a subset of `{wait}`.

| Hold | Placed for | Lifted by |
|---|---|---|
| `poison` | the bead failed enough attempts that retrying stops | a rule-bound unpoison (`groomer` / `spira-claim`), or the operator |
| `ask` | an aeon asked the operator a question | only a `Reply` event carrying the reply's message id, or an ask withdrawal. A close, drop, supersede or `Unhold` on an asking row is refused: an escalation closes on the reply, never on marker text |
| `wait` | blocked on an unlanded dependency, a timed snooze (`snooze-until:<epoch>`), or a withdrawn base | the graph (when the blocker lands) or the clock |
| `manual` | a person's hold; it must carry a reason saying who lifts it and how. A bare bead id (that is a dependency edge) or a snooze (that is a `wait`) is refused | whoever the reason names |

After a bead is rejected twice, its next claim runs on the persona's `escalation_model` (if
configured) with the rejection history in the brief; a third rejection on that model holds it
for the Concierge instead of another claim.

### Asks

An **ask** is an escalation to the operator. It is its own machine (`lifecycle/src/ask.rs`),
never a row on the bead machine, so nothing that lists or claims work can see one. It starts
`OPEN` and leaves by exactly one exit, each recording who closed it and the words that did (an
empty actor or quote is refused):

| Exit | Terminal state | Meaning |
|---|---|---|
| `Answer {quote, channel}` | `ANSWERED` | the operator's own answer through a channel (pane, mail, chat) |
| `TakeDefault {quote}` | `DEFAULT_TAKEN` | dismissed: the default the ask carried was executed |
| `Withdraw {quote}` | `WITHDRAWN` | moot |

An ask may point at a work bead; closing the ask lifts that bead's `ask` hold.

## Delivery and batch machines

Two more machines live beside the bead machine in `lifecycle/`, in the same database.

**Delivery** (`delivery.rs`) is the sub-state of a bead's `IN_DELIVERY`, one machine per land
mode. States: `QUEUED`, `BATCHED`, `PR_OPEN`, `PUSHING`, `EXITED`, plus three local-mode publish
states reached from `EXITED`: `PUBLISHING`, `PUBLISHED`, `PUBLISH_RED`. Every delivery ends in
one of three exits back to the bead: `Delivered`, `Returned`, `Requeued`. A new delivery is a
new row.

**Batch** (`batch.rs`) is a round, queue modes only, keyed by batch id. States: `STAGED`
(assembled behind a running round, not yet cut), `OPEN`, `CI_RUNNING` (phases `build` and
`suites`), `GREEN`, `LANDING`, `DEPLOYING`, `ATTRIBUTING`, `REBUILDING`, and the terminal
`LANDED`, `SETTLED`, `ABANDONED`, `DISCARDED`. The main line is
`OPEN → CI_RUNNING → GREEN → LANDING → DEPLOYING → LANDED`. A red pass goes to `ATTRIBUTING`,
where members at fault are ejected and the rest return to `OPEN` for another pass, or the batch
settles; if the base moves under a green batch it goes to `REBUILDING` and back through
`CI_RUNNING`. A batch event and the matching exit event on each member's delivery row are
applied in one transaction.

## Persona, fayth, aeon

A **fayth** is a persona *definition*: a file `spira/chamber/<name>.fayth` (shell-style
`NAME=value` lines parsed in-process by `spira-config`; any other construct is refused). It
fixes the **partition** of the work graph the persona may claim (`FAYTH_LABELS`, ANDed, minus
`FAYTH_EXCLUDE_LABELS`), the statutes it must read (`FAYTH_MEMORY_PREFIXES`,
`FAYTH_STATUTE_CORE`), the tools it may use (`FAYTH_TOOLS`), and how it is woken and bounded
(`FAYTH_MAX_CONCURRENT`, `FAYTH_LEASE_MINUTES`, `FAYTH_HEARTBEAT_SECONDS`, `FAYTH_LANE`,
`FAYTH_ELASTIC`). Its **brief**, the instructions the model receives, is the matching
`spira/chamber/<name>.md`, rendered with `{{TOKEN}}` substitutions. The model, lease, tool list
and system-prompt mode of a running system come from the `[persona.<name>]` table in
`spira.toml` (a `model` is required: a persona with no model is refused); the `.fayth` values
seed that table when a config is first converted.

An **aeon** is one summoned instance of a persona: a runner process (`aeon <fayth>`) that
claims one bead, builds its worktree and branch, renders the brief, runs the model session (a
headless `claude -p` stream), renews its lease while the trace file grows, and accounts for the
outcome at teardown. It runs as a transient user unit named `spira-aeon-<fayth>-<n>`. Inside
the session the model has no `bd`, no credentials and no database access: its environment is an
explicit allow-list, and everything it does to state goes through the `work` client, which
talks to `spira-lc` over a Unix socket. Each `work` verb is checked against a per-persona allow
table in the service, so a persona can do only what its role permits.

Shipped personas (`spira/chamber/`):

| Persona | Role |
|---|---|
| `builder` | The only persona that writes code. Claims plan beads, implements, submits. Elastic: takes whatever pool capacity is left. |
| `ops` | Diagnostician. Claims incident beads and sweeps, matches them against the SOP shelf, files the fix. Read-mostly tools. |
| `czar` | Acts on merge-queue stall escalations. |
| `groomer` | Graph hygiene: splits, merges duplicates, closes beads whose premise is gone, corrects mislabelled lanes. Graph-only. |
| `mimir` | Retrospective: reads the failure distribution, names recurring classes, files work to end them. |
| `warden` | Read-only judge of whether landed work is actually in force; attributes anomalies. |
| `mender` | Judges one failing suite of a round pass and mends the member at fault, inside a machine-enforced deadline. |
| `batcher` (the Judge) | Settles a double-red or CI-only red a round could not attribute. |
| `spike` | Researches one question to a costed feasibility document. |
| `concierge` | The operator's own long-lived session; claims nothing (see below). |
| `archivist` | Brief only (`archivist.md`), no `.fayth`: run by the `archivist` binary to rescue a full session's unfinished business before it is cleared. |

## Lane

"Lane" has two meanings, both in code.

1. **Scheduling lane** (`SPIRA_LANES`, `FAYTH_LANE`). A persona that declares a lane draws
   capacity from its own `FAYTH_MAX_CONCURRENT` instead of the shared pool
   (`SPIRA_MAX_AEONS`), so builders can occupy the whole pool without starving the rest.
2. **Repository lane** (`lanes` in a repository's registry row): which kinds of bead a
   repository admits (`plan`, `incident`, `groom`, `mimir-sweep`, `spike`, `czar-trigger`).

Partitions are expressed as bead labels, each a configuration key rather than a literal
(`SPIRA_PLAN_LABEL`, `SPIRA_INCIDENT_LABEL`, `SPIRA_CZAR_LABEL`, `SPIRA_GROOMER_LABEL`,
`SPIRA_SPIKE_LABEL`, `SPIRA_MENDER_LABEL`, `SPIRA_WARDEN_LABEL`, `SPIRA_MIMIR_LABEL`,
`SPIRA_BATCH_JUDGEMENT_LABEL`), so persona predicates, the sentinel and every other reader
agree on what a label means. `SPIRA_SCOPE_LABEL`, when set, is ANDed into every persona's
predicate to restrict Spira to one project's beads.

## Land mode and landing ref

Each managed repository has a registry row giving its **land mode** and its **landing ref**
(`base`): the ref work must reach to count as landed. Closed is not landed: a bead is landed
only when its commit is on the landing ref.

| Mode | What landing does |
|---|---|
| `push` (default) | rebase, confine, gate, merge in a private worktree, push to the base |
| `pr` | rebase, force-push, open or refresh a pull request; the PR's CI is the gate |
| `hold` | rebase, confine, gate, note "gated and held"; never advances the base |
| `queue` (alias `queue.forge`) | certify tips; rounds go through the forge's merge queue |
| `queue.local` | certify tips; a round lands by fast-forwarding a *local* branch with no forge round trip on the critical path; a publish step later carries it to the forge |

The walk-through in [architecture](architecture.md) follows `queue.local`, the harness's own path.

## Round, batch, gate, sentinel

- **Round** (also **batch**; a **pass** is one test run of a round). The unit that lands: a set
  of `SUBMITTED`/`CERTIFIED` beads merged together, proven by running the repository's full
  test corpus on a round VM, then landed as one fast-forward. Cut by the `batcher` binary. A
  red round is attributed to members; those are ejected and the rest retried.
- **Gate** (`gate` crate, `gate.steps`). The per-branch certification: the branch merged onto
  the landing ref is judged by the repository's own gate definition, read *from the tree being
  judged* (fences such as lint and the lifecycle guard, then a budgeted suite selection run
  through `testenv`). Verdicts: `PASS`, `FAIL`, `BASE_FAIL` (the base itself fails its own
  gate, so nothing is charged to the branch) and `NO_VERDICT` (the gate could not decide). Only
  `FAIL` charges the bead. A gate `PASS` makes a bead `CERTIFIED` but is not what
  `queue land-local` accepts for a landing: that needs a round `GREEN` for the exact tree.
- **Screen** (`sift`). The cheap pre-round check on every `SUBMITTED`/`CERTIFIED` bead: does it
  merge cleanly onto the base, does another bead carry an identical patch (the duplicate is
  superseded), does it meet its declared target and identity rules. A failing bead is sent
  back with an evidence note (capped at three send-backs); a conflict can be handed to a
  mender first.
- **Sentinel** (`sentinel`). The loop's heartbeat. One pass every two minutes reconciles the
  graph toward its desired state: reaps dead holders, applies poison, frees stale blocks,
  dispatches the landing worker, sweeps landed branches, and summons aeons. A fast path
  (`sentinel --summon-only`, every minute and after every aeon unit) refills a freed slot
  without a full pass.

## Release

The running system executes one thing: an immutable, read-only directory
`<releases>/<sha>/` with a `MANIFEST`, holding every workspace binary in `bin/`, the shell
layer and the unit templates. The `release` binary is the only thing that builds one
(`build`), checks one (`verify`), switches the system onto one (`activate`, which restarts
units) or reverts (`rollback`). `<releases>/current` is a symlink to the release in force.
Nothing the running system uses points at a checkout, worktree or `target/` directory.

## Statute and SOP

A **statute** (`law-<slug>`) is a standing rule, enacted with `rule enact` and kept in the
beads database as a memory, with a git-backed page regenerated from it. It must name its
mechanism (`--tool`, `--mechanism`, or `--tool none --reason`); `rule audit` checks that the
tools statutes name exist in the activated release. Personas receive the statutes selected by
`FAYTH_MEMORY_PREFIXES`, with `FAYTH_STATUTE_CORE` rendered in full. An **SOP** (`sop-<slug>`)
is a runbook for a recurring incident, written with `sop write`, matched to incidents with
`sop match`, and read by `ops`. Builders read statutes only.

## The Concierge and the operator

The **operator** is the human. The operator's mailbox (`mail`) is the channel for an
escalation; the class of an ask (`permissions`, `policy`, `destructive`, `design`) decides
whether it goes to the operator or first to the Concierge.

The **Concierge** is the operator's long-lived session (`concierge.sh start`, kept up by
`concierge.timer`; `concierge-guard` mails the operator if its inbox waiter lapses). It is not
summoned for a bead and claims none. Its brief tells it to reach Spira by *filing* beads, not
by claiming them or running aeons itself. It also holds the break-glass powers: an ungated
landing requires `SPIRA_LAND_UNGATED_BY` to name the Concierge or the operator, plus evidence
in the head's own history.
