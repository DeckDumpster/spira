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
| `SPIRA_GATE_SUITES` | `off` skips admission; in the key; passed through | `on` |
| `SPIRA_GATE_BEAD` | ejected-suites lookup, the key, `lc_certify` | — |
| `SPIRA_GATE_CALLER` | `by=` in the cache entry | the branch |
| `LANDSTATE` (lib.sh) | `<bead>.ejected`, else an `EJECTED` landstate row | `$SPIRA_RUN/landstate` |
| `SPIRA_GATE_BUDGET`, `SPIRA_GATE_ALL`, `SPIRA_CERTIFY_ALWAYS_COVERS`, `SPIRA_BATCH_MAXPAR`, `SPIRA_VERDICT_REPEAT_CONSIDERED`, `SPIRA_LINT_BIN`, `SPIRA_TESTENV_BIN`, `PATH`, `HOME` | passed through to the gate command | as bash |

### The gate command's environment (unchanged list, `env -i`)

`PATH=$HOME/.cargo/bin:$PATH`, `HOME`, `TERM=dumb`, `SPIRA_GATE_REPO`, `SPIRA_GATE_REPO_NAME`,
`SPIRA_GATE_BRANCH`, `SPIRA_GATE_BASE`, `SPIRA_GATE_SELECT_HEAD=<branch>`, `SPIRA_GATE_FILES`,
`SPIRA_GATE_HOST_CORES`, `SPIRA_GATE_EJECTED_SUITES`, `SPIRA_GATE_ALL` (default 0),
`SPIRA_GATE_SUITES` (default on), `SPIRA_CERTIFY_ALWAYS_COVERS`, `SPIRA_BATCH_MAXPAR`,
`SPIRA_VERDICT_REPEAT_CONSIDERED`, `SPIRA_GATE_BUDGET` (default 300), `SPIRA_RUN`,
`SPIRA_LINT_BIN`, `SPIRA_TESTENV_BIN`. Run as `timeout $SPIRA_GATE_TIMEOUT bash -c "$CMD"` in
the gate tree, stdout and stderr on one pipe. No lock descriptor reaches it (every descriptor
this binary opens is close-on-exec; the bash leaked the admission slot's fd 8).

**One value changes meaning: `SPIRA_GATE_BRANCH`** in the branch trial is the revision under
test. When the landing ref is already an ancestor of the branch that is the branch name, as
before. Otherwise it is the **merge commit's id** (below), because the repository's gate string
hands it to `testenv`, which builds its own worktree from it. `SPIRA_GATE_SELECT_HEAD` stays
the branch, so suite selection still reads what the branch changed. In the base trial it is the
landing ref, as before.

### The merge (the change)

After the landing ref `BASE` (`spira_landref`) and the branch both resolve:

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
harness-fault | timeout | base-timeout | base-untestable | lib-unavailable | died`, and
`no-evidence:<reason>` when a FAIL carried no message (downgraded to NO_VERDICT).

The message before the VERDICT line is the bash's, word for word, except the conflict and
merge-failed messages, which are new.

### Files it writes

| file | format | when |
|---|---|---|
| `$SPIRA_GATE_LOG` | `<UTC %Y-%m-%dT%H:%M:%SZ> <repo> <branch> waited=<n>s ran=<n>s rc=<status>[ <reason>][ compose=<label> phases=<phase>:<secs>,…]\n`, appended | every verdict once the repository resolved; the composition fields once one was chosen |
| `$SPIRA_VERDICTS/<key>` | `when=<UTC>\nat=<epoch>\nby=<caller>\nrepo=<name>\nbranch=<br>\nsuites=<csv or ->\ncompose=<label>\n`, written to `.<key>.<pid>` and renamed | PASS from a trial only |
| `$SPIRA_VERDICTS/trees/<repo>/<T>` (the tree certificate, `src/cert.rs`) | `verdict=PASS\nsource=gate\ntree=<T>\nrepo=<name>\nrev=<gate revision>\nbranch=<br>\nby=<caller>\nwhen=<UTC>\nat=<epoch>\nharness=<harness_h or ->\nsuites=<csv or ->\n`, temp + rename | every PASS (`pass`, `cached`, `syntax-only`) once the merged tree `T` is known |
| `$SPIRA_RUN/gate-admission/slot.<n>.lock` | empty; `flock` held for the trial | unless `SPIRA_GATE_SUITES=off` |
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
**merge** → `bash -n` → beads data → foreign harness → empty gate string (PASS
`syntax-only`) → preflight (`bash <path>` words exist on the base) → key → cache → admission →
sweep → tree lock → checkout the gate revision (proved by `rev-parse HEAD`) → **composition** →
branch trial (its phases) → PASS, or: harness-fault line / 124 / 75 → base trial on the landing
ref (the same composition, over the crates the base has) → attribution.

The mode (`gate_mode`) is read just before the key, since it is part of it.

The meter is armed as soon as the repository resolves, so the early refusals (conflict among
them) now write a gate.log row. The bash armed it later and those refusals were invisible in
the log (law-absence-needs-a-positive-control).

### Attribution (unchanged; `gate_attribute`)

Given the branch trial failed with an ordinary red:

* base trial not run, or it exited 124 or 75 → NO_VERDICT `base-untestable`
* base passed → FAIL `branch-red` (suite = first red)
* base red, and some suite red on the branch only → FAIL `branch-red` (first such suite), the
  message lists both sets
* base's reds are all timeouts → NO_VERDICT `base-timeout`
* else → BASE_FAIL `base-red`

Suite names come from the batch runner's lines: a `*.sh` word followed by `RED`, `TIMEOUT`,
`FAILED` or `was killed` (reds); `TIMEOUT`/`was killed` (timeouts); those plus `ok`,
`SKIPPED`, `SKIP-REQ`, `QUARANTINED-RED`, `DISABLED`, `UNREACHED` (ran).

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
| `unit`, ejected suites present | `suites(ejected)` | the same — a returned bead re-runs what the round named (item 6 narrows this) |
| `unit`, the touched set or the graph unreadable | `suites(no-diff)` / `suites(no-metadata)` | the same — no guessing a cheaper gate |
| `unit`, crates (± docs, config, suites) | `unit` | the gate string with **suites off**, then on the host: `cargo test --profile aeon -j J --no-run -p …` (build) and `cargo test --profile aeon -j J -p … -- --test-threads=J` (test) |
| `unit`, nothing buildable | `fences` | the gate string with **suites off** |

"Suites off" is the gate command's environment: `SPIRA_GATE_SUITES=off` and
`SPIRA_CERTIFY_ALWAYS_COVERS=` (empty). `gate-touched.sh` then runs its fences (build fence,
tier budgets, plan matrix, lockfile lint) and selects nothing: its always-covers default,
`spira/lib.sh`, is a script, and a script never composes as unit or fences.

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
  `rc=0 pass compose=unit phases=fences:41,build:63,test:4`; base-trial phases carry `base-`.
  `yield.sh` takes the note's first word as the reason, so the trailing fields are compatible.
* the verdict file: a `compose=<label>` line.
* stderr, before the trial: `gate: composition=<…>` naming the crates and the touched ones.

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
* **A script touched anywhere keeps today's whole sequence**, including suites, until that
  component moves to Rust (the operator's default, 2026-09-29). Executables with no extension
  count as scripts, by their mode.
* **Everything the switch cannot judge falls back to `suites`** (an invalid config, an
  unreadable diff or graph, ejected suites, `SPIRA_GATE_ALL`) — the stronger check, never a
  cheaper one on a guess.
* **`gate-lib.sh` is retired.** Its functions are ported (`src/parse.rs`, `src/key.rs`) with
  unit tests; `test-gate-unit.sh`, which only exercised them, is retired with it.
