# Spira

Spira is an agent harness for software work. You file work items (**beads**); Spira calls AI
agent sessions (**dwarves**) to do them, each in its own git worktree and branch; and it lands
the results onto a repository's landing ref through gated **rounds** that run the full test
corpus before anything merges. Every step between "filed" and "released" is a task of one
scheduler process, reading and writing explicit state machines in one control store, so the
loop keeps going with nobody watching. Humans decide only escalations: the few questions the
loop cannot answer for itself (permissions, policy, destructive actions, design) are routed to
the operator, and everything else is retried, reworked, held or dropped by rule.

It is a Cargo workspace of single-purpose Rust crates plus a thin shell layer in `spira/`. On
one Linux box it runs as two long-lived processes and a handful of supporting units:

| Process | What it is |
|---|---|
| `spira-runtime serve` | **The scheduler.** One process runs every periodic and event-driven task of the loop. |
| `spira-lc serve` | **The store server.** The only process that opens the control store; everything else asks it over a Unix socket. |
| dwarf units | One transient unit per agent session, started by the scheduler and gone when the session ends. |
| supporting units | The bead content database, metrics and logs, the web view (`loom`), the off-box backup, the operator's cockpit. |

## Why it is shaped this way

- **State machines, not scripts.** A bead, a round, an ask, a mend and a release each have an
  explicit lifecycle. A task moves one step and returns, so a crash costs one step, not the
  loop.
- **One scheduler.** There are no per-job timers, watchers or forked loops to keep in step.
  A task is a handler with a cadence or a triggering event; the scheduler owns when it runs.
- **One writer of state.** Every state change is a compare-and-swap sent to the store server,
  which applies it or refuses it and records both. Nothing else touches the file.
- **Nothing lands unproven.** Work reaches the landing ref only through a round that has run
  the repository's tests in a clean environment against the exact tree it merges.
- **Humans decide only what the loop cannot.** Everything else is retried, reworked, held or
  dropped by rule, and what is held says who lifts it.
- **Mechanism, not inventory.** The repository ships rules and guards; names, hosts and data
  belong to whoever runs it.

## The scheduler

`spira-runtime serve` is the loop. It reads its configuration once at start and pins it, then
runs tasks: calling dwarves, screening submissions, gating branches, cutting and settling
rounds, landing, cleaning up, watching for stuck work, and reconciling what the stores say
against what is on disk.

- **A task is a handler**, registered with a name (`sentinel-call`, `sift`, `gate.worker`,
  `batcher.local`, `queue.step`, `landing`, `hel.sweep`, `watch.*`, `reconcile.*`,
  `trigger.*`). Its cadence is a config key, `SPIRA_RUNTIME_<TASK>_SECS`.
- **Tasks are woken by events as well as by time.** A dwarf's exit wakes the caller to refill
  its slot. Nothing polls for what an event can tell it.
- **Scheduler state is journaled in the control store**, so a restart resumes where it
  stopped: what is due, what effects are open, each task's carried state.
- **The same contract runs in simulation.** The `runtime` crate defines the event and handler
  contract, the journal, the scheduler and a conformance suite every executor must pass. The
  `sim` crate runs the scheduler on virtual time with a seeded source of every ordering and
  duration decision, so a schedule that went wrong can be replayed.
- **Stopping is one command.** `world stop|drain|resume|start` halts or releases the whole
  loop; a drain lets running sessions finish and calls no new ones.

The systemd units that remain are the two servers, the content database and its durability
helpers, metrics, the off-box backup, and the operator's surfaces. The jobs that used to be
timers are tasks.

## The stores

Spira keeps state and content apart.

| Store | Holds | Reached through |
|---|---|---|
| **Control store** | Every lifecycle record (bead, delivery, round, ask, mending), readiness and claims, holds, the audit log of every applied and refused event, locks, and the scheduler's journal | `spira-lc`, and only `spira-lc` |
| **Content database** | A bead's words: title, body, labels, priority, dependency edges, notes; statutes and runbooks | `bead` / `work` (which go through `spira-lc`) |
| **Run directory** | Tree certificates, round working files, gate queue, time series, log records, watcher logs | the program that owns each file |
| **Git repositories** | Branches `spira/<id>`, round branches, the landing ref, archived rounds | dwarves (their own branch), the queue (rounds and landing) |
| **Releases** | Immutable release directories and the `current` link | `release` |

The control store is the part most worth understanding:

- **It is one SQLite file** (`SPIRA_LC_STORE_PATH`), opened exclusively by `spira-lc serve`.
  A second opener is refused, and a verb run without the server answers "cannot tell" rather
  than fall back to anything else.
- **It is a key-value contract, not a schema of tables.** The `state-store` crate defines the
  semantics: revisions, transactions with preconditions, history, watch, locks with fencing
  tokens, key lifetimes, request ids that make a retried write happen once. `state-store-sqlite`
  is the backend, and any backend that passes the contract's conformance suite gets all of it.
- **Reads are ranges over indexes.** Each key's live row is the only copy on disk; a read at
  the head is a key-ordered range, and a state-filtered list is a range over a state index.
  Recent history is a bounded ring in memory, which is also the watch log.
- **Every request has a deadline** (3 seconds). A request that cannot finish answers "cannot
  tell", which is safe to retry; work begun past the deadline is refused unstarted, and
  follow-on writes a request still owes are written by a settler under a deadline of their own.
- **Answers are four exit codes:** 0 applied, 1 no such row, 2 cannot tell (retry), 3 refused
  (do not retry). A refusal is a normal answer and is recorded.
- **Hot reads do not ask the store at all.** A long-lived process keeps the rounds, open
  batches and ready counts in memory (`store-view`), kept current by the store's watch from the
  last revision it saw, and refuses to answer once that picture is older than its configured
  staleness.
- **It is backed up off the box.** Every commit appends to an outbox the shipper reads;
  `lc-backup` ships it hourly and a daily drill restores it and reports a verdict.

A bead's *state* therefore never comes from its text, and no program but `spira-lc` decides
it. Static analysis (`lifecycle-guard`) refuses code that reaches for state any other way.

## How work flows

```
 bead ──► dwarf ──► screen ──► gate ──► round ──► landing ──► release
 filed    claims    conflicts   certifies  merges    fast-forwards   builds, verifies,
 READY    one bead  and policy  the tip    a set of  the landing ref  activates the landed
          in its    before a    (tests in  certified                  commit
          worktree  round       a clean    tips
                                box)
```

1. **File.** A bead is filed by a person, a session, a GitHub issue or a trigger task. Its
   content goes to the content database and its lifecycle record starts in `READY`.
2. **Call and claim.** The scheduler counts claimable beads per persona and calls dwarves up
   to the configured pool. Each dwarf claims exactly one bead (a compare-and-swap, so two
   cannot win it) and works it on `spira/<id>` under a lease it renews while it makes
   progress.
3. **Submit.** The dwarf commits to its branch and submits; the store reads the tip from the
   branch itself and refuses a tip a round already found red.
4. **Screen and certify.** A screen sends back what conflicts, duplicates or breaks policy
   before it costs a round. The gate certifies the exact commit in a clean container.
5. **Round and land.** Certified tips are merged together in a round and proven again as a
   set against the full corpus. A red is attributed to the member at fault, which is ejected
   with the evidence while the rest re-run. A green round fast-forwards the landing ref.
   Changes that are only text land on their lint, without a round.
6. **Release.** A release is built from the landed commit, verified, and activated by an
   atomic link swap; merging never deploys by itself.
7. **Clean up.** Branches and worktrees of landed work are removed; a dead session's lease
   lapses and its bead becomes claimable again with the work preserved.

The full walk-through, with the crate and task behind each step, is
[docs/architecture.md](docs/architecture.md).

## Repository layout

| Path | What is there |
|---|---|
| `spira-runtime/`, `runtime/`, `sim/` | the scheduler, its contract and journal, and virtual-time simulation |
| `spira-lc/`, `state-store/`, `state-store-sqlite/`, `store-view/`, `lifecycle/` | the store server, the control-store contract and backend, in-memory hot reads, the lifecycle machines |
| `bead/`, `work/`, `spira-claim/`, `gh-intake/` | filing, the dwarf's client, claiming, GitHub intake |
| `dwarf/`, `sentinel/`, `rule/`, `habits/` | the session runner, the caller, statutes, session habits |
| `sift/`, `gate*/`, `batcher*/`, `queue/`, `landing-pass/`, `round-vm/`, `testenv/` | screening, gating, rounds, landing, the test environment |
| `release/`, `install/`, `skew/`, `doctor/` | building, installing and checking a release |
| `reconciler*/`, `watchd/`, `watchtower/`, `strand/`, `hel/`, `census/` | reconciling, watching, finding stuck work, cleanup, failure census |
| `cockpit*/`, `loom/`, `mail/` | the operator's panes, the web view, and mail to and from the operator |
| `spira/` | the shell layer and `conf.d/`, one file per configuration key |
| `systemd/` | the units an installation gets |
| `docs/` | concepts, architecture, components, install, operating, development |

## Quick start

Spira runs as systemd **user units** on Linux. Prerequisites and the full procedure are in
[docs/install.md](docs/install.md); in short:

```sh
make dist      # build the workspace and package ./spira-<UTC timestamp>.tar.gz
target/release/release install-tarball spira-<ts>.tar.gz --answers answers.txt
export SPIRA_TOML=<path the command printed>
export SPIRA_RELEASE=<releases>/current          # the release install runs from; no default
<releases>/current/bin/spira-install --dry-run   # what it would do
<releases>/current/bin/spira-install             # then for real
```

The answers file names the instance, the bead id prefix, the home repository and the data
directories; [docs/install.md](docs/install.md) lists every key.

Check it is healthy:

```sh
doctor          # read-only; exits 1 on any FAIL
ready.sh        # "can this installation receive work right now?"
world status    # halted / draining / running
```

File your first bead with `bead.sh file "<title>" --for builder --repo <name>`; see
[docs/operating.md](docs/operating.md).

## Where to go next

| Read | For |
|---|---|
| [docs/concepts.md](docs/concepts.md) | the vocabulary: bead, dwarf, persona, lane, round, gate, release, statute |
| [docs/architecture.md](docs/architecture.md) | components and stores, and a bead from filing to released |
| `state-store/src/lib.rs`, `runtime/src/lib.rs` | the control-store contract and the scheduler contract, in their crates' own module docs |
| [docs/components/](docs/components/README.md) | one page per group of binaries and units |
| [docs/install.md](docs/install.md) | prerequisites, building, configuration, starting and stopping, health |
| [docs/operating.md](docs/operating.md) | filing work, queue modes, rounds, releases, when something is stuck |
| [docs/development.md](docs/development.md) | repository layout, building, tests, how a change lands, conventions |
| [docs/glossary.md](docs/glossary.md) | every term, one line each |
| [docs/README.md](docs/README.md) | the whole corpus, including designs, runbooks, spikes and the test plan |

## Contributing

A change to Spira is work like any other: file a bead against this repository and let a
builder dwarf take it, or work by hand on a branch. Either way, read
[docs/development.md](docs/development.md) first, and `CLAUDE.md` before editing: this
repository is cloned by people whose infrastructure is not yours, so it ships mechanism (rules,
guards, the reason a check fails closed) and never inventory (repository names, hosts, people,
paths, bead ids).

Run the suites that cover what you changed with the project's test runner, in a container,
never directly on the host. Details are under *Tests* in the development guide.
