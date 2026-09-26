# Test plan — Test and CI infrastructure (`test-infrastructure`)

> **2026-09-25: suite timing converged onto `run/tsd/`.** The `refs/notes/suite-times` ledger
> and `suite-times.sh` (UC-34, G11 below) are retired; `testenv-batch.sh` now appends each
> suite-timing row via `tsd-write`, CI uploads it as an artifact, `tsd-ingest.sh` pulls it into
> the coordinator's `run/tsd/`, and every former reader queries `tsd-query.sh` instead.
> `test-suite-times.sh` is gone; `test-tsd.sh` and `test-tsd-ingest.sh` cover the replacement.

> **2026-09-25: `test-testenv-publish.sh` deleted** (sp-2076n; law-a-test-that-flips-is-deleted). It flipped in the full-corpus round 6 run. Its coverage — the suites job holding packages:write, and publish pushing the closure tag rather than :latest — is lost until a deterministic test is written.

> **2026-09-25: `test-suites-timeout.sh` deleted** (sp-95ooh; `law-a-test-that-flips-is-deleted`; flake:test-suites-timeout.sh, 6 recurrences). It was the suite the row below describes as "applied" for UC-30, and it also carried real-process cases for UC-25 and UC-29 and the systemd structural checks named in row 33. **UC-25 lost coverage:** the real watchdog killing a TERM-trapping suite and classifying it `timeout`, not `red` (the pure `classify()` table in `test-suites-classify.sh` still covers rc→status mapping, but not the watchdog's own kill detection). **UC-29 lost coverage:** `# timeout: N` deferring a suite to `unreached` when the declared minimum exceeds the remaining budget (real budget-wall arithmetic for a size the suite has never run is otherwise untested; `test-suites-unreached.sh` covers the last-runtime-based defer). **UC-30 lost coverage:** the per-suite watchdog killing a hung suite by process group and the runner continuing to the next suite, and a leaked background child marking its own suite red without wedging the pass — UC-30's only remaining coverage after this deletion is none. The systemd unit's structural checks (`TimeoutStartSec`, `SPIRA_SUITES_MAXSEC` injection, `SuccessExitStatus=2`) that row 33 lists as living here are also gone. See Gaps G17; replacement filed as sp-z3i42.

> **2026-09-25: UC-30 applied** (sp-lrljk). `test-suites-timeout.sh` absorbed `test-suites-watchdog-classify.sh` (TERM-trap classification) and `test-suites-result-files.sh` (the real budget-exhaustion → unreached row) and switched to a stub filer and `SPIRA_SUITES_SKIP_TESTDB=1`, so it needs no bd. `test-suite-cleanup.sh` deleted (never called suites.sh; the leaky-child case in test-suites.sh already covers the runner). **Correction, same day**: `test-suites.sh` had already been deleted whole by sp-wqdse (`law-a-test-that-flips-is-deleted`) before that sentence was written, so the leaky-child case had no home; `test-suites-timeout.sh` now carries it directly. The rest of section 3's coverage map — UC-24/-27 remainder (minus the leaky-child case, now covered above; test-suites.sh itself no longer exists to slim down), UC-23, UC-10 through -20, and everything after — is still open.

Part of [[test-plan-2026-09-23]], section 5. Area id `test-infrastructure`; use-case ids are `UC-test-infrastructure-NN`.

Scope: the machinery that selects, runs and reports suites. That covers `select.sh` and suite header metadata (`# covers:`, `# requires:`, `# exclusive:`, `# timeout:`, `# priority:`, `# selects-on:`, `# host-reason:`, `# defect:`). It also covers `testenv.sh` (image and container), `testenv-batch.sh` (runner, verdict cache, parallelism), `testdb.sh` fixtures, `suites.sh` (timed runner: budget, watchdog, red classification, filing, quarantine hygiene), `suite-assert.sh` (ASSERTIONS trailer), `suite-state*.sh`, `suite-times.sh`, `gate-retry.sh`, `gate-diag.sh` and `gate-check.sh` (annotation to bead), and the `gate.yml`/`release.yml`/`acceptance.yml`/`testenv-image.yml` workflows.

Inputs: 53 primary files (42 with CI timings, 531 suite-seconds; 11 absent from the main-push run 35947142904) and 5 secondary files (`test-batch-repeat-refused-delivers.sh`, `test-host-reason.sh`, `test-orphan-test.sh`, `test-install-self-test.sh`, and the Rust-crate record). Sources: mapper records `map/*.jsonl`, reducers `r0` suite-runner/ci-pipeline, `r1` test-infrastructure, `r2` test-harness and `r3` test-harness-ci, and `signals.tsv`. The harness source was read to confirm the gaps. No suite was executed.

---

## 0. Tier model: proposed amendments

The T0 to T4 ladder fits most of this area. Two facts from this area break it:

1. **Nested podman does not exist in CI.** Eleven suites (`test-testenv*.sh` ×9, `test-parallel-isolation.sh`, `test-requires.sh`, `test-suites-containment.sh`) exit 77 at a podman pre-flight inside the testenv container. They are absent from the main-push record. Their **container-free parts** also never run: `test-testenv-batch.sh` Part C/D, `test-testenv-mode.sh` Part A, `test-requires.sh` Part A and `test-testenv-stdin.sh` Part A/B all sit behind the skip or are not counted. Today, T3 "real container" coverage for the runner is zero in CI. **Add a tier, T3c (container smoke):** runs where podman is native, meaning the `testenv-image.yml` job (when the image closure tag changes) and the release acceptance VM. It is not part of the per-branch batch.
2. **T0 lints currently pay container and suite overhead.** Examples: `test-script-exec.sh` costs 5 s for a stat walk, and `test-covers-entries.sh` costs 5 s. Split **T0** out as a **lint stage** that runs before the batch (next to `inventory.sh`, `literal-lint.sh` and `scratch-fence.sh` in gate.yml), not as `test-*.sh` inside the batch.

Where each tier runs, in the terms used below:
- **lint**: T0 stage in gate.yml before provisioning, plus certification.
- **cert**: `gate-spira.sh` certification, meaning `spira/gate-suites` under a 300 s budget.
- **PR-CI**: `select.sh`-selected batch on PR and queue branches.
- **main-CI**: full corpus on a push to main.
- **image**: `testenv-image.yml` (T3c).
- **accept**: `acceptance.yml` on the release VM (T4).

---

## 1. Intent

Taken from what the tests check, the infrastructure must do four things.

- **Choose the right suites for a change.** It maps a diff to suites through `# covers:` globs (down to function granularity). It falls back to the whole corpus when a changed file is unmapped, refuses an unclaimed source file, and ignores inert files. One selector, `select.sh`, serves CI, certification and the timed pass.
- **Run the chosen suites hermetically, in one container per batch.** Each suite gets a private HOME, instance, run dir and testdb copy, no inherited `$TMUX`, and a per-suite timeout.
- **Classify every outcome honestly.** A suite's result is one of: ok, red, skip, skip-req, timeout, unreached, setup-fault, fixture-fault, red-unconfirmed, harness-fault (exit 2 in the batch, 75 in CI). A missing signal never reads as green: missing trailer means red, unreadable list means refuse, unmeasurable cost means `?`/over budget.
- **Turn persistent outcomes into exactly one deduplicated, labelled bead.** It clusters same-cause reds, quarantines flakes on a branch (never in the production checkout), and reactivates them only when the bead has landed and clean runs have accrued.

It must never write to the production bead store. It must leave a per-suite record (`.result`, `suite-times.tsv`, GitHub annotations, a step summary) that later runs and the cockpit can read. The workflows must wire all of this into a gated, reproducible, toolchain-pinned release.

---

## 2. Use cases

Dimensions (from taxonomy): correctness (C), fail-closed (F), observability (O), idempotency (I), concurrency (X), recovery (R), config-compat (K). Contract and static-shape checks are marked (S).

### A. Selection and suite metadata

| ID | Requirement | Dims | Tier / where |
|---|---|---|---|
| UC-test-infrastructure-01 | The header parser extracts `# covers:` (first line), `# requires:` (comma or space separated), `# exclusive:`, `# timeout:`, `# priority:` and `# selects-on:`, and returns empty with no declaration or a missing file. | C | T1 / lint+cert |
| UC-test-infrastructure-02 | Every declared `# covers:` token (every line, not only the first) resolves to an existing file, and every suite metadata key is well-formed. | F,S | T0 / lint |
| UC-test-infrastructure-03 | Selection over a changed-file set works as follows. A covered file selects its covering suites plus the always-run suites. An unmapped file selects all suites (`mode=all`) unless `--no-all-fallback` is set. An empty diff selects only the always-run suites. Inert files select nothing. `selects-on: added,mode` fires only on A or mode changes. `--files` and `--base/--head` give identical output. | C,K | T1 (table over `--files`) / lint+cert |
| UC-test-infrastructure-04 | `file#func` covers narrow selection to hunks inside that function. A global hunk selects all of the file's suites. `--files` mode is conservative. | C | T2 (git) / PR-CI |
| UC-test-infrastructure-05 | A changed file matching `SPIRA_SELECT_SOURCE` that no suite claims makes selection fail, naming the file. `--report-file` lists unclaimed and unplaced files. | F,O | T1 / lint+cert |
| UC-test-infrastructure-06 | A queue-branch diff selects the union of its members' suites and falls back to all if any member is unmapped. | C | T2 / PR-CI |
| UC-test-infrastructure-07 | `gate-spira.sh`, `testenv-batch.sh`, `gate-touched.sh`, `suites.sh names` and gate.yml all delegate to `select.sh`. There is one selector with no private copy. | S | T0 / lint |

### B. Container runner (`testenv.sh`, `testenv-batch.sh`)

| ID | Requirement | Dims | Tier / where |
|---|---|---|---|
| UC-test-infrastructure-08 | The image tag is a content hash of the build closure (Containerfile, doctor.sh program list, bd pin). It is path-independent and unchanged by unrelated files. | C,I | T1 / cert |
| UC-test-infrastructure-09 | Image acquisition pulls the closure tag from the registry when one is configured. It builds on a miss (not an error) and builds with no registry configured. Publish pushes exactly the closure tag and never `:latest`. Callers get one `localhost/` ref regardless of source. | R,K | T1 (podman argv stub) / cert |
| UC-test-infrastructure-10 | `testenv.sh up` gives a container with systemd as PID 1, a reachable user systemd, the checkout at `/workspace` and cargo cache volumes. `down` is idempotent. | K | T3c / image |
| UC-test-infrastructure-11 | Batch argument contract: an unknown `--mode` or a missing branch exits 2. `--mode=x` is accepted and the default is parallel. `--suites a,b` and `--suites -` run exactly the named suites (`producer=explicit`). An unknown name exits non-zero and names it. Empty stdin means "nothing to run" and exits 0. With no `--suites`, `producer=diff`, or `all` on fallback. | C,F | T1 (pre-container, needs `--print-selection` seam) / cert |
| UC-test-infrastructure-12 | The batch runs the suites **as they exist on the named branch**, not the caller's working tree. | C | T2 (git worktree, podman stub) / PR-CI |
| UC-test-infrastructure-13 | Exit contract: all green exits 0. Any red exits 1 (branch fault). A container death, start failure or exec storm exits 2 (harness fault), recording unrun suites as `unreached` without overwriting completed results. gate.yml maps 2 to 75. | C,F,R | T2 (podman stub) + one T3c smoke |
| UC-test-infrastructure-14 | Liveness: one failed `podman inspect` is not death. `Running=false` is death and the container is removed. Zero-second empty exec failures with a live container are a harness fault, not reds. On parallel death, in-flight reds become unreached. | F,R | T2 (podman stub) / PR-CI |
| UC-test-infrastructure-15 | Isolation: in parallel mode each suite gets a distinct HOME, SPIRA_INSTANCE and SPIRA_RUN (in serial mode they share). No suite inherits `$TMUX`. Runner-injected vars are stripped from the primary launch. A suite cannot read host files. | X,F | T2 (assert exec argv/env via podman stub) + one T3c smoke for real containment |
| UC-test-infrastructure-16 | `# exclusive:` suites drain the parallel pool before starting. Maxpar = min(nproc, ⌊(MemAvailable−reserve)/per-suite⌋) and names its binding resource. `SPIRA_BATCH_MAXPAR` is a ceiling only. | C,K | T1 / cert |
| UC-test-infrastructure-17 | `# requires:` unmet means the suite is recorded `skip-req` with the missing token in the fingerprint and not run. It stays distinct from a plain `skip` (77). Neither makes the batch red. | C,O | T1 (PATH-controlled dispatch) / cert |
| UC-test-infrastructure-18 | Per-suite `SPIRA_SUITE_TIMEOUT` reaps a suite as `timeout` (fingerprint `timeout:*`). Later suites continue, the batch exits non-zero, and 0 disables the limit. | R | T2 (podman exec stub that sleeps) / PR-CI |
| UC-test-infrastructure-19 | Verdict cache: a repeat attempt at the same (branch, sha, mode, selection) key after red is refused with exit 2 unless the override carries a reason of at least 10 characters, which is recorded. A refusal files one incident per branch (deduplicated). | F,I,O | T2 (stub filer) / PR-CI |
| UC-test-infrastructure-20 | Constants mirrored from `testenv.sh` match, and every `_CONTAINER_*` used is declared. The batch cleanup trap removes the fixture home and owner file on TERM and on normal exit. | S,R | T0 (constants) + T2 (real `_batch_cleanup`) |

### C. Fixtures (`testdb.sh`)

| ID | Requirement | Dims | Tier / where |
|---|---|---|---|
| UC-test-infrastructure-21 | The batch builds one shared bd baseline and hands suites `TESTDB_SHARED/BASELINE/BD`. `testdb_up` gives each borrower a private copy distinct from the baseline and from other concurrent borrowers. `testdb_drop` removes it. | X,C | T2 (real bd, once) / PR-CI |
| UC-test-infrastructure-22 | A vanished baseline makes `testdb_up` exit 75 (fixture-fault). On any `testdb_up` failure, SPIRA_DB is unset, so no suite can write to production. A fixture that cannot be built causes a skip (77), never a pass. | F | T1 (stub `TESTDB_SERVER_BD`) + T0 lint "every `testdb_up` is guarded" |
| UC-test-infrastructure-23 | `testenv.sh scratch` prints a new, empty, working DB distinct from SPIRA_DB. `testenv.sh shell [-c]` points SPIRA_DB at a fixture and tears down SPIRA_DB, SPIRA_RUN and SPIRA_SPOOL on exit. | F,R | T2 (one real bd) + T1 teardown with stubbed testdb_up |

### D. Timed runner (`suites.sh`)

| ID | Requirement | Dims | Tier / where |
|---|---|---|---|
| UC-test-infrastructure-24 | Discovery is the `test-*.sh` glob. Gated suites are not re-run and the pass says so. Suites without covers are named as omissions and still run. Every real suite is claimed by the gate or the timed run. The real `gate-suites` names only suites that exist, and an unreadable gate list refuses with rc 1, runs nothing and shows status `?`. | C,F | T1 (discovery) + T0 (real-tree claims) / lint |
| UC-test-infrastructure-25 | Classification of one suite outcome from (rc, output, watchdog-killed, shared-fixture, trailer, budget): ok, red, skip, timeout (even when TERM is trapped), setup-fault (ASSERTIONS 0 and rc≠0), red when the trailer is missing, fixture-fault (75 with shared fixture, else red), red-unconfirmed (no budget to confirm). Signal exits map to 124. | C,F | T1 (table, after extracting `classify()`) / cert |
| UC-test-infrastructure-26 | The failure fingerprint is stable across ISO timestamps, per-suite slice length and debris output. The cause fingerprint (normalised first FAIL line) groups same-cause suites. | C,I | T1 (table over `fingerprint`/`cause_fp`) / cert |
| UC-test-infrastructure-27 | Filing: each red files one bead with ref `suite:<name>`, sin-exempt, labelled plan + `repo:`, at the declared priority, with its output. If there is no output, the body carries a "no output" sentinel. A recurring or changed failure stays on one bead and logs a recurrence. Same-cause reds file one cluster bead naming all members, with a stable ref and a suppressed count reported. Fixture-faults file one bead naming all borrowers. Setup-faults name the suite, not the covered file. Timeouts name the limit. Greens and skips file nothing. | O,I | T1 (body/ref builders, stub filer recording env) + **one** T2/T3 real-bd dedupe case / PR-CI |
| UC-test-infrastructure-28 | Environment confirmation: a red under the runner is re-run with the runner vars stripped. It is confirmed red (suite-defect bead), passes (environment-finding bead via `file_env_red`), or is recorded red-unconfirmed with nothing filed when the budget is short. | C,O | T2 (stub filer) / PR-CI |
| UC-test-infrastructure-29 | Budget is a wall. `MAXSEC` caps the budget (MAXSEC−60) and never raises it. A suite whose last runtime exceeds the remaining budget, or whose `# timeout:` exceeds it, is `unreached` (not TIMEOUT). The cursor names where the next pass starts. Unreached never overwrites the last verdict or runtime and is cleared when the suite is next reached. | C,R | T1 (budget arithmetic, `record_*`/`unreached_*`) / cert |
| UC-test-infrastructure-30 | The per-suite watchdog kills a hung suite at its limit (process group). The runner continues. A leaky background child holding stdout does not wedge the pass and marks its suite red. | R | T2 (real processes, stub filer, PERSUITE=1) / PR-CI |
| UC-test-infrastructure-31 | Quarantine lifecycle. Two distinct flake runs inside the window, with a run_id counted once, quarantine the suite on a `spira-suite-state/auto-*` branch submitted to the queue, never in the checkout. Reactivation needs both bead LANDED and N clean runs. The max-age mail is sent once per period. A nonexistent suite is refused. | C,I,F | T1 (window/threshold/predicate with canned `bd show`) + T2 (branch, git) / cert |
| UC-test-infrastructure-32 | The suite-state file parser, `state_of`, write, clear and lint behave as specified: replace never duplicates, `active` stores nothing, and bad lines are ignored. The fence refuses a quarantine that has no bead, no reason, a missing suite or a **CLOSED** bead, and passes an empty file. | C,F | T1 / lint+cert |
| UC-test-infrastructure-33 | The suites systemd unit runs `suites.sh run` periodically. It injects `SPIRA_SUITES_MAXSEC = TimeoutStartSec`, and the conf default budget is under it. It treats exit 2 as success. It is installed only under `SPIRA_SELF_TEST`. | K,S | T0 / lint |

### E. Reporting, CI workflows, meta-tooling

| ID | Requirement | Dims | Tier / where |
|---|---|---|---|
| UC-test-infrastructure-34 | **One report grammar.** Every suite emits `ok`/`FAIL` lines in one format and an `ASSERTIONS n` trailer through `suite-assert.sh`. The runner writes one `.result` schema (`status ts secs fp mode producer rc`) and one `suite-times.tsv` row per suite (run_id, suite, rc, wall, bd_calls, and — new — pass/fail/skip counts). The row is persisted to `refs/notes/suite-times`. `suite-times.sh` reports the Top 20 slowest, the sum and wall time, and movers above 25%. | O,S | T1 (helper + report) + T0 (every suite sources the helper) |
| UC-test-infrastructure-35 | CI serial retry. Only red and timed-out suites are re-run once, serially, at the same sha, with timeouts given `GATE_RETRY_RERUN_TIMEOUT`. Red then green is a pass with a `flaky suite` warning. Red twice fails with a `red-twice suite` error. Structural failure (more than max, or more than half hard-red, timeouts excluded) fails without retry. A harness fault passes through. | C,F,O | T1 / cert |
| UC-test-infrastructure-36 | Red diagnostics. For each red suite, print its FAIL lines, a tail excerpt, `::group::`/`::error file=` annotations, and a step-summary table of reds only showing rc and red-green/red-red. A red with no output shows `(no output)`. An all-green batch prints nothing and exits 0. | O | T1 / cert |
| UC-test-infrastructure-37 | `gate-check.sh` files one bead per `flaky suite` annotation, and one P1 bead per `red-twice suite` annotation on failed main runs. It deduplicates on rescans and raises an existing lower-priority bead to P1. The annotation strings match what `gate-retry.sh` emits (producer/consumer contract). | C,I | T1 (canned gh JSON) + one T2 real-bd case + T1 contract test |
| UC-test-infrastructure-38 | Workflow shape. The real gate runs (inventory, literal-lint, scratch-fence, testenv-batch). PRs run the diff selection, a main push runs the corpus, empty selection skips provisioning and exits 0, and a provision failure fails the job with 75. There is a per-run VM that is always torn down, with every required action input passed. Release is gated on main via `workflow_call` with a pinned toolchain, no `continue-on-error`, all `--*-bin` flags and tag retraction on publish failure. Acceptance is dispatched with an App token. The suites job has `packages: write` and does not pin maxpar. | S,K | T0 (YAML-parsing lint) / lint |
| UC-test-infrastructure-39 | The CI runner host check (`runner-deps.sh --check`) refuses a broken container runtime with a non-75 exit, a MISSING line and "branch not at fault". A working host passes. Mutating steps are gated behind check-only. | F,O | T1 (stub podman, dry-run seam) / cert |
| UC-test-infrastructure-40 | The citation report classifies each suite's `# defect:` as resolved (with status), unresolved or uncited. | O | T1 (stub `bd show`) / cert |
| UC-test-infrastructure-41 | Every non-test, non-sourced operator script is executable. | S | T0 / lint |
| UC-test-infrastructure-42 | Certification budget. `gate-spira.sh` times itself. Over `SPIRA_GATE_BUDGET`, or an unmeasurable cost, files one harness bead ("something must leave gate-suites") and is not a branch failure. **Untested today (see Gaps).** | O,F | T1 / cert |
| UC-test-infrastructure-43 | A suite skipping (77) on consecutive timed passes files a `suite-skip:<name>` bead, so that "a check that cannot run is not a check that passed". A skip must exit 77, never 0. **Untested today.** | F,O | T1 (stub filer) + T0 lint |

---

## 3. Coverage map

Costs are `ci_secs` from main-push run 35947142904. NA means the suite is absent from that run, so it has zero effective CI coverage.

| UC | Existing tests (file::case) | Level & cost now | Verdict |
|---|---|---|---|
| 01 | test-dummy.sh::covers/requires extracted, first covers only; test-requires.sh::A0–A3; test-testenv-batch.sh::A4 (exclusive) | T1 1 s; requires NA; batch NA | **MERGE-INTO new `test-suite-covers.sh` (T1)**: dummy (whole), requires Part A, testenv-batch A4. Rename away from "dummy". |
| 02 | test-covers-entries.sh::all declarations resolve | T0 in batch, 5 s | **MERGE-INTO new `test-suite-metadata-lint.sh` (T0, lint stage)**. Also validate continuation `# covers:` lines (test-czar-pass.sh line 2 is unchecked). |
| 03 | test-select.sh::A1–D2, F1–F2, G1–G5, I, L, O, P; test-testenv-batch.sh::A1–A3; test-testenv-suites.sh::C | T2 19 s; batch A NA | test-select **KEEP, trim**: run B/C/F and G as one table over both input modes; delete E8 (≡K1); change K2/P from real-tree to fixtures. testenv-batch A1–A3 **DELETE** (a stale copy of the selector: A3 asserts "unmapped selects 0", which contradicts B5c and select.sh). |
| 04 | test-select.sh::M1/M1-ctrl/M2 | T2 (inside the 19 s) | KEEP (T2, git) |
| 05 | test-select.sh::H1–H4, J0–J2 | inside 19 s | KEEP, move to the T1 table |
| 06 | test-select.sh::N1 | inside 19 s | KEEP (T2) |
| 07 | test-select.sh::E1–E8, K1 (grep counts in callers) | source-grep | **SOURCE-GREP**: replace with a T1 check that runs each caller with `SPIRA_SELECT=<stub>` and asserts the stub was invoked with the expected flags (`--no-all-fallback` for batch, `--files` for gate-spira). |
| 08 | test-testenv-image-tag.sh::all | T1 5 s | KEEP; delete the "narrow closure" control (it tests sha256sum) |
| 09 | test-testenv-registry.sh::1–6; test-testenv-publish.sh::sections 2–3 | T1 3 s + 5 s | registry **KEEP**. publish **MERGE-INTO test-testenv-registry.sh** (near-verbatim duplicate). Its gate.yml `packages: write` check moves to the workflow lint. |
| 10 | test-testenv.sh::all; test-testenv-systemctl.sh::all | NA, NA | **MERGE** into one `test-testenv-image.sh` and **move to T3c** (run in testenv-image.yml). Drop the docker.io pull control and the "second build faster" wall-clock assertion. |
| 11 | test-testenv-mode.sh::A1–A3; test-testenv-stdin.sh::A,B,C,D; test-testenv-suites.sh::A1,A2,B1,B2 | all NA | **MERGE** stdin and suites (same fixture and assertions) and mode A into new `test-testenv-batch-unit.sh`. **DEMOTE-TO-T1** via a `--print-selection` seam so they run before any container. |
| 12 | test-testenv-batch-branch.sh::D1,D2 | NA | **DEMOTE** to T2 in testenv-batch-unit: real git worktree, podman stub that runs the mounted suite on the host. |
| 13 | test-testenv-batch.sh::B1–B3, B4, B5a–c; test-testenv-batch-branch.sh::D1 | NA | B4/B5 **DEMOTE** (stub podman). B1/B2/B3 **KEEP as the T3c smoke** (green → 0, red → 1, kill → unreached, not overwritten). |
| 14 | test-testenv-batch.sh::B9, B10a–c, B11 | NA (B10/B11 already use a stub podman but sit behind the pre-flight skip) | **DEMOTE**: move into testenv-batch-unit, where they run without a pre-flight. B9 needs a stub exec that blocks, then the stub reports `Running=false`. |
| 15 | test-parallel-isolation.sh::B1; test-testenv-mode.sh::C, D1, D2; test-testenv-tmux-isolation.sh::C1,C2 (grep), B1 (NA); test-suites-containment.sh::probe (NA); test-suites-confirm-red.sh::strip cases | NA ×3; tmux-isolation 2 s (grep only; exits 0 on skip) | env/argv properties (HOME, INSTANCE, RUN, `-e TMUX=`) **DEMOTE** to T2: record the exec argv via the podman stub. One real **T3c** containment probe merges parallel-isolation B1, mode D2 and the containment probe. tmux-isolation **SOURCE-GREP** is replaced by the argv check. |
| 16 | test-batch-maxpar.sh::A1–A8, B; test-gate-vitals.sh::A2,A3; test-testenv-batch.sh::B6a,B6b; test-testenv-batch.sh::B12a,B12b | T1 6 s; vitals 1 s; batch NA | batch-maxpar **KEEP** (rename `test-testenv-batch-maxpar.sh`; misnamed). vitals A2/A3 and batch B6a/B6b **DELETE** (duplicates). B12 (exclusive drain) **DEMOTE** to T2 stub. |
| 17 | test-requires.sh::B–D | NA | **DEMOTE** to T1: PATH-controlled per-suite dispatch in testenv-batch-unit |
| 18 | test-testenv-timeout.sh::A1–A3, B1 | NA | **DEMOTE** to T2: stub `podman exec` that sleeps, with timeout=1 |
| 19 | test-testenv-batch.sh::B7a–f, B8; test-batch-repeat-refused-delivers.sh (secondary, 25 s, never calls testenv-batch.sh) | NA; 25 s | B7 **DEMOTE** to T2 with a stub filer (run with no container: the verdict cache is pre-exec). B8 (real incident.sh dedupe) **DELETE**; it belongs to the incident area. Recommend ops-detection rewrite repeat-refused-delivers to call testenv-batch's real filing path. |
| 20 | test-testenv-batch.sh::C1–C3, D-pos/D1–D5 | NA | C **MOVE to T0** lint (never runs today). D **DELETE** (tests an in-test stub trap) and replace with a T2 test of the real `_batch_cleanup` (the function exists at testenv-batch.sh:484). |
| 21 | test-testdb-concurrent.sh::all; test-testenv-batch-baseline.sh::B1,B2,C1–C3 | T2 10 s; 7 s | concurrent **KEEP**. baseline Part A **DELETE** (tests its own `check_baseline`), Part B **KEEP** (runner env contract), Part C **MERGE-INTO test-testdb-concurrent.sh**. |
| 22 | test-testdb-failsafe.sh::testdb_up non-zero, SPIRA_DB unset, six-suite sweep, count unchanged; test-testdb-concurrent.sh::gone baseline 75 | T2 15 s | **DEMOTE-TO-T1**: keep the two testdb_up contract cases with a stub server bd. Replace the hardcoded six-suite sweep with a T0 lint "every `testdb_up` call is followed by `|| exit`". |
| 23 | test-testenv-scratch.sh::all | T2 40 s (≈4 real bd inits) | **DEMOTE (partial)**: one real-bd "scratch opens and is empty". Teardown and `-c` run with stubbed testdb_up. Fix the vacuous "production not mutated" case, which re-counts the scratch DB; point it at a stand-in production store. |
| 24 | test-suites.sh::gate-suites names only existing, every suite claimed, glob discovery, gated not re-run, no-covers omission, unreadable gate list, gate-spira list refusals | T3 part of 80 s | real-tree claims **MOVE to T0**. Discovery/gated/omission **DEMOTE** to T1 via sourced `all_suites`/`gated_suites`/`is_gated`. Unreadable list stays as one T2 case. |
| 25 | test-suites-setup-fault.sh::all; test-suites-fixture-fault.sh::all; test-suites-watchdog-classify.sh::TERM-trapping; test-suites-confirm-red.sh::red-unconfirmed; test-suites.sh::77 status; test-suites-timeout.sh::rc 124 grep | T3 24+18+25+16 s | **DEMOTE-TO-T1**: one classification table (one row per status) after extracting `classify()` from `cmd_run`. The watchdog TERM-trap row keeps one real process in UC-30. |
| 26 | test-suites.sh::timestamped failure one bead; test-suites-unreached.sh::different budgets same fp; test-suites-watchdog-classify.sh::fp identical; test-suites-cluster.sh::same-cause grouping | T3 in 80/35/25/41 s | **DEMOTE-TO-T1**: a `fingerprint`/`cause_fp` table (the functions exist at suites.sh:218 and :242). This deletes three watchdog waits used only to produce fingerprints. |
| 27 | test-suites.sh::one bead for the red (8 asserts), recurrence, changed failure, priority; test-suites-red-dedup.sh::all; test-output-visibility.sh::all; test-suites-cluster.sh::all; test-suites-fixture-fault.sh::bead cases; test-suites-setup-fault.sh::bead cases | T3: 80 (part), 32, 15, 41, 18, 24 s | **Keep one** real-bd dedupe/recurrence case in test-suites.sh. red-dedup **DELETE** (it re-proves incident.sh; its suites.sh link is a **SOURCE-GREP**, replaced by a stub filer asserting `SPIRA_INCIDENT_REF=suite:<s>` and `SPIRA_SIN_EXEMPT=1`). output-visibility **DEMOTE-TO-T1** (body builder). Cluster, fixture-fault and setup-fault bead assertions **DEMOTE** to a stub filer (labels, titles and member names are in its recorded env and stdin). |
| 28 | test-suites-confirm-red.sh::all | T3 16 s | **DEMOTE** to T2 with a stub filer. Add the missing **positive** `file_env_red` case (only its absence is asserted today). |
| 29 | test-suites.sh::budget wall, MAXSEC, pre-skip; test-suites-unreached.sh::red preserved, .unreached lifecycle, secs preserved; test-suites-result-files.sh::unreached record; test-suites-timeout.sh::declared-timeout deferred | T3: 80 (part), 35, 41, 9 s | **DEMOTE-TO-T1**: `record_write`/`unreached_*` plus budget arithmetic over a STATE dir. result-files **MERGE-INTO test-suites-timeout.sh** and unreached shrinks to the T1 table. |
| 30 | test-suites-timeout.sh::hung killed, continues, bead names limit; test-suites-result-files.sh::timeout result; test-suites-watchdog-classify.sh::all; test-suites.sh::leaky background child; test-suite-cleanup.sh::all | T3 9, 41, 25 s; cleanup 1 s | **KEEP test-suites-timeout.sh** as the single T2 watchdog test with PERSUITE=1 and a stub filer, absorbing watchdog-classify (TERM trap). test-suite-cleanup **DELETE**: it tests `setsid`, never calls suites.sh, and the leaky-child case in test-suites.sh covers the runner. |
| 31 | test-suites-flake-branch.sh::all; test-suites-hygiene.sh::all | T2 1 s; T3 12 s | flake-branch **KEEP**. hygiene Part 1 **DELETE** (duplicate of flake-branch). Window/dedup/reactivation/max-age **DEMOTE-TO-T1** with canned `bd show` and a stub mail. |
| 32 | test-suite-state.sh::all; test-suite-state-fence.sh::all | T1 2 s; T2 4 s | suite-state **KEEP** (the model bash unit test; delete the dead `result`-file block). fence **MERGE-INTO test-suite-state.sh**: its three structural rules duplicate `suite_state_lint`, and the CLOSED-bead rule becomes T1 with a stub `bd show`. |
| 33 | test-budget-drift.sh; test-suites-timer.sh; test-suites-timeout.sh::unit TimeoutStartSec/MAXSEC/SuccessExitStatus; test-install-self-test.sh (secondary) | T0 2 + 3 + part of 9 s | **MERGE** into one T0 `test-suites-unit-lint.sh` (lint stage) |
| 34 | test-suite-times.sh::A1–A4 (report), B0–B7 (NA inside CI: exits early); test-suites-setup-fault.sh::suite-assert helper; test-gate-ci-diag.sh (fixes the `.result` schema by fixture) | T1 1 s | suite-times A **KEEP**. B **DEMOTE** to T2: run the per-suite wrapper on the host with the bd shim; B5 must fail when bd_calls = 0. Add a T1 test of `suite-assert.sh` itself and a T0 adoption lint (see Gaps G1). |
| 35 | test-gate-retry.sh::all; test-gate-retry-structural.sh::all | T1 5 + 2 s | **MERGE** structural into test-gate-retry.sh (identical stub and `first()` helper) |
| 36 | test-gate-ci-diag.sh::1–8 (section 9 greps gate.yml) | T1 1 s | **KEEP**; section 9 moves to the workflow lint |
| 37 | test-gate-check-flaky.sh::all; test-gate-check-red-twice.sh::all | T2 8 + 15 s | **MERGE** into `test-gate-check-annotations.sh`: T1 table over canned gh JSON plus the existing-bead list, and one real-bd filing/dedupe/raise case. Add the retry → check string contract (Gap G4). |
| 38 | test-gate-workflow.sh::1–19; test-release-workflow.sh::1–8; test-gate-vitals.sh::A0,A1,B3–B5; test-testenv-publish.sh::section 1; test-gate-ci-diag.sh::9 | T0 1 + 3 + 1 + part of 5 + part of 1 s | **MERGE** into one T0 workflow lint in the lint stage, rewritten on `yq`/actionlint instead of substrings (e.g. `'75'` anywhere, `always()` anywhere). Delete gate-workflow 16a (tests `find`) and gate-vitals B1/B2/C (test the test's own sampler and `kill`). |
| 39 | test-runner-deps.sh::broken runtime, MISSING, not 75, branch not at fault; 9 grep cases | T1+grep 4 s | behaviour cases **KEEP**. 9 grep cases **SOURCE-GREP**: add `--check --dry-run` printing the planned commands, then assert on that. |
| 40 | test-citations.sh::all | T2 6 s | **DEMOTE-TO-T1** (stub `bd show` returning found or not-found JSON) |
| 41 | test-script-exec.sh::all | T0 in batch 5 s | **MERGE-INTO test-suite-metadata-lint.sh** (lint stage). Read the sourced-only list from headers rather than hardcoding it. |
| 42 | none | – | **GAP** (G2) |
| 43 | none (grep: no suite mentions `suite-skip:` or "skipping on consecutive") | – | **GAP** (G3) |
| (secondary) | test-host-reason.sh (14 s), test-orphan-test.sh (12 s) | T2 | Owned by safety-fences. Note: host-reason spends most of its 14 s on a real `suites.sh status` sweep of the shipped tree. The orphan-test "broken matcher" control is tautological. |

---

## 4. Duplicate clusters

| # | Behaviour | Files (evidence) | Keep |
|---|---|---|---|
| D1 | Hung suite becomes timeout via the watchdog | test-suites-timeout.sh (PERSUITE=3 on `sleep 300`), test-suites-result-files.sh (35 s wait on `sleep 300`), test-suites-watchdog-classify.sh (two PERSUITE=3 kills and a 6 s control), test-suites-unreached.sh (four kills to compare fingerprints), all with the same planted fixture and the copied `sut()` harness. About 110 s of watchdog waiting in total. | **test-suites-timeout.sh** at PERSUITE=1 with a stub filer. Fingerprint properties go to the T1 table. |
| D2 | One bead per red suite, recurrence on the same bead | test-suites.sh (recurrence, changed failure, timestamps), test-suites-red-dedup.sh (8 real incident.sh filings, 32 s; its suites.sh link is only a grep), test-output-visibility.sh ("one bead filed per red suite"), test-suites-cluster.sh (second pass reuses the bead) | **test-suites.sh**: one real-bd case. The rest use a stub filer. |
| D3 | N suites to one aggregate bead | test-suites-cluster.sh, test-suites-fixture-fault.sh (same shape: bead names every member, no per-member bead) | Both keep their decision rows in the T1 classification/cluster table. **One** shared aggregate-filing case with a stub filer. |
| D4 | Two-observation quarantine on a branch, checkout clean | test-suites-flake-branch.sh (1 s, stubbed) and test-suites-hygiene.sh Part 1 (real bd and a worktree) | **test-suites-flake-branch.sh** |
| D5 | Suite-state lint rules (no bead, missing suite, missing reason) | test-suite-state.sh (sourced `suite_state_lint`) and test-suite-state-fence.sh (same rules through the fence with a real DB) | **test-suite-state.sh**, plus the CLOSED-bead row with a stub `bd show` |
| D6 | spira-suites unit static parse | test-budget-drift.sh, test-suites-timer.sh, test-suites-timeout.sh (static half) | One **T0 unit lint** |
| D7 | gate.yml Suites step and workflow greps | test-gate-workflow.sh, test-gate-vitals.sh (same awk extraction), test-gate-ci-diag.sh §9, test-testenv-publish.sh §1, test-release-workflow.sh | One **T0 workflow lint** (yq-based) |
| D8 | Maxpar derivation | test-batch-maxpar.sh (behaviour), test-gate-vitals.sh A2/A3 (same sed extraction), test-testenv-batch.sh B6a/B6b | **test-batch-maxpar.sh** |
| D9 | Registry and publish | test-testenv-registry.sh and test-testenv-publish.sh §2–3 (identical podman stub, fixture and asserts) | **test-testenv-registry.sh** |
| D10 | Explicit selection via `--suites` list or stdin | test-testenv-stdin.sh and test-testenv-suites.sh (same fixture, same C/B1 asserts), plus test-testenv-batch.sh B5b | One T1 case table in **test-testenv-batch-unit.sh** |
| D11 | Container pre-flight up/probe/down | Copied verbatim in 7 testenv suites and duplicating test-testenv.sh | One shared helper, and one T3c image suite that owns up/probe/down |
| D12 | Suite metadata parsing | test-dummy.sh (covers and requires), test-requires.sh Part A (requires), test-testenv-batch.sh A4 (exclusive), test-covers-entries.sh (covers resolution) | **test-suite-covers.sh** (T1 parse) plus **test-suite-metadata-lint.sh** (T0 tree) |
| D13 | Selection logic | test-select.sh vs test-testenv-batch.sh Part A (a stale copy of the algorithm inside the test, contradicting select.sh on unmapped files); select E8 ≡ K1; G mirrors B/C/F | **test-select.sh**, trimmed |
| D14 | Annotation to bead with gh stub, real bd and count-by-title | test-gate-check-flaky.sh and test-gate-check-red-twice.sh (same harness; red-twice Part 4 re-asserts the flaky path) | One **test-gate-check-annotations.sh** |
| D15 | Serial retry stub batch | test-gate-retry.sh and test-gate-retry-structural.sh (same `GATE_RETRY_BATCH` stub and `first()`; both assert `--mode serial`) | **test-gate-retry.sh** |
| D16 | Shared-fixture failure and production isolation | test-testdb-failsafe.sh, test-testdb-concurrent.sh (exit 75), test-testenv-scratch.sh, test-testenv-batch-baseline.sh Part C | test-testdb-concurrent.sh (T2) plus the failsafe T1 contract |
| D17 | Copied ~20-line `sut()/beads()/count()` harness | ~10 test-suites-*.sh files | Not a test cluster but a helper duplication: extract `spira/testlib/suites-harness.sh` |

---

## 5. Unit-extractable logic

| Logic | Where | Tested today through | Seam that enables T1 |
|---|---|---|---|
| Outcome classification from (rc, output, killed, shared-fixture, ASSERTIONS trailer, confirm budget) | Inline in `suites.sh cmd_run` (~600 lines, suites.sh:642) | 5 suites, about 110 s, each running full inline passes plus bd | Extract `classify <rc> <outfile> <killed> <shared> <confirm_ok>` → status word. Add a source guard before the `case "${1:-list}"` dispatch (`[[ "${BASH_SOURCE[0]}" != "$0" ]] && return 0`) so tests can source the functions. |
| `fingerprint`, `cause_fp` | suites.sh:218, :242 (already functions) | Fingerprints are produced only by running and killing suites (unreached, watchdog-classify, suites, cluster) | Source guard only. Table: timestamps, slice length, debris and first-FAIL normalisation. |
| `record_write/read`, `unreached_write/clear/read`, budget arithmetic, `priority_of`, `timeout_of` | suites.sh:127–193 | test-suites.sh (budget passes with sleeps), test-suites-unreached.sh, test-suites-result-files.sh (35 s wait) | Source guard plus `STATE=$TMP`. The pre-skip decision should become `would_fit <last_secs> <declared> <remaining>`. |
| Bead body/ref builders for `file_red`, `file_cluster`, `file_fixture_fault`, `file_setup_fault`, `file_env_red`, `file_skip` | suites.sh:272–640 | Real incident.sh on an embedded Dolt DB (output-visibility 15 s, red-dedup 32 s, cluster 41 s) | `INC` is already a variable (`bash "$INC" file`). Point it at a stub that records env and stdin; assert REF, LABELS, PRIORITY, SIN_EXEMPT and body text. No bd needed. |
| Flake window, run_id dedup, clean-run count, reactivation predicate, max-age once | suites.sh:1399–1616 | test-suites-hygiene.sh (real bd, worktree) | Source guard. `bd show` stubbed via `SPIRA_BD` returning canned `land_state` JSON. `mail.sh` stubbed. |
| Runner env strip (RUNNER_VARS → `env -u` argv) | suites.sh confirm path | test-suites-confirm-red.sh, 4 inline passes (16 s) | A function that prints the launch argv; assert on it. |
| Arg parsing, selection, `--suites`/stdin validation, producer field | testenv-batch.sh, before container start | test-testenv-{mode,stdin,suites}.sh: **NA, never run in CI** | Add `--print-selection` (exit after writing the selection, producer and mode). The case tables then become T1. |
| Verdict key and repeat refusal | testenv-batch.sh `_batch_key` (:326) and the refusal path | test-testenv-batch.sh B7: **NA** | Source guard for `_batch_key`. The refusal happens before the container, so a stub podman plus `SPIRA_BATCH_INCIDENT_CMD` suffice. |
| Liveness and exec-storm classification | testenv-batch.sh `_container_check_live` (:877) and the exec loop | B10/B11 use a PATH podman stub but sit behind the real-podman pre-flight: **NA** | Move them into a suite with **no** pre-flight; the PATH stub is already the seam. |
| Exec env (HOME, INSTANCE, RUN, `-e TMUX=`) per suite | testenv-batch.sh parallel exec | Real containers (NA) or a count of `"TMUX="` strings | The podman argv recorder stub already used by the registry tests. |
| `_batch_cleanup` trap | testenv-batch.sh:484 | test-testenv-batch.sh D, **a copy of the trap written in the test** | Source guard; call the real function with `SPIRA_INSTANCE=$TMP`. |
| Annotation parse, dedupe and raise | gate-check.sh | Two suites on real Dolt (23 s) | A function taking gh JSON plus the existing-bead list and returning actions (file, skip or raise). `bd` is reached through `bdq` (`SPIRA_BD`), which is stubbable. |
| `testdb_up` failure contract | testdb.sh | A six-suite sweep plus a real-bd stand-in (15 s) | `TESTDB_SERVER_BD` stub (already used) with no real bd, plus a T0 guard lint. |
| runner-deps check steps | runner-deps.sh | 9 greps over source | `--check --dry-run` printing the command plan |

---

## 6. Gaps

- **G1 — No standard report, and the helper that would provide one is unused.** `spira/suite-assert.sh` (29 lines: ok/bad/is/want/nowant plus the `ASSERTIONS n` trailer) is sourced by **2 of ~466** suites. About 31 files print their own `ASSERTIONS` line. At least five summary wordings exist (`%d passed, %d failed`, `N ok, M FAIL`, `Results: …`, `ASSERTIONS N` only, and `ok:`/`FAIL:` to stderr). suites.sh treats a missing trailer as red, so the setup-fault classification is effectively disabled for the corpus. `suite-times.tsv` has no pass/fail/skip counts. `timings-main.tsv` only knows ok and RED, and the 19 not-run suites leave no row. There is no test that a suite's output conforms to the grammar and no lint that suites source the helper. **Add:** a T1 test of suite-assert.sh (count, trailer on early exit, trap chaining), a T0 adoption lint (ratcheted), and a runner-side JUnit/TSV emitter test.
- **G2 — Certification budget is untested.** `gate-spira.sh` (lines ~376–400) handles `SPIRA_GATE_BUDGET` (default 300), `file_budget_bead`, and "unmeasurable counts as over budget". Grep finds no suite referencing `SPIRA_GATE_BUDGET` or `file_budget_bead`. This is the mechanism that forces "something must leave gate-suites", and nothing verifies it files a bead, or that `?` counts as over budget.
- **G3 — Persistent-skip filing is untested.** `suites.sh file_skip` (:523) files a `suite-skip:<s>` bead ("A check that cannot run is not a check that passed"). No suite mentions `suite-skip`. The consecutive-skip detector is also unasserted. Related: test-testenv-tmux-isolation.sh exits 0 when it skips, and nothing lints "skip ⇒ exit 77".
- **G4 — The retry → gate-check annotation contract is never tested end to end.** `gate-retry.sh` emits `::warning title=flaky suite::` and `::error title=red-twice suite::`. gate-check.sh parses them from canned gh JSON typed by hand in its tests. A wording change on one side stays green on both. **Add** a T1 test that feeds gate-retry's actual stdout to gate-check's parser.
- **G5 — The whole container runner has zero CI coverage.** The eleven NA suites include the verdict cache (UC-19), exit and liveness contract (UC-13/14), parallel isolation (UC-15), skip-req (UC-17), batch timeout (UC-18), branch checkout (UC-12, bead sp-2f51e) and containment (test-suites-containment). None of them runs on any push, and test-suite-times.sh Part B also skips. The regression scar sp-u1g ("unreached must not overwrite a completed status") is guarded in CI for suites.sh only, not for testenv-batch B3.
- **G6 — The testenv-batch real cleanup trap is untested.** Part D tests an in-test stub, and its `trap … EXIT` also replaces the suite's `rm -rf $TMP` (leak).
- **G7 — Multi-line `# covers:` is silently truncated.** `suite_covers_of` reads the first line (asserted as intended by test-dummy "first covers only"). test-covers-entries validates only `head -1`. test-czar-pass.sh has a continuation line on line 2, so changes to its second-line files do not select it. A decision is needed: reject multi-line covers in the T0 lint, or parse them.
- **G8 — `file_env_red` positive path.** No test drives a suite that passes only when stripped and asserts the "passes in an aeon" environment-finding bead. test-suites-confirm-red asserts only its absence (sp-ezs7o/xw80r scars).
- **G9 — `suite-shape.sh`** (the wave-classifier of suites by host-access shape) has no suite at all.
- **G10 — Timed-pass producers `suites.sh names` / `corpus`** (law-a-runner-takes-a-list) have no direct test of "one name per line, empty means exit 0, unreadable gate list means refuse". Only test-host-reason touches `names`, and only incidentally.
- **G11 — `persist-suite-times` in gate.yml** writes `refs/notes/suite-times` on the merged sha and swallows every failure (`|| true`). Nothing checks that the note is written or readable by `suite-times.sh`. It is the only cross-run comparability record.
- **G12 — Harness-fault exit mapping in gate.yml** (testenv-batch 2/3 → 75; gate-retry passthrough) is checked only by the substring `'75'` anywhere in the file. The Suites step script should be extracted to `spira/gate-suites-step.sh` and table-tested on (batch rc, retry rc) → job rc.
- **G13 — The testdb failsafe covers a hardcoded six suites.** More than 180 suites mention testdb.sh. The comment says "four". New users are unprotected; this needs a T0 guard lint (UC-22).
- **G14 — Gate fence wiring in `gate-spira.sh`** ("suite-state-fence.sh missing — refusing to land unchecked", orphan-test 77 → SKIPPED) is asserted only by grepping for the fence name. It needs a behaviour test with the fence removed or failing.
- **G15 — test-suites-fixture-fault's header promises a SPIRA_DB RESTORE property that no assertion implements** (scars sp-n0xj7/sp-5l05i/sp-7nblo).
- **G16 (cross-area)** — no workflow runs `cargo test`. The broker, czar-pass and supervise crates have zero `#[test]`. The Rust tests reach CI only through bash shims. Owned by landing and ops, but the workflow lint (UC-38) should assert that a `cargo test` step exists once there is one.
- **G17 — UC-30 has zero coverage after `test-suites-timeout.sh` was deleted for flipping** (sp-95ooh; see the note above). UC-25 and UC-29 keep their pure-logic coverage in `test-suites-classify.sh`/`test-suites-unreached.sh` but lose the real-process cases named above. **Add**, at the lowest tier that still proves the property (T1 over a pure core; fall back to a real T2 process only for the hung-kill and leaky-child cases, which need an actual process group): a replacement for the watchdog kill/continue and leaky-child-marks-red behavior, and a declared-`# timeout:` defer case. Filed as a replacement bead rather than rewritten in the same sitting that deleted the flake (`law-a-test-that-flips-is-deleted` forbids investigating a flip; a hasty rewrite risks reproducing it).

---

## 7. Cost

**Current** (primary files with main-push timings, 42 files; 11 more are NA and cost 0 while providing 0 coverage):

```
suites 80 + suites-cluster 41 + suites-result-files 41 + testenv-scratch 40
+ suites-unreached 35 + suites-red-dedup 32 + suites-watchdog-classify 25
+ suites-setup-fault 24 + select 19 + suites-fixture-fault 18 + suites-confirm-red 16
+ gate-check-red-twice 15 + output-visibility 15 + testdb-failsafe 15 + suites-hygiene 12
+ testdb-concurrent 10 + suites-timeout 9 + gate-check-flaky 8 + testenv-batch-baseline 7
+ batch-maxpar 6 + citations 6 + covers-entries 5 + gate-retry 5 + script-exec 5
+ testenv-image-tag 5 + testenv-publish 5 + runner-deps 4 + suite-state-fence 4
+ release-workflow 3 + suites-timer 3 + testenv-registry 3 + budget-drift 2
+ gate-retry-structural 2 + suite-state 2 + testenv-tmux-isolation 2
+ dummy 1 + gate-ci-diag 1 + gate-vitals 1 + gate-workflow 1 + suite-cleanup 1
+ suite-times 1 + suites-flake-branch 1
= 531 suite-seconds
```

The runner-classification cluster (suites, cluster, result-files, unreached, red-dedup, watchdog-classify, setup-fault, fixture-fault, confirm-red, output-visibility, timeout, hygiene) is **348 s, or 66%** of the area. Most of that is watchdog `sleep` waits and real-Dolt incident filings that assert things a stub filer would show.

**Projected** after the verdicts:

| Target file | Tier | Secs | Replaces |
|---|---|---|---|
| test-suites.sh (slim: one real-bd dedupe/recurrence, leaky child, unreadable list) | T3 | 25 | suites 80 |
| test-suites-timeout.sh (single watchdog, PERSUITE=1, stub filer) | T2 | 8 | timeout 9, result-files 41, watchdog-classify 25 |
| test-suites-classify.sh (new T1 tables: classify, fingerprint, cause_fp, records, budget) = unreached remainder | T1 | 1 | unreached 35 |
| test-suites-cluster.sh | T1+stub | 3 | 41 |
| test-suites-fixture-fault.sh | T1+stub | 3 | 18 |
| test-suites-setup-fault.sh (+ suite-assert T1) | T1 | 2 | 24 |
| test-suites-confirm-red.sh | T2 stub | 4 | 16 |
| test-suites-hygiene.sh | T1 | 3 | 12 |
| test-output-visibility.sh | T1 | 1 | 15 |
| test-suites-red-dedup.sh | DELETE | 0 | 32 |
| test-suites-flake-branch.sh | T2 | 1 | 1 |
| test-suite-state.sh (+ fence rows) | T1 | 3 | 2 + 4 |
| test-suite-cleanup.sh | DELETE | 0 | 1 |
| test-suite-times.sh (A + host-run B) | T1/T2 | 2 | 1 |
| test-select.sh (trimmed, T1 table plus T2 git) | T1/T2 | 8 | 19 |
| test-suite-covers.sh (new) | T1 | 1 | dummy 1 (+ NA parts) |
| test-suite-metadata-lint.sh (new; lint stage) | T0 | 2 | covers-entries 5, script-exec 5 |
| test-suites-unit-lint.sh (new; lint stage) | T0 | 1 | budget-drift 2, suites-timer 3 |
| workflow lint (new; lint stage) | T0 | 2 | gate-workflow 1, release-workflow 3, gate-vitals 1 |
| test-gate-check-annotations.sh | T1 + 1 T2 | 6 | flaky 8, red-twice 15 |
| test-gate-retry.sh | T1 | 4 | 5 + 2 |
| test-gate-ci-diag.sh | T1 | 1 | 1 |
| test-runner-deps.sh | T1 | 2 | 4 |
| test-citations.sh | T1 | 1 | 6 |
| test-batch-maxpar.sh (renamed) | T1 | 3 | 6 |
| test-testenv-image-tag.sh | T1 | 3 | 5 |
| test-testenv-registry.sh | T1 | 3 | 3 + publish 5 |
| test-testenv-scratch.sh | T2 | 8 | 40 |
| test-testdb-concurrent.sh (+ baseline C) | T2 | 8 | 10 |
| test-testdb-failsafe.sh | T1 | 2 | 15 |
| test-testenv-batch-baseline.sh (Part B only) | T2 | 2 | 7 |
| test-testenv-batch-unit.sh (new; stub podman: args/selection/verdict cache/liveness/exec env/tmux/skip-req/timeout/branch/cleanup) | T1/T2 | 8 | tmux-isolation 2 (+ 9 NA suites' logic) |

```
25+8+1+3+3+2+4+3+1+0+1+3+0+2+8+1+2+1+2+6+4+1+2+1+3+3+3+8+8+2+2+8 = 121 suite-seconds
```

**531 → 121 s (−410 s, −77%)** of batch suite-seconds. At the same time CI coverage grows: the logic of the 11 NA suites (UC-11 to UC-20, now with zero CI coverage) moves into testenv-batch-unit and runs on every push. The 5 s of T0 moves out of the container batch into the lint stage. What remains truly container-bound goes to a new **T3c container smoke**: B1/B2/B3 green/red/kill, one containment probe, and the testenv up/probe/systemctl image suite. It costs an estimated ~60–90 s in `testenv-image.yml` and the release acceptance VM, and **0 s** per branch. File count for the area goes from 53 to about 32 (25 DELETE/MERGE; 4 new).

---

---

## 8. UC catalogue (machine-readable index for spira/plan-lint.sh)

One bullet per use case declared in section 2 above, in the format
`docs/test-plan/README.md` requires (`* \`UC-<area>-NN\` [T<n>] — <one-line>`).
The tier here is the cheapest tier that would catch a regression per that
schema; a covering suite's own `# tier:` need not match exactly. Section 2 is
authoritative; this is a direct copy of its Requirement column, not a second
draft of it.

* `UC-test-infrastructure-01` [T1] — The header parser extracts `# covers:` (first line), `# requires:` (comma or space separated), `# exclusive:`, `# timeout:`, `# priority:` and `# selects-on:`, and returns empty with no declaration or a missing file.
* `UC-test-infrastructure-02` [T0] — Every declared `# covers:` token (every line, not only the first) resolves to an existing file, and every suite metadata key is well-formed.
* `UC-test-infrastructure-03` [T1] — Selection over a changed-file set works as follows. A covered file selects its covering suites plus the always-run suites. An unmapped file selects all suites (`mode=all`) unless `--no-all-fallback` is set. An empty diff selects only the always-run suites. Inert files select nothing. `selects-on: added,mode` fires only on A or mode changes. `--files` and `--base/--head` give identical output.
* `UC-test-infrastructure-04` [T2] — `file#func` covers narrow selection to hunks inside that function. A global hunk selects all of the file's suites. `--files` mode is conservative.
* `UC-test-infrastructure-05` [T1] — A changed file matching `SPIRA_SELECT_SOURCE` that no suite claims makes selection fail, naming the file. `--report-file` lists unclaimed and unplaced files.
* `UC-test-infrastructure-06` [T2] — A queue-branch diff selects the union of its members' suites and falls back to all if any member is unmapped.
* `UC-test-infrastructure-07` [T0] — `gate-spira.sh`, `testenv-batch.sh`, `gate-touched.sh`, `suites.sh names` and gate.yml all delegate to `select.sh`. There is one selector with no private copy.
* `UC-test-infrastructure-08` [T1] — The image tag is a content hash of the build closure (Containerfile, doctor.sh program list, bd pin). It is path-independent and unchanged by unrelated files.
* `UC-test-infrastructure-09` [T1] — Image acquisition pulls the closure tag from the registry when one is configured. It builds on a miss (not an error) and builds with no registry configured. Publish pushes exactly the closure tag and never `:latest`. Callers get one `localhost/` ref regardless of source.
* `UC-test-infrastructure-10` [T3c] — `testenv.sh up` gives a container with systemd as PID 1, a reachable user systemd, the checkout at `/workspace` and cargo cache volumes. `down` is idempotent.
* `UC-test-infrastructure-11` [T1] — Batch argument contract: an unknown `--mode` or a missing branch exits 2. `--mode=x` is accepted and the default is parallel. `--suites a,b` and `--suites -` run exactly the named suites (`producer=explicit`). An unknown name exits non-zero and names it. Empty stdin means "nothing to run" and exits 0. With no `--suites`, `producer=diff`, or `all` on fallback.
* `UC-test-infrastructure-12` [T2] — The batch runs the suites **as they exist on the named branch**, not the caller's working tree.
* `UC-test-infrastructure-13` [T2] — Exit contract: all green exits 0. Any red exits 1 (branch fault). A container death, start failure or exec storm exits 2 (harness fault), recording unrun suites as `unreached` without overwriting completed results. gate.yml maps 2 to 75.
* `UC-test-infrastructure-14` [T2] — Liveness: one failed `podman inspect` is not death. `Running=false` is death and the container is removed. Zero-second empty exec failures with a live container are a harness fault, not reds. On parallel death, in-flight reds become unreached.
* `UC-test-infrastructure-15` [T2] — Isolation: in parallel mode each suite gets a distinct HOME, SPIRA_INSTANCE and SPIRA_RUN (in serial mode they share). No suite inherits `$TMUX`. Runner-injected vars are stripped from the primary launch. A suite cannot read host files.
* `UC-test-infrastructure-16` [T1] — `# exclusive:` suites drain the parallel pool before starting. Maxpar = min(nproc, ⌊(MemAvailable−reserve)/per-suite⌋) and names its binding resource. `SPIRA_BATCH_MAXPAR` is a ceiling only.
* `UC-test-infrastructure-17` [T1] — `# requires:` unmet means the suite is recorded `skip-req` with the missing token in the fingerprint and not run. It stays distinct from a plain `skip` (77). Neither makes the batch red.
* `UC-test-infrastructure-18` [T2] — Per-suite `SPIRA_SUITE_TIMEOUT` reaps a suite as `timeout` (fingerprint `timeout:*`). Later suites continue, the batch exits non-zero, and 0 disables the limit.
* `UC-test-infrastructure-19` [T2] — Verdict cache: a repeat attempt at the same (branch, sha, mode, selection) key after red is refused with exit 2 unless the override carries a reason of at least 10 characters, which is recorded. A refusal files one incident per branch (deduplicated).
* `UC-test-infrastructure-20` [T0] — Constants mirrored from `testenv.sh` match, and every `_CONTAINER_*` used is declared. The batch cleanup trap removes the fixture home and owner file on TERM and on normal exit.
* `UC-test-infrastructure-21` [T2] — The batch builds one shared bd baseline and hands suites `TESTDB_SHARED/BASELINE/BD`. `testdb_up` gives each borrower a private copy distinct from the baseline and from other concurrent borrowers. `testdb_drop` removes it.
* `UC-test-infrastructure-22` [T1] — A vanished baseline makes `testdb_up` exit 75 (fixture-fault). On any `testdb_up` failure, SPIRA_DB is unset, so no suite can write to production. A fixture that cannot be built causes a skip (77), never a pass.
* `UC-test-infrastructure-23` [T2] — `testenv.sh scratch` prints a new, empty, working DB distinct from SPIRA_DB. `testenv.sh shell [-c]` points SPIRA_DB at a fixture and tears down SPIRA_DB, SPIRA_RUN and SPIRA_SPOOL on exit.
* `UC-test-infrastructure-24` [T1] — Discovery is the `test-*.sh` glob. Gated suites are not re-run and the pass says so. Suites without covers are named as omissions and still run. Every real suite is claimed by the gate or the timed run. The real `gate-suites` names only suites that exist, and an unreadable gate list refuses with rc 1, runs nothing and shows status `?`.
* `UC-test-infrastructure-25` [T1] — Classification of one suite outcome from (rc, output, watchdog-killed, shared-fixture, trailer, budget): ok, red, skip, timeout (even when TERM is trapped), setup-fault (ASSERTIONS 0 and rc≠0), red when the trailer is missing, fixture-fault (75 with shared fixture, else red), red-unconfirmed (no budget to confirm). Signal exits map to 124.
* `UC-test-infrastructure-26` [T1] — The failure fingerprint is stable across ISO timestamps, per-suite slice length and debris output. The cause fingerprint (normalised first FAIL line) groups same-cause suites.
* `UC-test-infrastructure-27` [T1] — Filing: each red files one bead with ref `suite:<name>`, sin-exempt, labelled plan + `repo:`, at the declared priority, with its output. If there is no output, the body carries a "no output" sentinel. A recurring or changed failure stays on one bead and logs a recurrence. Same-cause reds file one cluster bead naming all members, with a stable ref and a suppressed count reported. Fixture-faults file one bead naming all borrowers. Setup-faults name the suite, not the covered file. Timeouts name the limit. Greens and skips file nothing.
* `UC-test-infrastructure-28` [T2] — Environment confirmation: a red under the runner is re-run with the runner vars stripped. It is confirmed red (suite-defect bead), passes (environment-finding bead via `file_env_red`), or is recorded red-unconfirmed with nothing filed when the budget is short.
* `UC-test-infrastructure-29` [T1] — Budget is a wall. `MAXSEC` caps the budget (MAXSEC−60) and never raises it. A suite whose last runtime exceeds the remaining budget, or whose `# timeout:` exceeds it, is `unreached` (not TIMEOUT). The cursor names where the next pass starts. Unreached never overwrites the last verdict or runtime and is cleared when the suite is next reached.
* `UC-test-infrastructure-30` [T2] — The per-suite watchdog kills a hung suite at its limit (process group). The runner continues. A leaky background child holding stdout does not wedge the pass and marks its suite red.
* `UC-test-infrastructure-31` [T1] — Quarantine lifecycle. Two distinct flake runs inside the window, with a run_id counted once, quarantine the suite on a `spira-suite-state/auto-*` branch submitted to the queue, never in the checkout. Reactivation needs both bead LANDED and N clean runs. The max-age mail is sent once per period. A nonexistent suite is refused.
* `UC-test-infrastructure-32` [T1] — The suite-state file parser, `state_of`, write, clear and lint behave as specified: replace never duplicates, `active` stores nothing, and bad lines are ignored. The fence refuses a quarantine that has no bead, no reason, a missing suite or a **CLOSED** bead, and passes an empty file.
* `UC-test-infrastructure-33` [T0] — The suites systemd unit runs `suites.sh run` periodically. It injects `SPIRA_SUITES_MAXSEC = TimeoutStartSec`, and the conf default budget is under it. It treats exit 2 as success. It is installed only under `SPIRA_SELF_TEST`.
* `UC-test-infrastructure-34` [T1] — **One report grammar.** Every suite emits `ok`/`FAIL` lines in one format and an `ASSERTIONS n` trailer through `suite-assert.sh`. The runner writes one `.result` schema (`status ts secs fp mode producer rc`) and one `suite-times.tsv` row per suite (run_id, suite, rc, wall, bd_calls, and — new — pass/fail/skip counts). The row is persisted to `refs/notes/suite-times`. `suite-times.sh` reports the Top 20 slowest, the sum and wall time, and movers above 25%.
* `UC-test-infrastructure-35` [T1] — CI serial retry. Only red and timed-out suites are re-run once, serially, at the same sha, with timeouts given `GATE_RETRY_RERUN_TIMEOUT`. Red then green is a pass with a `flaky suite` warning. Red twice fails with a `red-twice suite` error. Structural failure (more than max, or more than half hard-red, timeouts excluded) fails without retry. A harness fault passes through.
* `UC-test-infrastructure-36` [T1] — Red diagnostics. For each red suite, print its FAIL lines, a tail excerpt, `::group::`/`::error file=` annotations, and a step-summary table of reds only showing rc and red-green/red-red. A red with no output shows `(no output)`. An all-green batch prints nothing and exits 0.
* `UC-test-infrastructure-37` [T1] — `gate-check.sh` files one bead per `flaky suite` annotation, and one P1 bead per `red-twice suite` annotation on failed main runs. It deduplicates on rescans and raises an existing lower-priority bead to P1. The annotation strings match what `gate-retry.sh` emits (producer/consumer contract).
* `UC-test-infrastructure-38` [T0] — Workflow shape. The real gate runs (inventory, literal-lint, scratch-fence, testenv-batch). PRs run the diff selection, a main push runs the corpus, empty selection skips provisioning and exits 0, and a provision failure fails the job with 75. There is a per-run VM that is always torn down, with every required action input passed. Release is gated on main via `workflow_call` with a pinned toolchain, no `continue-on-error`, all `--*-bin` flags and tag retraction on publish failure. Acceptance is dispatched with an App token. The suites job has `packages: write` and does not pin maxpar.
* `UC-test-infrastructure-39` [T1] — The CI runner host check (`runner-deps.sh --check`) refuses a broken container runtime with a non-75 exit, a MISSING line and "branch not at fault". A working host passes. Mutating steps are gated behind check-only.
* `UC-test-infrastructure-40` [T1] — The citation report classifies each suite's `# defect:` as resolved (with status), unresolved or uncited.
* `UC-test-infrastructure-41` [T0] — Every non-test, non-sourced operator script is executable.
* `UC-test-infrastructure-42` [T1] — Certification budget. `gate-spira.sh` times itself. Over `SPIRA_GATE_BUDGET`, or an unmeasurable cost, files one harness bead ("something must leave gate-suites") and is not a branch failure. **Untested today (see Gaps).**
* `UC-test-infrastructure-43` [T1] — A suite skipping (77) on consecutive timed passes files a `suite-skip:<name>` bead, so that "a check that cannot run is not a check that passed". A skip must exit 77, never 0. **Untested today.**

