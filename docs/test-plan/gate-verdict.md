# Test plan — Per-branch gate verdict (`gate-verdict`)

> **2026-09-25: `test-gate-tree.sh` and `test-gate-locks.sh` rebuilt** (sp-fm2wn), closing
> gap #16. `test-gate-tree.sh` (deleted as sp-78xpb) now proves concurrency with start/end
> marker overlap detection instead of an elapsed-time threshold — a busy host slows every
> run by the same amount, which a wall-clock threshold read as "serialised"; markers read
> the actual command windows instead. `test-gate-locks.sh` (deleted as sp-fxvgo) is
> unchanged in shape and adds the genuine STALE positive gap #11 asked for.

Part of [[test-plan-2026-09-23]], section 5. Area id `gate-verdict`; use-case ids are `UC-gate-verdict-NN`.

Subject scripts: `spira/gate.sh` (761 lines), `gate-touched.sh` (96), `gate-locks.sh` (119), `gate-sweep.sh` (114), `yield.sh` (486), `lib.sh:host_cores`, `governor.sh` (host-core sizing only).
Primary test files (14, was 15 — `test-gate-missing-cmd.sh` merged into `test-gate-preflight.sh` by sp-ajxg3; `test-gate-host-cores.sh` deleted by sp-ztxbr, its one non-duplicate assertion folded into `test-gate-base-evidence.sh`'s env-contract row, the rest already covered by `test-gate-unit.sh`): `test-gate-base-evidence.sh`, `test-gate-base-selection.sh`, `test-gate-fixture-diag.sh`, `test-gate-locks.sh`, `test-gate-preflight.sh`, `test-gate-sweep.sh`, `test-gate-touched.sh`, `test-gate-tree.sh`, `test-gate-unit.sh`, `test-gate-verdict.sh`, `test-governor-host-cores.sh`, `test-reopen-queue-eject.sh`, `test-soak.sh`, `test-yield.sh`. No file is secondary to this area.
Tests in other areas that overlap this one: `test-select.sh` (19 s, covers:-selection), `test-certify-suites-off.sh` (6 s), `test-landing-gate-wait.sh` (17 s, lock-timeout), `test-auron.sh` (60 s, lock-timeout), `test-skew-foreign.sh` (3 s, skew exit 3), `test-landing-base-fail.sh` (34 s, BASE_FAIL consumer), `test-watchtower.sh`, `test-cockpit-probe-fault.sh`.

Fixture facts (signals.tsv and the mapper records): no file in this area touches bd, Dolt, tmux, systemd or podman. The cost comes from git (13 of 15 files build a bare remote plus a clone), from repeated whole `gate.sh` runs (each doing worktree add, a branch trial and, on red, a base trial), and from wall-clock `sleep` in the concurrency suites. Every file uses its own copy of `ok()/bad()/want()`. None of them produces machine-readable output.

---

## 1. Intent

`gate.sh <branch> [repo]` is the deterministic trial a branch must pass before it can merge. Every exit goes through one function, `verdict()`. It produces exactly one of four outcomes: PASS (0), FAIL (1, the branch is at fault), BASE_FAIL (76, the base fails its own gate) or NO_VERDICT (75, the machinery could not judge). Each outcome comes with a single anchored `gate: VERDICT=… reason=… branch=… repo=… suite=…` line that callers key on.

The gate fails closed without blaming the branch. A missing map, an unresolvable ref, a lock it cannot get, a timeout or a base trial that cannot run all withhold the verdict as NO_VERDICT. A red is charged to the branch only when a base trial shows that the base is green on the same suites.

The gate runs the repository's own command (from repo-map) in a per-branch worktree, serialised per branch by flock. It exports a fixed env contract to that command, including covers:-driven file selection, ejected suites and host cores. It caches PASS verdicts under a key made of tree, files, command and harness bytes, with a TTL.

The gate also meters its own cost to `gate.log` and records each red to `yield.sh`. yield.sh classifies reds as DEFECT, GATE_FAULT or UNKNOWN and reports cost and recorder health with `?`, never 0. The governor sizes CPU budgets from the host's online core count, not from the cgroup-limited `nproc`.

---

## 2. Use cases

Dimensions come from `taxonomy.json` (correctness, fail-closed, observability, idempotency, concurrency, recovery, config-compat, contract, performance, test-integrity). "cert" means the certification/gate lane that runs on every push.

| ID | Requirement | Dimensions | Tier | Runs in |
|---|---|---|---|---|
| UC-gate-verdict-01 | Every exit goes through `verdict()`, which prints exactly one anchored `gate: VERDICT=<PASS\|FAIL\|BASE_FAIL\|NO_VERDICT> reason=<slug> branch= repo= suite=` line and exits 0/1/76/75 to match. | contract, observability | T1 (source `verdict()` with the meter and yield stubbed) | cert |
| UC-gate-verdict-02 | A FAIL that carries no message is downgraded to NO_VERDICT with `reason=no-evidence:<orig>` (sp-io5j backstop). | fail-closed | T1 | cert |
| UC-gate-verdict-03 | An unreadable repo-map gives NO_VERDICT `no-repo-map-file` (this used to be a silent PASS). A repo absent from the map gives NO_VERDICT `no-repo-map`. | fail-closed, config-compat | T1/T2 | cert |
| UC-gate-verdict-04 | An unresolvable branch or base gives NO_VERDICT (`no-diff` / `no-base`), exit 75. The message names `base...branch`, the repo and git's `fatal:` line. | fail-closed, observability | T2 | cert |
| UC-gate-verdict-05 | Universal layer: a changed `*.sh` that fails `bash -n` gives FAIL `syntax`. So does beads data anywhere in the branch tree (`beads-data`). A branch that vendors a foreign harness copy gives FAIL `foreign-harness`. If `exclude.sh` or `skew.sh` is missing, or skew exits 3, the result is NO_VERDICT (`missing-exclude`, `missing-skew`, `skew-init-fault`). | fail-closed, correctness | T2 (T1 with exclude and skew stubbed) | cert |
| UC-gate-verdict-06 | A repo whose map row has an empty gate column passes with `reason=syntax-only`, and the VERDICT line is still printed. | contract | T2 | cert |
| UC-gate-verdict-07 | If a gate command names `bash <path>` and that path is absent from the base, the result is NO_VERDICT `cmd-missing-file` naming the path, never BASE_FAIL. Once the path exists, the command runs. (sp-lkzl) | config-compat, fail-closed | T2 | cert |
| UC-gate-verdict-08 | covers:-based selection. A suite is selected when its covers: names a changed file, or when it has no covers:, or when it is a meta suite covering `spira/test-*.sh`. A suite covering only unchanged files is not selected. A suite added by the branch is present on the branch tree and absent on the base. `SPIRA_GATE_FILES` overrides the computed diff. | correctness | T1 (file list in, suite list out) + one T2 diff case | cert |
| UC-gate-verdict-09 | Suites in `SPIRA_GATE_EJECTED_SUITES` (CSV) are always added. A suite that is both covered and ejected appears once. An ejected suite that no longer exists is skipped. (sp-px6ng) | correctness, recovery | T1 | cert |
| UC-gate-verdict-10 | The gate command receives the env contract: `SPIRA_GATE_BRANCH`, `_BASE`, `_SELECT_HEAD` (= the branch on *both* trials), `_FILES`, `_HOST_CORES` (= `host_cores()`, not `nproc`), `_EJECTED_SUITES`, `_SUITES`. | contract | T2 (one env-logging run) | cert |
| UC-gate-verdict-11 | `host_cores` returns `getconf _NPROCESSORS_ONLN` even when `nproc` is cgroup-limited. `governor.sh` writes `SP_CORES` from it. | correctness, config-compat | T1 | cert |
| UC-gate-verdict-12 | Base-trial attribution. Branch red and base green gives FAIL `branch-red`. The same suite red on both gives BASE_FAIL `base-red`, naming the suite and carrying the base output. Base reds that are all timeouts give NO_VERDICT `base-timeout`. Genuine reds plus timeouts give BASE_FAIL. Suites red only on the branch give FAIL even when the base is red elsewhere. An unnamed red on both gives `suite=-`. A base trial that exits 75/124 or cannot check out gives NO_VERDICT `base-untestable`. | correctness, fail-closed | T1 (pure function of rc and output text) + one T2 wiring run | cert |
| UC-gate-verdict-13 | A branch trial killed at `SPIRA_GATE_TIMEOUT` gives NO_VERDICT `timeout`, naming the budget and the command, and is not cached. A branch trial exiting 75 gives NO_VERDICT `harness-fault`. (sp-p4rl) | fail-closed, observability | T2 | cert |
| UC-gate-verdict-14 | A red verdict carries the command's full early diagnostics, not a `tail -20` window. A passing gate does not print them. | observability | T2 (assertion folded into the UC-12 wiring run) | cert |
| UC-gate-verdict-15 | Verdict cache. The key is repo + tree + changed files + command + harness bytes (gate.sh, exclude.sh, skew.sh) + `SPIRA_GATE_SUITES`. Only PASS is cached. Reuse is metered `rc=0 cached`. A landing on the base does not invalidate an un-rebased branch. A restored command finds its old entry. A verdict older than `SPIRA_VERDICT_TTL` is refused, as is an entry without `at=` or a non-numeric TTL. (sp-0v8) | correctness, idempotency, performance, fail-closed | T1 (key derivation + freshness) + T2 (reuse loop) | cert |
| UC-gate-verdict-16 | Gates on different branches run concurrently in separate trees and never see each other's content. Gates on the same branch serialise, and the wait is metered `waited=Ns`. (sp-64v0) | concurrency, observability | T3 | batch/main CI |
| UC-gate-verdict-17 | A gate that cannot get its tree lock within `SPIRA_GATE_LOCK_WAIT` gives NO_VERDICT `lock-timeout` ("not a fault") and never removes the holder's tree. Missing `flock` or an unopenable lockfile gives NO_VERDICT (`no-flock`, `no-lockfile`). (sp-d8h0r) | concurrency, fail-closed | T2 | cert |
| UC-gate-verdict-18 | Tree lifecycle. PASS keeps the detached worktree for reuse. Non-PASS removes it, but only when this process held the lock. A worktree that cannot be created gives NO_VERDICT `tree-unidentified`. | recovery | T2 | cert |
| UC-gate-verdict-19 | `gate-locks.sh` reports FREE, HELD (with PID) or STALE. A lock whose holder PID is dead but whose PGID has a live member (inherited fd) is HELD. A lock that does not exist is not listed. | observability, concurrency | T2 (real flock + /proc) | cert |
| UC-gate-verdict-20 | `gate-sweep.sh` removes stale `.gate.<repo>[.<key>]` worktrees and orphaned `/tmp/spira-batch-*` homes. It keeps any tree whose lock is held (and says so), young trees, and homes with a live container. (sp-ic8n, sp-q7d72) | recovery, concurrency | T2 | cert |
| UC-gate-verdict-21 | Under contention (N aeon gates plus a landing pass with a small lock wait), every gate reaches a verdict. None is starved to 75, no tree is crossed, landing verdicts come from the cache, and the whole soak finishes inside its deadline. | concurrency, performance | T3 soak | nightly / batch only |
| UC-gate-verdict-22 | Yield recording. Every non-PASS verdict is recorded with branch, bead, suite and tree. Gating the same tree twice records one red. A red tree changed to PASS becomes inferred DEFECT. BASE_FAIL arrives as GATE_FAULT. A failing recorder never changes the verdict or the exit code. (sp-1xb0) | correctness, idempotency, observability | T2 (two real gate runs) | cert |
| UC-gate-verdict-23 | Yield reporting. The configured window (`SPIRA_YIELD_WINDOW`) excludes older reds. A stated `classify` overrides an inferred one and keeps its reason. Classify refuses an unknown branch or an invalid verdict. Cost is split solo vs contended and excludes cached passes. An absent record or log renders `?`, never 0. A recorder that shows reds in the meter log but has an empty record is reported `silent` and withholds its counts. | correctness, observability, fail-closed | T1 (planted files in, KEY=VAL out) | cert |
| UC-gate-verdict-24 | Yield reaches Ops and the cockpit: `watchtower --show` carries the yield columns, and every `SP_YIELD_*` key is written by the collector, read by the pane and defaults to `?`. | observability, contract | belongs to cockpit-observability; T0 key-parity check there | cert |
| UC-gate-verdict-25 | Metering. Every verdict reached after the lock writes one `gate.log` row (rc, waited, ran). Preflight refusals write none. The EXIT trap is disarmed so a verdict is never metered twice, and a `set -e` death or signal is still metered once. | observability, test-integrity | T2 | cert |

_Use case ids, one line each, as `spira/plan-lint.sh` (sp-qu948's T0 lint) parses them — mechanically derived from the ID/Requirement/Tier columns of the table above; the table is the source of truth, this list exists only because the lint reads `* \`UC-<area>-NN\` [T<n>] — ...` and not a table row:_

Machine-readable declarations live in `docs/test-plan/gate-verdict.toml` (schema: `test-plan/schema/catalogue.schema.json`), read by `spira/plan-lint.sh`.

---

## 3. Coverage map

ci_secs are from main-push run 35947142904. "Level now" is the mapper's classification.

| Use case | Existing tests (file::case) | Level now, cost | Verdict |
|---|---|---|---|
| 01 VERDICT line shape | implicit everywhere; explicit full line only in `test-yield.sh::verdict line names the suite` | T2-T3, spread across | DEMOTE-TO-T1: one table test over `verdict()`; drop the full-line assertion from test-yield (it duplicates UC-12). |
| 02 no-evidence downgrade | **none** | — | GAP (§6) |
| 03 repo-map unreadable/unknown | **none** at gate level | — | GAP |
| 04 no-diff / no-base | `test-gate-preflight.sh::unresolvable branch`, `::unresolvable base`, `::valid branch passes` | T2, 3 s | KEEP, as the host file for the merged preflight suite |
| 05 universal layer | `test-skew-foreign.sh::init failure exits 3` (skew side only, instance-lifecycle area) | — | GAP at gate level |
| 06 syntax-only PASS | `test-gate-preflight.sh::an empty gate column exits PASS` | T2, folded in | DONE (sp-ztxbr) |
| 07 cmd-missing-file | `test-gate-preflight.sh::CASE 3/4` (was `test-gate-missing-cmd.sh::SEEN RED*`/`::SEEN GREEN*`) | T1, folded in | DONE (sp-ajxg3): merged into `test-gate-preflight.sh`, on the shared `gate-fixture.sh` builder; the SPIRA_HOME vs SPIRA_CONF env drift is resolved by always setting `SPIRA_CONF` to a nonexistent path |
| 08 covers: selection | `test-gate-touched.sh::A1-A5`, `::B1-B2`, `::C1-C3`; `test-reopen-queue-eject.sh::positive control`; `test-select.sh` (test-infra area, 19 s) | T2, 1 s + 2 s | DEMOTE-TO-T1 for the `SPIRA_GATE_FILES` rows (no git). Keep one diff-derived row and the base-tree row at T2. Collapse the 5 `sel()` calls into 1. |
| 09 ejected suites | `test-reopen-queue-eject.sh::ejected suite added`, `::CSV`, `::dedup`, `::absent ejected suite skipped` | T2, 2 s | MERGE-INTO `test-gate-touched.sh` as T1 rows |
| — (landstate sidecar) | `test-reopen-queue-eject.sh::.ejected sidecar survives`, `::no sidecar → empty`, `::land_mark EJECTED`, `::RED overwrites EJECTED` | T2 | DELETE the sidecar pair: tautological, since the test writes and reads the file itself. Replace it with a behaviour test (`_attr_eject` writer → gate.sh reader) in landing-merge-queue. Move the `land_mark` rows to landing-merge-queue. |
| 10 env contract | `test-gate-base-evidence.sh::the env contract reaches both trials, unchanged in shape`; `test-gate-base-selection.sh::branch trial selects from branch`, `::base trial also selects from branch`, `::second trial ran at the base` | T2, 3 s | DONE (sp-ztxbr): env-logging command added to the base-evidence wiring run, asserting `SPIRA_GATE_BRANCH/_BASE/_SELECT_HEAD/_FILES/_HOST_CORES/_EJECTED_SUITES/_SUITES` on both trials. `test-gate-host-cores.sh` deleted, its one row folded in here. |
| 11 host_cores / governor | `test-gate-unit.sh::host_cores()` (UC-11, T1) | T1 | DONE. `test-gate-host-cores.sh`'s stub-nproc/getconf controls were the duplicate of this and are deleted. `test-governor-host-cores.sh` was itself deleted with the governor (sp-8mzsh) — this row's own file list above is stale on that point, outside this bead's scope to sweep. |
| 12 attribution | `test-gate-base-evidence.sh` (all 6 cases); `test-gate-base-selection.sh::red on both charged to base`; `test-yield.sh::the gate refuses it as BASE_FAIL`, `::verdict line names the suite`, `::a red naming no suite` | T2, 4 s (+2 s, + part of 34 s) | DEMOTE-TO-T1 for the 6 classification rows and the unnamed-suite row. KEEP one T2 wiring run (red on both → BASE_FAIL with base output + env log + early diagnostic). DELETE the test-yield BASE_FAIL line assertions (redundant). |
| 13 timeout / harness-fault | `test-gate-verdict.sh::deadline is NO_VERDICT`, `::timeout not cached`, `::a branch trial reporting a harness fault exits NO_VERDICT`, `::and is not cached` | T2, part of 7 s | DONE (sp-ztxbr): harness-fault row added, on `gate-fixture.sh` now (item 2 migration). |
| 14 full output | `test-gate-fixture-diag.sh::positive control`, `::gate exits 1 / builder diagnostic survives`, `::diagnostic absent from a passing gate` | T2, 3 s | STILL OPEN. `test-gate-base-evidence.sh` now has an env-contract wiring run (UC-10) but not yet the full-output assertion; the fixture-diag merge itself is unstarted — follow-up. |
| 15 verdict cache | `test-gate-verdict.sh` (15 cases) | T2, 7 s | KEEP the T2 reuse loop (first run, cached, base landing, changed tree, red not cached). DEMOTE-TO-T1 the key and TTL rows (changed command, restored command, changed harness, expired, longer TTL, no `at=`). Add the `SPIRA_GATE_SUITES`-in-key and non-numeric-TTL rows. |
| 16 concurrent / serialised trees | `test-gate-tree.sh::gate 1/2 reached a verdict`, `::ran concurrently`, `::neither observed the other's branch`, `::serialised`, `::wait metered`; `test-soak.sh::no gate judged another branch's tree` | T2/T3, 9 s (+15 s) | DONE (sp-fm2wn). Rewrote `test-gate-tree.sh`: start/end marker overlap detection (keyed on `SPIRA_GATE_BRANCH`, the one label that survives gate.sh's own `env -i`) replaces the elapsed-time thresholds, `GATE_SECS` cut 3 → 1, `TREE_KEY` now calls `gate_tree_key()` from `gate-lib.sh` instead of a local re-implementation. |
| 17 lock-timeout | `test-gate-tree.sh::a gate that cannot get the tree returns NO_VERDICT` (one run, both assertions) | T2, part of 9 s | DONE (sp-fm2wn). The old case 3 and case 5 (two separate `LOCK_WAIT=1` runs) are merged into one held-lock run asserting both the NO_VERDICT/lock-timeout/"not a fault" message and holder-tree survival. Landing/auron copies belong to their areas and assert the consumer side, so left alone. |
| 18 tree lifecycle | `test-gate-tree.sh::gate exits 0/1 on a passing/failing branch`, worktree kept/removed | T2 | DONE (sp-fm2wn), inside the rebuilt test-gate-tree. `tree-unidentified` is still a GAP. |
| 19 lock report | `test-gate-locks.sh` 1-pos … 5d | T2, 12 s | DONE (sp-fm2wn). Added the genuine STALE row: a holder file naming a dead PID and a dead PGID while an unrelated process still holds the flock. |
| 20 sweep | `test-gate-sweep.sh` (7 cases) | T2, 6 s | KEEP. Already has `SPIRA_BATCH_HOME_GLOB` and `SPIRA_PODMAN_PS_FILE` seams. |
| 21 soak | `test-soak.sh` (7 cases) | T3, 15 s | KEEP, but move off the per-push lane to nightly/batch (tunable via `SOAK_*`). Fix its `covers: landing.sh` header, which is false because only gate.sh runs. |
| 22 yield recording | `test-yield.sh::the gate blames the branch`, `::recorded UNKNOWN`, `::record names branch/bead/suite`, `::does not double the red`, `::byproduct classification`, `::BASE_FAIL lands as GATE FAULT` | T2, part of 34 s | KEEP as a T2 wiring file with 2 gate runs (red then amended-pass, and base-red). Replace the other ~3 gate runs with planted records. |
| 23 yield reporting | `test-yield.sh::unwritten record … ?`, `::window configured`, `::stated verdict overrides`, `::classify refusals`, `::window excludes`, `::wide window finds`, `::cost split`, `::cached passes left out`, `::missing gate log`, `::absent`, `::healthy meter`, `::recorder silent`, `::real record not withheld` | T2, most of 34 s | DEMOTE-TO-T1 (`yield.sh report/list/classify` over planted `record/` and `gate.log`; no git) |
| 24 yield surfaced | `test-yield.sh::watchtower --show carries yield`, `::missing record reaches Ops as ?`; `::SP_YIELD_* written by collector and read by pane`, `::every yield field defaults to ?` | T2 + source-grep | MOVE the watchtower rows to `test-watchtower.sh`. SOURCE-GREP: replace the two grep rows with a behaviour test in cockpit-observability (run the collector against a planted yield record and render the pane). |
| 25 metering | `test-gate-tree.sh::serialisation wait metered`; `test-gate-verdict.sh::same tree not judged twice` (gate log `rc=0 cached`) | T2 | KEEP. The double-meter and no-meter-on-preflight rows are a GAP. |

---

## 4. Duplicate clusters

1. **"Red on both trials is BASE_FAIL base-red with suite="**. Asserted by `test-gate-base-evidence.sh::base-red verdict`, `test-gate-base-selection.sh::red on both charged to base`, `test-yield.sh::the gate refuses it as BASE_FAIL` + `::verdict line names the suite` + `::a red naming no suite` (and consumed again by `test-landing-base-fail.sh`, 34 s, in another area). Five full gate runs over their own git fixtures prove one classification rule.
   **Keep:** `test-gate-base-evidence.sh`, with the rule as T1 rows plus one T2 wiring run. test-yield keeps only "BASE_FAIL is *recorded* as GATE_FAULT", which is its own concern.

2. **"A passing gate run on a fresh fixture" as a positive control**. The same control (`rc 0`, `VERDICT=PASS`) appears in `test-gate-preflight.sh::valid branch passes`, `test-gate-host-cores.sh::gate passes`, `test-gate-fixture-diag.sh::diagnostic absent from a passing gate` and `test-gate-verdict.sh::positive control first run`. Each builds a bare remote plus a clone and runs gate.sh once to prove it can pass.
   **Keep:** one shared fixture builder, `spira/testlib/gate-fixture.sh` (built by sp-ajxg3, adopted so far only by `test-gate-preflight.sh`) plus the controls in `test-gate-verdict.sh`. The remaining ~11 files still building their own remote-plus-clone are follow-up work.

3. **"Host cores are not cgroup nproc"**. `test-governor-host-cores.sh::A1/B2` and `test-gate-host-cores.sh::SPIRA_GATE_HOST_CORES equals real host count` both stub `nproc`→1 and compare against `getconf`, with identical controls.
   **Keep:** a T1 `host_cores` row. The gate side becomes one variable in the UC-10 env-contract assertion.

4. **"Lock timeout gives NO_VERDICT and preserves the holder"**. Within one file, `test-gate-tree.sh` case 3 and case 5 each run `rungate spira/sp-t1 SPIRA_GATE_LOCK_WAIT=1` against a held lock (lines 139 and 187). Outside the area, `test-landing-gate-wait.sh` and `test-auron.sh` re-derive the same gate outcome.
   **Keep:** one run in test-gate-tree asserting rc 75, `lock-timeout`, `not a fault` and holder-tree survival.

5. **"Per-branch trees isolated under concurrency; cache serves repeat callers"**. `test-gate-tree.sh` (30 s), `test-soak.sh` (15 s) and `test-gate-verdict.sh::same tree not judged twice`.
   **Keep:** test-gate-tree for isolation and serialisation, test-gate-verdict for the cache. Move test-soak off the per-push lane.

6. **"covers: selection"**. `test-gate-touched.sh`, the selection half of `test-reopen-queue-eject.sh`, `test-certify-suites-off.sh` (ejected with suites=off) and `test-select.sh` (19 s, test-infrastructure) all drive `gate-touched.sh`/`select.sh` over tiny git repos.
   **Keep:** one T1 table in `test-gate-touched.sh` (file list → suite list, ejected CSV, dedupe, missing). The selector internals stay with the test-infrastructure area's `test-select.sh`.

7. **Internal duplicates**:
   - `test-gate-fixture-diag.sh` runs the same red command twice (the "positive control" and case 1).
   - `test-gate-touched.sh` calls `sel()` five times for Part A where one call would do.

---

## 5. Unit-extractable logic

The single blocking seam is that **`gate.sh` is not sourceable**: all of its logic runs at top level after `BR="${1:?}"`. Two fixes would each work. One is a `return`-if-sourced main guard. The other is to move the functions below into `spira/gate-lib.sh`, which gate.sh sources. With either in place, every row below becomes a sub-second table test.

| Logic | Where | Tested today only via | Seam for T1 |
|---|---|---|---|
| Base attribution (branch-red / base-red / base-timeout / base-untestable / branch-only suites) | gate.sh L701–760, inline after the base trial | 6+ full gate runs with two git trials each (`test-gate-base-evidence`, `test-yield`) | Extract `attribute <branch_rc> <branch_out_file> <base_ran> <base_rc> <base_out_file>` → `status reason suite`. `red_suites()` and `timed_out_suites()` are already functions of text. |
| `verdict()` shape + no-evidence downgrade | gate.sh L45–87 | never isolated | Source with `gate_meter`/`yield_note` undefined (they are guarded by `command -v`) and `HELD_LOCK=0`, then call it in a subshell and capture stderr and the exit code. |
| Cache key | gate.sh `gate_key()` L365–377 (hashes `$0`, `$EXCLUDE`, `$SKEW`, tree, files, cmd, `SPIRA_GATE_SUITES`) | ~10 gate runs in `test-gate-verdict.sh` | Pass tree-hash, files-hash, cmd and harness paths as arguments instead of reading `$0` and git, then assert key inequality per changed input. |
| Cache freshness | gate.sh L397–410 inline (`sed`+`eval` of `when/by/at`, TTL digits check) | backdated `at=` + full gate runs | Extract `cache_fresh <entry> <ttl> <now>`, and test rows for no `at=`, non-numeric TTL, expired, fresh, and `eval` of a hostile `when=` value (see §6). |
| Tree key | gate.sh `TREE_KEY=` L428 (`tr '/' '-' \| tr -c …`) | `test-gate-tree.sh` re-implements it as `tree_key()`/`branch_key()` | Extract `gate_tree_key <branch>` and have the test call it instead of copying it. |
| covers:/ejected selection | `gate-touched.sh` with `SPIRA_GATE_FILES` | a git repo per file | This seam already exists: write suites into a temp dir and pass `SPIRA_GATE_FILES`, with no `git init`. |
| Yield classify, window, cost, recorder liveness | `yield.sh cmd_report/cost_report/cmd_classify/rec_key` | ~5 real gate runs building record state cumulatively | Already file-in/KEY=VAL-out. Plant `record/*` and `gate.log`. A `SPIRA_YIELD_NOW` clock seam would remove the "plant at now-3×WINDOW" arithmetic. |
| `host_cores` | `lib.sh` (~4.7 k lines, sourced whole) | whole governor run, whole gate run | Move `host_cores` into a small `lib-host.sh` that lib.sh sources, so a T1 test avoids sourcing all of lib.sh. |
| Lock classification | `gate-locks.sh _pid_alive/_pgid_live_member/_in_proc_locks_inode` | real background flock holders | Keep at T2. The FREE/HELD/STALE decision could be a pure function of `(flock_free, pid_alive, pgid_live)` with `/proc` injected, but the /proc coupling is the thing under test, so the gain is small. |

---

## 6. Gaps

Every `verdict` reason in gate.sh was grepped across all `test-*.sh`. Nothing asserts these:

1. **`no-repo-map-file`** (gate.sh L94–103). An unreadable `SPIRA_REPO_MAP` is NO_VERDICT. The comment records that this used to PASS every branch. No test covers it, so the most dangerous fail-open regression in the file is unguarded.
2. **`no-evidence:` downgrade** (L48–52, sp-io5j backstop). FAIL with an empty message becomes NO_VERDICT. Untested. The related rule "a gate command that ran and printed nothing" (comment L43–44) is also untested.
3. **Universal layer: `syntax`, `beads-data`, `missing-exclude`, `missing-skew`, `foreign-harness`, `skew-init-fault`** (L180–260). None is exercised through gate.sh. `test-skew-foreign.sh` proves skew exits 3 on a conf failure, but nothing proves gate.sh maps 3 to NO_VERDICT rather than FAIL (class `sp-gate-conf-fail-as-foreign-harness`). `beads-data` is law-beads-is-never-public, and its enforcement point has no test.
4. **`syntax-only` PASS** for an empty gate column (L266–268). No test.
5. **`harness-fault`**: a branch trial exiting 75 → NO_VERDICT (L681). `test-gate-workflow.sh` and `test-verdict.sh` test the consumers, but the gate side is untested.
6. **`base-untestable`** (L752). It is reached by base exit 75, base killed at 124, or base checkout failure. `test-gate-base-evidence.sh::base NV` asserts only `NO_VERDICT`, not the reason slug, and the 124 and checkout-failure paths are never driven.
7. **`tree-unidentified`, `no-flock`, `no-lockfile`** (L448–449, L524). No gate-level test. `flock` absence makes the suites *skip* (exit 77) instead of asserting the refusal.
8. **`SPIRA_GATE_SUITES` in the cache key** (L377). A fences-only certification pass must not be able to serve a cached full-suite PASS, or the reverse. No test varies it.
9. **Non-numeric `SPIRA_VERDICT_TTL`** is treated as 0 (L398). Untested.
10. **`eval` of cached `when/by/at` values** (L403). The entry file is parsed with `sed` into an `eval`, and a `when=` containing `"$(…)"` would execute. There is no test and no hardening. This is a correctness and safety gap for a T1 `cache_fresh` row.
11. ~~**gate-locks STALE positive**.~~ CLOSED (sp-fm2wn): `test-gate-locks.sh` case 5 now shows STALE is reported for a dead PID and dead PGID holding a lock a different, unrelated process still holds.
12. **Double metering / trap path** (L68–71). No test kills a gate mid-run (`set -e` death or signal) and asserts exactly one `gate.log` row. No test asserts that preflight refusals write no meter row.
13. **Output bound**. Base output is `tail -c 8000` and branch output `tail -c 4000` (L746–748). `test-gate-fixture-diag.sh` proves there is no `tail -20`, but a diagnostic more than 8 KB before the end is still cut. Nothing states whether that is intended.
14. **Yield bookkeeping cannot change the verdict** (L82–86, L126–138). No test makes `yield.sh` fail or hang and asserts that the exit code is unchanged. A *hang* would change it, because `yield_note` runs synchronously without a timeout.
15. **`test-reopen-queue-eject.sh` sp-px6ng section is vacuous**. The regression it names (the `.ejected` sidecar surviving a RED overwrite, read by the gate) is unguarded until a writer→reader behaviour test exists.
16. ~~**UC-16/17/18/19 have no covering suite at all.**~~ CLOSED (sp-fm2wn): `test-gate-tree.sh` and `test-gate-locks.sh` are rebuilt with the marker-based rewrite §4 point 3 called for, and UC-16/17/18/19 are tagged on them.

---

## 7. Cost

Current per-push suite-seconds (sum of ci_secs, primary files):

```
base-evidence 4 + base-selection 2 + fixture-diag 3 + gate-host-cores 3 + gate-locks 5
+ missing-cmd 6 + preflight 3 + sweep 6 + touched 1 + gate-tree 30 + gate-verdict 7
+ governor-host-cores 4 + reopen-queue-eject 2 + soak 15 + yield 34
= 125 s
```

Projected after the verdicts. Estimates assume one shared git fixture per file, with T1 rows costing under 1 s per file.

| After | From | Est. s | Basis |
|---|---|---|---|
| `test-gate-base-evidence.sh` (T2 wiring: 2 gate runs incl. env contract + early diagnostic) | base-evidence 4 + base-selection 2 + fixture-diag 3 + gate-host-cores 3 = 12 | 3 | ~11 gate runs → 2 |
| `test-gate-unit.sh` (new T1: attribution, verdict(), gate_key, cache_fresh, tree_key, host_cores, governor cores) | rows lifted from the above + gate-verdict + governor-host-cores 4 | 2 | pure functions, one `source` |
| `test-gate-preflight.sh` (merged) | preflight 3 + missing-cmd 6 = 9 | 4 | one fixture, 4 runs |
| `test-gate-verdict.sh` (T2 reuse loop only) | 7 | 4 | ~17 runs → ~8 |
| `test-gate-tree.sh` (marker-based, GATE_SECS=1, one timeout run) | 30 | 9 (measured, sp-fm2wn) | 4×3 s sleeps → 4×1 s; the two lock-timeout runs merged into one |
| `test-gate-locks.sh` (adds the STALE row) | 5 | 12 (measured, sp-fm2wn) | one more scenario (dead PID + dead PGID) added over the unchanged four |
| `test-gate-sweep.sh` | 6 | 6 | unchanged |
| `test-gate-touched.sh` (T1 table + 1 diff row, absorbs eject selection) | touched 1 + reopen-queue-eject 2 = 3 | 1 | |
| `test-soak.sh` | 15 | 0 per push (15 nightly) | moved off lane |
| `test-yield.sh` → T2 wiring (2 gate runs) + T1 report file | 34 | 7 (6 + 1) | ~5 gate runs → 2; watchtower rows move to test-watchtower (~+2 s there) |

```
3 + 2 + 4 + 4 + 9 + 12 + 6 + 1 + 0 + 7 = 48 s per push   (vs 125 s; −77 s, −62 %; gate-tree/
gate-locks are measured post-rewrite, sp-fm2wn — the rest of this row is still projected)
off-lane: soak 15 s nightly/batch; +~2 s moved into test-watchtower.sh (another area)
```

Closing the §6 gaps adds roughly 10 T1 rows (under 1 s) and about 4 T2 gate runs in the merged preflight file (3–4 s). The projection including gap closure is **~44 s**.

Reporting note for this area: all 15 files hand-roll `ok()/bad()/want()`, and `test-gate-locks.sh` even prints `%d ok, %d fail` where the others print `%d passed, %d failed`. The merged files should adopt whatever shared assertion and report helper (TAP or JUnit lines) the test-infrastructure area settles on, so that gate-area runs become comparable.

