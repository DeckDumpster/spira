# round-controller

A round is a resource with one owner and one store. This crate replaces the batcher, piece by
piece: each later piece of the controller lands as a step of `pass`, behind its own scenario.
Until the cutover it is **inert** — no unit, no timer, no install-manifest entry, no change to a
shared verb's behaviour. The world sim's sim-only `round-controller` actor
(`sim/actors.toml`, units under `sim/units/`, which nothing installs) is the only thing that
runs it.

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
- `[[store]]` — `key`, `data`, optional `ttl_secs`: seeded through the contract on the
  in-memory backend before the first pass.
- `[[verb]]` — `verb`, optional `repo`, one of `ok = { key = "value" }` or
  `refuse = { code, text }`, and the `lock_hold_ms` the verb reports. Answers are taken in
  order. `certify` is answered by the fake round VM unless a refusal is scripted.
- `[[vm]]` — `identity` (the batch's head), `verdict` (`green`, `red`, `fault`), `suites`,
  and `duration_secs`, scaled by the profile's `duration_factor`.
- `[[expect]]` — `name`, `count` (`calls`, `calls:<verb>`, `outcomes`, `outcomes:<kind>`,
  `store-writes`, `vm-runs`, `lock-hold-ms`), `equals`, optional `inverted`.

Every scenario is held to two invariants under every profile: every finished pass has a
recorded verdict, and nothing happened that no script asked for — an unscripted verb call,
pass or spawn, a pass started with no free runner slot, or a pass over its cap.

`scenarios/profiles.toml` holds `default` and `reference-minimum`; every scenario runs under
both.
