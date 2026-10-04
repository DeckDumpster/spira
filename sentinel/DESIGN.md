# sentinel — the loop's heartbeat

Replaces `spira/sentinel.sh` (1,596 lines). The binary is `sentinel` and takes the same four
entry points. This document is the contract. The code satisfies it, and the unit tests
(`cargo test -p sentinel`) are derived from it.

## 1. Intent

Every pass compares the work graph to its desired state and closes the gap with the one
deterministic action each gap names. There is no goal bead: Spira works the whole backlog
continuously (the operator's answer to sp-2f9sa, sp-k6m1m), so "the work" is every open plan bead, never
one epic's children, and no pass ever declares it finished. The shape is the operator's: "look at the current state,
the goal state, reflect on the gap … cheap, quick, and frequent to run with deterministic
heuristics; drop down to inference when judgement is required."

The pass has four jobs, in order of how much stalls when each is late:

1. **Keep the fleet full.** When there is ready work and a free slot, summon an aeon
   (CHECK 7). This runs every 15 s on its own path (`--summon-only`) and again inside every
   full pass.
2. **Keep work moving.** Reap dead holders (CHECK 2, 2b, 2c), fix stale blocked flags
   (CHECK 3), gate queue-mode dependents (CHECK 3b/3c), and dispatch the landing worker
   (CHECK 6).
3. **Stop waste and surface lies.** These walks are decoupled into the audit worker:
   - poison a bead that keeps failing, and ask about beads that keep being requeued or
     keep losing their holder (CHECK 4);
   - report beads that are closed but not landed (CHECK 5);
   - reap landed branches (CHECK 6b);
   - name ready beads that no persona can claim (CHECK 7c);
   - un-jam branch collisions (CHECK 7d).
4. **Know when rules have run out.** If the plan is starved and nothing moved, ask for
   inference, rate-limited to once an hour (CHECK 8). Inference is a cost centre. Adding a
   deterministic check is how the system learns.

Two counters answer two different questions:

- **`acted`** means a write happened.
- **`progressed`** means the DAG moved: a bead changed status or a branch landed.

CHECK 8 gates on `progressed` and never on `acted`. A futile write must not mute the one check
that notices paralysis. Measured before this rule existed: 76 passes, and judgement fired zero
times.

### What it is deliberately NOT

- **Not the summoner.** Lanes, the pool, the fleet ceiling, the express grant and the
  elastic reservations are handled by `ck7_summon_pass`, `summon_fayth` and friends in
  lib.sh. That code is shared with `aeon --escape` and the aeon units, so it is reached through
  a seam (§6).
- **Not the poison arithmetic.** Attempt, requeue and reclaim counts, and the CHECK 4
  decision itself, belong to `spira-claim` (`counts`, `decide`, `attempts`).
- **Not the stranded-work detector.** That is the `strand` crate (`strand check`).
- **Not a config reader.** The binary reads no `spira.toml` key itself. conf.sh resolves
  configuration through `spira-config` (law-config-through-the-cli-only). The binary receives
  the result from the context probe (§6, S0).

### The lifecycle switch — one key, two record sets

The operator's decision (2026-09-29): `lifecycle_enforce` is **the one switch** for
everything that touches the lifecycle machine. §2.9 is the per-CHECK contract.

The machine was never deployed on this host. There is no `spira_lifecycle` database, no
`spira_lc` grant (every spira-lc call answers "Access denied for user 'spira_lc'"), and no
spira-lc user, service or socket. So every path sp-ki12s, sp-mys5p and sp-i2m7y moved onto
spira-lc unconditionally has been "cannot tell" in production. The poison hold, the wait
hold, the stale-lease reap and the consistency sweep all silently did nothing.

The two modes:

- **OFF** is production today.
  - The sentinel never calls spira-lc.
  - Every CHECK runs on the records the harness kept before the lifecycle epic: bd
    status, labels, assignee and lease, the landstate files, and the bd events trail read
    by spira-claim.
  - The behaviour is recovered from git history as intent (sentinel.sh and lib.sh at
    `f3391c8ec^`, `f043dee14^` and `542b9445f^`), not as a line port.
  - Nothing the sentinel starts is handed a live path to spira-lc (§2.9, "children").
- **ON:** `lifecycle.rs` is authoritative.
  - A machine that cannot answer is a loud error: one `LIFECYCLE UNREACHABLE — …` line per
    pass, no lifecycle decision, and exit 1 after the rest of the pass has run.
  - It is never a silent skip, and there is no fallback to labels.

## 2. Contract

### 2.1 Entry points

```
sentinel                 one full pass                                  (spira-sentinel.service)
sentinel --report        STATE only: print the open plan beads, change nothing
sentinel --summon-only   CHECK 7 alone                                  (spira-summon.service;
                         every aeon unit's ExecStopPost via lib.sh summon_refill_argv)
sentinel --audit         the decoupled audit worker (CHECK 4/5/6b/7c/7d), started by the
                         full pass as the transient unit `spira-audit`
sentinel --open-children [--dry-run]
                         CHECK 3c alone over a fresh snapshot (the suites' entry point).
                         --dry-run prints `would add|remove <label> to|from <id>` and
                         writes nothing: a read-only probe safe against production
```

Only the **first** argument selects the mode, exactly as the old `[ "${1:-}" = … ]` did. Any
other first argument, or none, means a full pass. The one second argument read is
`--dry-run` after `--open-children`.

| exit | when |
|---|---|
| 0 | the pass ran to its end. That includes every `--summon-only` pass whether it summoned or declined, `--report`, and `--audit` |
| 1 | `DATABASE UNREADABLE` (the bulk store read failed), skipped under `SPIRA_SKIP_RECLAIM=1`. Also any mode when the harness itself cannot be found: the context probe failed, or lib.sh was not found (new, `FATAL` line). Also, with `lifecycle_enforce` ON, a full pass or audit run in which the lifecycle machine could not be read or written (`LIFECYCLE UNREACHABLE`); that pass still runs every other check first |

Nothing is printed on stdout except log lines, the verbatim output of the scripts it calls,
and `--report`'s listing. The units append stdout and stderr to `$SPIRA_RUN/sentinel.log`,
and the audit worker appends them to `$SPIRA_RUN/audit.log`.

### 2.2 Log lines other programs parse (byte-identical)

The format is `<YYYY-MM-DDTHH:MM:SSZ> spira: <msg>` (lib.sh `log`), in UTC.

| line | parsed by |
|---|---|
| `state: open=<n> plan_ready=<n> in_progress=<n> aeons=<n> fayths=[<f …>]` | strand (`: state: open=` marks a pass start, `probe::PASS_MARKER`), auron-classify.py:49, cockpit-metrics.py:64,75, chamber/czar.md |
| `CHECK7 <fayth>: not evaluated (pass budget exhausted)` (the seam emits it) | strand probe.rs:363, strand.sh:369 |
| `ACT <msg>` for every act/progress (`summoned a <f> aeon`, `invoked reflection`, `poisoned <id> after <n> attempts`, `reclaimed <n> stale lease(s)`, …) | cockpit-metrics.py:65,69, cockpit.sh:626 |
| `pass complete — <a> action(s), <p> progress` | auron-classify.py:110 |
| `summon-only pass complete — <a> action(s)`, `audit pass complete — <a> action(s), <p> progress` | nothing outside tests |
| untimestamped `RECLAIMED`, `SENT`, `HELD`, `KEEP`, `FAILED` lines passed through from strand and sending | cockpit.sh:637, cockpit-metrics.py:236 |

Every other message is kept word for word as well, because the bash suites and the operator
grep them. The exact strings are the ones §4 quotes; `src/tests.rs` pins the parsed ones
(`state:`, `ACT`, `pass complete`, the CHECK 4/5/6 lines).

### 2.3 Files

| path (under `$SPIRA_RUN`) | r/w | shape | who else |
|---|---|---|---|
| `list-snapshot.XXXXXX`, `ready-snapshot.XXXXXX`, `ready-cache.XXXXXX` | w, removed at exit | bd JSON; the cache is `<fayth> <count>` lines | exported as `SPIRA_LIST_SNAPSHOT`, `SPIRA_READY_SNAPSHOT`, `SPIRA_READY_CACHE` to every child (strand, the seams) |
| `audit.status` | w (audit), r (pass) | `SP_AUDIT_AT=<epoch>\nSP_AUDIT_RC=0\n` | tests only |
| `audit.progress`, `audit.progress.drain.<pid>` | append (audit), drain-by-rename (pass) | one progress message per line | — |
| `audit.dispatched`, `landing.dispatched` | w once, never overwritten | `<epoch>\n` | cockpit.sh reads landing.* |
| `landing.status` | r | `SP_LAND_AT= SP_LAND_RC= SP_LAND_BRANCHES= SP_LAND_MOVED=` lines | written by `landing-pass land` |
| `landing.progress`, `landing.progress.drain.<pid>` | drain-by-rename | one line per movement | written by `landing-pass land`/stage.sh |
| `inference.cooldown` | r/w | `<epoch>` | — |
| `sending.base` | r/w (audit) | `<repo>=<sha>` lines | — |
| `poison-asked/<id>`, `requeue-asked/<id>`, `reclaim-asked/<id>` | r, append | one count per line | lib.sh (`spira-claim unpoison` clears `poison-asked`) |
| `poison-lifted/<id>` | r | last line = count lifted at | written by `spira-claim deadlocked` (via `groomer deadlocked`, spira-claim/DESIGN.md §9) |
| `landstate/<id>` | r | `<STATE> <tip> …` | the landing pass |
| `<id>.log` | existence (CHECK 5), trace tail (ask) | aeon session log | aeon.sh |
| `roster-warn` stamp (`$SPIRA_ROSTER_WARN_STAMP`) | r/w | sorted excluded fayths | lib.sh |
| `reflect.log` | append (reflect.sh output) | — | — |
| `summon.lock` | flock (inside the CHECK 7 seam) | — | `aeon --escape` |
| `.sentinel-tally.<pid>` | w/r/removed around each seam | `act\|progress<TAB>msg` lines (§6) | — |

The directory paths come from the probe's variables (§6, S0): `SPIRA_POISON_ASKED`,
`SPIRA_REQUEUE_ASKED`, `SPIRA_RECLAIM_ASKED`, `SPIRA_POISON_LIFTED` and
`SPIRA_ROSTER_WARN_STAMP`. Each defaults to the lib.sh default.

### 2.4 Store rows read and written

**Reads, once per pass.** This is the performance contract (§5).

| read | how |
|---|---|
| the whole bead store | `bd -C $SPIRA_DB list --all --limit 0 --json` |
| the broad ready set | `bd -C $SPIRA_DB ready --limit 0 --exclude-type epic,event -u [--exclude-label $SPIRA_NO_LOOP_LABEL] --json` (lib.sh `ready_raw_args`) |
| every lifecycle row (**ON only**) | `spira-lc list` → `[{bead_id,state,holder,lease_until,holds,version}]` |

**Extra reads, only on the rare paths:**

- CHECK 3's recount after `bd recompute-blocked` (one `bd ready`);
- one `bd show <id>` before each poison or ask;
- one `spira-lc show <id>` before each lifecycle event (the CAS needs the current version);
- one `spira-claim counts` per CHECK 4 set;
- `bd` via strand and the seams, as those components do today.

**Writes:**

| check | writes |
|---|---|
| CHECK 2 | spira-lc Hold/Unhold `wait`, HolderDead; an events row `reclaimed stale-lease` (bd sql); `bd note` |
| CHECK 3 | `bd recompute-blocked` |
| CHECK 4 | spira-lc Hold/Unhold `poison`; `bd note`; `events.log` (spira_event seam); mail |
| seams | whatever lib.sh does in CHECK 3b, 3c, 7, 7c and 7d, unchanged |

### 2.5 Processes it runs

**Systemd units:**

- `spira-sentinel[-<inst>].timer` → `.service` runs `sentinel` every 2 min. Type=oneshot,
  TimeoutStartSec=900. CHECK 5 and its skip switch are gone.
- `spira-summon[-<inst>].timer` → `.service` runs `sentinel --summon-only` every 15 s.
  TimeoutStartSec=60.
- Every aeon unit's `ExecStopPost=<systemd-run> --user --collect --quiet <sentinel>
  --summon-only`. This is lib.sh `summon_refill_argv`, and it runs **with no `--setenv`**,
  so the binary must find its harness from its own path (§2.7).

**Transient units it starts.** In each case the unit name is the mutex and `--collect` is
mandatory.

**No explicit CPU quota and no niceness** — on these transient units, on the sentinel's own
units, or anywhere else (sp-b4oct, law-isolate-greedy-work-in-vms, operator 2026-09-29): the OS
time-shares the host, and greedy work is isolated in a VM rather than throttled in place. The
former `SPIRA_AUDIT_CPU_QUOTA` / `SPIRA_LAND_CPU_QUOTA` knobs are gone; setting either in the
environment has no effect.

`spira-audit` runs `<this exe> --audit`. It is launched through
`${SPIRA_LAUNCH:-systemd-run} --user --collect --quiet` with:

- `--unit=${SPIRA_AUDIT_UNIT:-spira-audit}`
- `--property=RuntimeMaxSec=${SPIRA_AUDIT_MAXSEC:-1800}`
- stdout and stderr as `append:$SPIRA_RUN/audit.log`
- `--setenv` for `PATH HOME SPIRA_HOME SPIRA_RUN SPIRA_DB SPIRA_REPO SPIRA_REPO_MAP
  SPIRA_HOME_REPO SPIRA_BD SPIRA_GH SPIRA_POISON_AT SPIRA_REQUEUE_AT SPIRA_RECLAIM_AT
  SPIRA_ASK_LABEL SPIRA_SCOPE_LABEL SPIRA_WORK_CLOSE_TYPES`, in that order, plus **new**
  `SPIRA_SKIP_RECLAIM` when it is set (§9, B1).

`spira-landing` runs `landing-pass land` (the PATH-resolved program: a transient unit has no launcher PATH). It is launched the same way with:

- `--unit=${SPIRA_LAND_UNIT:-spira-landing}`
- `RuntimeMaxSec=${SPIRA_LAND_MAXSEC:-3600}`
- stdout and stderr to `landing.log`
- `--setenv` for `PATH HOME SPIRA_HOME SPIRA_RUN SPIRA_DB SPIRA_REPO SPIRA_REPO_MAP
  SPIRA_HOME_REPO SPIRA_BD SPIRA_GH SPIRA_BATCH_MAXPAR SPIRA_LAND_MAXSEC`

**Liveness query:** `${SPIRA_SYSTEMCTL:-systemctl} --user is-active <unit>.service`
(`active` means in flight).

**Aeon count:** `${SPIRA_SYSTEMCTL:-systemctl} --user list-units 'spira-aeon-*' --no-legend`
when `SPIRA_SUMMON` is `systemd-run` (the default). A unit counts for fayth `f` iff its name
starts `spira-aeon-<f>-`, which is the same match as the old per-fayth glob, in one call
instead of one per fayth. Otherwise the fallback is live pidfiles
`$SPIRA_RUN/aeon-<f>-*.pid`: the pid exists and `/proc/<pid>/cmdline` contains `aeon.sh`.
Dead ones are deleted, as `aeon_count` does.

**Scripts and binaries it calls** (interfaces unchanged):

| step | command | reads back |
|---|---|---|
| CHECK 1 | `$SPIRA_HOME/pilgrimage.sh check` | output passed through; `^PILGRIMAGE COMPLETE` counted → progress |
| CHECK 2b | `strand check` | output passed through; `^RECLAIMED` → progress, `^STRANDED` → act |
| CHECK 4 | `spira-claim counts` (ids on stdin), `decide --poison-at P --requeue-at R --reclaim-at C -- n rq rc labels stamp [poisoned]`; `mail send operator --from … --subject … --kind question --default …` (body on stdin) | `id\tatt\treq\trcl`; tokens; rc |
| CHECK 6 | `watchtower --throttle-check`, `--czar-outcome-check`, `--pr-stall-check`, `--disabled-timer-check` (bare name on the release PATH, each 2>/dev/null; sp-lnmbq) | ignored |
| CHECK 6b | `sending --skip-queue` (sending/DESIGN.md; sending.sh until sp-arpjt) | output passed through; `^SENT <id> <repo> <branch>` → act; `^FAILED` → log |
| CHECK 8 | `$SPIRA_HOME/reflect.sh "<open children, newline-separated>"` >> `reflect.log` | — |
| tsd | `tsd-write --family sentinel-phase --root $SPIRA_RUN --field-str pass=<id> --field-str check=<name> --field secs=<n>` | best-effort |

Every Spira tool is invoked by its bare name on the PATH the launcher set (sp-gypjk; design
runtime-is-a-release). There is no resolver, no `SPIRA_*_BIN` override and no "not
installed" state: a tool missing from PATH fails its spawn (rc 127), naming itself. The
names live as plain `Cfg` fields so a unit test can point one at a fixture.

### 2.6 Configuration (environment, as resolved by conf.sh through the probe)

| key | default | used by |
|---|---|---|
| `SPIRA_DB`, `SPIRA_RUN`, `SPIRA_HOME` | conf.sh | everything |
| `SPIRA_BD` | `bd` | store |
| `BD_TIMEOUT` | 180 | every bd call (lib.sh `bdq`) |
| `SPIRA_BDQ_CONN_RETRIES` | 2 | retry on `invalid connection` |
| `SPIRA_BDJSON_FIXTURE` | — | test seam: `python3 $SPIRA_HOME/bdsim.py <fixture> …` |
| `SPIRA_SCOPE_LABEL`, `SPIRA_ASK_LABEL`, `SPIRA_NO_LOOP_LABEL`, `SPIRA_INCIDENT_LABEL` | conf.sh; `incident` | predicates |
| `SPIRA_QUEUE_WAIT_LABEL`, `SPIRA_SUBMITTED_LABEL` | conf.sh | the ready cache |
| `SPIRA_WORK_CLOSE_TYPES` | `task bug feature` | CHECK 5 |
| `SPIRA_POISON_AT` | 3 | CHECK 4 |
| `SPIRA_REQUEUE_AT` | 5 | CHECK 4 |
| `SPIRA_RECLAIM_AT` | 5 | CHECK 4 |
| `SPIRA_RECLAIM_GRACE_SECS` | 10800 | CHECK 2 |
| `SPIRA_INFERENCE_EVERY` | 3600 | CHECK 8 |
| `SPIRA_AUDIT_UNIT` | `spira-audit` | audit |
| `SPIRA_AUDIT_MAXSEC` | 1800 | audit |
| `SPIRA_AUDIT_STALE` | 1800 | audit |
| `SPIRA_AUDIT_MAILBOX` | `$SPIRA_RUN/audit.progress` | audit |
| `SPIRA_LAND_UNIT` | `spira-landing` | CHECK 6 |
| `SPIRA_LAND_MAXSEC` | 3600 | CHECK 6 |
| `SPIRA_LAND_STALE` | 1800 | CHECK 6 |
| `SPIRA_LAUNCH`, `SPIRA_SYSTEMCTL`, `SPIRA_SUMMON` | `systemd-run`, `systemctl`, `systemd-run` | test seams |
| `SPIRA_SKIP_RECLAIM` | 0 | fixture fast path (skips the DB check, STATE, CHECK 2/2c/3/7c/7d) |
| `SPIRA_LIFECYCLE_ENFORCE` / `spira.lifecycle_enforce` | off | **the lifecycle switch** (§2.9). The unit's own environment wins (`1`/`true` = on, anything else = off). It is read from this process's original environment, not conf.sh's, which defaults it to 0. Else `spira.lifecycle_enforce` in the spira.toml conf.sh resolved (`SPIRA_TOML_FILE`), read with the spira-config library. Else off. This is the same resolution as the aeon crate (concierge/rw-aeon `aeon/src/conf.rs`). Binary presence is never consulted |
| `SPIRA_RECLAIM_SKIP_LABEL` | `spira-waiting-operator` | OFF's CHECK 2 protection label. The key was retired by sp-i2m7y; this is its last default, kept as a literal |
| `SPIRA_FAYTHS` | the chamber | the roster (lib.sh `spira_fayths`, via the probe) |
| `SPIRA_SENTINEL_PASS_TARGET_SECS` | 60 | **new**: the full-pass budget WARN (§5) |
| `SPIRA_SENTINEL_PASS_BUDGET_SECS` | 90 | CHECK 7's own budget, inside the seam |

### 2.9 The lifecycle switch, per CHECK

What each CHECK reads and writes in each mode. OFF is the pre-lifecycle behaviour; the
source commits it was recovered from are named.

| CHECK | OFF (production today) | ON |
|---|---|---|
| **2 (protect waiting)** | Candidates are snapshot beads that are `in_progress` and either carry `$SPIRA_RECLAIM_SKIP_LABEL` or have dependencies. Dependency facts come from the snapshot join. Protect with `bd label add <id> spira-waiting-operator` (log `… — labeled <skip>, excluded from reclaim`). Release with `bd label remove` (log `… — removed <skip>, re-enters the reaper`). Source: `f043dee14^` lib.sh `check2_protect_waiting` | The same decision over the lifecycle `wait` hold. Hold and unhold go through `spira-lc event` (log `… — wait-held …` / `… — wait released …`) |
| **2 (reap)** | One `bd reclaim --older-than <grace/60>m --label <partition> --exclude-label <skip>` per partition. `bd reclaim` resets status and assignee and records the recovery. Each `✓`/`Reclaimed` line is one reclaim, and its id gets a `reclaimed/stale-lease` events row. Then progress `reclaimed <n> stale lease(s)`. No partition logs `CHECK2 no persona in the chamber declares a partition — no lease is being reaped`. Source: `f043dee14^` `check2_reclaim_stale` + `parse_reclaimed` | WORKING rows past `SPIRA_RECLAIM_GRACE_SECS`, not wait-held, get a HolderDead event, an events row and a `bd note`. The protect step's successful writes apply before the reap |
| **2b** | `strand check` with `SPIRA_LIFECYCLE_ENFORCE=0`. strand then reads no wait holds and fires no HolderDead (see the gap below) | `strand check` with `SPIRA_LIFECYCLE_ENFORCE=1` |
| **2c** | Orphaned claims: snapshot beads per partition that are `open`, with an assignee, and whose lease is absent or past (an unparseable lease is left alone). Each gets `bd assign <id> ""`; only a successful assign prints `RELEASED\t<id>\t<assignee>`. Then progress `released <n> orphaned claim(s)`, and plan_ready is re-counted live for CHECK 3 and 8. Source: `542b9445f^` `release_orphan_claims_partitions` | Detect only: `INCONSISTENT` lines for rows whose holder and state disagree |
| **3b / 3c** | The seam runs with `SPIRA_LIFECYCLE_ENFORCE=0`. `mark_queue_waiters` and `close_landed_queue_waiters` then write only the `spira-queue-waiting` label; their `lc_hold wait`/`lc_unhold` dual writes are no-ops. That is exactly their pre-sp-mys5p behaviour | The same seam with the switch on: label plus hold, as lib.sh has it |
| **4 (poisoned?)** | The `spira-poison` bd label. Every partition excludes it, so a poisoned bead is not in the dispatchable set. `decide`'s `poisoned` argument is the label | The lifecycle `poison` hold |
| **4 (poison)** | `bd label add <id> spira-poison`. The note ends `… no persona can claim it again while the label stands.` Source: `e08d8982b^` | Hold poison (`spira-lc event`). The note ends `… while the hold stands.` No bd label |
| **4 (stale clear)** | Snapshot beads carrying `spira-poison`, not closed, not an epic or event. `clear` means `bd label remove <id> spira-poison` | Beads the lifecycle rows hold `poison` on. `clear` means Unhold poison |
| **4 (counts, asks)** | spira-claim over the bd events trail, the asked stamps, mail. Identical in both modes | ← |
| **5, 6, 6b, 7, 7c, 7d, 8** | No lifecycle read or write of their own. Children run with `SPIRA_LIFECYCLE_ENFORCE=0` | Children run with it `1` |

**Children.** The switch reaches everything this process starts:

- Every child gets `SPIRA_LIFECYCLE_ENFORCE=0|1`. That covers seams, strand,
  pilgrimage.sh, sending, watchtower, incident.sh and reflect.sh.
- Both systemd-run workers get it as `--setenv`. The audit worker resolves the same mode
  from it.
- That switch is the whole of OFF (sp-gypjk). No child is handed a poisoned tool path;
  `spira-lc`'s caller verbs gate each call on `SPIRA_LIFECYCLE_ENFORCE` (they replaced `lc.sh`,
  sp-arpjt), never on whether `spira-lc` exists.

**ON is loud.** Two things fail the unit:

- a missing binary, or a failed or unparseable `spira-lc list`: one line,
  `LIFECYCLE UNREACHABLE — lifecycle_enforce=1 but <why>; CHECK 2/2c/4 make no lifecycle decision this pass and the unit exits 1 (…)`;
- an event that cannot be applied, `rc=2` (cannot tell) or `127`.

After either, the rest of the pass (landing, summoning) still runs, and the process then
exits 1. A CAS refusal (rc 3) is the normal race: a WARN, and it is retried next pass.

**Gaps outside this crate** (they follow the same switch, but live in other components):

- **strand (CHECK 2b), OFF.** Its ghost fix only fires HolderDead through spira-lc, and its
  wait exemption reads only lifecycle holds. With the machine disabled a ghost gets the
  counter, note and event, but its status is not reset. Before sp-i2m7y it was
  `bd reclaim --id <id> --older-than 1s`, and the exemption read `spira-waiting-operator`.
  The strand crate needs the same switch (it reads `SPIRA_LIFECYCLE_ENFORCE`, which the
  sentinel now hands it).
- **sending.sh:488, OFF.** It wrote only `lc_content_on_base`, a no-op when disabled.
  Before `dc3e364bf` it was `bdq label add "$id" content-landed`, which CHECK 5's exemption
  still reads. Cutover row 39 — closed by sp-arpjt: the `sending` binary writes the label
  OFF and the ContentOnBase event ON (sending/DESIGN.md §2).

### 2.7 Finding the harness

`SPIRA_HOME` from the environment wins, as a test fixture sets it. Otherwise the first
directory with a `lib.sh` among these:

1. `<exe dir>/../spira`, the release layout `rel/bin/sentinel` + `rel/spira/lib.sh`;
2. `<exe dir>/../../spira`, which is `target/release/sentinel`;
3. `<exe dir>/../../../spira`, which is `target/<profile>/deps`.

If none has one, the binary prints
`FATAL sentinel: cannot find lib.sh (set SPIRA_HOME)` on stderr and exits 1. That is what
lets the refill ExecStopPost, which has no environment, still work.

### 2.8 Guarantees

- **G1. One bulk read per pass.** Each store read in §2.4 happens once. Every consumer
  derives its answer from the snapshot in-process: STATE, CHECK 2, 4, 5 and the ready cache
  here; strand and the seams through the exported snapshot files.
- **G2. The DB check is the bulk read.** A failed `bd list --all` is `DATABASE UNREADABLE`
  and exit 1. A pass that cannot see the graph reports no `state:` line and no `pass complete` (sp-4fss).
- **G3. Fail closed.** A failed `counts` makes no CHECK 4 decision this pass. The same holds
  for a failed lifecycle read of the poison set (ON), a failed ready read
  (plan_ready unknown, so neither CHECK 3 nor CHECK 8 fires on it), and an unresolvable base
  in CHECK 5 (unjudged) (law-a-control-that-cannot-check-must-refuse).
- **G4. `progressed` counts only DAG movement.** It is counted from both mailboxes too
  (drain by rename, every drain file, each line exactly once). CHECK 8 reads `progressed`,
  never `acted`.
- **G5. One pass per summon slot.** CHECK 7 only ever runs inside `ck7_summon_pass`'s flock,
  shared by the full pass and `--summon-only`.
- **G6. The audit worker and the landing worker never run twice.** The unit name is the
  mutex, and `--collect` is always passed.
- **G7. Positive controls.** `audit.status`, `landing.status`, `*.dispatched` and the stale
  WARN lines make "never ran" visible.
- **G8. Temp files never outlive the process.** The snapshots and the cache are removed on
  every exit path, including the early exits: RAII guard plus a SIGTERM handler.
- **G10. OFF never reaches spira-lc.** Not directly, and not through anything it starts (§2.9).
  The unit tests assert this for every OFF-mode test.
- **G11. Re-read before every write (sp-du8bv).** A decision comes from the pass-start
  snapshot; the write it leads to does not trust it. Before a snapshot-driven mutation, the
  check re-reads the beads it is about to touch — one `bd show <id>… --json` per check —
  and skips any whose status is no longer what the snapshot showed, whose row is gone, or
  whose re-read failed (`<CHECK> <id>: <was> in this pass's snapshot, <now> now — skipped,
  the next pass decides it again`). CHECK 2c also requires the same assignee and no live
  lease, because `bd assign <id> ""` is unconditional and would strip a claim an aeon took
  after the snapshot. Covered: CHECK 2's protect label, CHECK 2c's release, CHECK 3c's
  label, CHECK 4's poison and ask, and the stale-poison lift. ON-mode lifecycle writes are
  exempt because `spira-lc apply` carries the row version and the machine refuses a stale
  one. Summons are exempt because an aeon claims through `bd ready --claim`, which reads
  live. `bd reclaim` queries live itself. Implemented in `src/fresh.rs`.
- **G9. No payload in argv or env to a seam.** Seam scripts are constants. Data travels on
  stdin, and the environment carries only configuration and file paths.

## 3. Schema

```rust
/// A bead row, as `bd list --all --json` and `bd show --json` return it. Unknown fields are
/// ignored; null or absent become the default.
struct Bead {
    id: String, title: Option<String>, description: Option<String>,
    notes: Option<serde_json::Value>,       // string, or a list of strings/{text}
    status: String,                         // open | in_progress | blocked | deferred | closed | …
    priority: Option<serde_json::Value>, issue_type: Option<String>,
    labels: Vec<String>, parent: Option<String>, assignee: Option<String>,
    created_at: Option<String>, close_reason: Option<String>,
    dependency_count: u64, dependencies: Vec<Dep>,
}
struct Dep { depends_on_id: Option<String>, id: Option<String>,
             #[serde(alias = "dependency_type")] r#type: Option<String> }

/// One `spira-lc list` row. Dolt returns columns string-valued, so every field accepts a
/// string or its native type.
struct LcRow { bead_id: String, state: String, holder: Option<String>,
               lease_until: Option<i64>, holds: Vec<String> /* JSON array or its encoding */,
               version: Option<String> }

/// The context probe's answer (S0).
struct Context {
    env: Vec<(String, String)>,          // exported environment after sourcing lib.sh
    vars: BTreeMap<String, String>,      // fixed list of lib/conf variables (§6)
    fayths: Vec<Fayth>,                  // spira_fayths order
    partitions: Vec<Partition>,          // fayth_partitions (deduplicated by labels)
    chamber: Vec<String>,                // fayth_names (for roster_warnings)
    repos: Vec<Repo>,                    // audit only: spira_repos with their resolution
}
struct Fayth { name: String, labels: Vec<String>, exclude: Vec<String> }
struct Partition { labels: Vec<String>, exclude: Vec<String> }
struct Repo { name: String, root: Option<PathBuf>, landrefs: Vec<String> /* [0] = base */,
              queued: bool }

/// A landstate record's first two fields.
struct LandState { state: String, tip: String }

/// The two status files, read by `KEY=<digits or ->` lines only (as the old sed/eval did).
struct AuditStatus { at: Option<i64>, rc: Option<String> }
struct LandStatus  { at: Option<i64>, rc: Option<String>, branches: Option<String>, moved: Option<String> }

/// CHECK 4's per-bead inputs.
struct Counts { attempts: u32, requeues: u32, reclaims: u32 }
struct Stamp { requeue: bool, reclaim: bool, poison_at_n: bool, lifted_at_or_above_n: bool }
enum Token { RequeueMail, ReclaimMail, Poison, Clear, Ask }
/// CHECK 8's pure decision (lib.sh check8_should_judge).
enum Judge { Yes, Cooldown, No }

/// The pass's bookkeeping.
struct Tally { acted: u32, progressed: u32 }
enum Mode { Pass, Report, SummonOnly, Audit, OpenChildren { dry: bool } }
```

## 4. The pass, check by check

All numbered checks keep their names in the tsd phase rows (`setup`, `CHECK1`, `CHECK2`,
`CHECK2b`, `CHECK2c`, `CHECK3`, `CHECK6`, `CHECK3b`, `CHECK3c`, `CHECK7`, `CHECK8`, `end`).
These rows are the per-check timing; the `end` phase is never flushed, so a complete pass is
one with a `CHECK8` row. The full
pass writes them. The audit worker writes none: the old one crashed with
`_phase: command not found`.

**Bootstrap (every mode).**

1. Locate SPIRA_HOME (§2.7).
2. Run the context probe (S0), which gives the environment, the roster and (for audit)
   the repos.
3. Every child process then runs with exactly that environment, plus the snapshot paths.

**--summon-only.**

1. S1 (the world gate and capacity, with the same `summon-only: …` lines).
2. `live` summed over the fayths, logged as `summon-only: live=<n> fayths=[…]`.
3. One `bd ready` (raw), bucketed into the ready cache in-process (the port of
   ready-bucket.py).
4. S2 (`ck7_summon_pass`).
5. Log `summon-only pass complete — <a> action(s)` and exit 0.

**Full pass, --report and --audit.**

1. **Bulk read.**
   - Under `SPIRA_SKIP_RECLAIM≠1`, a failed `bd list --all` logs
     `DATABASE UNREADABLE — bd cannot reach <db>; state is unknown and this pass cannot close any gap`
     and exits 1.
   - The raw ready read follows.
   - Under `SPIRA_SKIP_RECLAIM=1` a failed read is not fatal: that snapshot is simply not
     exported, as before.

**STATE** (full pass and --report). Every value comes from the snapshot:

- `open_plan` (`Snapshot::plan_open`): rows whose labels ⊇ {scope?, `plan`}, with
  `status ≠ closed` and type not `epic`/`event` — the whole open plan backlog, wherever a
  bead is parented. Logged as `open=`.
- `plan_ready`: rows in the ready snapshot whose labels ⊇ {scope?, `plan`} and are disjoint
  from {`spira-poison`, ask}.
- `plan_inprog`: `status == in_progress` and labels ⊇ {scope?, `plan`}.
- `live`: as §2.5.

Under SKIP_RECLAIM all four are 0 and empty.

The pass then logs `state: …` and applies `roster_warnings`, ported: a WARN line per
chamber fayth left out of SPIRA_FAYTHS, deduplicated by the stamp. `--report` then prints
`\nOpen plan beads:\n` and one `  <id>` line per open plan bead (§9, B3), and exits 0.

**CHECK 1 — completed pilgrimages.** Runs pilgrimage.sh as §2.5.

**CHECK 2 — dead workers** (skipped under SKIP_RECLAIM). OFF: `legacy.rs`, the label and `bd reclaim` (§2.9). ON: `lifecycle.rs`, described below.

`check2_protect_waiting`, ported:

- **Candidates.** Rows in the snapshot with `status == in_progress` that either hold `wait`
  or have `dependency_count > 0`.
- **Dependency facts.** Each dependency's status and labels are joined from the snapshot.
  The old code used one `bd show` of all candidates for this.
- **Release.** A wait-held bead with no open `blocks` dependency carrying the ask label gets
  Unhold wait, then:
  - log `CHECK2 <id>: <ask> dep no longer blocking — wait released, re-enters the reaper`
  - act `unprotected <id>: <ask> dep closed`
- **Hold.** An unheld candidate whose open dependencies are all ask-labelled `blocks`
  edges (and there is at least one) gets Hold wait, then:
  - log `CHECK2 <id>: only open dep(s) carry <ask> — wait-held, excluded from reclaim`
  - act `protected <id> from reclaim: waiting on <ask> dep`

`check2_reclaim_stale`, ported:

- **Which rows.** WORKING lifecycle rows with a `lease_until` more than
  `SPIRA_RECLAIM_GRACE_SECS` past, and no `wait` hold.
- **Per row.** A HolderDead event, then the `reclaimed/stale-lease` events row, then
  `bd note` with
  `Reclaimed by CHECK 2: in_progress with a lease that expired <m>m ago and was never released.`
- **After the loop.** progress `reclaimed <n> stale lease(s)`.
- **A failed HolderDead** skips that row, as `|| continue` did.

**CHECK 2b — stranded work.** Runs `strand check` as §2.5.

**CHECK 2c** (skipped under SKIP_RECLAIM). OFF: `legacy.rs` releases orphaned claims (§2.9). ON: `lifecycle.rs` checks lifecycle consistency, as below.

- A WORKING row with no holder prints `INCONSISTENT\t<id>\tWORKING with no holder`.
- A non-WORKING row with a holder prints `INCONSISTENT\t<id>\t<state> with a holder still set`.
- If any printed, log
  `CHECK2c: <n> spira-lc row(s) with holder/state out of sync — a bug reached spira_lifecycle outside its own CAS`
  and act `surfaced <n> inconsistent spira-lc row(s)`.

**CHECK 3 — stale blocked flags.** Runs only when not SKIP_RECLAIM, `plan_ready == 0`,
`plan_inprog == 0` and `n_open > 0`.

1. `bd recompute-blocked`.
2. Recount with `bd ready --limit 0 --exclude-type epic,event -u [--label scope]
   [--exclude-label noloop] --label <scope,>plan --exclude-label spira-poison,<ask> --json`.
3. Log `recomputed is_blocked`.
4. If the count changed, progress `recompute-blocked freed <n> bead(s)`.

**Audit dispatch** (full pass).

1. Drain the audit mailbox.
2. If `audit.status` exists, log `CHECK4/5 audit: last run <age>s ago — rc=<rc>`.
3. Dispatch, and log whichever holds:
   - `CHECK4/5 audit: already running — this pass does not start another`
   - `CHECK4/5 audit: dispatched as <unit>`, then write `audit.dispatched` if absent
   - `CHECK4/5 audit: started underneath this pass — not starting another`
   - `CHECK4/5 audit WARN: could not dispatch the audit worker; poison, closed-not-landed, sending and collision checks will not run until this is fixed`
4. If nothing ran within the stale window and none is running, log
   `CHECK4/5 audit WARN: no audit pass has completed in <age>s and none is running`.
5. Drain the mailbox again.

**CHECK 6 — land dispatch.**

1. Drain the landing mailbox.
2. Run the four watchtower checks.
3. Read the positive control: `CHECK6: last landing <age>s ago — rc=… , … branch(es) seen, … moved`,
   or `CHECK6: no landing has ever completed on this host`.
   - A non-zero rc logs `CHECK6 WARN: the last landing exited <rc> — see <run>/landing.log`
     and escalates through S3.
4. Dispatch, and log whichever holds:
   - `CHECK6: a landing is already in flight — this pass does not start another`
   - `CHECK6: landing dispatched as <unit>`
   - `CHECK6: a landing started underneath this pass — not starting another`
   - `CHECK6 WARN: could not dispatch the landing worker; nothing will land until this is fixed`,
     then S3
5. Stale check: `CHECK6 WARN: no landing has completed in <age>s and none is running`,
   then S3.
6. Drain again.

The S3 evidence strings are byte-identical to today's.

**CHECK 3b.** S4: `mark_queue_waiters`, then `close_landed_queue_waiters`.

**CHECK 3c — coordination beads with open children** (`src/open_children.rs`, replacing
lib.sh `mark_open_children`, sp-du8bv). The label is `SPIRA_OPEN_CHILDREN_LABEL`; empty
disables the check.

1. Build the candidates: the ready snapshot narrowed to `SPIRA_SCOPE_LABEL`, then every
   snapshot row with `status = open` that carries the label. Deduplicate, ready first. With
   no ready snapshot, make one live claim query (`READY_ARGS`).
2. Build the open-parents set in one walk over the list snapshot: for every row whose status
   is not `closed`, its `parent` field and the target of every `parent-child` dependency.
3. Add the label where a candidate is an open parent and lacks it; remove it where a
   candidate carries it and is not. Log exactly as lib.sh did:
   `mark_open_children: <id> — has an open child, excluded from dispatch` and
   `mark_open_children: <id> — children all closed, re-enters dispatch`.
4. Re-read before writing (G11).

No `bd children` call is ever made. The bash version made one per candidate, and 221 of them
cost 302 s against a 60 s pass budget (§5).

**CHECK 7.** S2.

**CHECK 4 — the poison valve** (audit). The poisoned set, the poison write and the stale-clear lift follow the switch (§2.9): labels OFF, holds ON. Everything else below is common to both modes.

1. **Build the set.** `dispatchable` is every snapshot row, per partition in roster order,
   with:
   - `status ≠ closed`;
   - type not `epic` or `event`;
   - labels ⊇ the partition's labels and disjoint from its exclusions.

   Rows are deduplicated by id, first occurrence wins. If no partition exists, log to
   stderr `WARN no persona in the chamber declares a partition — …`.
2. **Log the set.**
   `CHECK4 examining <n> dispatchable bead(s), poison=<P> requeue=<R> reclaim=<C>`.
3. **Count.** One `spira-claim counts`. A non-zero exit logs
   `CHECK4 bulk attempts query failed (rc=<rc>) — making no poison/requeue/reclaim decision this pass`
   and the loop is skipped.
4. **Poisoned set.** Read from the lifecycle rows (holds ∋ `poison`).
5. **Decide, per bead, in order.** The stamp is `rq:rc:po:pl`, read from the four asked or
   lifted files. Then `decide n rq rc labels stamp poisoned`, and the tokens act:
   - **`requeue-mail`.** Causes come from the legacy `sp-requeue-N[-cause]` labels, as
     `<cause> x<N>` joined by `, `, else `unrecorded`. Subject:
     `Spira bead <id> — completed and requeued <rq> times, never landed (<causes>) — the harness cannot land it`.
     The evidence is bead_context + `REQUEUES`/`ATTEMPTS` lines. It is mailed with
     `SPIRA_MAIL_REPEAT_CONSIDERED=sentinel-own-dedup`. It is marked only on success, else
     logged `CHECK4 <id>: requeue escalation path refused the ask — retries next pass`.
   - **`reclaim-mail`.** The same shape, with the reclaim wording and caps.
   - **`poison` or `ask`.**
     - First re-read the bead with `bd show <id>` (G11). If it is now `closed`, log
       `CHECK4 <id>: <n> attempts, but it closed while this pass ran — not poisoned, not asked`.
       If its status is otherwise not the snapshot's, or the re-read failed, log the G11 skip
       line. Either way, nothing is written.
     - For `poison`: Hold poison `poisoned after <n> in_progress transition(s) without landing`
       (actor `sentinel`), then `bd note` (fixed text), then progress
       `poisoned <id> after <n> attempts`, then the spira_event `bead.poisoned` (S5).
     - For `ask`: the evidence is bead_context from that show, plus `REPO`, `ATTEMPTS`,
       `BRANCH` (commit count and diffstat against the bead repo's landref), and the
       25-line trace tail (S6). It is mailed without the repeat override, and
       `poison_asked_mark` is written only on success, else the log line
       `CHECK4 <id>: the escalation path refused the ask — it stands, and the next pass retries it`.
6. **CHECK 4 supplement (closed but unlanded).**
   - The set is snapshot rows per partition that are closed, not an epic or event, carry a
     `branch:` label, and match the partition's labels and exclusions.
   - Count the set; failure logs `CHECK4-closed bulk attempts query failed (rc=…) — making no requeue decision this pass`.
   - Per bead: `decide 0 rq 0 labels rq:1:1`; only `requeue-mail` acts.
   - Skip, with a log, when `landed()` holds:
     `CHECK4-closed <id>: reopens=<rq> but already landed — no escalation`.
   - Skip when the LANDED landstate tip is an ancestor of the base:
     `CHECK4-closed <id>: landed by ancestry (landstate tip on <repo>) — no escalation`.
   - Otherwise mail, with the `STATUS    closed (not landed in <repo>)` evidence.
7. **Stale poison clear.**
   - Walk the lifecycle rows holding `poison`.
   - Skip a bead that is closed in the snapshot, or is an epic or event.
   - One `spira-claim counts` for the set. On failure, log per bead
     `CHECK4 <id>: attempts query failed — stale-poison-clear makes no decision this pass`.
   - `decide n 0 0 "" 1:1:1 1`. If it says `clear`, Unhold poison and progress
     `CHECK4 <id>: stale poison cleared — <n> attempt(s), below threshold <P>`.

**CHECK 5 — closed but not landed** (audit — DELETED; lifecycle LANDED supersedes it).

1. **Incidents.** Open or in-progress snapshot rows carrying `$SPIRA_INCIDENT_LABEL`,
   keyed by their `ref:<hash>` label.
2. **Candidates.** Closed rows of a work type, per partition (labels ⊇ partition; no
   exclusions, as today). Each carries: repo (`repo:` else the home repo), superseded (any
   `supersedes` edge), `spira-dropped`, `delivers:*`, `content-landed`, subsumed (the
   close_reason matches `SUBSUMED|DUPLICATE|tracked in epic`, case-insensitive), and
   `branch:`. They are sorted and deduplicated by (repo, id).
3. **Per bead, skip when:** it has no `<run>/<id>.log`, or any exemption flag is set.
4. **Resolve the repo.** A repo absent from the map logs once:
   `CHECK5: repo:<r> is not in repo-map — skipping its closed beads`.
5. **One `git log --format=%s <base>` per repo** builds the landed-id set from three
   subject shapes:
   - `spira: land <id>…`
   - `Merge branch 'spira/<id>'…` (neither `round-*` nor anything containing a space)
   - `<token>:` (the awk port)
6. **No base.** Log `CHECK5 <id>: cannot resolve the ref <r> lands on — not judging whether it landed`.
7. **Prove landed**, by the landed set, the branch ancestry, or the landstate LANDED tip
   ancestry. Proof means resolving its incident (`bd close --force … --reason-file -`, with
   evidence text identical to today's), within `MAX_RESOLVE` and the budget.
8. **Otherwise file**, within `MAX_FILE` and the budget, through incident.sh with the same
   env and body.
9. **Summary lines**, identical to today's.

**CHECK5-LC — the ON-path replacement, alongside CHECK 5, never instead of it** (audit;
design sp-pswer.2: "design ON-path replacement for CHECK5 / groomer STATE sweeps"). CHECK 5
and lib.sh's three groomer STATE sweeps (`detect_landed_but_open`,
`detect_closed_unlanded_states`, `detect_false_blockers`) all prove the same three drifts —
bd's own status disagreeing with what actually landed — from git log and a hand-maintained
exclusion list, because `spira_lifecycle` had no equivalent record. It does now: a work
bead's `spira-lc` row reaches `LANDED`/`SUPERSEDED`/`DROPPED`/`DONE` only through a
proof-carrying transition (`content_on_base`, `delivered`, `done`, `supersede`, `drop`,
never a bare bd close), so the row's own terminal-ness is the same fact CHECK 5 spends a
`git log` walk proving, and comparing it against `bd`'s status is a lookup, not a walk.

Only when `lifecycle_enforce` is ON (`lc_rows()`, the same one read CHECK 2/2c already share
this pass), run unconditionally in `Lifecycle::On` right after CHECK 5, never gating CHECK
5's own call — `spira/test-legacy-state-checks-ungated.sh` (sp-pswer.1) fails the build the
day anyone tries. `lifecycle.rs`'s `landed_but_open` / `closed_unlanded` / `false_blockers`:

1. **landed-but-open** — a work bead `bd` shows open/in_progress whose `spira-lc` row is
   `LANDED`. `STATE-LC <id> landed-but-open — spira-lc row is LANDED; close it`.
2. **closed-unlanded** — a work bead `bd` shows closed whose `spira-lc` row is *not* one of
   the four terminal states (`BeadState::is_terminal`, lifecycle crate) — replacing the
   legacy sweep's whole hand-maintained exclusion list (`supersedes`, `spira-dropped`,
   `delivers:*`, `content-landed`) with the one property those all encode. `STATE-LC <id>
   closed-unlanded — spira-lc row is <state>, not a terminal state`.
3. **false-blockers** — an open/in_progress bead with a `blocks` dependency on one of (2)'s
   ids. `STATE-LC <id> blocked-by-unlanded <blocker> — depends on <blocker>, which is closed
   but its spira-lc row is not a terminal state`.

Detect, never repair — the same posture as CHECK 2c's `INCONSISTENT` lines, for the same
reason: this is the side-by-side comparison the epic's scope needs before either legacy path
is retired, not a fourth writer racing `bd_close_on_land`. Summary: `CHECK5-LC: <n> state
drift line(s) from spira-lc, alongside CHECK 5's own`; act `surfaced <n> CHECK5-LC line(s)`;
silent when `<n>` is 0.

**CHECK 6b — the Sending** (audit).

1. **Skip test.** Skip only when the `sending.base` stamp matches every swept repository's
   current landref sha, and every swept repo is in the stamp. Swept means not queue-mode,
   with a root and a landref. On a skip, log `sending: base unchanged — skipped`.
2. **Otherwise** run `sending --skip-queue` and act on each `SENT` line:
   `act "sent <repo> <branch> <id>"`.
3. **Failures.** A `FAILED` line logs `sending reported a branch it could not delete`.
4. **Stamp.** Rewrite the stamp.

**CHECK 7c / 7d** (audit; skipped under SKIP_RECLAIM).

- S7 runs `detect_unclaimable_ready`. For each result:
  1. print it;
  2. log `CHECK7c: <n> ready bead(s) no persona can claim — fix each by adding or removing the label named above`;
  3. act `surfaced <n> unclaimable ready bead(s)`;
  4. S8 `file_unclaimable_incidents`, with the output on stdin.
- S9 runs `detect_branch_collisions`. For each result:
  1. print it;
  2. S10 `park_branch_collisions`, on stdin;
  3. count `FREED`, `UNLABELED` and the parked remainder;
  4. log and act with the three messages from today.

**End.**

- Audit: write `audit.status` (`SP_AUDIT_AT=<now>`, `SP_AUDIT_RC=0`), log
  `audit pass complete — <a> action(s), <p> progress`, exit 0.

**CHECK 8 — judgement.** `check8_should_judge(plan_ready, plan_inprog, n_open, progressed,
last, now, every)` decides:

- `no` when `plan_ready > 0`;
- `cooldown` or `yes` when `plan_inprog == 0 ∧ n_open > 0 ∧ progressed == 0`;
- `no` otherwise.

The outcomes:

- **Cooldown.** Log `starved, but inference is in cooldown (<left>s left)`, then
  `pass complete`, and exit 0.
- **Yes.**
  1. Write the cooldown.
  2. Log `STARVED — <n> open, 0 ready, 0 running. Dropping to inference.`
  3. Run reflect.sh.
  4. Act `invoked reflection`.

Then log `pass complete — <a> action(s), <p> progress`.

## 5. Performance — the pass-time budget

**History.** The bash full pass took a median 213 s (max 537 s, 2026-09-28) against a
2-minute timer, before sp-bo67y (the snapshots) and sp-994y9 (the audit split). After them
it took about 20 s.

The Rust pass regressed to a median 198 s (106–375 s over the last 30 production passes,
2026-09-29 15:37Z to 2026-09-30 09:53Z; sp-du8bv). The `sentinel-phase` rows attributed
91.5 % of it to CHECK 3b (median 181.5 s, max 338 s). Every other check had a median of
4 s or less. Inside CHECK 3b, lib.sh `mark_open_children` ran `bd children` once per
candidate, and 221 candidates took 302 s when measured alone. CHECK 3c now decides from the
snapshot. The same 8,504-row store and 221 candidates take 2.4 s end to end with
`--open-children --dry-run`, including the probe and both snapshot reads; the decision
itself takes milliseconds.

**Budget:**

| mode | target (p50) | alarm | hard ceiling |
|---|---|---|---|
| full pass | ≤ 20 s (Rust aims under 10 s: the snapshot reads are ~1.2 s + ~1 s, and the seams are ~0.2 s each plus their own work) | `WARN pass took <s>s — over the <T>s pass budget (SPIRA_SENTINEL_PASS_TARGET_SECS)`, logged when wall > T (default 60) | TimeoutStartSec=900 |
| --summon-only | ≤ 2 s | — | TimeoutStartSec=60 |
| --audit | minutes; it is off the critical path | the stale WARN at 1800 s | RuntimeMaxSec=1800 |
| CHECK 7 inside a pass | — | `not evaluated (pass budget exhausted)` | `SPIRA_SENTINEL_PASS_BUDGET_SECS=90` (in the seam) |

**Store reads per full pass: exactly one of each**, and the audit worker makes the same
three:

- `bd list --all` (~25 MB, ~1.1 s on the live store, 2026-09-29),
- `bd ready` (raw),
- `spira-lc list`.

The bash worker made one `bd list` per partition (seven) for dispatchable_open, seven more
for check4_closed_branched (~10.5 MB each), and seven for CHECK 5, because it never took
the snapshot.

**Other processes per full pass:**

- one bash startup each for the probe, CHECK 3b and CHECK 7 (~0.2 s each);
- pilgrimage.sh, strand, the four watchtower calls and two systemd-run calls;
- nothing per bead on the common path.

The per-bead subprocesses are:

- `spira-claim decide` in the audit, a few ms each, ~240 beads;
- `bd show`, and the lifecycle `show` plus event, only when a decision acts.

## 6. The lib.sh seams

Every seam runs `bash -c '<fixed script>' sentinel-<name>`. That means:

- The script is a `const &str` in `src/seams.rs`, never assembled from data.
- Data arrives on stdin.
- The environment is the probe's, plus these:
  - `SENTINEL_LIB`, the lib.sh path;
  - `SENTINEL_TALLY`, a temp file;
  - the snapshot paths.
- Each script starts with the shared prelude:

```bash
set -uo pipefail
. "$SENTINEL_LIB" || exit 97
act()      { printf 'act\t%s\n' "$*" >> "$SENTINEL_TALLY"; log "ACT $*"; }
progress() { printf 'progress\t%s\n' "$*" >> "$SENTINEL_TALLY"; log "ACT $*"; }
```

After each seam, the sentinel reads the tally and adds it to its counters. Each `progress`
line counts as both acted and progressed, since the old `progress` called `act`. In audit
mode each progress line is also appended to the audit mailbox. This preserves the exact
counter semantics lib.sh functions had when they ran in-process.

| id | name | script body (after the prelude) | stdin | stdout |
|---|---|---|---|---|
| S0 | probe | `env -0`; then the NUL-separated sections `@vars` (fixed list: `SPIRA_POISON_ASKED SPIRA_REQUEUE_ASKED SPIRA_RECLAIM_ASKED SPIRA_POISON_LIFTED SPIRA_ROSTER_WARN_STAMP SPIRA_CAPACITY_PAUSE SPIRA_HOME_REPO_RESOLVED=$(spira_home_repo)`), `@fayths` (`spira_fayths`, each with `FAYTH_LABELS`/`FAYTH_EXCLUDE_LABELS` via `fayth_get`), `@partitions` (`fayth_partitions`), `@chamber` (`fayth_names`), and, when `SENTINEL_PROBE_REPOS=1`, `@repos` (`spira_repos`, each with `repo_root`, `spira_landrefs`, `repo_land_queued`) | — | parsed |
| S1 | summon-gate | `world_gate fleet summon-only \|\| exit 1; if capacity_paused; then log "summon-only: account out of capacity for another ${SPIRA_CAPACITY_LEFT}s — not summoning"; exit 1; fi` | — | inherited (log) |
| S2 | ck7 | `ck7_summon_pass` | — | inherited |
| S3 | ~~land-escalate~~ | **retired (sp-31hjr):** `land_escalate` (and the `ask_already_open` dedupe it leans on) is native Rust now — `dispatch.rs::land_escalate`/`pass.rs::ask_already_open`, called in-process from CHECK 6. `sentinel --land-escalate` (stdin: line 1 = why, rest = evidence) drives it alone for the real-sender suites that used to source lib.sh directly. | — | — |
| S4 | ~~check3b~~ | **retired (sp-fbqsv, wave 4.28):** `mark_queue_waiters`/`close_landed_queue_waiters` are native Rust now — `waiters.rs`, called in-process from `full()` with the pass's own broad ready snapshot already in memory (no `$SPIRA_READY_SNAPSHOT` temp-file round trip). `sentinel --mark-queue-waiters`/`--close-landed-queue-waiters` drive each alone for lib.sh's own shims. (`mark_open_children` is CHECK 3c, also in Rust.) | — | — |
| S5 | event | `IFS= read -r -d '' k; IFS= read -r -d '' t; IFS= read -r -d '' ti; IFS= read -r -d '' de; spira_event "$k" "$t" "$ti" "$de" \|\| true` | 4 NUL-terminated fields | — |
| S6 | ~~trace-tail~~ | retired (sp-27d3d, wave 4.34): `trace_tail` was lib.sh; `check4.rs` now calls `aeon::trace::trace_tail` in-process (`SPIRA_TRACE_MARK` read from S0's `@vars`), no seam | — | — |
| S7 | ~~detect-unclaimable~~ | **retired (sp-fbqsv, wave 4.28):** `detect_unclaimable_ready` is native Rust now — `detect.rs`, called in-process from CHECK 7c. It still shells to `unclaimable.py` for the classification itself (kept as the one shared, independently-tested classifier — cockpit-collect's own suite cross-checks against it); this seam only carried the bash orchestration around that call. `sentinel --detect-unclaimable` drives it alone. | — | — |
| S8 | ~~file-unclaimable~~ | **retired (sp-fbqsv, wave 4.28):** `file_unclaimable_incidents` is native Rust now — `detect.rs`. `sentinel --file-unclaimable` (stdin: S7's output) drives it alone. | — | — |
| S9 | ~~detect-collisions~~ | **retired (sp-fbqsv, wave 4.28):** `detect_branch_collisions` is native Rust now — `detect.rs`, called in-process from CHECK 7d. `sentinel --detect-collisions` drives it alone. | — | — |
| S10 | ~~park-collisions~~ | **retired (sp-fbqsv, wave 4.28):** `park_branch_collisions` is native Rust now — `detect.rs`. `sentinel --park-collisions` (stdin: S9's output) drives it alone. | — | — |

Retire each seam when the function behind it gets its own crate. S0's roster half retires
when spira-config's persona table carries labels and exclusions, the same condition as
strand's §2.5.

## 7. Tests

Every external effect goes through one port, `Runner`: a program, its argv, stdin, extra
env, and a capture mode, returning rc, stdout and stderr. The clock is a separate port,
`Clock`. The files under a temp `SPIRA_RUN` are real.

The fakes script responses by program and argv, and record every call. So each contract
row above is a unit test on the exact argv and stdin, and on the log lines. The seams are
tested by asserting on the script name and stdin, and by feeding a tally file back. Pure
functions have their own tests:

- the partition predicate;
- the ready bucket (ported from ready-bucket.py);
- CHECK 5's subject parser;
- the cause renderer;
- `check8_should_judge`;
- bead_context;
- the status-file parsers;
- the Sending skip test.

## 8. Cutover (applied by the Concierge; nothing below is edited by this branch)

> **Historical record.** This cutover has been applied. Its `SPIRA_SENTINEL_BIN` / `spira_bin`
> rows were later superseded by sp-gypjk: every caller invokes `sentinel` by bare name on the
> launcher's PATH, and no binary resolver remains.

Line numbers are against this branch's base, `7ce25b21b`.

**Build and resolve the binary**

| # | file:line | current | replacement |
|---|---|---|---|
| 1 | `Makefile:46` | `for _b in loom broker czar-pass reconciler queue-watch spira-supervise spira-config tsd-write spira reconciler-flow; do \` | `for _b in loom broker czar-pass reconciler queue-watch spira-supervise spira-config tsd-write spira reconciler-flow sentinel strand spira-claim; do \` (strand and spira-claim per their own cutovers; the sentinel needs both at runtime) |
| 2 | `Makefile:53` | `… tsd-write spira reconciler-flow panel; do \` | `… tsd-write spira reconciler-flow sentinel strand spira-claim panel; do \` |
| 3 | `spira/conf.sh` after `:2310` (`export SPIRA_LC_BIN`) | — | `: "${SPIRA_SENTINEL_BIN:=$(spira_bin sentinel 2>/dev/null)}"; export SPIRA_SENTINEL_BIN`. Also add spira-claim's own item 1 (`SPIRA_CLAIM_BIN`) and strand's row 1 (`SPIRA_STRAND_BIN`). The sentinel falls back to `spira_bin` without them, but aeon.sh and lib.sh need them. |

**Units**

| # | file:line | current | replacement |
|---|---|---|---|
| 4 | `systemd/spira-sentinel.service:16` | `ExecStart=@SPIRA_PROD@/sentinel.sh` | `ExecStart=@SPIRA_PROD_ROOT@/bin/sentinel` |
| 5 | `systemd/spira-sentinel.service:6` | `Documentation=file://@SPIRA_HOME@/sentinel.sh` | `Documentation=file://@SPIRA_PROD_ROOT@/sentinel/DESIGN.md` |
| 6 | `systemd/spira-summon.service:11` | `ExecStart=@SPIRA_PROD@/sentinel.sh --summon-only` | `ExecStart=@SPIRA_PROD_ROOT@/bin/sentinel --summon-only` |
| 7 | `systemd/spira-summon.service:6` | `Documentation=file://@SPIRA_HOME@/sentinel.sh` | `Documentation=file://@SPIRA_PROD_ROOT@/sentinel/DESIGN.md` |
| 8 | `spira/lib.sh:2570-2573` (`summon_refill_argv`) | `printf -- '--property=ExecStopPost=%s --user --collect --quiet %s --summon-only' "$bin" "$SPIRA_HOME/sentinel.sh"` | `printf -- '--property=ExecStopPost=%s --user --collect --quiet %s --summon-only' "$bin" "${SPIRA_SENTINEL_BIN:-$(spira_bin sentinel 2>/dev/null)}"`. Not `${…:?}`: that would kill summon_fayth's shell mid-summon. The binary finds its harness from its own path (§2.7), so no `--setenv` is needed. |
| 9 | `~/.config/systemd/user/spira-sentinel-prod.service.d/skip-check5-until-lifecycle-cutover.conf` | `Environment=SPIRA_SKIP_CLOSED_CHECK=1` | unchanged, but note it **starts taking effect** with this binary (§9, B1). Today CHECK 5 runs in the audit worker regardless: 979 `CHECK5` lines in `run/audit.log`, the last at 03:58Z 2026-09-29. |

Then `systemd/install.sh` (or `unit-ensure.sh`) re-renders, and `daemon-reload`.

**Installers that parse ExecStart**

| # | file:line | current | replacement |
|---|---|---|---|
| 10 | `install.sh:227-229` (`_conflict_foreign`) | `inst_exec_dir="$(dirname "${inst_exec%% *}" 2>/dev/null)"` | `inst_exec_dir="$(dirname "${inst_exec%% *}" 2>/dev/null)"; case "${inst_exec%% *}" in */bin/sentinel) inst_exec_dir="$(dirname "$inst_exec_dir")/spira" ;; esac` (the binary lives in `<root>/bin`, and its SPIRA_HOME is `<root>/spira`) |
| 11 | `spira/deploy.sh:287` | `_dry_exec="$SPIRA_RELEASES/current/spira/sentinel.sh"` | `_dry_exec="$SPIRA_RELEASES/current/bin/sentinel"` |
| 12 | `systemd/install.sh:431` (comment) | `… runs from $SPIRA_PROD/sentinel.sh.` | `… runs from $SPIRA_PROD/../bin/sentinel.` |

**Callers**

| # | file:line | current | replacement |
|---|---|---|---|
| 13 | `spira/ready.sh:192` | `if ! _rep="$(timeout 20 "$SPIRA_HOME/sentinel.sh" --report 2>&1)"; then` | `if ! _rep="$(timeout 20 "$SPIRA_SENTINEL_BIN" --report 2>&1)"; then` |
| 14 | `spira/ready.sh:193` | `UNKN "ready work — sentinel.sh --report failed or timed out" \` | `UNKN "ready work — sentinel --report failed or timed out" \` |
| 15 | `spira/canary.sh:136` | `bash "$SPIRA_HOME/sentinel.sh" 2>&1 \| sed 's/^/  sentinel: /' \|\| true` | `"$SPIRA_SENTINEL_BIN" 2>&1 \| sed 's/^/  sentinel: /' \|\| true` |
| 16 | `spira/timeout-lint.sh:41` | `files=("$HERE/aeon.sh" "$HERE/sentinel.sh" "$HERE/landing.sh")` | `files=("$HERE/aeon.sh" "$HERE/landing.sh")` |
| 17 | `spira/config-fence-allow:68` | `spira/sentinel.sh` | delete the line |

**Delete**

| # | file:line | current | replacement |
|---|---|---|---|
| 19 | `spira/sentinel.sh` | the component | delete |
| 20 | `spira/ready-bucket.py` | — | **keep**: `bulk_ready_by_fayth` still feeds the watchtower crate's `--show`/sweep (idle-while-ready, via `seams::pipeline_probe`, sp-lnmbq). The sentinel buckets the cache in Rust (`summon::bucket`, a port of it). |

**Bash tests to repoint or retire.** Each is either repointed at the binary
(`"$SPIRA_SENTINEL_BIN"` for `bash "$SH/sentinel.sh"`, with a fixture that also puts the
binary where §2.7 finds `$SH/lib.sh`, or sets `SPIRA_HOME=$SH`) or retired in favour of the
named unit tests.

| # | test | action |
|---|---|---|
| 21 | `test-sentinel-pass.sh` | repoint. The `--unit=spira-audit … sentinel.sh --audit` assertion becomes `… <binary> --audit`. The rest are `tests::*` (whole passes against the fake runner) |
| 22 | `test-summon-fast-path.sh:342` | `*"--property=ExecStopPost=$T/bin/mock-summon-noop --user --collect --quiet $T/sentinel.sh --summon-only"*)` → `… --quiet $SPIRA_SENTINEL_BIN --summon-only"*)` |
| 23 | `test-poison.sh` | reduce to the "stays end-to-end" rows of spira-claim/DESIGN.md §4, run against the binary |
| 24 | `test-check5-invariant.sh` | repoint `--audit` |
| 25 | `test-sentinel-check5-subsumed.sh` | repoint `--audit` |
| 26 | `test-closed-strand.sh` | repoint |
| 27 | `test-loop-readonly.sh` | repoint: run `$CURRENT/bin/sentinel` |
| 28 | `test-strand-truncated.sh` | repoint |
| 29 | `test-canary.sh` | follows canary.sh |
| 30 | `test-tsd-producers.sh` | repoint; the phase names are unchanged |
| 31 | `test-sentinel-store-reads.sh` case 8 | repoint. The bd-call counter should now see exactly 1 `list` + 1 `ready` from the sentinel itself (the audit worker makes the same two) |
| 32 | `test-check4-unit.sh` | drives lib.sh `check4_decide`; it follows spira-claim's cutover item 7, not this one |
| 33 | `test-check8-progressed.sh` | drives lib.sh `check8_should_judge`, which has no caller once sentinel.sh is gone; retire together with the function (row 38). Its table is `audit::tests::judgement_table` |
| 34 | `test-ready-timers.sh:88` | mocks `sentinel.sh --report`; mock `SPIRA_SENTINEL_BIN` instead |
| 35 | `test-install-exec.sh:146,207` | drop `sentinel.sh` from the executable list; the render-fallback assertion becomes `ExecStart=$(dirname "$HERE")/bin/sentinel` |
| 36 | `test-install-conflicts.sh:57` | the fake foreign unit becomes `ExecStart=$FOREIGN_HOME/../bin/sentinel` |
| 37 | `test-deploy.sh:140,433` | the stub moves to `current/bin/sentinel` |
| 39 | `spira/sending.sh:488` | `lc_content_on_base "$id" "merge-tree:$(git -C "$REPO" rev-parse "$LANDREF" 2>/dev/null)" sending >/dev/null 2>&1 \|\| true` | `if [ "${SPIRA_LIFECYCLE_ENFORCE:-0}" = 1 ]; then lc_content_on_base "$id" "merge-tree:$(git -C "$REPO" rev-parse "$LANDREF" 2>/dev/null)" sending >/dev/null 2>&1 \|\| true; else bdq label add "$id" content-landed >/dev/null 2>&1 \|\| true; fi` (restores CHECK 5's `content-landed` exemption in OFF mode) |
| 40 | `strand` crate (`check.rs` `act_ghost`/`lc_holder_dead`; the wait-held exemption in `probe.rs`) | spira-lc only | behind `SPIRA_LIFECYCLE_ENFORCE`. OFF: `bd reclaim --id <id> --older-than 1s`, and the exemption reads the `spira-waiting-operator` label. That is strand's own change, not made here (§2.9 gaps) |
| 38 | lib.sh functions left with no production caller | **DONE at sp-8itaf.** `check2_protect_waiting` (was 1363), `check2c_lc_consistency` (was 2335), `check2_reclaim_stale` (was 2348) — ported to `lifecycle.rs`; `check8_should_judge` (was 1697, → `audit::tests::judgement_table`); `check4_closed_branched` (was 5600); `ready_cache_populate` (was 1563); `roster_warnings` (was 1051, Rust port already in `pass.rs`) all deleted outright, along with `test-check2-reclaim.sh`, `test-check2-reaper.sh`, `test-check8-progressed.sh`, `test-roster-warn.sh` and the `test-poison.sh`/`test-sentinel-store-reads.sh` references to them. **Correction:** this row's earlier note said `dispatchable_open` stays — re-grepped at sp-8itaf (whole tree, including Rust string literals), it had zero live callers too (`all_partition_members` already covers `groomer deadlocked`, as this row said before `groomer.sh` itself moved to Rust) and was deleted with the rest. |

**Source greps that break when the file goes.** Each greps sentinel.sh's text; point it at
`sentinel/src/*.rs`, or at lib.sh where the text now lives:

- `test-lanes.sh:39`
- `test-czar-pass.sh:287`
- `test-wire-token.sh:35`
- `test-fayth.sh`
- `test-event-taxonomy.sh:61`
- `test-timeout-lint.sh:58`

About 30 suites carry `# covers: … spira/sentinel.sh`. That changes to
`sentinel/src/*.rs`, which only affects suite selection.

## 9. Behaviour deliberately changed

- **B1. The audit worker inherits `SPIRA_SKIP_CLOSED_CHECK` and `SPIRA_SKIP_RECLAIM`.**
  - The old `--setenv` list dropped both. The drop-in the operator installed on 2026-09-26
    to turn CHECK 5 off never reached the only process that runs CHECK 5 (since sp-994y9).
    It filed and resolved incidents all along: 979 CHECK5 lines in the live audit.log.
  - With this binary CHECK 5 really stops while the drop-in stands.
  - If the operator wants CHECK 5 running until the cutover, delete the drop-in.
- **B2. STATE and every CHECK 2, 4 and 5 input come from the pass's one snapshot, not from
  live per-check queries.**
  - The values are as of pass start, seconds older than before.
  - The two reads that decide a write stay live, so no race widens:
    - the poison and ask status re-read (`bd show`);
    - the lifecycle CAS version (`spira-lc show`).
  - The audit worker now also takes the snapshot. The bash worker never did, and paid
    twenty-odd per-partition `bd list` calls.
  - CHECK 2's dependency facts come from the snapshot join instead of a batch `bd show`.
- **B3. `--report` with no open children prints no indented line.** It used to print one
  line of two spaces, which `ready.sh:196` counted as "1 open bead".
- **B4. CHECK 4 counts through spira-claim.**
  - Requeues are counted from judged returns rather than raw `reopened` rows (sp-j1q6o), so
    rebase returns stop tripping REQUEUE_AT.
  - The stale-poison-clear sweep reads its counts in one call instead of one per bead.
  - A failed poison-set read (spira-lc present but erroring) makes no CHECK 4 decision. It
    used to read as "not poisoned", which could re-hold a held bead and never clear one.
- **B5. The audit worker writes no tsd phase rows.** It never did successfully, and its
  `_phase: command not found` stderr noise is gone.
- **B6. `bd note` text goes on stdin (`--stdin`), not argv** (law-payloads-go-on-stdin).
  The text is unchanged.
- **B7. New WARN when a full pass exceeds `SPIRA_SENTINEL_PASS_TARGET_SECS`** (default 60),
  the positive control on §5's budget.
- **B8. `FATAL` exit 1 when lib.sh or the probe cannot be found or fails.** The bash script
  could not start without lib.sh either. This names the reason.
- **B9. A ready read that failed renders `plan_ready=?` in the `state:` line**, never `0`
  (a failed probe renders `?`). The bash `ready_count` printed `0` on failure. auron-classify
  and cockpit-metrics match `plan_ready=(\d+)`, so they skip that one pass's line rather than
  read a false zero, and CHECK 3 and CHECK 8 do not fire on it (G3).
- **B11. The lifecycle switch (§2.9).** With `lifecycle_enforce` OFF, CHECK 2, 2c and 4
  return to the bd records. The protect label, `bd reclaim`, orphan release and the poison
  label all do real work again; since sp-i2m7y they had silently done nothing, because
  spira-lc is not deployed. With it ON, the unreachable machine fails the unit instead of
  logging `WARN` and moving on.
- **B12. CHECK 3c is Rust, and reads no bead store per candidate (sp-du8bv).** lib.sh
  `mark_open_children` and `bead_has_open_children` are deleted.
  `test-dispatch-open-children.sh` drives `sentinel --open-children`. The candidate set, the
  decision and the log lines are unchanged. The children are read from the pass-start
  snapshot rather than live, so a child created mid-pass is seen by the next pass. That is
  under a minute now, where the old live read landed 3 to 6 minutes after the snapshot.
- **B13. Re-read before every write (G11, sp-du8bv).** A CHECK 4 poison or ask is also
  skipped when the re-read fails. It used to go ahead on the snapshot row with "could not
  read the bead" as its evidence. CHECK 2c no longer strips a claim taken after the snapshot.
- **B10. The Sending's "every swept repo is stamped" test matches `<name>=` at the start of
  a stamp line.** The bash `grep -qF "<name>="` matched it anywhere, so `a=` was satisfied
  by `ba=…`.

**Pre-existing defects, not fixed here and not caused by the split:**

- `unpoison.sh:145`, `cockpit.sh:626` (poisoned ACT) and cockpit-metrics.py's
  SENT/HELD/KEEP/FAILED all look in `sentinel.log` for lines the audit worker writes to
  `audit.log`. Cutover row 18 fixes the first. The other two want a decision: read both
  logs, or have the audit unit append to sentinel.log.

## 10. Decisions — the goal is retired (sp-k6m1m)

The operator's answer to sp-2f9sa: Spira has no single goal bead; it works the backlog
continuously. What the pass keeps, and what it drops:

- **Kept: the backlog count.** `open=` is now every open plan bead (`plan_open`), not the
  goal epic's direct children. CHECK 3 and CHECK 8 reason about it exactly as before
  (`plan_ready == 0 && plan_inprog == 0 && open > 0` → recompute blocked / judgement), so a
  plan bead parented nowhere — or under any epic — can now starve the plan and be judged. The
  goal-children set was the wrong question for that already (lib.sh's own comment said so,
  and `dispatchable_open` exists because of it).
- **Dropped: `goal reached`.** A continuous backlog is never finished. The pass used to skip
  CHECK 8 after `goal reached`; CHECK 8 does nothing with `open == 0` anyway, so the only
  visible change is that the phase row `CHECK8` is written on every complete pass and the
  `pass complete` line never carries `, goal reached`.
- **Dropped: `GOAL UNRESOLVABLE`.** Nothing is configured that could fail to resolve.
- **Changed: the state line.** `state: goal=<g> open=…` → `state: open=…`. strand's
  pass-start marker moves with it (`: state: open=`, `strand::probe::PASS_MARKER`), and the
  two Python readers of rotated logs accept one leading `<field>=<value>` so a tail that
  still holds old passes parses.
- **Changed: `--report`.** Prints `Open plan beads:` and the backlog ids.
- **Bounded: what judgement is shown.** reflect.sh runs one `bd show` per id it is given.
  A goal's children were a handful; the backlog is hundreds. CHECK 8 passes the first
  `REFLECT_IDS` (25) and its `STARVED — <n> open` line still carries the full count. Note
  also that production's goal (`sp-spira`) never existed, so `open` was always 0 there and
  CHECK 3's recompute and CHECK 8's judgement never fired; with the backlog they can, still
  rate-limited by `SPIRA_INFERENCE_EVERY`.
- **Moved: the id prefix.** conf.sh derived `SPIRA_ID_PREFIX` from the goal's id. It is now
  the required `[spira] id_prefix` key: `spira-config validate` — what doctor and the
  release's pre-activate run against the config in force — refuses a `[spira]` table without
  one, so a release cannot be activated over it (fail closed).
