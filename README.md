# Spira

## 1. What Spira is

Spira is an agent harness for software work. You file work items (**beads**) into a store;
Spira summons AI agent sessions (**aeons**) to do them, each in its own git worktree and
branch; and it lands the results onto a repository's landing ref through gated **rounds** that
run the full test corpus before anything merges. Every step between "filed" and "released" is
driven by small timer-activated programs reading and writing explicit state machines, so the
loop keeps going with nobody watching. Humans decide only escalations: the few questions the
loop cannot answer for itself (permissions, policy, destructive actions) are routed to the
operator, and everything else is retried, reworked, held or dropped by rule.

It is a Cargo workspace of single-purpose binaries plus a shell layer in `spira/`, run as
systemd units (`systemd/*.service|timer`) on one Linux box and backed by a Dolt SQL server.

Contents: [concepts](#2-core-concepts-and-vocabulary) · [architecture](#3-architecture) ·
[components](#4-component-reference) · [installation](#5-installation-and-first-run-on-a-fresh-box) ·
[operating](#6-operating-it-day-to-day) · [development](#7-development) ·
[glossary](#8-glossary)

## 2. Core concepts and vocabulary

### Bead

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

#### Transitions

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

#### Holds

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

#### Asks

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

### Delivery and batch machines

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

### Persona, fayth, aeon

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
| `maechen` | Retrospective: reads the failure distribution, names recurring classes, files work to end them. |
| `warden` | Read-only judge of whether landed work is actually in force; attributes anomalies. |
| `mender` | Judges one failing suite of a round pass and mends the member at fault, inside a machine-enforced deadline. |
| `batcher` (the Judge) | Settles a double-red or CI-only red a round could not attribute. |
| `spike` | Researches one question to a costed feasibility document. |
| `concierge` | The operator's own long-lived session; claims nothing (see below). |
| `archivist` | Brief only (`archivist.md`), no `.fayth`: run by the `archivist` binary to rescue a full session's unfinished business before it is cleared. |

### Lane

"Lane" has two meanings, both in code.

1. **Scheduling lane** (`SPIRA_LANES`, `FAYTH_LANE`). A persona that declares a lane draws
   capacity from its own `FAYTH_MAX_CONCURRENT` instead of the shared pool
   (`SPIRA_MAX_AEONS`), so builders can occupy the whole pool without starving the rest.
2. **Repository lane** (`lanes` in a repository's registry row): which kinds of bead a
   repository admits (`plan`, `incident`, `groom`, `maechen-sweep`, `spike`, `czar-trigger`).

Partitions are expressed as bead labels, each a configuration key rather than a literal
(`SPIRA_PLAN_LABEL`, `SPIRA_INCIDENT_LABEL`, `SPIRA_CZAR_LABEL`, `SPIRA_GROOMER_LABEL`,
`SPIRA_SPIKE_LABEL`, `SPIRA_MENDER_LABEL`, `SPIRA_WARDEN_LABEL`, `SPIRA_MAECHEN_LABEL`,
`SPIRA_BATCH_JUDGEMENT_LABEL`), so persona predicates, the sentinel and every other reader
agree on what a label means. `SPIRA_SCOPE_LABEL`, when set, is ANDed into every persona's
predicate to restrict Spira to one project's beads.

### Land mode and landing ref

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

The walk-through in section 3 follows `queue.local`, the harness's own path.

### Round, batch, gate, sentinel

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

### Release

The running system executes one thing: an immutable, read-only directory
`<releases>/<sha>/` with a `MANIFEST`, holding every workspace binary in `bin/`, the shell
layer and the unit templates. The `release` binary is the only thing that builds one
(`build`), checks one (`verify`), switches the system onto one (`activate`, which restarts
units) or reverts (`rollback`). `<releases>/current` is a symlink to the release in force.
Nothing the running system uses points at a checkout, worktree or `target/` directory.

### Statute and SOP

A **statute** (`law-<slug>`) is a standing rule, enacted with `rule enact` and kept in the
beads database as a memory, with a git-backed page regenerated from it. It must name its
mechanism (`--tool`, `--mechanism`, or `--tool none --reason`); `rule audit` checks that the
tools statutes name exist in the activated release. Personas receive the statutes selected by
`FAYTH_MEMORY_PREFIXES`, with `FAYTH_STATUTE_CORE` rendered in full. An **SOP** (`sop-<slug>`)
is a runbook for a recurring incident, written with `sop write`, matched to incidents with
`sop match`, and read by `ops`. Builders read statutes only.

### The Concierge and the operator

The **operator** is the human. The operator's mailbox (`mail`) is the channel for an
escalation; the class of an ask (`permissions`, `policy`, `destructive`, `design`) decides
whether it goes to the operator or first to the Concierge.

The **Concierge** is the operator's long-lived session (`concierge.sh start`, kept up by
`concierge.timer`; `concierge-guard` mails the operator if its inbox waiter lapses). It is not
summoned for a bead and claims none. Its brief tells it to reach Spira by *filing* beads, not
by claiming them or running aeons itself. It also holds the break-glass powers: an ungated
landing requires `SPIRA_LAND_UNGATED_BY` to name the Concierge or the operator, plus evidence
in the head's own history.

## 3. Architecture

### Components and stores

```
                  +------------------- systemd (user + system) -------------------+
                  |  timers: sentinel, summon, sift, rounds(-forge), gate-worker,   |
                  |  landing-pass, verdict, czar-pass, watchtower, skew, sending... |
                  +---+-----------------------------------------------------------+
                      |
 operator / Concierge |                                       aeon unit (transient)
 (mail, panes) -files-+--> bead.sh / work file               spira-aeon-<fayth>-<n>
      ^               |        |                             +------------------+
      | escalations   v        v                             | aeon (runner)    |
      |         +-------------------------+  claim / renew   |  claude -p ...   |
      +-- mail -|  spira-lc (lifecycle    |<-----------------|  model session   |
                |  service, Unix socket)  |  work <verb>     |  `work` client   |
                |  bead/delivery/batch/   |----------------->|  (no bd, no db)  |
                |  ask machines, CAS      |                  +--------+---------+
                +-----------+-------------+                           | commits
                            |                                         v
                            v                                git worktree + branch
          +-------------------------------------+            spira/<bead-id>
          | Dolt SQL server (dolt-beads.service)|                     |
          |  - beads DB     (content, via bd)   |                     v
          |  - spira_lifecycle (state, events)  |        +---------------------------+
          +-------------------------------------+        | managed git repositories  |
                            ^                            |  landing ref, spira/<id>  |
 sentinel, spira-claim, sift|                            |  branches, round branches |
 batcher, landing-pass,     +-- read/write via --------- +-------------+-------------+
 queue, sending, strand ...    spira-lc / bd                           |
                                                                       v
 run/ ($SPIRA_RUN)                                          <releases>/<sha>/
   verdicts/trees/<repo>/<tree>  tree certificates          <releases>/current -> <sha>
   queue/<repo>/...              open-round records
   gate-worker/                  gate queue + locks
   tsd/ otel/ watchd/ ...        time series, spans, watcher logs
```

| Store | Where | Holds | Written by |
|---|---|---|---|
| Beads database | Dolt server (`dolt-beads.service`), `$SPIRA_DB` | bead content, labels, deps, comments, statute and SOP memories | `bd` (through `bead`, `rule`, `sop`) |
| `spira_lifecycle` | same Dolt server | `bead`, `delivery`, `batch`, `batch_member`, `ask`, `mending`, `mend`, append-only `event`, `bead_dep` | **only** `spira-lc` (a distinct database user holds the grants; the event table is insert/select only) |
| `run/` state | `$SPIRA_RUN` | tree certificates, open-round records, gate queue and locks, time-series rows, spans, watcher logs, pass state | the binaries that own each file |
| Git repositories | the registry's paths | branches `spira/<id>`, round branches, the landing ref, `refs/archive/rounds/<n>` | aeons (their branches), `batcher` / `queue` (rounds, landing) |
| Releases | `$SPIRA_RELEASES` | immutable releases and the `current` link | `release` |

`spira-lc` serves the machines over a Unix socket (`SPIRA_LC_SOCKET`). On a multi-user install
it runs as `spira-lc.service` / `.socket` under its own system user, so the database
credential is unreadable by anyone else; in same-user mode it is `lc-serve.service` /
`.socket` on the user manager's runtime directory. The same `dispatch` code serves the socket
and the one-shot CLI, so they cannot drift. `spira-lc` exit codes: 0 applied, 1 no row,
2 cannot tell (store unreachable; safe to retry), 3 refused (not safe to retry).

### A bead from filing to released

This follows the `queue.local` path with default timer cadences; each step names the binary
and unit that does it.

1. **File.** A bead is created: `bead.sh file`, `work file` / `file-followup` / `split` from a
   session, `gh-intake` from GitHub issues, or a trigger unit (`spira-groom`, `spira-warden`,
   `spira-maechen`, `watchtower`, `incident`). `bead` also writes its lifecycle row in `READY`.
   A label places it in a persona's partition (the plan label, for builders).

2. **Sentinel and summon.** `spira-sentinel.timer` (every 2 min) runs `sentinel`;
   `spira-summon.timer` (every minute) runs `sentinel --summon-only`. The pass counts ready
   beads per partition and summons aeons: lane personas against their own caps, then task
   personas from the shared `SPIRA_MAX_AEONS` pool, builders last as the elastic persona. The
   same pass reaps `WORKING` rows whose lease expired (`HolderDead`), applies the poison hold
   when attempts are exhausted, holds blocked beads on `wait`, and dispatches the landing
   worker (step 7).

3. **Claim.** The summoned `aeon` asks `spira-claim select` for the top-ranked claimable bead
   (`READY`/`REWORK`, no hold but `wait`, in its partition, blockers satisfied; epics first,
   express beads ahead) and sends `Claim` to `spira-lc`. The compare-and-swap means two aeons
   cannot win the same bead. The claim records holder, persona, lease and, for a bead built on
   unlanded prerequisites, the stack of their certified tips.

4. **Aeon session.** `aeon` creates the worktree and branch `spira/<id>`, renders the persona
   brief (`<name>.md` plus statutes), and launches the model with the restricted environment.
   It reports `Phase`, renews the lease while the trace grows, and ends the session with the
   work preserved if the lease lapses. The model commits to its branch and runs tests only
   through `testenv`.

5. **Submit.** The model runs `work submit`. The client reads the tip from
   `refs/heads/spira/<id>` (never from an argument); `spira-lc` refuses if the tip is one a
   round ejected as red, if the stack is stale, or if a lint rule or the declared proof tier
   fails; otherwise `Submit` moves the row to `SUBMITTED`. Alternatives: `work done` closes a
   bead with nothing to land; `work blocked` holds it and files an ask. At exit the aeon's
   teardown decides whether the attempt is charged, and the unit's stop hook runs
   `sentinel --summon-only` to refill the slot.

6. **Screen.** `spira-sift.timer` (every minute) runs `batcher sifts`, which screens each queue
   repository's proto-round (all `SUBMITTED` and `CERTIFIED` rows). A pass is recorded as
   `Sifted {tip}`; a conflict, duplicate or policy gap sends the bead back, supersedes it, or
   files a mend. The batcher cuts only beads whose current tip has a screen pass.

7. **Per-branch gate (optional certification).** `landing-pass land` (dispatched by the
   sentinel) queues each done branch for `gate-worker` (`spira-gate-worker.timer`, every 30 s),
   which runs up to `certify_par` gates at once under host admission control and files each
   verdict. `landing-pass` applies it: `PASS` → `GatePass` (`CERTIFIED`), `FAIL` → `GateRed`
   (`REWORK`, charged), `NO_VERDICT` → `GateInfra`, `BASE_FAIL` → one incident per repository.
   A round does not require a gate verdict: its full corpus is the stronger trial.

8. **Cut.** `spira-rounds.timer` (every 30 s) runs `batcher rounds-local`, cutting one
   `queue.local` repository at a time; `spira-rounds-forge.timer` runs `batcher rounds-forge`
   for `queue.forge` repositories. A cut picks the round (feature-first: the epic with the most
   pool members, plus what they stack on and every express bead; else the whole pool), merges
   the members, and creates the batch row and, per member, a delivery row. Members move to
   `IN_DELIVERY` (`Deliver`). While one round's suites run, the next may be staged behind it.

9. **Gate on the round.** The round's full corpus runs through `testenv` on a round VM
   (`round-vm`, a pooled warm VM; `spira-round-vm-drain.timer` releases an idle one). Each pass
   is recorded on the batch row. A red is attributed first by path (reds intersected with each
   member's diff), then by a mender, then by the batcher judge. Members at fault are ejected
   (`Returned` → `REWORK`, with quoted evidence) and the rest re-run. A green writes a round
   certificate for the exact tree under `verdicts/trees/<repo>/<tree>`.

10. **Settle.** `spira-verdict.timer` (every 2 min) runs `queue step --all`, which settles open
    batches of queue repositories; `pr`-mode repositories are walked by
    `spira-landing-pass.timer` (every 90 s).

11. **Land.** `queue land-local` refuses unless a round `GREEN` certifies the head's tree. For
    the harness repository it first builds and verifies the release from the round's own
    tested binaries, so a release that cannot verify never lands. Then it fast-forwards the
    landing ref with a compare-and-swap on the base sha, archives the head under
    `refs/archive/rounds/<n>`, closes the beads, and walks each member on `spira-lc`:
    `Deliver`, then `Delivered` → `LANDED`. A later fault never undoes the landing; a failed
    `Delivered` is reported and the ref stays. Waiting beads are rebased onto the new base.

12. **Cleanup.** `sending` deletes the branch and worktree of every bead whose work has landed,
    and nothing else; the sentinel runs it each pass, `spira-straggler-sweep` covers queue
    repositories hourly, and `spira-worktree-reaper` removes stale worktrees. Content found
    already on the base is closed by `ContentOnBase`.

13. **Release.** After the ref moves, `release activate <sha>` switches `current` onto the
    verified release and restarts running units, waiting a bounded time for passes in flight.
    A deploy fault leaves `current` where it was, writes a fault marker, and makes `land-local`
    exit non-zero (the landing itself stays). Under `queue.local`, a later publish step carries
    the commit to the forge through delivery states `PUBLISHING` → `PUBLISHED` or
    `PUBLISH_RED`; a red publish is fixed forward on a new bead. `skew` checks hourly that the
    harness in force is the harness that landed; `release rollback` re-activates the previous
    release.

### Watching the loop

Several units exist only to notice the loop failing and turn that into work or mail rather than
silence: `watchtower` (hands `ops` the pipeline's vital signs), `czar-pass` (30 s queue-stall
detection with deterministic remedies), `reconciler` (declared structural invariants: remedy or
escalate), `auron` (escalates a stalled loop and does nothing else), `strand` (stranded-work
detector), `cert-sweep` (re-certifies the landing ref's tip on a schedule), `refusal-watch`,
`mend-watch`, and the `spira-lc event-continuity` check. `cockpit-collect` and `cockpit/`
present the same stores to the operator.

## 4. Component reference

Conventions: `$SPIRA_RUN` is the runtime directory, `$SPIRA_HOME` the `spira/` shell layer of
the active release, `<repo>` a repository from the registry. Binaries are resolved by bare name
on the release's `bin/`. All configuration comes from one `spira.toml` (`$SPIRA_TOML`), keys
registered in `spira/conf.d/`. A "shim" is a `spira/*.sh` file that sources `conf.sh` and
`exec`s the Rust binary of the same job.

Crates that are libraries only: `lifecycle` (pure state machines), `spira-otel`, `batcher`
(the round decisions; its I/O is `batcher-cut`), `migration-numbers`, `inbox-rules`,
`reconciler-engine` and `testkit` (a dev-dependency). `spira-world` is a library that builds
`world`, `aeons` and `slay`.

### 4.1 Lifecycle and store

| Component | Binaries / units | Purpose, reads and writes |
|---|---|---|
| **Dolt store** | `dolt-beads.service` (`dolt sql-server`, `Restart=always`, refuses manual stop), `dolt-persist.service`, `dolt-persist-compact.timer`, `dolt-tmp-prune.timer`, `spira-dolt-boot-check.timer` | The bead store, installed when `SPIRA_DOLT_DATA` is set (persistence units also need `SPIRA_DOLT_DURABLE`). The boot check alarms the operator if the store is not active shortly after boot. |
| **store-persist** | `store-persist restore\|pass [--offline]\|run\|seed --from DIR\|compact\|age` | Keeps a RAM-resident Dolt data dir durable: one `dolt backup` per database, restored before the server starts, synced continuously, compacted daily. `age` exits 1 past the lag bound. |
| **beads-store** | `beads-store commit --db P --message M`, `push --db P --remote R` | Commits a beads store's dirty tracked tables through the real `dolt` engine (never `bd`) and verifies the commit cleared the working set. Operator tool; no unit calls it. |
| **lifecycle** | library; `lifecycle/schema.sql`, `migrations/`, `grants.sql` | The bead, delivery, batch, ask and mending machines and the event-log fold. Defines database `spira_lifecycle`; grants make `spira_lc` its only writer. |
| **spira-lc** | `spira-lc` (`serve`, one-shot verbs); `spira-lc.service`/`.socket` (system user) or `lc-serve.service`/`.socket` (same user); `spira-event-continuity.timer` (15 min), `spira-mend-watch.timer` (2 min), `spira-reconcile-content.timer` (10 min) | The only process that writes `spira_lifecycle`. One JSON argv line in, one JSON line out. Primitives (`show list history event stats facts mending cut stage promote land settle abandon-batch eject-member create-bead create-ask …`), caller verbs (`hold unhold reply release drop supersede certify deliver renew express …`), admin verbs, and the persona-gated `work <bead> <verb>` layer. Also runs the tool verbs (`groom incident sop census queue landing-pass strand sending mender`) as the service user on behalf of aeons. Reads `SPIRA_LC_SOCKET`, `SPIRA_LC_PASSWORD_FILE`/`SPIRA_LC_USER`. |
| **lifecycle-guard** | `lifecycle-guard [--json] [--gate] [--rules F] <path>...` | Static analyser (shell via tree-sitter, and Rust) refusing any route to lifecycle state around `spira-lc`: `bd close/claim/…`, `bd` status reads feeding a decision, the lifecycle credential outside its owners. No allow-list. A gate fence, not a unit. |
| **bead**, **bdq** | `bead` (shim `spira/bead.sh`), `bdq` | `bead` is the filing contract (`file amend label drop contract lint judge-create event dep …`). `bdq` is the one chokepoint for invoking `bd`: retry on writer-lock contention and create-time fences. |
| **tsd** | `tsd-write`, `tsd-read`, `cert-report`; `tsd-lifecycle-export` | Time-series rows by family (names `[a-z][a-z0-9-]*`). A family with a reader on the landing path is an append-only file `$SPIRA_RUN/tsd/<family>.jsonl`; a family whose readers are all alarms, reports and ops views (`tsd::store::MOVED`) is pushed to the metrics store instead and read back by `tsd-read`, which refuses with a named error when the store is unreachable or holds nothing for the window. `tsd-lifecycle-export` is the sole writer of the `bead-stage` family, projected from the lifecycle event log. |
| **spira-otel** | library | One OTLP/JSON span per call appended to `$SPIRA_RUN/otel/<service>.<date>.jsonl`; gauge points pushed to `SPIRA_METRICS_ENDPOINT`. Emitting never blocks or fails the caller. |
| **unit-run** | `unit-run watch\|ensure-accounting`; `spira-unit-run-dbus.service` | Learns user-unit executions from the user manager's D-Bus signals and stores one `unit-run-dbus` row per run in the metrics store. |
| **lc-backup** | `spira/lc-backup.sh run\|drill`; `lc-backup.timer` (6-hourly), `lc-backup-drill.timer` (daily) | Snapshots each lifecycle database through the live server, uploads one object to `SPIRA_LC_BACKUP_URL` (`s3://` or `file://`), and the drill restores it into a scratch dir and compares row counts. |

### 4.2 Work intake and claiming

| Component | Binaries / units | Purpose, reads and writes |
|---|---|---|
| **sentinel** | `sentinel`, `sentinel --summon-only`, `--report`; `spira-sentinel.timer` (2 min), `spira-summon.timer` (1 min) | One reconcile pass over the work graph: completed epics, dead workers (expired leases), stranded work, stale blocked flags, `landing-pass land` dispatch, summoning aeons as transient `systemd-run --user` units `spira-aeon-<fayth>-<n>`, rate-limited judgement. Gated by the `world.halted` / `world.draining` stamps, the capacity pause, the pool and per-lane limits. |
| **spira-claim** | `spira-claim` | Claim-time arithmetic, no daemon: attempt counts, the poison decision, epic-first `select --fayth`, `ready-count`, `urgent`, `deadlocked`, and `unpoison` (the one verb that lifts poison). |
| **gh-intake** | `gh-intake`; `spira-gh-intake.timer` (15 min) | Ingests issues from `SPIRA_GH_INTAKE_REPO`. A trusted author (or collaborator with write access, fail-closed) becomes a work bead; others become `gh-untrusted` beads until a trusted actor applies an accept label. Also `closeout`, `unlanded-scan`, `backfill`, `provenance`. |
| **incident** | `incident` (shim `spira/incident.sh`); `spira-incident-settle.timer` (5 min) | Turns a production event into a bead Ops can claim, deduplicated on `external_ref`, with a write-ahead spool (`SPIRA_SPOOL`) so events arriving while the store is down are not lost. Verbs: `file`, `alarm`, `drain`, `list`, `settle`, `collapse`. |
| **groomer** | `groomer`; `spira-groom.timer` (6 h) runs `spira/groom-trigger.sh` | Graph hygiene ops (`split-piece supersede close correct-lane depends-on-fix unpoison triage-poison deadlocked`), reached by the groomer persona as `work groom`. The trigger files one trigger bead when live beads plus landings since the last pass reach `SPIRA_GROOM_THRESHOLD`. |
| **maechen-trigger** | `maechen-trigger`; `spira-maechen.timer` (15 min) | Files a sweep bead for the Maechen persona when landings since the last pass reach `SPIRA_MAECHEN_LANDING_INTERVAL`, the gap exceeds `SPIRA_MAECHEN_MAX_GAP_SECONDS`, or closed-record anomalies are found; idempotent. |
| **refusal-watch** | `refusal-watch pass`; `spira-refusal-watch.timer` (1 h) | Groups refused lifecycle events by (actor, event, state, refusal) and files one bead per class over its baseline, at most once per repeat window. |
| **kit-bump** | `kit-bump pass --config FILE`; `spira-kit-bump.timer` (10 min) | Reads the shared kit's newest release from the Cargo registry (`SPIRA_KIT_REGISTRY`) and keeps one open bead per consuming site whose lockfile is behind it — a lockfile update when the site's semver range admits the release, a breaking-upgrade bead when it does not — amended (not duplicated) on a newer release; kit commits that publish nothing file nothing; pushes `spira_kit_bumps_filed`. |
| **filings-collect** | `filings-collect pass --state FILE`; `spira-filings-collect.timer` (10 min) | Reads each `SPIRA_FILINGS_HOSTS` product host's `filings/outbox` over ssh and files one builder bead per (host, class), appending later filings to it while it is open; acknowledges each filing on the host only once its bead holds it. An unreachable host or unparseable filing is named on stderr and fails the unit; nothing unacknowledged is lost. |
| **runtime-watch** | `runtime-watch pass --state FILE`; `spira-runtime-watch.timer` (6 h) | Reads each registered repo's Rust, Node and Python pins and files one bump bead per repo and runtime when upstream no longer supports it (Rust two or more releases behind stable; Node and Python past end-of-life). An unreadable source reports "cannot check" and exits 3; it never counts as support. |
| **triggers** | `spira/warden-trigger.sh`; `spira-warden.timer` (2 h); `spira-ops.timer` (5 min) | File at most one open warden sweep bead; summon an Ops aeon for waiting incidents (`aeon ops --sweep`). |
| **work** | `work` | The aeon-side client, bound to one summoned bead by `SPIRA_WORK_BEAD_ID`; persona from `SPIRA_FAYTH`. Holds no database code: every verb goes to the `spira-lc` socket. Bound verbs `show note submit done blocked file-followup split superseded-by`; lane verbs (`ask read list search label-add dep-add close-other file …`) and tool verbs. Each operation is checked against one allow table (`spira-lc/src/work.rs`). Local verb `crates` prints the `-p` list for the branch's diff. |

### 4.3 Aeons and personas

| Component | Binaries / units | Purpose, reads and writes |
|---|---|---|
| **aeon** | `aeon <fayth> [--dry-run\|--sweep [--prompt T\|-]]`, `aeon stop <bead>`, `aeon --escape <fayth>` | Claims a bead (via `spira-claim select`), creates the worktree under `$SPIRA_RUN/worktree` (one aeon, one worktree), renders the brief, starts `claude -p` with an `env -i` allow-list environment and the `spira/hooks/aeon-fence.sh` PreToolUse hook, heartbeats the lease, and runs teardown. Model from `persona.<name>.model`. The model's `PATH` holds only `model-bin/` (`work`, `testenv`). No unit of its own. |
| **chamber** | `spira/chamber/<name>.fayth` + `<name>.md`; `chamber/disabled/` | The persona definitions (section 2). Discovered by glob. |
| **mender** | `mender` (reached as `work mender <verb>`) | Evidence pack for one failing suite (output, environment signatures, members touching the suite, diffs) and the triage outcomes (`mended environment flaky return unsure`) recorded on the `MENDING` row. The deadline belongs to the lifecycle machine. |
| **archivist** | `archivist sweep\|list\|now\|digest\|digest-send …`; `spira-archivist.timer` (5 min) | Measures live session transcripts, orders candidates by drift within a per-pass budget (`SPIRA_ARCHIVIST_*`), runs the agent to persist unfinished business, and sends a daily digest mail. |
| **sending** | `sending [--all\|--dry-run\|<bead>\|--skip-queue\|--queue-only\|--budget-secs N]`, `reap-terminal`, `reap-stale`, `destroy-worktree`, `destroy-branch`, `salvage`; `spira-reap-terminal.timer`, `spira-worktree-reaper.timer`, `spira-straggler-sweep.timer` (hourly) | Deletes the branch and worktree of every bead whose work has landed, and nothing else. Destruction goes through one chokepoint; a git hook refuses deleting a bead branch outside it. |
| **broker** | `broker token\|read` | `token` mints the GitHub App installation token behind `gh-app.sh`; `read` answers `run-view`, `run-jobs` and `artifact-download` for a mapped repo, without log content. |
| **spira-world** | `world`, `aeons`, `slay` | `world stop\|start\|drain\|resume\|status` acts on units by `Plane=work\|observability\|maintenance` declared in each unit's `[X-Spira]` section; `aeons` sets the fleet ceiling and task pool; `slay --bead` stops one aeon cleanly and records the disposition. |
| **reclaim** | `reclaim pass …`; `spira-reclaim.timer` (1 h) | Removes rebuildable build-target directories no live process or lease holds once free disk is short; caps the shared compilation cache; prunes container storage losslessly. |
| **ctrl** | `ctrl suspend\|resume\|check\|reason\|list\|suspended\|divergence` | Durable suspend/resume state in one JSON file named by `spira.ctrl`, outside source control. The installer reads it so a suspended unit is not re-enabled. |
| **spira-supervise** | `spira-supervise <child>` | Main-PID supervisor for `cockpit-collect`; sends systemd `READY=1`/`WATCHDOG=1` only while the collector's snapshot is fresh. |

### 4.4 Gating, rounds and landing

| Component | Binaries / units | Purpose, reads and writes |
|---|---|---|
| **gate** | `gate <branch> [repo]`, `gate status\|cancel\|wait`, `gate --definition` | Judges the branch *merged onto the current landing ref* (a non-merging branch is `FAIL reason=no-rebase`). Reads `gate.steps` from the judged tree, runs the fence chain then the selected suites, emits `gate: VERDICT=…`. |
| **gate.steps** | data (repo root) | One directive per line: `bin VAR pkg` (build `pkg` from this tree and expose it as `$VAR`) and `step <cmd>` (an element of the `&&` chain). A branch that edits the gate edits this file in the same commit. |
| **gate-run** | `gate-run [--status\|--exec] <branch> [repo]` | Runs `gate` detached and answers each call from bounded state under `$SPIRA_RUN/gate-run/`, so no single call outlives a tool-call ceiling. |
| **gate-worker** | `gate-worker run`; `spira-gate-worker.timer` (30 s) | Drains the gate queue (`$SPIRA_RUN/gate-worker/{inbox,claimed,refused,output}`) with up to `certify_par` workers; files each verdict for the landing pass to apply. |
| **gate-check** | `gate-check`; `spira-gate-check.timer` (2 min) | Resolves `gh:run` CI gates on open beads, files flaky-suite and red-twice-on-main beads, ingests run timings. |
| **gate-diag** | `gate-diag <results-root>` | Reads a batch's result files; writes `red-suites.json`, `results.jsonl`, `junit.xml` and a report. Never re-runs a suite. |
| **suite-select** | `suite-select select\|gate\|budget\|header` | Chooses which `spira/test-*.sh` suites a diff must run from each suite's `# covers:` / `# tier:` header; a changed source file no suite claims is an error. |
| **testenv** | `testenv`, `bd-meter`, `testenv-cap`, `target-reap`; `spira-target-reap.timer` (2 min) | The test runner (section 7): builds a tree in place, runs the selected suites in a fixture container, caches verdicts by tree. `testenv-cap` kills a run past its wall-clock cap; `target-reap` removes `target/` of landed worktrees. |
| **test-plan** | `test-plan validate\|matrix\|render\|schema` | Validates the use-case catalogue `docs/test-plan/*.toml` against suites' `# covers:` ids. |
| **spira-lint** | `spira-lint [--root D] [--only RULE] [--base REV\|--diff REV] [--emit-allow]` | One binary, one rule per static fence (section 7). First step of `gate.steps`. |
| **forge** | `forge <verb> <repo-dir> [args]` | The one seam onto the code host (PR, check, run and branch-protection verbs). Execs `$SPIRA_GH` under a timeout and never reads a credential itself. |
| **landing-pass** | `landing-pass --pass`, `land`, `halt`; `spira-landing-pass.timer` (90 s) | Moves each done `spira/<id>` branch one step by its repository's land mode (`pr`, `push`, `hold`, `queue`, `queue.local`). The timer runs `--pass`; the sentinel dispatches `land`. Applies gate verdicts; runs `unit-ensure` and `target-reap` afterward. |
| **queue** | `queue submit\|step\|eject\|abandon\|open-batch\|claim\|release\|resubmit\|absolve\|land-local\|publish\|publish-settle\|to-forge\|to-local\|rollback-local\|round …`; `spira-verdict.timer` (2 min), `spira-publish.timer` (30 min) | The merge queue's record keeper and terminal step: settles open batches, lands (`land-local`), publishes the landing ref. Queue state under `$SPIRA_QUEUE_DIR`; lifecycle state through `spira-lc`. |
| **queue-watch** | `queue-watch watch\|health` | Streams merge-queue transitions for every queue repository from queue state, `spira-lc` and the forge; `health` is non-zero when stale or blind. A watcher row. |
| **batcher** | `batcher cut\|rounds\|rounds-local\|rounds-forge\|judgement-ci\|path-attribute\|sift\|sifts`; `spira-rounds`, `spira-rounds-forge`, `spira-sift` (1 min) | Cuts a round, merges members (renumbering colliding migrations), runs the corpus on the round VM, attributes a red to a member by path overlap, ejects or lands. Gated by `SPIRA_BATCHER_ENABLE` / `SPIRA_BATCHER_FORGE_ENABLE`. The `batcher` crate holds the decisions; `batcher-cut` does the git/forge/bead I/O. |
| **sift** | `sift status [--json] [bead]` | Screen results (`screened`, `sent-back(n)`, `capped`) recorded per bead under `$SPIRA_RUN`. |
| **pilgrimage** | `pilgrimage open\|walk\|defects\|wait\|clean\|handoff\|done\|status [--json] [bead]` | A feature's walk as a recorded state machine under `$SPIRA_RUN/pilgrimage` (`waiting`, `walking`, `defects-filed`, `clean`, `concierge-walk`, `done`). `clean` files the Concierge-walk ask (`handoff` retries it); every move pushes `spira_pilgrimage_*` points, read by the Pilgrimages dashboard. |
| **rebase-stale** | `rebase-stale <bead> [repo]` | Mechanically rebases a stale submitted branch; only a real content conflict or a red gate returns it to an aeon. |
| **habits** | `habits` (hook JSON on stdin); wired by `aeon` where the fayth sets `FAYTH_HABITS=1` | The PostToolUse/Stop habit hooks: runs the repo's `.spira/habits.toml` checks on the edited file and returns the findings on the lines the edit touched as `additionalContext`. Feedback only; always exits 0. |
| **cert-sweep** | `cert-sweep pass --mode full\|subset`; `spira-cert-sweep-full.timer` (hourly), `spira-cert-sweep-sample.timer` | Continuously certifies the landing ref's tip and records every suite verdict, so a red has a last-green to point at. |
| **round-vm** | `round-vm acquire\|release\|run\|status\|reap\|warm\|drain\|ami-prune\|template\|refresh\|teardown`; `spira-round-vm-drain.timer` (5 min), `spira-round-vm-ami-prune.timer` (daily) | Runs a round's full corpus on a disposable VM cloned from a template (providers: Proxmox, EC2); a pool of one warm VM. |
| **sccache-dav** | `sccache-dav`; `sccache-dav.service` (only when its address key is set) | A WebDAV store for sccache shared by host builds and every round VM. |
| **czar-pass** | `czar-pass --pass`; `spira-czar-pass.timer` (30 s) | Deterministic queue-stall detection (deadlock, attribution failure, loop stalled, CI queued or red too long, base red, pool idle) with mechanical remedies; escalates to an incident or the czar persona. |
| **reconciler** | `reconciler --pass`, `reconciler-flow --pass`; `spira-reconciler.timer` (60 s), `spira-reconciler-flow.timer` (30 min) | Structural invariants (units vs the composite, fleet occupancy, cockpit up, release currency, disk, queue health, lifecycle orphans, main health) with cheap remedies; `reconciler-flow` evaluates backlog trend, stage velocity and dwell. |
| **desired-state** | `spira apply <doc>`, `spira compose`, `spira schema` | Merges TOML fragments of resource specs into one versioned composite under `$SPIRA_DESIRED_DIR`, read by the reconciler and `watchtower`. |
| **migration-numbers** | library | Renumbers a lifecycle migration whose number another round member took; refuses a tree with gaps or duplicates. |

### 4.5 Releases and activation

| Component | Binaries / units | Purpose, reads and writes |
|---|---|---|
| **release** | `release build\|verify\|activate\|rollback\|prune\|status\|install-tarball\|stage\|canary\|acceptance\|session-hook\|intake\|config-stage\|rotate-lc-credentials` | `build` makes an immutable `<releases>/<sha>/`; `verify` checks the `MANIFEST` and unit binaries and runs `pre-activate.sh`; `activate` re-renders units, moves `current`, restarts what changed, checks it came up and undoes it all on failure; `rollback` re-activates the previous release. `--hotfix` activates an unlanded commit and alarms after `SPIRA_HOTFIX_ALERT_HOURS`. |
| **skew** | `skew check [--escalate]\|refresh\|units\|gap\|copies\|foreign`; `spira-skew.timer` (1 h) | Is the activated release the latest published, and does the tree match its `MANIFEST`? Exit 3 = cannot verify (fails closed). `foreign` is the gate's fence against editing a vendored copy of the harness. |
| **install** | `spira-install`, `units-install`, `unit-ensure`, `render-unit` | `spira-install` runs the install phases (section 5). `units-install` renders every `systemd/*` template and enables per `install/src/manifest.rs`. `unit-ensure` installs new or changed units without restarting a running service. `render-unit` renders one template. |
| **pre-activate** | `spira/pre-activate.sh` | Checks before `current` moves: dependencies at their minimum versions, config validates against the release's own schema, store reachable and schema matches, every unit renders, the release's `self-test.sh`, then pending `lifecycle/migrations/*.sql` applied last. |
| **acceptance** | `spira/acceptance-local.sh`, `acceptance-ci.sh`, `acceptance-prev-tag.sh`, `acceptance-agent.sh` | Wrap `release acceptance <tag>`: install a tarball on a clean machine or container and run a probe bead end to end with a deterministic stub agent, optionally upgrading from the newest published predecessor. |
| **build and deploy scripts** | `spira/deploy.sh`, `build-tarball.sh`, `build.sh`, `uninstall.sh`, `first-run.sh`, `configure.sh`, `seed.sh` | Tarball build, re-render step, config bootstrap, seeding statutes into a fresh database. |

### 4.6 Observation

| Component | Binaries / units | Purpose, reads and writes |
|---|---|---|
| **watchd** | `watchd manifest\|units\|exec\|status\|drain\|peek\|tail\|next\|notify\|restart\|prune`; `spira-watch@<name>.service`, `spira-notify.timer` (5 min), `spira-refresh.timer` (1 min) | The watcher manifest (`spira/watchers`, plus `*.watchers` in `$SPIRA_WATCHERS_OVERLAY`): rows `name\|kind\|target[\|health]`, kinds `daemon`, `log`, `extern`, `off`. Per-watcher contract: an append-only `$SPIRA_RUN/watchd/<name>.log` and a `.cursor` of lines delivered. `notify` mails the operator for events no reader took. |
| **loom** | `loom`; `spira-loom.service` | Read-only HTTP server on `SPIRA_LOOM_ADDR` (loopback by default) showing where work is stuck. Reads the lifecycle read model and the cockpit snapshot; never runs `bd`; refuses to start without a database. |
| **cockpit-collect** | `cockpit-collect collect`; `spira-cockpit.service` (under `spira-supervise`) | Runs the probe set on 5 s / 60 s / 600 s cadences as isolated subprocesses and merges fragments into `$SPIRA_RUN/cockpit.env`. |
| **cockpit** | `cockpit.sh`; `cockpit/ops` (`health layout rebuild resolve reply lc-view`); `cockpit-ensure.timer` (1 min) | The operator's tmux workspace. `layout` builds and heals the layout; `rebuild` makes a cockpit from nothing after the tmux server dies; `health` repaints the ops dashboard from `cockpit.env`. |
| **metrics stack** | `spira/metrics-stack.sh vm\|collector\|grafana`; `spira-metrics-{vm,collector,grafana}.service` | VictoriaMetrics, an OpenTelemetry Collector (span files → metrics) and Grafana, under podman. Needs `SPIRA_METRICS_DATA`; otherwise the units idle. Producers push to `SPIRA_METRICS_ENDPOINT`; empty disables. |
| **auron** | `auron [--report]`; `spira-auron.timer` (2 min) | Watchdog over the loop: classifies a stalled loop and only speaks, raising or clearing one alert bead per cause. Repairs nothing. |
| **watchtower** | `watchtower`; `spira-watchtower.timer` (30 min) | Hands Ops the pipeline's vital signs and runs cheap detectors for "every unit green but nothing landing": throttle, czar outcome, PR stall, release skew, deploy fault, slow query, drift, disk and memory. |
| **perf-watch** | `perf-watch --probes FILE`, `perf-watch happy`; `spira-perf-watch.timer` (5 min), `spira-perf-happy-path.timer` (1 h) | Times the hot read paths end to end and alarms past a limit with the top callers. |
| **census** | `census [--with-suppressed]` | Ranks failure classes by distinct beads since the last watermark, suppressing classes with an open remedy. |
| **strand** | `strand report\|check\|throttle-state` | Reports work that exists and is not moving, with the reason and the one action that clears it; `check` acts where the fix is mechanical. |
| **doctor** | `doctor` | Read-only runtime health (section 5). |
| **intent-report** | `intent-report` | Prints certification latency, no-verdict rate, round time per member and certified-to-landed latency for a window from `$SPIRA_RUN/tsd`. Operator tool. |

### 4.7 Operator surfaces

| Component | Binaries / units | Purpose, reads and writes |
|---|---|---|
| **mail** | `mail send\|template\|list\|read\|count\|unread-age\|done\|sendmail\|reconcile\|tidy\|ensure\|sweep-dismissed`; `spira-mail-reconcile.timer` (1 min), `spira-mail-tidy.timer` (15 min) | The operator/persona channel as a Maildir tree `$SPIRA_MAIL/<mailbox>/{tmp,new,cur}`. One gate enforces lint, a repeat-suppression window and per-kind skeletons (`alert decision event note question report suit`). `reconcile` applies replies (matched by `In-Reply-To`, once per Message-ID) to the bead they answer: notes the answer, closes the ask, lifts the ask hold. |
| **mail delivery** | `spira/spira-mail-deliver.sh`; `spira-mail-deliver.service` | One `inotifywait` watch per registered mailbox (`SPIRA_MAIL_READERS`); wakes the reader and keeps waking while unread mail remains. |
| **spira-submission** | `spira-submission serve\|provision\|health`; `spira-submission.service`, `spira-submission-provision.timer` | Authenticated implicit-TLS SMTP on a private-network address; hands each accepted message to `mail sendmail`, so a reply from any mail client behaves like a cockpit reply. |
| **asks** | `spira/asks.sh`; `cockpit/moot-sweep.sh`, `cockpit/verify-asks.sh`; `spira-moot-sweep.timer`, `spira-verify-asks.timer` (15 min) | An ask carries `SPIRA_ASK_LABEL`. `asks.sh` announces new asks and ask holds into the Concierge inbox; moot-sweep closes asks whose `MOOT-WHEN:` predicate passes; verify-asks closes asks whose `VERIFY:` command passes. |
| **panel** | `panel [--once]` | The decisions TUI (bottom-left cockpit pane): decisions, FYI, notifications and alerts read from the bead store by label. |
| **concierge** | `concierge.sh start\|here\|attach\|brief\|status\|wake\|stop`; `concierge.timer` (10 min) | The operator's session: a `claude` session on its own tmux socket composed from the `concierge` persona plus the statute book. |
| **concierge-guard** | `concierge-guard check\|health`; `spira-concierge-guard.timer` (3 min) | Looks for the inbox waiter among the session's descendants; on a lapse writes an inbox row and mails the operator if it persists. |
| **inbox-triage** | `inbox-triage`; library `inbox-rules` | The Concierge's in-session monitor over `SPIRA_CONCIERGE_INBOX`: drops echoes, dedups, passes the rest. `inbox-rules` is shared with `watchd next` so both agree on what is noise. |

### 4.8 Maintenance timers

Each timer runs the service of the same name. "boot+X" is `OnBootSec`; "every X" is
`OnUnitActiveSec` (X after the last pass *ended*). Timers named in 4.1–4.7 are not repeated
here unless they have no other home.

| Timer | Runs | Cadence | Purpose |
|---|---|---|---|
| `spira-archive` | `archive.sh sweep` | boot+5 min, every 20 min | index session transcripts |
| `spira-escape-census` | `escape-classify.sh census` | weekly | escape-by-class census |
| `spira-export-beads` | `export-beads.sh` | every 6 h | regenerate the bead mirror |
| `spira-nix-gc` | `nix-gc.sh` | weekly | collect Nix garbage keeping the current and previous environment |
| `spira-sop-lint` | `sop-lint-watch.sh` | every 1 h | lint the SOP shelf |
| `spira-statute-audit` | `rule audit --file --for builder --max 5` | weekly | statutes naming tools that do not exist |
| `spira-incident-settle` | `incident settle` | every 5 min | close incidents whose fix landed and detector is quiet |

Long-running services with no timer: `dolt-beads`, `dolt-persist`,
`lc-serve` / `spira-lc` (+ sockets), `sccache-dav`, `spira-loom`, `spira-cockpit`,
`spira-mail-deliver`, `spira-submission`, `spira-metrics-*`, `spira-unit-run-dbus`,
`spira-watch@<name>`.

### 4.9 Shell layer

`spira/` holds the shell that is still live beside the Rust: `conf.sh` and `conf.d/` (the
config registry), `lib.sh` (shared library whose bead-store functions are one-line shims onto
`bdq`), the hooks in `spira/hooks/` (`aeon-fence.sh`, `pre-commit`, `reference-transaction`,
`session.sh`, concierge hooks), `czar-fence.sh`, `hold.sh` / `held.sh` / `holds.sh`,
`ready.sh`, `host-check.sh`, `testdb.sh`, `schema.sh`, and every `test-*.sh` suite.

## 5. Installation and first run on a fresh box

### Prerequisites

Spira runs as systemd **user units** on Linux, so you need a systemd user manager (the
installer enables lingering for the installing user) and a login user. The authoritative tool
list is `spira/deps.toml`, which `doctor` reads:

| Tier | Tools | Notes |
|---|---|---|
| runtime (the loop cannot run) | `bd`, `dolt`, `git`, `python3`, `flock`, `tar`, `gunzip`, `duckdb` | `bd`, `dolt`, `duckdb`, `gh`, `jq`, `zstd`, `sccache`, `awscli2`, `inotify-tools`, `aerc`, `go`, `nodejs`, `openssl`, `terraform` and `tailscale` are pinned in `flake.nix` as the `spira-env` package; the installer builds it with `nix build` when `nix` is present. |
| optional (one feature off) | `claude` (the agent CLI; `SPIRA_AGENT` names the binary), `tmux` (cockpit), `gh` + `openssl` (`pr`-mode repositories), `jq`, `zstd`, `aws` (EC2 spill), `node`, `cargo` | With no `claude` the loop is armed and summons nothing; `ready.sh` warns. |
| operator | `sccache` (WebDAV backend), `inotifywait`, `aerc`, `hunk`, `go` | The installer fetches `duckdb`, `sccache`, `inotifywait` into `~/.local/bin` and copies a vendored `aerc` from the release when they are missing. |
| development | `rustup` (toolchain pinned by `rust-toolchain.toml`), `nix`, `podman`, `bd-embedded`, `terraform` | `podman` runs the test containers; `bd-embedded` is the embedded-Dolt build of `bd` used as the test-fixture engine. |

### Build and install

A release is an immutable directory under `<releases>/` with a `current` symlink; units execute
out of `current`, so a clone is never the running copy.

```sh
make dist      # cargo build --release --workspace, then spira/build-tarball.sh build --workspace .
               # -> ./spira-<UTC timestamp>.tar.gz
```

Write an answers file (`key = value`, `#` comments). These are required; any other key
registered under `[spira]` may also be given and is validated like the rest:

```
instance  = prod                        # name of this installation
id_prefix = sp                          # prefix of your bead ids, no hyphen
home_repo = <repo>                      # the repository this harness works on
db        = <home>/.local/share/spira/db
run       = <home>/.local/run/spira
releases  = <home>/.local/share/spira/releases
dolt_data = <home>/.local/share/spira/dolt
# optional: the home repository's registry row
home_repo_path = <path to checkout>
home_repo_land = push                   # push | pr | queue.local   (default push)
home_repo_base = origin/main
```

```sh
target/release/release install-tarball spira-<ts>.tar.gz --answers answers.txt
export SPIRA_TOML=<path the command printed>   # default: ${XDG_CONFIG_HOME:-~/.config}/spira/spira.toml
<releases>/current/bin/spira-install --dry-run # what it would do
<releases>/current/bin/spira-install           # then for real
```

`release install-tarball` unpacks beside existing releases, swaps `current` atomically and, if
no `spira.toml` exists, writes one from the answers (every unanswered key gets its registered
default; an existing file is validated and never overwritten). With no answers file and a
terminal it prompts; with neither it refuses and names the missing inputs.

`spira-install [<instance>] [--dry-run] [--ephemeral] [--laptop] [--skip-build]
[--no-session-hook] [--system-user] [--populate-lifecycle]` runs, in order: dependencies,
`doctor` preflight, conflict checks, config, same-user lifecycle credential, build, database,
units, lifecycle store, desired state, hooks, cockpit, verify (ending with `ready.sh`).
`--system-user` and `--populate-lifecycle` are standalone single-phase modes. Exit codes: 0
ready, 1 preflight refused, 2 a phase failed, 3 installed but not ready, 5 a conflict (another
copy's units, a live aeon, the gate lock, an instance/run-dir or Dolt-port collision; the
message names the remedy and the override `SPIRA_INSTALL_CONFLICT_CONSIDERED=1`). It refuses a
beads database inside a git checkout, or one whose own repository has a remote.

To re-render and re-enable units without a full install: `units-install [<instance>]
[--diff | --render | --laptop]`; to install missing units without restarting anything:
`unit-ensure [--diff]`. After a release activates, `spira/first-run.sh --range <A>..<B> --repo
<path>` starts each newly shipped oneshot unit once and files a bead for any that fails.

Put `<releases>/current/bin` and `<releases>/current/spira` on your `PATH`; units and aeons get
that prefix from their launchers.

### Configuration

**One source.** The configuration is the file or layered spec named by `SPIRA_TOML` and nothing
else: no search of the checkout, XDG or `/etc`, and no default for a key that is not declared.
Unset or missing, `spira-config` and `conf.sh` refuse by name. (`install`, `doctor` and
`ready.sh` adopt `~/.config/spira/spira.toml` when `SPIRA_TOML` is unset and the file exists,
because they run before anything exports it.) `SPIRA_TOML` may be `base:override[:...]`; every
layer must exist. A value resolves environment variable first, then `spira.toml`, then the
registered default; the environment is the seam test fixtures drive.

**Configuration goes through the CLI only.** Only the `spira-config` crate may open, parse or
write `spira.toml` or the repo map; the `config-fence` lint refuses any other code that does.

| Command | Does |
|---|---|
| `spira-config validate [file]` | exit 1 naming the TOML path of the first error; also refuses a `[spira]` table with no `id_prefix` |
| `spira-config get <dotted.path> [file]` | one value, e.g. `spira.max_aeons`, `repo.<name>.mode`, `persona.builder.model` |
| `spira-config set <dotted.path> <value> <file>` / `unset <dotted.path> <file>` | edit one value in place |
| `spira-config export --sh [file]` | `[spira]` as quoted `KEY=value` lines |
| `spira-config resolve --sh\|--sh-all [--conf-d DIR] [file]` | env > toml > default, as shell assignments |
| `spira-config locate` | the file in force, or the refusal naming why |
| `spira-config init [--out F] [--answers F] [--<key> V]...` | write a first `spira.toml` (never overwrites) |
| `spira-config schema` | the JSON Schema `spira.toml` is validated against |
| `spira-config repo names\|all\|field\|add\|check ...` | the repository registry |
| `spira-config fayth names\|get\|roster\|task\|lane\|partitions\|for-labels\|model` | the persona registry |

On an installed (read-only) release, callers that regenerate config resolve their write target
through `spira-config writeback <candidate>`, which redirects away from the release unless
`SPIRA_CONFIG_WRITE=1`. `aeons` (below) is the friendly door for the two pool-size keys.

**The key registry** is `spira/conf.d/<KEY>`, one file per key: `TYPE=` (`string|u32|u64|bool|list|onoff|czar_stage`),
optional `MAX=` and `SCHEMA=`, `GROUP=`, `DOC=`, and a default block holding the default as one
`: "${KEY:=...}"` statement. `SPIRA_FOO` appears in `spira.toml` as `spira.foo`, and
`COCKPIT_FOO` as `spira.cockpit_foo`. `spira/conf.toml.d/` holds keys that exist only in the
typed config. `spira/conf.d.*.generated.sh` are generated by `spira/conf-gen.sh` and never
edited. Adding a key means adding its file, declaring it under `[spira]` in
`spira-config/tests/fixtures/complete.toml`, listing it in
`spira-config/schema/spira-key-history.txt`, and, when a release adds or drops a key,
declaring it in `spira/config-delta.toml` (applied at activation). Unknown keys are a parse
error.

**Two keys are deliberately not configurable: `SPIRA_HOME` and `SPIRA_REPO`.** Where the
harness is, is a fact about where `conf.sh` sits. The landing gate extracts a branch to a
scratch tree and runs that tree's suites; a config that could point them at the installed copy
would make the gate test the code already in force.

`spira.toml` has four top-level tables:

| Table | Content |
|---|---|
| `[spira]` | the registered keys (`id_prefix` is required) |
| `[repo.<name>]` | `path`, `mode`, `base`, `format`, `lanes`, `forge`, `gate_mode` (`suites` default, or `unit`) |
| `[persona.<name>]` | `model`, `tools`, `labels`, `lane`, `lease`, `system_prompt`, `escalation_model` |
| `[publish.<name>]` | `remote` (`none` = publishes nowhere) and `branch`, for `queue.local` repositories |

**Repositories.** The registry is the pipe-delimited file named by `spira.repo_map` when it
exists (`name | path | mode | base | format | gate | lanes`), else the `[repo.*]` tables.
Register one with `spira-config repo add <name> --path P --queue local|forge [--remote URL]
[--land-ref R] [--format CMD] [--gate CMD]` and audit with `spira-config repo check
<name>|--all`. A repository carries its own gate in the tree under test (`gate.steps`) and, for
repositories other than the harness, its test entry in `.spira/testenv.toml`.

**Personas.** `spira/chamber/<name>.fayth` and `<name>.md` (section 2). The live model per
persona is `persona.<name>.model`; the `FAYTH_MODEL` line only seeds it. The aeon pool size is
`spira.max_aeons`.

### Starting and stopping the world

`world` (also installed as `world.sh`) stops and starts the work as a whole, by the
`Plane=work|observability|maintenance` each unit declares in its `[X-Spira]` section.

| Command | Effect |
|---|---|
| `world status` | halted / draining / running, essential timers, round-VM state |
| `world stop [--why "..."] [--work\|--observability\|--maintenance\|--all\|--hard] [--round-drain] [--round-drain-timeout SECS] [--grace SECS]` | halt planes (default `work`); `--hard` = work + observability; live aeons are stopped through `slay`, never killed |
| `world start [--work\|--observability\|--maintenance\|--all]` | default starts work + observability |
| `world drain [--reboot] [--timeout SECS \| --deadline SECS]` | stop summoning new aeons only; loop and landing keep running; `--deadline` slays what remains at the end |
| `world resume` | lift a drain |

`world` never stops Dolt, the cockpit or the Concierge. A halt names any round in flight;
`--round-drain` waits for it first.

Pool size: `aeons` (show), `aeons set <n>` (fleet ceiling across all personas), `aeons unset`,
`aeons pool <n>` (the task pool, `SPIRA_MAX_AEONS`). Stop one aeon: `aeon stop <bead> [--why
<text>]` or `slay --bead <id> [--why "<text>"] [--keep-work] [--close "<reason>" | --reopen]`.

### Checking health

- `doctor`: read-only; runs every check and exits 1 on any FAIL. It covers release tools,
  config, the store, the compilation cache, round-VM mirror, systemd units, the cockpit, the
  operator channel, the Concierge, git hooks and `spira-env`.
- `ready.sh`: the postflight, "can this installation receive work right now?" (sentinel,
  timers, world, database, ready work, loom, agent, cockpit). Exit 1 on any FAIL or unknown.
- `world status`, `release status [--json]`, and the pages in section 6.

## 6. Operating it day to day

### Filing work

All work enters as a bead through the contract; never `bd create` directly.

```sh
bead.sh contract                    # legal personas, repos and kinds, read from source
bead.sh file "<title>" --for builder --repo <name> [--priority N] [--body-file F] [--parent <id>] [--stack-after <id>] [--target "<metric> <op> <value>"] [--express] [--json]
bead.sh file "<title>" --kind <kind> [--repo <name>] ...
bead.sh amend <id> [--note "<text>"] [--body-file F --replace | --append-body F] [--priority N] [--express]
bead.sh label add|remove <id> <label>
bead.sh dep add <id> <depends-on-id> [--type <type>]
bead.sh dep remove <id> <depends-on-id>
bead.sh drop <id> --reason <text>
bead.sh lint [--all|<id>...]
```

`--for <persona>` derives the partition labels from the persona's own predicate, so the filing
tool and the claim predicate cannot disagree. Kinds without a persona (`event`, `escalation`,
`proposal`, `insight`) are created closed and are deliberately unclaimable. A `--repo` not in
the registry is refused. A bead whose description carries a `Target:` line closes only with a
verdict (`work submit --met|--contradicted|--short`). The sentinel summons aeons for ready
beads; `sentinel --report` prints the open plan beads without changing anything.

### Queue modes

Each repository has a mode (`[repo.<name>].mode`, or the registry file's third column):

| Mode | What the landing pass does with a finished branch |
|---|---|
| `pr` | rebase, confine, force-push, open or refresh the pull request; the PR's CI is the gate (`landing-pass --pass`, its own timer) |
| `push` (default) | rebase, confine, gate, merge in a private landing worktree, push to the base |
| `hold` | rebase, confine, gate, note "gated and held"; never advances the base. `held.sh [--summary] [<repo>]` lists; `held.sh --merge <bead-id>` fast-forwards after confirmation |
| `queue.forge` (alias `queue`) | certify the tip once; the forge's merge queue lands rounds; `queue step` settles open rounds |
| `queue.local` | same certification; rounds are cut by `batcher`, landed with `queue land-local`, published to the forge target with `queue publish` |

A gate returns `PASS`, `FAIL` (only this reopens and charges the branch), `BASE_FAIL` (the base
itself is red) or `NO_VERDICT` (the harness could not judge); nothing lands on the last three.
Inspect or stop a gate with `gate status <bead>` and `gate cancel <bead> [--why <text>]`.
`landing-pass halt [--reason T | --reason-file F|-] [--dry-run]` halts a running landing pass.
Switch a repository between local and forge queues with `queue to-forge [<repo>]` /
`queue to-local [<repo>]`; undo a local landing with `queue rollback-local [<repo>]`.

Read a bead's state with `spira-lc show <bead-id>` and
`spira-lc list [--live] [--state S[,S...]] [--repo NAME] [--hold poison|ask|wait|manual]`. A
closed bead is not a landed bead.

### Rounds

For queue repositories `batcher rounds-local` (every 30 s) cuts, tests and lands rounds, and
`queue step --all` (every 2 min) settles open batches. A round that goes red is attributed to a
member (path-based auto-eject, else a summoned `mender` or `batcher` judgement). Read and drive
rounds with:

Switch a repository's mode with `queue mode-switch <repo> <queue.forge|queue.local>`: it settles
the forge batch first and refuses while one is unsettled. A forge batch left behind by any other
route is settled by `queue verdict` whatever the mode, and `rounds-local` ignores forge
`open`/`prepared` records for a queue.local repo.

```sh
queue round status [<repo>]
queue step <repo>
queue eject <id> [--reason <text>] [--harness-fault] [--dry-run] [<repo>]
queue abandon [<repo>] --reason <text> [--dry-run]
queue open-batch [<repo>] [--members <ids>] [--skip-pregate] [--dry-run]
```

`queue` with no arguments prints the full verb set.

### Releases

| Command | Does |
|---|---|
| `release build <commit> [--repo R] [--target-dir T \| --bin-dir D]` | build `<releases>/<sha>`; prints the sha |
| `release verify <sha> [--no-pre-activate]` | exit 1 listing problems |
| `release activate <sha> [--hotfix <reason>] [--repo R] [--landed-ref REF] [--settle SECS] [--drain-wait SECS]` | switch `current`, re-render and restart changed units, prune |
| `release rollback [--repo R] [--settle SECS] [--drain-wait SECS]` | back to the previous release |
| `release status [--json]` / `release prune [--keep N]` | state; remove old releases |
| `release install-tarball <tarball> ...` | the packaged-tarball pipeline (section 5) |

Every subcommand also takes `--releases D` and `--run D`. A release is never deployed from a
worktree: merging does not deploy, activation does.

### When something is stuck

| Question | Look here |
|---|---|
| What is in each state, and what is stuck beyond its normal dwell? | Loom (`SPIRA_LOOM_ADDR`): `/`, `/stuck`, `/states`, `/lifecycle`, `/aeons`, `/units`, `/rounds`, `/rounds/<repo>/<batch>` (+ `/log`), `/beads/<id>`; JSON under `/api/` (`stuck states lifecycle aeons units ops rounds/<repo>/<batch>`) |
| Work claimed but nobody is working it | `strand report [--json]`; `strand check --dry-run` (`strand check` is the timer path) |
| Which failure classes keep recurring | `census [--with-suppressed]` |
| What the watchers saw | `watchd status`; `watchd peek [name] [--all] [--limit N]`; `watchd tail <name>` (stream from the cursor; one reader per watcher); `watchd next <name> [--timeout SECS]`; `watchd drain [name] [--all]` |
| Operator-facing questions and alerts | `mail list operator --unread`, `mail read operator [<message>]`, `mail count operator`; asks: `spira-lc list-asks [--state S]`, `spira-lc show-ask <id>` |
| The cockpit and Concierge | `cockpit.sh` builds or repairs it and attaches; `cockpit.sh status` changes nothing; `cockpit.sh resume` after a planned reboot drain; `concierge.sh status` |
| A gate | `gate status <bead>` |
| Hold a bead by hand for non-aeon work | `hold.sh <bead-id> [--pid P \| --release]` |
| Statutes and runbooks | `rule list`, `rule show <slug>`, `rule enact <slug> "<text>" (--tool <cmd> \| --mechanism <bead> \| --tool none --reason <why>) [--dry-run]`, `rule audit`, `rule retire <slug>`; `sop list`, `sop show <slug>`, `sop write <slug> [-\|<file>]`, `sop match [-\|<file>]`, `sop lint`, `sop retire <slug>` |

Never read state from the files, logs or process lists another component owns; each state has
one owning command (see the lint rules in section 7).

## 7. Development

### Repository layout

| Path | Contents |
|---|---|
| `Cargo.toml` | one workspace of about eighty member crates, each a binary and/or library; most carry a `DESIGN.md` stating their contract |
| `spira-config`, `lifecycle`, `spira-lc`, `beads-store` | config, the lifecycle machines and their service, the bead-store seam |
| `sentinel`, `aeon`, `landing-pass`, `queue`, `batcher-cut`, `gate*`, `testenv`, `release`, `install` | the loop: dispatch, aeon runner, landing, rounds, gate, test runner, releases, installer |
| `loom`, `cockpit/`, `cockpit-collect`, `watchd`, `mail`, `doctor`, `census`, `strand` | read surfaces and operator tools |
| `spira/` | shell not yet ported, `conf.sh` / `lib.sh`, the registry (`conf.d/`, `conf.toml.d/`), personas (`chamber/`), hooks (`hooks/`), `deps.toml`, `watchers`, and every suite (`test-*.sh`) |
| `systemd/` | unit templates (`@PLACEHOLDER@` substitution; `.service` + `.timer` pairs) |
| `docs/test-plan/*.toml` | the use-case catalogue suites declare coverage against |
| `gate.steps` | this repository's certification gate |
| `flake.nix`, `rust-toolchain.toml`, `Makefile` | pinned tools, pinned compiler, build entry points |

The ownership boundary between this repository and everything that is not it is generated from
`spira/boundary` and kept below; amend that file and run `bash spira/boundary.sh write`, never
edit the region.

<!-- BOUNDARY:BEGIN -->

### Ships in `spira` — the harness

Generic mechanism. A colleague clones this and it carries none of the operator's data.

| path | what it is |
|---|---|
| `spira/` | the harness proper — aeon runner, sentinel and its checks, gate, sending, strand, the chamber and its fayth format, lib.sh, and the test suites that hold them |
| `spira/boundary` | this manifest — it describes the harness, so it travels with it |
| `spira/boundary.sh` | renders this manifest into every document that publishes it; the wiki-side target is configured and skipped when unset, per rule 2 |
| `spira/conf.sh` | the one configuration surface: the loader, the key allowlist, the derived defaults, and `spira_require`, which names a missing program instead of dying as a shell error |
| `spira/exclude.sh` | keeps the beads database and its exports out of this repository — the check the gate runs, the pre-commit hook, and the installer that arms both. A beads database is never public |
| `spira/hooks/` | the pre-commit hook itself, TRACKED and armed by core.hooksPath. .git/hooks is not cloned, so a hook that lived there would reach a colleague missing and unannounced |
| `spira-lint/src/rules/inventory.rs` | the fence that keeps one operator's infrastructure out of a repository meant to be cloned — repository names, hosts, paths, people, dates. It scans comments, which is where all of it was. |
| `spira/inventory-deny` | the tokens that fence refuses beyond the structural ones. Ships EMPTY: a list of somebody else's names is itself the inventory |
| `spira/actors.example` | commit author to harness, for authors the commit graph cannot vote on. Its rows are one installation's roster |
| `auron/` | the watchdog over the loop (Rust) — reads timestamps and counters, raises or clears an alert bead. Its only power is speech: it repairs nothing, restarts nothing and summons nothing |
| `skew/` | is the activated release the latest published (Rust) — the hourly check that the installed release matches the most recent release tag; also the landing gate's fence against work landing in a copy nothing executes |
| `doctor/` | read-only preflight (Rust) — every missing program, unreadable database, unmapped repository and unbuilt panel, named in one pass |
| `census/` | the Maechen census (Rust) — failure classes ranked by frequency from the events table, with open-remedy suppression |
| `spira/incident.sh` | turns a production event into a bead Ops can claim — systemd OnFailure, arbitrary payload, or a spool drain; deduplicates by external_ref |
| `spira/statutes/` | the SEED statute book, one file per statute. Statutes live in the beads KV store, which is per-installation, so a clone gets the mechanism and none of the law unless it ships as text |
| `spira/seed.sh` | writes those statutes into a fresh database, and never over one already in force |
| `spira/bare-main.sh` | operator tool: `check` or `apply [repo]` makes a repository's main worktree bare while its linked worktrees stay working |
| `spira/otel-query.sh` | operator tool: `report` or `sql [hours]` — DuckDB views over the OTLP span files spira-otel writes (verb stats, large responses) |
| `spira/round-spill-profile.sh` | operator tool: `ensure` or `status` prepares the AWS profile and tailscale key location the EC2 spill provider reads, never a credential |
| `cockpit/` | the decisions panel (Rust) and the ops pane — how a human sees what the harness is doing and answers what it asks. Generic; it reads whatever database it is pointed at |
| `loom/` | Loom — a Rust read endpoint over the live beads graph with a per-request budget; refuses to start without a database so it cannot silently serve another harness's graph |
| `systemd/` | unit TEMPLATES. The units in force on a machine are generated from these, never edited in place |
| `install/` | the installer crate (spira-install, units-install, unit-ensure, render-unit): nine sequential phases — preflight, conflict checks, config, build, database, units, hooks, cockpit, verify — ending with ready.sh |
| `spira/uninstall.sh` | inverse of install.sh; walks owned.sh so the two cannot drift; instance-aware and refuses to guess when multiple instances are installed |
| `spira/owned.sh` | single declaration of what one installation owns outside the checkout — the load-bearing contract walked by both installer and uninstaller |
| `spira/ready.sh` | postflight: seven readiness checks after install; exit 3 when installed-but-not-ready |
| `spira/build.sh` | builds Loom and the cockpit panel; a clone that skips this gets a service that exits 2 and an empty panel |
| `spira/configure.sh` | bootstraps ~/.config/spira/spira.toml on first install; never overwrites an existing file |
| `testenv/src/container.rs` | rootless podman container with user systemd for the install-rehearsal suite tier |
| `docs/` | spike investigations and evidence files from the harness's own development |
| `concierge.sh` | one named Remote Control session, so a phone can reach the harness |
| `rule.sh` | enacting a statute writes the beads KV store, which is the harness's substrate |
| `spira/archive.sh` | keeps every session transcript and indexes it by time range and by the lineage id that survives a clear. The mechanism ships; the transcripts and the store they land in are the operator's own and stay out of every repository |
| `spira.toml.example` | the annotated template an operator copies to spira.toml, the one configuration source ($SPIRA_TOML) |
| `README.md` | the harness's own entry point, carrying this table |
| `AGENTS.md`, `CLAUDE.md` | how an agent works ON the harness. Distinct from the wiki repository's own agent instructions, which are how an overseer works WITH it |

### Stays in `brain` — the wiki

Everything whose write target is a page. It may read the harness freely; the harness may not require it.

| path | what it is |
|---|---|
| `wiki/` | the operator's knowledge base: pages, journal, projects, notes |
| `raw/` | source documents, and any JSONL mirror of the database kept in a PRIVATE repository |
| the wiki's own agent instructions | its catalog, its chronological log, and how an overseer works with the harness |
| a tasks generator | regenerates a page listing the beads that need the operator |
| a statute-book generator | regenerates the readable copy of the statutes in force |
| its cron wrapper | runs that generator on a timer and commits only on substantive change |
| a beads exporter | writes the JSONL mirror into a private repository |
| a bead-table generator | regenerates a plan table on a wiki page from the bead graph |
| that repository's own gate | one repository's landing gate, named by its row in repo-map. Every repository declares its own |
| a checkbox ticker | ticks a checkbox on the wiki page that holds it — the one cockpit tool that writes a page |
| its session guards | PreToolUse fences for the overseer's own session. A guard binds the actor who broke the rule, so they live with that actor |
| its session settings | that session's own hooks and environment |
| its skills | whatever lands its output in the wiki |

### In neither repository — data

It belongs to whoever runs the harness. No shared repository holds it, and no beads database is ever public.

| path | what it is |
|---|---|
| the beads database | served by Dolt, addressed as a path. It accumulates internal working notes and agent memories, so it is never public and never in a shared repo |
| the statutes in force | rows in that database's KV store, per-installation. A wiki may render a read-only copy; the harness ships SEED statute text an installer writes into a fresh database |
| `spira.conf`, `repo-map` | the operator's real paths, repositories and personas. The examples ship; these do not. Both are gitignored, and spira.conf is looked for outside the checkout first for that reason |
| the systemd units in force | rendered from systemd/ templates by install.sh, filled from spira.conf. Never edited in place — `systemd/install.sh --diff` is how you find out somebody did |
| the transcript archive | compressed session logs plus their index, written by archive.sh. They carry paths, credentials read aloud and everything anyone ever said, so they live outside every checkout and no shared repository holds them. Nothing deletes them: retention is the operator's decision |
| `.runtime/` | logs, worktrees, leases, cockpit state. Regenerated, machine-local, gitignored |

<!-- BOUNDARY:END -->

### Building

```sh
make build     # cargo build --release --workspace
make test      # cargo test --workspace
make dist      # build + spira/build-tarball.sh build --workspace .
```

Profiles (`Cargo.toml`): `release` (opt-level 2, no LTO, stripped; what a release ships),
`aeon` (inherits `dev`, incremental; what aeon worktrees and the gate build, with the fence
tools such as `spira-lint` and `lifecycle-guard` optimised within it), and `unittest`
(workspace members unoptimised, dependencies as release builds them; the round's unit-test
build). `testenv --profile <p>` selects one; `--with-bins` means `release`. The toolchain is
pinned by `rust-toolchain.toml`.

### Tests

Tests run through `testenv`, which stages the tree and runs suites in a podman container.
Never run a `spira/test-*.sh` suite directly on the host.

```sh
testenv [--mode parallel|serial] [--suites <list|->] [--profile <p>] [--with-bins] [--artifacts <dir>] [--deadline <secs>] [--report [N]] <branch> [<repo-name>]
testenv --suites test-asks.sh,test-bead-file.sh <branch>
testenv status [--json] <results-dir>
```

`testenv testdb …` manages throwaway Dolt instances for suites that need the real engine.
Rust unit tests run with `cargo test -p <crate>`; `work crates` prints the `-p` words for your
branch's diff.

- **Suites** are `spira/test-*.sh`, found by glob. Each carries `# tier: T0..T4` and a
  `# covers:` line (space-separated path globs and use-case ids from `docs/test-plan/*.toml`)
  just above `set -uo pipefail`. A glob that matches nothing is refused (`covers-entries`); a
  use case no suite covers is reported by `plan-matrix`. A suite that cannot run in a container
  carries `# host-reason: <why>` (`bash spira/host-check.sh` enforces container-or-reason);
  `# bare-host: <why>` marks the few that need the runner itself.
- **Selection.** `suite-select gate <base> <head>` maps a diff to suites through their
  `# covers:` lines; a changed source file that no suite claims is an error.
  `suite-select header covers|tier|uc|requires|exclusive|selects-on|testenv-unmet <file>` reads
  one suite's headers. The landing gate runs that selection under a budget, from `gate.steps`.
- **Two places a suite runs.** `spira/gate-suites` is the only hand-written list: a short,
  reasoned set. Everything the glob finds that it does not name is run on a schedule by
  `testenv suites` and files a bead per red without blocking a landing. `testenv suites
  list|names|corpus|status|hygiene|lint|observe-flake|quarantine|unquarantine|disable|activate`
  reads and changes this; a suite's lifecycle state lives in `spira/suite-state` and changes
  only through the queue like any other change.
- **Use-case catalogue.** `test-plan validate --catalogue-dir docs/test-plan [--suites FILE|-]`,
  `test-plan matrix …`, `test-plan render`, `test-plan schema catalogue|matrix`.
- **Habits every suite shares.** A check that finds nothing must first prove it could have
  found something (plant an offender). Test against the real dependency (`spira/testdb.sh`),
  not a hand-written stub. Run in an explicit, minimal environment, so ambient configuration
  cannot decide a verdict. Pin a configured value to a non-default in fixtures, so a literal in
  the code cannot pass.

### How a change lands

A change to Spira is work like any other. File a bead for the harness repository
(`bead.sh file "<title>" --for builder --repo <name>`); a builder aeon claims it in a worktree
on `spira/<id>`, implements it, runs only the touched crates' unit tests, and calls
`work submit`. The commit subject names the bead id. The sentinel, screen, gate and round carry
it from there (section 3), and a release is built, verified and activated from the landed
commit. For a repository in `pr` mode, open the pull request instead.

When working by hand, arm the tracked hooks once with `bash spira/exclude.sh install [root]`
(writes the `.gitignore` stanza and points `core.hooksPath` at `spira/hooks`). The pre-commit
hook runs `exclude.sh staged` (refuses beads data), the cheap lint rules, and
`branch-guard.sh staged` (refuses aeon-identity commits on the base); a `reference-transaction`
hook refuses deleting a bead branch outside the destruction chokepoint; `aeon-fence.sh` is the
PreToolUse hook that blocks queue-operating and credential-touching commands inside aeon
sessions.

### Fences and conventions

`gate.steps` is read from the tree being judged, one directive per line: `bin VAR package`
(build that package from the tree and use it instead of the installed one) and `step
<command>` (an element of the `&&` chain). This repository's steps are `spira-lint`,
`lifecycle-guard --gate .`, `spira/build-fence.sh`, `spira/ci-spill-guard.sh --gate .`,
`spira/boundary.sh check`, then the selected suites. Every fence prints `fence: <name> checked
<n> <unit>` with n > 0; a fence that checked nothing is a failure.

**`spira-lint`** exits 0 clean, 1 findings, 2 usage, 3 a rule refused to report clean, 4 the
tree could not be walked. Rules are discovered from `spira-lint/src/rules/*.rs`; the
shrink-only allow ledgers live beside them. By theme:

- *Config discipline:* only `spira-config` touches `spira.toml` or the repo map
  (`config-fence`); a registered key is read through `spira_config::process::cfg`, never an
  environment fallback or a literal (`config-env-read`, `config-literal-fallback`,
  `conf-key-registry`); a new key needs its schema field, fixture entry, history line and a
  release delta entry (`config-delta`, `config-fixture`).
- *One door per state:* an aeon's phase, a gate's status, a pool, a release's state, a round's
  record, a sift verdict, a testenv result and liveness are read through their owning command
  (`spira-lc`, `gate status`, `round-vm status --json`, `release status --json`,
  `queue round status`, `sift status --json`, `testenv status --json`, `watchd status`), never
  from their files, logs or process lists; nothing reads the journal (`aeon-state-readers`,
  `gate-state-readers`, `pool-state-readers`, `release-state-readers`, `round-record-readers`,
  `sift-state-readers`, `testenv-result-readers`, `liveness-readers`, `journal-reads`).
- *Hygiene:* no installation inventory in tracked files (`inventory`, deny list
  `spira/inventory-deny`, which ships empty); no scratch notes at the repository root
  (`scratch-fence`); every external program declared in `spira/deps.toml` (`deps-lint`); one
  language runtime per repository (`one-runtime`); no download-and-run installs; every
  unattended call has a deadline (`call-deadline`) and every wait is on an object, not a clock
  (`time-only-wait`); payloads go on stdin (`payload-argv-lint`); hash-ordered output is
  sorted; tmux calls are scoped (`tmux-scope-fence`); the Concierge is signalled by inbox,
  never keystrokes (`concierge-send-keys-fence`).
- *Test hygiene:* tier budgets, `wall-clock-budget`, `tmp-leak`, `env-set-var-leak`,
  `chmod-exec-leak`, `testlib-migrated`, `testdb-mode-lint`, `plan-lint`, `plan-matrix`,
  `covers-entries`.

**`lifecycle-guard [--json] [--gate] [--rules <file>] <path>...`** refuses any path that
reaches bead state by a route other than `spira-lc`. It has no allow-list: a finding is cleared
by moving the access to `spira-lc`, deleting it, or arguing a named exception in the owning
crate's design.

**`bash spira/boundary.sh check`** fails when the generated region above is stale.

**Conventions** (from `CLAUDE.md`, which governs anyone editing the harness):

- *Ship the mechanism, not the inventory.* The repository is cloned by people whose
  infrastructure is not yours: no repository, host, person, account, bead id or incident date
  in tracked files. Keep the scar as one generic clause.
- *Configuration, not constants.* Every path, name and label is a registry key with a default
  derived from where the harness is installed.
- *Prefer no comment.* A fact belongs in a name, a guard or a test, where something runs it.
  When a comment is needed, state the rule, not the incident, in a line or two.
- *Prove the check.* A suite that finds nothing must first have found something planted.

## 8. Glossary

| Term | Meaning |
|---|---|
| **absolve** | break-glass clearing of a red recorded against a tip; needs evidence |
| **aeon** | one summoned instance of a persona; claims one bead, works it in a worktree, exits |
| **ask** | an escalation to the operator, with its own machine (`OPEN` → answered, default taken or withdrawn) |
| **base / landing ref** | the ref work must reach to count as landed |
| **batch** | a round's record in the lifecycle store |
| **batcher** | cuts and lands rounds; also the Judge persona for unattributed reds |
| **bead** | one work item: content in the beads store, state in `spira_lifecycle` |
| **bdq** | the one chokepoint through which the harness invokes `bd` |
| **break-glass** | a move the machine allows only with a reason, evidence and the evidence tip |
| **broker** | `broker token\|read` | `token` mints the GitHub App installation token behind `gh-app.sh`; `read` answers `run-view`, `run-jobs` and `artifact-download` for a mapped repo, without log content. |
| **certified** | a gate or round passed an exact tip |
| **chamber** | `spira/chamber/`: persona definitions and briefs |
| **Concierge** | the operator's long-lived session; claims nothing |
| **cockpit** | the operator's tmux workspace: session, mail, decisions, ops dashboard |
| **delivery** | the per-bead sub-machine of `IN_DELIVERY` |
| **disposition** | how a session was cut short: `lapsed`, `thrash`, `slain` |
| **escalation model** | the persona's stronger model used after a bead is rejected twice |
| **express** | the flag that puts a bead ahead of the line |
| **fayth** | a persona definition (`.fayth` plus `.md` brief) |
| **fence** | a static check that fails the gate on a class of violation |
| **gate** | per-branch certification, defined by the judged tree's `gate.steps` |
| **hold** | an orthogonal suspension of a bead: `poison`, `ask`, `wait`, `manual` |
| **lane** | a scheduling slot outside the task pool, or a bead kind a repository admits |
| **lease** | an aeon's time-bounded hold on a bead, renewed while it works |
| **Loom** | the read-only web view of where work is stuck |
| **mender** | persona that triages one failing suite of a round |
| **operator** | the human who decides escalations |
| **partition** | the set of beads a persona may claim, as a label predicate |
| **persona** | a fayth, the role an aeon plays |
| **plane** | `work`, `observability` or `maintenance`: how `world` groups units |
| **poison** | the hold on a bead that exhausted its attempts |
| **release** | an immutable `<releases>/<sha>/` directory; `current` points at the one in force |
| **round** | a set of beads merged and proven together on a round VM, then landed as one fast-forward |
| **round VM** | the disposable machine a round's full corpus runs on |
| **screen / sift** | the cheap pre-round check for conflict, duplicate and policy gaps |
| **sentinel** | the heartbeat that reconciles the graph and summons aeons |
| **SOP** | a runbook (`sop-<slug>`) for a recurring incident, read by `ops` |
| **stack** | the certified tips of a bead's unlanded prerequisites, recorded at claim |
| **statute** | a standing rule (`law-<slug>`) that must name its enforcing mechanism |
| **suite** | a `spira/test-*.sh` test script with `# covers:` and `# tier:` headers |
| **tip** | the exact commit a submission or certification names |
| **trigger bead** | a bead a timer files to summon a persona (groom, warden, sweep) |
| **work** | the aeon's only client onto lifecycle state |
| **world** | the work as a whole; `world stop/start/drain/resume` control it |
