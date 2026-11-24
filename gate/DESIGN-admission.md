# Host-wide admission — pipeline shaping (sp-f4ig1)

The gate has owned host-wide admission since sp-4vq2q: `SPIRA_CERTIFY_PAR` flock slots under
`$SPIRA_RUN/gate-admission`. This document extends that one pool into several, one per heavy
phase of a dwarf's loop, and says how each is sized. The shared code is
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

* *"what you want to avoid is a thundering herd: N dwarves doing compiles at the same time,
  then tests at the same time, then gates at the same time. throughput is higher if you can
  have dwarves running in each phase."* The answer is **pipeline shaping**: a separate
  host-wide pool per heavy phase, so at any moment some dwarves compile, some test, some gate,
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

### 3.1 The pools

| pool | what takes a slot | holder | size key (env / spira.toml) | directory |
|---|---|---|---|---|
| **test** | an agent's own `testenv` trial, from container `up` to teardown | the testenv process | `SPIRA_TEST_PAR` / `test_par` | `$SPIRA_RUN/test-admission` |
| **gate** | a gate trial that runs suites or unit phases (unchanged) | the gate process (flock) | `SPIRA_CERTIFY_PAR` / `certify_par` | `$SPIRA_RUN/gate-admission` |

* **A slot is admission, never a limit.** An admitted job runs at the default width: cargo's
  own `-j` (all cores), testenv's configured `batch_maxpar`, and the gate's unit phases at the
  host's cores (§6 D3).
* **Inherited admission.** A process whose environment carries `SPIRA_ADMISSION=<pool>:<slot>`
  runs inside a job that is already admitted. It takes no slot of its own. The gate sets this on
  every command it runs, so its testenv runs on the gate's slot. Inheritance also follows the
  process tree: a process whose ancestor already holds a lease takes none.
* **Waiting never fails.** A test waiter waits for as long as it takes. It prints, to
  its own stderr, once when it starts waiting and then every 30 s:
  `waiting for a test slot: 2 of 2 held by sp-abc (testenv pid 123, 41s), sp-def (testenv pid 456, 12s)`
  and, once it is admitted, `admitted to test slot 1 after 37s`. Under `--deadline`, testenv
  moves its deadline later by the time it spent waiting. The budget meters work, not queueing.
  The gate's bounded wait is unchanged (§6 D4). It now prints the same kind of waiting line.
* **Sizes are in units, and the queue is first come, first served.** Every job has a weight,
  which every pool now sets to 1. A waiter is admitted when
  two things hold: no older live waiter is queued ahead of it, and its weight fits in what the
  live leases leave of the size. A waiter is also admitted when the pool is empty, so a job
  heavier than the whole pool runs alone instead of never running. A waiter's queue position is
  its first `since`, which is written once.
* **Sizes are re-read on every pass of a wait**, so a size an operator raises takes effect at
  once. Lowering a size never evicts a holder: the pool drains to its new size as holders
  finish.

### 3.2 Leases (test)

A lease is a file `slot.<n>` in the pool's directory. Its content is one line:
`pid=<p> start=<starttime> who=<label> since=<epoch> waited=<secs> last=<epoch> weight=<units>`
(a line without `weight=` reads as 1). Every
read-modify-write of the directory happens under an exclusive `flock` on `<dir>/lock`, which is
held only for the scan, never across a wait or a job.

* **Live** means `/proc/<pid>/stat` exists and its field 22 (starttime) equals `start`. A
  dead or recycled pid frees the slot. The next scan **reclaims** it and writes the lease's
  telemetry row with `end=reclaimed`. Nothing depends on a holder exiting cleanly.
* **Take**: under the lock, reap the dead first. If a live lease already names this holder,
  it is ours (`last` is refreshed). Otherwise, if a live lease names an ancestor of the holder,
  admission is inherited. Otherwise, if this holder is at the head of the queue and its weight
  fits (§3.1), a lease is written at the lowest free slot number. Otherwise the pool is busy:
  the waiter file is written (once), and the scan returns the live holders for the waiting
  line.
* **Release**: remove `slot.<n>` if it still names this holder, and write the telemetry row
  with `end=released`. testenv releases through a guard's `Drop`.   `spira-admit status` scans too. The row's `held_secs` is `last − since`, so the row stays honest when the reclaim happens late.
* **Waiters** leave a `wait.<pid>` file (`start= who= since= weight=`) while they wait. It is removed
  once they are admitted, and reclaimed if they die. `spira-admit status` counts these files.

### 3.4 Call jitter

Before it starts the model session, the dwarf sleeps a uniform random `0..=SPIRA_CALL_JITTER`
seconds (spira.toml `call_jitter`, default **20**, and `0` disables it). A sentinel pass that
calls a batch of dwarves at the same moment therefore does not start them in lockstep. The
sleep checks the stop flag every second, so a stopped dwarf exits at once. The jitter is
logged: `dwarf: call jitter 13s`.

### 3.4a The cold-build slot (RETIRED, sp-ke0uk8)

**Retired.** No dwarf acquires this pool any more: each building session leases one owned build
directory for its whole bead (spira-config `build_dir`), never waits for it, and takes its
dependencies from sccache. Its size never reached the dwarf anyway — read only from the dwarf's own
environment, which the unit does not carry, so every lease said size 1 whatever `cold_par` held.
`SPIRA_COLD_PAR` stays declared (deprecated) until a later release removes it. What follows is the
record of what it did.

A fourth pool, `cold` (`$SPIRA_RUN/cold-admission`, size 1, `SPIRA_COLD_PAR` overrides), admits one
cold build host-wide. Before its model session starts, the dwarf asks `admission::cold_reason`:
the title names a toolchain change, the branch diff touches `rust-toolchain(.toml)`, `flake.lock`
or the top-level `Cargo.lock`, or its build target has no `.rustc_info.json`. A cold session
takes a cold lease and logs `waiting on cold-build slot (<why>): …` until the holder releases;
a warm session never touches the pool. The lease is held for the session or `COLD_HOLD_SECS`
(1200), whichever is shorter. The watchtower raises `cold-overlap` when two or more leases are
held while host io `full avg60` is at the pressure threshold.

### 3.5 `spira-admit`

```
spira-admit status [--json]                      # pools: size, held, waiting, holders
```

`status` probes gate slots with a non-blocking flock. It names a gate holder from
`slot.<n>.holder`, which the gate now writes beside the lock when it takes a slot.

## 4. Schema

* **Lease / waiter files:** §3.2. **Gate holder:** `$SPIRA_RUN/gate-admission/slot.<n>.holder`,
  in the same one-line format (`pid= start= who=<branch> since=`). It is advisory: the flock
  stays the lock.
* **Config** (typed spira.toml, `[spira]`; all optional). `test_par` is a u32,
  and unset derives the size from the box (§5). `call_jitter` is a u64 of seconds, and unset
  means 20. Each is exported to the shell by `export --sh` as
  `SPIRA_TEST_PAR` and `SPIRA_CALL_JITTER`, and each is registered in `conf.sh`'s key lists.
* **Telemetry**: `$SPIRA_RUN/tsd/admission.jsonl`, family `admission`, one row per lease that
  ends. The envelope is `ts`, `host` and `family`, followed by `pool`, `who`, `slot`, `size`, `weight`,
  `waited_secs`, `held_secs` and `end` (`released` or `reclaimed`). From these rows come
  time-waiting and time-in-phase per dwarf and per pool. Pool occupancy at any instant comes
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
| test — testenv `up`→teardown, 8 suites at `batch_maxpar` 8 | 78 s (up 3, install 11, testdb 4, suites 55, teardown 5) | 1.2 / 8 | 0.44–0.55 GiB container (testenv: "cgroup peak 536MiB, ~67MiB/slot") | 35 MiB in place; a scratch slot's release target ≈ 3.2 GiB | 7 private sql-servers at peak | 6.3 MiB/s host-wide during the suites; host io full avg10 ≤ 3.8% |
| gate — a certification trial (gate.log, 2026-09-30, 30 trials) | 200–800 s (median ~400) | its own composition | a gate tree's target on tmpfs ≈ 0.7 GiB, plus a warm testenv slot ≈ 3.2 GiB | ≈ 3.9 GiB | ≤ 8 | — |

**What binds each phase.** Test is not bound by cores, RAM or IO
on its own: one trial measured 1.2 cores, 0.5 GiB and io full ≤ 3.8%. What a trial holds is
dolt fixtures (up to 8) and, when it does not run in place, a 3.2 GiB tmpfs scratch slot.
Gate is bound by tmpfs and wall time. A gate holds about 3.9 GiB of `/tmp` for as long as
800 s.

**The sizes.** Each derived size is at least 1, re-read on every pass of a wait, and
overridable by its key:

* `test_par = min(cores ÷ 8, MemAvailable ÷ 8 GiB)`. **4** on this box. That is 32 private
  sql-servers at most: the jam saw 34 alongside the release builds. The 8 GiB per slot covers a scratch slot's 3.2 GiB tmpfs target, the
  0.55 GiB container, and margin. Four trials' scratch (4 × 3.2 GiB) plus three gates' ≈ 12
  GiB stays under the 31 GiB `/tmp`.
* `certify_par`: unchanged. It is derived as `min(cores ÷ 4, MemAvailable ÷ 400 MiB)` and set
  to 3 in production. A gate trial carries about 3.9 GiB of tmpfs, so 3 gates hold about
  12 GiB of the 31 GiB `/tmp`.

**The statute, applied.** When a measurement shows a phase congesting, the fix is **that
phase's pool size**, and nothing else. A job never runs narrower: no CPU quota, no cargo
`jobs` cap, no `--test-threads` cap, no smaller `batch_maxpar`, no nice or ionice.
law-reduce-the-count-never-throttle-the-job.

**What a builder session declares.** `admission::RUN_CARGO_JOBS` and `RUN_TEST_THREADS` are the
width one run is *sized* at: the first is the divisor that turns the host's cores into how many
runs fit, so a session's `CARGO_BUILD_JOBS` and `RUST_TEST_THREADS`
(`spira_config::build::session_env`, set by dwarf for a rune that builds) state that sizing; they
are not a cap added to a job. A host cargo config carries none of it; `doctor` warns when one does.
Builds outside a session (gate, `queue`, `release`) set no jobs and use the host's cores.

## 6. Decisions

* **D1: per-phase pools, not one pool** (operator amendment, 2026-09-30). The bead was first
  written as "one host-wide admission". A single pool admits whatever phase arrives. Fourteen
  dwarves that finish reading their briefs together then compile together, and the pool only
  decides which N of them do it. Separate pools let a compiling dwarf and a testing dwarf run
  side by side, so each phase's resource (cores, dolt/fsync, tmpfs) is shared by its own jobs
  only.
* **D3: the gate's `J = cores ÷ certify_par` is removed.** The gate divided the host's cores
  among its pool and passed that as `cargo -j` and `--test-threads` to its unit phases (it
  was 10 at `certify_par` 3). That is a per-job speed limit, and the statute forbids it. A
  gate that runs alone was paying for gates that did not exist. `compose::jobs` now returns
  the host's cores. How many gates run together is `certify_par`'s job.
* **D4: the gate keeps its flock pool and its bounded wait.** `landing-pass` probes
  `slot.N.lock` before it dispatches a gate (landing-pass/DESIGN.md), and the landing pass's
  run budget assumes `SPIRA_GATE_LOCK_WAIT`. Only the waiting line and the holder sidecar are
  new. The "never fails for waiting" rule applies to the dwarf-side pools. A gate is started by
  the landing pass, never by a dwarf (dwarf teardown.rs: self-certifying blocks a session on
  gate admission).
* **D8: jitter at the dwarf, not in lib.sh.** The call loop is still bash
  (`ck7_call_pass`), and new logic is Rust. The dwarf is the first Rust code a call reaches.
* **D5: leases, not flocks, for the test pool.** A lease names a pid and dies with it, with no
  fd to inherit; one mechanism gives `status` and the telemetry one reader.
* **D9: dropped behaviour.** testenv's existing container-count queue
  (`SPIRA_TESTENV_MAX_CONCURRENT`, which gives up after `SPIRA_TESTENV_QUEUE_TIMEOUT`) stays
  as a backstop, and the test pool normally keeps it from ever engaging. Nothing else is
  dropped.
* **Compile admission deleted 2026-11-14 per Ryan (sp-i7n3j4):** builds go straight to the
  compiler; the builder ceiling and certify_par bound them.

## 7. Proof and acceptance

**Acceptance is correctness** (per the Concierge, 2026-09-30: a load replay on the one
production host is not worth its cost). Correctness is shown by deterministic unit tests of
the pools, all run against real pool directories through the real `try_take` / `acquire` /
`release`:

* admit, release and reclaim, pid reuse, lowering a size, FIFO, weights
  (`spira-config/src/admission/tests.rs`);
* no hold-and-wait across phases: a gate's children inherit the gate's slot, and an inherited
  job never waits
  (`testenv/src/run/tests.rs`, `gate/src/tests.rs`);
* visible waits: the waiting line and the admitted line, from testenv and the gate;
* jitter bounds, and the sizes read from typed config (`test_par`, `call_jitter`).

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
