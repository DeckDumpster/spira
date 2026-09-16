# Spira

Spira is an unattended work loop. You decompose a design into **beads** — issues in a
dependency graph — and Spira summons short-lived agent sessions that each claim one bead, work
it in its own git worktree, put the branch through a gate, land it, and exit. Nothing is
long-lived except a handful of systemd timers.

Three properties shape everything else:

- **Deterministic before inference.** Every routine decision is a predicate over the issue
  graph and the commit graph. A model is asked for judgement only after every deterministic
  check has passed and work is still not moving.
- **Nothing durable lives in a session.** A worker holds a *lease*, not state. If it dies the
  lease goes stale and the bead returns to ready. Crash recovery is a property of the
  substrate, not of the agent.
- **Detection outranks rejection.** A gate that refuses bad work is worth less than a watcher
  that notices the pipeline has stopped.

---

## Vocabulary

The names are from Final Fantasy X. Learn them once; they are used everywhere.

| term | what it is |
|---|---|
| **bead** | one issue in the graph — id, labels, dependencies, lease. The unit of work |
| **fayth** | a persona *definition*: a predicate carving its partition out of the graph, the memories it reads, the tools it may use, a model, and what wakes it |
| **aeon** | one summoned *instance* of a fayth. Claims a bead, works it, closes or fails it, exits |
| **the chamber** | `spira/chamber/` — one `.fayth` (definition) and one `.md` (brief) per persona |
| **the sentinel** | the reconcile loop: compare current state to goal state, close the gap |
| **the Cloister** | the landing gate a branch passes before it may merge |
| **the Sending** | reaping the branch and worktree of work that has landed |
| **a pilgrimage** | an epic; it completes when every child has closed |
| **a Sin** | an incident class that keeps recurring because no runbook has broken the cycle |
| **poison** | a bead that has failed its attempt ladder and will not be retried |

A **party member** (`FAYTH_ROLE=party`) travels with you: it has its own summoner and its own
lane, so it is never crowded out. A **task fayth** is summoned for one encounter and dismissed
after it, drawing from the shared aeon pool (`SPIRA_MAX_AEONS`).

---

## The substrate: beads

Everything Spira knows lives in one **beads** (`bd`) database — a dependency-graph issue
tracker over Dolt. It holds two things:

- **The work graph.** Beads, labels, dependencies, leases, attempt counts. Readiness is a
  query, not a scheduler: `bd ready --claim` is atomic, so N aeons can pull from the same
  queue without a coordinator. A lease is a column rather than an agent's memory, which is why
  a dead worker costs nothing to recover.
- **The knowledge base.** `bd remember` / `bd recall`, split by key prefix: **statutes**
  (`law-`) every agent reads at summon, and **SOPs** (`sop-`) the ops persona executes.

A bead carries labels that place it in a persona's partition, a `repo:<name>` label naming the
repository its work belongs to, and optionally `fayth:<name>` — which *narrows* and never
widens: the partition still decides whether the work is claimable at all.

**Keep the database outside every repository.** It accumulates internal working notes and agent
memories, and a path inside a checkout is one `git add -A` away from publishing them.
`spira/exclude.sh` installs the pre-commit hook that enforces this.

---

## How it works

### The loop

Timers, each doing one thing, none waiting on another.

| unit | cadence | what it does |
|---|---|---|
| `spira-sentinel.timer` | 2 min | reconcile the graph — the checks below |
| `spira-auron.timer` | 2 min | watch the sentinel; escalate if the loop has stalled |
| `spira-gate-check.timer` | 2 min | sweep work parked on CI |
| `spira-ops.timer` | 5 min | summon ops for any waiting incident |
| `spira-archivist.timer` | 5 min | rescue a session's unfinished business before it is cleared |
| `spira-maechen.timer` | 15 min | read the failure distribution; cut work to end recurring classes |
| `spira-watchtower.timer` | 30 min | read the pipeline's vital signs and file them as work ops claims |
| `spira-skew.timer` | 1 h | is the harness in force the harness that landed? |
| `spira-suites.timer` | 1 h | run every test suite the landing gate does not |
| `spira-groom.timer` | 6 h | graph hygiene: split, merge, close beads whose premises are gone |

`spira-archive`, `spira-watch-refresh`, `spira-watch-notify`, `spira-moot-sweep`,
`spira-verify-asks`, `spira-promote`, `cockpit-ensure` and `beads-push` keep the surrounding
machinery honest — transcripts archived, watchers running current code, unread events
escalated, panes repaired, databases pushed to their remotes.

### The sentinel's checks

Each is a deterministic predicate naming the single action that closes its gap.

1. **Completed pilgrimages** — an epic whose children have all closed is announced and closed.
2. **Dead workers** — a stale lease is reclaimed. Three cases: a lease that expired, a holder
   `/proc` says is gone, and an orphaned claim held by nobody.
3. **Stale blocked flags** — `is_blocked` is a cached column and goes wrong after an edit.
4. **Poison** — a bead past its attempt ceiling stops being retried and is escalated instead.
5. **Closed but not landed** — a bead closed with no commit naming it unblocks its dependents
   on a promise nobody kept, so it is reopened. *Closed is not landed.*
6. **Landing** — dispatched to its own process, plus the Sending and the CI sweep.
7. **Idle capacity** — ready work plus a free slot is the whole point. The pool is drawn down
   in the order personas are named; an *elastic* persona takes what is left.
8. **Judgement** — everything above passed, work remains, nothing is ready, nothing running.
   Only here is a model asked what is wrong, and only on whether the graph actually *moved*.

**Auron** (`spira/auron.sh`) is the watchdog over the loop, and its only power is speech: it
reads timestamps and counters and raises or clears an alert bead. It repairs nothing and
summons nothing — a watchdog that can act is a second controller with no supervisor. It fires
on sentinel-pass staleness, summon starvation, an unreachable database, and a lease held past
expiry with no live process, and writes a heartbeat so the ops pane can show its own age.

**Landing runs as its own transient systemd unit**, and the unit name is the mutex: systemd
refuses to start a unit already active, so a pass arriving mid-landing declines and moves on.
No lockfile. It writes a status file and an append-only progress mailbox, because "nothing
landed" and "the landing worker never ran" look identical from outside.

### The life of a bead

1. **You file it.** Labels put it in a partition; `repo:<name>` says which repository.
2. **An aeon claims it** atomically under a lease with a TTL the persona sets.
3. **It gets a worktree** cut from that repository's declared base ref, and a brief assembled
   from the persona's `.md`, the statutes in force, and the bead itself.
4. **It works.** A heartbeat refreshes the lease only while something observably moves; a
   persona quiet for `FAYTH_STALL_BEATS` checks stops heartbeating and its lease expires.
   There is deliberately no wall-clock ceiling on the worker persona — a clock cannot tell slow
   from stuck, and killing on one charges an attempt for being legitimately long.
5. **It commits naming the bead id**, cuts the review, and **exits.** An aeon does not sit
   watching CI; it labels the bead and the sweep raises priority on red or releases it on green.
6. **The gate judges the branch.**
7. **The landing pass merges or opens a pull request**, per the repository's declared mode.
8. **The Sending reaps** branch and worktree once a commit on the base branch names the bead.

### Landing

Each repository declares in `repo-map`: the checkout an aeon may work in, the ref its work is
cut from and judged against, how it lands, a formatter, and the gate command it must pass.

**Never assume the base branch is `main`** — it is declared rather than derived, because every
automatic source is a local cache that can be stale, absent, or answering whatever branch a
human last looked at.

Three landing modes, and the column also decides whether there is CI to wait for:

- `push` — merge into base and push. The branch is the release.
- `pr` — push, open a pull request, arm auto-merge, let the repository's own CI be the
  authority. Only `pr` has CI; a bead parked on CI under another mode waits forever, invisibly.
- `hold` — gate it, note it once, leave the branch for a human.

**The gate is cheap and mechanical** — a gate that needs judgement is a review, not a gate. One
layer is universal (a shell script that does not parse is the commonest way an unattended change
breaks a harness); everything else is the repository's own declared command.

**It fails closed, which is not the same as blaming the branch.** Four outcomes: `PASS`, `FAIL`,
`BASE_FAIL`, `NO_VERDICT`. Nothing lands under the last three, but only `FAIL` says the branch
is at fault and only `FAIL` costs it an attempt.

---

## The personas

The chamber ships six. Adding a seventh is two files, not a code change.

| persona | role | model | what it does |
|---|---|---|---|
| **builder** | task, elastic | Sonnet | the Guardian, and the only persona that writes code. Its partition is deliberate plan work and nothing else. Named last, so it takes whatever the pool has left |
| **spike** | task | Opus | researches one question to a costed feasibility document and carries no conversation |
| **ops** | lane | Haiku | the healer, and the only persona whose work arrives from outside the plan. Matches an SOP, executes it, or writes one |
| **groomer** | lane | Haiku | reconciles the DAG: splits unsplittable beads, merges duplicates, closes beads whose premises are gone, corrects mislabelled lanes |
| **maechen** | lane | Opus | the unsent historian: reads the failure distribution, names recurring classes, cuts the work to end them |
| **concierge** | operator | Opus | the operator's own session and the phone's way in. Unbounded remit; claims nothing, and nothing but a human may summon it |

**Why spike exists** when the builder could do the reading: feasibility research is the worst
thing to do inside a long-lived session, because almost nothing it reads is needed once the
question is answered, and every page stays in context and is re-read on every later turn. A
spike starts at the floor, reads, writes one document, and exits — the conversation gets the
document, not the reading. `confine.sh` enforces the deliverable at the gate: **a spike may
leave a branch and must not leave a merge.**

**Ops reads both books**, statutes and runbooks; the builder reads statutes only, because a
shelf of runbooks would push the law it must obey off the end of the context budget. Ops's
session is walled to comfortably less than the sweep cadence, so a session ends before the next
snapshot arrives and unfinished findings are cut into beads rather than lost.

Two mechanisms worth knowing:

- **The fence on the predicate.** An installation that imported a predecessor's beads has a
  ready queue full of work that predecessor is still doing. A persona whose predicate is too
  loose refuses to claim *at all* rather than trusting a config string to be right.
- **The closing rule.** A persona may declare that its work is not resolved until something is
  written back to the shelf. Ops does: an incident closed with no runbook coming out of it has
  its close undone and the bead marked poison. "It matched, it held, it taught us nothing new"
  is a complete outcome, and so is "it did not hold". Only silence is outlawed.

---

## Loom — the graph in a browser

`loom/` is a Rust read endpoint over the live beads graph and a page that renders it.

- `GET /api/beads` — the raw rows as `bd` returns them, plus the dependency edges between them.
- `GET /api/ops` — the ops dashboard, a mirror of the cockpit's health column, readable on a phone.
- `GET /` — the page, with its scripts embedded in the binary at compile time. One thing to
  install, one thing to start, no asset directory to keep in sync.

**The server serves the graph and nothing else** — no layout, no buckets, no ranking. All of
that measured 2 ms in the browser at the live corpus and 60 ms at a hundred times it, so the
page derives every view: where the work lives (a treemap by repository), what blocks what
(dependency chains), how it has proceeded (churn), flow, build, and ops.

**Its query carries a budget.** A query that overruns is *refused* rather than served late:
serving a stale snapshot would be kinder to one reader and fatal to the design, because it
hides the one signal that says a query per request has stopped being cheap enough. The work
either side of the query is reported next to it as `refresh_ms` — a refresh that grew slow by
growing its payload rather than its query would otherwise be invisible.

It **refuses to start without a database** rather than letting `bd` discover one from its
working directory, which would come up healthy serving a different harness's graph.

---

## Watching, and being answered

**The cockpit** (`cockpit/`) is a terminal UI over the beads that want a human — decisions,
FYIs, notifications and alerts — because chat is a log, and a log cannot hold an open question.

- **A verdict is written into the bead**; the close reason *is* the answer. Never a
  side-channel log, so any session can read what was decided.
- **An escalation is a decision request, not a problem report**: the question with a default,
  what is blocked until it is answered, and what it costs to reverse the wrong choice.
  `cockpit/ask.sh` files one, and a default is close to mandatory.
- **Nothing in the pane closes an alert** — only whatever asserted a self-clearing condition
  may retract it. A hand-closed alert whose condition still holds comes straight back, which
  teaches the operator that the pane does nothing.
- **A failed probe renders `?`, never 0.** A panel reporting a broken check as all-clear
  displaces the suspicion that would have prompted a look.
- **A session that escalates watches for the answer** — `cockpit/watch-answers.sh loop`.

**Watchers are rows in a manifest** (`spira/watchers`), not unit files: the installer renders
one systemd unit per row and disables any instance whose row has gone. The contract is two
files — an append-only log and an integer cursor — so `tail -n +$((cursor+1)) -F` is a
conforming client. A row may carry a **health assertion**, because a watcher reports silence
identically whether nothing happened or it is reading the wrong database.

**Incidents.** `spira/incident.sh` turns a production event into a bead ops can claim, from a
failed systemd unit, an arbitrary payload, or a spool flush. `spira/install-intake.sh` wires
`OnFailure=` drop-ins over `SPIRA_ALERT_GLOB` so a failed unit files itself; dedup keys on the
unit name, so a recurrence labels the open incident rather than filing a second one.

**The archivist** (`archivist.sh`) rescues a session's unfinished business before it is cleared
— questions asked and never answered, findings never filed, verdicts never recorded. It reads
the transcript from disk rather than the conversation, costing that session no turn and no
tokens: a persistence step that adds turns makes the problem worse in exactly the sessions that
need it most.

**`skew.sh`** asks whether the harness in force is the harness that landed. A second copy inside
a repository is how work aimed at the harness lands in it, passes its gate, and never runs.

---

## Staying inside its means

| program | question it answers |
|---|---|
| `governor.sh` | how much of this machine may Spira use, and what did it get for it |
| `capacity.sh` | is the rate-limit window shut, and which attempts did that cost |
| `tokens.sh` | what is actually spent, and on what |
| `attempts.sh` | what every claimable bead carries on the retry ladder, and why |
| `yield.sh` | what the gate is *worth*, recorded beside what it costs |
| `ctx-meter.sh` | how much context a session carries, and how close that is to the edge |

The governor only ever *withholds* — a persona's own concurrency stays the ceiling — and it
averages `/proc` across passes rather than sampling, because a single two-second reading lands
on or between a test suite at random and swings the budget between 0 and 2 at a constant worker
count. Attempts charged while the rate-limit window was shut are given back, but only where the
session log that proves it survives; reclassification refuses to reason about the rest.

---

## Statutes and SOPs

Two bodies of written knowledge in the same store, injected into a session at summon; a persona
declares which prefixes it reads.

- **Statutes** (`law-`) are how to behave. `rule.sh enact <slug> "<text>"` is one command,
  because a rule that depends on remembering a second step is a resolution, not a mechanism.
  `rule.sh retire` is the other half, and retiring is as deliberate an act as enacting.
- **SOPs** (`sop-`) are how to fix. `sop.sh` writes, matches, recalls and synthesises them. An
  SOP has a *shape* — match expression, checks, steps — and the program refuses prose, because
  a runbook written as a paragraph cannot be matched to an incident by a program.

Write both to be read a thousand times: one paragraph, imperative, the scar as a single clause.
Every agent pays that context on every session, and `enact` refuses anything over 130 words.
`spira/statutes/` is the seed set a fresh install starts with; `seed.sh` never overwrites a key
you have amended, and ships machinery rules only.

**A rule tightens when it is re-violated**, not when it is annoying: practice → written down
once → statute every agent reads → a program that refuses. That program is a *fence*, a polite
refusal rather than a wall, so every guard names its own override and binds the actor that
actually violated the rule.

---

## Tests

`spira/test-*.sh`, discovered by glob and never from a list, so adding one puts it in the timed
set automatically. `spira/gate-suites` names the subset run on the scheduled gate pass, each
with the reason it earns the wait; everything the glob finds that the list does not name is run
by `suites.sh` on a timer, which files a bead per failure and blocks nothing. The two sets
cannot be edited into overlapping, and a deleted suite stops being run with no edit anywhere.

Every suite declares what it covers on a `# covers:` line. Three properties a new one needs:

- **A check that finds nothing must first prove it could have found something.** Plant an
  offender, require the matcher to say so, and only then believe it when it is silent.
- **Test against the real dependency** on a throwaway instance — `spira/testdb.sh` for a beads
  database, `spira/testenv.sh` for a rootless container with real user systemd — never a
  hand-written model of it. A stub reproduces the surface you remember, so its gaps surface as
  failures in correct code.
- **Run in an explicit, minimal environment.** `hermetic.sh` refuses a suite that reaches the
  real box: a suite inheriting a real config is asserting about one machine.

The landing gate is deliberately *not* the suite set. What this system **is** is N workers
pulling from a graph into a merge queue, and every failure it has had is a property of that
pipeline — two things running at once, a lock, a queue that stopped moving — never a function
returning the wrong value. So the gate is the sub-second fences that guard the irreversible,
plus a soak that reproduces a merge-queue livelock. The 17-minute, 43-suite gate it replaced
found zero real defects on the morning the pipeline could not land anything, and was itself
most of the contention that livelocked the queue.

To try a harness command against a real database without touching production:

```sh
bd -C "$(spira/testenv.sh scratch)" <command>   # a throwaway database path
spira/testenv.sh shell                          # a subshell where everything targets the fixture
```

---

## Installing

You need `bd`, `git`, `flock`, `python3`, and whichever coding-agent CLI your personas name.

| program | what it does | if absent |
|---|---|---|
| `bd` | the beads issue tracker — the substrate | nothing runs |
| `git` | every repository operation | nothing runs |
| `python3` | every JSON payload the harness parses | nothing runs |
| `flock` | serialising writers that share one path | nothing runs |
| `dolt` | the SQL server beads stores its database in | `bd` cannot reach a database |
| `gh` | opening and landing pull requests | repositories whose mode is `pr` cannot land |
| `claude` | the agent an aeon is a session of | no work is done, only reported |
| `tmux` | the cockpit panes | no attention surface |
| `cargo` | builds Loom and the cockpit panel | no panel, no Loom; the loop is unaffected |
| `node` | gates the browser page's view model | that one suite skips |
| `inotifywait` | delivers mail the moment it arrives (`inotify-tools`) | mail waits for the next session start |
| `aerc` | the operator's mail client | read the Maildir with any other client |

```sh
git clone <this repo> spira-harness && cd spira-harness
./install.sh
```

On a fresh box with the prerequisites installed, those two commands are enough. `install.sh` is
the one entry point and runs nine sequential phases — 0 preflight, 0.5 conflict checks, 1
config, 2 build, 3 database, 4 units, 5 hooks, 6 cockpit, 7 verify.

```
install.sh [<instance>] [--dry-run] [--ephemeral] [--laptop] [--skip-build] [--no-session-hook]
```

- **`<instance>`** — the name of this installation (default `prod`). A different name installs
  a second instance alongside an existing one.
- **`--dry-run`** — print each phase's intended action without changing anything.
- **`--ephemeral`** — isolated instance for CI or test: its own database and runtime directory,
  statutes seeded, agent pointed at a stub, no session hook or alert drop-ins.
- **`--laptop`** — tuning for a battery-powered box.
- **`--skip-build`** — skip `build.sh` when binaries are pre-built.
- **`--no-session-hook`** — skip session-hook registration (phase 5).

### Exit codes

| code | meaning |
|---|---|
| `0` | ready — `ready.sh` passes (warns allowed) |
| `1` | preflight refused — `doctor.sh` named a fatal missing dependency |
| `2` | a phase failed — config, build, database, units, or hooks |
| `3` | installed but not ready — every phase completed, `ready.sh` exited non-zero |
| `5` | conflict — another harness copy owns these unit names, a live aeon is running, a landing pass is in flight, the instance argument disagrees with the config, or a Dolt server is listening on the configured port with a different data directory |

Every conflict guard names its own override; `SPIRA_INSTALL_CONFLICT_CONSIDERED=1` bypasses the
whole conflict phase. The unit renderer carries two more refusals, both overridden by
`SPIRA_INSTALL_FORCE=1`: it refuses when the checkout is not on its declared base ref or is
behind it, and while `spira-aeon-*` instances are active.

### What readiness means

`ready.sh` asserts seven rows at the end of every install. FAIL or `?` (unknown) exits non-zero;
WARN does not.

| # | row | on failure |
|---|---|---|
| 1 | the sentinel timer is active | fails |
| 2 | the world is not halted | fails |
| 3 | the database is readable and shipped statutes are in force | fails |
| 4 | `sentinel.sh --report` names an open bead under `SPIRA_GOAL` | WARN if none |
| 5 | Loom answers 200 at `/api/beads` inside its budget | fails |
| 6 | the configured agent binary is present | WARN by design — an ephemeral install is valid without a credentialled agent |
| 7 | the two tagged tmux panes are present | WARN by design — the loop runs without a terminal surface |

The install rehearsal (`spira/test-install-rehearsal.sh`, run by `testenv.sh` inside a container
with real systemd) proves rows 1–3 and most of 5.

### Uninstalling

```sh
spira/uninstall.sh [<instance>] [--yes] [--dry-run] [--purge] [--purge-database]
```

With no argument and exactly one instance installed it removes that one; with several it refuses
to guess. Three retention tiers:

- **Removed by default:** systemd units, the linger flag, `~/.local/bin` symlinks into this
  tree, session hooks, alert drop-ins, cockpit panes.
- **Kept unless `--purge`:** your config directory and `$SPIRA_RUN` (the transcript archive and
  worktrees).
- **Kept unless `--purge-database`:** `$SPIRA_DB`, `$SPIRA_DOLT_DATA`, `$SPIRA_TESTDB_DATA`.
  This tier requires typing the bead count back to confirm.

After removing the declared inventory it sweeps for `spira-*` units; anything found but not
predicted by `spira/owned.sh` is reported as a stray rather than silently left behind.

`spira/owned.sh` is the single declaration of what one installation owns outside the checkout,
walked by both installer and uninstaller so the two cannot drift. `doctor.sh` is the read-only
preflight and names every missing program, unreadable database and unmapped repository in one
pass, distinguishing *fatal* from *warn*. The units in `systemd/` are **templates** — never edit
an installed unit; edit the template and re-run `install.sh`, and `systemd/install.sh --diff`
tells you when somebody did.

---

## Running it

```sh
spira/world.sh status        # what is up, what is down, what is running
spira/world.sh stop          # halt the loop: no summons, no landing, no live workers
spira/world.sh start

spira/sentinel.sh --report   # the gap, changing nothing
spira/strand.sh report       # work that exists and is not moving, with the reason
spira/watchtower.sh --show   # the pipeline's vital signs
spira/suites.sh list         # every suite, where it runs, what it claims to cover

spira/slay.sh --bead <id>      # stop one aeon cleanly and make its bead say what is true
spira/hold.sh <bead-id>      # claim a bead for a non-aeon actor
spira/release.sh <bead-id>
```

`world.sh` deliberately touches neither the databases — stopping the loop must never risk the
data, and a stopped database makes every diagnostic you are about to run fail — nor the panes
the operator is reading, because halting the loop must not blind the person halting it.

Other operator-facing scripts carry `--help`. Highlights: `promote.sh` fast-forwards the
production checkout and restarts only changed units; `stage.sh` stands up an isolated Spira for
testing; `canary.sh` runs an end-to-end pipeline canary; `escape.sh` summons an aeon directly,
bypassing pool and lane checks; `aeons.sh` sets the fleet ceiling.

---

## Configuration

**One surface: `spira.conf`.** Every path, name and label resolves through it. Three sources,
first to speak wins: the **environment** (how every test suite drives a fixture, and what keeps
a suite off your real database), the **config file**, then a **default derived from where the
harness is installed** — so a clean clone with no configuration resolves to something coherent
rather than to someone else's box.

It is **parsed, not sourced.** A config file that is shell can set `PATH`, run a command, or
shadow a library function, and it is read by a process that summons agents. `KEY = value`, `#`
comments, an allowlist of keys, and an unrecognised key is *reported*, not obeyed — a typo
silently ignored is a setting you believe is in force. Two keys are deliberately not settable
from the file: where the harness *is* is a fact about where its loader sits, and a config that
could point the gate's scratch tree back at the installed copy would make the gate test the code
already in force, and pass.

The other files you own:

| file | what it says |
|---|---|
| `repo-map` | `repo:<label>` → checkout, base ref, landing mode, formatter, gate |
| `spira/chamber/*.fayth` | your personas: partition, model, concurrency, lease, tools |
| `spira/watchers` | what should be watching, one row per watcher |
| `spira/inventory-deny` | extra names the publish fence must refuse. Ships empty |
| `spira/actors`, `spira/prefix-map` | only what the graph cannot vote for itself |

That last row is the rule the map files follow: **derive what can be derived, and keep a file
only for what cannot.**

### Two fences on this repository

This repository is meant to be cloned by people whose infrastructure is not yours, and two
checks run from its own gate to keep it that way.

- `spira/exclude.sh` — a beads database is never public. The check, a pre-commit hook, and the
  installer that arms both.
- `spira/inventory.sh` — refuses to ship one operator's infrastructure: absolute paths rooted in
  a home or workspace directory, real e-mail addresses, provenance marks naming a person and a
  date. **It scans comments rather than stripping them**, because that is where all of it was:
  a version that stripped them passed a tree naming seven repositories, a host and a person.

Ship the **mechanism** — the rule, the trap, the reason a guard fails closed. Do not ship the
**inventory** — a repository name, a deploy path, a host, a person, a bead id, a date. Those
teach a colleague's agent to reason about a machine that does not exist, and sometimes to act
on it.

---

## Four facts that most often produce a wrong answer

- **A bead is closed when an agent says the work is done; it has landed when a commit on the
  base branch names its id.** Different claims. Verify with an ancestry check, never by
  comparing tip SHAs — a tip moves under you.
- **The base branch is not always `main`.** Ask for it; never assume.
- **Ready sees bead status, not merge state.** A bead can be ready while its prerequisite
  exists only in an open pull request, so ready-but-unstarted is often correct sequencing.
- **A check that reports success is not evidence the thing works.** Before believing a green
  signal, ask what it would look like if the check itself were broken. On the day this system
  took over, eleven of fourteen defects were in the checking machinery rather than the work.

---

## Repository layout

<!-- BOUNDARY:BEGIN -->

### Ships in `spira` — the harness

Generic mechanism. A colleague clones this and it carries none of the operator's data.

| path | what it is |
|---|---|
| `spira/` | the harness proper — aeon runner, sentinel and its checks, gate, governor, sending, strand, drain, the chamber and its fayth format, lib.sh, and the test suites that hold them |
| `spira/boundary` | this manifest — it describes the harness, so it travels with it |
| `spira/boundary.sh` | renders this manifest into every document that publishes it; the wiki-side target is configured and skipped when unset, per rule 2 |
| `spira/conf.sh` | the one configuration surface: the loader, the key allowlist, the derived defaults, and `spira_require`, which names a missing program instead of dying as a shell error |
| `spira/repo-map.example` | example rows showing the six columns. The real rows are operator data |
| `spira/exclude.sh` | keeps the beads database and its exports out of this repository — the check the gate runs, the pre-commit hook, and the installer that arms both. A beads database is never public |
| `spira/hooks/` | the pre-commit hook itself, TRACKED and armed by core.hooksPath. .git/hooks is not cloned, so a hook that lived there would reach a colleague missing and unannounced |
| `spira/inventory.sh` | the fence that keeps one operator's infrastructure out of a repository meant to be cloned — repository names, hosts, paths, people, dates. It scans comments, which is where all of it was |
| `spira/inventory-deny` | the tokens that fence refuses beyond the structural ones. Ships EMPTY: a list of somebody else's names is itself the inventory |
| `spira/actors.example` | commit author to harness, for authors the commit graph cannot vote on. Its rows are one installation's roster |
| `spira/auron.sh` | the watchdog over the loop — reads timestamps and counters, raises or clears an alert bead. Its only power is speech: it repairs nothing, restarts nothing and summons nothing |
| `spira/skew.sh` | is the activated release the latest published — the hourly check that the installed release matches the most recent release tag; also the landing gate's fence against work landing in a copy nothing executes |
| `spira/doctor.sh` | read-only preflight — every missing program, unreadable database, unmapped repository and unbuilt panel, named in one pass |
| `spira/incident.sh` | turns a production event into a bead Ops can claim — systemd OnFailure, arbitrary payload, or a spool drain; deduplicates by external_ref |
| `spira/statutes/` | the SEED statute book, one file per statute. Statutes live in the beads KV store, which is per-installation, so a clone gets the mechanism and none of the law unless it ships as text |
| `spira/seed.sh` | writes those statutes into a fresh database, and never over one already in force |
| `cockpit/` | the decisions panel (Rust) and the ops pane — how a human sees what the harness is doing and answers what it asks. Generic; it reads whatever database it is pointed at |
| `loom/` | Loom — a Rust read endpoint over the live beads graph with a per-request budget; refuses to start without a database so it cannot silently serve another harness's graph |
| `systemd/` | unit TEMPLATES plus install.sh. The units in force on a machine are generated from these, never edited in place |
| `install.sh` | the one entry point: nine sequential phases — preflight, conflict checks, config, build, database, units, hooks, cockpit, verify — ending with ready.sh |
| `spira/uninstall.sh` | inverse of install.sh; walks owned.sh so the two cannot drift; instance-aware and refuses to guess when multiple instances are installed |
| `spira/owned.sh` | single declaration of what one installation owns outside the checkout — the load-bearing contract walked by both installer and uninstaller |
| `spira/ready.sh` | postflight: seven readiness checks after install; exit 3 when installed-but-not-ready |
| `spira/build.sh` | builds Loom and the cockpit panel; a clone that skips this gets a service that exits 2 and an empty panel |
| `spira/configure.sh` | bootstraps ~/.config/spira/ on first install; never overwrites an existing file |
| `spira/testenv.sh` | rootless podman container with user systemd for the install-rehearsal suite tier |
| `docs/` | spike investigations and evidence files from the harness's own development |
| `concierge.sh` | one named Remote Control session, so a phone can reach the harness |
| `rule.sh` | enacting a statute writes the beads KV store, which is the harness's substrate |
| `spira/archive.sh` | keeps every session transcript and indexes it by time range and by the lineage id that survives a clear. The mechanism ships; the transcripts and the store they land in are the operator's own and stay out of every repository |
| `beads-push.sh` | pushes the beads database to its configured Dolt remote. The mechanism ships; the remote it is pointed at is the operator's own and is private |
| `spira.conf.example` | the annotated template an operator copies to spira.conf. Every key optional, every default derived from where the harness is installed |
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

---

## Prose, and why the comments are long

Every comment in this codebase exists because something failed in a way that was not obvious
from the code, and the next reader is entitled to know which. State the rule first, then the one
clause of why. Never leave a correction on top of a wrong statement — say the thing as it now
stands.

`CLAUDE.md` is the guide for an agent working *on* this harness. The brief an aeon gets when the
harness summons it comes from a persona in `spira/chamber/`, not from that file.
