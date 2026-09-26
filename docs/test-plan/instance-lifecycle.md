# Test plan — Install, release and world control (`instance-lifecycle`)

Part of [[test-plan-2026-09-23]], section 5. Area id `instance-lifecycle`; use-case ids are `UC-instance-lifecycle-NN`.

Scope: getting a Spira instance onto a box and switching it on and off. That covers build and release tarballs, deploy, activate and rollback, root `install.sh` phases and `systemd/install.sh` unit and timer rendering, the owned-artifact inventory (`owned.sh`), drift and skew detection (`skew.sh`, `install.sh --diff`), `uninstall.sh`, and `world.sh stop|start|drain|resume|status`. It also covers `ctrl.sh` suspensions, because both `install.sh` and `world.sh` honour them.

Inputs: 62 primary files plus 1 secondary (`test-pr-notify.sh`, unit-render section only), from taxonomy.json and map/*.jsonl. Costs come from signals.tsv, main-push run 35947142904. Four primary files have no timing because they did not run there (NA): `test-cadence.sh`, `test-cadence-tool.sh`, `test-install-rehearsal.sh`, `test-host-unit-names.sh`.

**Misfiled in this area:** 5 files belong elsewhere, and this plan only prices them.
- `test-capacity-probe.sh` belongs to dispatch.
- `test-session-hook.sh` belongs to the operator channel or hooks.
- `test-canary.sh` belongs to test infrastructure or acceptance.
- `test-review.sh` belongs to release review.
- `test-boundary.sh` belongs to publishing lint.

Together they cost 58 s. They are kept in the arithmetic in section 7, so the total reconciles with signals.tsv.

---

## 1. Intent (the de facto spec)

A Spira instance is built into an immutable, timestamped release tarball, and the tarball carries every binary its units execute. Activating a release unpacks it read-only beside earlier releases and atomically swaps `current`. Activation then restarts only non-aeon units, and it refuses while aeons are live unless forced. `deploy.sh` wraps activation in resolve, drain, health check and automatic rollback.

The installer renders one instance-qualified unit set from templates. It refuses early, before writing anything, on any condition that would damage another instance or live work: a foreign harness, live aeons, a landing in flight, a stale landref, a path collision, or a non-executable ExecStart. It is idempotent: it changes, restarts and daemon-reloads only what differs. It respects operator state (masked, disabled, ctrl-suspended, world halted).

Drift between installed units and templates, and between the activated release and the newest tag, is reported with a 0/1/3 exit contract. Exit 3 means "cannot check" and is never read as a pass. `uninstall.sh` removes exactly what `owned.sh` says the instance owns and reports strays without deleting them. `world.sh` stops, drains, resumes and starts the loop honestly: it never claims STOPPED when a stop failed, never touches cockpit or Loom, and never starts what the operator suspended.

---

## 2. Use cases

Dimension keys: COR correctness, FC fail-closed, OBS observability, IDEM idempotency, CONC concurrency, REC recovery, CFG config-compat, CON contract, PERF performance, TI test-integrity.

Where they run: **cert** = certification (every commit, local and gate), **batch** = batch CI, **main** = main-push CI, **accept** = acceptance.yml on a fresh VM, release only.

### Build and release artefact
| ID | Requirement | Dims | Tier / where |
|---|---|---|---|
| UC-instance-lifecycle-01 | `build.sh` builds loom, panel, broker, czar-pass and spira-supervise, in order, to the `SPIRA_*_BIN` paths. With no cargo it exits 0 and names the lost features. `--skip-build` never invokes cargo. | COR, FC | T1 / cert |
| UC-instance-lifecycle-02 | `build-tarball.sh` produces `spira-YYYYMMDDTHHMMSSZ.tar.gz` from a clean checkout, unpacking to a same-named dir. MANIFEST records the source SHA, and `--name` pins stem and timestamp. | COR, CON | T2 / cert |
| UC-instance-lifecycle-03 | The tarball ships every `@*_BIN@` binary that a non-optional unit's ExecStart references, executable. The build refuses without `--supervise-bin`, and no `sp-*` or `*.fixed` scratch files ship. | FC, CON | T0 (token scan) + T2 (refusal) / cert |
| UC-instance-lifecycle-04 | `build-tarball.sh verify` accepts a MANIFEST whose commit exists in the source repo and refuses one that does not. | FC | T2 / cert |
| UC-instance-lifecycle-05 | `build-bd.sh --from-release` derives `beads_<ver>_<os>_<arch>.tar.gz` and refuses a checksum mismatch before unpacking. | FC, COR | T2 / cert |
| UC-instance-lifecycle-06 | `release.sh cut` creates an annotated tag listing the bead ids landed since the previous tag, on the repo-map base ref (not hardcoded `main`). A no-op cut creates no tag, and `show` reads the tag back. | COR, CFG | T2 / cert |
| UC-instance-lifecycle-07 | `review.sh` records a ship/block verdict per release unit, files a finding bead on block, and never duplicates it. *(misfiled; price only)* | COR, IDEM | T2 / batch |

### Activate, deploy, rollback
| ID | Requirement | Dims | Tier / where |
|---|---|---|---|
| UC-instance-lifecycle-08 | `activate.sh` unpacks to a versioned, read-only release dir, atomically repoints `current` and keeps the previous release. `--dry-run` changes nothing. | COR, REC | T2 / cert |
| UC-instance-lifecycle-09 | `activate.sh` prunes releases beyond `SPIRA_RELEASES_KEEP`, oldest first, and never prunes the `current` target. | REC, FC | T1 (prune selection) / cert |
| UC-instance-lifecycle-10 | `activate.sh` refuses while this instance's aeons are live (naming them and `SPIRA_ACTIVATE_FORCE`), leaving `current` untouched. After the swap it daemon-reloads and restarts only non-aeon units. | FC, CONC | T2 / cert |
| UC-instance-lifecycle-11 | Rollback is re-activation of an already-unpacked release (unpack skipped), after which units execute the older code. | REC | T2 / cert |
| UC-instance-lifecycle-12 | `deploy.sh` resolves `latest`, refuses drafts, the already-current release, a DB-migration mismatch and a failed pre-deploy doctor, all before any disruptive step. | FC, COR | T2 / cert |
| UC-instance-lifecycle-13 | `deploy.sh` drains first: a drain refusal blocks the deploy, and `--force` slays with `--keep-work --reopen`. It writes `SPIRA_PROD` into spira.conf before activate restarts services, then re-renders units with `SPIRA_PROD=releases/current/spira`. | COR, CONC | T2 / cert |
| UC-instance-lifecycle-14 | A post-activation health failure (doctor or skew) rolls back to the prior release and restores spira.conf. On a first deploy it removes `current` and re-renders on the checkout. It never re-enables ctrl-suspended units. | REC, FC | T2 / cert |
| UC-instance-lifecycle-15 | A release installed from its tarball runs from a read-only tree: sentinel dispatches and loom serves without cargo, and `conf.sh` resolves `SPIRA_LOOM_BIN` to `bin/loom`. | CFG, COR | T1 (conf resolution) + T3 (ro sentinel) main + T4 (loom live) accept |

### Install (root `install.sh` and `systemd/install.sh`)
| ID | Requirement | Dims | Tier / where |
|---|---|---|---|
| UC-instance-lifecycle-16 | Root install refuses with exit 5 on each conflict (foreign harness owns the unit names, live `aeon.sh` for this home, gate lock held, instance mismatch, Dolt port serving another data_dir), naming the remedy and `SPIRA_INSTALL_CONFLICT_CONSIDERED`. A clean box passes. | FC, CONC | T1 per predicate / cert |
| UC-instance-lifecycle-17 | Root install refuses a git-checkout `SPIRA_PROD` (exit 2, override named) and a `CONFIGURE_PROD` without `conf.sh`. On an empty `SPIRA_RELEASES` it bootstraps `releases/bootstrap` with `current -> bootstrap`. Otherwise it refuses with exit 2 and names `activate.sh`. | FC, CFG | T1 / cert |
| UC-instance-lifecycle-18 | Root install exits 3 when installed but `ready.sh` fails. `--dry-run` prints each phase and mutates nothing. | CON, OBS | T2 / cert |
| UC-instance-lifecycle-19 | The unit installer refuses a checkout on the wrong branch or behind the landref, before writing any unit. It skips the check for artifact deploys and exact-tag detached HEADs, and `SPIRA_INSTALL_FORCE` overrides it. | FC, CFG | T1 (decision) + T2 (git) / cert |
| UC-instance-lifecycle-20 | The unit installer refuses when another instance's config shares any of `SPIRA_RUN`, `SPIRA_DB`, `SPIRA_PROD`, `SPIRA_DOLT_DATA` or `SPIRA_TESTDB_PORT`, naming the key, the other instance and its file. It writes no unit on refusal. | FC, CONC | T1 / cert |
| UC-instance-lifecycle-21 | The unit installer refuses while this instance's aeons are live (before daemon-reload), and refuses when any ExecStart target is missing or lacks +x. | FC | T1 / cert |
| UC-instance-lifecycle-22 | Per-unit apply decision, as a table: changed → write, restart, count; unchanged → nothing, no daemon-reload, "unchanged"; masked → left, reported masked; operator-disabled → not re-enabled; ctrl-suspended → not enabled; world halted → `enable` without `--now`; `spira-aeon-*` → never restarted or enabled. | IDEM, COR | T1 / cert |
| UC-instance-lifecycle-23 | A changed timer whose oneshot is mid-pass is applied only after the oneshot drains. After install, any enabled-but-inactive unit makes the installer exit non-zero. | CONC, OBS | T2 / cert |
| UC-instance-lifecycle-24 | Rendering produces instance-suffixed `spira-*-<inst>` units whose timers target `Unit=spira-*-<inst>.service`. Shared units (cockpit-ensure, concierge, beads-push, dolt-beads) keep plain names. Watchers render as `spira-watch-<name>-<inst>.service`, never `@`. No `@PLACEHOLDER@` survives. | COR, CFG | T1 over one shared render / cert |
| UC-instance-lifecycle-25 | Optional units are gated by what is present: loom and broker by their binary, mail-deliver by `inotifywait`, suites by `SPIRA_SELF_TEST` (derived from `.git` presence, overridable). A missing prerequisite marks the unit OPTIONAL, not installed and not enabled, with a note. | CFG, FC | T1 / cert |
| UC-instance-lifecycle-26 | The renderer refuses `@DOLT@` templates when dolt is absent and never emits an empty ExecStart. `SPIRA_PROD=` (explicitly empty) falls back to `SPIRA_HOME`, and cockpit-ensure derives from `dirname(SPIRA_PROD)`. | FC, CFG | T1 / cert |
| UC-instance-lifecycle-27 | Prune: a `spira-*-<inst>` unit absent from UNITS is disabled, stopped and removed. An orphaned watcher (row left the manifest) is disabled, and a manifest watcher is never pruned. Neither loop prunes the other's units. | REC, COR | T2 (recording systemctl) / cert |
| UC-instance-lifecycle-28 | Legacy migration disables un-suffixed and `spira-watch@` units before their per-instance replacements are enabled. It never touches shared units, is silent on a clean box, and `--no-migrate-watchers` spares watchers. | REC, CFG | T2 / cert (retire when no host is un-suffixed) |
| UC-instance-lifecycle-29 | A non-prod install seeds `SPIRA_INSTANCE=<inst>` into `dirname(SPIRA_PROD)/spira.conf` once, preserving existing lines. A prod install does not write the file. | IDEM, CFG | T1 / cert |
| UC-instance-lifecycle-30 | `unit-ensure.sh` reinstalls missing or changed units (daemon-reload only then), is a no-op otherwise, and reports MISSING-TARGET without enabling. | IDEM, FC | T2 / cert |
| UC-instance-lifecycle-31 | Unit-file invariants: every timer template is in UNITS (or OPTIONAL) and in ENABLE, and fires periodically. Restarting services have a reachable start limiter in `[Unit]`. WatchdogSec appears only on non-shell ExecStart. The verdict service has `TimeoutStartSec>=3600` and no CPUQuota. | CON | T0 / cert |
| UC-instance-lifecycle-32 | `cadence.sh` changes a timer's cadence via a revertible drop-in and never leaves it without a next elapse. It refuses unknown units, and verify refuses an empty sweep. | FC, REC | T1 (drop-in text) cert + T4 (real systemd disarm) accept |

### Inventory, drift, skew, uninstall
| ID | Requirement | Dims | Tier / where |
|---|---|---|---|
| UC-instance-lifecycle-33 | `owned.sh list` emits every artefact kind in 5-field rows, with the unit rows equal to `install.sh --render` for that instance. `check` reports absent, present or drifted per artefact. | CON, OBS | T2 / cert |
| UC-instance-lifecycle-34 | `install.sh --diff` and `skew.sh units` return 0 when clean, 1 with DIFFERS or MISSING, and 3 when the installer is missing. | CON, OBS | T2 / cert |
| UC-instance-lifecycle-35 | `skew.sh check` returns 1 for NOT-LATEST or MANIFEST-MISMATCH, 0 "in effect", and 3 for missing releases, `current` or MANIFEST. In artifact mode it reads tags via gh. | FC, CON | T2 / cert |
| UC-instance-lifecycle-36 | `skew.sh foreign` refuses branches that change a vendored harness copy outside the harness repo (exit 1). It exempts the harness's own repo and dev source, and returns 3 on conf failure. | FC | T2 / cert |
| UC-instance-lifecycle-37 | `skew.sh gap` counts commits behind the remote. `refresh` advances even under a live lease, swaps atomically (new inode) and stashes dirty files. | CONC, REC | T2 / cert |
| UC-instance-lifecycle-38 | `uninstall.sh` stops, disables and removes owned units, disables linger, and removes harness symlinks and session hooks. It is idempotent, refuses ambiguity when several instances are installed, reports strays (never deletes them), and honours `--purge` and `--dry-run`. A partial install uninstalls with exit 0. | IDEM, FC, REC | T2 / cert |
| UC-instance-lifecycle-39 | `uninstall.sh --purge-database` removes the DB only when the operator types back the bead count, and refuses on mismatch. | FC | T2 / cert |
| UC-instance-lifecycle-40 | A fresh-box install→ready→uninstall round trip on real systemd leaves the unit directory byte-identical to before. | REC, CON | T4 / accept |

### World control
| ID | Requirement | Dims | Tier / where |
|---|---|---|---|
| UC-instance-lifecycle-41 | `world.sh stop` stops active work units (resolved instance-qualified name, with plain fallback). It never stops `spira-cockpit*`, `spira-loom*` or landing, and spares CI watchers unless `--hard`. `--hard` stops watchers in both name forms. On any failed stop it exits non-zero without printing STOPPED. | COR, OBS, FC | T1 (service filter, name resolution) + T2 / cert |
| UC-instance-lifecycle-42 | `world.sh start` starts timers except those ctrl-suspended (with the reason printed) or systemd-disabled. It revives non-oneshot watchers stopped by `--hard` and leaves active ones alone. | COR, IDEM | T1 decision table + T2 / cert |
| UC-instance-lifecycle-43 | `world.sh drain` touches no unit and writes a stamp with a resume hint. `--timeout` exits 1 with NOT DRAINED / REMAIN GATED. `--deadline` slays with `--keep-work --reopen`. `resume` clears the stamp. Every aeon door carries both the drain and halt gates. | CONC, FC | T2 / cert; door coverage T0 |
| UC-instance-lifecycle-44 | `world.sh status` reports landing, live gate and landing workers from `/proc`, DRAINING, active CI watchers, and `svc: <result>` for a failed last run. It says "unknown" (not "inactive") for an unresolvable unit. | OBS | T1 row formatting + T2 /proc / cert |
| UC-instance-lifecycle-45 | `ctrl.sh`: a suspension needs a reason and an owner. check, list and resume work, and divergence reports suspended-but-running and masked-but-undeclared units. | FC, OBS | T1 / cert |

### Acceptance runner (release gate)
| ID | Requirement | Dims | Tier / where |
|---|---|---|---|
| UC-instance-lifecycle-46 | `acceptance-ci.sh` builds a scratch repo on `main` with a git identity and a repo-map, puts `~/.local/bin` on PATH, passes `XDG_RUNTIME_DIR`, and propagates `acceptance-run.sh`'s exit code. | CON | T2 / cert |
| UC-instance-lifecycle-47 | `acceptance-run.sh` runs install with `SPIRA_OPERATED=0`, runs the clone's own `ready.sh` (not masked by `\|\| true`), extracts bead ids robustly (GH#2950), proves landing by ancestry, and records a git note. It emits a single PASS/FAIL line. | CON, TI | T1 (extracted functions) cert + T4 accept |

Machine-readable declarations (read by `spira/plan-lint.sh`), added as each UC gets a covering
suite rather than all at once:

* `UC-instance-lifecycle-31` [T0] — every timer template is in UNITS (or OPTIONAL) and in ENABLE, and fires periodically; restarting services have a reachable start limiter in `[Unit]`; WatchdogSec appears only on non-shell ExecStart; the verdict service has `TimeoutStartSec>=3600` and no CPUQuota
* `UC-instance-lifecycle-32` [T1] — cadence.sh changes a timer's cadence via a revertible drop-in and never leaves it without a next elapse; it refuses unknown units, and verify refuses an empty sweep (the T4 real-systemd disarm case is acceptance-only, tracked separately)

---

## 3. Coverage map

The cost column is ci_secs from the main-push run; NA means not run there.

| UC | Existing tests (file::case) | Level / cost now | Verdict |
|---|---|---|---|
| 01 | test-build.sh::absent cargo, normal run, second run, --skip-build | T1-ish, 4 | KEEP. Make the idempotence case assert on the cargo call log, not on the names printed. |
| 02, 04 | test-build-tarball.sh::build/name/MANIFEST/verify/wrong commit/--name | T2, 4 | KEEP |
| 03 | test-tarball-bins.sh::1–5; test-build-tarball.sh::bins present | T2, 1 + dup | test-tarball-bins: MERGE-INTO test-build-tarball.sh (the `--supervise-bin` refusal) and MERGE-INTO the unit-lint T0 (the ExecStart token scan, deriving OPTIONAL from `units.sh` instead of a hand-copied list). Delete release.yml grep (belongs in test-release-workflow). |
| 05 | test-build-bd-release.sh::all | T2, 6 | KEEP |
| 06 | test-release.sh::1–7 | T2, 6 | KEEP. Drop case 8 (a trivially-true self-check). |
| 07 | test-review.sh | T2 (real bd), 8 | KEEP (misfiled; owner: release review) |
| 08 | test-activate.sh::basic, dry-run; test-rollback.sh::activate-A/B; test-loop-readonly.sh::activate/ro; test-artifact-install.sh::activate | T2, 2 (+4 +12 +152 dup legs) | KEEP test-activate.sh. Delete the activate legs elsewhere. |
| 09 | test-activate.sh::prune | T2, 2 | KEEP + fix. The test sets `current` to the oldest release, but activation repoints it before the prune runs, so "never prunes current" is not exercised (Gap G4). |
| 10 | test-activate.sh::aeon-guard, force, restart | T2, 2 | KEEP. Add the reload-before-restart ordering assertion. |
| 11 | test-rollback.sh::rollback | T2, 4 | MERGE-INTO test-activate.sh (only the rollback step is unique). |
| 12–14 | test-deploy.sh::P1–P23 | T2, 14 | KEEP. Factor the 6 copy-pasted `env -i` blocks into one runner and collapse the ~8 redundant healthy-deploy runs. Fix the tautological controls P4, P9 and P12. |
| 15 | test-artifact-install.sh::conf resolves SPIRA_LOOM_BIN, cargo build, /api/beads 200, ready.sh; test-loop-readonly.sh::sentinel from ro | T4-in-CI, 152; T3, 12 | test-artifact-install: DEMOTE-TO-T1 the conf-resolution case (the actual defect). Move the cargo build and live loom to acceptance.yml, which already builds loom once, and use the real `build-tarball.sh` there. test-loop-readonly: KEEP as T3 on main; drop its activate leg and use a `chmod a-w` copy. |
| 16 | test-install-conflicts.sh::conflict-1…5, override, clean | T2, 6 | KEEP now. Next step DEMOTE-TO-T1 per predicate. Fix fixed port 19877 (collides with test-install-dolt-port-wait) and the false "fail-first" claim. |
| 17 | test-install-prod-checkout.sh::all; test-install-bootstrap-release.sh::refuse, bootstrap | T2, 10 + 5 | DEMOTE-TO-T1 (early path predicates). The 12-stub root-install fixture is shared with install-conflicts, so extract a `mk_install_fixture` helper. |
| 18 | *(none behavioural)*; test-release-acceptance greps | — | GAP G3 |
| 19 | test-install-landref.sh::all | T2, 4 | KEEP (exemplar). Add rc assertions to the FORCE nowant cases. |
| 20 | test-install-paths.sh::collision/distinct/no-conf/same-instance | T2 + real systemd, 39 | DEMOTE-TO-T1 on `_check_path_collisions`, with all five keys (only `SPIRA_RUN` is tested today). Fix the vacuous "no unit written" check. |
| 21 | test-install-aeons.sh::aeon guard, force; test-install-exec.sh::clean/missing/noexec/tarball | T2, 57 + 29 | install-exec: DEMOTE-TO-T1 (the executability check is a path predicate). Use a 2-file fake release instead of tar-copying the repo, and derive NOEXEC from templates. install-aeons: KEEP the guard cases only. |
| 22 | test-install-idempotent.sh::positive-ctrl, idempotent, masked, disabled; test-install-aeons.sh::no-op, selective, aeon safety; test-install-halt.sh::running/halted; test-ctrl.sh::install no suspension/suspended/after resume; test-install-unit-ensure.sh::no-op | T2 ×5, 42+57+17+27+12 | MERGE into one new **T1 table** `test-install-decide.sh`, over an extracted `_unit_action(state)` in systemd/install.sh. install-idempotent, install-halt and the ctrl install tier become MERGE-INTO it. Fix the `$?`-after-cat bug in install-idempotent before porting; its exit-0 assertions are vacuous today. |
| 23 | test-install-aeons.sh::drain, end-state | T2, (in 57) | KEEP in test-install-aeons.sh (trimmed to guard + drain + end-state, ~15 s). |
| 24 | test-install-instance.sh::naming, watcher install; test-install-unit-directive.sh::unit directive; test-owned.sh::unit rows match render; test-pr-notify.sh §9 | T2 + real systemd, 37 + 14 | install-instance naming: DEMOTE-TO-T1 over one shared render. install-unit-directive render half: MERGE-INTO the unit-lint over the shared render; it is needlessly container-gated today. Remove the unit-render section from test-pr-notify (secondary; it is covered generically). |
| 25 | test-install-loom-broker.sh::A–D; test-install-mail-deliver.sh::A/B; test-install-self-test.sh::A–C | T1, 2+3+5 | MERGE all three into one table `test-install-loom-broker.sh`, renamed to test-units-optional. Make mail-deliver skip (not fail) without inotifywait, and stub `.git` detection instead of depending on the harness being a checkout. |
| 26 | test-install-dolt.sh::renderer, doctor; test-install-exec.sh::conf empty, render fallback, cockpit path, heal log | T1, 7; T2 (in 29) | test-install-dolt: SOURCE-GREP. Awk-extracting the renderer from install.sh's heredoc couples the test to source layout. Move the renderer to `systemd/render.py` and call it directly. |
| 27 | test-install-unit-prune.sh::all; test-install-instance.sh::prune | T2 + real systemd, 28 + (37) | test-install-unit-prune: MERGE-INTO test-install-instance.sh as a prune section on a recording mock (no real user systemd). The watcher-skip negative relies on 4-space formatting, so replace it with a structured check. |
| 28 | test-install-migrate.sh::all; test-install-unit-directive.sh::migrate @-form | T2 + real systemd, 31 + (14) | KEEP test-install-migrate.sh on a recording mock. The `@`-form case goes MERGE-INTO it. Tag the file for retirement. |
| 29 | test-install-conf-seed.sh::all | T2, 30 | DEMOTE-TO-T1 (append-if-absent on a file). |
| 30 | test-install-unit-ensure.sh::all | T2, 12 | KEEP. MISSING-TARGET currently tests a reimplementation inside the test, so rewire it to `unit-ensure.sh`'s `_ue_execstart_ok` (Gap G10). |
| 31 | test-timer-templates.sh; test-unit-restart-limits.sh; test-cockpit-unit-notify.sh; test-verdict-timer.sh | T0, 10+1+2+2 | Consolidate as **one T0 unit-lint**, hosted by test-timer-templates.sh. restart-limits, cockpit-unit-notify and the unique parts of verdict-timer (Timeout/CPUQuota, driver) go MERGE-INTO it. Delete the live section that runs against an always-yes stub; that check belongs in doctor. Stop cockpit-unit-notify's controls testing copies of the parser. |
| 32 | test-cadence-tool.sh::all | T4-ish, NA (never runs in CI) | Split. DEMOTE-TO-T1 the drop-in text, span→OnCalendar mapping and refusals. Keep the real-systemd "never disarms" case (sp-dah) in acceptance. |
| 33 | test-owned.sh::all | T2, 19 | KEEP. Render once, reuse. |
| 34 | test-unit-drift.sh::all | T2, 20 | KEEP. Share the render→DEST fixture with uninstall and owned. |
| 35 | test-skew-check-release.sh::all | T2, 2 | KEEP (exemplar 0/1/3 contract) |
| 36 | test-skew-foreign.sh::all | T2, 3 | KEEP |
| 37 | test-skew-refresh.sh::all | T2, 2 | KEEP |
| 38 | test-uninstall.sh::all; test-install-rehearsal.sh::stray sweep | T2, 78; T4, NA | KEEP test-uninstall.sh, but share the render fixture and drop the git bare/clone used only for the landref bypass (use FORCE or artifact mode). Target ~20 s. |
| 39 | *(none)* | — | GAP G1 |
| 40 | test-install-rehearsal.sh::all; test-cadence.sh | T4, NA | test-install-rehearsal: KEEP, moved to acceptance.yml (it never runs today). test-cadence.sh: DELETE. Its formula check is covered by test-watchd-unit-name.sh (hermetic), and its live half by the rehearsal. |
| 41 | test-world.sh::stop*, hard halt, CI watchers; test-work-services-exclusion.sh::all; test-world-timer-names.sh::A–D | T2, 25+3+13 | DEMOTE-TO-T1: `work_services` filter + `_resolve_timer` as a table in test-world-timer-names.sh. work-services-exclusion goes MERGE-INTO it; its "positive control" re-runs a hand-copied regex. test-world.sh keeps stop-failure honesty and /proc counting. |
| 42 | test-world-start-honours-ctrl.sh::all; test-world-start-revives-watchers.sh::all (greps) | T2, 71; T0, 20 | DEMOTE-TO-T1 for the start skip decision. world-start-revives-watchers: SOURCE-GREP → rewrite as behavioural cases on the same stub, then MERGE-INTO world-start-honours-ctrl. |
| 43 | test-world.sh::drain*, resume, drain --timeout 0, gate covers every door; test-world-drain-deadline.sh::all | T2, 25 + 20 | test-world.sh's `drain --timeout 0` case is a duplicate: DELETE it from test-world.sh and keep it in drain-deadline. The door-coverage grep moves to T0. |
| 44 | test-world.sh::status*; test-world-timer-service-result.sh::all; test-unit-name.sh::A–D; test-world-drain.sh::all | T2, 25+17+1+58 | world-timer-service-result: MERGE-INTO the test-world-timer-names T1 table. test-world-drain.sh: DELETE (redundant; the subject is `cockpit/health.sh` `drain_banner`, owned by test-drain-banner.sh at 2 s). test-unit-name.sh: KEEP. |
| 45 | test-ctrl.sh::suspend…divergence | T2, 27 | KEEP the ctrl core as T1. DELETE its install tier (moved to UC-22). |
| 46 | test-acceptance-ci.sh::all | T2, 6 | KEEP. Drop the git positive control (it tests git itself). Move the YAML greps to workflow lint. Assert the git-identity side effect on `$HERE/..`. |
| 47 | test-acceptance-run.sh::all; test-release-acceptance.sh::1–24; test-acceptance-ready.sh::all | T0/T2, 43+2+6 | test-acceptance-ready: DELETE (tautological; it never runs the subject). test-release-acceptance: SOURCE-GREP (~70 greps pin local variable names; cases 19/22/24 run a copy). Keep only cases 1–3. test-acceptance-run: SOURCE-GREP + DEMOTE-TO-T1. Extract `_extract_bead_id` and `_phase_env` into a sourceable file acceptance-run.sh uses, and delete the real `bd init` block (43 s spent testing bd, not Spira). |
| — | test-host-unit-names.sh | NA (exit 77 in CI) | DELETE from the suite set; move to a doctor check on an installed host. |
| — | test-capacity-probe.sh 5, test-session-hook.sh 13, test-canary.sh 27, test-boundary.sh 5 | — | Misfiled. KEEP in their owning areas. Canary: bring the stage up once, not 5 times, and set `_REAL_DB` (Gap G9). Session-hook: move its systemd greps to T0. Boundary: move shipped-tree freshness to T0. |

---

## 4. Duplicate clusters

1. **Per-unit apply decision (changed / unchanged / masked / disabled / halted / suspended).**
   - Files: test-install-idempotent (42), test-install-aeons no-op + selective (≈30 of 57), test-install-halt (17), test-ctrl install tier (≈15 of 27), test-install-unit-ensure no-op (unit-ensure.sh's copy of the same decision).
   - Evidence: every one of them seeds DEST from `--render` with the same copy-pasted loop, then runs the full `systemd/install.sh` 2–5 times against a recording systemctl, and asserts on `daemon-reload` / `enable --now` / "unchanged".
   - **Keep:** a new T1 `test-install-decide.sh`, table-driven over an extracted `_unit_action`, plus one T2 smoke in test-install-aeons.sh.

2. **Basic activation (unpack, swap `current`, read-only, previous kept).**
   - Files: test-activate (2), test-rollback (4), test-loop-readonly (12), test-artifact-install (152).
   - Evidence: the r0/r1 reducers name this identical cluster, and test-rollback steps 1–2 duplicate test-activate::basic.
   - **Keep:** test-activate.sh (plus the rollback step merged in). The others keep only their unique leg (ro-sentinel dispatch; loom-from-release in acceptance).

3. **acceptance-run.sh structure.**
   - Files: test-acceptance-run, test-release-acceptance, test-acceptance-ready.
   - Evidence: all three grep for `SPIRA_OPERATED` per phase, the clone `ready.sh` path and rc capture (release-acceptance cases 16/18 ≡ acceptance-run). acceptance-ready runs a copy of the idiom.
   - **Keep:** none as they stand. Keep T1 tests of the extracted functions, plus the real acceptance run.

4. **Drain `--timeout` with a live aeon → NOT DRAINED / REMAIN GATED / exit 1.**
   - Files: test-world.sh, test-world-drain-deadline.sh case 3 (near-verbatim).
   - **Keep:** test-world-drain-deadline.sh.

5. **Drain banner.**
   - Files: test-world-drain.sh (58 s, 6 full health.sh frames), test-drain-banner.sh (2 s).
   - **Keep:** test-drain-banner.sh. DELETE test-world-drain.sh.

6. **ctrl suspension honoured on (re)enable or start.**
   - Files: test-ctrl install tier, test-world-start-honours-ctrl, test-install-halt (adjacent), test-deploy P20.
   - **Keep:** one predicate `(suspended?, enabled?)` shared by world.sh and install.sh, tested in test-install-decide (install) and the world-start T1 table (world). P20 stays in deploy because it tests deploy's rollback path.

7. **Optional-unit gating by prerequisite.**
   - Files: test-install-loom-broker, test-install-mail-deliver, test-install-self-test A/B.
   - Evidence: the same UNITS/OPTIONAL/ENABLE assertion shape.
   - **Keep:** one table in test-install-loom-broker.sh.

8. **Timer and unit-file lint.**
   - Files: test-timer-templates, test-verdict-timer (UNITS/_ENABLE_TMPL/periodic, identical awk), test-install-unit-directive render half, test-unit-restart-limits, test-cockpit-unit-notify, test-tarball-bins token scan.
   - **Keep:** one T0 lint hosted by test-timer-templates.sh.

9. **Tarball has `bin/spira-supervise` executable.**
   - Files: test-tarball-bins case 3, test-build-tarball (identical assertion text).
   - **Keep:** test-build-tarball.sh.

10. **Watcher unit-name formula.**
    - Files: test-cadence.sh (NA), test-host-unit-names.sh (NA), test-install-instance watcher cases, test-watchd-unit-name.sh (other area, hermetic).
    - **Keep:** test-watchd-unit-name.sh for the formula and test-install-instance for the installed name. Delete the two NA suites.

11. **Render→DEST fixture boilerplate** (not duplicated assertions, but duplicated cost).
    - Copied into test-uninstall, test-unit-drift, test-owned, test-install-{aeons,idempotent,halt,conf-seed,exec,instance,migrate,unit-prune,unit-directive}: roughly 12 suites × 1–6 `--render` runs.
    - **Keep:** one `lib-test-install.sh` helper that renders once per suite into a cached dir keyed on the templates' hash.

---

## 5. Unit-extractable logic

| Logic | Where it lives | Tested today only via | Seam for T1 |
|---|---|---|---|
| Per-unit apply decision (UC-22) | `systemd/install.sh` main loop (903 lines, not sourceable) | 5 suites, ~150 s of full installs | Extract `_unit_action <rendered> <installed> <is-enabled> <halted> <suspended>` → `write\|skip\|masked\|operator-disabled\|enable\|enable-now`. Add a `SPIRA_INSTALL_LIB=1` guard so the file can be sourced without running `main`. |
| Path-collision check | `systemd/install.sh:_check_path_collisions` | test-install-paths (39 s, real systemd) | Already a named function. Source it under the same guard and feed it a temp dir of `spira-*.conf` files. |
| Landref currency | `systemd/install.sh:_check_landref_current` | test-install-landref (4 s, 3 git repos) | Inject `git` via a `SPIRA_GIT` shim, or pass (branch, behind, tag) directly. Low priority; already cheap. |
| Conflict predicates 1–5 | root `install.sh` phase 0.5 (inline, lines 148–266) | test-install-conflicts (~8 full root installs, /proc, fixed port) | One function per conflict (`_conflict_foreign`, `_conflict_aeon <proc-root>`, `_conflict_lock`, `_conflict_instance`, `_conflict_dolt <port-probe>`), with injectable `/proc` root and probe command. |
| SPIRA_PROD git and bootstrap guards | root `install.sh` early phases | test-install-prod-checkout, test-install-bootstrap-release (12-stub fixture) | `_prod_guard <path>` and `_bootstrap_decision <prod> <releases>`. |
| Unit renderer | Python heredoc inside `systemd/install.sh`, awk-extracted by test-install-dolt | test-install-dolt; every `--render` | Move to `systemd/render.py` with named argparse flags (11 positional args break silently today). |
| Conf seeding (append-if-absent) | `systemd/install.sh` | test-install-conf-seed (30 s, 5 full installs) | `_seed_instance_conf <file> <inst>`. |
| ExecStart executability | `systemd/install.sh`, and separately `unit-ensure.sh:_ue_execstart_ok` | test-install-exec (29 s), test-install-unit-ensure (a copy) | One shared `_execstart_ok <unitfile>` in `units.sh`, used by both; T1 table. |
| World start skip decision | `world.sh start` loop calls `ctrl.sh check` then `ctrl.sh reason` per timer | test-world-start-honours-ctrl (71 s) | **Root cause of the 71 s:** `ctrl.sh` sources `conf.sh` and spawns `python3` on every call. world start makes 2 calls per timer (units.sh lists 44 timer mentions), over 4 runs, so hundreds of conf.sh + python start-ups. Fix: `ctrl.sh list --json` once, then decide in-process. That is also a production win on every `world.sh start`. Then T1 on `_start_action <timer> <suspended-set> <is-enabled>`. |
| work_services filter, timer-name resolution, status row | `world.sh` (`work_services`, the `_timer_seen` resolver, status printf) | test-work-services-exclusion, test-world-timer-names, test-world-timer-service-result (33 s together) | Add a `WORLD_LIB=1` source guard in world.sh, then pure functions over a unit list plus canned `systemctl show` output. |
| ctrl divergence | `ctrl.sh divergence` (python over ctrl JSON + systemctl output) | test-ctrl (27 s incl. install tier) | Already near-T1 with a fake systemctl. Just drop the install tier. |
| Activate prune selection | `activate.sh` lines 205–224 | test-activate::prune (does not exercise the protect-current case) | `_prune_candidates <releases-dir> <keep> <current-target>`. |
| Cadence drop-in text | `cadence.sh` | test-cadence-tool (NA, podman + real systemd) | `_dropin_for <unit> <span> <reason>` → text. Only the "armed after apply" check needs systemd. |
| acceptance-run bead-id extraction and phase env | `acceptance-run.sh` inline | test-acceptance-run (a copy of the sed), test-release-acceptance (greps) | Move to `spira/acceptance-lib.sh`, sourced by acceptance-run.sh; T1 against captured `bd create` outputs, including the GH#2950 warning prefix. |
| SPIRA_LOOM_BIN resolution in a release layout | `conf.sh` | test-artifact-install (152 s incl. cargo) | Already a 1-second assertion inside that file. Lift it into test-conf or a release-layout T1 case. |

---

## 6. Gaps

| # | Gap | Evidence | Proposed test |
|---|---|---|---|
| G1 | **`uninstall.sh --purge-database` is untested.** It is the one irreversible verb: it counts beads, requires the operator to type the count back, and refuses on mismatch. | uninstall.sh:330–349; `grep purge-database test-*.sh` finds nothing. | T2: fake `bd` returning N. Assert that a wrong typed count leaves `SPIRA_DB` intact with a non-zero exit, that the right count removes it, and that plain `--purge` never touches the DB. |
| G2 | **`skew.sh copies` is untested.** It exits 3 "the map or the matcher is wrong" when no mapped repo carries a harness. | skew.sh:539; no test invokes `skew.sh copies`. | T2: repo-map with one harness-bearing repo → listed; none → exit 3. |
| G3 | **Root `install.sh` exit-code contract is unverified** (0 ready / 1 preflight / 2 phase failed / 3 installed-but-not-ready / 5 conflict). Only 2 and 5 are asserted, and `--dry-run` "change nothing" is not asserted across phases. | install.sh:33–39; test-release-acceptance only greps. | T2 on the shared root-install fixture: stub `ready.sh` exit 1 → install exit 3. With `--dry-run`, snapshot HOME and XDG dirs before and after. |
| G4 | **activate never proves "prune never removes current".** Activation repoints `current` before the prune runs. | test-activate notes; activate.sh:217. | T1 on `_prune_candidates` with current = oldest, keep = 1. |
| G5 | **activate's daemon-reload-before-restart ordering is claimed but not asserted.** | test-activate header, property 8. | Assert line order in the mock systemctl log. |
| G6 | **deploy rollback restoring spira.conf is not asserted.** `_rollback` moves `_conf_backup` back, and if that fails it only logs. | deploy.sh:373–378; test-deploy asserts `current` restored (P5), but no case checks conf content after rollback. | Extend P5: after the doctor-fail rollback, `SPIRA_PROD` in spira.conf equals its pre-deploy value. |
| G7 | **Four of the five path-collision keys are untested** (`SPIRA_DB`, `SPIRA_PROD`, `SPIRA_DOLT_DATA`, `SPIRA_TESTDB_PORT`). The "no unit written after refusal" assertion is vacuous. | test-install-paths notes; sp-dt8u. | The T1 table in section 5, one row per key. |
| G8 | **The shipped tarball is never exercised in gate CI.** test-artifact-install hand-assembles the tarball because `git archive` fails in the container, so `build-tarball.sh → activate → run` is proven only in acceptance.yml. The scratch-file exclusion check in test-build-tarball is vacuous because nothing plants an `sp-*` file. | test-artifact-install and test-build-tarball notes; sp-kcx8. | Plant `sp-x.fixed` in the fixture (positive control). Make acceptance.yml the one place a real tarball is built and installed. |
| G9 | **Stage isolation from the real DB is unverified.** `_REAL_DB` is never set, so test-canary::T3 "not visible in real db" always auto-passes. | test-canary notes. | Set `_REAL_DB` to a second throwaway store and assert absence (a positive control first). |
| G10 | **unit-ensure MISSING-TARGET tests an in-test reimplementation.** It would pass if the guard in `unit-ensure.sh` were deleted. | test-install-unit-ensure notes. | Drive the real script with a unit whose ExecStart lacks +x. |
| G11 | **`world.sh start` watcher revival has no behavioural test.** The escalation-undelivered-8-hours scar in world.sh's own comment is guarded only by a source grep. | test-world-start-revives-watchers (SG); world.sh:289–327. | T2 on the recording stub: an inactive `spira-watch-x-prod` gets started; a oneshot or already-active one is not. |
| G12 | **The real-systemd properties run nowhere automated.** cadence "never disarms" (sp-dah), the install→uninstall byte-identical round trip, and the installed watcher names are all NA in CI; they are container- or host-only. | signals.tsv NA ×4; r1 "not_run_in_ci". | Put test-install-rehearsal.sh and the systemd half of test-cadence-tool.sh into acceptance.yml, which already has a user systemd session. |
| G13 | **Vacuous exit-code assertions.** In test-install-idempotent, `ctrl_rc`/`noop_rc` are read after `$(cat …)`. The FORCE nowant cases in test-install-landref, and test-install-halt, have no rc assertion at all. | mapper notes. | Fix while porting to the T1 table. Add a T0 lint that flags `rc=$?` following a command substitution. |
| G14 | **test-install-conflicts binds a fixed port (19877) shared with test-install-dolt-port-wait.** Under parallel batch this is a flake source. | mapper notes. | Allocate an ephemeral port (`python -c 'socket…bind(0)'`). |

---

## 7. Cost

**Now.** 58 primary files with timings sum to 1,092 suite-seconds. The 4 NA files add 0 because they did not run.

Top contributors: artifact-install 152, uninstall 78, world-start-honours-ctrl 71, world-drain 58, install-aeons 57, acceptance-run 43, install-idempotent 42, install-paths 39, install-instance 37, install-migrate 31, install-conf-seed 30, install-exec 29, install-unit-prune 28, canary 27, ctrl 27, world 25. Those 16 files account for 794 s (73%).

**Projected, file by file:**

| File | Now → projected (s) | Reason |
|---|---|---|
| artifact-install | 152 → 1 | T1 conf case; cargo build and live loom move to acceptance |
| uninstall | 78 → 20 | shared render, no git |
| world-start-honours-ctrl (+revives-watchers 20) | 91 → 6 | ctrl single read + T1 table |
| world-drain | 58 → 0 | deleted |
| install-aeons | 57 → 15 | guard + drain + end-state only |
| acceptance-run | 43 → 2 | real bd init deleted, T1 extraction |
| install-idempotent, install-halt | 42 + 17 → 0 | merged into new test-install-decide, +3 |
| install-paths | 39 → 2 | |
| install-instance (+unit-prune 28) | 65 → 8 | |
| install-migrate (+unit-directive 14) | 45 → 6 | |
| install-conf-seed | 30 → 1 | |
| install-exec | 29 → 3 | |
| canary | 27 → 8 | one stage bring-up |
| ctrl | 27 → 4 | |
| world | 25 → 15 | |
| unit-drift | 20 → 8 | |
| world-drain-deadline | 20 → 20 | unchanged |
| owned | 19 → 8 | |
| world-timer-names (+service-result 17, +work-services-exclusion 3) | 33 → 4 | |
| deploy | 14 → 10 | |
| install-unit-ensure | 12 → 8 | |
| loop-readonly | 12 → 8 | |
| install-prod-checkout | 10 → 2 | |
| timer-templates (+restart-limits 1, +cockpit-unit-notify 2, +verdict-timer 2, +tarball-bins 1) | 16 → 3 | |
| install-dolt | 7 → 4 | |
| acceptance-ci | 6 → 5 | |
| acceptance-ready | 6 → 0 | deleted |
| install-bootstrap-release | 5 → 4 | |
| loom-broker (+mail-deliver 3, +self-test 5) | 10 → 3 | |
| activate (+rollback 4) | 6 → 3 | |
| release-acceptance | 2 → 1 | |
| cadence-tool T1 half (new) | 0 → 1 | |
| unchanged | 69 → 69 | session-hook 13, review 8, build-bd-release 6, install-conflicts 6, release 6, boundary 5, capacity-probe 5, build-tarball 4, build 4, install-landref 4, skew-foreign 3, skew-check-release 2, skew-refresh 2, unit-name 1 |

The rows sum to **255 suite-seconds**, a saving of 837 s (−77%).

Out of the gate and into acceptance.yml (release only): the loom cargo-build + live-serve leg (about 150 s today), test-install-rehearsal (~3 min, header), and the test-cadence-tool systemd half (~2 min). test-host-unit-names and test-cadence.sh leave the suite set.

**Excluding the 5 misfiled files:** now 1,034 s → projected 221 s.

Largest single lever: extracting a sourceable `systemd/install.sh` library (`_unit_action`, `_check_path_collisions`, `_seed_instance_conf`, `_execstart_ok`) plus one cached render fixture. Together they remove about 330 s across 10 suites. Second: `world.sh start` reading ctrl state once, which removes about 85 s in CI and a per-timer conf.sh + python start-up on every production `world.sh start`.

---
