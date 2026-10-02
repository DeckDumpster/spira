# Host-wide admission — pipeline shaping (sp-f4ig1)

The gate has owned host-wide admission since sp-4vq2q: `SPIRA_CERTIFY_PAR` flock slots under
`$SPIRA_RUN/gate-admission`. This document extends that one pool into **three**, one per heavy
phase of an aeon's loop, and says how each is sized. The shared code is
`spira_config::admission` (every heavy tool already depends on `spira-config` for the build
wrapper, `spira_config::build`); the agent-facing entry is the `spira-admit` binary of the
same crate. `gate/DESIGN.md` §"Admission" is unchanged except where this document says so.

## 1. Intent

On 2026-09-30 the box jammed three times while ~14 agents worked at once: load 64 on 32
cores; memory full-stall 59% and 66% with ~20 `rustc` and 34 dolt fixtures alive; and `/tmp`
(a tmpfs sharing the RAM) overflowing. Gate trials already queued on the certify slots.
Nothing else did. An agent's own `cargo build`/`cargo test` in its worktree and its own
`testenv` runs bypassed admission entirely. So the agents' peaks coincided: they compiled
together, then tested together, then gated together.

What the operator ordered (Ryan, 2026-09-30):

* *"what you want to avoid is a thundering herd: N aeons doing compiles at the same time,
  then tests at the same time, then gates at the same time. throughput is higher if you can
  have aeons running in each phase."* The answer is **pipeline shaping**: a separate
  host-wide pool per heavy phase, so at any moment some aeons compile, some test, some gate,
  and none of the three peaks stacks on another.
* *"stagger, don't throttle."* Earlier, per-job limits (cargo `-j`, `batch_maxpar`) were cut to
  survive the peak, and every gate was slowed by it, even a gate running alone. Then
  **law-reduce-the-count-never-throttle-the-job** was enacted: when the box congests,
  lower the number of concurrent jobs and let each one run at full speed. The pools
  control **how many** jobs run in each phase **and nothing else**.

## 2. Non-goals

* **Any per-job speed limit.** That covers CPU quotas, cargo `jobs` caps, `--test-threads`
  caps, reduced suite width or `batch_maxpar` cuts, and nice/ionice. This change also
  *removes* one the gate already had (§6 D3).
* Admission for the round (it runs on the round VM, law-isolate-greedy-work-in-vms), for
  `release build` (the landing path publishes tested binaries through `--bin-dir` and does not
  compile, DESIGN-build-cache.md §1 item 4), or for the model session itself (it is cheap).
* Changing the gate's pool mechanism. The gate keeps its flock slots and its
  `SPIRA_GATE_LOCK_WAIT` bound, because `landing-pass` probes `slot.N.lock` and the
  landing pass's budget depends on that bound (§6 D4).

## 3. Contract

### 3.1 The three pools

| pool | what takes a slot | holder | size key (env / spira.toml) | directory |
|---|---|---|---|---|
| **compile** | a cargo build of a workspace: an agent's own `cargo build/test/check/clippy` in its worktree (through the aeon's build wrapper, §3.3); testenv's in-place build | the cargo process (agents) or the testenv process | `SPIRA_COMPILE_PAR` / `compile_par` | `$SPIRA_RUN/compile-admission` |
| **test** | an agent's own `testenv` trial, from container `up` to teardown | the testenv process | `SPIRA_TEST_PAR` / `test_par` | `$SPIRA_RUN/test-admission` |
| **gate** | a gate trial that runs suites or unit phases (unchanged) | the gate process (flock) | `SPIRA_CERTIFY_PAR` / `certify_par` | `$SPIRA_RUN/gate-admission` |

* **A slot is admission, never a limit.** An admitted job runs at the default width: cargo's
  own `-j` (all cores), testenv's configured `batch_maxpar`, and the gate's unit phases at the
  host's cores (§6 D3).
* **One phase at a time, never two slots at once.** testenv holds its compile slot for the
  build only, releases it, then takes a test slot for `up` through teardown. No holder waits for
  a slot while it holds another, so the pools cannot deadlock (there is no hold-and-wait).
* **Inherited admission.** A process whose environment carries `SPIRA_ADMISSION=<pool>:<slot>`
  runs inside a job that is already admitted. It takes no slot of its own. The gate sets this on
  every command it runs, so its testenv and cargo run on the gate's slot, except that a gate's build also takes a compile lease without waiting, so agent builds queue behind it (D11). testenv's own build
  uses the plain build wrapper (no `spira-admit`), so it runs on the compile slot testenv took
  in-process. Inheritance also follows the process tree
  for compile leases: a cargo whose ancestor already holds a compile lease (for example a nested
  `cargo` run from a test) takes none.
* **Waiting never fails.** A compile or test waiter waits for as long as it takes. It prints, to
  its own stderr, once when it starts waiting and then every 30 s:
  `waiting for a test slot: 2 of 2 held by sp-abc (testenv pid 123, 41s), sp-def (testenv pid 456, 12s)`
  and, once it is admitted, `admitted to test slot 1 after 37s`. Under `--deadline`, testenv
  moves its deadline later by the time it spent waiting. The budget meters work, not queueing.
  The gate's bounded wait is unchanged (§6 D4). It now prints the same kind of waiting line.
* **Sizes are in units, and the queue is first come, first served.** Every job has a weight.
  It is 1, or `WEIGHT_RELEASE` = 4 for a release (LTO) build (§5). A waiter is admitted when
  two things hold: no older live waiter is queued ahead of it, and its weight fits in what the
  live leases leave of the size. A waiter is also admitted when the pool is empty, so a job
  heavier than the whole pool runs alone instead of never running. A waiter's queue position is
  its first `since`, which is written once.
* **Sizes are re-read on every pass of a wait**, so a size an operator raises takes effect at
  once. Lowering a size never evicts a holder: the pool drains to its new size as holders
  finish.

### 3.2 Leases (compile and test)

A lease is a file `slot.<n>` in the pool's directory. Its content is one line:
`pid=<p> start=<starttime> who=<label> since=<epoch> waited=<secs> last=<epoch> weight=<units>`
(a line without `weight=` reads as 1). Every
read-modify-write of the directory happens under an exclusive `flock` on `<dir>/lock`, which is
held only for the scan, never across a wait or a job.

* **Live** means `/proc/<pid>/stat` exists and its field 22 (starttime) equals `start`. A
  dead or recycled pid frees the slot. The next scan **reclaims** it and writes the lease's
  telemetry row with `end=reclaimed`. Nothing depends on a holder exiting cleanly.
* **Take**: under the lock, reap the dead first. If a live lease already names this holder,
  it is ours (`last` is refreshed). This is what lets the hundreds of `rustc` invocations of
  one cargo share a single lease. Otherwise, if a live lease names an ancestor of the holder,
  admission is inherited. Otherwise, if this holder is at the head of the queue and its weight
  fits (§3.1), a lease is written at the lowest free slot number. Otherwise the pool is busy:
  the waiter file is written (once), and the scan returns the live holders for the waiting
  line.
* **Release**: remove `slot.<n>` if it still names this holder, and write the telemetry row
  with `end=released`. testenv releases through a guard's `Drop`. A cargo lease is never
  released explicitly: it ends when the cargo process ends, and the next scan reclaims it.
  `spira-admit status` scans too. The row's `held_secs` is `last − since` (the last compile
  the lease saw), so the row stays honest when the reclaim happens late.
* **Waiters** leave a `wait.<pid>` file (`start= who= since= weight=`) while they wait. It is removed
  once they are admitted, and reclaimed if they die. `spira-admit status` counts these files.

### 3.3 The compile wrapper (`spira-admit` as `RUSTC_WRAPPER`)

An agent's cargo gets `RUSTC_WRAPPER` from the aeon, which asks `spira_config::build` for it.
The aeon now asks for **`Wrapper::admitted_env(spira-admit)`**. That sets
`RUSTC_WRAPPER=<abs path of spira-admit>`, sets `SPIRA_ADMIT_INNER=<abs path of sccache>`
(empty with `SPIRA_BUILD_CACHE=off`), sets `SPIRA_ADMIT_WHO=<bead>`, and keeps
`SCCACHE_IGNORE_SERVER_IO_ERROR=1`. cargo runs `spira-admit <rustc> <args…>` for every
compilation:

1. `SPIRA_ADMISSION` set: exec the inner compiler at once (inherited).
2. Arguments with no `--crate-name` (the `rustc -vV` and `--print` probes that `cargo
   metadata` makes too): exec at once. A probe is not a build, and `cargo metadata` must
   never queue behind builds.
3. Otherwise, take a compile lease for the **parent process** (the cargo), waiting visibly if
   the pool is busy. The lease's weight is `WEIGHT_RELEASE` when the invocation is optimised
   (`-C opt-level=` other than 0) or uses LTO, and 1 otherwise. Then exec `SPIRA_ADMIT_INNER <rustc> <args…>`, or `<rustc> <args…>`
   when the inner compiler is empty.

The wrapper adds no `CARGO_*` variable, so sccache's keys are unchanged
(DESIGN-build-cache.md §2.2), and it execs, so the compiler's exit status and output are the
compiler's own. The gate and testenv keep the **plain** `Wrapper::env()`, because each of them
takes its slot in-process.

`spira-admit` is absent from the aeon's PATH on an old release. The aeon then hands out the
plain wrapper and logs `aeon: spira-admit not on PATH — this session's builds are not
admitted`, the same way it already falls back when sccache is missing. A session is not a
build, and a scheduling tool never stops the world.

### 3.4 Summon jitter

Before it starts the model session, the aeon sleeps a uniform random `0..=SPIRA_SUMMON_JITTER`
seconds (spira.toml `summon_jitter`, default **20**, and `0` disables it). A sentinel pass that
summons a batch of aeons at the same moment therefore does not start them in lockstep. The
sleep checks the stop flag every second, so a stopped aeon exits at once. The jitter is
logged: `aeon: summon jitter 13s`.

### 3.5 `spira-admit`

```
spira-admit <rustc> <args…>                      # RUSTC_WRAPPER mode (§3.3)
spira-admit status [--json]                      # pools: size, held, waiting, holders
spira-admit run --pool compile|test [--who W] [--weight N] -- <cmd…>   # hold a lease
```

`status` probes gate slots with a non-blocking flock. It names a gate holder from
`slot.<n>.holder`, which the gate now writes beside the lock when it takes a slot. `run`
lets a human put any job in a pool's queue, and it is the building block of the Concierge's
quiet-box replay (§7).

## 4. Schema

* **Lease / waiter files:** §3.2. **Gate holder:** `$SPIRA_RUN/gate-admission/slot.<n>.holder`,
  in the same one-line format (`pid= start= who=<branch> since=`). It is advisory: the flock
  stays the lock.
* **Config** (typed spira.toml, `[spira]`; all optional). `compile_par` and `test_par` are u32,
  and unset derives the size from the box (§5). `summon_jitter` is a u64 of seconds, and unset
  means 20. Each is exported to the shell by `export --sh` as `SPIRA_COMPILE_PAR`,
  `SPIRA_TEST_PAR` and `SPIRA_SUMMON_JITTER`, and each is registered in `conf.sh`'s key lists.
* **Telemetry**: `$SPIRA_RUN/tsd/admission.jsonl`, family `admission`, one row per lease that
  ends. The envelope is `ts`, `host` and `family`, followed by `pool`, `who`, `slot`, `size`, `weight`,
  `waited_secs`, `held_secs` and `end` (`released` or `reclaimed`). From these rows come
  time-waiting and time-in-phase per aeon and per pool. Pool occupancy at any instant comes
  from `spira-admit status`. The gate's own wait was already in gate.log (`waited=`) and in
  the `gate-run` family.

## 5. Sizing — derived from measured phase costs

**Measured on this box, 2026-09-30.** The box has 32 cores and 60 GiB RAM, with 36 GiB
MemAvailable under the day's load. `/tmp` is a 31 GiB tmpfs that shares that RAM, and the
disk is one virtual SATA device. Each run went into its own `systemd-run --user --scope`, one
at a time. A 1 s sampler (`/proc` and the scope's `cpu.stat` / `memory.stat`) recorded cores,
anonymous memory, shmem (the tmpfs pages charged to the run), process counts, the containers
the run `podman exec`ed into, and host `sda` writes. **cgroup `io.stat` is not available on
this host.** The root cgroup delegates only `cpu memory pids`, and no `io` controller is enabled
anywhere, so per-run IO comes from `/proc/diskstats` (host-wide, and noisy while other agents
run) and from the PSI of the host.

| phase (one job) | wall | cores avg / peak | anon RAM peak | tmpfs | dolt | disk written |
|---|---|---|---|---|---|---|
| compile — agent `cargo build --profile aeon --workspace --all-targets`, deps from sccache, cold target | 38–43 s | 8.1–9.8 / 14 | 2.7 GiB (rustc peak 8) | 0 | 0 | 0.8–1.2 GiB (3.0 GiB target on disk) |
| compile — agent `cargo test --profile aeon --workspace` after that build | 71 s | 1.0 / 5 | 0.6 GiB | 6 MiB | 0 | host-noisy |
| compile — testenv in-place `cargo build --profile release --workspace` (LTO, cgu=1), cold target | 127–145 s | 7.1 / 33 | **15.3 GiB** (22 rustc alive; above 8 GiB for about 60 s) | 0 | 0 | ≈ 1.1 GiB (host) |
| test — testenv `up`→teardown, 8 suites at `batch_maxpar` 8 | 78 s (up 3, install 11, testdb 4, suites 55, teardown 5) | 1.2 / 8 | 0.44–0.55 GiB container (testenv: "cgroup peak 536MiB, ~67MiB/slot") | 35 MiB in place; a scratch slot's release target ≈ 3.2 GiB | 7 private sql-servers at peak | 6.3 MiB/s host-wide during the suites; host io full avg10 ≤ 3.8% |
| gate — a certification trial (gate.log, 2026-09-30, 30 trials) | 200–800 s (median ~400) | its own composition | a gate tree's target on tmpfs ≈ 0.7 GiB, plus a warm testenv slot ≈ 3.2 GiB | ≈ 3.9 GiB | ≤ 8 | — |

**The release profile changed under this measurement.** sp-zqo8s landed while this bead was
in flight. It moved `[profile.release]` from fat LTO with `codegen-units = 1` and `opt-level
= "z"` to `lto = false`, `codegen-units = 16` and `opt-level = 2`. The 15.3 GiB and 22-`rustc`
row above was measured on the old profile. `WEIGHT_RELEASE = 4` is therefore a starting
value, and it errs on the safe side: a lighter release build weighing 4 only runs alone when
it could have shared the pool. Re-derive it from the `admission` rows (`held_secs` per weight)
and a single release build's anon peak on the new profile, on a quiet box, and change it by
editing the constant. The detection (`-C opt-level=` other than 0) still marks the new profile
as release-like.

**What binds each phase.** Compile is bound by RAM first and cores second. An aeon-profile
build carries 2.7 GiB of anon and uses about 9 cores. A release (LTO) build carries **15.3
GiB**, with 22 `rustc` alive for a minute. The jam's "~20 rustc" and its memory full-stall
were testenv release builds that happened to coincide. Test is not bound by cores, RAM or IO
on its own: one trial measured 1.2 cores, 0.5 GiB and io full ≤ 3.8%. What a trial holds is
dolt fixtures (up to 8) and, when it does not run in place, a 3.2 GiB tmpfs scratch slot.
Gate is bound by tmpfs and wall time. A gate holds about 3.9 GiB of `/tmp` for as long as
800 s.

**The sizes.** Each derived size is at least 1, re-read on every pass of a wait, and
overridable by its key:

* `compile_par = min(cores ÷ 10, MemAvailable ÷ 4 GiB)` units. **3** on this box. A unit is
  one aeon-profile build, about 9 cores and at most 4 GiB. A release build weighs
  ⌈15.3 ÷ 4⌉ = **4** units (`WEIGHT_RELEASE`). That is more than this box's whole pool, so a
  release build runs alone. Three debug builds average about 27 of 32 cores and carry about
  8 GiB of anon. Their peaks (3 × 14 cores) oversubscribe the CPU only for seconds, which costs
  time-slicing but no stalls. One release build is 15.3 GiB. Either load is under half of
  MemAvailable. Unshaped, 14 agents could run three release builds at once, about 46 GiB, and
  that is the stall the operator saw.
* `test_par = min(cores ÷ 8, MemAvailable ÷ 8 GiB)`. **4** on this box. That is 32 private
  sql-servers at most: the jam saw 34 alongside the release builds, and the compile pool now
  keeps the two apart. The 8 GiB per slot covers a scratch slot's 3.2 GiB tmpfs target, the
  0.55 GiB container, and margin. Four trials' scratch (4 × 3.2 GiB) plus three gates' ≈ 12
  GiB stays under the 31 GiB `/tmp`.
* `certify_par`: unchanged. It is derived as `min(cores ÷ 4, MemAvailable ÷ 400 MiB)` and set
  to 3 in production. A gate trial carries about 3.9 GiB of tmpfs, so 3 gates hold about
  12 GiB of the 31 GiB `/tmp`.

**The statute, applied.** When a measurement shows a phase congesting, the fix is **that
phase's pool size**, and nothing else. A job never runs narrower: no CPU quota, no cargo
`jobs` cap, no `--test-threads` cap, no smaller `batch_maxpar`, no nice or ionice.
law-reduce-the-count-never-throttle-the-job.

**Why the pools do not stack their peaks.** Take a worst-case instant: the compile pool full
(one release build at 15.3 GiB, or three debug builds), 4 test trials and 3 gates. The anon
RAM is about 15 + 4 × 0.6 + 3 × 3, roughly 27 GiB, which is under the 36 GiB MemAvailable.
The tmpfs is about 12 GiB of gates plus at most 13 GiB of test scratch. The average cores are
about 27 + 4 × 1.2 + 3 × ~9, roughly 2× the box. That is CPU time-slicing, which delays jobs
but never stalls them. Unshaped, 14 agents could put 14 builds on the box at once, three of
them release builds.

## 6. Decisions

* **D1: per-phase pools, not one pool** (operator amendment, 2026-09-30). The bead was first
  written as "one host-wide admission". A single pool admits whatever phase arrives. Fourteen
  aeons that finish reading their briefs together then compile together, and the pool only
  decides which N of them do it. Separate pools let a compiling aeon and a testing aeon run
  side by side, so each phase's resource (cores, dolt/fsync, tmpfs) is shared by its own jobs
  only.
* **D2: the compile gate is `RUSTC_WRAPPER`, not a `cargo` shim.** The build wrapper is the
  seam every agent build already passes through (sp-z61hj). A `cargo` shim on PATH would
  shadow rustup's proxy for every tool on the launcher PATH, the gate included. The wrapper
  sees one `rustc` at a time, so the lease is keyed by the cargo process (the wrapper's parent)
  and lives as long as the cargo does: a pid+starttime lease, with no daemon and no fd to
  inherit. Rejected: flock per `rustc`, which is a host-wide jobserver, meaning per-job
  throttling under another name.
* **D3: the gate's `J = cores ÷ certify_par` is removed.** The gate divided the host's cores
  among its pool and passed that as `cargo -j` and `--test-threads` to its unit phases (it
  was 10 at `certify_par` 3). That is a per-job speed limit, and the statute forbids it. A
  gate that runs alone was paying for gates that did not exist. `compose::jobs` now returns
  the host's cores. How many gates run together is `certify_par`'s job.
* **D4: the gate keeps its flock pool and its bounded wait.** `landing-pass` probes
  `slot.N.lock` before it dispatches a gate (landing-pass/DESIGN.md), and the landing pass's
  run budget assumes `SPIRA_GATE_LOCK_WAIT`. Only the waiting line and the holder sidecar are
  new. The "never fails for waiting" rule applies to the aeon-side pools. A gate is started by
  the landing pass, never by an aeon (aeon teardown.rs: self-certifying blocks a session on
  gate admission).
* **D5: leases, not flocks, for compile and test.** A flock is held by an open fd, and a cargo
  has no fd of ours. A lease names a pid and dies with it. testenv could have held a flock,
  but one mechanism for both aeon-side pools gives `status` and the telemetry one reader.
* **D6: probes are not builds.** Without rule 2 of §3.3, `cargo metadata` would take a
  compile slot, and the gate's own `cargo metadata` composition step would queue behind agent
  builds.
* **D7: release builds are not admitted.** The landing path no longer compiles (sp-z61hj
  item 4). `release build` is run by hand or by `release.yml`. Admitting it would put the
  landing path behind agent builds for no measured gain.
* **D8: jitter at the aeon, not in lib.sh.** The summon loop is still bash
  (`ck7_summon_pass`), and new logic is Rust. The aeon is the first Rust code a summon reaches.
* **D10: weights, not a second compile pool.** A release build measured 5.7× a debug build's
  anon RAM. Counted as one slot, three release builds (46 GiB) would pass through a pool sized
  for debug builds. A separate release pool would split one resource, the box's RAM and cores
  for compiling, in two. A weight in units keeps one pool and one queue. FIFO keeps a heavy job
  from being starved by light jobs that would fit around it.
* **D9: dropped behaviour.** testenv's existing container-count queue
  (`SPIRA_TESTENV_MAX_CONCURRENT`, which gives up after `SPIRA_TESTENV_QUEUE_TIMEOUT`) stays
  as a backstop, and the test pool normally keeps it from ever engaging. Nothing else is
  dropped.
* **D11: a gate's builds take compile leases without waiting.** This is option 2 of
  sp-f4ig1-fix, chosen by the Concierge on 2026-10-01 after concierge/sp-yyk47 ended in
  `NO_VERDICT reason=budget`.
  **What happened.** That trial's `__batch__` row is `resolve:2,build:148`, against a setup
  share of 150 s. It shows no `admit-compile` phase and no `admission` row, so the trial never
  waited for admission. Its warm-copy build was slow because three admitted agent builds were
  compiling at full width beside it, and the share expired.
  **The rule.** While a gate's build runs, it holds weight in the compile pool, as a release
  build does. Agent builds therefore queue behind it instead of competing with it, and the
  gate never waits for a slot. That changes a count, which law-reduce-the-count-never-throttle-the-job
  allows. Raising the setup share was rejected: budgets are not raised to make things pass.
  **Mechanism.**
  * `spira_config::admission::take_now` writes the lease at once, whatever the pool holds.
    When the pool is full, the lease oversubscribes it. Agent leases must fit (`try_take`), so
    they wait until the gate's lease is released.
  * The gate's build environment fronts every cargo with `spira-admit`, carrying the token
    `SPIRA_ADMISSION=gate` and `SPIRA_ADMIT_WHO=gate:<branch>`. That covers tools, unit phases,
    the build fence's `make build` and `--release-bins`. Before this change, `--release-bins`
    ran outside the trial environment without the token.
  * The wrapper's one decision, `wrapper_action`, returns `TakeNow` for a gate token, `Wait`
    for no token and `Exec` for a probe or a job admitted elsewhere. A `TakeNow` lease is held
    by that cargo process and is reclaimed when the cargo exits.
  * testenv under a gate token takes its build's lease in-process with `take_now_guard`, holds
    it for the build only, and releases it before the test phase. The test phase takes no test
    slot, because it rides the gate's slot.
  * A lease's weight follows the build: an optimised or release build weighs
    `WEIGHT_RELEASE`, anything else weighs 1.
  * Every fresh take is logged: `gate build took compile slot N (weight W) without waiting —
    oversubscribed: H of S held; new agent builds queue behind it`. The lease ends in the
    `admission` telemetry like any other. cargo hides a dependency crate's stderr, so when the
    first compile of a build is a dependency, the line is not shown; the lease is still taken
    and recorded.
  * If `spira-admit` is not on the gate's PATH (an older release), the gate keeps the plain
    wrapper and the token. It never waits, but it holds nothing.
  **What it does not do.** Agent builds that are already running when the gate's lease is
  taken keep running at full width. The lease stops new agent builds from starting; it never
  pre-empts one.
* **Finding, not fixed here.** Under `lifecycle_enforce`, the model's restricted environment
  (`aeon/src/restrict.rs`) carries neither `RUSTC_WRAPPER` nor cargo on its PATH. An enforced
  session therefore gets neither the build cache nor admission. That is the restriction's
  scope, and it is reported to the Concierge.

## 7. Proof and acceptance

**Acceptance is correctness** (per the Concierge, 2026-09-30: a load replay on the one
production host is not worth its cost). Correctness is shown by deterministic unit tests of
the pools, all run against real pool directories through the real `try_take` / `acquire` /
`release`:

* admit, release and reclaim, pid reuse, lowering a size, FIFO, weights
  (`spira-config/src/admission/tests.rs`);
* **the pools as a model in virtual time**: 14 fake agents released at one instant walk
  compile → test with the phase costs of §5, and every fourth one is a release build. At every
  tick the test checks that no pool is over its size (a heavier-than-the-pool build only runs
  alone), that every agent finishes (no deadlock, no starvation), that the phases overlap
  across agents, and that no release build is overtaken;
* no hold-and-wait across phases: testenv releases its compile lease before it takes a test
  lease, a gate's children inherit the gate's slot, and an inherited job never waits
  (`testenv/src/run/tests.rs`, `gate/src/tests.rs`);
* visible waits: the waiting line and the admitted line, from testenv, the gate and the
  wrapper;
* jitter bounds, and the sizes read from typed config (`compile_par`, `test_par`,
  `summon_jitter`).

> **2026-10-01: `gate_occupancy_reads_flocks_and_their_holder_sidecars` deleted and
> re-added same round** (sp-os3of; `law-a-test-that-flips-is-deleted`). Flipped between the
> round-209 certification (green) and the full VM sweep (red) at the same commit
> (`16a5fa144`): `assertion failed: gate_occupancy(&d, 3).holders.is_empty()`. Cause: it held
> an flock on an in-process fd, dropped it, and asserted the slot free in the same step —
> but other `spira-config` lib tests (`containment.rs`, `repos.rs`, `resolve.rs`) fork real
> `git` children, and a fork duplicates the open file description; a concurrently-forked
> child held a copy of the lock past this test's own `drop()`, until that child execed.
> Re-added holding the lock from a `testkit::ChildGuard`-spawned `flock -x <file> sleep 60`
> child instead of an in-process fd, so this process never opens the lock file itself and no
> concurrent fork anywhere in the binary can inherit a copy of a lock it never held; the
> child is killed and reaped before the release assertion. Audited the rest of the tree for
> the same shape (an in-process flock, dropped, then asserted released/free) and converted
> `archivist/src/lock.rs::a_second_try_lock_is_refused_until_the_first_drops` and
> `watchd/src/lock.rs::the_lock_is_free_again_once_the_holder_is_dropped` the same way.
> Left alone, with reasons: `queue/src/lock.rs`'s two equivalents (already serialised against
> every fork in that crate's lib tests by `testutil::serial()`, which every forking test in
> that crate already takes); `gate/src/real.rs::run_gate_never_lets_a_daemon_it_starts_inherit_the_callers_lock_fd`
> (the in-process, non-`O_CLOEXEC` fd it opens IS what it tests — a daemon inheriting the
> caller's real lock fd — swapping it for a child-held lock would stop testing that);
> `testenv/src/worktree.rs`'s slot-reuse test (the in-process fd it drops is the real
> `Worktree` object under test, not a fabricated "another process holds it" setup; already
> carries a documented bounded retry for the same race).
>
> Seen red once, 2026-10-01 (pre-fix): looping `cargo test -p spira-config --lib` flipped
> this test at iteration 107 of 200: `thread
> 'admission::tests::gate_occupancy_reads_flocks_and_their_holder_sidecars' panicked at
> spira-config/src/admission/tests.rs:268:5: assertion failed:
> gate_occupancy(&d, 3).holders.is_empty()`.

**The full 14-agent acceptance replay runs only on a quiet box.** The bead's load criteria are
memory full-stall under 5%, io full avg60 under 30% sustained, and more landings per hour than
unshaped at the same N. The Concierge schedules that replay after the rewrite waves land. The
unshaped baseline is 2026-09-30's real jam (§1), and the shaped result is measured in
production after rollout, from the `admission` telemetry family and host PSI. No load test runs
on the live box.
