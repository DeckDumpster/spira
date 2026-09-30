# gate — design

Replaces the logic of `spira/gate.sh` and `spira/gate-lib.sh` with one Rust binary, `gate`.
`spira/gate.sh` stays as the one entry point every caller already names; it resolves
`SPIRA_GATE_BIN` through `conf.sh` and `exec`s it. Bead: sp-0tpcs (epic sp-8m1at, design
`gate-unit-round-integration-2026-09-29`, item 3). First crate of the rewrite order
(law-rust-rewrite-order: the gate first).

## Intent

The certification gate answers one question: **may this branch land on the ref it lands on?**
It answers it cheaply and mechanically (syntax, beads data, foreign harness copies, then the
repository's own gate string), fails closed, and says whose fault a refusal is.

The change this crate exists to make: the gate judges **the branch merged onto the current
landing ref**, not the branch's own tree. The bash gate checked out the branch as it was cut.
After the Rust cutover on 2026-09-29 every branch cut before it carried files `local/main` had
since deleted, and the fences flagged those files. All seven branch-reds of that day were false
(sp-7xnpv, sp-falao, sp-x1c6k), and content that was stale on the base was charged to the
branch. The merge is the tree that would actually land, so it is the tree to judge.

A branch that **does not merge** is not red. Its work may be correct; it is stale. The gate
says `reason=conflict` and the landing pass hands it to `rebase-stale`, which rebases it
mechanically or returns it to an aeon with the conflicting hunks quoted.

## Non-goals

* Changing what the repository gate string itself runs. Item 4 (sp-2ghui, "Composition" below)
  decides *around* it: whether it runs with suites on or off, and what runs after it.
* Porting `exclude.sh`, `skew.sh`, `yield.sh`, `gate-sweep.sh` or `lifecycle-cert.sh`. The
  binary calls them as the bash did; each moves in its own turn of the rewrite order.
* The verdict cache's storage format, the gate.log format and the admission pool's lock files.
  Other programs read all three (landing-pass probes `slot.N.lock`; the cockpit and
  `gate-timing.sh` read gate.log; the landing pass reads `verdicts/`).

## Contract

### Invocation

```
gate.sh <branch> [repo-name]                       # every caller, unchanged
gate [--home <spira-dir>] <branch> [repo-name]     # what gate.sh execs
gate [--home <spira-dir>] --definition [repo-name] # the landing ref's gate command (doctor.sh)
```

* `repo-name` defaults to `spira_home_repo`.
* `--home` is the directory holding `lib.sh`, `exclude.sh`, `skew.sh`, `yield.sh` and
  `gate-sweep.sh`. The default is `$SPIRA_HOME`, then `<dir of this binary>/../spira`.
* `gate.sh` execs with `exec -a "$0"`, so `/proc/<pid>/cmdline` still names `…/gate.sh` and the
  two process scans that look for a running gate (`gate-run.sh unmanaged_gate`, `world.sh
  live_workers`) keep matching.
* A missing `<branch>` prints usage and exits 1, as `${1:?}` did.

### Callers (all unchanged: they call `gate.sh`)

| caller | how | reads |
|---|---|---|
| `landing-pass` (`real.rs gate_command`) | `gate.sh <br> <repo>`, `SPIRA_GATE_LOCK_WAIT`, `SPIRA_GATE_BEAD` | exit status, the `VERDICT=` line |
| `queue submit` (`queue/src/real.rs`) | `bash gate.sh <br> <repo>`, `SPIRA_GATE_BEAD`, `SPIRA_GATE_SUITES` | exit status, output |
| `rebase-stale` | through `queue submit` | green or not |
| `batch.sh _pf_gate` | `bash gate.sh`, `SPIRA_GATE_BEAD=batch-*` | exit status |
| `gate-run.sh` (the aeon's self-certification; `aeon` reads `gate-run.sh --status`) | `bash gate.sh <br> <repo>` | exit status, output file |
| suites (`testlib/gate-fixture.sh` and the gate suites that copy `spira/`) | `bash $SH/gate.sh`, `SPIRA_GATE_BIN` passed through | output |

### Environment it reads

Everything is read **after sourcing `lib.sh`** (one seam call, below), so a value set in the
caller's environment, in `spira.toml` or by a conf.sh default reaches the gate exactly as it
reached the bash.

| variable | use | default |
|---|---|---|
| `SPIRA_REPO_MAP` | must be readable, else NO_VERDICT `no-repo-map-file` | — |
| `SPIRA_RUN` | trees, admission, gate.log, verdicts | conf.sh |
| `SPIRA_GATE_LOG` | the meter | `$SPIRA_RUN/gate.log` |
| `SPIRA_VERDICTS` | the verdict cache | `$SPIRA_RUN/verdicts` |
| `SPIRA_VERDICT_TTL` | seconds a cached PASS is good; non-numeric reads 0 | 0 |
| `SPIRA_GATE_TIMEOUT` | `timeout` on the gate command | 2700 |
| `SPIRA_GATE_LOCK_WAIT` | wait for an admission slot and for the tree lock | `4 × SPIRA_GATE_TIMEOUT` |
| `SPIRA_CERTIFY_PAR` | admission pool size; unset or non-numeric derives `min(nproc/4, MemAvailable/400MiB)`, at least 1, re-read every second | derived |
| `SPIRA_GATE_SUITES` | `off` skips admission unless the round named suites against the bead; in the key; passed through | `on` |
| `SPIRA_GATE_BEAD` | ejected-suites lookup (the re-entry check), the key, `lc_certify` | — |
| `SPIRA_GATE_CALLER` | `by=` in the cache entry | the branch |
| `LANDSTATE` (lib.sh) | `<bead>.ejected`, else an `EJECTED` landstate row | `$SPIRA_RUN/landstate` |
| `SPIRA_GATE_BUDGET`, `SPIRA_GATE_ALL`, `SPIRA_CERTIFY_ALWAYS_COVERS`, `SPIRA_BATCH_MAXPAR`, `SPIRA_VERDICT_REPEAT_CONSIDERED`, `SPIRA_LINT_BIN`, `SPIRA_TESTENV_BIN`, `SPIRA_TESTENV_SETUP_SHARE`, `SPIRA_TESTENV_WARM_SLOTS` (sp-govet), `PATH`, `HOME` | passed through to the gate command | as bash |

### The gate command's environment (unchanged list, `env -i`)

`PATH=$HOME/.cargo/bin:$PATH`, `HOME`, `TERM=dumb`, `SPIRA_GATE_REPO`, `SPIRA_GATE_REPO_NAME`,
`SPIRA_GATE_BRANCH`, `SPIRA_GATE_BASE`, `SPIRA_GATE_SELECT_HEAD=<branch>`, `SPIRA_GATE_FILES`,
`SPIRA_GATE_HOST_CORES`, `SPIRA_GATE_EJECTED_SUITES`, `SPIRA_GATE_ALL` (default 0),
`SPIRA_GATE_SUITES` (default on), `SPIRA_CERTIFY_ALWAYS_COVERS`, `SPIRA_BATCH_MAXPAR`,
`SPIRA_VERDICT_REPEAT_CONSIDERED`, `SPIRA_GATE_BUDGET` (default 300), `SPIRA_RUN`,
`SPIRA_LINT_BIN`, `SPIRA_TESTENV_BIN`, `SPIRA_TESTENV_SETUP_SHARE`, `SPIRA_TESTENV_WARM_SLOTS`. Run as `timeout $SPIRA_GATE_TIMEOUT bash -c "$CMD"` in
the gate tree, stdout and stderr on one pipe. No lock descriptor reaches it (every descriptor
this binary opens is close-on-exec; the bash leaked the admission slot's fd 8).

**One value changes meaning: `SPIRA_GATE_BRANCH`** in the branch trial is the revision under
test. When the landing ref is already an ancestor of the branch that is the branch name, as
before. Otherwise it is the **merge commit's id** (below), because the repository's gate string
hands it to `testenv`, which builds its own worktree from it. `SPIRA_GATE_SELECT_HEAD` stays
the branch, so suite selection still reads what the branch changed. In the base trial it is the
landing ref, as before, but pinned: the commit it resolved to when the gate started.

**`SPIRA_GATE_BASE` is that pinned commit's id, not the ref's name** (sp-hh5h0). The ref moves
while the gate runs, and both trials must select against the same base.

### The merge (the change)

After the landing ref (`spira_landref`) and the branch both resolve:

0. **The base is pinned** (sp-hh5h0). The landing ref is resolved to a commit `BASE` once,
   here; one that does not resolve is NO_VERDICT `no-base`. Every later read uses that
   commit, never the ref's name again: the changed-file list, the merge, `skew.sh`, the
   preflight, the touched set, the gate command's `SPIRA_GATE_BASE`, and the base trial's
   checkout and `SPIRA_GATE_BRANCH`. The name appears only in messages. The ref moves while
   a trial runs, because a landing lands. On 2026-09-29 the gate re-read it by name for the
   base trial, judged a newer base that had fixed `test-gate-verdict.sh`, and charged a red
   the merge's own base shared to `concierge/sp-b4oct` as branch-red.

1. `git merge-tree --write-tree --name-only --no-messages BASE BR`.
   * exit 0: the merged tree `T`.
   * exit 1: **conflict** → NO_VERDICT, `reason=conflict`, the conflicted paths in the message.
   * anything else: NO_VERDICT, `reason=merge-failed`.
2. If `BASE` is an ancestor of `BR` the gate revision is `BR` itself (its tree is `T`).
   Otherwise it is `git commit-tree T -p BASE -p BR` with a fixed identity and a fixed date, so
   the same pair always yields the same commit id. The commit is unreferenced; it lives for
   git's prune grace, which outlasts any trial.
3. Everything that asks "what would land" reads the gate revision: `bash -n` on the changed
   `.sh` files, the beads-data filter over the whole tree, the checkout the repository gate
   string runs in, and the tree id in the verdict key.

What still reads the branch: the changed-file list (`BASE...BR`, what the branch changed),
`skew.sh foreign` (a diff, same), `SPIRA_GATE_SELECT_HEAD`, the yield record's tree (whether the
*branch* changed between a red and a pass), and `lc_certify`'s tip.

### Exit status and the VERDICT line

The last line on stderr is always

```
gate: VERDICT=<PASS|FAIL|BASE_FAIL|NO_VERDICT> reason=<slug> branch=<br> repo=<name> suite=<suite|->
```

| exit | VERDICT | who is at fault |
|---|---|---|
| 0 | PASS | — |
| 1 | FAIL | the branch |
| 76 | BASE_FAIL | the base |
| 75 | NO_VERDICT | the machinery — or, for `reason=conflict`, nobody: the branch is stale |

**CONFLICT IS A NO_VERDICT, not a fifth status.** Every caller already treats 75 as "not
judged, charge nothing, retry"; a new number would read as FAIL (`spira_gate_blames_branch`,
`GateOutcome::of`) at every caller not taught it, and poison correct work. The one caller
that acts on it, the landing pass, matches `reason=conflict` on the VERDICT line.

Reasons: PASS `syntax-only | cached | pass`; FAIL `syntax | beads-data | foreign-harness |
branch-red`; BASE_FAIL `base-red`; NO_VERDICT `no-repo-map-file | no-repo-map | no-base |
no-diff | conflict | merge-failed | missing-exclude | missing-skew | skew-init-fault |
cmd-missing-file | admission-timeout | no-lockfile | lock-timeout | tree-unidentified |
harness-fault | timeout | base-timeout | base-untestable | lib-unavailable | died |
reentry-unproven | fence-silent`, and
`no-evidence:<reason>` when a FAIL carried no message (downgraded to NO_VERDICT).

The message before the VERDICT line is the bash's, word for word, except the conflict and
merge-failed messages, which are new.

### Files it writes

| file | format | when |
|---|---|---|
| `$SPIRA_GATE_LOG` | `<UTC %Y-%m-%dT%H:%M:%SZ> <repo> <branch> waited=<n>s ran=<n>s rc=<status>[ <reason>][ compose=<label> phases=<phase>:<secs>,…]\n`, appended | every verdict once the repository resolved; the composition fields once one was chosen |
| `$SPIRA_VERDICTS/<key>` | `when=<UTC>\nat=<epoch>\nby=<caller>\nrepo=<name>\nbranch=<br>\nsuites=<csv or ->\ncompose=<label>\n`, written to `.<key>.<pid>` and renamed | PASS from a trial only |
| `$SPIRA_VERDICTS/trees/<repo>/<T>` (the tree certificate, `src/cert.rs`) | `verdict=PASS\nsource=gate\ntree=<T>\nrepo=<name>\nrev=<gate revision>\nbranch=<br>\nby=<caller>\nwhen=<UTC>\nat=<epoch>\nharness=<harness_h or ->\nsuites=<csv or ->\n`, temp + rename | every PASS (`pass`, `cached`, `syntax-only`) once the merged tree `T` is known |
| `$SPIRA_RUN/gate-admission/slot.<n>.lock` | empty; `flock` held for the trial | unless `SPIRA_GATE_SUITES=off` and no suites were named against the bead |
| `$SPIRA_RUN/worktree/.gate.<repo-basename>.<tree-key>` | the gate worktree (detached) | removed on every non-PASS verdict; kept on PASS for cargo's fingerprints |
| `<tree>.lock`, `<tree>.lock.holder` | lock; `<pid> <pgid>\n` | holder removed at exit |
| a temp file (`SPIRA_GATE_FILES`) | `git diff --name-status BASE...BR` | removed at exit |

`<tree-key>` is the branch with `/` → `-` and anything outside `[A-Za-z0-9.-]` → `-`.

Yield records go through `yield.sh pass|record` exactly as before (with `SPIRA_RUN` set
explicitly), and certification events through `lc_certify` (lib.sh) when `SPIRA_GATE_BEAD` is set
and the branch tip resolved: `pass <key>` on 0, `infra <reason>` on 75/76, `red <reason>` else.

### The verdict key

`sha256("<repo> <T> <files_h> <cmd_h> <harness_h> suites=<S> bead=<B> ejected=<E>\n")`, where
`T` is **the merged tree** (was: the branch's tree), `files_h`/`cmd_h` are sha256 of the
changed-file list and of the gate string (under `gate_mode = "unit"`, of the gate string plus
`\n# gate_mode=unit`, so a unit-mode PASS never answers for a suites-mode trial; suites mode
hashes the string alone and its keys are unchanged), and `harness_h` is sha256 of `gate.sh`, `exclude.sh`,
`skew.sh` **and this binary** concatenated. Only a PASS is cached; a cached PASS younger than
`SPIRA_VERDICT_TTL` (by its own `at=`) is returned before admission and before the lock, with
its own meter row and `suites=` line.

With the merged tree in the key a moving landing ref retires a verdict, where the bash kept
it: the bash judged the branch alone, so the base could not change its answer. Now it can, so
it must. When the landing ref is an ancestor of the branch the merged tree is the branch's
tree and the key is what it was.

### Order of the trial (as the bash, plus the merge)

repo map → repository → yield/meter armed → landing ref → changed files → ejected suites →
**merge** → `bash -n` → beads data → foreign harness → **the definition** (the tree's
`gate.steps`, or the column for a repository that never adopted one; "The tree owns its
gate") → empty gate string (PASS `syntax-only`, column only) → preflight (`bash <path>` words
exist on the base for a column, on the revision under test for a tree definition) → key →
cache → admission →
sweep → tree lock → checkout the gate revision (proved by `rev-parse HEAD`) → **composition**
and **re-entry** → branch trial (its phases, then the re-entry phase) → the re-entry proof →
PASS, or: harness-fault line / 124 / 75 → base trial on the pinned landing commit (the same
composition, over the crates the base has, the landing ref's own definition and tools) →
base re-run of the branch's red suites the base
trial did not run → attribution.

The mode (`gate_mode`) is read just before the key, since it is part of it.

The meter is armed as soon as the repository resolves, so the early refusals (conflict among
them) now write a gate.log row. The bash armed it later and those refusals were invisible in
the log (law-absence-needs-a-positive-control).

### Attribution (`parse::attribute`; per suite since sp-hh5h0)

**Each red is judged against the base on that suite.** A base trial that exited 0 is not
evidence about a suite it did not run. It may not have run it because its selection differed
or because `testenv --deadline` deferred it (`DEFERRED deadline`, exit 0).

**The base re-run.** After the base trial, when it ran (not 124 or 75), each suite that is red
on the branch and absent from the base trial's ran-set is checked in this order:

* the base has no `spira/<suite>` → the branch's own, with nothing to run;
* otherwise it is re-run on the pinned base, by name, in one call:
  `"$SPIRA_TESTENV_BIN" --suites <a,b> "$SPIRA_GATE_BRANCH"` in the gate tree, under the
  base trial's environment. It has no `--deadline`, so only the gate timeout bounds it.
  Exits 2 and 3 map to 75, as in the gate string. Names outside `[A-Za-z0-9._-]` are dropped.
  Its output is appended to the base's output, and its wall time is metered as the
  `base-rerun` phase. With `SPIRA_TESTENV_BIN` unset there is no re-run.

Then, given the branch trial failed with an ordinary red:

* base trial not run, or it exited 124 or 75 → NO_VERDICT `base-untestable`
* the red names no suite (a fence, a unit phase): base passed → FAIL `branch-red`, suite `-`
* some branch red that the base ran and that was not red there, or that the base lacks →
  FAIL `branch-red` (the first such suite). When the base is red too, the message lists both
  sets
* some branch red still not run on the base (the re-run faulted, or there was no testenv) →
  NO_VERDICT `base-untestable`: never branch-red on a base that did not look
* every branch red is red on the base, and the base's reds are all timeouts → NO_VERDICT
  `base-timeout`
* else → BASE_FAIL `base-red`

Suite names come from the batch runner's lines: a `*.sh` word followed by `RED`, `TIMEOUT`,
`FAILED` or `was killed` (reds); `TIMEOUT`/`was killed` (timeouts); those plus `ok`,
`SKIPPED`, `SKIP-REQ`, `QUARANTINED-RED`, `DISABLED`, `UNREACHED` (ran).

### The trial's budget (sp-govet)

`SPIRA_GATE_BUDGET` is the **whole suites trial's** budget: the runner starts its clock when
it starts, bounds its setup by a share of it and gives the suites what is left (testenv
DESIGN.md §11, D9). Two consequences here:

* **A budget cut is `NO_VERDICT reason=budget`.** When the gate string exits 75 and the
  runner's last line is `VERDICT FAULT … reason=deadline-<phase>` (a setup phase cut at its
  share, or `deadline-suites`: every suite deferred), the trial judged nothing. The message
  names the phase and the budget; there is no base trial. Any other runner fault stays
  `harness-fault`.
* **A red before the suites step is judged on the base's fences only.** When the branch trial
  failed and its output shows the suites step never started (no runner `VERDICT` line, no
  suite line — a fence or the selector failed first), the base trial runs as composition
  `fences` (suites off). Its suites cannot answer whose fault a fence red is, and they were
  most of every such base trial's wall: in the rewrite waves a 12 s fence red was followed by
  a 164–501 s base trial. Attribution is unchanged: base fences pass → `branch-red`, suite `-`.

## Composition (sp-2ghui)

Design `gate-unit-round-integration-2026-09-29`, item 4: **what a branch touches decides what
the gate runs**, behind a typed per-repository switch.

### The switch

`[repo.<name>] gate_mode = "unit" | "suites"` in `spira.toml`, typed and validated by
spira-config (`spira_config::GateMode`), read through the library (`discover` + `load`,
law-config-through-the-cli-only). Absent means `suites`. No spira.toml in force reads as
`suites`; one that does not validate is said on stderr and also reads as `suites`, the stronger
check — a typo must not buy a branch a cheaper gate.

```
spira-config set repo.spira.gate_mode unit  <spira.toml>    # on
spira-config set repo.spira.gate_mode suites <spira.toml>   # off (or: unset repo.spira.gate_mode)
```

### The touched set and the touched crates

* **Touched set**: `git diff --raw -z --no-renames <landing ref> <gate revision>` — the files
  that differ between the landing ref and the tree that would land, with each side's mode.
* **Workspace graph**: `cargo metadata --format-version 1 --no-deps --offline` in the gate tree
  (after checkout): each member's directory, and its path dependencies on other members
  (normal, dev and build alike).
* Each path is classified, first match wins:

| class | paths |
|---|---|
| crate `<m>` | inside member `<m>`'s directory (the deepest member wins: `cockpit/panel/…` is the panel crate, `cockpit/*.sh` is not) |
| workspace | root `Cargo.toml`, `Cargo.lock`, `rust-toolchain(.toml)`, `Makefile`, `clippy.toml`, `.cargo/…` — touches every member |
| doc | `docs/…`, `wiki/…`, any `*.md` or `*.txt` |
| suite | `spira/test-*`, `spira/testlib/…` |
| script | `*.sh`, `*.bash`, `*.py`, or any file executable (`100755`) on either side |
| config | anything else (units, data, examples) |

* **Touched crates** = the crate classes' members, plus every member that depends on one of
  them, transitively (reverse dependents: their own tests exercise the changed code).

### The decision

| mode / branch | composition | runs |
|---|---|---|
| `suites` (any branch) | `suites(mode)` | the gate string, whole — today, byte for byte (no touched set, no metadata read) |
| `unit`, any **script** touched | `suites(script)` | the same |
| `unit`, `SPIRA_GATE_ALL=1` | `suites(gate-all)` | the same — the caller asked for the corpus |
| `unit`, the touched set or the graph unreadable | `suites(no-diff)` / `suites(no-metadata)` | the same — no guessing a cheaper gate |
| `unit`, crates (± docs, config, suites) | `unit` | the gate string with **suites off** and **without the build fence** (below), then on the host: `cargo build --profile aeon -j J --all-targets -p …` (build) and `cargo test --profile aeon -j J -p … -- --test-threads=J` (test) |
| `unit`, nothing buildable | `fences` | the gate string with **suites off** |

"Suites off" is the gate command's environment: `SPIRA_GATE_SUITES=off` and
`SPIRA_CERTIFY_ALWAYS_COVERS=` (empty). `gate-touched.sh` then runs its fences (tier budgets,
plan matrix, lockfile lint) and selects nothing: its always-covers default, `spira/lib.sh`, is
a script, and a script never composes as unit or fences.

### A unit gate builds once (sp-aprxm)

**Intent.** A Rust-only branch's gate compiles the tree once, in the profile its tests run in.
The probe of 2026-09-29 21:02Z (a one-line change to `tsd`, `gate_mode = "unit"`) spent
`fences:147,build:21,test:4`: 144s of the fence phase was `spira/build-fence.sh`, a cold
`make build` (release profile: LTO, one codegen unit, the whole workspace) in a fresh gate
tree, before the composition's own incremental build proved the same crates compile.

**Contract** (`compose::gate_string`). For a `unit` composition the gate string runs with the
step `bash spira/build-fence.sh` (`compose::BUILD_FENCE_STEP`) removed wherever it is a whole
element of the `&&` chain (at the start or after `&& `, followed by ` &&` or the end); anything
else is left as written. `suites(…)` and `fences` compositions run the string byte for byte:
they have no build phase of their own, so the build fence stays their compile check (for
`fences` it skips itself, since nothing buildable changed). The base trial runs the same
string as the branch trial. `gate-touched.sh` no longer calls `build-fence.sh` (it had since
sp-9uro3, a second call once the gate string named the fence itself); `doctor.sh` fails a row
whose gate names no `build-fence.sh`.

**The build phase is `cargo build --all-targets`**, not `cargo test --no-run`: without
integration tests (23 of the workspace's binary crates have none) the latter compiles a binary
only under `cfg(test)`, so `#[cfg(not(test))]` code (spira-claim's `main`) would go uncompiled.
`--all-targets` builds every library and binary both ways plus the test harnesses, and the test
phase reuses the harnesses.

**What the build fence caught that the unit gate does not, and where it is caught now:**

| the build fence caught | now caught by |
|---|---|
| a crate that does not compile (the touched crates and their reverse dependents) | the unit build phase |
| a workspace input that breaks every crate (`Cargo.lock` the toolchain cannot parse, sp-upkae; `Cargo.toml`, the toolchain pin, `Makefile`) | the unit build phase: a workspace input touches every member |
| `#[cfg(not(test))]` code in a binary | the unit build phase (`--all-targets`) |
| `cockpit/panel` | the unit build phase: it is a workspace member |
| a failure only the **release profile** shows (LTO or link errors, `opt-level = "z"`, `strip`; the workspace has no `debug_assertions` code) | **the round**: its corpus run builds `testenv --profile release` of the whole workspace (`round-vm` `REMOTE_SCRIPT`); then main CI (`gate.yml`'s `make build`) and `release.yml` |
| the `Makefile`'s binary list | nothing new: `make build` never read it (the list is in `install`); `make install` and `release.yml` do |

The unit phases run through the same port as the gate string (`run_gate`: `env -i`, the same
environment, `timeout`, the tree), in order, stopping at the first failure. **Admission** is the
same slot the trial already holds. **The CPU budget** is `J = max(1, host cores ÷ the admission
pool size)` for both `cargo -j` and `--test-threads`, so the pool's gates together use about the
host. **The timeout** is shared: each phase gets what is left of `SPIRA_GATE_TIMEOUT`, and a
phase with nothing left is status 124 (NO_VERDICT `timeout`).

**The base trial** runs the same composition on the landing ref, over the touched crates the
base has (per its own metadata): a crate the branch adds cannot be tested on a base without it,
and its absence is not a red. With none left the base runs its fences only.

### What it records (for item 7)

* gate.log: ` compose=<label> phases=<phase>:<secs>,…` after the reason, e.g.
  `rc=0 pass compose=unit phases=fences:41,build:63,test:4`; base-trial phases carry `base-`,
  and the per-suite re-run is `base-rerun`.
  `yield.sh` takes the note's first word as the reason, so the trailing fields are compatible.
* the verdict file: a `compose=<label>` line.
* stderr, before the trial: `gate: composition=<…>` naming the crates and the touched ones.
* **run/tsd `gate-run`** (sp-cln99, `src/telemetry.rs`): one row per verdict, beside the
  gate.log line and under the same condition (the repository resolved). Envelope `ts`, `host`,
  `family` (tsd's), then `repo`, `branch`, `bead`, `caller`, `status` (PASS / FAIL / NO_VERDICT
  / BASE_FAIL), `rc`, `reason`, `waited_secs`, `ran_secs`, `wall_secs` (= waited + ran),
  `gate_mode`, `compose`, `branch_type`, `phases`. `branch_type` is what the branch touches as
  *unit* mode would compose it, whatever mode is in force: `rust-only` (unit),
  `nothing-buildable` (fences), `bash-touching` (a script), `unknown` (the trial ended before a
  composition, or the touched set or workspace graph could not be read). Under
  `gate_mode = suites` that costs one `git diff --raw` and one `cargo metadata --no-deps`; under
  unit mode it reuses the composition. Best-effort: a failed append never changes a verdict.
  `intent-report` (its own crate) reads it.

## Re-entry check (sp-p3srm)

Design item 6. **Intent:** a bead the round returned must pass the suites the round named
against it, in its next gate, before it re-certifies — whatever `gate_mode`, the touched set
or `SPIRA_GATE_SUITES` selects. A Rust-only branch keeps its cheap unit gate *and* passes
those suites; nothing else about its composition changes.

**Why it was not already true.** The key carried the ejected suites (sp-px6ng, sp-hkfdp) and
`gate-touched.sh` unions them into its selection — but only when suites are on. Production
certifies with `certify_suites = "off"`, and `gate-touched.sh` exits before the union, so the
named suites never ran at certification (the queue's "recertification will force these suites
regardless of SPIRA_CERTIFY_SUITES" was not kept). Under sp-2ghui's unit mode the ejected
suites forced the whole `suites` sequence, which with suites off is fences only. And with
suites on, the gate string's `--deadline` budget can defer a named suite while testenv still
exits 0.

### Contract

* **The named suites** (`compose::reentry`): the ejected list (`<bead>.ejected`, else an
  `EJECTED` landstate row's fourth field — what batcher-cut's concurrent attribution writes
  through `land_mark`, sp-hvtgs; comma or space separated), split three ways against the gate
  tree: **required** (`spira/<name>` exists on the tree under test, first-named order, each
  once), **gone** (a suite name the tree no longer has: nothing to run, said on stderr), and
  **invalid** (not `test-<x>.sh` with `[A-Za-z0-9._-]` only: ignored, said on stderr; the name
  is interpolated into a command).
* **The phase** (`run_composed`): after the composition's own phases pass, the suites of
  *required* that the output so far does not show satisfied run as one more phase,
  `reentry`:
  `"$SPIRA_TESTENV_BIN" --suites <a,b> "$SPIRA_GATE_BRANCH"`, the runner's 2/3 mapped to 75
  as the gate string maps them, **no `--deadline`** (the round named them; the phase is bounded
  by what is left of `SPIRA_GATE_TIMEOUT`), through the same `run_gate` port, environment and
  tree. After a unit or fences composition, or suites off, that is all of *required*; after a
  suites gate that already ran them, nothing (no second container); after one whose budget
  deferred some, just those. An unset `SPIRA_TESTENV_BIN` exits 75 (NO_VERDICT).
* **The proof** (`compose::unproven`): the branch trial's output must show every required
  suite's **last** runner line as `ok`, `DISABLED` or `QUARANTINED-RED` (the last two do not
  block a round either, so a round never ejects on them). `SKIPPED`, `SKIP-REQ`, `UNREACHED`,
  `DEFERRED` or no line proves nothing: NO_VERDICT `reason=reentry-unproven suite=<first>`,
  no cache entry, no certificate. A red is an ordinary red (exit 1) and goes to attribution.
* **The base trial** runs the same re-entry phase on the landing ref, so a named suite that is
  red on the base too is `base-red`, not the branch's, as for any other red.
* **Admission**: a gate with named suites takes a slot even when `SPIRA_GATE_SUITES=off`,
  because it will run suites.
* **Records**: the composition label gains `+reentry` whenever *required* is non-empty
  (`compose=unit+reentry`, `compose=suites(mode)+reentry`) in gate.log and the verdict file;
  the phase is `reentry:<secs>` / `base-reentry:<secs>`; stderr before the trial says
  `gate: re-entry — the round named suites against <bead>; each must pass in this gate: …`.
  The PASS's `suites=` line (verdict file and tree certificate) lists them, since they ran.
* **The key is unchanged**: it already carries the ejected list.

### Decisions

* **A phase, not a force.** sp-2ghui forced `suites(ejected)`, the whole gate string with
  suites on — a container and a budgeted corpus for a Rust-only branch, and (with suites off)
  no named suite at all. The re-entry phase runs exactly the named set, after whatever the
  composition is.
* **Proved from the runner's lines, not assumed from its exit status.** testenv exits 0 for a
  deferred, skipped or unreached suite; absence is never green.
* **Gone suites are said, not enforced.** A suite the tree no longer has cannot be run; the
  plan-matrix fence is what guards a suite deletion that drops coverage.

## Every fence proves it checked (sp-ufbkh)

**Intent.** A fence that exits 0 having checked nothing is indistinguishable from one that
checked everything. On 2026-09-29 the test plan's fence was found skipped at every gate since
it was wired: `gate-touched.sh` ran it only when `SPIRA_GATE_REPO` resolved to its own tree,
and the gate sets that to the repository while running in its worktree. The rule was already
law (law-a-control-that-cannot-check-must-refuse); it was re-violated, so it becomes
structure: **the gate cannot PASS a trial in which a fence it ran was silent.**

### Contract

* **The line.** Every fence, on success, prints one line (stdout or stderr; the trial's
  output is one pipe): `fence: <name> checked <n> <unit>[ anything]`, `n` an integer > 0.
  A fence that cannot check exits non-zero naming what is missing; it never exits 0 silent.
* **Which fences** (`fence::expected`, over the gate string the composition actually runs —
  under `gate_mode = unit` without the build fence, `compose::gate_string`): each
  `bash <dir>/<x>.sh` word (the preflight's scan) is the fence `<x>`; the suite selector
  `gate-touched.sh` is not a fence and runs none; the `$SPIRA_LINT_BIN` word is `spira-lint`
  plus each rule in `LINT_RULE_FENCES` that the word's `--only` (if any) includes:
  `plan-matrix`, `lockfile-lint`, `tier-budget-allowlist`, `tier-budget-area-allowlist`,
  `tier-budget-areas`.
* **The check** (`fence::silent`), after the branch trial exits 0 and before the re-entry
  proof: any expected fence with no line, or whose largest reported `n` is 0, makes the
  verdict **NO_VERDICT `reason=fence-silent` `suite=<the first such fence>`**, the message
  naming all of them, with no cache entry and no certificate. A trial that exits non-zero is
  judged as before: a fence that failed is a red.
* **The PASS** message carries `gate: fences checked: <name>=<n> <unit>, …`.

### Decisions

* **The selector runs no fence.** The gate string captures it as `_s="$(bash
  spira/gate-touched.sh …)";` — the assignment's status is ignored, so a fence failing inside
  it emptied the selection and the string exited 0. Its fences (the test plan's, the lockfile
  lint, the three tier-budget ratchets) moved into spira-lint, which the string runs in its
  `&&` chain, in the gate tree, against `SPIRA_GATE_BASE`.
* **Read from the string, not configured beside it.** A second list of fences in
  configuration is a second thing to forget; the gate string is the one list (as in
  "Composition"). The two names the string cannot show — the selector, and which spira-lint
  rules print their own line — are constants here, and `fence.rs`'s tests fail when
  `gate-touched.sh` calls a script again or spira-lint loses one of those rules.
* **`checked 0` is silence.** A count of zero is a fence reporting that it looked at
  nothing. A fence whose input may legitimately empty counts what it compared instead (the
  tier-budget ledgers report `1 ledger`).
* **NO_VERDICT, not FAIL.** A silent fence judged nothing; the branch is not shown to be at
  fault, and nothing is certified.

## The tree owns its gate (sp-quu2w)

### Intent

The fence chain used to live in production config (the repo-map's gate column, mirrored as
`[repo.<name>] gate` in spira.toml), and `$SPIRA_LINT_BIN` was the installed binary. So a
branch that ported or deleted a fence could not pass its own gate: its tree no longer had the
script the config named (wave 1, sp-pppt0: `bash spira/testdb-mode-lint.sh: No such file`),
and the rule it added was not in the installed spira-lint. Landing it needed a hand-edited
config flip in the same instant. **The tree under test owns its gate**: what a branch will
land with is what its gate runs.

### Contract

* **The definition is `gate.steps` at the repository root** (`def::PATH`), read from the
  revision under test (the merge, as everything else). One directive per line: `step
  <command>` (one element of the `&&` chain, in order), `bin <VAR> <package>`, `#` comments,
  blank lines. Anything else refuses. `Def::command()` joins the steps with ` && ` — the
  string config held, byte for byte at migration — so the composition's build-fence drop
  (`compose::gate_string`) and the fences' expectations (`fence::expected`) read the same
  kind of string as before, now derived from the tree.
* **Resolution (`def::resolve`)**, from the landing ref's blob and the tree's:

  | landing ref has it | tree has it | the gate runs |
  |---|---|---|
  | no | no | the repo-map column, as before sp-quu2w (empty = `syntax-only`) |
  | no | yes | the tree's (the adopting branch is gated by what it adds) |
  | yes | yes | the tree's; a non-empty column is ignored with a warning |
  | yes | no | **FAIL `gate-definition`**: deleting the gate is not a way through it |

  A tree definition that does not parse, or names no step, is FAIL `gate-definition` when
  the landing ref's parses, BASE_FAIL `base-gate-definition` when it does not. Nothing falls
  back to config once the landing ref has adopted.
* **Preflight on the tree under test.** Each `bash <path>` a tree definition names must be
  in the revision under test: a branch that deletes a fence script and its `step` line
  passes; one that deletes the script and leaves it named is FAIL `gate-definition`
  (BASE_FAIL `base-gate-definition` when the landing ref already names a file it lacks). A
  column keeps today's check against the base (NO_VERDICT `cmd-missing-file`).
* **Tools from the tree.** Each `bin <VAR> <package>` is built before any step, in a `tools`
  phase (`cargo build --profile aeon -j <jobs> -p <package>…`, then `[ -x
  target/aeon/<package> ]`), and the steps get `<VAR>=<gate tree>/target/aeon/<package>` in
  place of the installed value. The aeon profile is the unit phases' profile, so a unit
  composition's build reuses it. The spira repository declares `bin SPIRA_LINT_BIN
  spira-lint`. A tools phase that fails is a red like any other and is attributed by the base
  trial.
* **The base trial runs the landing ref's own definition** (`def::resolve_base`), its own
  tools, its own steps — never the branch's. A landing-ref definition that does not read
  leaves the base untested (BaseUntestable, NO_VERDICT).
* **The key** hashes `Def::key_text()` (the command plus one `# bin VAR package` line per
  tool) in place of the bare string; a column is hashed as before.
* **`gate --definition [repo]`** prints the command the landing ref defines (or its column),
  exit 1 naming why it cannot. doctor.sh's compile-check check (sp-1hmrm) reads it, so the
  one resolution lives here.
* **Config:** `[repo.<name>] gate` is retired in spira-config (`RETIRED_REPO_KEYS`, warning,
  ignored; `convert` no longer carries the column). The repo-map column stays readable by
  the gate for a repository that never adopted `gate.steps`, and retires with repo-map.

### Decisions

* **A line format, not a TOML array or one long string.** The five wave-1 fence ports each
  edit the definition after this lands; one directive per line makes each port a one-line
  diff that merges beside the others, and needs no quoting (the suite step carries both
  quote kinds, which a TOML string would need escaping or `'''` for). The `step`/`bin`
  keyword on every line keeps it extensible and makes an unknown line a refusal, not a
  command.
* **A fixed path, not a config key naming one.** A path in config is one more thing that can
  disagree with the tree; a fixed path cannot, and adoption is recorded where it matters —
  in the landing ref itself.
* **The column survives for non-adopting repositories.** Dozens of suites' fixture
  repositories gate through a repo-map column (including empty = syntax only); refusing a
  tree without `gate.steps` outright would break every one of them for no safety gain,
  because the refusal that matters — an adopted repository losing its definition — keys on
  the landing ref, which the branch cannot change.
* **`testenv` is not built from the tree.** It is the executor of the suite step, not a
  check whose rules a branch adds; a branch that broke it would judge itself with the broken
  runner. `bin SPIRA_TESTENV_BIN testenv` is one line if that is ever wanted.
* **Dropped:** the config gate string as the spira repository's source of truth, and
  hand-editing it at landing.

## Boundaries (ports)

`World` in `src/ports.rs`: git reads and the merge, the touched set (`diff_raw`), the mode
(`gate_mode`, spira-config), the workspace graph (`cargo_metadata`), the helper scripts, the
lib.sh seam, the admission pool, the tree lock and worktree, the gate command, the clock, and
the files above. `src/compose.rs` is the pure decision (classification, reverse dependents,
the composition, the unit commands) with its own tests.
`src/real.rs` implements it; `src/tests.rs` drives the engine through a recording fake. The
lib.sh seam is one `bash -c '. lib.sh; …'` at start (NUL-separated `key=value`) and one per
`lc_certify` / `spira_prune_worktrees`, payloads never on argv.

## Decisions

* **Every PASS certifies the merged tree it judged (queue/DESIGN.md §8 D12).**
  `queue land-local` refuses a head whose tree has no certificate. The certificate is keyed
  by `(repo, T)` alone, unlike the verdict cache key, so that a gate upgrade does not make
  an earlier PASS of the same content stop counting. The harness hash is recorded, not
  matched. `src/cert.rs` owns the format for all three crates that touch it: this one and
  batcher-cut write it, and queue reads it. A cached PASS rewrites the certificate, so a
  tree whose PASS predates certificates is certified the next time it is gated.

* **A conflict is NO_VERDICT `reason=conflict`**, not a new exit status (see above).
* **The landing pass routes it.** `certify_judge` runs `rebase-stale <id> <repo>` on
  `reason=conflict` (0/1/2: rebase-stale did the bookkeeping; 3: the old NO_VERDICT path).
  The batcher's `handle_base_conflicts` only sees CERTIFIED members, so without this a stale
  branch would be re-gated every pass and never certified.
* **Callers keep calling `gate.sh`.** One shim is one place to change, and the process scans
  and the lock-holder files keep their meaning.
* **Composition wraps the gate string; it does not parse it** (sp-2ghui). The string is
  configuration and stays the one list of fences; unit mode runs it with suites off and adds
  the host phases after it. Splitting the string would make a second copy of the fence list.
  **One exception, by exact text** (sp-aprxm): a unit composition removes the build fence's
  step, because its own build phase is the compile check. Rejected: a warm per-gate target
  dir for the fence (a release build relinks every dependent binary with full LTO, and gates
  sharing a target dir serialise on cargo's lock); an environment switch the fence reads
  (bash logic); a separate unit-mode gate string in spira.toml (a second copy of the fences).
* **A script touched anywhere keeps today's whole sequence**, including suites, until that
  component moves to Rust (the operator's default, 2026-09-29). Executables with no extension
  count as scripts, by their mode.
* **Everything the switch cannot judge falls back to `suites`** (an invalid config, an
  unreadable diff or graph, `SPIRA_GATE_ALL`) — the stronger check, never a cheaper one on a
  guess. Ejected suites were on this list until sp-p3srm; they are the re-entry check now.
* **A red is the base's when the base is red on that suite; the gate never infers it from
  the base's exit status** (sp-hh5h0). The re-run costs one testenv call over the unjudged
  suites, and only on a branch that is already red. That cost buys never ejecting a correct
  branch for a base red: an ejection of every branch whose selection includes the suite,
  as FAIL, is what throttled certification.
* **`gate-lib.sh` is retired.** Its functions are ported (`src/parse.rs`, `src/key.rs`) with
  unit tests; `test-gate-unit.sh`, which only exercised them, is retired with it.
