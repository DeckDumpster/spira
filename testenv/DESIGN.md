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
   test, staged inside the container as a release's `bin/` (§5). There is no `bin/` copy, no tree-keyed
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
| (no `--suites`) | diff-derived: the `suite-select` library (`io::select_diff`, linked; sp-wx2tw) over `<landref>...<branch>`, no all-suites fallback, tiers `$SPIRA_BATCH_TIERS` (T2,T3); its mode names the producer (`diff`/`all`). A selection it cannot compute is `FAULT rc=2 reason=select-refused` (an unclaimed source file: `select-unclaimed`), never an empty selection. |
| `--profile P` | cargo profile. Default `aeon` (aeon and gate runs). A round passes `release` (its binaries ship). |
| `--with-bins` | **no build mode of its own** (builds, unless `--artifacts`). Accepted as an alias for `--profile release` when `--profile` is not given, because every caller that passes it today is a round whose binaries ship (round.sh, round-vm.sh, batcher-cut). See §7, decision D1. |
| `--artifacts DIR` | **do not build**: DIR holds the workspace's executables, prebuilt from this tree (CI's `bin/`). Relative to the current directory. Validated before anything else happens: DIR must be a directory holding an executable for every workspace binary target of the tree under test (read from the worktree's `Cargo.toml` files) and at least `spira-config`, `test-plan` and `testenv`; otherwise `VERDICT FAULT rc=2 reason=artifacts-invalid` naming what is missing — never a build, never a partial set. Exclusive with `--profile` and `--with-bins` (usage error). Only the flag selects this mode: an inherited `SPIRA_ARTIFACTS` is still ignored (§5). See D8. |
| `--deadline S` | a **hard** wall-clock budget, in whole seconds (> 0), for the **whole trial** (sp-govet, §11): the clock starts when testenv starts. Setup (build, container, install, test databases) must finish within its share of S (`SPIRA_TESTENV_SETUP_SHARE`, default 50 %) or the run is `VERDICT FAULT rc=2 reason=deadline-<phase>`; the suite phase gets what is left of S: after that no suite starts and running suites are killed; both are recorded `deferred`. It also selects the warm path (§11.2). Absent = no deadline, and the run is byte-for-byte what it was before the flag existed. See D7, D9. |
| `--report [N]` | print each suite's median `wall_secs` over its last N (default 20) suite-timing rows as JSON `[{"suite","median","n"}]` (the shape `tsd-query.sh suite-medians` printed) and exit; no branch or container. |

`--mode=X`, `--suites=X`, `--report=N`, `--profile=P`, `--artifacts=DIR`, `--deadline=S` spellings are accepted. `--` ends
options. An unknown option prints `batch: unknown option: X` and the usage line, exit 2.

### 2.2 Exit status (unchanged from testenv-batch.sh — every caller branches on it)

| rc | meaning | whose fault |
|---|---|---|
| 0 | every selected suite passed, was **declared** skipped or skip-req (§3.7), was disabled; or nothing was selected; or a cached green for this key | — |
| 1 | suites ran and at least one non-quarantined suite was `red` or `timeout` — including an **undeclared** SKIP/SKIP-REQ, reclassified red by §3.7 | the branch |
| 2 | usage error; unknown suite; `--artifacts` directory invalid or incomplete; base ref unresolvable; worktree unobtainable; container did not come up / probe failed / died mid-batch / exec storm / `--user` account vanished; concurrent run holds the same key; a repeat of a cached red refused; `spira/skip-allowlist.tsv` declares a requirement testenv itself must provide (§3.7, reason=`skip-allowlist-invalid`) | the harness (or the caller) |
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
  An **undeclared** SKIP or SKIP-REQ (§3.7) prints `RED     undeclared skip — <requirement>`
  or `QUARANTINED-RED  undeclared skip — <requirement>` — the first word stays the plain
  status every existing reader already keys on (this regex, `gate::parse::red_suites`,
  `batcher-cut::result_status`'s field 1); only the text after it differs from a real
  assertion failure's `rc=<rc> after <n>s`.
* Under a RED line, the suite's TAP `not ok` lines (max 10), indented six spaces — they do
  not match the per-suite regex above.
* On rc 1, `gate-diag.sh <results>` runs and prints its `| suite | red |` table, which
  round.sh greps (`^\| test-.*\| red`).
* **Last line, always** (new): `VERDICT GREEN ran=<n> [cached=<when>] [selected=0] [skipped=<s>]`,
  `VERDICT RED ran=<n> red=<k> [skipped=<s>]`, or `VERDICT FAULT rc=<rc> ran=<n> reason=<word>`. `ran`
  counts suites that executed (ok, skip, red, timeout, quarantined-red). Under `--deadline`
  the GREEN and RED lines gain ` deferred=<d> (deadline <S>s)` — `VERDICT GREEN ran=7
  deferred=5 (deadline 300s)` — and a deferred suite is never counted in `ran` (D7). `skipped=<s>`
  (new, §3.7) appears only when `s > 0`: the count of suites still `skip`/`skip-req` after the
  skip contract — i.e. **declared** ones only, since an undeclared one was already
  reclassified red and is not counted here. A log line names them:
  `N suite(s) skipped (declared, spira/skip-allowlist.tsv): <names>`. round.sh prints its
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
| `batch.meta` | `image_tag= branch= base= key= mode= selection=` then new `profile= artifacts= worktree= tree=` — `key=value` lines; gate-timing.sh reads `key`, `branch`, `base`. Under `--deadline` only, three more lines: `deadline=<S> deferred=<d> deferred_suites=<space list>` (D7). Under `--deadline` also `phases=<name>:<secs>,…` and `warm=spare|cold|off` (§11.2). Under `--artifacts`, `profile=prebuilt`, `artifacts=<DIR, absolute>` (the directory given, not the staging copy), and three more lines: `build=prebuilt artifacts_id=<sha256> staged=<wt>/target/prebuilt` (D8). Without it the file is unchanged. |
| `runner.meta` | `nproc= memtotal_kb= maxpar= cpu_busy_pct= suites_wall_s=` |
| `timing.tsv` | `<suite>\t<wall_s>\t<status>\t<bd_ms or ->` per suite with a result |
| `$SPIRA_VERDICTS/batch-<key>` | `VerdictFile` (§3.3), shared with gate.sh's directory |
| `$SPIRA_RUN/tsd/suite-timing.jsonl` | one `SuiteTimingRow` (§3.4) per executed suite plus one `__batch__` row |
| `$SPIRA_RUN/tsd/round.jsonl` | one `phase=build` row when `SPIRA_ROUND_BATCH_ID` is set |
| `/tmp/spira-batch-<inst>.owner` | our pid (owner-file protocol shared with `testenv container`) |
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
| `SPIRA_BATCH_MAIL_CMD` / `SPIRA_BATCH_INCIDENT_CMD` | `mail` / `incident.sh` on PATH (sp-gypjk) | — |
| `SPIRA_SUITE_STATE_FILE` | `spira/suite-state` | — |
| `SPIRA_SKIP_ALLOWLIST_FILE` (new, §3.7) | `spira/skip-allowlist.tsv` | — |
| `SPIRA_GATE_SELECT_HEAD` | `<branch>` | — |
| `SPIRA_ROUND_BATCH_ID`, `SPIRA_ROUND_MEMBERS` | — | — |
| `GITHUB_RUN_ID` / `SPIRA_BATCH_RUN_ID` | `local-<epoch>` | — |
| `SPIRA_TESTENV_SCRATCH_SLOTS` (new) | 4 | — |
| `SPIRA_TESTENV_HARNESS` (new) | the ancestor of the running binary that holds `spira/testenv/Containerfile` (was `spira/testenv.sh`, §12) | — |

Retired with the tree-keyed cache: `SPIRA_BATCH_BINS_TARGET_DIR`, `SPIRA_BATCH_BINS_TTL`.

### 2.6 Collaborators (subprocesses)

testenv owns orchestration; these stay separate components with their own contracts.

| collaborator | used for |
|---|---|
| `git` | rev-parse tree/commit, worktree list/add/checkout, status, `show <rev>:spira/suite-state`, `show <rev>:spira/skip-allowlist.tsv` (§3.7), landref rungs |
| `cargo` | `cargo build --profile <p> --workspace` in the worktree — **not** run at all under `--artifacts` |
| `suite-select` (crate, linked) | diff-derived selection (the ONE selector, sp-wx2tw) |
| this executable's `container` subcommand (§12; was `spira/testenv.sh`), run as a child against the harness copy `Harness::locate` found — a gate-built testenv therefore drives the tree under test's own image closure, sp-isom7 | `tag` (image build-closure hash), `up --name --checkout` (image acquisition, boot, linger, cargo-volume ownership), `probe`, `down --name --volumes` |
| `podman` | `exec`, `container inspect`, `container exists`, `ps -a`, `stop`, `rm`, `volume rm` |
| `spira/gate-diag.sh`, `spira/gate-timing.sh` | red diagnostics table / batch-timing ledger row |
| `mail`, `spira/incident.sh` | a refused repeat of a cached red: one Concierge note per key (payload on stdin), one incident (payload on stdin) |

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
* `harness_hash` = sha256 of the running `testenv` executable (which links the selector since
  sp-wx2tw; `select.sh` is gone), then
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

**bd wait (`bd_calls`, `bd_ms`), sp-cln99.** Measured by `bd-meter` (`src/bdmeter.rs`, a
second binary of this crate): in parallel mode the suite's private HOME is made by
`<artifacts>/bd-meter --install <home>`, which links `$HOME/.local/bin/bd` and
`…/bd-embedded` to itself. conf.sh puts `$HOME/.local/bin` ahead of the system directories,
so every `bd`/`bd-embedded` a suite runs by name goes through the meter, which runs the next
binary of that name on PATH, and appends `<wall_ms> <rc> <subcommand>` to `SPIRA_BD_LOG`.
An artifact set without the meter falls back to `mkdir -p` (unmetered: `bd_calls` 0). Serial
mode is unmetered. `bd_calls = 0` therefore means "no metered call", not "no wait"; readers
report the metered share. Before sp-cln99 nothing wrote the log and every row read 0/0 (D3).
Invoked **by path** through a directory on PATH (a caller holding `$SPIRA_BD`, which
`testdb_up` exports as the absolute path of the meter's link since sp-34ru2), the meter
searches only the PATH entries after that directory, so a `bd` wrapper ahead of it that execs
`$SPIRA_BD` (loom's counting shim) is never taken for the real one — the two would exec each
other until fork failed with EAGAIN.

The `__batch__` row also carries the trial's phases (sp-govet, §11.2):
`"setup_secs":<n>` (everything before the first suite could start), `"phases":"build:20,up:2,…"`
and `"warm":"spare|cold|off"`. Per-suite rows are unchanged; readers that do not know the
fields ignore them.

`round` family (only with `SPIRA_ROUND_BATCH_ID`): `batch_id`, `phase="build"`, `secs`,
`members`, `reds` (red + timeout count).

### 3.5 `TapSummary` — `<suite>.tap.json`

`{"plan":N|null,"passed":n,"failed":n,"skipped":n,"todo":n,"bail_out":str|null,"not_ok":[str]}`
from TAP lines `1..N`, `ok N - d`, `not ok N - d`, `# SKIP`/`# TODO` directives and
`Bail out! reason`. Informational: the verdict is the suite's exit status, not its TAP.

### 3.5a `jsonl_rows` — a suite's `results.jsonl` rows (sp-9gd4e)

Replaces `spira/tap-jsonl.sh`'s `tap_jsonl_rows`, deleted (sp-9gd4e). Its only caller,
`gate-diag`, now depends on this crate and calls `tap::jsonl_rows` directly instead of
shelling out to the bash — "ONE PARSER, ONE PLACE" (the bash's own rule for itself and
`suite-covers.sh`, which it sourced) now means one Rust function two crates share, not one
sourced file. `suite-covers.sh` itself is untouched (`suites.sh`/`gate-touched.sh`'s
concern, unrelated to this bead); its accessors were already ported to `suite-select::header`
(`tier_of`, `covers_of`) by an earlier bead, and this function reuses them rather than
re-deriving tier/UC from the suite source a third way.

**Contract.** `jsonl_rows(suite, suite_source, out_text, fallback, secs) -> String`: zero or
more `\n`-terminated JSON objects, `{"suite":s,"tier":t,"case":c,"status":st,"seconds":n,
"uc":[...],"detail":d}`. `tier` and `uc` come from `suite_source` (empty/`[]` when
undeclared or the source could not be read — never an error: absence is the ordinary case
for most suites). `uc` is `# covers:`'s tokens matching the shell glob `UC-*-[0-9][0-9]`,
verbatim.

- **`out_text` opens with the literal line `TAP version 14`:** one row per `ok`/`not ok`
  case (`status` `pass`/`fail`), with a `not ok`'s first following `# ` comment as `detail`;
  a whole-suite `1..0 # SKIP <reason>` or `Bail out! <reason>` becomes one more row,
  `case="(suite)"`, `status` `skip`/`bail`. A suite not yet migrated to testlib.sh emits no
  TAP and never takes this branch (its `.out` does not open with the header line).
- **Otherwise (`out_text` is `None`, unreadable, or does not open with the header):** one
  `case="(suite)"` row built from `fallback` (the caller's own status vocabulary, not
  reinterpreted here) — `ok`→pass, `skip`→skip, `unreached`→unreached, everything else
  (`timeout`, `red`, `quarantined-red`, anything unrecognised)→fail, `detail="not migrated
  to testlib.sh"`. Every suite gets a row on day one; migrating to testlib.sh only adds
  per-case rows where there was one row before.

**Escaping asymmetry, kept from the bash exactly.** `suite` and `tier` (and the whole
fallback row) escape backslash, quote, newline and tab. `case` and `detail`, read from a TAP
line, escape only backslash and quote — a stray tab in a suite's own failure message is
passed through, not turned into the two characters `\t`. Two functions, `esc_full`/
`esc_case`, not one, because the bash carried two (`_tap_json_escape` outside `awk`, a
narrower `esc()` inside it) and a reader downstream may already depend on the difference.

**Parity (sp-9gd4e's report has the full evidence).** Byte-identical to
`bash -c '. spira/tap-jsonl.sh; tap_jsonl_rows …'` over every `spira/*.sh` file in the tree
(633 files, exercising tier/UC extraction through the fallback branch) plus fixtures for
the TAP branch, `1..0 # SKIP`, `Bail out!`, and the escaping asymmetry.

### 3.6 Suite headers and state

Read from the **tree under test**. Headers before the first `set -` line:
`# requires: a, b` (tokens; `testenv` is always met inside the container),
`# exclusive: <reason>`, `# tier: T0..T4`. `spira/suite-state` at the revision:
`<suite> | <state> | <since> | <bead> | <reason>`, `#` comments; state
`active|quarantined|disabled`, unknown states and malformed lines read as active
(fail-closed: absent file = everything active and blocking).

### 3.7 The skip contract — `spira/skip-allowlist.tsv` (sp-gjx1b)

**A SKIP is not automatically green.** Before this, `Status::Skip` (a suite that exits 77,
the automake-skip convention testlib.sh's `skip <reason>` uses) and `Status::SkipReq` (a
suite pre-empted because a `# requires:` token was not on PATH in the container) were both
`blocking() == false` unconditionally — the exit-status table (§2.2) simply listed them next
to `ok`. sp-cln99 broke the server-mode testdb template lookup on 2026-09-29 and 13 suites
SKIPped with no red anywhere, because that is exactly the shape a SKIP with no further check
produces: absence read as a positive result (law-absence-needs-a-positive-control).

**The rule now:** a SKIP or SKIP-REQ is green only when it is **declared** — its exact
requirement, for that exact suite, is a line in `spira/skip-allowlist.tsv` on the tree under
test (read the same way as `suite_state_file`, §2.5/§2.6). Undeclared is red. There is no
third state: every SKIP is either declared-and-green or undeclared-and-red.

**The requirement is the record's fingerprint, unchanged.** `ResultRecord.fingerprint`
already named the reason for SKIP-REQ (`requires:<tok,...>`, §3.1); this bead gives SKIP the
same treatment: `skip:<reason>`, where `<reason>` is testlib.sh's `skip <reason>` text (TAP's
skip-all form `1..0 # SKIP <reason>`, read by `tap::skip_all_reason`), normalized exactly
like a red's fingerprint (§3.2 — paths, timestamps, numbers) and then space-joined with `_`.
The join is not cosmetic: the `.result` line is seven fields split on whitespace, and a
reason is free text that would otherwise shift every field after it. A suite that exits 77
without the `1..0 # SKIP` convention gets the fixed key `skip:(no_reason_given)` — silence is
never a way to skip undetected. The allow list's `requirement` column is this exact string,
so a `.result` fingerprint and an allow-list line always read the same.

**Reclassification reuses `Status::Red` (or `QuarantinedRed` on a quarantined suite) as-is —
never a new status word.** `gate::parse::red_suites`/`ran_suites` (token-scan the line after
`<suite>.sh`), `batcher-cut::result_status` (field 1 of the `.result` line) and round.sh's own
regex all already treat `red` as blocking; teaching three readers across two languages a new
word was rejected in favor of keeping the *fingerprint* — which already carried the reason —
and only changing which *status* it is filed under. The one visible difference is cosmetic:
`suite_line` (§2.3) prints `RED     undeclared skip — <requirement>` instead of `RED
rc=<rc> after <n>s`, so a human reading the log sees the reason at once; the leading `RED`
token is untouched, which is all any of the three readers key on.

**Two ways to be declared wrong, both refused, not silently ignored:**
- **Undeclared** (no matching line) → red, as above.
- **Reserved** — the requirement names a category testenv itself must provide: a fixture, a
  binary in the artifact set, a template, a testdb (matched case-insensitively as a substring
  of the requirement: `fixture`, `template`, `testdb`, `binary`). `SkipGate::load` refuses the
  **whole allow-list file** outright (`VERDICT FAULT rc=2 reason=skip-allowlist-invalid`) if
  any line declares one — declaring it would hide testenv's own defect (sp-cln99's exact
  shape) behind "genuinely external" cover, so the file that would do that never loads at all,
  rather than merely having that one line ignored.
- Everything else is a **curation choice**, not a keyword match: `cargo`/`dolt`/`git`/
  `spira-config` absence is deliberately left undeclared even though the words don't match
  the reserved list, because the same container already needs `cargo` for the build step that
  ran moments earlier — its absence at suite time is an environment defect, not host hardware.
  What *is* declared today: T4 host-acceptance suites needing a real `systemd --user` session
  or `tmux`, and nested `podman` — genuinely outside the container testenv provides.

**Pre-emption is not automatically green either.** When every selected suite is pre-empted
(no requirement was met for any of them — the historical "nothing to run" shortcut), the run
now checks whether any of those pre-emptions is itself an undeclared SKIP-REQ before
returning green: if so, the run is red (`VERDICT RED ran=0 red=<k>`), because "nothing ran"
and "nothing failed" are not the same claim.

**Observability:** the `VERDICT` line (§2.3) gains `skipped=<n>` (only when `n > 0`) counting
suites still `skip`/`skip-req` — declared ones only, since an undeclared one is already
red and not counted here — and a log line names them:
`N suite(s) skipped (declared, spira/skip-allowlist.tsv): <names>`.

**Allow-list format**: `<suite>\t<requirement>\t<why>`, tab-separated; `#` comments and blank
lines dropped. It **only shrinks**: an entry is removed when the suite stops needing it, never
added to paper over a red. `src/skipgate.rs` owns parsing, the reserved check and
reclassification; unit tests there and in `record.rs`/`run/tests.rs` cover both directions
(declared stays green, undeclared goes red) and the fixture required by this bead's
acceptance: a suite green on one tree that SKIPs — undeclared — on the next yields a red
verdict, not a green one (`run::tests::a_suite_that_turns_from_green_to_an_undeclared_skip_is_no_longer_green`).

## 4. Pipeline

```
parse args ─ resolve repo, landref, tree ─ acquire worktree (in place | scratch slot)
  ─ select suites ─ drop disabled (DISABLED records) ─ image tag ─ batch key
  ─ VERDICT CACHE (exit here on a hit)                        ← nothing built yet
  ─ cargo build --profile p  (rc 4 on failure)  ─ artifacts <wt>/target/<dir>
      └ --artifacts DIR: validated right after the worktree (rc 2), hashed into the key,
        staged to <wt>/target/prebuilt here instead of building  ─ artifacts …/prebuilt
  ─ orphan sweeps ─ claim owner file ─ testenv container up / probe
  ─ stage the tree as a release, PATH from it (§5; rc 3 reason=stage)
  ─ configure, suspend loom+cockpit+queue-watch units, install  (skippable)
  ─ requirements check (skip-req records) ─ testdb template (else shared baseline)
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
* **Image and boot**: `testenv container up --name <n> --checkout <worktree>` (§12; image by
  build-closure tag: local, else pulled from the registry, else built; `--systemd`,
  pids-limit 8192, the worktree bind-mounted at `/workspace`, cargo volumes), then
  `testenv container probe`. The runner owns neither the Containerfile nor the tag scheme; the
  `container` subcommand (§12) does.
* **Install** (unless `SPIRA_BATCH_SKIP_INSTALL`): `configure.sh` with
  `CONFIGURE_PROD=/workspace/spira CONFIGURE_MAX_AEONS=1 CONFIGURE_MAX_LIVE_AEONS=1
  CONFIGURE_LOOM_ADDR=127.0.0.1:7300 CONFIGURE_DOLT_DATA=`; `ctrl.sh suspend
  spira-loom|spira-cockpit|spira-watch-queue-watch --reason ... --owner sp-fud1` (the
  queue-watch watcher execs `/workspace/bin/queue-watch`, which the container never has; a
  CPUQuota on its unit hid the crash loop from install's is-active check until quotas were
  retired, sp-b4oct); `systemd/install.sh <instance>`
  with `SPIRA_PROD=/workspace/spira SPIRA_INSTALL_FORCE=1 SPIRA_RUN=/tmp/spira-batch-<i>
  SPIRA_TESTDB_DATA=/tmp/spira-batch-<i>/testdb`. Any failure → rc 3.
* **Requirements**: each distinct `# requires:` token (bar `testenv`) is checked once with
  `command -v` inside the container.
* **Test databases (sp-34ru2, DESIGN-testdb.md §2.4)**: the server-mode template is built
  first, for every batch. When it builds, every suite gets `SPIRA_TESTDB_MODE=server
  TESTDB_SHARED=0 TESTDB_NAME= TESTDB_DIR=` — a private sql-server fixture per suite, so no
  `bd` call opens an embedded store — and the embedded baseline below is not built. Only
  when the template fails is the baseline built, as follows.
* **Test-DB baseline** (fallback): `. /workspace/spira/testdb.sh && testdb_up batch_baseline`, which
  prints `TESTDB_NAME/DIR/BASELINE/BD/BIN/MODE=`. Complete → every suite gets
  `TESTDB_SHARED=1` and those six values (a ~26 ms copy each); otherwise
  `TESTDB_SHARED=0 TESTDB_NAME= TESTDB_DIR=` (a ~6 s `bd init` each).
* **Server-mode template** (sp-v2lqd; built for every batch since sp-34ru2): formerly only when a runnable suite carried `# testdb-mode:
  server`, `<stage>/bin/testenv testdb template --bd bd --dolt dolt` builds the
  pre-initialised store once; each such suite then gets a private Dolt sql-server copied
  from it (~0.1 s) instead of sharing `dolt-beads-test.service`. See DESIGN-testdb.md.
* **Per-suite exec**: `podman exec --user spirauser -e ... <name> bash
  /workspace/spira/<suite>`, output captured to a file, wrapped in the per-suite timeout
  (rc 124). Parallel mode adds a private `HOME=/tmp/spira-batch-<i>-<n>/home` (created
  first, with `.config/systemd/user`), `SPIRA_INSTANCE=<i>-<n>`,
  `SPIRA_RUN=/tmp/spira-batch-<i>-<n>`.
* **Every** exec that runs harness code (configure, suspend, install, baseline, suites)
  carries `SPIRA_RELEASE=<stage>` and `PATH=<stage>/bin:<stage>/spira:<image PATH>` (§5).
* **Teardown** (always, including on SIGINT/SIGTERM): only while the owner file still names
  us, `testenv container down --name <n> --volumes`; remove the host home; release the owner file
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

## 5. The staged release: one PATH, set by the launcher (sp-isom7)

Production runs one thing, a release (`spira-releases/<sha>`: the commit's tree plus `bin/`),
and every launcher sets PATH outright to `$SPIRA_RELEASE/bin:$SPIRA_RELEASE/spira:` then the
system directories (brain `runtime-is-a-release-2026-09-29`). A suite running inside testenv
is the one case where the system under test is used, so testenv is that suite's launcher and
follows the same rule against a different release:

1. **Stage.** After the container is up and before configure (whether or not the install
   runs), testenv stages the tree under test as a release at `/tmp/spira-release-<instance>`
   inside the container (`fixture::STAGE_SCRIPT`): every top-level entry of `/workspace`
   linked in, `target/` left out, and `bin/` holding one link per workspace binary target
   (`prebuilt::required` — the same list `--artifacts` validates; with `--artifacts`, the
   staged set) into `target/<profile-dir>`. A binary the build did not produce is a refusal
   naming it, `VERDICT FAULT rc=3 reason=stage`; `bin/` is never partial.
2. **Set PATH outright.** Every exec that runs harness code — configure, unit suspend,
   install, the requirement checks, the test-DB baseline and template, every suite — carries
   `SPIRA_RELEASE=<stage>` and `PATH=<stage>/bin:<stage>/spira:<image PATH>` (the image's
   own `PATH`, `fixture::IMAGE_PATH`). Nothing is appended to an inherited PATH.
3. **Render against it.** configure gets `CONFIGURE_PROD=<stage>/spira` and install
   `SPIRA_PROD=<stage>/spira`, so the units installed in the container name
   `<stage>/bin/<tool>` and carry the same `Environment=PATH=` a production unit does.

`SPIRA_ARTIFACTS`, `SPIRA_ARTIFACTS_ROOT`, `SPIRA_TEST_PLAN_BIN` and `TESTDB_TESTENV` are
gone: nothing in the tree reads them, and a suite finds `testenv`, `test-plan` and every
other tool by name. `testlib.sh` does not touch PATH. Host-side helpers (`gate-timing.sh`,
`gate-diag.sh`, `incident.sh`, `mail`) run on testenv's own PATH — the running release's,
set by whoever launched testenv. testenv still strips an inherited `SPIRA_ARTIFACTS` from
cargo and `testenv.sh`, as hygiene against a stale shell.

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
* **D2 — testenv.sh stays the image/boot owner.** *(Retired 2026-09-30 by D16, §12: the
  script is deleted and its contract is `testenv container`.)* Image acquisition (tag = build-closure
  hash, registry pull, build), boot and probe are testenv.sh's contract with other callers
  too (gate.yml publishes images through it). testenv drives it through the runtime trait;
  everything testenv-batch.sh did with podman directly is Rust.
* **D3 — the bd timing shim is dropped.** testenv-batch.sh created `/tmp/bd-shim-<i>/bd`
  but never put it on any PATH (`SPIRA_PATH` was always passed empty), so it never ran.
  The env it passed (`SPIRA_PATH=`, `SPIRA_BD_LOG`, `SPIRA_BD_TIMING_LOG`) is kept, as is
  reading those logs into `bd_calls`/`bd_ms`/`timing.tsv`.
  **Amended 2026-09-29 (sp-cln99):** nothing wrote `SPIRA_BD_LOG`, so `bd_calls`/`bd_ms`
  read 0 on every row. `bd-meter` (§3.4) now writes it — Rust, installed as the suite HOME's
  `bd`, not a shim on `SPIRA_PATH`.
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
  * **Clock.** As first written: starts when the suite phase starts, setup outside it.
    **Amended by D9 (sp-govet, §11):** starts when testenv starts; setup is inside it,
    bounded by its share.
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
  * **CI passes it twice**: gate.yml's suites step runs `testenv --artifacts
    "$SPIRA_RELEASE/bin"`, and its serial re-run of reds, `gate-retry.sh`, takes
    `GATE_RETRY_ARTIFACTS="$SPIRA_RELEASE/bin"` and passes the same flag — otherwise the first
    red would turn into a `no-cargo` fault on the retry. `$SPIRA_RELEASE` is the release the
    suites job stages from its downloaded build (`release build --bin-dir`, sp-6cbna), so the
    runner is a launcher like any other: PATH is that release's, set once for every step.

## 8. Cutover (bash and workflow edits for the operator — none made here)

Invocation: from a checkout, `"$(spira_bin testenv)"` (the installed release in
production; `SPIRA_ARTIFACTS` inside a test run). A round that tests its own runner uses
`cargo run -q --profile release -p testenv --` from its worktree.

| file:line | current | replacement |
|---|---|---|
| `.github/workflows/gate.yml:508` | `printf '%s\n' "$_s" \| bash spira/testenv-batch.sh --suites - "${{ github.sha }}" \|\| _b=$?` | `printf '%s\n' "$_s" \| bin/testenv --artifacts bin --suites - "${{ github.sha }}" \|\| _b=$?` — **corrected 2026-09-29 (D8)**: this row first said "the run rebuilds in place", but the `suites` runner has no cargo, so that faulted `no-cargo` on every PR |
| `.github/workflows/gate.yml:214-225,317-327` | `make build`, copy the release profile's executables into `bin/`, download into `bin/` | keep: `bin/` is the `--artifacts` directory (D8), and the source of `bin/testenv` — **amended 2026-09-30 (sp-6cbna)**: the suites job downloads into the runner's temp directory, stages a release from it (`release build --bin-dir`) and sets `SPIRA_RELEASE`/PATH through `GITHUB_ENV`; `--artifacts` names `$SPIRA_RELEASE/bin`. A `bin/` inside the checkout was scanned by spira-lint as untracked source |
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
  and on that day it was the larger half. **Closed by D9/D10 (sp-govet, §11).**

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

## 11. The gate trial under one budget (sp-govet)

### 11.1 Intent

A gate trial (`--deadline S`, the only caller that passes it is the gate string) is judged
**inside its budget, all of it**: the setup around the suites counts, a setup that cannot
finish in its share is a named NO_VERDICT and never a silent overrun, and the setup a gate
pays is small because the container it runs in was booted **before** it was asked for —
never reused, so no trial can see another's state.

Measured before this change (DESIGN F3, the gate log of the rewrite waves, and a probe
branch touching `spira/watchtower.sh`, 15 suites):

| phase | quiet host, 2026-09-30 02:23Z | loaded host, 2026-09-29 (F3) |
|---|---|---|
| resolve, select, key, image tag | 0.2 s | — |
| cargo build (the gate's fresh worktree) | 20.4 s | 44 s |
| orphan sweep + owner claim | 0.1 s | 55 s |
| `testenv.sh up` + probe | 2.3 s | 178 s |
| configure + suspend + install | 10.9 s | 12 s |
| requirements + testdb template | 4.6 s | 11 s |
| **suite phase** | 75.5 s | 61 s (deadline 60) |
| results, gate-diag/timing, teardown | 6 s | 16 s |
| **total** | 120 s | 381 s |

Across the 22 wave trials (2026-09-30 00:31–02:16Z) setup ran 23–258 s (median 48 s, p90
~170 s) on top of a suite phase that alone could use the whole 300 s. Setup is not one
cost: on a quiet host it is the cold build; on a loaded one it is the podman control plane
(`podman ps`/`run`/systemd boot under contention) — both are what a pre-booted container in
a warm checkout removes from the trial's critical path.

### 11.2 Contract

**The budget (D9).** `--deadline S` starts at process start. Two instants follow:

* `setup_cutoff = start + S × share / 100`, share = `SPIRA_TESTENV_SETUP_SHARE` (integer
  percent, default 50, clamped to 10..=90 so the suites always get at least 10 %).
  Every setup phase is bounded by it — the build (cargo is killed), the image tag, the
  container (`testenv.sh up`/`probe`, or claiming a spare), configure/suspend/install, the
  requirement checks and the test databases (their execs carry it as their deadline). A
  phase still running at the cutoff is killed, and the run ends
  `VERDICT FAULT rc=2 ran=0 reason=deadline-<phase>` with a log line naming the phase, its
  share and the budget. rc 2 is what the gate string maps to 75: **NO_VERDICT, never red,
  never green** (a killed build is `deadline-build`, never the candidate's rc 4).
  `<phase>` ∈ `build | tag | sweep | up | install | requirements | testdb`.
* `deadline_at = start + S`: the suite phase's hard cut (D7's mechanism, unchanged) is
  `deadline_at`, not "S after the suites start".
* **A trial that judged nothing is not a pass.** When suites were runnable and every one of
  them was deferred (`ran = 0`, `deferred > 0`), the run is
  `VERDICT FAULT rc=2 reason=deadline-suites`, not `VERDICT GREEN ran=0`.

Without `--deadline` none of this applies: no cutoff, no warm path, same output.

**The warm path (D10).** Under `--deadline`, unless `--artifacts` is given or
`SPIRA_TESTENV_WARM_SLOTS=0`, testenv tries `SPIRA_TESTENV_WARM_SLOTS` (default 3) warm
slots before the §4.1 rules:

| thing | where |
|---|---|
| slot worktree | `$SPIRA_RUN/worktree/.testenv-warm-<i>` — a detached worktree of the repo, reset per trial exactly as a scratch slot (`checkout --detach --force`, `clean -ffdx -e /target`), so `target/` stays warm |
| slot lock | `$SPIRA_RUN/worktree/.testenv-warm-<i>.lock`, `flock` exclusive, non-blocking; held for the whole trial |
| spare record | `$SPIRA_RUN/worktree/.testenv-warm-<i>.spare`: `name=<container> tag=<image tag> booted=<epoch>` lines, written atomically (rename) only after the spare booted and probed |
| spare container | `spira-warm-<i>-<epoch>-<pid>-<seq>`, booted by `testenv container up --name <n> --checkout <slot worktree>` — the slot is bind-mounted at `/workspace`, so the trial's checkout into the slot is what the spare sees |

* **Claim.** With the slot lock held and the tree checked out and built, the trial reads
  the spare record and **deletes it before using the container** — a spare is claimable at
  most once, even if the trial dies. It is used only if it is running, its tag is the
  current image tag, a nonce the trial writes to `<slot>/target/.testenv-warm-nonce` reads
  back identically from `/workspace/target/.testenv-warm-nonce` inside it (it mounts this
  slot and sees this checkout), and `testenv container probe` passes. Otherwise it is purged and
  the trial boots its own container on the slot (`warm=cold`) — the same work as before,
  with a warm build.
* **Never reused (isolation by construction).** The claimed container is torn down at the
  end of the trial exactly like a cold one (§4.2 teardown). No container ever runs suites
  for two trials. The slot's checkout is reset by git and the container's user (uid 1001,
  a sub-uid on the host) cannot write the bind-mounted host checkout.
* **Refill.** When a warm trial ends, testenv spawns `testenv warm refill <i>` detached
  (`setsid`, stdio to `$SPIRA_RUN/testenv-warm.log`). The refiller waits for the slot lock
  (bounded by `SPIRA_TESTENV_WARM_BOOT_TIMEOUT`, default 600 s), runs the orphan sweeps (moved
  off the trial's critical path, §4.2), boots the spare (bounded by the same timeout), probes
  it, and writes the record. A refill that fails leaves no record; the next trial boots cold.
* **Sweep of warm containers.** `spira-warm-*` containers that no spare record names and
  whose owner file names a dead pid (or none, when older than the orphan min age) are
  purged by the refiller's sweep.
* A spare holds one of `testenv container`'s admission slots (`SPIRA_TESTENV_MAX_CONCURRENT`) while
  idle.

**Phase timings (D11).** Every trial records its phases, in order, in seconds:
`resolve, build, sweep, up, install, requirements, testdb, suites, post, teardown`
(`up` covers claiming a spare). Written to the log line `phases: …`, `batch.meta` (`phases=`,
`warm=`) and the `__batch__` suite-timing row (`setup_secs`, `phases`, `warm`, §3.4).
`setup_secs` = the time from start to the first suite's launch. A trial cut at its setup share still writes its `__batch__` row, with `rc` 2 and the
cut phase last in `phases`, so an overrun is visible to every reader of the family.

### 11.3 Decisions

* **D9 — the budget is the trial's, not the suites'.** The operator order is that a gate is
  cut at its budget; a 300 s suite phase inside a 460 s trial obeys the letter and misses
  the point. Setup gets a share, not the whole budget, so a trial that spent 280 s in
  `podman run` cannot leave its suites 20 s and call the result a gate. A setup overrun is
  a harness fault (rc 2 → NO_VERDICT) because nothing about the branch was judged.
  *Rejected:* raising the budget; letting setup run unbounded and subtracting it (the
  overrun the bead exists to end).
* **D10 — a pre-booted, never-reused container per warm slot.** *Rejected:* one long-lived
  container reused across trials — isolation would then rest on a cleaning list (tmp, homes,
  user units, lingering processes, dolt servers, cargo volumes) that is only as good as its
  author's imagination; a podman image snapshot of an installed container — install
  depends on the tree under test, so it cannot be baked ahead of the tree. Booting the
  *next* trial's container while nobody waits is the whole saving and costs no isolation.
  *Rejected:* making `/workspace` a symlink into a shared mount of every worktree — every
  trial could then read every other trial's tree.
* **D10a — the warm path is chosen by `--deadline`.** Rounds (`--profile release`) must
  build in their own worktree (§4.1, land reads `target/release` there); aeon runs have
  their own warm worktree. The gate trial is the one caller that pays setup on every run.
* **D10b — the refill is detached, and failure is quiet.** A refill failing (image pull,
  admission timeout) only costs the next trial a cold boot, which is today's path; it must
  not fail the trial that spawned it, and it runs after that trial's verdict.
* **D11 — phases go on the `__batch__` row**, the one end-to-end row per run, so the
  suite-timing family's per-suite readers (LPT order, medians, bd wait) are untouched.

### 11.4 The setup in one exec (sp-t26yx)

**The fault.** Under concurrent load (2026-09-30: four gate trials, landing-pass certifying
four gates, other agents' testenv runs) trials ended `NO_VERDICT` in setup:
`deadline-install` and `deadline-testdb`. In a reproduction (four concurrent trials of eight
suites, `--deadline 300`) all four ended NO_VERDICT: install phases of 29–120 s, one
`ctrl.sh suspend` sequence alone 104 s, where a quiet host takes 11 s for the whole install.

**The cause is podman, not the scripts.** Every `podman exec` takes podman's global locks:
the libpod database (sqlite in rollback-journal mode — `db.sql-journal`, `fdatasync` on every
commit, readers spin on the PENDING byte while a writer commits) and the storage
`overlay-layers/layers.lock` (held by every container create/remove and image export).
Sampled while trials ran: `podman exec <idle container> true` 0.3–0.8 s quiet, **24–73 s**
loaded, `podman ps` up to 130 s; an strace of one 67 s exec shows 37 `fdatasync`s on
`db.sql` (7.5 s) and a 53 s blocking wait on `layers.lock`. The setup paid that toll once per
step: stage, configure, three suspends, install, one `command -v` per requirement token and
the testdb template — 7 + k execs.

**Contract.** The setup is **one** `podman exec`: the host links its own executable into
the worktree (`target/.testenv-runner/testenv`, a hard link, else a copy) and runs
`/workspace/target/.testenv-runner/testenv plan <json>` as the suite user under the setup
cutoff. The plan is the exact list of requests that used to be execs, each with its argv and
env, its phase (`install`, `requirements`, `testdb`) and whether it must succeed. The runner
executes them in order and frames each result (`<nonce> begin <name>` /
`<nonce> end <name> rc= ms= len=` + output); a failing `must` step ends the plan. The host
reads it back:

* a failed stage/configure/suspend/install step is the fault its own exec was (rc 3,
  reasons `stage` / `install`, same messages and output tails);
* a requirement step that fails is unmet (SKIP-REQ) — never a fault;
* the template's rc and output feed §2.4's server/embedded choice exactly as before;
* an exec killed at the cutoff is `deadline-<phase of the step it was in>` (the one that
  began and never ended, else the first without a result) — never "unmet", never a
  template failure;
* a runner that returns fewer results than steps, without a deadline, is a harness fault
  (rc 3, `stage`) — never a pass.

The runner is the *host's* testenv, never the candidate's, so the two always speak the same
plan format whatever tree is under test. Phases are unchanged (D11): the in-container time
of each phase is measured by the runner; the exec's own podman overhead is added to
`install`, and a log line names it: `setup in one exec: <wall>s wall, <inside>s in the
container (<step> <s>, …) — podman exec overhead <s>`.

### 11.5 Decisions (sp-t26yx)

* **D12 — a gate trial never sweeps inline.** Off the warm path (a scratch slot, all warm
  slots busy) a `--deadline` trial spawns `testenv warm sweep` detached (one sweeper at a
  time, `flock` on `$SPIRA_RUN/testenv-sweep.lock`) instead of running both orphan sweeps on
  its critical path: their podman calls cannot be cut, and one sweep ran **nine minutes**
  (10:53–11:02Z), past the trial's whole 300 s budget. Without `--deadline` the sweep is
  unchanged.
* **D13 — one exec, not a faster exec.** *Rejected:* longer setup share or timeouts (the
  operator rule; the cost is lock queueing, which a longer wait only hides); one
  `podman exec` per step with retries (multiplies the toll); running setup through
  `podman run` arguments (the container is booted before the tree is built — warm spares).
* **D14 — test databases live on the container tmpfs** (DESIGN-testdb.md §2.5).
* **D16 — scratch and warm slots live on a tmpfs, bounded, fail closed.** A slot is
  disposable build state: cargo's `target/` (1.4–2.4 GB a slot, seven slots) is rewritten
  on every relink, and on the host's one virtual disk (66–100 % utilised, IO pressure
  `full` 30–70 % under gate load) it competed with production Dolt and podman's own
  database. The slots' root is `worktree::scratch_root($SPIRA_RUN)`:
  `$SPIRA_TESTENV_SCRATCH` if set; else `$SPIRA_RUN/worktree` when `$SPIRA_RUN` is itself on
  a tmpfs (unit tests); else `/tmp/spira-testenv-<sha256(SPIRA_RUN)[..6]>` when `/tmp` is a
  tmpfs; else `$SPIRA_RUN/worktree`. Slot locks and spare records move with the slots.
  Before a scratch or warm slot is used, the root must have
  `SPIRA_TESTENV_SCRATCH_MIN_FREE_MIB` (default 4096) free and, on a tmpfs,
  `SPIRA_TESTENV_SCRATCH_MIN_MEM_MIB` (default 8192) of MemAvailable — tmpfs pages are RAM,
  and a build that pushed the host into swap would put it back on the disk. Short of either:
  no warm slot, and a scratch slot is refused with `VERDICT FAULT rc=2
  reason=scratch-short` — never a silent fallback to the disk. The caller's own worktree
  (in place) is untouched. After the move every slot starts cold once (cargo keys its
  fingerprints on the workspace path, so the old `target/` cannot be carried over); the old
  disk slots under `$SPIRA_RUN/worktree/.testenv-{slot,warm}-*` are no longer read and can
  be removed with `git worktree remove`.
* **D15 — the refill boots through the slot's own harness.** `testenv warm refill <i>` is
  spawned with `SPIRA_TESTENV_HARNESS=<slot worktree>` and cwd `$SPIRA_RUN`. A gate's testenv
  lives in a transient `.gate.harness.*` worktree the gate removes the moment testenv exits;
  the refill it spawned then ran that worktree's `testenv.sh`, found it gone
  (`testenv-warm.log`: `…/.gate.harness.concierge-sp-9thdw/spira/testenv.sh: No such file
  or directory` → `warm refill 0: no image tag — no spare`), so gate trials never found a
  spare and every one paid a cold `up` — the `deadline-up` NO_VERDICTs. The slot holds the
  tree the trial just tested and outlives it.
* **D22 — a `harness-fault` line names the container and never fabricates an exit reason it
  never read (sp-2zu0t).** Five gate trials on 2026-09-30 (concierge/sp-48f6g,
  concierge/sp-31dm0, concierge/sp-zpaq0; ~19:18–21:20Z, ~90 min, under the current 3-slot
  admission) ended `NO_VERDICT reason=harness-fault`, each after the suite phase had already
  run for several minutes — not a setup-time fault. `podman events` for the window shows
  short-lived `spira-batch-*`/`spira-warm-*` containers dying across several concurrently
  admitted trials, the same podman control-plane contention §11.4 named for the setup phase
  (libpod's sqlite lock, `layers.lock`) — no kernel OOM and no podman `container oom` event
  appear anywhere in the journal for the window, so it is not memory exhaustion. Two defects
  followed from reading [`Session::liveness`]: (1) the detail carried only `ExitCode=`/
  `OOMKilled=`, with no container name — by the time a human reads the gate's output the
  container is already `podman rm`'d, so the fault cannot be correlated to *which* container,
  slot, or concurrent trial died; (2) `liveness_retries` exhausted by failed/empty `podman
  inspect` answers (never a definitive `Running=false`) was reported exactly like a confirmed
  death, asserting an `ExitCode`/`OOMKilled` that were themselves read from a container whose
  state was never actually observed — conflating "the lookup is unreliable under load" with
  "the container died". Fixed: the detail always names the container
  (`container=<name> …`), and the two cases are worded distinctly — a confirmed `Running=false`
  still reports `ExitCode=`/`OOMKilled=`, but an inspect that never got a definitive answer
  reports `container=<name> state unknown — <n> consecutive \`podman inspect\` lookups failed
  or returned no answer (never saw Running=false)` instead. No plumbing changed: `batch.rs`,
  `run.rs` and `gate/src/parse.rs::harness_fault_detail` already carry the detail opaquely
  through to the gate's NO_VERDICT message, so the fix is confined to
  `fixture.rs::Session::liveness`. *Not fixed here*: whether admission concurrency
  (`SPIRA_CERTIFY_PAR`) against warm-slot count (`SPIRA_TESTENV_WARM_SLOTS`, both currently 3)
  should be widened, and whether the per-suite exec loop should get D13's one-exec treatment —
  reported, not landed, since that is a production-config and cross-cutting-design call
  outside one bead's diff.

**Measured (same eight suites, `--deadline 300`, 2026-09-30).**

| run | concurrency | setup NO_VERDICTs | setup s | post s | host disk writes |
|---|---|---|---|---|---|
| before, loaded (world running) | 4 | 4 of 4 (install, testdb) | 139–150+ | — | — |
| before, loaded + a 126-suite corpus | 4 + 1 | 4 of 4 (2 install, 2 testdb) | cut at 150 | — | — |
| after (one exec, tmpfs testdb), loaded | 4 | 0 of 4 | 55–76 | 91–136 | — |
| before, world stopped | 2 | 0 of 2 | 34 | 115 | 1.87 GB |
| after (+ tmpfs slots, spares), world stopped | 2 | 0 of 2 | 26 | 7 | 0.10 GB |

With the slots on tmpfs a trial's container upper dir held 348 KB (32 files) mid-suite, so
the container adds nothing to the disk either.

**Not fixed here (reported).** The podman control plane itself (libpod sqlite and
`layers.lock` on the saturated disk) still bounds `testenv.sh up` for a cold trial and the
teardown; an external `podman save` of the 2 GB testenv image ran at least 16 minutes during the
measurements (10:51–11:07Z+), and the two probe rounds in that window were the worst. Those are host/operator matters (podman's
`static_dir`/database backend, and what else runs image operations on the gate host).

## 12. The container driver — `testenv container` (sp-s0e1k, replaces spira/testenv.sh)

### 12.1 Intent

One program owns the fixture container end to end. Until sp-s0e1k the runner (this crate)
drove containers through `spira/testenv.sh` (769 lines of bash) for the four things it did
not own — the image tag, image acquisition, boot, and teardown — and every other caller
(round-vm, gate.yml, testenv-image.yml, landing-pass halt, acceptance-local.sh and a dozen
suites) called the script directly. D2 kept it that way while the runner was being written;
this section retires D2. The script is deleted; its contract is the `container` subcommand
of this binary, and the runner's `ContainerRuntime::testenv` seam runs **its own
executable** (`<current_exe> container …`) as a child process, so the deadline kill
(D9, a process-group kill) and the owner-file protocol (the owner is the child's parent)
are exactly what they were.

What matters, and is ported:

1. **The image is named by its build closure** — sha256 over the Containerfile's content,
   the bd pin file's content and `deps.toml`'s content, 12 hex characters. A pulled image is
   exactly as safe as a built one. Nothing floating is ever pushed.
2. **Acquire, then build.** Local image → registry pull (`SPIRA_TESTENV_REGISTRY`, retagged
   to the local name) → build. A miss is slow, never fatal. A cold build prints a heartbeat
   line (furthest `STEP`, last output line, elapsed, free disk and memory) at least every
   `SPIRA_TESTENV_BUILD_HEARTBEAT` seconds (60), and a failed build names disk exhaustion.
3. **Admission.** At most `SPIRA_TESTENV_MAX_CONCURRENT` (8; 0 disables) containers
   labelled `spira.testenv=1` run at once; `up` queues, polling every
   `SPIRA_TESTENV_QUEUE_POLL` (5) seconds, and gives up after `SPIRA_TESTENV_QUEUE_TIMEOUT`
   (900) seconds.
4. **Boot.** `podman run -d --systemd=true --pids-limit 8192 --label spira.testenv=1`, the
   checkout bind-mounted at `/workspace`, plus one bind mount per distinct directory
   `Host::symlinked_targets(<checkout>/target)` names (D23) — empty for an ordinary
   checkout — the two named cargo volumes; wait for `basic.target` (one retry); on failure
   measure inotify, pids, the user slice's tasks and the keyring, name the one closest to
   its cap, and remove the container. Then linger, `safe.directory`, cargo-volume
   ownership, and wait for `user@1001.service`.
5. **The owner-file guard** (law-guard-binds-the-caller). `up` records its parent pid in
   `/tmp/<name>.owner` unless a caller claimed it first; `down` refuses a live owner that is
   not the caller or an ancestor of it, unless `--force-foreign`.

### 12.2 Contract

```
testenv container up      [--name N] [--checkout PATH]
testenv container down    [--name N] [--volumes] [--force-foreign]
testenv container exec    [--name N] [--user U] [--] CMD ARGS...
testenv container probe   [--name N]
testenv container tag                 # the build-closure hash, stdout
testenv container image               # acquire the image; its local ref, stdout
testenv container publish             # push it to SPIRA_TESTENV_REGISTRY; the remote ref, stdout
```

Defaults, flags, stdout/stderr and exit statuses are the script's (`N` = `spira-testenv`,
`exec` runs as root and adds `XDG_RUNTIME_DIR`, `DBUS_SESSION_BUS_ADDRESS`, `CARGO_HOME`,
`CARGO_TARGET_DIR` for `spirauser`; `exec` replaces this process with `podman exec`, so its
status and stdio are podman's). Progress goes to stderr prefixed `testenv:`.

**The harness** is `SPIRA_TESTENV_HARNESS`, else the nearest ancestor of the executable that
holds `spira/testenv/Containerfile` (was: `spira/testenv.sh`) — a checkout's
`target/<p>/testenv`, a release's `bin/testenv`. `tag`, `image`, `publish` and `up` need it
(the build context is `<harness>/spira`, the Containerfile `<harness>/spira/testenv/
Containerfile`, the manifest `<harness>/spira/deps.toml`); `down`, `exec` and `probe` do not.
The default `--checkout` is the harness root.

**Settings**, each environment first, then spira.toml (through spira-config), then the
default conf.sh carries — conf.sh is no longer sourced:

| key | spira.toml | default |
|---|---|---|
| `SPIRA_BD_PIN` | `spira.bd_pin` | `$SPIRA_RUN/bd-pin` (SPIRA_RUN resolved as §2.5) |
| `SPIRA_TESTENV_REGISTRY` | `spira.testenv_registry` | empty (no registry) |
| `SPIRA_TESTENV_MAX_CONCURRENT` / `_QUEUE_TIMEOUT` / `_QUEUE_POLL` | `spira.testenv_*` | 8 / 900 / 5 |
| `SPIRA_TESTENV_BUILD_HEARTBEAT` | — | 60 |
| `SPIRA_TESTENV_BASIC_WAIT_TICKS` / `_BASIC_RETRY_SLEEP` | — | 20 / 2 |

### 12.3 Decisions

* **D16 — D2 is retired: testenv owns the image and the boot.** The script's callers are
  repointed to `testenv container <sub>` by bare name on the launcher PATH (landing-pass
  halt, acceptance-local.sh, round-vm's VM script, gate.yml) or to the tree's own build
  (round-vm's template VM, which has an unpacked tree and cargo but no release:
  `cargo run -q --release -p testenv -- container image`; testenv-image.yml, likewise).
* **D17 — the runner calls itself.** `Podman::testenv` spawns `current_exe() container …`
  rather than calling the module in-process: the deadline is a process-group kill, the owner
  pid `up` records must be the runner's, and a wedged podman must not hold the runner's own
  threads. `SPIRA_ARTIFACTS`/`SPIRA_ARTIFACTS_ROOT` are still removed from its environment.
* **D18 — fail closed where the script guessed.** (a) `tag` refuses (rc 1, no output) when
  the Containerfile or `deps.toml` cannot be read; the script hashed whatever it could read
  and printed a tag for an image nothing could build. The bd pin stays optional — CI and a
  fresh install have none, and the script's tags on those machines must not move. (b) `up`
  refuses at once when `podman run` fails, instead of recording an owner and spending the
  basic.target wait (≈22 s) on a container that was never created. (c) `tag`/`image`/
  `publish`/`up` without a harness refuse naming it.
* **D19 — `scratch` and `shell` are retired, not ported.** Their only caller was their own
  suite (test-testenv-scratch.sh) and a README line; both sourced `testdb.sh`, which the
  testdb crate replaces (`testenv testdb up` prints a throwaway database's paths).
* **D20 — the stub-podman suites are retired for unit tests.** test-testenv-image-tag.sh,
  test-testenv-registry.sh, test-testenv-image-heartbeat.sh,
  test-testenv-resource-diagnosis.sh and test-testenv-owner-guard.sh drove the script
  against a podman stub (or, for the owner guard, real throwaway containers whose only
  discriminating fact was the owner file); `container::tests` drives the same cases through
  the `Host` port. test-testenv.sh and test-testenv-systemctl.sh (real podman, real systemd)
  and every suite that boots its own container are repointed.
* **D21 — the in-container cargo-volume chown stays one `sh -c`.** It runs inside the
  container, as root, on paths the image defines; one exec instead of nine.
* **Dropped:** the `_image_tag` "narrow closure" positive control (it tested `sha256sum`);
  `cut -c` byte-truncation of heartbeat lines (now characters); `df -Ph`/`free -h` (now statvfs and `/proc/meminfo` MemAvailable, same units).
* **D23 — a symlinked build directory gets its own mount too, so a bind-mounted checkout's
  `/workspace` can still see what it points to (sp-e5v53-3).** Found 2026-10-01: a gate
  tree's `target/{aeon,release,debug,gate-tools}` are each a symlink to a tmpfs root
  *outside* the checkout (gate/src/target.rs, sp-z61hj — "every path a step or the tools
  phase reads is unchanged; only where the bytes land moves"). `up`'s bind mount of the
  checkout preserves that symlink as a symlink; a binary built right through it is
  genuinely there on the host, but `/workspace/target/aeon` inside the container resolves
  to a host path nothing mounted there, so `stage` faults "aeon was not built into
  /workspace/target/aeon" — a suite the gate's own targeted base-suite rerun (sp-kqger,
  sp-e5v53, sp-e5v53-2) was the first caller to hit, because it is the one caller that
  builds "in place" directly inside a gate tree rather than a warm or scratch slot (those
  are ordinary directories, never symlinked this way). Reproduced directly: `cargo build
  --profile aeon --workspace` in such a tree succeeds; `stage` still faults rc=1, same
  message, until the extra mount is added. Fix: `Host::symlinked_targets(dir)` lists each
  of `dir`'s immediate entries that is a symlink, resolved to its target's own canonical
  parent, each listed once — every `[LINKED]` entry in a gate tree shares one root
  (`gate/src/target.rs`), so this is one extra mount, not four. `cmd_up` adds a
  `--volume <path>:<path>:z` for each. An ordinary (non-gate) checkout, whose `target/` is
  a real directory, gets nothing extra — the existing `up_boots_with_the_label_limit_and_
  volumes_and_records_its_caller` test is the fixture's own positive control that it stays
  that way.

## Build IO (sp-z61hj)

Full contract: `spira-config/DESIGN-build-cache.md`. The builder (`src/build.rs`) compiles
through the box's sccache (`RUSTC_WRAPPER`, resolved on testenv's own PATH) and one-shot
(`--config profile.<p>.incremental=false`; a caller's `CARGO_INCREMENTAL` is removed, since
sccache hashes it into every key). sccache absent is `VERDICT FAULT rc=3
reason=no-build-cache` — never a cold build of every dependency; `SPIRA_BUILD_CACHE=off` opts
out, loudly. The fixture container sets `SPIRA_BUILD_CACHE=off` (it has no sccache; testenv
builds on the host). The crate also ships **`target-reap`** (`src/reap.rs`): the `target/`
of every `$SPIRA_RUN/worktree/<bead id>` whose bead is closed is removed, on one `bd show`;
an unreadable answer removes nothing. The landing pass runs it once per pass.
