# gate — design

Replaces the logic of `spira/gate.sh` and `spira/gate-lib.sh` with one Rust binary, `gate`.
`spira/gate.sh` stays as the one entry point every caller already names; it `exec`s `gate`
by bare name on the launcher's PATH (sp-gypjk; it once resolved `SPIRA_GATE_BIN`). Bead: sp-0tpcs (epic sp-8m1at, design
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
* `--home` is the directory holding `lib.sh` (and the files `harness_h` hashes). The default is
  `$SPIRA_HOME`, then `<dir of this binary>/../spira`. `exclude.sh`, `skew.sh`, `yield.sh` and
  `gate-sweep.sh` are found on PATH (sp-gypjk); exclude.sh or skew.sh missing is NO_VERDICT.
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
| suites (`testlib/gate-fixture.sh` and the gate suites that copy `spira/`) | `gate.sh`, with the fixture's `gate` first on PATH | output |

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
| `SPIRA_GATE_BUDGET`, `SPIRA_GATE_ALL`, `SPIRA_CERTIFY_ALWAYS_COVERS`, `SPIRA_BATCH_MAXPAR`, `SPIRA_VERDICT_REPEAT_CONSIDERED`, `SPIRA_TESTENV_SETUP_SHARE`, `SPIRA_TESTENV_WARM_SLOTS` (sp-govet), `HOME` | passed through to the gate command | as bash |
| `SPIRA_RELEASE` | the gate command's PATH, built from it (below) | the launcher |

### The gate command's environment (unchanged list, `env -i`)

`PATH=$SPIRA_RELEASE/bin:$SPIRA_RELEASE/spira:/usr/local/bin:/usr/bin:/bin:<SPIRA_PATH tail>`
— set outright from `SPIRA_RELEASE` and the box's own tool tail, `SPIRA_PATH`
(`spira_config::release_path_from_env_with_tail`, sp-c7b85, amending sp-31gtu's
`release_path_from_env`, which carried no tail: cargo, for tree builds, lives in the tail,
not the release), never the PATH the gate inherited, so a bare tool name is the release's
first and the box's own tools after the system directories. Either `SPIRA_RELEASE` unset or
a tail entry inside a release or a checkout is `NO_VERDICT reason=release-unset` before
anything runs, and `SPIRA_RELEASE` is passed through. `HOME`, `TERM=dumb`, `SPIRA_GATE_REPO`, `SPIRA_GATE_REPO_NAME`,
`SPIRA_GATE_BRANCH`, `SPIRA_GATE_BASE`, `SPIRA_GATE_SELECT_HEAD=<branch>`, `SPIRA_GATE_FILES`,
`SPIRA_GATE_HOST_CORES`, `SPIRA_GATE_EJECTED_SUITES`, `SPIRA_GATE_ALL` (default 0),
`SPIRA_GATE_SUITES` (default on), `SPIRA_CERTIFY_ALWAYS_COVERS`, `SPIRA_BATCH_MAXPAR`,
`SPIRA_VERDICT_REPEAT_CONSIDERED`, `SPIRA_GATE_BUDGET` (default 300), `SPIRA_RUN`,
`SPIRA_TESTENV_SETUP_SHARE`, `SPIRA_TESTENV_WARM_SLOTS` (sp-govet), and each `bin <VAR>` of
the tree's gate.steps (`SPIRA_LINT_BIN`, `SPIRA_SELECT_BIN`: the tree under test's builds, the
design's one explicit hand-off). Every installed tool (`testenv`) is invoked by bare name. Run as `timeout $SPIRA_GATE_TIMEOUT bash -c "$CMD"` in
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
| `$SPIRA_RUN/gate-admission/slot.<n>.holder` | `pid=<p> start=<starttime> who=<branch> since=<epoch> waited=0 last=<epoch>\n` (advisory; the flock is the lock; sp-f4ig1) | written when the slot is taken; read by `spira-admit status` and the waiting line |
| `$SPIRA_RUN/worktree/.gate.<repo-basename>.<tree-key>` | the gate worktree (detached) | removed on every non-PASS verdict; kept on PASS for cargo's fingerprints |
| `<tree>.lock`, `<tree>.lock.holder` | lock; `<pid> <pgid>\n` | holder removed at exit |
| a temp file (`SPIRA_GATE_FILES`) | `git diff --name-status BASE...BR` | removed at exit |

`<tree-key>` is the branch with `/` → `-` and anything outside `[A-Za-z0-9.-]` → `-`.

Yield records go through `yield.sh pass|record` exactly as before (with `SPIRA_RUN` set
explicitly), and certification events through `spira-lc certify` (lib.sh's `lc_certify` until sp-arpjt) when `SPIRA_GATE_BEAD` is set
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
  `testenv --suites <a,b> "$SPIRA_GATE_BRANCH"` in the gate tree, under the
  base trial's environment. It has no `--deadline`, so only the gate timeout bounds it.
  Exits 2 and 3 map to 75, as in the gate string, and so does 127 (testenv not on PATH). Names outside `[A-Za-z0-9._-]` are dropped.
  Its output is appended to the base's output, and its wall time is metered as the
  `base-rerun` phase.

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
same slot the trial already holds. **The width** is the host's cores for both `cargo -j` and
`--test-threads`. How many gates share the host is the pool size's job, never a narrower gate
(sp-f4ig1, DESIGN-admission.md D3; law-reduce-the-count-never-throttle-the-job). It was `host
cores ÷ the admission pool size` until then. **The timeout** is shared: each phase gets what is left of `SPIRA_GATE_TIMEOUT`, and a
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
  `testenv --suites <a,b> "$SPIRA_GATE_BRANCH"`, the runner's 2/3 (and 127, not on PATH) mapped to 75
  as the gate string maps them, **no `--deadline`** (the round named them; the phase is bounded
  by what is left of `SPIRA_GATE_TIMEOUT`), through the same `run_gate` port, environment and
  tree. After a unit or fences composition, or suites off, that is all of *required*; after a
  suites gate that already ran them, nothing (no second container); after one whose budget
  deferred some, just those.
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
  `gate-touched.sh` was not a fence and ran none, and its successor, the `suite-select` binary
  (`"$SPIRA_SELECT_BIN" gate …`, sp-wx2tw), is no `bash` word; the `$SPIRA_LINT_BIN` word is `spira-lint`
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
  target/aeon/<package> ]`), and the steps get `<VAR>=<gate tree>/target/gate-tools/<tree id>/<package>` (the
  build's output, copied into a directory keyed by the tree it was built from — see "Tools
  keyed by the tree they were built from") in place of the installed value. The aeon profile is the unit phases' profile, so a unit
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

## Tools keyed by the tree they were built from (sp-g9f3t)

### Intent

A trial's tools must provably be the tree under test's own. Both trials build in the one gate
tree (the base trial checks the landing ref out over the branch trial's merge), into the one
`target/`, and the steps read `target/aeon/<package>` — a path that names no tree. Whether the
binary there was built from the base or is the branch's, left behind, rests on cargo's mtime
fingerprints and nothing the gate can show. On 2026-09-30 two base trials judged local/main
red with `base-tools:0` and the question "which spira-lint did the base run?" had no answer
from the record. (That red turned out not to be a stale binary — see "The finding" — but a
gate that cannot answer the question cannot rule it out either.)

### Contract

* **The tree is proved before its tools are built.** Before a trial's tools phase the gate
  reads `HEAD^{tree}` of the gate tree and requires it to equal the tree id of the revision
  that trial judges (the merge's for the branch trial, the pinned base commit's for the base
  trial). A mismatch, or an unreadable HEAD, is `tools-unattributed`: NO_VERDICT for the
  branch trial; for the base trial, no base trial (BaseUntestable, NO_VERDICT). Never a guess.
* **Tools live in a directory keyed by that tree id:** `<gate tree>/target/gate-tools/<tree
  id>/<package>`, with `TREE` holding the id. The steps get `<VAR>=` that path — never
  `target/aeon/<package>`. After a build, the gate (Rust, `World::install_tools`) copies each
  `target/aeon/<package>` into `<dir>.tmp`, writes `TREE`, renames it into place, and removes
  every other keyed directory beside it (the gate tree is locked for the whole trial, so no
  other gate reads them).
* **Reuse only for the same tree.** A keyed directory whose `TREE` equals the id and that holds
  every package is reused without building (the `tools` phase is not run and not metered, and
  the trial says so on stderr). A directory keyed by any other tree is never read.
* **Fail closed after the phase too.** Whether built or reused, before any step runs the gate
  re-reads `TREE` and checks every package exists there; otherwise `tools-unattributed`
  exactly as above. An install that fails is the same.

### The finding (sp-g9f3t)

The two 2026-09-30 base-reds (12:18Z sp-t26yx, 12:32Z sp-9thdw) were not a stale binary: the
same freshly built `spira-lint`, on the same local/main tree, flips between 0 and 5
literal-lint findings on file mtimes alone. literal-lint asks `spira/schema.sh` for the
configured names; schema.sh sources conf.sh, which reads the operator's
`~/.config/spira/spira.toml` (`ask_label = "needs-ryan"`) when it is newer than the tree's
`spira/chamber/*.fayth`, and otherwise regenerates a persona-only `spira.toml` into the tree
and reads that (ask = the default, `needs-operator`). The reused gate tree keeps old fayth
mtimes, and something rewrote the operator's spira.toml at 12:32:08Z, so both trials of the
next gate read the box's config and flagged every `needs-ryan` fixture literal. The fix is in
spira-lint (literal-lint pins schema.sh's config inputs, see its DESIGN.md); this section
closes the question the gate could not answer.

### Decisions

* **In the gate tree, not a shared cache under SPIRA_RUN.** A shared cache keyed by tree would
  let concurrent gates prune each other's directories; inside the locked gate tree nothing
  else reads them, and `git clean -e target` already keeps them across checkouts.
* **The build stays in the tree's own `target/`** (amended sp-z61hj: one-shot, not
  incremental, and its build directories live on tmpfs — see "Build IO" below). A cold
  per-tree `CARGO_TARGET_DIR` would rebuild every dependency for every new base (minutes a
  gate). What is attributed is
  the binary the steps run: copied out of a build run in a tree proved to hold the id.
* **Copying is Rust (`install_tools`), not more shell in the tools string.** The proof and the
  install are logic; the tools string stays `cargo build … && [ -x … ]`.

## Boundaries (ports)

`World` in `src/ports.rs`: git reads and the merge, the touched set (`diff_raw`), the mode
(`gate_mode`, spira-config), the workspace graph (`cargo_metadata`), the helper scripts, the
lib.sh seam, the admission pool, the tree lock and worktree, the gate command, the clock, and
the files above. `src/compose.rs` is the pure decision (classification, reverse dependents,
the composition, the unit commands) with its own tests.
`src/real.rs` implements it; `src/tests.rs` drives the engine through a recording fake. The
lib.sh seam is one `bash -c '. lib.sh; …'` at start (NUL-separated `key=value`) and one per
`spira_prune_worktrees`, payloads never on argv (certification is `spira-lc certify`, a binary, since sp-arpjt).

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
* **Suite selection is the `suite-select` crate** (sp-wx2tw, `suite-select/DESIGN.md`). The
  gate string calls the installed binary (`"$SPIRA_SELECT_BIN" gate …`), which the gate passes
  into the gate command's environment; its exit 1 (an unclaimed source file) is FAIL and any
  other failure NO_VERDICT, where `gate-touched.sh` swallowed both into an empty selection.
  The re-entry check and the base re-run read suite names by the selector's rule
  (`names::is_suite_name`, `names::split_list`), so the suites the round named, the ones the
  selector forces, and the ones the base re-run names are one vocabulary.

## Wave 1 — gate-spira.sh, exclude.sh, build-fence.sh (sp-hyc3a)

Rewrite-order wave 1 ("gate and fences first"); brief: rewrite waves, Concierge scratchpad
2026-09-29. For each of the three named scripts, retire vs. port, decided against what the
Rust gate already does:

* **`gate-spira.sh`: RETIRED (deleted), not ported.** It was this repository's *scheduled*
  test runner, never the production landing gate (its own header said so; decision sp-wyep).
  It has had no caller anywhere in the tree since sp-b99nj retired the background suites
  sweep and `systemd/spira-suites.{service,timer}` on 2026-09-26 — confirmed again here by
  grep, matching three independent prior findings (sp-nhid0, sp-z3i42.3, sp-9mnvm). It was
  the sole caller of `gate-fences.sh`'s fence loop, deleted with it. Nothing it did needs a
  Rust port: at the time this bead started, the ten fences that loop ran (bd-stdin-lint.sh,
  gh-intake-lint.sh, incident-cause-lint.sh, tmux-scope-fence.sh, wiki-add-fence.sh,
  testdb-mode-lint.sh, literal-lint.sh, scratch-fence.sh, binary-path-fence and
  payload-argv-lint) were re-homed as plain calls in `spira/repo-map.example`'s shipped gate
  command (mirroring what sp-9mnvm had already applied to this box's live
  `~/.config/spira/repo-map` by hand) and into `.github/workflows/gate.yml`'s Lints step.
  **Overtaken by events on merge with `local/main`:** sp-ekkak/this wave's own concurrent
  work ported nine of those ten (everything but `build-fence.sh`) into spira-lint rules of
  the same name and deleted their standalone scripts outright — `spira/repo-map.example` and
  `gate.yml` are updated again here to call `spira-lint`/`bin/spira-lint --only <rule>`
  instead of a deleted `bash spira/<rule>.sh`, and `spira/repo-map.example`'s row is now
  explicitly illustrative only, since `gate.steps` (sp-quu2w, landed after this bead started)
  is this repository's actual, tree-owned gate definition — see its own header. `sop.sh lint`
  and `suite-state-fence.sh` are still deliberately left out of both (need a real `SPIRA_DB`;
  `sop.sh lint` also hangs past 120s under load until sp-oc2i6/sp-rjbrc's O(n²) fix lands on
  `local/main` — test-sop-gate-wired.sh says so and skips that row rather than asserting it).
  Its own suite-budget bead-filing and per-suite leaky-child watchdog had exactly one caller
  (itself) and no live analog before this change either (already so decided: sp-z3i42.2, left
  open P3). Every test that asserted wiring by grepping `gate-spira.sh`'s source is either
  deleted with its now-ported subject (nine of them, by the concurrent sp-ekkak work) or
  repointed to grep `.github/workflows/gate.yml` instead (`test-orphan-test.sh`,
  `test-sop-gate-wired.sh`'s prose); `test-gate-fences.sh`, whose only subject was
  `gate_fence_list`, is deleted with it. Supersedes the never-landed `spira/sp-m893q` branch
  (cut before the Rust gate cutover; stale against current `gate.sh`), redone here against
  `local/main`.
* **`build-fence.sh`: KEPT AS BASH, not ported, not retired.** Its job for a `unit`
  composition is *already done* by this crate's own build phase (sp-aprxm, above) — that is
  precisely why `compose::gate_string` strips its step for that mode, and why `gate.steps`
  keeps `step bash spira/build-fence.sh` its own line (sp-quu2w's own comment says so). For
  `suites` and `fences` compositions (a bash-touching branch, or the temporary state before a
  component moves to Rust) it remains the only compile check and `doctor.sh` requires every
  repository's gate definition to name it. Porting the remainder to Rust now would mean either
  a new single-purpose binary crate (rejected: `gate/DESIGN.md`'s own fence-scripts law says a
  new fence is a spira-lint rule, never a new script either direction) or extending spira-lint
  itself, which other concurrent work owns this wave. It retires on its own once every
  bash-touching path is gone (law-rust-rewrite-order); nothing to do here but leave it wired,
  unchanged, in `gate.steps`.
* **`exclude.sh`: KEPT AS BASH, not ported, not retired.** The beads-database guard
  (law-beads-is-never-public) is already this crate's own non-goal above ("each moves in its
  own turn of the rewrite order") — not this turn. It is independently wired (called directly
  by this binary via `ports.rs`'s `ls-tree`/`exclude` boundary, and by `hooks/pre-commit`'s
  `staged` entry point), so `gate-spira.sh`'s own redundant call to it (one of three
  layers its own header described) is deleted along with the rest of that script without
  changing exclude.sh's behaviour at all. A guard whose whole purpose is refusing to publish
  a beads database fails closed today and is not the rewrite to attempt under a P0 clock.

Parity: every suite this bead's own changes touch still passes under `testenv` (from the
worktree, never the host): `test-exclude.sh`, `test-build-fence.sh`, `test-orphan-test.sh`,
`test-sop-gate-wired.sh` at each stage of this branch's history, and the full `spira-lint`
run (18 rules, including `config-fence`) is clean against the merged tree. `cargo test -p
gate` is green (149 tests, including `a_unit_gate_builds_once_and_every_other_composition_
keeps_the_build_fence` and, after the sp-quu2w merge, `the_checked_in_definition_keeps_a_
compile_check_and_proves_its_fences`). The real gate's own VERDICT on the merged head is the
final proof; see the bead's report for the line.

## Build IO (sp-z61hj)

Full contract: `spira-config/DESIGN-build-cache.md`. In the gate:

* **Every build compiles through the box's sccache.** The wrapper is resolved on the PATH the
  gate command gets (`World::build_wrapper`); `RUSTC_WRAPPER` and
  `SCCACHE_IGNORE_SERVER_IO_ERROR=1` join the command's environment (with
  `SPIRA_BUILD_CACHE`, so testenv resolves the same). sccache absent →
  `NO_VERDICT reason=no-build-cache` for a trial that builds in the tree (a `bin` line, a unit
  composition, `--release-bins`), before anything builds; a trial that builds nothing (a
  column-gated repository, the suites' fixture repositories) does not need it. The tmpfs
  preparation below applies to the same trials. `SPIRA_BUILD_CACHE=off` is
  honoured and printed.
* **The tools phase and the unit phases are one-shot**: `--config
  profile.aeon.incremental=false` (never `CARGO_INCREMENTAL`, which sccache hashes into every
  key and so would split the cache from an aeon's interactive builds).
* **The gate tree builds on tmpfs** (`src/target.rs`, `World::target_on_tmpfs`): after the
  checkout, `target/{aeon,release,debug,gate-tools}` are links into
  `$SPIRA_GATE_TARGET_ROOT` (default `/tmp/spira-gate-target-<run hash>`); orphaned
  directories are removed, the least recently used unlocked ones evicted over
  `SPIRA_GATE_TARGET_CAP_MIB`, and short of room (`_MIN_FREE_MIB`, `_MIN_MEM_MIB`) is
  `NO_VERDICT reason=scratch-short` — never the disk.
* Measured on a one-line Rust probe (unit composition, cold gate tree): 1,020,960,768 bytes
  written before; see the bead's closing note for after.
* **`gate.sh --release-bins <branch> <repo>`** (the hand landing): on a PASS, `cargo build
  --release --workspace --locked` (one-shot, through the cache) runs in the gate tree, whose
  `target/release` is on tmpfs, and the gate prints the `queue land-local … --worktree <gate
  tree>` that ships it. The gate tree holds the judged (merged) tree, which is the landing
  head's tree, so land-local's tree check accepts it; a failed build empties
  `target/release`, so an older tree's binaries can never be shipped from it. This replaces
  the release build a hand landing ran in its worktree on the disk. The verdict is unchanged.

## The base-suite cache (sp-kqger)

Measured 2026-09-30 (gate.log, suites composition): every branch-red gate re-ran its selected
suites on the landing base to decide whose fault the red was — 275-337 s, on top of a clean
trial's 200-450 s — and while many branches iterate against the same `local/main` tip, each
red re-ran an **identical** base trial: same base tree, same suites. `SPIRA_GATE_BUDGET`
bounds each trial; the doubling sat outside it.

### The change

**A suites composition's base trial no longer mirrors the branch's selection at all.** Before
this bead, "Order of the trial" ran the base through `self.base_composition(&comp, …)`, which
for `Composition::Suites` returned it unchanged — the whole repository gate string, over
whatever suites it happened to select against the pinned base. That selection cost the
275-337 s above and answered nothing the branch's own red suites did not already ask. The base
trial's main phase is `Composition::Fences` now, unconditionally, whenever `comp` is
`Suites { .. }` — cheap (fences + the build fence, ~12-60 s; DESIGN.md "The trial's budget"
measured the same cost for the pre-existing pre-suites-red case, which this collapses into).
`SPIRA_GATE_SUITES=off` reaches the base's own command either way; a tree-defined gate
string's literal text is unaffected (`compose::gate_string` only rewrites a `Unit`
composition).

Every one of the branch's red suites is then judged in the loop that already existed for "a
red the base trial did not run" (sp-hh5h0) — which, since the base's main phase never runs
suites now, is *every* red suite, not just the ones a mirrored selection happened to miss.
Each is answered by:

1. **The cache** (`src/basecache.rs`), keyed on the base tree, the suite, the gate's own
   harness hash and the testenv image — a fresh `PASS`/`FAIL` skips running anything.
2. **A targeted rerun**, otherwise — the same `testenv --suites <a,b> "$SPIRA_GATE_BRANCH"`
   this bead's predecessor (sp-hh5h0) already used for the suites a mirrored selection missed,
   now the only mechanism, paying for only the suites the cache could not answer. A **full**
   cache hit across every red suite means this call never runs at all — "a flip-suspect (base
   green in cache, branch red) re-runs nothing extra."

A suite absent on the base (`ls_tree_has` false) is still the branch's own, unchanged, and
never reaches the cache — nothing to look up or cache for a suite that does not exist there.

**Measured 2026-09-30**, 10 real `gate.sh` runs each (the real binary, real git, real file
locks, real admission — `testlib/gate-fixture.sh`'s own convention, extended with a fake
`testenv` standing in for the suite container's cost, as `test-gate-base-evidence.sh` already
fabricates suite-status text rather than running a real corpus), same branch-red fixture,
same base tree, before (`local/main`, 3f2babc39) and after this change: median wall 5.69 s →
4.81 s, median gate.log `ran=` 3 s → 2 s. The fixture's stand-in "suite corpus" costs a fixed
1.00 s (standing in for production's 275-337 s); before, every run's `base-gate` phase paid
that cost in full; after, only the first (cold-cache) run's `base-rerun` phase does — every
run after it shows `base-fences` and `base-image-tag` only, no `base-rerun` at all, the "full
cache hit re-runs nothing extra" acceptance holding on a real gate, not only in the crate's
own unit tests. The proportional saving (the entire corpus cost, once the cache is warm)
scales to production's measured 275-337 s the same way; this fixture's numbers are the
mechanism's proof, not a production timing (its corpus is a fixed sleep, not 500+ real
suites).

### The cache

`${SPIRA_VERDICTS:-<run>/verdicts}/base-suites/<repo>/<base-tree>/<suite>`,
`verdict=PASS|FAIL\nharness=<h>\nimage=<tag>\nwhen=<UTC>\nat=<epoch>\n` — through the same
generic `read`/`write_atomic`/`mkdir_p` ports the verdict cache and `cert.rs` already use, so
no new I/O port exists for the cache file itself (`basecache::path`/`render`/`fresh` are pure;
`path` refuses anything not `cert::is_repo_name` / `cert::is_object_id` /
`compose::is_suite_name`, the same discipline `cert.rs` and `base_rerun_cmd`'s own suite-name
filter hold to).

* **The base tree is the directory, not merely a key inside one file.** A landing ref that
  moves reads an entirely different, empty namespace under its new tree — never a stale
  answer under the old one — by construction. No TTL is needed on that axis, unlike the
  verdict cache's own `SPIRA_VERDICT_TTL` (a different cache, a different question: "did this
  exact tree already pass," not "is the base independently red on this suite").
* **`harness` and `image` are recorded and checked on every read** (`basecache::fresh`),
  because the base tree not moving does not mean nothing that could change a suite's answer
  has: a new gate binary, or a rebuilt testenv container from the *same* tree content (an
  external pin — `testenv`'s own `bd_pin`, DESIGN.md of `testenv` — is not itself in the tree).
  An entry recorded under either differing is a miss, never trusted with the right suite name
  attached to the wrong answer.
* **The testenv image tag** is `testenv container tag` (`testenv/src/container.rs`'s
  `Driver::image_tag`, a pure hash of `testenv/Containerfile` + `deps.toml` + the `bd_pin`),
  invoked once per base trial that has at least one non-absent red suite to ask about — never
  on a PASS, never on a red whose suites are all absent — via the same `World::run_gate` port
  already used for the targeted rerun, bare `testenv` on PATH (no tree-built binary; the
  common case, a repository with no `bin` mapping in its `gate.steps`, has none to use
  anyway). Metered as its own `base-image-tag` phase.
* **FAIL CLOSED** (sp-kqger's own acceptance): an unresolved image tag (the query faulted, or
  printed nothing) never consults the cache at all for that trial, and writes nothing to it
  afterward — every red suite is run, exactly as before this bead, rather than trust a cache
  entry it cannot validate or produce one nothing will ever validate against. An unreadable,
  unparseable, or mismatched cache entry is the same miss, by `basecache::fresh` returning
  `None` for all three.
* **Warming.** After a targeted rerun, every suite it actually reported on (`ran_suites` of
  the rerun's own output) is written back, `PASS` or `FAIL` by `red_suites`; a suite the rerun
  never reported on — a fault, an unexpected timeout, despite the rerun's own `--deadline`-free
  contract (sp-hh5h0) — is left uncached, so the next gate asks again rather than trust a run
  that did not actually answer.
* **A cached `FAIL` still names the suite.** The synthetic line pushed into `base_out`
  (`"  <suite> RED     cached (sp-kqger base-suite cache)"` or `"ok     cached …"`) is read by
  the same `parse::red_suites`/`ran_suites`/`attribute` every other base-trial output already
  is — no new attribution path, no new case in `parse::Attribution`.

### Decisions

* **Every suites-composition base trial is fences-only, not just the pre-suites-red case.**
  The two were already the same code path in spirit (sp-govet's own comment: a red before the
  suites step "is judged on the base's fences only… the base's suites answer no question this
  red asks") — sp-kqger's finding is that this is equally true *after* the suites step: the
  base's own mirrored selection never answered a question the branch's red suites did not
  already ask either, because attribution only ever reads the branch's own red suite names
  out of `base_out`.
* **The cache lives beside the tree certificates and the verdict cache, under the same
  `SPIRA_VERDICTS` root, through the same generic ports** — not a new `SPIRA_*` directory
  variable, not a new I/O boundary. `basecache.rs` mirrors `cert.rs`'s shape (pure
  path/render/parse, the engine drives the actual reads and writes) because the two problems
  are the same shape: "a directory of small files, keyed by things that must all match."
* **No cache eviction or TTL is implemented.** An abandoned base tree's entries are dead
  weight, not wrong answers (the directory is simply never read again once the landing ref
  moves past that tree) — cleanup is deferred to whatever eventually prunes `SPIRA_VERDICTS`
  wholesale, not invented here for one sub-directory of it.
* **The image tag is queried once per base trial, lazily** (only once a red suite has passed
  the `base_ran_set`/absent checks and there is actually a cache question to ask), not once
  per suite and not unconditionally — a trial whose only red suite is absent on the base, or
  whose base's own broad run already happens to mention it, never pays for it.

### Scar: the targeted rerun used bare `testenv`, invisible until this bead made it primary

Landed 2026-09-30 23:17Z; base-untestable went from 1 in 52 gates to 7 in 50 within hours
(concierge/sp-ooh1k, sp-aufxu, sp-31dm0). `gate.steps` here declares `bin SPIRA_TESTENV_BIN
testenv` — "the release's testenv could not run such a branch's suites at all" (sp-isom7) —
but both the targeted rerun (`base_rerun_cmd`, pre-existing since sp-hh5h0) and this bead's
own image-tag query called bare `testenv` on the release's PATH regardless, because the code
computing them sat **outside** the `if let (Some(bdef), Some(base_tools)) = …` block that
holds the base's own tool bindings — a scoping accident, not a deliberate choice. Bare
`testenv` exits 127 for a tree it cannot run; `base_rerun_cmd`'s own case clause maps that to
NO_VERDICT. `base_out` then never gained the suite's line at all — not run, not red — so
`parse::attribute` returned `BaseUntestable`, indistinguishable from a rerun that genuinely
could not judge. Invisible at 1/52 (the old mirror answered most suites directly; only a
selection mismatch ever reached the rerun); common once this bead made the rerun the primary
path for every red suite on such a tree.

**Fix:** `base_bdef`/`base_tools_ref` are read once, right after the `if let` closes (from
the same `base_gate`/`base_tools` it no longer consumes by value), and both the image-tag
query and the rerun now run through `with_bins`, and name `"${SPIRA_TESTENV_BIN:-testenv}"`
rather than a bare name — the environment variable when the base's own tree declares one,
the same bare name as always when it does not. No repository without a tree-owned `bin`
testenv sees any behavior change. `cargo test -p gate`'s
`a_tree_owned_testenv_reruns_through_its_own_binary_never_bare_testenv` and
`the_image_tag_query_also_uses_the_tree_owned_testenv` fix this as a positive control: a
fixture with `bin SPIRA_TESTENV_BIN testenv` declared, a branch red on a suite the base does
not share, must attribute `branch-red` — never `base-untestable` — and the rerun's own env
must carry the tree-keyed binary path, not nothing.

**Lesson for the next port out of this `if let`:** anything computed inside depends on that
scope ending where the destructuring does; a value needed later must be re-derived from the
`Option` it was matched out of (`.as_ref()`, not by value) rather than assumed to still be
in reach.

## Admission: visible, logged, re-read (sp-q20wb)

Seen 2026-09-30 ~19:05Z: two Concierge landing gates sat 20 minutes behind
`certify_par=2` host-wide admission slots (`run/gate-admission/slot.N.lock` held by other
gates) and (1) printed nothing at all — not even the composition line, so the wait looked
exactly like a hang; (2) `gate.log` reported `waited=0s` for gates that had in fact queued
(the field measured only the tree-lock wait that follows admission, never admission itself);
(3) raising `certify_par` 2→4 in `spira.toml` did not help the already-waiting gates — the
slot count was read from `Ctx`, which conf.sh exports once, before this process's own `exec`,
so nothing short of a new process ever saw the raised number.

### The fix

* **Visible.** sp-f4ig1 landed concurrently and now owns this part: `World::admission_try`
  takes the branch as `who` and records it against the slot directly
  (`spira_config::admission::gate_holder_line`), and `World::admission_wait_line` renders the
  occupancy line the admission loop prints — first, and every 60s while it keeps waiting —
  plus an inline `gate: admitted to a gate slot after <n>s` the moment a wait that had
  actually been announced ends. This bead's own contribution here — a small
  `admission_mark`/`admission_holder` port pair writing `slot.<n>.lock.holder` directly —
  was dropped at the merge in favour of sp-f4ig1's library: one holder-naming mechanism, not
  two. What stays this bead's own: the admission loop is never silent, and the loop below
  proves it by passing sp-f4ig1's `admission_wait_line` the *same* `par` this bead's own
  re-read produces (see "Re-read"), not a second derivation.
* **Logged.** `self.s.waited` — gate.log's `waited=` field — used to be set only by the
  tree-lock wait that runs after admission, unconditionally overwriting whatever (nothing)
  admission had set. It is now set right after the admission loop exits by any path (granted,
  timed out, or signalled), and the tree-lock section adds its own wait to that instead of
  replacing it (`let admission_waited = self.s.waited;` carried across, `self.s.waited =
  admission_waited + tree_waited`). The two waits are conceptually one queue from the caller's
  point of view — "how long before this gate did anything" — so one field sums them; the
  printed "waited Ns for TREE" message stays the tree-lock's own portion only, since the
  admission side already announced its own wait separately (sp-f4ig1's inline message, above).
* **Re-read.** `World::certify_par_live` re-derives `[spira] certify_par` straight from the
  config document (`spira_config::discover` + `load`, the same library call `gate_mode`
  already makes per-trial) on every single pass of the admission loop — not
  `Ctx.SPIRA_CERTIFY_PAR`, frozen at this process's own start. `admission_par` prefers this
  live read; only when no document resolves, or it sets no `certify_par`, does it fall back to
  the frozen `Ctx` value and then the derived-from-host default, exactly as before. A limit
  raised in the file reaches every gate already polling within one second (the loop's own
  `sleep_ms(1000)`), not only gates started afterward. `admission_par`'s result is the one
  `par` the loop uses both to bound the slot-try range and to pass into
  `admission_wait_line` — sp-f4ig1's own pool-sizing (`spira_config::admission::size_from_env`,
  which reads the *process* environment, frozen the same way `Ctx` is) sizes the compile and
  test pools, spawned fresh per job, but never substitutes for this read inside one
  long-lived gate process: one mechanism sizes the gate's own pool, not two.

### Decisions

* **The config file wins over the frozen environment during the poll.** `conf.sh`'s own
  documented precedence is environment, then file, then derived default — but the Rust
  process cannot tell, from `Ctx.SPIRA_CERTIFY_PAR` alone, whether that value reached the
  environment because an operator explicitly exported it or because conf.sh itself exported
  it from the file at gate start; both look identical by the time `Ctx` is built. Given that
  ambiguity, and that the reported defect is exactly "I edited the file and it didn't reach
  the waiting gate," `certify_par_live` is read first. The cost is a narrow case — an operator
  who explicitly pins `SPIRA_CERTIFY_PAR` in the gate's own calling environment *and* has an
  unrelated config document with its own `certify_par` set — decided in the file's favour
  instead of the shell's, for the ordinary case to just work.
* **sp-f4ig1's admission library superseded this bead's own holder-naming at the merge.**
  Both beads landed changes to the same wait loop; the reconciliation
  kept whichever mechanism was the more complete one for each concern — sp-f4ig1's shared
  `spira_config::admission` for visibility and holder identity (it also serves the compile and
  test pools, which this bead never touched), this bead's `certify_par_live` for the live
  size re-read (sp-f4ig1 does not re-read the gate pool's own size mid-wait) and its
  `self.s.waited` accounting (sp-f4ig1 did not log the admission wait at all). Nothing here
  changed `admission_try`'s or `compose::jobs`'s signature a second time; the merge took
  sp-f4ig1's versions of both.
* **No new admission pool semantics from this bead.** It was, and remains, scoped to
  visibility, logging and the live re-read of the gate's one pool's size — not to changing
  what is pooled or how many pools exist. That part is entirely sp-f4ig1's (`gate/DESIGN-
  admission.md`): three pools, compile/test/gate, gate's own staying the flock slots under
  `gate-admission/` it always used.

