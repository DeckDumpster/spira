# Test plan — Cockpit telemetry and operator views (`cockpit-observability`)

> **2026-09-25: `test-snap-stale-threshold.sh` deleted** (law-a-test-that-flips-is-deleted): same tree green in round 6, red twice after. The watchtower/doctor snapshot-staleness threshold has no coverage until a deterministic test replaces it.

Part of [[test-plan-2026-09-23]], section 5. Area id `cockpit-observability`; use-case ids are `UC-cockpit-observability-NN`.

**Scope:** 67 primary records: 59 bash suites, 7 Rust test sources, and 1 dead fixture. There is 1 secondary suite, `test-statute-projection.sh`.
**Current cost:** 480 suite-seconds on main-push run 35947142904. `test-loom-ops.sh` did not run. The Rust files are paid for inside `test-panel.sh`, `test-pane-fyi.sh` and `test-loom.sh`.
**Projected cost:** about 188 suite-seconds (arithmetic in §7).

Most of the cost comes from four habits, not from the checks themselves:

- **Full-snapshot runs to read one or two keys.** 20+ suites run `cockpit.sh once`, a 2,770-line script that runs every probe, just to read those keys.
- **An embedded-Dolt store where a JSON list would do.** 12 suites build one (`testdb_up`) to feed logic that is really a pure function over a list of bead JSON rows.
- **Wall-clock timeouts** around loops that could take an injected tick.
- **Cargo compiled several times** inside bash shims that then report 120 Rust tests as one line.

---

## 1. Intent

The cockpit gives the operator a view of Spira that is **read-only and honest**. The tests below are the de facto specification.

- **Snapshot.** A supervised collector (`spira/cockpit.sh` sections fanned out as tiered probes by `spira/collect.sh`) publishes one atomic `cockpit.env` of `SP_*` keys.
  - Every key that could not be read is `?` or absent, never `0`.
  - An empty-but-readable source is a real `0`.
  - A failed or killed probe keeps its last-good value and records its status.
- **Rendering.** Renderers turn the snapshot into operator surfaces without inventing values:
  - `cockpit/health.sh` (the pane)
  - the Rust `cockpit/panel` (decisions, FYI and alerts)
  - Loom (`/api/beads`, `/api/ops`, `model.js`)
  - `ready.sh` (the readiness verdict)
  - `ctx-meter.sh` (status line, context and rate-limit meter)
  - `tokens.sh` and `model-switch-report.sh` (spend attribution)

  Every renderer must fit its geometry and propagate `?`.
- **Layout.** The tmux layout (`cockpit/layout.sh`, `cockpit/rebuild.sh`) must:
  - self-heal from a timer without stealing focus or killing the session pane;
  - rebuild from a dead server into linked windows;
  - pin its config, and exit so it is restarted when that config changes.
- **Boundary.** No view changes bead state. The exceptions are the panel's explicit operator acts and `resolve.sh`, and those must surface `bd` failures verbatim.

## 2. Use cases

**Dimensions:**

- COR: correctness
- FC: fail-closed / honest unknown
- OBS: observability
- IDM: idempotency
- CON: concurrency
- REC: recovery
- CFG: config-compat
- CTR: seam contract
- PERF: performance
- HYG: test integrity

**Where tests run:**

- cert: certification, local and every commit
- bCI: batch CI
- mCI: main CI
- acc: acceptance

### A. Collector and snapshot (cockpit.sh / collect.sh)

| ID | Requirement | Dims | Tier / where |
|---|---|---|---|
| UC-cockpit-observability-01 | The collector writes `cockpit.env` only when it is the supervised unit (INVOCATION_ID matches) or `SPIRA_COCKPIT_FORCE=1`, which is recorded as `SP_WRITER=force`. Otherwise it prints keys to stdout. `cockpit.sh loop` and `collect.sh loop` refuse to run unsupervised. | FC, CON, CTR | T1 cert |
| UC-cockpit-observability-02 | `SP_AT` is stamped before any section data. `SP_WINDOW_HOURS`, `SP_PASS_SECS` and `SP_COLLECTOR_REV` are always emitted. | CTR, OBS | T1 cert |
| UC-cockpit-observability-03 | Every probe that a reader depends on is registered in `collect.sh PROBES`. | CTR | T0 cert |
| UC-cockpit-observability-04 | Fragment merge:<br>• an `ok` fragment contributes its keys;<br>• `never`/`timeout`/`error` contribute only `_PROBE_AT_<n>` and `_PROBE_STATUS_<n>`;<br>• `stale` keeps its last-known-good values;<br>• on a key clash the alphabetically first fragment wins. | COR, FC, CTR | T1 cert, against the real `collect.sh merge` |
| UC-cockpit-observability-05 | A probe that times out:<br>• writes `status=timeout`;<br>• logs its name;<br>• increments `_PROBE_KILLED`, which is surfaced as `SP_PROBE_KILLED_<n>`;<br>• leaves no temp file.<br>A success resets the counter and logs `probe <n> ok <N>s`. | REC, OBS | T1 cert, with a COCK seam and a 1 s timeout |
| UC-cockpit-observability-06 | A collector pass killed by TERM, INT or HUP removes its `.cockpit.*` temp, re-raises (exits 128+signo), and never disturbs the published `cockpit.env`. Orphan temps are swept at startup, for both `cockpit.sh` and `collect.sh`. | REC, CON | T2 bCI, hermetic probe seam |
| UC-cockpit-observability-07 | The collector loop exits 1 after N consecutive merge failures. It exits 0 with `config changed` when the `SPIRA_CONF` mtime changes, and otherwise keeps running. The same contract holds for `health.sh` and `loom.sh`. | REC, CFG | T1 cert, injected tick |
| UC-cockpit-observability-08 | Every bd-backed count is `?` when `bd` exits non-zero, exits 0 with empty output, prints a schema-mismatch refusal on exit 0, or the persona set does not resolve. An empty readable store yields a numeric `0`. Covers `SP_READY`, `SP_NEXT_N`, `SP_WAITING`, `SP_INFLOW_N`, `SP_CLOSED`, `SP_LANDED` and `SP_UNLANDED_N`. | FC | T1 cert (stub bd) + one T2 real-bd row per query (bCI) |
| UC-cockpit-observability-09 | Reachability: open work reachable from ready seeds through `blocks` edges counts as `SP_REACHABLE`. Beads behind a needs-ryan ask or a `spira-poison` stopper count as `SP_STRANDED`. Asks and insights are excluded, `relates-to` does not block, and the scope label is required. | COR, FC | T1 cert |
| UC-cockpit-observability-10 | Landing classification: a closed bead is LANDED only when a commit **subject** on `origin/<base>` names it. A body mention does not count. This holds for main- and master-based repos. A closed bead with a branch and no landstate is an anomaly; one with neither is not. `SP_CLOSED` covers the last 24 h. | COR, CFG | T2 bCI (real git, stub bd) |
| UC-cockpit-observability-11 | Queue: `SP_QUEUE_DEPTH` counts CERTIFIED beads. The open batch yields PR, age and members, including still-open beads. Certified beads that are not batched appear as NEXT, in batcher order: priority, then certification epoch. Quarantined suites are counted. | COR, OBS | T1 cert (files + JSON) |
| UC-cockpit-observability-12 | Unsent and refs, across all repos:<br>• `SP_BRANCH_DONE` and `SP_UNSENT` count only bead-backed branches;<br>• a no-bead branch ahead of base is `SP_ORPHAN_WORK`, and one with its tip on base is `SP_UNADOPTED` (named);<br>• `spira/queue/*` and `spira-suite-state/*` are protected;<br>• BATCHED stranded or too-long entries are counted and named;<br>• a failed bead lookup counts in `SP_PROBE_FAIL` and never feeds `SP_UNADOPTED`. | COR, FC | T2 bCI (real git, stub bd) |
| UC-cockpit-observability-13 | Hygiene meters:<br>• `SP_DUP_REFS`/`SP_DUP_BEADS` count external_refs shared by more than one incident bead within the 7-day lookback;<br>• `SP_REPO_UNMAPPED`/`SP_REPO_ABSENT` are counted over non-closed beads;<br>• `SP_POISON` counts every non-closed `spira-poison` bead whatever its plan/incident label.<br>An unreadable store or map gives `?`. | COR, FC | T1 cert + T2 contract row |
| UC-cockpit-observability-14 | SOP and sweep:<br>• never-fired and recurred counts respect the window, `held` and `check=fail`;<br>• `SP_SWEEP_AGE` is the age of the newest applied-ledger entry;<br>• a missing, empty or unreadable ledger, or a broken shelf, gives `?`. | COR, FC | T1 cert |
| UC-cockpit-observability-15 | Liveness:<br>• a gate is live only when its pid is alive **and** its cmdline is the gate (a reused pid does not count);<br>• per-aeon FUSE/WALL read 0 on a fresh worktree, `?` with no worktree, and `gate` while its gate runs;<br>• `landing.status` and `landing.progress` are copied verbatim, and a missing file gives `?`. | COR, FC | T2 bCI (real /proc) |
| UC-cockpit-observability-16 | Rate limits and SELF metrics:<br>• `SP_RATELIM_*` comes from the newest `rate_limit_event`, and is `?` with no trace;<br>• SELF metrics are computed over their window: repeated ACT across passes, and stillborn aeons. | COR, FC | T1 cert |
| UC-cockpit-observability-17 | `SP_NEXT` attributes each ready bead to the persona that will claim it, or marks it `unclaimable` when its `fayth:` preference cannot match. | COR, CTR | T1 cert |
| UC-cockpit-observability-18 | The statute probe reports DB/page counts and SKEW as `?` with no wiki, MISMATCH when DB < page/2, and OK otherwise. (Secondary suite; the enact/synth half belongs to operator-channel.) | FC, OBS | T1 cert |
| UC-cockpit-observability-19 | The 24 h BEADS opened/closed/landed counts and 8-bucket sparklines come only from bead JSON and `landing.log`. An unreadable `landing.log` gives `?` for the landed series. | COR, FC | T1 cert |
| UC-cockpit-observability-20 | `spira_event`:<br>• records kind, target, title and detail to `events.log`;<br>• suppresses repeats per (kind, target) within a window anchored at the first emission, then reports `+N more`;<br>• refuses an empty kind or title;<br>• every emitted kind is declared, lowercase-dotted and ≤32 characters. | COR, IDM, CTR | T1 cert (clock seam) + T0 (taxonomy) |

### B. Pane rendering (health.sh)

| ID | Requirement | Dims | Tier / where |
|---|---|---|---|
| UC-cockpit-observability-21 | Every count the pane renders shows `?` when its key is absent or `?`, never `0`. A key missing because of collector skew (`SP_COLLECTOR_REV` set) renders `coll <rev>`. | FC, CTR | T1 cert, table-driven |
| UC-cockpit-observability-22 | The pane fills exactly its height:<br>• NOW is served first;<br>• NEXT and RECENT are capped at 5;<br>• slack goes to INFLOW;<br>• CI keeps its row;<br>• no row exceeds 70/96 columns;<br>• labels align at column 9. | COR, OBS | T1 cert (golden frames) |
| UC-cockpit-observability-23 | NOW rows come from `trace_stats`/`trace_tail`:<br>• turns are distinct message ids;<br>• ctx is the last usage;<br>• only the current attempt counts;<br>• MODEL comes from init, and a mismatch with `FAYTH_MODEL` shows ⚠;<br>• an unreadable trace gives `?` and an empty trace gives `-`;<br>• the live-aeon count comes from pidfiles and /proc. | COR, FC | T1 cert (source functions) + one T2 /proc case |
| UC-cockpit-observability-24 | Banners and badges:<br>• DRAINING (minutes and `world.sh resume`) and STOPPED are independent;<br>• a stale snapshot is STALE (naming the probe and kill count) when its core probe timed out, and FAULT (age) otherwise;<br>• the threshold is `SPIRA_SNAP_STALE_S`, shared with `watchtower.sh` and `doctor.sh`. | OBS, CFG | T1 cert + T2 three-reader contract |
| UC-cockpit-observability-25 | Snapshot location: no snapshot anywhere gives "no snapshot". A snapshot at the XDG alternative while the reader looks elsewhere gives PATH MISMATCH, naming both paths. | OBS, CFG | T1 cert |
| UC-cockpit-observability-26 | The MAIL, QUEUE and LAND sections render their keys: NEW/READ/DONE, `batch #<pr>`, rc and branches, and the lease countdown. Retired labels (UNLND, CTX, SELF, GOV) stay removed. | OBS | T1 cert |

### C. Operator views (panel, Loom, ready, ctx-meter, reports)

| ID | Requirement | Dims | Tier / where |
|---|---|---|---|
| UC-cockpit-observability-27 | Panel frame: exactly `h` rows and at most `w` columns at every geometry, view and mode. There are two bands. Content is never dim. The reply editor wraps losslessly with exactly one caret. Ages are coarse durations taken from an exact epoch/iso pair. | COR | T1 cert (cargo) |
| UC-cockpit-observability-28 | Panel views:<br>• events go only to NOTIFICATIONS;<br>• alerts are selected by label, sorted oldest-first, and self-clear to history;<br>• silences expire at their deadline, and an unparseable deadline silences nothing;<br>• acking does not hide an alert;<br>• a dismissed insight returns only when the operator spoke last;<br>• an optimistic hide ends only when the write is visible;<br>• an empty or errored read is never an all-clear. | COR, FC | T1 cert (cargo) |
| UC-cockpit-observability-29 | Panel affordances:<br>• FYI never offers a verdict;<br>• ALERTS offers ack, silence and history, and never decide;<br>• `p reject` appears only on DECISIONS;<br>• only `ask-*` beads close on reply, while `ask-law`/`ask-suit` are parsed as law.<br>The enact line `<slug>: <statute>` splits on the first colon and does not double the `law-` prefix. | COR, CTR | T1 cert (cargo) |
| UC-cockpit-observability-30 | Panel write paths: close, comment, archive, enact and premise-reject call `bd`/`rule.sh` with the right arguments and actor, and surface failure. | CTR, FC | T2 bCI (stub bd recording argv), **gap** |
| UC-cockpit-observability-31 | Loom `/api/beads` serves only live work:<br>• typed edges are hoisted once, and dangling or closed edges are dropped and counted;<br>• JSON-escaped titles survive;<br>• an advisory preamble before the JSON is tolerated;<br>• two requests within the cache window cost one bd call;<br>• over budget returns 503, and a bd failure returns 502, never an empty graph;<br>• static routes are served and unknown paths return 404. | COR, FC, PERF, CTR | T1 cert (fake bd) + one T2 real-bd shape row |
| UC-cockpit-observability-32 | Loom `/api/ops` decodes `cockpit.env` shell quoting exactly, exposes only `SP_` keys, and leaves absent keys absent. An unreadable file is an error. It reports `halted` and `cockpit_env_age_s`. | COR, FC | T1 cert (ops.rs) + the HTTP route in endpoint.rs |
| UC-cockpit-observability-33 | Loom `model.js`:<br>• derives components, depth and width;<br>• excludes closed, provenance and dangling edges, and collapses duplicates;<br>• honours the configured labels;<br>• groups by repo and epic;<br>• uses a 14-day arrivals flow;<br>• parses real bd JSON.<br>The shipped page carries no embedded payload. | COR, CFG, CTR | T1 cert (node --test) + T2 contract row |
| UC-cockpit-observability-34 | Loom self-maintenance: `layout.sh` rebuilds when the source is newer than the binary or the binary is missing, and restarts a stale service only when the binary is executable. | REC, IDM | T1 cert |
| UC-cockpit-observability-35 | `ready.sh` reports FAIL, `?`, WARN, pass or skip for each check: sentinel timer, halt stamp, DB and dolt starting-port, statutes, ready work, loom latency, agent, snapshot freshness and panes. It exits 1 on any FAIL or `?`. | FC, OBS, CTR | T1 cert (already stub-seamed) |
| UC-cockpit-observability-36 | `ctx-meter.sh` session pointer: the operator's status-line tick owns the pointer. An idle second session cannot steal it until the holder is stale. A stale, deleted or corrupt pointer means "no session". | CON, REC | T1 cert |
| UC-cockpit-observability-37 | `ctx-meter.sh` limits:<br>• burn rate is measured over the span, not pairwise;<br>• a projection appears only if the window fills before it resets;<br>• flat, single-sample, short or 1-point histories project nothing;<br>• a reset is a boundary;<br>• an absent or malformed window renders nothing (never 0%);<br>• the sample file is pruned to 6 h and deduplicated;<br>• env mode prints `-` or `?`;<br>• the line is ≤110 columns. | COR, FC | T1 cert |
| UC-cockpit-observability-38 | `statusline-check.py` classifies settings as ours, other, absent, unreadable, stale (another copy of the meter, including through a /tmp wrapper) or fragile. | COR | T1 cert |
| UC-cockpit-observability-39 | Attribution and reports:<br>• `tokens.sh` attributes turns to aeon, archivist or session, deduplicated by message id;<br>• `model-switch-report.sh` gives $/bead as `$?` (never `$0`) with no cost data;<br>• `released-defects.sh` counts only introductions that reached base before the fix, and gives `?` for an unresolvable repo. | COR, FC | T1 cert (tokens, report); T2 bCI (released-defects, real git) |
| UC-cockpit-observability-40 | `cockpit/resolve.sh` exits non-zero and relays bd's complaint (from stdout or stderr) on a failed close, and prints `resolved` on success. | FC, OBS | T1 cert |

### D. Layout (layout.sh / rebuild.sh / tmux)

| ID | Requirement | Dims | Tier / where |
|---|---|---|---|
| UC-cockpit-observability-41 | Pane identity comes from the process a pane actually runs, not from argv text that merely mentions `health.sh`. `ensure` untags an impostor, logs it, and never kills the session pane. | COR, REC | T1 predicate cert + T2 one tmux case (bCI) |
| UC-cockpit-observability-42 | `up` builds session, mail (below, left) and a full-height health pane. `ensure` respawns a closed mail pane. With no mail client, or one that is not installed, there is no mail pane. | REC, CFG | T2 bCI (shared tmux server) |
| UC-cockpit-observability-43 | `ensure` detaches ghost clients idle longer than `COCKPIT_CLIENT_IDLE_SECS`, keeps the live client's height, and logs the tty. It does not move the operator's active pane when there is nothing to repair. | REC, COR | T1 selector cert + T2 one PTY case |
| UC-cockpit-observability-44 | The health pane command carries `SPIRA_CONF`. In split-checkout mode it runs the `SPIRA_PROD` renderer unless `SPIRA_DEV_RENDERER=1`. Mouse mode is on by default, can be switched off with off/no/0, and never fails the caller. | CFG | T1 cert (command builder, shimmed tmux) |
| UC-cockpit-observability-45 | A copy of `layout.sh` refuses `ensure` and names the installed path. The collector watchdog restarts an (instance-qualified) collector that started before `cockpit.sh` was last promoted. | REC, CON | T1 cert (pinned socket) |
| UC-cockpit-observability-46 | `rebuild.sh`:<br>• from a dead server, creates brain, hunk, chat and cockpit, with the dashboards in `brain:0` and the cockpit windows **linked**;<br>• verifies itself and fails naming the session pane when the brief is missing;<br>• is idempotent against a healthy server (same pid);<br>• reports an over-long socket as unusable. | REC, IDM | T3 mCI (the one end-to-end tmux case) |
| UC-cockpit-observability-47 | `tmux-env.sh` scrubs the Claude session identity from the server's global env. Panes opened afterwards do not inherit it, and a second run is idempotent. | CON, IDM | T2 bCI |
| UC-cockpit-observability-48 | `cockpit-remote`:<br>• the dialer refuses an empty `COCKPIT_HOST`;<br>• a watcher that loses the lock exits non-zero promptly, naming the holder;<br>• `start` delegates to systemd when the unit is enabled (lock holder = MainPID) and forks with setsid otherwise;<br>• the orphan-lock probe names the holder's pid and path. | CON, REC, FC | T2 bCI (real flock, stub systemd) |

## 3. Coverage map

Current cost is the ci_secs from main-push run 35947142904. The Rust sources carry no ci_secs of their own; they are billed to their shim.

| UC | Existing tests (file::case) | Level & cost now | Verdict |
|---|---|---|---|
| 01 | test-cockpit.sh::unsupervised no snapshot, FORCE writes, loop refuses; test-cockpit-tiered-collector.sh::6 collect.sh loop unsupervised | T2, 6 + part of 17 | KEEP test-cockpit.sh, **DEMOTE-TO-T1**: test `cockpit_may_write` and the loop guard as sourced functions, not a full `once`. The tiered::6 refusal stays (it is a different script). |
| 02 | test-cockpit.sh::SP_AT ordering; test-cockpit-tiered-collector.sh::7 cockpit.sh now, ::11 SP_COLLECTOR_REV; test-cockpit-self.sh::pass_secs | T2 | MERGE-INTO test-cockpit.sh (drop the tiered::7 duplicate, which the file itself says is explicitly duplicated) |
| 03 | test-cockpit-collect-probes.sh::PROBES contains queue/statute | T2 file (source grep) | **SOURCE-GREP**: replace with a T0 lint that makes every `*_keys` function in cockpit.sh map to a `PROBES` entry (this would also have caught `czar_triggers`; see §6) |
| 04 | test-cockpit-tiered-collector.sh::1–5, 8a (**on a pasted copy of the merge Python**), ::11/15 (real `collect.sh merge`); test-cockpit-collect-probes.sh::ok/never fragment (same pasted copy) | T2, 17 + 1 | MERGE-INTO test-cockpit-tiered-collector.sh, rewritten to call `collect.sh merge` on fixture fragments; DELETE the pasted copies in both files |
| 05 | test-cockpit-tiered-collector.sh::8b, 8c, 12, 13; test-cockpit-collect-probes.sh::_probe_body_test; test-cockpit-collector-quota.sh::pass log | T2 | KEEP tiered::8b/8c/12/13; DELETE collect-probes::_probe_body_test and quota::pass-log (the regex case subsumes 3 of its 4 assertions) |
| 06 | test-cockpit-tmp.sh::TERM/INT/HUP ×3, published untouched, startup sweep; test-cockpit-tiered-collector.sh::12, 14 | test-cockpit-tmp.sh is T3 **non-hermetic** (11 s, real probe against the ambient conf/DB) | KEEP test-cockpit-tmp.sh but **DEMOTE-TO-T2 hermetic**: add a sleeping probe seam like COCK's plus `env -i`, bringing it to about 3 s |
| 07 | test-cockpit-tiered-collector.sh::9, 10; test-cockpit-conf-change.sh (health.sh); test-loom-conf-change.sh (loom.sh) | T2 timing-bound: 17 (part) + 8 + 6 | **DEMOTE-TO-T1**: one table-driven suite over the three loops, with a sub-second tick (`SPIRA_LOOM_TICK` already exists; add the same to health and collect). Keep 1 s windows. |
| 08 | test-cockpit-probe-fault.sh::collector SP_WAITING/NEXT_N/INFLOW_N/UNLANDED_N; test-cockpit-ready.sh::refusal, persona unresolved; test-cockpit-reachable.sh::refusal; test-cockpit-ready-seeded.sh::empty db 0, seeded 2; test-cockpit-unsent.sh::bd-fail, bd-zero-empty | T2 + T3 (ready-seeded 9 s) | KEEP probe-fault as the collector FC table. test-cockpit-ready.sh: **DEMOTE-TO-T1** (call `core`/`core_detail`, not `once`). test-cockpit-ready-seeded.sh: **MERGE-INTO the new test-cockpit-bd-contract.sh** |
| 09 | test-cockpit-reachable.sh (all 11 cases) | T2 stub-bd, 4 | KEEP (T1-shaped already). Fix: health.sh runs without `env -i`, and the stub ignores its args, so query shape is unverified. Add one argv assertion. |
| 10 | test-cockpit-landed.sh::SP_CLOSED 24h, SP_LANDED, SP_UNLANDED_N; test-cockpit-unlanded.sh::main+master, zero case, pane QUEUE; test-cockpit-queue-section.sh::body mention; test-cockpit-queue.sh::SP_UNLANDED_N | T3 real bd + git: 14 + 15 + parts | **MERGE** landed into test-cockpit-unlanded.sh (keep its main/master pair and add the body-mention case from queue-section). **DEMOTE-TO-T2**: stub bd returns the closed list, git stays real. Call the section, not `once`. |
| 11 | test-cockpit-queue.sh (all); test-cockpit-queue-section.sh (all) | T3 real bd: 9 + 13 | **MERGE** test-cockpit-queue.sh INTO test-cockpit-queue-section.sh, keeping only its unique quarantine and BATCH_AGE rows. Replace embedded Dolt with fixture JSON. Separate priority from epoch in the ordering case. |
| 12 | test-cockpit-unsent.sh (13 cases); test-cockpit-unadopted-queue.sh (5) | T3: 39 + 12 | **MERGE** unadopted-queue INTO test-cockpit-unsent.sh (same `sp-true-stray` fixture). Run `cockpit.sh unsent` rather than 7× `once`, with stub bd and real git. Move `no timeout(1) wraps a shell function` to **T0 (SOURCE-GREP to lint)**. |
| 13 | test-cockpit-dup-refs.sh (6); test-cockpit-repo-labels.sh (8); test-cockpit-sphere.sh (5) | T3 real bd: 7 + 6 + 7 | **DEMOTE-TO-T1** each, using fixture JSON through a stub `bdjson`. Keep one real-bd row per query (label filter, `--all`, lookback) in test-cockpit-bd-contract.sh. Dup-refs currently rebuilds the store 5× (`testdb_up`). |
| 14 | test-cockpit-sop.sh (9); test-cockpit-sweep.sh (9) | T3 real bd: 4 + 4 | **MERGE** test-cockpit-sweep.sh INTO test-cockpit-sop.sh (identical `run_sops` harness, same broken-shelf and dir-as-ledger fixtures). DEMOTE-TO-T1 with a stub bd. Replace the `<60 s` wall-clock bound with `SPIRA_NOW`. |
| 15 | test-cockpit-gate.sh (8); test-cockpit-fuse.sh (9) | T2: 14 + 5 | test-cockpit-gate.sh: **DEMOTE-TO-T1/T2**. Six full `once` runs against a missing DB are the whole 14 s; call the land/gate section function instead. KEEP fuse, and tighten `grep -q ?` to the exact column. |
| 16 | test-cockpit.sh::ratelim ×2; test-cockpit-self.sh::repeating, stillborn ×7 | T2: 6 + 6 | **DEMOTE-TO-T1**: call the ratelim parser and cockpit-metrics.py functions with a fixed now. Replace the 4-fixture "no SELF rows" guard with one row in the UC-21 renderer table. |
| 17 | test-cockpit-unclaimable.sh (4) | T2 stub, 3 | KEEP. Fix case 4 to assert per id (it currently matches `builder` anywhere). |
| 18 | test-statute-projection.sh::statute_keys ×3 cases | T3 (secondary), 5 | KEEP under operator-channel. Note: MISMATCH/OK are silently skipped when law-synth.sh is unreachable. Seed `PAGE_N` from a fixture page instead of brain. |
| 19 | test-beads-sparklines.sh (8) | T1, 3 | KEEP. Move the Python block into its own file (it is extracted today with awk by indentation). The "no HIST_COLS" source grep becomes T0. Add a bucket-distribution assertion. |
| 20 | test-event.sh (15) | T1 + sleeps, 11 | **DEMOTE**: add an injected clock (≈7 s of real `sleep`). The taxonomy and call-site greps become **SOURCE-GREP to T0 lint**. Fix the unconditional "kind format" ok. |
| 21 | test-cockpit-probe-fault.sh (renderer half, ~35 assertions); test-mail-pane.sh::? probe; test-now.sh::no trace ?; test-cockpit-reachable.sh::'? reachable' | T2, 13 (~25 `health.sh once` renders) | KEEP probe-fault as the **single** renderer FC table. **DEMOTE-TO-T1** via a health.sh seam that renders N snapshots per process. DELETE the weak bare-`?` duplicate in mail-pane. |
| 22 | test-now.sh::52/60/200/45/20-row panes, width, column 9; test-cockpit-fuse.sh::row fits 70/96 | T2, 8 | KEEP test-now.sh as golden frames. DELETE the width duplicate in fuse. |
| 23 | test-now.sh::trace_stats ×6, model_short, model substitution, live PID | T2 | **DEMOTE-TO-T1**: source `trace_stats`/`trace_tail`/`model_short` instead of `sed+eval`. The dirty-trace case tests an **in-test copy** of the sanitiser: make it call the collector's own. |
| 24 | test-drain-banner.sh (5); test-cockpit-collector-quota.sh::STALE/FAULT; test-snap-stale-threshold.sh (7) | T2: 2 + 4 + 6 | KEEP drain-banner. **Cross-area:** test-world-drain.sh (58 s, instance-lifecycle) duplicates it case for case, so keep 2 s here. Move quota::STALE badge INTO probe-fault's table. KEEP snap-stale-threshold as the three-reader contract. |
| 25 | test-cockpit-snap-absent.sh (7) | T2, 2 | KEEP. Drop the duplicate render: the positive control and "no snapshot" run the same fixture. |
| 26 | test-mail-pane.sh::health renders MAIL, CTX/SELF/GOV absent; test-cockpit-queue(-section).sh::pane; test-cockpit-gate.sh::LAND row; test-cockpit-fuse.sh::lease | T2 | KEEP, folded into the renderer table. Move the mail.sh `done`/sendmail cases in test-mail-pane.sh to operator-channel. |
| 27 | render.rs::every_row_fits_the_pane, a_threaded_item_fits_the_pane, the_alerts_view_fits_every_geometry (three copies of one geometry sweep), plus 20 editor and age cases; main.rs scroll ×5 | T1, billed in test-panel.sh 18 s | KEEP. Merge the three geometry tests into one property test over `View::ALL`. **Move to a native cargo job** with a cached target (see §7). |
| 28 | store.rs (31); model.rs alert labels (3); render.rs alerts (10) | T1 | KEEP store.rs + model.rs. Trim render.rs alert cases to the markers only; the filtering is already pinned in store.rs. |
| 29 | render.rs FYI/ALERTS/p-reject footers (~16); model.rs is_ask/is_suit/enact parse (19); test-pane-fyi.sh (7 filtered re-runs) | T1, plus a 17 s duplicate | KEEP the Rust tests. **DELETE test-pane-fyi.sh**: it re-runs a subset of test-panel.sh, and its only extra (per-test names) comes from reporting (§7). DELETE the tautological model.rs::reject_premise_* ×2 and render.rs::the_store_order_stays_oldest_first. |
| 30 | none | — | **GAP** (§6) |
| 31 | beads.rs (4); endpoint.rs (5) via test-loom.sh | T1 + T3, 31 | KEEP beads.rs. endpoint.rs: **DEMOTE 4 of 5 tests to T2** against a fake `bd` that serves canned JSON and counts calls (cache, budget 503, failure 502, static routes). Keep `the_payload_is_bounded…` as the real-bd shape row. |
| 32 | ops.rs (7); test-loom-ops.sh (8, **never runs in CI**: 77 skip, no prebuilt binary) | T1; dead T2 | KEEP ops.rs. **DELETE test-loom-ops.sh**; move its `/api/ops` route, `halted` and `cockpit_env_age_s` cases into endpoint.rs. Its header claims that "the gate covers `cargo test --lib`", which is false. |
| 33 | test-loom-page.sh (13) | T3, 9 | **DEMOTE-TO-T1**: move 12 arms to `node --test` over fixture.json. The real-bd arm moves to test-cockpit-bd-contract.sh. The static page-shape greps become T0. Fix: missing node exits 0, not 77. |
| 34 | test-loom-rebuild.sh (5) | T1, 2 | KEEP. The restart case should assert that the restart is (or is not) issued, not just queried. |
| 35 | test-ready.sh (35) | T2 stubbed, 6 | KEEP (a good model). Fix: when nc is absent the port-open case prints two fake "ok" lines; make that a skip. Pin ephemeral ports. |
| 36 | test-ctx-pointer.sh (13) | T1, 5 | KEEP |
| 37 | test-limits.sh (28) | T1, 13 | KEEP (exemplary). An optional speed-up is to generate the blobs once per file. |
| 38 | test-statusline.sh (13) | T1, 1 | KEEP |
| 39 | test-tokens.sh (8); test-model-switch-report.sh (6); test-released-defects.sh (5) | T1/T1/T3: 16 + 1 + 8 | KEEP tokens and report. Investigate the 16 s for 4 runs of tokens.sh (header says under 1 s; possibly scanning a real transcript dir, a hermeticity leak). released-defects: **DEMOTE-TO-T2**, with a bd graph JSON fixture and real git. |
| 40 | test-resolve-output.sh (7) | T1, 1 | KEEP. Fix: a missing subject exits 0 ("SKIP"); make it 77 or a failure. |
| 41 | test-cockpit-layout-identity.sh (5); test-cockpit-rebuild.sh::health tag/one session pane | T2 tmux, 3 | KEEP one tmux case in the shared-server suite. **DEMOTE the predicate to T1** over argv strings. |
| 42 | test-cockpit-layout-mail.sh (5 groups); test-cockpit-rebuild.sh::mail build | T2 tmux: 5 + part of 18 | **MERGE** layout-mail INTO a shared-tmux-server suite with rebuild. Drop rebuild's mail build case, which duplicates it. |
| 43 | test-cockpit-layout.sh (ghost detach); test-cockpit-layout-ensure-focus.sh (1 assertion, **vacuous**: `#{pane_active}` of the window's active pane is always 1) | T3 PTY 8; T2 4 | KEEP ghost detach (one PTY case). Unit-test the idle selector over `list-clients` output. **Rewrite ensure-focus** to compare the active `#{pane_id}`; as written it cannot fail. |
| 44 | test-cockpit-layout-conf.sh (4 groups); test-cockpit-mouse.sh (6, all source grep) | T2 tmux 4; T0 7 | **DEMOTE-TO-T1**: factor `build_health_cmd`, and call `apply_mouse_mode` with a PATH-shimmed tmux recording argv. test-cockpit-mouse.sh: **SOURCE-GREP to a behaviour test** (the `off\|no\|0)` regex matches any case arm). |
| 45 | test-layout-guard.sh (5); test-cockpit-collector-watchdog.sh (4) | T2 6; T1 4 | KEEP both. Fix guard: its installed-copy `ensure` talks to the **ambient tmux server** with no pinned socket, so pin `-L`. Fix watchdog: source the function instead of extracting it with awk, and a missing function gives SKIP with exit 0. |
| 46 | test-cockpit-rebuild.sh (8 groups) | T3 tmux, 18 | KEEP as the single tmux end-to-end. Fix the case that accepts `absent` as a pass for the long socket. Move remote.sh's source grep of rebuild.sh `COCKPIT_SESSIONS` here as a behaviour case. |
| 47 | test-tmux-env.sh (8) | T2 tmux, 12 | KEEP. Run it on the shared server and shorten the 10×0.3 s poll. |
| 48 | test-cockpit-remote.sh (10); test-cockpit-watcher-owner.sh (8) | T2: 1 + 3 | KEEP both. Remote: assert the dialer's rc, which is computed and never checked. The exec-bit and inventory greps move to T0. |

## 4. Duplicate clusters

1. **Panel cargo suite run twice.** test-panel.sh (18 s) runs all 120 tests in the panel crate. test-pane-fyi.sh (17 s) runs `cargo test` 7 more times with name filters (fyi, insight, enacted, statute, dismiss, promoted, why_it_matters), and each filter re-links; a test matching two filters runs twice.
   - **Evidence:** the mapper's overlaps for all four panel .rs files. test-pane-fyi's only unique assertion is a "≥15 matched" count.
   - **Keep:** test-panel.sh, moved to a native cargo job. **Delete** test-pane-fyi.sh. **Saves 17 s.**
2. **Queue section tested twice.** test-cockpit-queue.sh and test-cockpit-queue-section.sh use the same fixture shape:
   - CERTIFIED leads to next, BATCHED to batch, no landstate to anomaly;
   - `queue/open` with PR 42;
   - the same Python snapshot quoter;
   - the same pane assertions (QUEUE, `batch #42`, no UNLND).
   - **Unique to queue.sh:** QUARANTINE_N and BATCH_AGE.
   - **Keep:** queue-section, with those 2 rows added.
3. **Landed/unlanded classification in four places.** test-cockpit-landed.sh, test-cockpit-unlanded.sh, test-cockpit-queue.sh::SP_UNLANDED_N and test-cockpit-queue-section.sh::SP_UNLANDED_N all assert "closed + branch + no landstate = anomaly".
   - **Keep:** test-cockpit-unlanded.sh (it alone covers main vs master), plus the body-mention case from queue-section. **Merge** landed's 24 h-scope row into it.
   - The anomaly rows in the queue suites can go.
4. **Unadopted stray tested twice.** test-cockpit-unsent.sh::SP_UNADOPTED and test-cockpit-unadopted-queue.sh::positive control use an identical `sp-true-stray` fixture, and both run full `cockpit.sh once` against embedded Dolt (39 s + 12 s).
   - **Keep:** unsent, with the protected-namespace rows added.
5. **SOP and sweep harness duplicated.** test-cockpit-sop.sh and test-cockpit-sweep.sh share the same `run_sops` helper, the same testdb name `sop`, and the same broken-shelf and dir-as-ledger `?` fixtures. The sweep suite never seeds an SOP.
   - **Keep:** sop, with the sweep rows added.
6. **Fragment merge pasted into tests.** A copy of `collect.sh _merge_fragments` is pasted into test-cockpit-tiered-collector.sh and test-cockpit-collect-probes.sh; the latter's comment reads "same as test-cockpit-tiered-collector.sh". Both test the transcription, not `collect.sh`.
   - **Keep:** one merge table in tiered-collector that calls `collect.sh merge`.
7. **Unsupervised-loop refusal and SP_AT ordering tested twice.** test-cockpit.sh and test-cockpit-tiered-collector.sh::6/7; the mapper notes "explicitly duplicated".
   - **Keep:** test-cockpit.sh for `cockpit.sh`, and tiered::6 for `collect.sh` only.
8. **"? not 0" rendering spread across five suites.** test-cockpit-probe-fault.sh, test-mail-pane.sh, test-now.sh, test-cockpit-reachable.sh and test-cockpit-gate.sh.
   - **Keep:** probe-fault's renderer table as the single place, and add a row for every new key. The other suites keep only their positive value rows.
9. **Config-change exit tested three times.** test-cockpit-conf-change.sh (health.sh, 8 s), test-loom-conf-change.sh (loom.sh, 6 s) and test-cockpit-tiered-collector.sh::10 (collect.sh). Same contract (`law-long-lived-processes-pin-their-config`), three timing-bound copies.
   - **Keep:** one table-driven T1 suite parameterised by script, with an injected tick.
10. **Loom parser and filtering tested at unit and HTTP level.** ops.rs against test-loom-ops.sh (apostrophes, absent key), and beads.rs against endpoint.rs (drop closed, hoist edges, `dropped_edges`).
    - **Keep:** the unit tests, plus one HTTP shape row. Delete test-loom-ops.sh, which is dead in CI anyway.
11. **Panel geometry sweep written three times.** render.rs::every_row_fits_the_pane, a_threaded_item_fits_the_pane and the_alerts_view_fits_every_geometry each sweep the same geometries (107×19, 100×16, 60×8, 40×5).
    - **Keep:** one sweep over views × modes × content.
12. **Drain banner duplicated across areas.** test-drain-banner.sh (2 s) and test-world-drain.sh (58 s, instance-lifecycle area) cover it case for case.
    - **Keep:** test-drain-banner.sh. The saving is booked in the instance-lifecycle area.
13. **Pane-tagging and mail-pane checks across three tmux servers.** test-cockpit-rebuild.sh, test-cockpit-layout-identity.sh and test-cockpit-layout-mail.sh each start a server.
    - **Keep:** one shared-server fixture suite, with rebuild as the only end-to-end run.

## 5. Unit-extractable logic

| Logic | Today tested through | Seam for a T1 test |
|---|---|---|
| Per-section probe functions (`queue_keys`, `unsent_keys`, `dup_refs_keys`, `repo_label_keys`, `sphere_keys`, `sop_keys`, `reachable_keys`, `core_counts_keys`) in spira/cockpit.sh | full `cockpit.sh once` (all 16 probes) on embedded Dolt; 7× in unsent alone | Two changes:<br>• every section already has a subcommand (`cockpit.sh unsent`, `sops`, `reachable`, …), so tests must call it, not `once`;<br>• route all bd reads through `bdjson`, overridable by `SPIRA_BDJSON_FIXTURE=<file>`, so the Python classification gets canned JSON.<br>Real bd is kept only in test-cockpit-bd-contract.sh. |
| `_merge_fragments` (Python) | pasted copy in two tests | Tests call the existing `collect.sh merge <dir>` on fixture fragments. |
| `health.sh` section renderers | ~25 `health.sh once` process starts per suite (1,844 lines + conf.sh each) | Add `health.sh render-many <dir>`, which renders every `*.env` in a directory in one process. probe-fault, now, mail-pane, drain-banner, snap-absent, self and gate then become one-spawn table tests. |
| `trace_stats`, `trace_tail`, `model_short`, and the collector's SAID sanitiser | health.sh renders; `model_short` is pulled out with `sed+eval`; the sanitiser is re-implemented in the test | Move them into a sourceable `cockpit/lib-trace.sh` (or a Python module) and call them directly. |
| `drain_banner`, the halt banner, and snapshot-path diagnosis (`snap-absent`) | full pane render | Source the functions with `SPIRA_NOW` and stamp files. |
| Config-mtime watch loop (health.sh, loom.sh, collect.sh) | 3–4 s `timeout` windows | Use `SPIRA_*_TICK` (exists for loom) plus a single shared `conf_changed <path> <mtime0>` helper. |
| `spira_event` rate limiter | real `sleep 1`/`sleep 2` (≈7 s) | Take `SPIRA_NOW` in lib.sh's event code. |
| `cockpit_may_write` fence and `ratelim_keys` parser | two full `once` runs | Source cockpit.sh functions behind a `[[ ${BASH_SOURCE[0]} == $0 ]]` main guard. |
| `layout.sh` pieces:<br>• health-pane command builder<br>• `apply_mouse_mode`<br>• the pane-identity predicate<br>• idle-client selection<br>• `restart_spira_collector_if_stale` | a tmux server (layout-conf, identity, ghost), source grep (mouse), or awk extraction (watchdog) | Give layout.sh a main guard so the functions can be sourced. Pass `tmux` as `$TMUX_BIN` so a shim records argv. Identity and idle selection become pure functions over `list-panes`/`list-clients` output. |
| Landing classification (subject-only, base resolution) | real bd + real git + `once` | Keep real git (it is cheap) and stub the closed-bead list. |
| Loom endpoint cache, budget and error paths | real bd + cargo + `testdb_up` (31 s) | A `LOOM_TEST_BD` fake script that prints canned JSON and counts calls. The mechanism already exists as `counting_bd`; it currently wraps real bd. |
| Loom `model.js` derive | node spawned ~25× by bash | `node --test loom/static/model.test.js` over fixture.json |
| `released-defects.sh` join | 7 real `testdb_seed` calls | Accept `--graph <json>` for the discovered-from graph, and keep a real git repo. |
| cockpit-metrics.py SELF functions | a python3 spawn per case plus health.sh | Import the module and call its functions with a fixed `now`. |

## 6. Gaps

1. **The `czar_triggers` probe has no test at all.** `spira/cockpit.sh:2279` (`czar_triggers_keys`) maps four `incident:queue-*` classes to `SP_CZAR_{DEADLOCK,ATTRIB,SORT,STALL}_{FIRED,BY,OUTCOME}`. It has a refusal branch (`_refused` → `?`).
   - No test-*.sh mentions `SP_CZAR` or `czar_triggers`.
   - The tiered-collector loops over probe names but omit it.
   - **Needed:** a T1 table covering the refused, empty, one-fired-per-class and bad-timestamp cases.
2. **The `strand_keys` computation is untested at the collector.** `SP_STRANDS`, `SP_STRAND_GHOST`, `SP_STRAND_OTHER` and `SP_STRANDS_ESCALATED` are only **seeded** into snapshots (probe-fault line ~119, and watchtower). Nothing checks that the collector derives them correctly, or gives `?` on bd failure.
3. **Panel write paths** (UC-30): `model.rs::close_decision`, `comment`, `run_as` (actor) and the enact → `rule.sh` call, and `store.rs::refresh`/`run` (the bd subprocess), all have no test. The mapper notes the event loop, key dispatch and `PANEL_FIXTURE --once` are untested too.
   - These are the only cockpit paths that **change bead state**; the boundary says they must be explicit and must surface failure.
   - **Needed:** a T2 stub-bd argv recorder, plus a failure-relay case like test-resolve-output.sh.
4. **The panel reads a bd shape nobody checks.** store.rs fixtures are hand-shaped "like `bd list --all --json`", and no test checks them against real bd output (loom does this; the panel does not).
   - **Needed:** one row in the bd-contract suite that feeds real `bd list --all --json` output into `Snapshot` parsing.
5. **cockpit/panel/tests/fixture.json is dead.** No cargo test and no shell suite reads it.
   - **Needed:** a golden-frame test via `PANEL_FIXTURE … --once` (which would cover the untested `--once` path), or delete the file.
6. **Loom `read_world_state`/`check_sentinel`** (loom/src/ops.rs: the `world.halted` stamp and the systemctl sentinel probe) have no unit test. Only `halted=false` is checked, in a suite that never runs in CI.
7. **`append_history` and cockpit-history.csv.** `spira/cockpit.sh:1921` has a load-bearing subshell comment: sourcing the snapshot would leak the previous pass's keys into a failed probe. No test covers that leak. test-beads-sparklines.sh only asserts the Python block does **not** reference history.
8. **Positive supervised path.** test-cockpit.sh never checks that INVOCATION_ID **matching** the unit writes the snapshot, so a fence that refuses everything would pass.
9. **Probe concurrency cap.** collect.sh's "maximum concurrent slow-tier probes" (the comment at spira/collect.sh:76) has no test. Neither does the 16-probe interval/timeout table, for example a timeout shorter than the interval.
10. **Collector watchdog promotion race** (sp-vjiug): only mtime-versus-start is covered. The case where `cockpit.sh` is replaced during a pass is not tested.
11. **Silent-pass skips inflate green.** In each of these, a green run can mean the subject was never exercised:
    - test-loom-ops.sh: 77 in CI, never run.
    - test-loom-page.sh: missing node exits 0.
    - test-resolve-output.sh: missing subject exits 0.
    - test-cockpit-layout-conf.sh, test-cockpit-layout-identity.sh and test-cockpit-layout.sh: missing tmux or PTY exits 0.
    - test-cockpit-collector-watchdog.sh: missing function exits 0.
    - test-statute-projection.sh: the law-synth half and its MISMATCH/OK checks are skipped with a stderr note only.

    **Needed:** a T0 lint that `exit 0` is never preceded by `SKIP`.
12. **Hermeticity leaks.** These suites can read or act on the live box:
    - test-cockpit-tmp.sh: no `env -i`, runs a real probe against the ambient conf/DB.
    - test-layout-guard.sh: ambient tmux server.
    - test-cockpit-reachable.sh: `health.sh` runs without `env -i`.
    - test-cockpit-layout-conf.sh: no `env -i`.

    **Needed:** fix each suite, and add a T0 fence that every `cockpit.sh`/`health.sh`/`layout.sh` invocation in a test runs under `env -i` or a pinned `-L` socket.
13. **Reporting.**
    - 120 panel tests and ~16 loom tests each collapse to **one** ok/FAIL line in the suite record.
    - The summary line appears in 6 different wordings across this area ("N ok, M fail", "N passed, M failed", "RESULTS: …", "Results: …", em-dash style).
    - test-panel.sh's header says 97 tests; the actual count is 120.

    This area needs the shared reporting contract before any timing comparison means anything.

## 7. Cost

**Current** (sum of ci_secs over the 59 primary bash suites; Rust is billed inside its shims; test-loom-ops.sh is NA):

```
sparklines 3 + collect-probes 1 + quota 4 + watchdog 4 + conf-change 8 + dup-refs 7 + fuse 5
+ gate 14 + landed 14 + layout-conf 4 + ensure-focus 4 + identity 3 + layout-mail 5 + layout 8
+ mouse 7 + probe-fault 13 + queue-section 13 + queue 9 + reachable 4 + ready-seeded 9 + ready(c) 6
+ rebuild 18 + remote 1 + repo-labels 6 + self 6 + snap-absent 2 + sop 4 + sphere 7 + sweep 4
+ tiered 17 + tmp 11 + unadopted-queue 12 + unclaimable 3 + unlanded 15 + unsent 39 + watcher-owner 3
+ cockpit 6 + ctx-pointer 5 + drain-banner 2 + event 11 + layout-guard 6 + limits 13 + loom-conf 6
+ loom-page 9 + loom-rebuild 2 + loom 31 + mail-pane 2 + model-switch 1 + now 8 + pane-fyi 17
+ panel 18 + ready.sh 6 + released-defects 8 + resolve 1 + snap-stale 6 + statusline 1 + tmux-env 12
+ tokens 16
= 480 s
```

**Projected, by verdict.** The numbers are estimates: a stub-bd, single-section run of `cockpit.sh` costs about 1 s, and `testdb_up` costs about 5 s.

| Change | Suites (now → projected) | Δ s |
|---|---|---|
| Delete the duplicate panel run | pane-fyi 17 → 0 | −17 |
| Native cargo job with a cached target: panel + loom lib + endpoint (fake bd) | panel 18 + loom 31 = 49 → cargo job 12 + test-loom.sh real-bd shape row 8 = 20 | −29 |
| Unsent + unadopted merge; section subcommand; stub bd | 39 + 12 = 51 → 8 | −43 |
| Landed + unlanded merge; stub bd, real git | 14 + 15 = 29 → 5 | −24 |
| Queue + queue-section merge; fixture JSON | 9 + 13 = 22 → 5 | −17 |
| Real-bd meters to T1, plus one shared contract suite:<br>• dup-refs 7→1<br>• repo-labels 6→1<br>• sphere 7→1<br>• sop 4 + sweep 4 → 2<br>• ready-seeded 9 → 0<br>• loom-page 9 → 3<br>• new test-cockpit-bd-contract.sh 10 | 46 → 18 | −28 |
| Gate section, not `once` | 14 → 3 | −11 |
| Collector: tiered + collect-probes (real merge, short tick); quota split out (0) | 17 + 1 + 4 = 22 → 6 | −16 |
| Config-change trio with an injected tick | 8 + 6 → 2 + 2 | −10 |
| Hermetic tmp probe seam | 11 → 3 | −8 |
| `spira_event` clock seam; taxonomy to T0 | 11 → 2 | −9 |
| Renderer batch seam:<br>• probe-fault 13→4<br>• now 8→5<br>• self 6→2<br>• mail-pane 2→1<br>• cockpit.sh 6→3<br>• ready(cockpit) 6→2 | 41 → 17 | −24 |
| Layout T1 extraction:<br>• layout-conf 4→1<br>• mouse 7→1<br>• ensure-focus 4→1<br>• guard 6→1<br>• watchdog 4→2<br>• identity 3→2<br>• layout-mail 5 → 0 (merged)<br>• ghost detach 8→5<br>• rebuild 18→16<br>• tmux-env 12→6 | 71 → 35 | −36 |
| Other:<br>• tokens 16→3 (investigate the leak)<br>• released-defects 8→4<br>• reachable 4→3<br>• unclaimable 3→2<br>• fuse 5→4 | 36 → 16 | −20 |
| Unchanged (KEEP): sparklines 3, remote 1, watcher-owner 3, ctx-pointer 5, drain 2, limits 13, loom-rebuild 2, model-switch 1, ready.sh 6, resolve 1, snap-absent 2, snap-stale 6, statusline 1 | 46 → 46 | 0 |

**Totals:**

- 480 − (17 + 29 + 43 + 24 + 17 + 28 + 11 + 16 + 10 + 8 + 9 + 24 + 36 + 20) = 480 − 292 = **188 s**, a 61% cut.
- About 150 s of the 188 moves to T1 (certification-eligible, under 1 s per case).
- About 38 s stays T2/T3: test-cockpit-rebuild.sh, the ghost PTY case, tmux-env, test-cockpit-bd-contract.sh, and the real-bd loom row. These run in batch/main CI.
- **Cross-area bonus:** retiring test-world-drain.sh (58 s) in favour of test-drain-banner.sh.

**Tier placement:**

- **T0 (cert):** the probe-registry lint, the timeout(1)-wraps-function lint, the event taxonomy, the no-SKIP-exit-0 lint, the `env -i`/pinned-socket fence, and the Loom page-shape greps.
- **T1 (cert):** everything else marked T1 above, including the cargo job.
- **T2 (batch CI):** tmp, landing, unsent, fuse/gate /proc, released-defects, the tmux shared-server suite, watcher-owner, and the bd-contract suite.
- **T3 (main CI):** test-cockpit-rebuild.sh as the one tmux end-to-end.
- **T4:** none. `ready.sh` already runs inside acceptance-run.

---
