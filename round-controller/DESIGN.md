# round-controller

A round is a resource with one owner and one store. This crate replaces the batcher, piece by
piece: each later piece of the controller lands as a step of `pass`, behind its own scenario.
Until the cutover only its `sifts` verb is installed: `spira-sift.service` runs it, and it
screens the proto-round exactly as `batcher sifts` did. Every other verb is **inert** — no
unit, no timer, no install-manifest entry, no change to a shared verb's behaviour. The world
sim's sim-only `round-controller` actor (`sim/actors.toml`, units under `sim/units/`, which
nothing installs) is the only thing that runs them.

The Intent, Non-goals and Test strategy below are the approved design's, verbatim but for its dated attributions.

## Intent

Replace the batcher with a **round controller**, in which a round is a resource with one owner and one store, so that:

1. **The same inputs are never tested twice.** A pass's identity is (members@tips, base, image digest, harness version, selection). An identity that holds a verdict never runs again unless an input changes. The Judge's own sub-passes (a plain rerun and a base run) belong to the verdict they are judging and are not repeats.
2. **A passing round is landed or escalated, never discarded.** A green round whose tested base can no longer land becomes SUPERSEDED, keeping its evidence, and its members are re-cut at once.
3. **A red names a culprit only with evidence.** A fault blames no member. A base, image or install fault stops that repo's lane, out loud.
4. **Cutting takes seconds and happens just in time for a free slot.** Running is scheduled. No lock is held across a test run, and one repo's round never delays another repo's cut.
5. **It is correct on tiny hardware, proven in the simulator.** Under the `reference-minimum` profile (1 runner slot, a 2 vCPU / 4 GB round VM), the 10-31 replay passes with no fault, no over-cap and no lost verdict. The real 2 vCPU round is a measurement taken after the world is released, not a cutover gate.
6. **Its tests are comprehensive and fast.** Every decision branch in the controller crate is covered, measured per test in each bead's gate. Unit tests take milliseconds and scenarios under 100 ms, on an injected clock.
7. **It stops when told.** Every pass obeys `world.halted`, scoped stops and the andon section before it does anything else.
8. **A hung runner cannot hold a slot.** A runner that stops making progress loses its lease within its phase deadline plus one TTL, its VM is proven dead, and the slot frees.

**Done when:**
1. The four releases in *Release composition* have shipped in order, and each bead has shipped in the release it names.
2. The 10-31 replay, built from the reconciler's captured fixture, shows every member certified green on 10-31 landing exactly once, 0 green rounds abandoned, and at most one wasted pass per pathology.
3. Every registered round repo lands one real round on the controller after cutover (`round-controller cutover-check` exits 0), and the batcher's code and units are gone.
4. The rollback to release R3 has been drilled once before cutover.
5. The world is released only when the state store, this controller and the minimal reconciler are all done and working. The order is fixed: the round-controller cutover, then the reconciler's arm step, then `world start`.


## Non-goals

- **Rewriting the Attributor, the lifecycle machine or the selection core.** These are the good parts, kept as libraries. The lifecycle machine gains arms (late boundaries, ATTRIBUTING re-drive, SUPERSEDED); it is not rewritten. *Price:* the controller inherits their shapes, including the Attributor's budget model.
- **Patching the batcher in place**, except for the five narrow R1 fixes to code that `round-recert` also runs. Patching D1–D16 one at a time is about 10–16 beads and leaves the single-lock, single-oneshot shape underneath. *Price:* the Concierge lands through a defective batcher until R4, mitigated by R1.
- **Bigger hardware.** It has historically hidden defects rather than fixed them. *Price:* lane capacity grows only by removing waste and by EC2 side-by-side rounds.
- **The andon cord.** It gets its own design before cutover. Until then the controller's contradictions are section conditions plus a page, and it pulls no cord. *Price:* a contradiction gets a page, not a stop; the Concierge stops the lane by hand if needed.
- **A production shadow run.** The world sim gets a sim-only controller actor; production never runs both. *Price:* the first production evidence is the cutover itself, mitigated by the R3 rollback drill.
- **A real tiny-hardware round before release.** *Price:* the simulator cannot see OOM or a real over-cap on 2 vCPU; the measured run after release can.
- **Reconciler actions that duplicate the controller's own loop** (`redrive-land`, `redrive-parent`, `release-orphan-lease`, `repoint-child`) and **`stop-lane`**, which belongs to the andon. The first four stay internal controller steps, taken by `pass` every pass (land, orphan re-drive, supersede-and-re-cut, lease expiry), and the reconciler never calls them. *Price:* the reconciler steers the controller only through `hold-member`, `concurrency ±1` and escalation.


## Storage

The store keys, from the design's Storage section. Everything lives in the state store's
control plane and is reached only through spira-lc; bead state stays under the lifecycle's
authority.

| Key | Holds | Written by |
|---|---|---|
| `batches/<repo>/<id>` | the round: lifecycle state, pass, head, `tested_base`, stack predecessor, members (inside the value), ejected, worktree, actor, forge PR and phase, `last_lease_token` | queue verbs and the controller, fenced |
| `batches/<repo>/<id>/progress` | in-flight pass and sub-pass results, advisory | the runner, fenced |
| `batches/<repo>/<id>/status` | the Lander's typed blocker and the inputs of the last refusal, advisory | the Lander, fenced |
| `batches/<repo>/<id>/redrive` | re-drive attempts: expired token, launch result, next attempt time | `pass` |
| `verdicts/<repo>/<hash>` | the identity's summary: class, attempt count, back-off, last attempt | CAS on `mod_rev` |
| `verdicts/<repo>/<hash>/attempt/<n>` | one attempt's verdict, with its sub-passes | create-if-absent |
| `verdicts/<repo>/by-member/<id>/<tip>` | the identity hash that proved this member red | the Judge, only for proven members |
| `faults/<repo>/<base>/<image-digest>` | a standing lane fault: cause (Base, Image, Install), evidence, since | the Judge |
| `locks/round/<repo>/<batch>` | the round's driver lease | the contract's `lock` |
| `locks/runner-slot/<pool>/<n>` | one runner slot | the contract's `lock` |
| `slots/<pool>/<n>` | the slot's last holder: unit name and VM id, advisory | the runner |
| `telemetry/sections/RoundController/<repo>` | the section, in R-02's kind (sp-iq5m32); regenerable telemetry, so its `txn`s write no `events/` key | the controller |
| `intents/<section>/<id>` | `{target, from, to}` plus the shared `ack` field, with the contract's TTL | the reconciler writes; the controller acks |


## Test strategy

- **Seen red at the parent tip, every time.** Each bead's own controller scenario runs on its parent commit without the fix, and the red is quoted in the commit. That costs milliseconds (law-a-regression-test-must-be-seen-to-fail; change review C7).
- **A sim-only controller actor in the world sim.** `sim/actors.toml` gains a `round-controller` actor that only scenarios select through `real_actors`. It is never installed and it is not a production shadow run. Each bead turns its own world twin green against it, so integration is spread across the beads rather than found at the last one (change review C6).
- **Comprehensive, measured.** Every decision branch in `cut`, `pass`, `run`, `judge`, `land`, leases, slots, the ledger, the section and the conditions is covered by a scenario or a unit test. Coverage runs in each bead's gate: per test for the `round-controller` crate, and as one aggregate branch-coverage run over the named functions in shared crates. It is never a step of every round (change review C8).
- **Fast.** Clock, process runner, store and launcher are injected, with fakes in tests (law-new-subsystems-are-simulation-ready). Nothing sleeps, waits out a timeout or boots a VM. Under 10 ms per unit test and under 100 ms per scenario, the full replay included; these are reported, never asserted (law-no-wall-clock-budgets-in-the-corpus).
- **Through the real call path.** `cut`, `pass`, `run`, `judge` and `land` run end to end against fakes. Lock holds are measured inside the real `queue` verbs, driven through `queue::ops::round::run` with `World` fakes on a virtual clock, not on a fake that cannot hold a lock (change review C10).
- **Correctness on tiny hardware in the simulator.** Every scenario runs under `default` and `reference-minimum`. This proves ordering, slots and no lost verdict on one slot; it cannot prove memory or real over-cap, which is 33's job.
- **The replay is data, not prose.** Its counts come from the reconciler's captured fixture. Acceptance is "every member certified green on 10-31 lands exactly once, and 0 green rounds are abandoned", with at most one wasted pass per pathology (change review C10, C11).
- **Off switches and liveness are scenarios:** halted, scoped stop, andon pulled, andon absent; hung child, hung runner, lease lost mid-pass, old unit still alive, launch failure.
- **Production:** per repo, green-to-landed dwell, repeat and Judge passes, slots per pool, stack depth, lease age against p95, and queue lock hold per verb, on the rounds dashboard, plus goodput through the reconciler.


## What every controller bead's acceptance includes

1. Its own controller scenario, seen red at the parent tip without the fix, then green.
2. Its world-sim twin green against the sim-only `round-controller` actor.
3. No uncovered branch in its scoped code, from the coverage run in its own gate
   (`coverage-scope.toml`: per test across this crate).
4. The `test-real-clock` lint clean, and durations quoted from the per-test report.

## Seen red

A scenario that has never failed proves nothing. For each bead:

1. Write the bead's scenario under `scenarios/` and add `scenario!(scenario_<slug>)` to
   `tests/scenarios.rs`.
2. Run it on the parent commit with the fix reverted: `cargo test -p round-controller
   scenario_<slug>` through testenv's unit tests.
3. Quote the failing expectation, which names itself (`expectation <name> unmet under
   <profile>: counted <n>, expected <m>`), in the commit message.
4. Apply the fix and run it green under both profiles.

Every scenario may carry an `inverted = true` expectation: one that must fail, naming itself.
`harness-smoke` carries one; it is the positive control that the harness can see what it
counts.

## Scenario files

`scenarios/<slug>.toml`, every key checked and an unknown one refused by name:

- `description`, `repos` — what it is about, and the registered repos `pass` visits.
- `[run]` — `epoch` (virtual epoch seconds), `passes`, `every_secs`, and `jitter_secs` drawn
  from the seeded generator (`SPIRA_SEED`, else 1).
- `same_without` — step names: the scenario is run again without them, its expectations
  aside, and fails on the first verb call or line (the section's and its acks' aside) the two
  runs differ on.
- `[[store]]` — `key`, one of `data` and `json` (for a value TOML cannot spell, such as
  `null`), optional `ttl_secs`: seeded through the contract on the in-memory backend before
  the first pass. `[[store_step]]` takes the same value and `ttl_secs`; keys under `intents/` and `telemetry/` need one.
- `[[verb]]` — `verb`, optional `repo` and `batch`, one of `ok = { key = "value" }` or
  `refuse = { code, text }`, and the `lock_hold_ms` the verb reports. Answers are taken in
  order. `certify` is answered by the fake round VM unless a refusal is scripted.
- `[[vm]]` — `identity` (the batch's head), `verdict` (`green`, `red`, `fault`), `suites`,
  and `duration_secs`, scaled by the profile's `duration_factor`.
- `[lease]` — `ttl_secs`, `pass_wall_max_secs`, `redrive_retries`, `ec2_max_lifetime_secs` and
  `[lease.deadline_secs]` (`boot`, `build`, `suites`, `subpass`, `certify`): the lease keys the
  scenario runs under. Absent, the bench's own values, which are not the shipped defaults.
- `[[runner]]` — `repo`, `batch`, `phase`, `start_secs`, `marks_every_secs`, optional
  `marks_stop_secs` (a hung child) and `hangs_secs` (a hung runner): a runner that takes the
  round's lease and renews it through the library every TTL/3, gated on its marks. When the gate
  shuts or a renewal is refused it tears down; a hung one stays alive.
- `[[unit]]` — `name`, `state` (an `is-active` word, or `unreadable`), optional `until` (epoch
  seconds) from which systemd no longer knows it; a unit never named is one systemd does not
  know. `[[vm_fact]]` — `unit`, `fact` (`none`, `running`, `unreadable`,
  `ec2-unreadable` with `launched_at` in epoch seconds), optional `until` (epoch seconds) from
  which the VM is gone. `[[launch]]` — `ok = true` or
  `refuse = "<why>"`, answered in order; an unscripted launch is a fault. A start with
  `finish_secs` is a runner that holds its lease and slot that long, then records its round
  GREEN and gives both back; it renews nothing, so the scenario's lease TTL must outlast it.
- `[off]` — `halted` (`run/world.halted`) and `stop = [{ repo, owner, reason, until }]`, scoped
  stops on those repos' rounds lanes, for the whole scenario. The andon section is a `[[store]]`
  seed under `telemetry/sections/Andon/`.
- `[slots]` — `spill` (`SPIRA_ROUND_VM_SPILL`'s word) and `ec2` (`SPIRA_ROUND_RUNNER_SLOTS_EC2`),
  or `per_profile = true` alone: the profile's `runner_slots`, the Proxmox slot first and the
  rest on EC2 spill. Absent, the bench's: spill on with three EC2 slots.
- `[[slot]]` — `repo`, optional `pool`, `n`, `ttl_secs`, `released`: a slot the harness hands the
  cut itself after each pass. A scenario with one is about the cut, and runs without the
  schedule step, which would cut on slots of its own.
- `[[slot_user]]` — `name` (its holder and unit), `start_secs`, `hold_secs`, optional `prefer`
  (pool words, default `proxmox` then `ec2`), `hand` (asks through `grant_for_hand`) and
  `retry_secs` (default the renewal interval): a round's runner or a hand driver that takes a
  runner slot through `slots::grant`, records its unit and VM, renews every TTL/3, and tears its
  VM down before it releases. A slot it loses while holding is an unscripted fault.
- `[[git]]` — `repo`, `landing` (the sha its landing ref points at) and `history` (commits
  already under it). A `land` that answers ok moves the ref to the round's head.
- `[[lease_release]]` — `repo`, `batch`, `secs`: the round lease let go then, as a runner that
  finished gives it back.
- `[[expect]]` — `name`, `count` (`calls`, `calls:<verb>`, `outcomes`, `outcomes:<kind>[:<batch>]`,
  `blockers:<word>` (rounds the Lander held so), `rounds:<STATE>` (round records in it),
  `store-writes`, `vm-runs`, `lock-hold-ms`, `launches`, `launches:<unit>`, `renewals`, `redrive-attempts`,
  `first-orphan-secs`, `slot-grants[:<pool>]`, `slot-refusals[:hand]`,
  `refusals-naming-holder` (refusals naming the holder of every slot not given back),
  `vms-at-once[:<pool>]`, `first-grant-secs`, `section-writes`, `section-refusals`,
  `section-raised:<Type>`, `section-distance:positive`, `section-last-distance` (rounded up),
  `section-green-with-blocker`, `acks:<applied|already-applied|refused>`, `acks-stale:<json>`,
  `opened-with:<bead>`, `stops:<word>` (repos stopped by that off switch), `calls-of:<repo>`,
  `writes-under:<prefix>` (keys written there since seeding), `members:<STATE>` (lifecycle
  records in it), `runners-at-once[:<repo>]`, `repos-at-once`), exactly one of `equals` and
  `at_most`, optional `inverted`, and optional `profile`, the one profile it is held under.
  `store-writes` leaves out the section door's own writes.

The store's GC ticks before every pass, so a lapsed lock is gone by the pass that follows it.

Every scenario is held to two invariants under every profile: every finished pass has a
recorded verdict, and nothing happened that no script asked for — an unscripted verb call,
pass or spawn, a pass started with no free runner slot, or a pass over its cap.

`scenarios/profiles.toml` holds `default` and `reference-minimum`; every scenario runs under
both.

## Launcher

A runner is the transient user unit `round-controller-run-<batch>-<lease-token>`
(`death::unit_name`, which refuses a batch `round-recert` would refuse). A re-drive takes a new
lease, so its unit never shares a name with the old runner's, and proof of death reads the old
name. `real::launch::start` runs `systemd-run --user --unit=… --collect --quiet` with only
`SPIRA_TOML` and `SPIRA_RELEASE` set, then `round-controller run` with both tokens; a non-zero
`systemd-run` is `Failed` with its text whole unless `is-active` shows the unit running.
`SPIRA_LAUNCH` and `SPIRA_LAUNCH_CTL` name the programs standing for `systemd-run` and
`systemctl`; a world sets both to `sim-launch`. A re-drive grants a slot, takes the lease,
records the unit under the slot, then launches; a failed launch gives both back and is recorded.

## Pass

`pass` checks the off switches first for each repo (`switches::check`): `run/world.halted`, then a
scoped stop on the repo's `rounds` lane, then the andon sections under
`telemetry/sections/Andon/`, where a condition `True` whose `reason` is `RoundController/<repo>`
(or the whole kind, `RoundController`) is a pulled cord. Under any of them the repo gets only its section
published, which names the switch, and one `stopped <switch>` outcome naming the owner, reason
and lift condition; a store that
cannot list the andon sections stops it too. The runner checks the same three before its pass and
each sub-pass. Otherwise the steps run in a fixed order: forge sweep, orphans, supersede, land,
schedule, intents, section (the forge sweep does nothing until its piece registers it). The
orphans step leaves a round that can no longer land to supersede. The schedule step takes a slot
for a repo with members ready and room in its stack, cuts on it, takes the round's lease and
launches the runner with both tokens; a failed launch gives both back and records the attempt in
`redrive`, and a round whose lease cannot be taken is abandoned before any pass ran. Nothing in
`pass` waits on a VM, sets `SPIRA_QUEUE_LOCK_HELD` or takes the per-repo queue lock. The hand
verbs `cut --members` and `launch` are the operator's and check no switch.

## Lander

`land::step` takes each GREEN round of a repo, lowest in its stack first, under a fresh lease of
its own. A round stacked on a live, unlanded round is held `StackedOnUnlanded` from the store's
own facts. A head already under the landing ref is recorded LANDED by `FastForward` from git,
with no `land` call. Otherwise `queue round land --lease-token`: `Ok` lands it; a refusal is
classified by its exit and first word only, `stacked` holding it `StackedOnUnlanded`,
`base-moved` superseding it, the deploy-fault exit recording `DeployFault` (LANDED when the ref
moved, GREEN when it did not), and any other word `Refused { text }`. Every landing moves the
lane's `landing_ref` in the txn that records it. The blocker and the inputs it was refused
against (head, base, parent, its state and head) are kept at `batches/<repo>/<id>/status`, and a
held round is tried again only once one of them differs. Nothing here abandons a round.

## Slots

`slots::grant` takes the first free slot in the caller's pools, in order: `proxmox`, always one
slot (the host's one VM round at a time), and `ec2`, `SPIRA_ROUND_RUNNER_SLOTS_EC2` slots, offered
only while `SPIRA_ROUND_VM_SPILL` is a word under which round-vm places a pass on EC2. A slot is
the lock `locks/runner-slot/<pool>/<n>`, taken in one txn with its record `slots/<pool>/<n>`
(CAS on the record's `mod_rev`). A holder records its unit before launching it and its VM once
round-vm names one, and marks the record released only once its VM is gone. A slot whose last
holder lapsed unreleased is granted again only on `death::prove_runner_dead` of the recorded
unit and VM; one that recorded no unit launched none. `grant_for_hand` is the same grant, so a
hand round and a controller round share the one Proxmox slot. A refusal names every slot's
holder, its unit and TTL left, each pool offering none and the key that opens it.

## Section and intents

`pass` ends with `intents` and then `section`, so the section shows what every other step did.
Both reach the store's section and intent verbs only through the `Lc` port: the binary spawns
`spira-lc`, and the scenario bench runs spira-lc's own dispatch on its store, so admission
judges each write in both. `section` builds `RoundController/<repo>` from the store and
publishes it when a status field changed, and otherwise once per `target_latency` (5 min);
a write another one moved first is re-read once. `intents` applies each unacknowledged intent
with `intent apply`, the from→to compare-and-swap on its knob and its ack in one txn, or acks it
refused with the bound it breaks, and sets the concurrency knob once at the lane's depth. The
knobs are `knobs/RoundController/<repo>/concurrency`, which the cut reads its stack depth from,
and `knobs/RoundController/<repo>/hold/<id>/<tip>`, each naming who lifts it, whose member the
cut leaves out at that tip. Both key shapes live in `spira_desired_state::sections::round_controller`,
which the reconciler's selector builds its targets from too. The section's `repeat_identity_holds`
names, for each open `RepeatIdentity`, the members proven red under it at their tips; the
selector holds the first one not already held, from null. A stopped or halted repo takes no step; its section says which.
