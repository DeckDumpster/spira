# testenv — design

`testenv` is the harness's test runner: the Rust replacement for `spira/testenv-batch.sh`
(1,891 lines at `local/main` f328a4071). Bead: **sp-5odic** (the speed half of sp-zv7j4,
"one build tool"). This document is the contract the implementation is tested against. It
was written before the code, from the script and from every caller, not by porting the
script line by line.

## 1. Intent

Run a selection of test suites **for a given tree**, against binaries that **cargo built
from that tree and nothing else**, inside a container that carries the fixture tier (image,
install, test-DB baseline), and report a **verdict**, **per-suite results** and **timings**.
Cache the verdict by tree, so an unchanged tree returns in seconds — and check that cache
**before building anything** (law-cheap-answers-first).

Four properties are the point; everything else serves them:

1. **One build tool, one artifact.** `cargo build --profile <p>` in the worktree under test,
   cargo's default `target/`. `target/<profile-dir>/` is always and only the artifact under
   test, exported as `SPIRA_ARTIFACTS`. There is no `bin/` copy, no tree-keyed
   `cargo-target-bins/<tree>` cache, no one-off `-p spira-config` / `-p test-plan` builds.
   **One exception, named on the command line:** `--artifacts <dir>` hands testenv
   executables that cargo already built from this tree on another machine (the CI `build`
   job). testenv then runs no cargo at all; it validates the set, stages it to
   `target/prebuilt/`, and keys the cache on its content (D8).
2. **A stable build path.** The worktree is the caller's own when it already holds the tree
   under test (an aeon's worktree, a round worktree — both outlive the run), otherwise a
   warm, reused scratch slot. Either way cargo is incremental: a one-line shell edit
   compiles nothing.
3. **Absence is never green.** A selected suite with no result is `unreached`; a fault in
   the harness (container, exec, account, install, build tool) is a distinct exit code from
   a red suite, and from a candidate that does not build.
4. **The verdict is explicit.** The last stdout line is `VERDICT GREEN|RED|FAULT ...`,
   never inferred by a caller from the absence of RED lines (round 96, 2026-09-28: a build
   that died with zero suites read as GREEN).

## 2. Contract

### 2.1 Invocation

```
testenv [--mode parallel|serial] [--suites <a.sh,b.sh,...|->] [--profile <p>]
        [--with-bins] [--artifacts <dir>] [--deadline <secs>] [--report [N]]
        <branch> [<repo-name-or-path>]
```

| argument | meaning |
|---|---|
| `<branch>` | any revision `git rev-parse` accepts: a branch, `HEAD`, a SHA. The **tree** of this revision is what is tested and what keys the cache. |
| `<repo>` | a repo-map name (resolved through spira-config's `[repo.<name>].path`) or a path (contains `/`). Default: `$SPIRA_REPO`, else the repository the running binary was built in. |
| `--mode` | `parallel` (default) or `serial`. Recorded in every result (a serial green is a weaker claim). |
| `--suites LIST` | comma list; each name must exist in the suite dir, else exit 2 with `batch: unknown suite: <name>`. Duplicates dropped, order kept. Given twice → exit 2. |
| `--suites -` | names one per line on stdin, blank lines ignored; **empty stdin = nothing to run, exit 0**. |
| (no `--suites`) | diff-derived: `select.sh --base <landref> --head <branch> --no-all-fallback --tiers $SPIRA_BATCH_TIERS(T2,T3)`; its `--mode-file` names the producer (`diff`/`all`). |
| `--profile P` | cargo profile. Default `aeon` (aeon and gate runs). A round passes `release` (its binaries ship). |
| `--with-bins` | **no build mode of its own** (builds, unless `--artifacts`). Accepted as an alias for `--profile release` when `--profile` is not given, because every caller that passes it today is a round whose binaries ship (round.sh, round-vm.sh, batcher-cut). See §7, decision D1. |
| `--artifacts DIR` | **do not build**: DIR holds the workspace's executables, prebuilt from this tree (CI's `bin/`). Relative to the current directory. Validated before anything else happens: DIR must be a directory holding an executable for every workspace binary target of the tree under test (read from the worktree's `Cargo.toml` files) and at least `spira-config`, `test-plan` and `testenv`; otherwise `VERDICT FAULT rc=2 reason=artifacts-invalid` naming what is missing — never a build, never a partial set. Exclusive with `--profile` and `--with-bins` (usage error). Only the flag selects this mode: an inherited `SPIRA_ARTIFACTS` is still ignored (§5). See D8. |
| `--deadline S` | a **hard** wall-clock budget, in whole seconds (> 0), for the suite phase: after S seconds no suite starts and running suites are killed; both are recorded `deferred`. Absent = no deadline, and the run is byte-for-byte what it was before the flag existed. See D7. |
| `--report [N]` | print each suite's median `wall_secs` over its last N (default 20) suite-timing rows as JSON `[{"suite","median","n"}]` (the shape `tsd-query.sh suite-medians` printed) and exit; no branch or container. |

`--mode=X`, `--suites=X`, `--report=N`, `--profile=P`, `--artifacts=DIR`, `--deadline=S` spellings are accepted. `--` ends
options. An unknown option prints `batch: unknown option: X` and the usage line, exit 2.

### 2.2 Exit status (unchanged from testenv-batch.sh — every caller branches on it)

| rc | meaning | whose fault |
|---|---|---|
| 0 | every selected suite passed, skipped, was disabled or skip-req; or nothing was selected; or a cached green for this key | — |
| 1 | suites ran and at least one non-quarantined suite was `red` or `timeout` | the branch |
| 2 | usage error; unknown suite; `--artifacts` directory invalid or incomplete; base ref unresolvable; worktree unobtainable; container did not come up / probe failed / died mid-batch / exec storm / `--user` account vanished; concurrent run holds the same key; a repeat of a cached red refused | the harness (or the caller) |
| 3 | configure / unit suspend / install inside the container failed; cargo not on PATH | the harness |
| 4 | the candidate's workspace failed to build | the branch |

callers: `gate.yml` and repo-map gate strings map `2|3 → 75`; `gate-spira.sh` names 2/3;
`batcher-cut` maps 4 → a `workspace-build` round red and reads `batch: unknown suite:` off
stderr; `attribute.sh` treats `>= 2` as a run fault; `gate-retry.sh` retries only rc 1.

### 2.3 Stdout (callers grep it)

* Log lines: `<UTC ISO-8601> spira: batch: <message>` (lib.sh `log`'s shape). Stdout, not
  stderr, as before.
* One line per suite as it completes, exactly `printf '  %-32s %s'`:
  `ok      <n>s`, `SKIPPED`, `DISABLED`, `SKIP-REQ requires:<tok,...>`, `TIMEOUT after <n>s`,
  `RED     rc=<rc> after <n>s`, `QUARANTINED-RED  rc=<rc> after <n>s`,
  `QUARANTINED-RED  TIMEOUT after <n>s`, `UNREACHED`, and (only under `--deadline`)
  `DEFERRED deadline after <n>s` (killed at the deadline) / `DEFERRED deadline` (never started). round.sh counts
  `^\s+test-\S+\.sh\s+(ok|RED|SKIPPED)`.
* Under a RED line, the suite's TAP `not ok` lines (max 10), indented six spaces — they do
  not match the per-suite regex above.
* On rc 1, `gate-diag.sh <results>` runs and prints its `| suite | red |` table, which
  round.sh greps (`^\| test-.*\| red`).
* **Last line, always** (new): `VERDICT GREEN ran=<n> [cached=<when>] [selected=0]`,
  `VERDICT RED ran=<n> red=<k>`, or `VERDICT FAULT rc=<rc> ran=<n> reason=<word>`. `ran`
  counts suites that executed (ok, skip, red, timeout, quarantined-red). Under `--deadline`
  the GREEN and RED lines gain ` deferred=<d> (deadline <S>s)` — `VERDICT GREEN ran=7
  deferred=5 (deadline 300s)` — and a deferred suite is never counted in `ran` (D7). round.sh prints its
  own VERDICT line to a different file, so the two never collide; once round.sh switches it
  can use this line instead of recomputing one.
* Cargo's own output and helper-script noise go to stderr.

### 2.4 Files written

`RESULTS = ${SPIRA_BATCH_RESULTS:-$SPIRA_RUN/batch-results}/<batch-key>`
(`<epoch>-<pid>` when no key could be computed).

| file | schema |
|---|---|
| `<suite>.result` | one line, `ResultRecord` (§3.1). Written **after** `<suite>.out`: its presence means complete. |
| `<suite>.out` | the suite's combined stdout+stderr, trailing newlines stripped, one `\n` added (bash `$(cat)` + `printf '%s\n'`). |
| `<suite>.tap.json` | new: `TapSummary` (§3.5) for the suite's output. |
| `batch.meta` | `image_tag= branch= base= key= mode= selection=` then new `profile= artifacts= worktree= tree=` — `key=value` lines; gate-timing.sh reads `key`, `branch`, `base`. Under `--deadline` only, three more lines: `deadline=<S> deferred=<d> deferred_suites=<space list>` (D7). Under `--artifacts`, `profile=prebuilt`, `artifacts=<DIR, absolute>` (the directory given, not the staging copy), and three more lines: `build=prebuilt artifacts_id=<sha256> staged=<wt>/target/prebuilt` (D8). Without it the file is unchanged. |
| `runner.meta` | `nproc= memtotal_kb= maxpar= cpu_busy_pct= suites_wall_s=` |
| `timing.tsv` | `<suite>\t<wall_s>\t<status>\t<bd_ms or ->` per suite with a result |
| `$SPIRA_VERDICTS/batch-<key>` | `VerdictFile` (§3.3), shared with gate.sh's directory |
| `$SPIRA_RUN/tsd/suite-timing.jsonl` | one `SuiteTimingRow` (§3.4) per executed suite plus one `__batch__` row |
| `$SPIRA_RUN/tsd/round.jsonl` | one `phase=build` row when `SPIRA_ROUND_BATCH_ID` is set |
| `/tmp/spira-batch-<inst>.owner` | our pid (owner-file protocol shared with testenv.sh) |
| `$SPIRA_LANDING_CONTAINERS` | container name appended, when that variable is set |

Nothing is written into the worktree except cargo's `target/` (and, under `--artifacts`,
`target/prebuilt/`, recreated each run).

### 2.5 Environment read

Everything testenv-batch.sh read keeps its name and meaning. Precedence: **environment**
(the caller's explicit per-run override — round.sh sets `SPIRA_BATCH_MAXPAR`) → spira.toml
through the `spira-config` library where it exposes the key → the old default. testenv never
parses spira.toml or the repo-map itself (law-config-through-the-cli-only).

| variable | default | config key |
|---|---|---|
| `SPIRA_RUN` | `$SPIRA_REPO/.runtime/spira[-inst]` if writable, else `$XDG_DATA_HOME/spira[-inst]/run` | `spira.run` |
| `SPIRA_BATCH_RESULTS` | `$SPIRA_RUN/batch-results` | — |
| `SPIRA_VERDICTS` | `$SPIRA_RUN/verdicts` | — |
| `SPIRA_VERDICT_TTL` | 86400 (0 disables) | `spira.verdict_ttl` |
| `SPIRA_VERDICT_REPEAT_CONSIDERED` | — (≥10 chars to override a cached red) | — |
| `SPIRA_SUITE_TIMEOUT` | 600 (0 disables) | `spira.suite_timeout` |
| `SPIRA_BATCH_MAXPAR` | hardware formula (0 = unlimited) | `spira.batch_maxpar` |
| `SPIRA_BATCH_MAXPAR_CEILING` | nproc | `spira.batch_maxpar_ceiling` |
| `SPIRA_BATCH_MEM_RESERVE_MIB` / `_PER_SUITE_MIB` / `_AVAIL_MIB` | 1024 / 192 / MemAvailable | `spira.batch_mem_*` |
| `SPIRA_BATCH_PSI_THRESHOLD` | 10 (0 disables) | `spira.batch_psi_threshold` |
| `SPIRA_BATCH_ORPHAN_MIN_AGE` / `_PREFIX` | 3600 / `spira-batch-` | `spira.batch_orphan_min_age` |
| `SPIRA_BATCH_PEAK_WARN_FRAC` | 60 | `spira.batch_peak_warn_frac` |
| `SPIRA_BATCH_EXEC_FAULT_THRESHOLD` | 5 | — |
| `SPIRA_BATCH_LIVENESS_RETRIES` / `_SLEEP` | 3 / 3 | — |
| `SPIRA_BATCH_INSTANCE` | first 12 chars of the key | — |
| `SPIRA_BATCH_SUITE_DIR` | `<worktree>/spira` | — |
| `SPIRA_BATCH_SKIP_INSTALL` | — | — |
| `SPIRA_BATCH_TIERS` | `T2,T3` | — |
| `SPIRA_BATCH_MAIL_CMD` / `SPIRA_BATCH_INCIDENT_CMD` | `<harness>/spira/mail.sh` / `incident.sh` | — |
| `SPIRA_SUITE_STATE_FILE` | `spira/suite-state` | — |
| `SPIRA_GATE_SELECT_HEAD` | `<branch>` | — |
| `SPIRA_ROUND_BATCH_ID`, `SPIRA_ROUND_MEMBERS` | — | — |
| `GITHUB_RUN_ID` / `SPIRA_BATCH_RUN_ID` | `local-<epoch>` | — |
| `SPIRA_TESTENV_SCRATCH_SLOTS` (new) | 4 | — |
| `SPIRA_TESTENV_HARNESS` (new) | the ancestor of the running binary that holds `spira/testenv.sh` | — |

Retired with the tree-keyed cache: `SPIRA_BATCH_BINS_TARGET_DIR`, `SPIRA_BATCH_BINS_TTL`.

### 2.6 Collaborators (subprocesses)

testenv owns orchestration; these stay separate components with their own contracts.

| collaborator | used for |
|---|---|
| `git` | rev-parse tree/commit, worktree list/add/checkout, status, `show <rev>:spira/suite-state`, landref rungs |
| `cargo` | `cargo build --profile <p> --workspace` in the worktree — **not** run at all under `--artifacts` |
| `spira/select.sh` | diff-derived selection (the ONE selector) |
| `spira/testenv.sh` | `tag` (image build-closure hash), `up --name --checkout` (image acquisition, boot, linger, cargo-volume ownership), `probe`, `down --name --volumes` |
| `podman` | `exec`, `container inspect`, `container exists`, `ps -a`, `stop`, `rm`, `volume rm` |
| `spira/gate-diag.sh`, `spira/gate-timing.sh` | red diagnostics table / batch-timing ledger row |
| `spira/mail.sh`, `spira/incident.sh` | a refused repeat of a cached red: one Concierge note per key (payload on stdin), one incident (payload on stdin) |

## 3. Schema

All records are Rust types with serde (`src/record.rs`, `src/verdict.rs`, `src/timing.rs`,
`src/tap.rs`); their `Display`/parse pairs are the on-disk formats above, byte-for-byte.

### 3.1 `ResultRecord` — `<suite>.result`

```
<status> <epoch> <secs> <fingerprint> <mode> <producer> <rc>
```

* `status`: `ok | skip | red | timeout | quarantined-red | disabled | skip-req | unreached |
  deferred` (`deferred` only under `--deadline`: `deferred <epoch> <secs> - <mode> <producer> -`,
  `secs` = how long it ran before the kill, 0 if never started; D7)
* `fingerprint`: `-` when green; `timeout:<suite>` on timeout; `requires:<tok,...>` for
  skip-req; else `fp(rc, output)` (§3.2).
* `mode`: `parallel | serial`; `producer`: `explicit | diff | all`; `rc`: the suite's exit
  code, or `-` for disabled / skip-req.
* `unreached` is the historical short form `unreached <epoch> 0 -` (4 fields). A reader
  takes field 1 as status and field 3 as seconds and must accept both forms.
* `unreached` never overwrites a completed record (sp-u1g).

### 3.2 Fingerprint

Identical to suites.sh so dedup keys match across callers: `sig` = the output lines
containing `FAIL`, else its last 20 lines; text = `rc=<rc>\n<sig>\n`; per line replace
`/tmp/[A-Za-z0-9._-]*`→`/tmp/X`, `/[A-Za-z0-9._/-]*/sptest_[A-Za-z0-9_]*`→`/X`,
ISO timestamps→`TIMESTAMP`, `HH:MM:SS`→`TIME`, `[0-9]{3,}`→`N`; then POSIX `cksum`
(CRC + byte length) printed with the separating space removed.

### 3.3 `BatchKey` and `VerdictFile`

`key = sha256("<repo_name> <tree> <image_tag> <sel_hash> <harness_hash> <mode> <producer> <profile>\n")`

* `sel_hash` = sha256 of the sorted suite names, one per line.
* `harness_hash` = sha256 of the running `testenv` executable, then `select.sh` and
  `suite-covers.sh` from the harness dir — the runner and the selector, as before
  (`$0` was the script).
* `profile` replaces `WITH_BINS`: an `aeon` green must not replay for a `release` run.
* Under `--artifacts` only, `profile` is `prebuilt` and ` prebuilt=<artifacts_id>` is appended
  before the newline, where `artifacts_id` = sha256 of `<name>\0<sha256 of the file>\n` for
  every executable directly under DIR, sorted by name. A prebuilt green therefore never
  replays for a different set of binaries (another build of the same tree, a different
  toolchain), nor for a cargo run. Without the flag the key line is byte-for-byte what it was.

`$SPIRA_VERDICTS/batch-<key>`: `key=value` lines `verdict=green|red|partial`, `when=<ISO UTC>`,
`by=testenv-batch`, `at=<epoch>`, `red_suites=<space list>` (red only),
`override_reason=`, `concierge_notified=<epoch>`, `deferred_suites=<space list>` (partial
only). Absent `verdict` = green (old files). `partial` = a deadline-cut green (D7): recorded,
never a cache hit.
Written only when `TTL > 0` and a key exists. A green within TTL exits 0 at once; a red
within TTL exits 2 unless `SPIRA_VERDICT_REPEAT_CONSIDERED` is a ≥10-char sentence, which
is then recorded as `override_reason` in the new verdict.

### 3.4 `SuiteTimingRow` — run/tsd family `suite-timing`

The one producer of a suite's timing. JSONL via the `tsd` crate's row builder, appended
under an exclusive flock (the same seam `tsd-write` uses):

```json
{"ts":"2026-09-29T03:00:00Z","host":"<hostname>","family":"suite-timing",
 "run_id":"local-1790000000","branch":"spira/sp-x","suite":"test-a.sh","rc":0,
 "wall_secs":12,"bd_calls":3,"bd_ms":40,"mode":"parallel","tier":"T2"}
```

`run_id`, `branch`, `suite`, `mode`, `tier` are strings (`tier` empty when undeclared);
`rc`, `wall_secs`, `bd_calls`, `bd_ms` are integers. `suite="__batch__"` is the one
end-to-end row per run (`rc=0`, `wall_secs` = whole batch). Readers: tsd-query.sh,
tier-budget.sh, gate-budget-select.sh, and testenv itself (LPT order, `--report`). A row
that does not parse is skipped, never fatal.

`round` family (only with `SPIRA_ROUND_BATCH_ID`): `batch_id`, `phase="build"`, `secs`,
`members`, `reds` (red + timeout count).

### 3.5 `TapSummary` — `<suite>.tap.json`

`{"plan":N|null,"passed":n,"failed":n,"skipped":n,"todo":n,"bail_out":str|null,"not_ok":[str]}`
from TAP lines `1..N`, `ok N - d`, `not ok N - d`, `# SKIP`/`# TODO` directives and
`Bail out! reason`. Informational: the verdict is the suite's exit status, not its TAP.

### 3.6 Suite headers and state

Read from the **tree under test**. Headers before the first `set -` line:
`# requires: a, b` (tokens; `testenv` is always met inside the container),
`# exclusive: <reason>`, `# tier: T0..T4`. `spira/suite-state` at the revision:
`<suite> | <state> | <since> | <bead> | <reason>`, `#` comments; state
`active|quarantined|disabled`, unknown states and malformed lines read as active
(fail-closed: absent file = everything active and blocking).

## 4. Pipeline

```
parse args ─ resolve repo, landref, tree ─ acquire worktree (in place | scratch slot)
  ─ select suites ─ drop disabled (DISABLED records) ─ image tag ─ batch key
  ─ VERDICT CACHE (exit here on a hit)                        ← nothing built yet
  ─ cargo build --profile p  (rc 4 on failure)  ─ SPIRA_ARTIFACTS=<wt>/target/<dir>
      └ --artifacts DIR: validated right after the worktree (rc 2), hashed into the key,
        staged to <wt>/target/prebuilt here instead of building  ─ SPIRA_ARTIFACTS=…/prebuilt
  ─ orphan sweeps ─ claim owner file ─ testenv.sh up / probe
  ─ configure, suspend loom+cockpit units, install  (skippable)
  ─ requirements check (skip-req records) ─ shared testdb baseline
  ─ schedule: exclusive first, then LPT; maxpar; PSI pause; per-suite timeout
  ─ faults: container death, exec storm, user-account loss → reclassify, rc 2
  ─ unreached records ─ batch.meta, runner.meta, timing.tsv, tsd rows
  ─ verdict file, gate-diag / gate-timing ─ VERDICT line ─ teardown (always)
```

### 4.1 Worktree

1. If the current directory is inside a worktree of the repo, or the branch is checked out
   in one, **and** that worktree's `HEAD` is the revision's commit **and** `git status
   --porcelain` is empty, build and test there in place.
2. Otherwise a scratch slot `$SPIRA_RUN/worktree/.testenv-slot-<n>` (n < slots), taken
   under an exclusive `flock` on `<slot>.lock`, `git checkout --detach --force <rev>` +
   `git clean -fdq -e target` — checkout rewrites only the files that differ, so cargo's
   mtime fingerprints keep `target/` warm across runs.
3. All slots busy → a throwaway `.testenv-<pid>` worktree, removed on exit (the old
   behaviour, cold).

The worktree the build ran in is never removed when it is the caller's, which is what lets
a round's `land` read `<round-worktree>/target/release` afterwards (the reason sp-5odic was
parked, now answered).

### 4.2 Container tier

Constants baked into the image (must agree with the Containerfile): user `spirauser`,
uid 1001, `XDG_RUNTIME_DIR=/run/user/1001`, `CARGO_HOME=/var/spira/cargo`,
`CARGO_TARGET_DIR=/var/spira/cargo/target`, checkout at `/workspace`.

* Name `spira-batch-<instance>`; home `/tmp/spira-batch-<instance>` on the host.
* **Sweeps** before `up`: owner files `/tmp/<prefix>*.owner` whose pid is gone, and
  `<prefix>*` containers with no owner file older than the min age — stop, rm, both cargo
  volumes, `/tmp/<name>`.
* **Owner claim**: refuse (rc 2) when the file names another live pid.
* **Image and boot**: `testenv.sh up --name <n> --checkout <worktree>` (image by
  build-closure tag: local, else pulled from the registry, else built; `--systemd`,
  pids-limit 8192, the worktree bind-mounted at `/workspace`, cargo volumes), then
  `testenv.sh probe`. testenv owns neither the Containerfile nor the tag scheme.
* **Install** (unless `SPIRA_BATCH_SKIP_INSTALL`): `configure.sh` with
  `CONFIGURE_PROD=/workspace/spira CONFIGURE_MAX_AEONS=1 CONFIGURE_MAX_LIVE_AEONS=1
  CONFIGURE_LOOM_ADDR=127.0.0.1:7300 CONFIGURE_DOLT_DATA=`; `ctrl.sh suspend
  spira-loom|spira-cockpit --reason ... --owner sp-fud1`; `systemd/install.sh <instance>`
  with `SPIRA_PROD=/workspace/spira SPIRA_INSTALL_FORCE=1 SPIRA_RUN=/tmp/spira-batch-<i>
  SPIRA_TESTDB_DATA=/tmp/spira-batch-<i>/testdb`. Any failure → rc 3.
* **Requirements**: each distinct `# requires:` token (bar `testenv`) is checked once with
  `command -v` inside the container.
* **Test-DB baseline**: `. /workspace/spira/testdb.sh && testdb_up batch_baseline`, which
  prints `TESTDB_NAME/DIR/BASELINE/BD/BIN/MODE=`. Complete → every suite gets
  `TESTDB_SHARED=1` and those six values (a ~26 ms copy each); otherwise
  `TESTDB_SHARED=0 TESTDB_NAME= TESTDB_DIR=` (a ~6 s `bd init` each).
* **Server-mode template** (sp-v2lqd): when any runnable suite carries `# testdb-mode:
  server`, `$SPIRA_ARTIFACTS/testenv testdb template --bd bd --dolt dolt` builds the
  pre-initialised store once; each such suite then gets a private Dolt sql-server copied
  from it (~0.1 s) instead of sharing `dolt-beads-test.service`. See DESIGN-testdb.md.
* **Per-suite exec**: `podman exec --user spirauser -e ... <name> bash
  /workspace/spira/<suite>`, output captured to a file, wrapped in the per-suite timeout
  (rc 124). Parallel mode adds a private `HOME=/tmp/spira-batch-<i>-<n>/home` (created
  first, with `.config/systemd/user`), `SPIRA_INSTANCE=<i>-<n>`,
  `SPIRA_RUN=/tmp/spira-batch-<i>-<n>`.
* **Every** exec that runs harness code (configure, suspend, install, baseline, suites)
  carries `SPIRA_ARTIFACTS=/workspace/target/<dir>` and
  `SPIRA_TEST_PLAN_BIN=/workspace/target/<dir>/test-plan`.
* **Teardown** (always, including on SIGINT/SIGTERM): only while the owner file still names
  us, `testenv.sh down --name <n> --volumes`; remove the host home; release the owner file
  only once podman confirms the container is gone.

The runtime is a trait (`ContainerRuntime`); the orchestration is unit-tested against a fake.

### 4.3 Scheduling

* **maxpar** = `min(ceiling, max(1, (avail - reserve) / per_suite))` with
  `budget = max(avail - reserve, per_suite)`; `SPIRA_BATCH_MAXPAR=0` unlimited; a positive
  value ≤ the hardware bound is used as an override, a larger one is clamped.
* **Order**: `# exclusive:` suites first (each drains the pool and runs alone), then
  longest-first by mean `wall_secs` from `suite-timing.jsonl`; a suite with no rows sorts
  as longest-on-record + 1; ties keep selection order.
* **Admission**: before each launch, pause 5 s at a time while memory PSI `avg10` exceeds
  the threshold; wait for a slot; check container liveness (retry `inspect` N times; a
  definitive `false` or N empties = dead, recording `ExitCode`/`OOMKilled`, a failed lookup
  named `inspect-failed`); stop launching once a user-account fault is seen.
* **Faults**: output matching `unable to find user … passwd file` = the account vanished;
  ≥ threshold reds with 0 s and empty output = exec storm (serial: consecutive). On any of
  these (and container death) red records with that shape — or the account-fault text — are
  rewritten `unreached`, and the batch exits 2.
* **Quarantined** suites run; a red or timeout is `quarantined-red` and does not block.

## 5. SPIRA_ARTIFACTS and the release-tarball layout

`conf.sh`'s `spira_bin <name>` resolves `${SPIRA_ARTIFACTS:-$SPIRA_REPO/bin}/<name>`:
SPIRA_ARTIFACTS in a test run, the installed release's `bin/` in production, and no fallback
between them. testenv exports SPIRA_ARTIFACTS **only to the processes that run the tree
under test**: the container's configure/install/baseline/suite execs, and host helpers that
run after the build. It never sets it in its own environment and strips an inherited one
from pre-build helpers (`testenv.sh tag`, `select.sh`), so a cold tree cannot trip conf.sh's
hard refusal for a missing `spira-config` before it has been built.

The two known reds of this bead's first attempt are both **fixtures that build a
production layout inside a test run** and then source that layout's `conf.sh`:

* test-artifact-install "conf.sh resolves SPIRA_LOOM_BIN to bin/loom in a tarball layout"
* test-concierge "launcher's --model came from persona.modeltest.model" (sp-5odic.2 says
  this one is red on the local/main baseline too).

Such a fixture inherits the suite's SPIRA_ARTIFACTS, so `spira_bin` answers the test
runner's `target/<dir>` instead of the fixture's `bin/`. testenv does **not** change how
`conf.sh` resolves a tarball's `bin/`; it changes nothing in conf.sh at all. The rule that
lets the two coexist is: *SPIRA_ARTIFACTS describes the checkout testenv built, and only
that checkout.* testenv therefore also exports `SPIRA_ARTIFACTS_ROOT=/workspace` (host:
the worktree) naming that checkout. Until conf.sh reads it, a fixture that simulates a
different layout must clear SPIRA_ARTIFACTS for its own subshell (commit e5332cc67 does this
for test-artifact-install). The structural fix — listed under Cutover because it is a bash
edit — is for `spira_bin` to honour SPIRA_ARTIFACTS only when `$SPIRA_REPO` is
`$SPIRA_ARTIFACTS_ROOT`; any other layout (a tarball, a fixture tree) then resolves its own
`bin/` without the fixture having to know.

## 6. Build profile (`[profile.aeon]`, already in the workspace Cargo.toml)

`inherits = "dev"`, `opt-level = 0`, `incremental = true`, `debug = "line-tables-only"`;
outputs to `target/aeon` (the profile's name); `dev` maps to cargo's `debug` directory.
Measured in this worktree, 2026-09-28, full workspace (`cargo build --profile aeon
--workspace`), host rustc 1.82.0:

| case | wall | what cargo compiled |
|---|---|---|
| cold, fresh target dir | 18.3 s | every workspace crate |
| warm, no change | 0.10 s | nothing |
| warm, one-line edit to `spira/lib.sh` | 0.12 s | **nothing** |
| warm, one-line edit to a leaf crate (`tsd-lifecycle-export/src/main.rs`) | 0.62 s | that crate only |
| warm, one-line edit to a shared crate (`spira-config/src/lib.rs`) | 2.84 s | spira-config and its 3 dependents (+ the leaf above, reverted) |
| `cargo build --release -p testenv` (LTO, `opt-level=z`) | 87 s | testenv and its dependencies |

So a warm one-suite aeon run after a shell edit spends ~0.1 s in the build step; the
<30 s-to-first-suite target of the bead is then the container tier (`testenv.sh up`,
install, baseline), not cargo. The old path rebuilt spira-config and test-plan from cold
into a fresh `cargo-target-bins/<tree>` on every new tree (95-116 s, sp-zv7j4).

## 7. Decisions

* **D1 — `--with-bins` = `--profile release` unless `--profile` is given.** The operator
  asked for a no-op alias; a pure no-op would silently turn every round (round.sh,
  round-vm.sh, batcher-cut all pass `--with-bins`) into an `aeon`-profile build and leave
  `target/release` stale for land. Mapping it keeps "switch by name" true. Flagged for
  the operator.
  **Amended 2026-09-29 (D8):** D1 as first written also said testenv *always* builds. That
  broke CI: gate.yml's `suites` job runs on an ephemeral VM with no Rust toolchain and was
  handed prebuilt binaries by the `build` job, exactly as testenv-batch.sh was. Every PR
  faulted `VERDICT FAULT rc=3 ran=0 reason=no-cargo` (run 36596357971, PR 454). "Always
  builds" now reads "builds unless `--artifacts` says what to test".
* **D2 — testenv.sh stays the image/boot owner.** Image acquisition (tag = build-closure
  hash, registry pull, build), boot and probe are testenv.sh's contract with other callers
  too (gate.yml publishes images through it). testenv drives it through the runtime trait;
  everything testenv-batch.sh did with podman directly is Rust.
* **D3 — the bd timing shim is dropped.** testenv-batch.sh created `/tmp/bd-shim-<i>/bd`
  but never put it on any PATH (`SPIRA_PATH` was always passed empty), so it never ran.
  The env it passed (`SPIRA_PATH=`, `SPIRA_BD_LOG`, `SPIRA_BD_TIMING_LOG`) is kept, as is
  reading those logs into `bd_calls`/`bd_ms`/`timing.tsv`.
* **D4 — the tier-budget step is dropped.** It ran only when `$RESULTS/suite-times.tsv`
  was non-empty, and nothing has written that file since suite-times moved to run/tsd; it
  always logged "skipped".
* **D5 — an empty selection says `VERDICT GREEN ran=0 selected=0`.** It is exit 0 by
  contract (the gate passes when nothing is affected). A caller that needs ran>0, as
  round.sh does, still has `ran=`.
* **D6 — scratch slots are warm and bounded.** 4 by default; overflow is a cold throwaway
  worktree, never a wait.
* **D7 — `--deadline S` is a hard cut of the suite phase, and a cut is not a verdict.**
  Operator order, 2026-09-29: "the gate timeout needs to be enforced; we cut it off at the
  timeout — it's not advisory." The gate selects suites to fit `SPIRA_GATE_BUDGET` (300 s) of
  historical *median* time, and nothing enforced it: gates measured that day ran 1732 s and
  2574 s (host contention, cold builds, suites slower than their median). The round runs the
  full corpus anyway, so a suite the gate did not finish is covered there.
  * **Clock.** Starts when the suite phase starts — immediately before the first suite is
    scheduled, after build, container up, install, requirements and the testdb baseline.
    Those are *not* under the deadline (F3 says what bounds them today).
  * **Cut.** Once `S` has elapsed no suite is launched (serial and parallel alike, and the PSI
    pause and slot wait give up at the deadline). A suite still running is killed by the
    **same mechanism as the per-suite timeout** (`run_bounded`: SIGTERM to the `podman exec`,
    SIGKILL 10 s later); container teardown, which always follows, reaps whatever survives
    inside. The exec reports `RC_DEADLINE` (-124, outside 0..=255 so no suite can produce it),
    and the per-suite timeout wins a tie, so a suite that reached its own timeout is still
    `timeout`.
  * **Record.** A killed suite and every never-started suite get `deferred` — distinct from
    `timeout` (the suite's fault) and `unreached` (a harness fault). `deferred` is neither
    `executed` (not in `ran`) nor `blocking`. A killed suite writes its partial `.out`, no
    `.tap.json` and **no suite-timing row** (a truncated wall time would drag its median down
    and make the selector pick more than fits next time).
  * **Verdict.** RED iff a suite that finished before the cut is red or timed out — exactly as
    today; otherwise GREEN, rc 0. A cut is not a fault (rc 2/3 would make the gate string
    exit 75 and retry forever). Order is untouched: the selector's order is the priority.
  * **Cache.** A green with `deferred > 0` writes `verdict=partial` with `deferred_suites=`;
    `decide` treats `partial` as a miss, so the same key re-runs rather than replaying a
    partial as a full green. (A partial must still overwrite a prior red for the key, or a
    later run without a reason would be refused on stale evidence.) A deadline run where
    nothing was deferred is a full green and is cached as one; the key does not include `S`,
    because a full green under any deadline is the same claim.
  * **Unchanged without the flag.** No `deferred` record, no `batch.meta` lines, no suffix on
    the VERDICT line, no `partial` file: `BatchCfg::deadline` is `None` and every new branch is
    guarded on it.

* **D8 — `--artifacts <dir>`: test prebuilt executables, build nothing.** The intent in §1
  is "binaries cargo built from that tree and nothing else"; *where* cargo ran is not the
  point. CI builds on a runner that has cargo and tests on one that does not, so the runner
  must be able to take the build as an input. Chosen shape, and why:
  * **A flag, not an environment variable.** An inherited `SPIRA_ARTIFACTS` is exactly what
    §5 strips from pre-build helpers, because a stale one silently points a run at the wrong
    binaries. Honouring it here would make "no build" something a caller can fall into. The
    flag is explicit, and it is exclusive with `--profile`/`--with-bins`, which describe a
    build that will not happen.
  * **Validated, never partial.** The required set is every binary target of the tree under
    test's workspace (each member's `[[bin]] name`s, else the package name when
    `src/main.rs` exists and `autobins` is not false — read from the files, since there is
    no cargo to ask) plus a floor of `spira-config` (conf.sh refuses without it under
    SPIRA_ARTIFACTS), `test-plan` (every exec's `SPIRA_TEST_PLAN_BIN`) and `testenv`. A
    missing directory, or any missing name, is rc 2 `artifacts-invalid` before the key is
    computed; the names go to stderr. There is no fallback to building.
  * **Staged, not mounted.** The container sees only the worktree (`/workspace`), and DIR can
    be anywhere, so every executable in DIR is copied to `<wt>/target/prebuilt/` (the
    directory is removed first, so nothing from an earlier set survives) and the container
    gets `SPIRA_ARTIFACTS=/workspace/target/prebuilt`. `SPIRA_ARTIFACTS_ROOT` stays the
    worktree. Copying ~30 release executables is seconds; a new bind mount would change
    testenv.sh's contract (D2).
  * **Keyed by content** (§3.3), so a prebuilt green is a claim about those bytes.
  * **Unchanged without the flag**: same key, same batch.meta, same build.
  * **CI passes it twice**: gate.yml's suites step runs `bin/testenv --artifacts bin`, and its
    serial re-run of reds, `gate-retry.sh`, takes `GATE_RETRY_ARTIFACTS=bin` and passes the
    same flag — otherwise the first red would turn into a `no-cargo` fault on the retry.

## 8. Cutover (bash and workflow edits for the operator — none made here)

Invocation: from a checkout, `"$(spira_bin testenv)"` (the installed release in
production; `SPIRA_ARTIFACTS` inside a test run). A round that tests its own runner uses
`cargo run -q --profile release -p testenv --` from its worktree.

| file:line | current | replacement |
|---|---|---|
| `.github/workflows/gate.yml:508` | `printf '%s\n' "$_s" \| bash spira/testenv-batch.sh --suites - "${{ github.sha }}" \|\| _b=$?` | `printf '%s\n' "$_s" \| bin/testenv --artifacts bin --suites - "${{ github.sha }}" \|\| _b=$?` — **corrected 2026-09-29 (D8)**: this row first said "the run rebuilds in place", but the `suites` runner has no cargo, so that faulted `no-cargo` on every PR |
| `.github/workflows/gate.yml:214-225,317-327` | `make build`, copy the release profile's executables into `bin/`, download into `bin/` | keep: `bin/` is the `--artifacts` directory (D8), and the source of `bin/testenv` |
| `spira/gate-spira.sh:464` | `} \| bash "$HERE/testenv-batch.sh" --suites - "${SPIRA_GATE_SELECT_HEAD:-HEAD}" >&2` | `} \| "$(spira_bin testenv)" --suites - "${SPIRA_GATE_SELECT_HEAD:-HEAD}" >&2` |
| `spira/gate-retry.sh:14` | `BATCH="${GATE_RETRY_BATCH:-$HERE/testenv-batch.sh}"` | `BATCH="${GATE_RETRY_BATCH:-$(spira_bin testenv)}"` and lines 64/66 `bash "$BATCH"` → `"$BATCH"` |
| `spira/attribute.sh:151` | `bash "$HERE/testenv-batch.sh" --suites "$suites_csv" "$sha" "$REPO"` | `"$(spira_bin testenv)" --suites "$suites_csv" "$sha" "$REPO"` |
| `spira/round-vm.sh:348` | `exec bash spira/testenv-batch.sh --mode parallel --with-bins --suites "$suites" round` | `exec cargo run -q --profile release -p testenv -- --mode parallel --profile release --suites "$suites" round` |
| `spira/round-vm.sh:350` | `exec bash spira/testenv-batch.sh --mode parallel --with-bins round` | `exec cargo run -q --profile release -p testenv -- --mode parallel --profile release round` |
| `spira/round-vm.sh:365,375,399-412` | pulls `round-work/.runtime/spira/cargo-target-bins/` and installs by tree | pull the VM worktree's release artifacts (`round-work/target/release`) and compare the VM's reported tree (batch.meta `key`/`worktree`) instead of a directory name |
| the operator's hand round tool (retired) | `bash "$w/spira/testenv-batch.sh" --mode parallel --with-bins --suites "$suites" "concierge/round-$2"` | `(cd "$w" && cargo run -q --profile release -p testenv -- --mode parallel --profile release --suites "$suites" "concierge/round-$2")` |
| `round.sh:134-138` | computes its own VERDICT from grep counts | may read testenv's last line: `tail -1 "$r.out"` is `VERDICT ...` |
| `round.sh:207` | `SPIRA_BATCH_BINS_TARGET_DIR="$w/.runtime/spira/cargo-target-bins" bash "$H/spira/queue.sh" land-local ...` | drop the variable; pass the round worktree (see queue.sh below) |
| `round.sh:212-213` | `_bins="$w/.runtime/spira/cargo-target-bins/$(git -C "$w" rev-parse "$head^{tree}")/release"` | `_bins="$w/target/release"`, and check `git -C "$w" rev-parse HEAD` = `$head` with a clean status (the build is of the worktree, not of a tree key) |
| `round.sh:325` | matches `*testenv-batch*"concierge/round-$n"*` | also match `*testenv*"concierge/round-$n"*` |
| `spira/queue.sh:1109-1119` (`_land_local_bins_dir`) | `${SPIRA_BATCH_BINS_TARGET_DIR:-$SPIRA_RUN/cargo-target-bins}/$tree/release` | take the round worktree (new `--worktree <path>` on `land-local`) and return `<worktree>/target/release` after checking its HEAD tree is `$head^{tree}` |
| `spira/queue.sh:1254-1255` | message "run testenv-batch.sh --with-bins first" | "run testenv --profile release in the round worktree first" |
| `batcher-cut/src/main.rs:134` | `home.join("testenv-batch.sh")` | the `testenv` binary (`spira_bin`-equivalent) |
| `batcher-cut/src/io.rs:671-672` | `cmd.arg("bash").arg(&env.testenv_batch); cmd.arg("--mode").arg("parallel").arg("--with-bins");` | `cmd.arg(&env.testenv_batch); cmd.arg("--mode").arg("parallel").arg("--profile").arg("release");` — **also** see Finding F1 |
| `batcher-cut/src/io.rs:605-620` (`bins_present`) | looks in `${SPIRA_BATCH_BINS_TARGET_DIR:-$run/cargo-target-bins}/<tree>/release` | look in the round worktree's `target/release`, same rule as queue.sh's `_land_local_bins_dir` below |
| `spira-config/examples/spira.toml:23`, repo-map gate strings (`spira/repo-map.example:141,149`, prod `repo.<name>.gate`) | `bash spira/testenv-batch.sh "$SPIRA_GATE_BRANCH"` | `"$(spira_bin testenv)" "$SPIRA_GATE_BRANCH"` (golden/fixture copies under `spira-config/tests/` follow the example) |
| `spira/conf.sh:459-467` (`spira_bin`) | `dir="${SPIRA_ARTIFACTS:-$SPIRA_REPO/bin}"` | honour SPIRA_ARTIFACTS only when `[ "${SPIRA_ARTIFACTS_ROOT:-}" = "$SPIRA_REPO" ]` or ROOT unset (§5) |
| `spira/testenv-guard.sh:17`, `spira/testlib.sh:307` | "run via spira/testenv-batch.sh" | "run via testenv" |
| `spira/chamber/*.fayth` `FAYTH_TOOLS` (`Bash(*testenv-batch.sh*)`) and the statute `law-tests-run-only-through-testenv-batch` | names the script | name the binary (`Bash(*testenv *)`) |
| **delete** `spira/testenv-batch.sh`, `spira/cargo-target-bins-prune.sh`, `spira/test-testenv-batch-with-bins.sh`, `spira/test-cargo-target-bins-prune.sh` (if present) | — | replaced by this crate |
| `spira/binary-path-fence-allow:11,63` | allows `spira/testenv-batch.sh`, `spira/test-testenv-batch.sh` | remove once deleted |
| `spira/batch-owner.sh` | sourced by testenv-batch.sh and its test | keep while testenv.sh and tests use it; testenv implements the same protocol |
| suites that source/grep testenv-batch.sh's text (`test-testenv-batch*.sh`, `test-select.sh` E2-E5, `test-testenv-tmux-isolation.sh` C2, `test-gate-workflow.sh`, `test-aeon-fence.sh` E1-E3, `test-testenv-batch-fence.sh`) | assert on the script | rewrite against `cargo test -p testenv` or the binary's behaviour |

## 9. Findings

* **F1 — batcher-cut reads results one directory too high.** It sets
  `SPIRA_BATCH_RESULTS=<dir>` and reads `<dir>/<suite>.result`, but the runner writes
  `<dir>/<batch-key>/<suite>.result` (both the script and this crate). Every suite reads as
  absent → Red. testenv keeps the key subdirectory (gate-retry.sh, gate-diag.sh and
  attribute.sh all read `*/`); batcher-cut should glob one level down.
* **F2** — the tier-budget and bd-shim dead paths (D3, D4).
* **F3 — the pre-suite phases have no bound of their own inside testenv.** The deadline (D7)
  deliberately excludes them, but "bounded by their own timeouts" is only partly true: the
  container `up` waits at most `SPIRA_TESTENV_QUEUE_TIMEOUT` (900 s) for an admission slot;
  `cargo build`, `configure`/`suspend`/`install` and the testdb baseline execs carry no
  timeout (`ExecRequest::timeout` is `None`). The outer bound is gate.sh's
  `timeout ${SPIRA_GATE_TIMEOUT:-2700}` around the whole gate string. Measured on
  2026-09-29, the D7 end-to-end run (`--deadline 60`, three suites, an aeon worktree's first
  build, a busy host), 381 s wall in all: cargo build 44 s; orphan sweep + owner claim 55 s;
  `testenv.sh up` + probe 178 s; configure/suspend/install 12 s; testdb baseline 11 s;
  **suite phase 61 s** (the cut: deadline + the kill); results, teardown and gate-timing 16 s.
  Under a 300 s deadline the suite phase is now bounded; the ~300 s of setup around it is not,
  and on that day it was the larger half.

## 10. `testenv suites` — the suite-state tooling (replaces spira/suites.sh)

`spira/suites.sh` (population, gate/timed partition, the watchtower's `status` block, flake
reports, quarantine hygiene and the `spira/suite-state` transitions) is folded into this
crate as the `testenv suites <cmd>` subcommand family (module `suites`), not a crate of its
own: it reads the same `spira/suite-state`, the same suite headers and the same result
record this runner already parses and writes, and a second crate would carry a second
parser of each — the drift suite-covers.sh was written to end. Its contract, schema,
seams, decisions and cutover are in [DESIGN-suites.md](DESIGN-suites.md). The runner's own
argument grammar is unchanged: `suites` as the first argument selects the family, and a
branch literally named `suites` is `testenv -- suites`.
