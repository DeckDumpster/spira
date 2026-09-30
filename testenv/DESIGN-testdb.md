# `testenv testdb` — design

The server-mode test database: one private Dolt `sql-server` per fixture, started from a
pre-initialised template. Bead **sp-v2lqd** (epic sp-8m1at, design
`gate-unit-round-integration-2026-09-29`, item 1). This document is the contract; it was
written before the code, from the fault's evidence and from every caller of
`spira/testdb.sh`, not by porting the script.

## 1. Intent

**A suite that needs a real `bd` against a Dolt server gets a server of its own.** No
fixture shares a server process, a data directory, a lock or a CPU budget with any other
fixture, so a full corpus at maxpar 16 is bound by CPU, memory and IO — never by the test
database (the operator's rule: every stage is CPU/memory/IO bound, never Dolt-bound).

### 1.1 The fault this replaces (evidence)

In every full run on 2026-09-29 (cert-batch-rust-074901, round-w1-111703, round-w2-125237)
5–9 of the 19 server-mode suites hit their 600 s timeout in one window, and the ones that
passed took 450–580 s against 72–406 s in isolation. The server-mode path of `testdb.sh`
routed every such suite in a batch container to **one** `dolt-beads-test.service`:

* that unit runs under `CPUQuota=50%` and `Nice=10`. Sampled inside a batch container
  running the 19 server suites plus 31 others at maxpar 16 (2026-09-29, 17:22Z): the
  server's cgroup was throttled in **1,898 of 1,902** CFS periods (99.8%), 280 s of
  throttled time in 190 s of wall, `cpu.max 50000 100000`, while the 32-core host's load
  average stood at ~11. Nineteen suites queued on half a core while thirty cores idled.
* `bd init --server` for each fixture ran under an `flock` on
  `$SPIRA_TESTDB_DATA/.server-init.lock`, because schema DDL on one server takes a global
  lock; every init waited for every earlier one.
* a fixture's reset issued `CALL DOLT_RESET` plus three `DELETE`s against the same starved
  server — the "stopped right after `OK, 0 rows affected`" signature.

Raising the quota, the timeout or the connection limit would move the knee, not remove it:
the design shared one server among fixtures that never need to see each other.

## 2. Contract

`testenv testdb <cmd>` — dispatched when the first argument is exactly `testdb` (as
`testenv suites`). Every command prints `KEY=value` lines on stdout (only on success) and
diagnostics on stderr; exit 0 success, 1 failure, 2 usage.

| command | does |
|---|---|
| `template --bd B --dolt D [--root R]` | ensure the template for (B, D) exists; print `TESTDB_TEMPLATE=` |
| `up --tag T --bd B --dolt D [--root R] [--owner PID]` | a fresh fixture from the template; print `TESTDB_NAME TESTDB_DIR TESTDB_FIXTURE TESTDB_SERVER_PORT TESTDB_SERVER_PID TESTDB_UP_MS TESTDB_BD` (`TESTDB_BD`: B as an absolute path, the first on PATH, links not resolved — §2.4) |
| `reset --fixture F` | back to exactly the template state; print `TESTDB_SERVER_PORT TESTDB_SERVER_PID TESTDB_RESET_MS` |
| `down --fixture F` | stop the server, remove the fixture; print `TESTDB_SERVER_CPU_MS TESTDB_LIFE_MS TESTDB_RESETS` |
| `reap --fixture F --owner PID` | internal: the watchdog `up` detaches |

`R` defaults to `$TESTDB_ROOT`, else `/var/tmp/spira-testdb`. It must have **no `.dolt`
ancestor** (bd refuses to init under one), which is why it is not `$SPIRA_TESTDB_DATA`.

**Inside testenv `R` is `/tmp/spira-testdb`, a tmpfs (sp-t26yx, §2.5).** testenv sets
`TESTDB_ROOT` and `TESTDB_REQUIRE_TMPFS=1` for the template build and in every server-mode
suite's environment; with `TESTDB_REQUIRE_TMPFS=1`, `template` and `up` refuse a root whose
nearest existing ancestor is not a tmpfs (`statfs` magic `0x01021994`), so a throwaway
database can never silently fsync to the host disk again. A refused template is the
existing embedded fallback, logged.

### 2.1 Template

`R/template-<key>/` where `key` = the first 16 hex of sha256 over the canonical path, size
and mtime of the `bd` and `dolt` executables — a new binary gets a new template, and a
stub `bd` (test-testdb-failsafe) never finds a real one's. Built once, under an exclusive
`flock` on `R/template-<key>.lock`, into a temporary sibling that is renamed into place
when complete, so a reader sees a whole template or none:

```
template-<key>/
  data/            # dolt data_dir holding database `sptest` (server stopped cleanly)
  ws/.beads/       # bd's workspace after `bd init --server --database sptest --external`
  READY
```

Build: start a private server on a free port, `bd init --non-interactive --prefix sp
--skip-agents --skip-hooks --server --server-host 127.0.0.1 --server-port P --database
sptest --external -q` under `env -i PATH HOME TERM=dumb BD_NON_INTERACTIVE=1` (the same
clean environment `testdb.sh` used), then SIGTERM and wait — Dolt writes its working set on
SIGTERM, so the copied store is clean. A failed build removes its temporary directory and
reports bd's output.

`testenv` itself builds the template during batch setup when any selected suite carries a
`# testdb-mode: server` line (the header `testdb-mode-lint.sh` already requires of every
server-mode suite), so no suite waits on the one-time build.

### 2.2 Fixture

```
R/fx-<name>/                 name = sptest_<tag>_<epoch>_<pid>
  ws/                        TESTDB_DIR: bd's workspace (.beads copied from the template)
  data/                      a copy of the template's data/ plus config.yaml
  server.pid  server.port  server.log  template  resets  started_ms
```

* **Port.** Bind `127.0.0.1:0`, read the port, close, write `data/config.yaml` and the
  port into `ws/.beads/metadata.json` (`dolt_server_port`) and `ws/.beads/dolt-server.port`
  (bd 1.2 reads the latter first). If the server dies before it accepts (someone took the
  port in the gap), pick another; five attempts.
* **Server.** `dolt sql-server --config data/config.yaml` in `data/`, in its own session
  (`setsid`), stdin `/dev/null`, stdout and stderr to `server.log`, every inherited fd ≥ 3
  closed — a server holding the caller's `$(…)` pipe or a suite's lock fd hangs the caller.
  No CPU quota and no nice: it runs in the suite's own cgroup, under the batch's.
* **Ready** means the server sends its MySQL protocol-10 greeting (first packet, sequence 0,
  payload byte `0x0a`) on a fresh connection while the recorded pid is still alive, within
  30 s; each attempt's connect and read are bounded (200 ms, 500 ms). A bare TCP connect is
  not readiness: the kernel completes the handshake for a listening socket before the
  server accepts (sp-t26yx).
* **Process identity.** A pid is ours only while `/proc/<pid>/cmdline` still names
  `sql-server` and this fixture's `config.yaml`; a zombie (`State Z`) counts as gone. We
  never signal a pid we cannot so identify (never kill a derived pid).
* **Reset** = stop the server, replace `data/` and `ws/.beads/` with fresh template copies,
  start again (same port when free). This is exactly a fresh fixture, including the tables
  `DOLT_RESET` does not touch (`events`, `bd_events_journal`, `leases` are dolt-ignored),
  measured at ~150 ms against ~2 s for the SQL reset it replaces.
* **Down** writes `DOWN`, stops the server (SIGTERM, 10 s, then SIGKILL), removes the
  fixture. Idempotent; a missing fixture is success.
* **Reap.** Unless `--owner 0`, `up` detaches `testenv testdb reap` (own session, null
  stdio, fds closed). It polls once a second; when the owner pid is gone (a suite killed by
  its timeout never runs its trap) it runs `down`; when the fixture directory is gone it
  exits. Leaked servers therefore cannot accumulate on a host.

### 2.3 `testdb.sh` (call-site shape only)

Server mode in `testdb_up` becomes one call —
`eval "$(testenv testdb up --tag … --bd "$TESTDB_SERVER_BD" --dolt … --owner "${TESTDB_OWNER_PID:-$$}")"`
— and `testdb_reset`/`testdb_drop` call `reset`/`down`. The binary is resolved with
`spira_bin testenv` (the artifact under test inside testenv, `$SPIRA_REPO/bin` otherwise).
Gone: `testdb_server_ensure`, the init lock, the init hash, `DOLT_PURGE_DROPPED_DATABASES`,
`DROP DATABASE`, and the shared-server borrower branch (a borrower now builds a private
fixture in ~0.2 s instead of resetting one database that other borrowers were using).
`dolt-beads-test.service` is no longer started by `testdb.sh`; its unit is left installed
for the operator's own use and is not changed here.

### 2.4 Every suite gets a server fixture (sp-34ru2)

**Intent.** A suite's `bd` call costs what the call does, not what opening the store
costs. The store is opened **once per fixture** (by its private server), never once per
`bd` call.

**The misuse this removes (evidence, 2026-09-29).** 120 of the 140 suites that call
`testdb_up` ran **embedded** Dolt: every `bd` invocation opened the Dolt engine in-process,
replayed the store's chunk journal and released it. Measured on a throwaway store (bd 1.2.1,
32-core host):

* `bd --version` (Go runtime start of the 200 MB binary, no store): ~85 ms.
* embedded `bd list` on a fresh store: 330–450 ms; `create` 500–550 ms. A CPU profile
  (`bd --cpu-profile`, 7 runs aggregated) puts 46 % of the process's CPU in
  `embeddeddolt.OpenSQL` → `nbs.newChunkJournal` → `processJournalRecords` plus the GC it
  drives; strace shows the store's `LOCK` taken and its manifest rewritten 6 times per
  `list`. The replay is O(journal): the fresh store's journal is 2.2 MB, and 60 creates
  grow it to 4.3 MB and `list` from 391 to 504 ms — a suite's calls get slower as it runs.
* the same calls against a private server fixture (§2.2): `list` 125–190 ms, `create`
  260–320 ms.
* bd-meter in testenv (six bd-heavy suites, parallel): 516 calls, 181.8 s of 298 s suite
  wall (61 %), 262–401 ms per call.

**Contract.** When the template for the artifact under test builds, testenv builds **no**
embedded baseline and every suite runs with `SPIRA_TESTDB_MODE=server` (and
`TESTDB_SHARED=0`), so each `testdb_up` takes the §2.3 server path: a private server from the
template in ~0.1 s. A suite's own settings still win (a suite that unsets or overrides the
variable, or a sub-run under `env -i`, is untouched). When the template does not build,
testenv logs it and falls back to the embedded shared baseline exactly as before — the
fixture tier degrades, it never fails a batch.

**The template key sees through the bd meter.** Inside a parallel suite `bd` resolves to
the bd meter (bdmeter.rs), whose canonical path is `bd-meter`; `Tools::resolve` resolves a
`bd` that is the meter to the real `bd` behind it on PATH, so the template a suite asks for
is the one testenv built during setup (not a second 4 s build under the template lock, keyed
on the meter's mtime).

**The bd path is absolute.** conf.sh caches its `bd migrate schema` check in
`$SPIRA_RUN/bd-schema-stamp`, keyed on `stat "$SPIRA_BD"`. `testdb_up` exported a bare
`SPIRA_BD=bd` (server) or `bd-embedded` (shared baseline), which never stats, so **every**
conf.sh source by a script the code under test spawns (landing-pass → `skew.sh refresh`,
`systemd/unit-ensure.sh`, `watchd.sh manifest|units`, …) re-ran the check: 123 of
test-certify's 231 bd calls were `migrate schema`. `up` now prints `TESTDB_BD`, the absolute
path of B as found on the caller's PATH **without** resolving links (inside a metered suite
it is the meter's link, so calls stay metered), and `testdb.sh` exports it as `SPIRA_BD`
(call-site: it reads the value, as it reads `TESTDB_DIR`).

**The server path finds testenv however a suite repoints.** `testdb.sh` resolves the binary
with `spira_bin testenv`, which a suite that points `SPIRA_REPO`/`SPIRA_HOME` at a fixture
tree cannot satisfy; the server tier therefore exports `TESTDB_TESTENV=<artifacts>/testenv`,
which `_testdb_testenv` already prefers. Three suites that test the embedded path itself
(`test-testdb-failsafe`, `test-testdb-concurrent`, and `test-beads-push-commit`, whose remote
is written and read through the embedded library) unset `SPIRA_TESTDB_MODE`.

**Measured (testenv, maxpar 8, same host, 2026-09-29).** The six bd-heavy suites (certify,
auron, landing-red-recurring, closed-strand, bead-dep-add, cockpit-funnel), three runs of
`local/main` against three of this branch: bd calls 516 → 346; bd wait 138–182 s → 74–81 s;
per call 267–352 ms → 213–235 ms; share of suite wall 52–61 % → 35–41 %. The 146 suites that
use testdb: 6,761 → 5,251 calls, 1,996 s → 1,107 s of bd, share 41.6 % → 24.2 %, batch wall
664 s → 612 s, cgroup peak 3,659 → 3,725 MiB — and 13 server-mode suites that `local/main`
SKIPPED run again (their template key was the meter's, and the meter cannot `init`).

**Floor.** What remains per call is the `bd` process itself: Go start (~85 ms CPU) and the
six `git` children bd runs to discover a repository and its role (~7 ms each), plus the
query. That is the floor for any suite that drives `bd` as a CLI; the remaining lever is the
number of calls (the harness binaries under test call `bd` per bead per pass), not the store.
Measured inside the testenv image, idle: `bd --version` 81 ms (Go package init alone is
63 ms over 689 packages, `GODEBUG=inittrace=1`), a server read 125 ms, a server write
206 ms, against embedded 254–287 ms and 408 ms. For the six suites (346 calls, ~106 s of
non-bd wall) the idle floor is ~48 s of bd, a share of ~31 %; 20 % would need ≤ 75 ms per
call, below `bd --version` itself.

### 2.5 Test databases never touch the host disk (sp-t26yx)

**The fault.** Under concurrent gate load (2026-09-30, sp-9thdw) the template build failed
`bd init (server) → [mysql] … i/o timeout / failed to create database: invalid connection`,
and the embedded fallback then overran the setup share. It is **not** a readiness race, a
port collision or an fd limit: `packets.go:58 read` is a read on an *established* connection
— bd's CREATE DATABASE — and bd's pool read timeout is 10 s. Dolt's CREATE DATABASE fsyncs
its new store; with `R` on the container overlay (`/var/tmp`, the host's ext4 on a QEMU
virtual disk at 66–99 % utilisation, IO pressure `full` 30–70 %), each fsync queued behind
the host's writeback.

**Measured, same host, same minutes** (`testenv testdb template`, real bd 1.2.1 and dolt
2.2.3, alternating roots, IO pressure `full` avg10 16–39 %):

| root | runs | failed (`i/o timeout`) | wall |
|---|---|---|---|
| ext4 (`/var/tmp`) | 4 | 4 | 15.6–23.2 s |
| tmpfs (`/tmp`) | 4 | 0 | 4.9–5.5 s |

**Decision.** Durability of a database that is deleted at the end of its suite is pure cost,
so it is not bought: `R` is on the container's tmpfs (`podman run --systemd=true` mounts
`/tmp` as tmpfs). The fixture is CPU/memory bound; the host disk is out of its path.
*Rejected:* raising bd's read timeout (`BEADS_DOLT_POOL_READ_TIMEOUT`) or the setup share —
both move the knee and keep the database IO-bound; a shared long-lived server — the
isolation §1 exists for.

## 3. Non-goals

* ~~Embedded mode is untouched~~ — superseded by §2.4: embedded remains the fallback when
  the template cannot build, and outside testenv.
* ~~Porting `testdb_seed` or the embedded path to Rust~~ — superseded by §6 (sp-k2oyn): the
  embedded fixture lifecycle (init, baseline, borrow, reset, drop) moved. `testdb_seed`
  itself did not: it is one external call (`bd import`) with a tempfile around it, no
  branching logic to port, and stays in `testdb.sh`.
* Changing any suite. The 19 server-mode suites keep `SPIRA_TESTDB_MODE=server`.
* A `--root`/tmpfs contract for embedded fixtures like §2.5's for the template. Each
  embedded fixture is already its own directory under the OS temp dir (`std::env::temp_dir()`,
  the same default `mktemp -d` used), never a shared root threaded through a template key —
  there is no shared state for a disk-backed root to corrupt under load.

## 4. Decisions

* **D1 — a server per fixture, not a pool or a bigger shared server.** Any shared server
  keeps a global DDL lock and one CPU budget in the path; the cost of isolation is one
  ~100 MB `dolt` process per running server-mode suite (≤ 16 at maxpar 16).
* **D2 — template copy, not `bd init` per fixture.** An init is ~4 s of server-side DDL;
  a 2 MB copy plus server start is ~120 ms.
* **D3 — reset by restart, not SQL.** Exact, faster, and needs no knowledge of which
  tables Dolt versions.
* **D4 — the owner watchdog, not a trap.** Suites are killed by `timeout`, and a killed
  shell runs no trap.
* **D6 — server fixtures for every suite, not a faster embedded store (sp-34ru2).**
  Compacting the embedded store (`dolt gc`: 2.2 MB journal → 0.4 MB archive) cuts `list` by
  a third at best and a suite's own writes regrow the journal; `--dolt-auto-commit off` saves
  ~10 % of a write and changes what a suite observes. Only a store opened once per fixture
  removes the per-call open. Production runs server mode too, so the fixtures now match it.
* **D5 — a subcommand of testenv, not a new crate:** testenv owns the fixture tier and is
  already resolved in every place a suite runs (`FLOOR` of `--artifacts`, `make install`).
* **D7 — `testdb.sh` keeps its function names and shell-variable contract (sp-k2oyn).**
  ~148 suites source it and call `testdb_up`/`testdb_reset`/`testdb_seed`/`testdb_drop`
  inline, because only sourcing can export `SPIRA_DB`/`SPIRA_BD`/`PATH` into the calling
  shell — a subprocess cannot. So the file is not deleted; every caller of the public API
  needed zero changes for this bead, the same shape §2.3 already established for server
  mode. What moved is what happens *behind* `testdb_up`, not how a suite asks for it.
* **D8 — the embedded PATH shim and the borrower's private copy are real logic, `testdb_seed`
  is not.** Building a fixture (`bd init`, snapshot, symlink), borrowing one (copy and
  validate a baseline) and resetting one (the rename dance, sp-i0vz5) all branch and can
  fail in ways worth a unit test. `testdb_seed` is `bd import` on a tempfile with no
  decision in it; porting it would just add a process hop.

## 5. Tests

Unit (`cargo test -p testenv testdb`): template key changes with either binary; config
rendering; port rewrite of `metadata.json` and `dolt-server.port`; cmdline identity
(ours / another fixture's / not dolt / zombie); argument parsing; the KEY=value report;
and, when `dolt` and `bd` are on PATH, a real lifecycle — template once under
concurrency, two fixtures isolated from each other, reset clearing a created issue, reap
after the owner dies, down removing everything. Embedded (§6): reset restores the baseline
and drops extra state, and is a no-op-tolerant swap when `.beads` is already gone; reset
and borrow both refuse a baseline with no `.beads` and leave the live fixture untouched;
two borrowers of one baseline never share a directory or see each other's writes; down
removes every path it is given and tolerates an empty or already-gone one; a failed
`embedded-check` leaves no fixture directory behind; and, when a real `bd` with the
embedded engine is on PATH, a live up → write → reset → down round trip. Suites: the 19
server-mode suites, `test-testdb-*` (concurrent borrowers get distinct `SPIRA_DB`s, the
fail-safe unset on a forced fixture-fault), and every other suite that sources `testdb.sh`
exercise it through the unchanged bash API.

## 6. Embedded mode moves behind testenv (sp-k2oyn)

**Intent.** §1's fault was specific to server mode's shared Dolt server; embedded mode
never shared a server. What it did carry, entirely in bash, was real logic worth the same
treatment as §2's fixture lifecycle: a `bd init` with failure diagnostics, a baseline
snapshot so `reset` never re-pays init's ~6 s, a PATH shim so a suite's bare `bd` calls
reach the binary the fixture was built with, a borrower's private copy of a shared
baseline, and a rename-based reset that avoids an overlay2 hazard (sp-i0vz5). Moving it
behind `testenv testdb` gives it the same unit-test coverage §5 gives the server path,
and turns `testdb.sh` into what §2.3 already made it for server mode: a call-site shim.

**Contract.** Five more `testenv testdb` commands, same KEY=value / stderr-diagnostics /
exit-code convention as §2:

| command | does |
|---|---|
| `embedded-check --bd B` | throwaway `bd init`; exit 0 if it works, 1 otherwise (no output) |
| `embedded-up --tag T --bd B` | `bd init` into a fresh dir, baseline snapshot, `bd` PATH shim; print `TESTDB_NAME TESTDB_DIR TESTDB_BASELINE TESTDB_BIN TESTDB_BD` (`TESTDB_BIN` empty if the shim could not be built) |
| `embedded-borrow --baseline BASE` | a private copy of `BASE/.beads`; print `TESTDB_PRIVATE_DIR` |
| `embedded-reset --dir D --baseline BASE` | swap `D/.beads` for a fresh copy of `BASE/.beads` |
| `embedded-down [--dir D] [--baseline BASE] [--bin BIN]` | detach-and-remove each given path; every path optional, a missing or empty one is a no-op, always exits 0 |

None of these take `--root`: an embedded fixture is its own directory under the OS temp
dir (`std::env::temp_dir()`), never a member of a shared template tree, so there is
nothing for a `--root` to name.

**`testdb.sh` (call-site shape only, mirroring §2.3).** `_testdb_embedded_check` calls
`embedded-check` and keeps its own session-level memoisation (the probe is slow; the cache
is a call-site concern, not fixture logic). `testdb_up`'s shared-fixture branch calls
`embedded-borrow`; its fresh-fixture branch calls `embedded-up`; `testdb_reset`'s embedded
branch calls `embedded-reset`; `testdb_drop`'s generic cleanup loop (`TESTDB_DIR`,
`TESTDB_BASELINE`, `TESTDB_BIN` — by the time it runs in server mode these are already
empty, so in practice this was always the embedded path) calls `embedded-down`. Every
failure mode a suite could observe is unchanged: a gone or empty baseline still exits
`TESTDB_FAULT_EXIT` (default 75); a failed init still clears `TESTDB_NAME`/`TESTDB_DIR`
and returns 1; `testdb_up` still unsets `SPIRA_DB`/`SPIRA_BD` before any of this runs
(testdb.sh's own fail-safe, unchanged — DESIGN-testdb.md never owned that half).

**The detach moves from a bash `&` to testenv (D9).** `testdb_drop`'s cleanup used to
`mv` each leftover aside and background `rm -rf … &` in the calling shell, so a slow
overlay2 delete (~19 s observed) never blocked past a Podman exec timeout. `embedded-down`
does the same rename, then spawns the `rm -rf` as its own detached session (the same
`detach()` server mode already uses for `reap`) before returning — the property survives
moving the rename out of the calling shell.

**Gone: nothing.** No caller changed. The public API (`testdb_require`, `testdb_up`,
`testdb_reset`, `testdb_seed`, `testdb_drop`, `testdb_available`) is exactly what it was;
every one of the ~148 sourcing suites needed no edit.
