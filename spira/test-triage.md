# Suite triage — every suite of 30 s or more

Deliverable of the build/test efficiency plan, item 10. One row per suite, with its wall time, the
property it asserts, where that property should be proven, and its family. The Concierge files one
migration bead per family from this table.

## How to read it

- **wall** is the median `.result` seconds across the round-VM salvage runs in
  `run/round-vm/.salvage.*` (n = 5 for nearly all; n = 1–4 where noted by a fractional value). Those
  runs share a VM with ~450 other suites, so the figure includes contention: use it to rank, and
  re-measure alone (`testenv --suites <suite>`) before and after a migration. 77 suites reach 30 s
  by this source; the plan's count of 66 came from a different window.
- **property** is read from the suite's own header and fixture, not re-derived by running it. A
  migration bead confirms the property against the body before deleting anything.
- **target** is one of:
  - **unit** — the property is a pure decision, table or argv/text builder; prove it as a Rust test in
    the crate that owns it (`cargo test -p`, sub-second), and delete the shell rows that restate it.
  - **sim** — the property is a hand-off between components (submit → gate → land → close, claim →
    teardown → requeue). Move it to a scenario under `spira/sim/scenarios/`; a suite moves only when
    a scenario covers the property faster and without flipping.
  - **keep** — the property is the real dependency itself (Dolt grants, systemd, installer, container
    runtime, a real clock). The reason is given. A kept suite may still be sharded or have its fixture
    shared; "keep+shard" means the wall is the lever, not the content.
- A suite named `-wired`, `-exit`, `-land`, `-rest`, `-mech` or `-sticking` is a wall-time shard of the
  suite beside it: it follows its parent's target and goes with it.

## Families

| fam | subject | shared fixture | suites | wall s |
| --- | --- | --- | ---: | ---: |
| A | sim worlds | `sim run` | 2 | 259 |
| B | submit → gate → land → close (landing pass) | landing.sh fixture, testdb, fixture repo | 7 | 403 |
| C | queue and round: publish, open-batch, transition, land-local, batcher-cut, release | queue fixture repo + forge fake | 11 | 588 |
| D | aeon teardown and close fences | full-aeon fixture | 8 | 518 |
| E | aeon claim, resume, worktree and wiki hygiene | testdb + fixture repo | 11 | 561 |
| F | persona brief, prompt layers, model | fayth + chamber render | 7 | 391 |
| G | install, uninstall, deploy, units, timers | mock systemctl, bare-host | 10 | 519 |
| H | watchers and ops detection | testdb | 11 | 622 |
| I | lifecycle store (Dolt) | throwaway `dolt sql-server` | 4 | 167 |
| J | gate internals | gate.sh fixture | 5 | 186 |
| K | bead tooling | testdb | 1 | 92 |

## Table

### A — sim worlds

| suite | wall | property | target | reason |
| --- | ---: | --- | --- | --- |
| test-sim-scenarios.sh | 195 | every scenario × fixed seeds reaches its goal with all invariants; two controls prove the invariant checker can fail | keep+shard | **Done (sp-ypunvh):** one `test-sim-<scenario>.sh` per scenario file over `sim-scenario-lib.sh`; the two controls and the every-scenario-has-a-suite check are `test-sim-controls.sh`. |
| test-sim-happy-path.sh | 64 | the happy-path world runs submit → land end to end under the real units' wiring | keep | **Done (sp-ypunvh):** it is the happy-path scenario's suite (run, probe, replay, unreachable-goal control); the shard loop does not run it a second time. |

### B — landing hand-offs

| suite | wall | property | target | reason |
| --- | ---: | --- | --- | --- |
| test-landing.sh | 88 | a landing pass puts finished work back on the board only when the work itself disagrees with the base | sim | Hand-off between pass, gate and lifecycle; `resubmit-moved-tip` and `conflicting-siblings` scenarios already cover the neighbouring rows. |
| test-submitted-lands.sh | 85 | a model-submitted work bead in a non-queue repo submits, gates, lands and is closed by close-on-land | sim | Literally the happy-path hand-off; `happy-path.toml` + `submit-from-other-cwd.toml` cover it, so this is the first candidate to delete. |
| test-landing-rebase.sh | 52 | survivor sweep rebases withheld branches when the base moves; loop-defer guard; checkout refresh | sim | Base-moves-under-a-branch is a scenario (`resubmit-moved-tip`); the checkout-refresh row is a unit test in the landing-pass crate. |
| test-landing-base-fail.sh | 30 | a red base holds every pending branch and files one incident; the held branch lands once base is green | sim | Multi-pass interleaving over time is what the sim's clock is for. |
| test-certify.sh | 54 | a closed branch is gated once per tip and certified on PASS, with no local rebase | sim | `certify-twice.toml` already asserts once-per-tip; add the no-rebase row there. |
| test-express-lane.sh | 56 | the express label bypasses sentinel admission, and only that | unit | Admission is a predicate in the sentinel crate; the shell fixture only reaches it through a repo. |
| test-config-compat-master-base.sh | 38 | the landing path works with a `master` base, not just `main` | sim | Same scenarios with `base = "master"`; a parameter, not a suite. |

### C — queue and round

| suite | wall | property | target | reason |
| --- | ---: | --- | --- | --- |
| test-batcher-cut.sh | 77 | a whole round through a fixture repo, stub round-vm and fake forge: the batcher binary's IO wiring | sim | `cut-in-bare-env.toml` and `publish-then-cut.toml` cover the wiring; the shell suite is the older copy. |
| test-batcher-cut-land.sh | 88 | the land-mode half of the batcher-cut cases | sim | Shard of the above; goes with it. |
| test-queue-publish.sh | 50 | `queue publish` pushes local/main's new commits to one forge branch and opens one PR; verdict settles it separately | sim | `publish-then-cut.toml` is this hand-off; the red-verdict fix-forward bead row stays as a unit test in the queue crate. |
| test-queue-open-batch.sh | 39 | open-batch assembles, pushes, opens the PR and writes the open record in one command | keep | Real git merges and conflicts against a fixture remote; the assembly is retired code paths' last cover. Decide delete-with-the-code in the family bead. |
| test-queue-transition.sh | 38 | `to-forge` holds the lock, publishes green, checks heads equal, flips the repo-map row, archives local/main | sim | Multi-step state machine with a lock; scenario step. |
| test-queue-land-local.sh | 36 | land-local fast-forwards only a descendant head, archives it, marks members landed, closes beads | sim | The landing leg of `happy-path.toml`; the refuse-non-descendant row is a unit test in the queue crate. |
| test-queue-ops.sh | 55 | queue eject and abandon | sim | Eject is a hand-off (`work`/lifecycle/queue); add a scenario step. |
| test-land-local-release.sh | 34 | a harness landing under queue.local builds, verifies and activates a release; a failed build leaves current alone | keep | Exercises the real release build/activate on the filesystem; the refusal rows (no corpus) are unit. |
| test-batch-owner.sh | 96 | owner-file protocol lets an orphan sweep find a container after its owner dies | keep | Needs real podman. Only `test-round-vm-e2e.sh` still references `batch-owner.sh`: the family bead should first ask whether the protocol outlived `testenv-batch.sh`, and delete suite and script if not. |
| test-cross-repo.sh | 34 | an aeon on a non-home-repo bead cuts its worktree in that repo and commits land there | sim | A scenario with two repos; the actor already has a repo param. |
| test-cockpit-funnel.sh | 41 | the DONE → LANDED funnel in the QUEUE section counts done, certify and red from lifecycle rows | unit | A read-model count over rows; test the collector in `cockpit-collect`. |

### D — aeon teardown and close fences

| suite | wall | property | target | reason |
| --- | ---: | --- | --- | --- |
| test-aeon-teardown-e2e.sh | 64 | charge and pre-session teardown rows through a real aeon.sh run | **migrated**: whole-run tests in `aeon/src/tests.rs`; suite deleted | The disposition tables are already Rust tests; what is left is aeon → lifecycle wiring. |
| test-aeon-teardown-e2e-wired.sh | 88 | decision-blocked, own-closeout and ledger-segment rows | **migrated**: whole-run tests in `aeon/src/tests.rs`; suite deleted | Shard; goes with parent. |
| test-aeon-teardown-e2e-exit.sh | 81 | yield-headless, exit-code and operator-wait rows | **partly migrated**: yield-headless and exit-code rows moved to `aeon/src/tests.rs`; the operator-wait rows stay (real work broker and mail) | Shard; goes with parent. |
| test-aeon-lifecycle-cutover.sh | 65 | aeon.sh on the semantic layer against a throwaway Dolt and real bd | sim (not migrated: the sim has no aeon actor, and the property is aeon against real bd) | The semantic layer has a sim; the Dolt-grant half belongs to family I. |
| test-ops-closing.sh | 92 | close-time fences on an aeon's finished session, through the real aeon with a shim | **migrated**: aeon runs are whole-run tests; the shell keeps only the pure phrase table | Fence = predicate over close evidence (unit) + wiring (scenario step); neither needs 92 s of aeon. |
| test-thrash-teardown.sh | 45 | a thrash disposition requeues instead of charging an attempt | **partly migrated**: dispositions moved to `aeon/src/tests.rs`; the real-SIGTERM single-teardown case stays (needs a real process group) | Branch in `cleanup()` over a recorded disposition; belongs in the aeon crate's `decide` tests. |
| test-aeon-gate-unfinished-attempts.sh | 31 | releasing a bead while its gate is still running leaves `attempts_of` unchanged | **migrated**: `aeon/src/tests.rs` plus `spira-claim` events fold; suite deleted | An SQL net-zero rule over events; test the attempts query. |
| test-slay.sh | 52 | slay stops one aeon cleanly, removes its work, makes the bead say what is true | sim (not migrated: the sim has no aeon actor or process group to slay) | Operator action in a world with a live aeon. |

### E — aeon claim, resume and hygiene

| suite | wall | property | target | reason |
| --- | ---: | --- | --- | --- |
| test-aeon-resume.sh | 80 | a branch with prior commits renders RESUME_BRIEF; a slay wip salvage renders SLAIN_BRIEF | unit | Text render over inputs. |
| test-aeon-resume-collision.sh | 40 | a lost resume race falls through to the next resumable candidate, not the general queue head | unit | Candidate selection after a failed claim; pure ordering. |
| test-epic-claim-order.sh | 74 | claim rank: min(epic, bead) priority, started epic first, resumable before fresh, oldest | unit | A sort key; `epic_rank_rows` already isolates it. Cheapest win in the family. |
| test-aeon-worktree-collision.sh | 77 | never a second live session in a worktree another bead holds | sim | Two aeons racing is a scenario, and flips in shell. |
| test-aeon-world-stop.sh | 40 | a world-stop-labelled bead is not run while live aeons exist | unit | A predicate over labels and live count. |
| test-aeon-prod-dirty.sh | 55 | the prod-dirty guard is bound to the aeon's own worktree, not the shared checkout | unit | Path-scoping predicate; keep one git fixture row. |
| test-aeon-prod-dirty-rest.sh | 47 | cases 2 and 3 of the above | unit | Shard; goes with parent. |
| test-aeon-wiki-dirty.sh | 39 | only this session's own still-dirty wiki writes are committed at exit, never `wiki/tasks.md` | unit | Path filter over a transcript. |
| test-aeon-sweep.sh | 44 | `aeon.sh --sweep` runs the persona without a bead | keep | Launcher wiring with real argv; only a real aeon.sh proves it. Candidate to share the full-aeon fixture with D. |
| test-unclaimable.sh | 30 | ready beads no persona can claim are surfaced; the detector does not report its own reports | unit | Claimers arithmetic plus a cycle guard. |
| test-unclaimable-worktree.sh | 35 | detection uses production config, not the calling worktree's conf.sh | unit | One resolver call with two configs. |

### F — persona brief, prompt, model

Migrated (sp-uxdjnk). Wall before is the table's round-VM median; the alone figures are
`testenv --suites <suite>` on the parent commit and on this branch.

| suite | wall | property | outcome |
| --- | ---: | --- | --- |
| test-aeon-chamber-overlay.sh, -mech.sh | 60 + 60 | builder brief says how to verify once; overlay beats release text | deleted. Overlay mechanism was already `brief::tests::overlays_whole_section_append_and_absent`, doctor's rows `doctor::tests::chamber_overlays_*`; added `brief::tests::the_shipped_builder_brief_has_one_way_to_verify_and_the_work_verbs_to_finish` (golden rows). |
| test-aeon-prompt-layers.sh, -sticking.sh | 53 + 58 | claude argv from the fayth knobs, thrash banner | deleted. Banner and split were `brief::tests`; added `aeon::tests::a_replace_fayth_…`, `an_append_fayth_…` and `brief::tests::the_shipped_groomer_splits_…`. |
| test-persona-model.sh | 42 | one resolver for a persona's launch model | deleted. Resolver was `conf::tests::persona_model_reads_…`; added `aeon::tests::the_launch_model_is_the_toml_persona_model_…` and `conf::tests::no_launcher_scrapes_a_model_…` (the source greps). |
| test-summon-fayth.sh | 86 | `summon_fayth` refusal ladder and `fayth_free` arithmetic | ladder moved to eight `sentinel` tests (`an_elastic_fayth_…`, `a_task_fayth_yields_…`, `the_collective_lane_cap_…`, `the_fleet_ceiling_…`, `a_task_fayth_honours_the_pool_…`, `an_elastic_fayth_takes_the_pool_remainder_…`, `a_summon_carries_a_timeout_…`, `a_halt_refuses_…`). Kept: lib.sh `fayth_free` (real bash), `aeon --escape` through the world gate, the `world drain` expiry writer. Alone: 312 s before, 133 s after. |
| test-concierge.sh | 32 | Concierge brief composition, resume/headless handling, historic ways in broke | kept. `concierge.sh` is the shell program under test (statute-book brief, tmux, `/proc` holder scan, wake delivery); there is no Rust owner to move it to, and a unit test of a shell function is the same shell test. |

### G — install, deploy, units

| suite | wall | property | target | reason |
| --- | ---: | --- | --- | --- |
| test-install-aeons.sh | 59 | install's live-aeon guard, oneshot drain and end-state check | keep | Real install.sh against a mock systemctl; bare-host. |
| test-uninstall.sh | 59 | uninstall removes the default tier, is idempotent and instance-aware, reports planted strays | keep | Bare-host, 816 lines of real filesystem effects. |
| test-deploy.sh | 33 | deploy: fetch, drain, activate, re-render, health check, rollback, dry-run, draft refusal | keep | Real deploy path; wall is 1,792 lines, shard by property. |
| test-units-lint.sh | 53 | every unit renders with no unresolved placeholder or literal path, and carries no CPUQuota/Nice | unit | A render pass plus text checks; render once in a Rust test. |
| test-cadence-tool.sh | 84 | cadence.sh changes a timer's cadence and refuses to disarm one | keep | Real systemd; wall is two fixed sleeps (8 s unit, 2 s) repeated per row. |
| test-canary.sh | 74 | `release stage` and `release canary` build an isolated environment and run the pipeline end to end | keep | The canary is the thing it tests. |
| test-testenv-curl-bounds.sh | 47 | every curl in the image build has connect and total timeouts and a retry | unit | A grep over the Containerfile that should cost nothing; 47 s means it builds something. |
| test-session-hook.sh | 33 | the session hook's output prepended to a fresh context window | unit | Text output. |
| test-spira-config-only-door.sh | 40 | no crate but spira-config resolves, reads or parses `spira.toml` | unit | A source scan; run as a cargo test over the workspace. |

### H — watchers and ops detection

| suite | wall | property | target | reason |
| --- | ---: | --- | --- | --- |
| test-watch-refresh.sh | 109 | the staleness guard, and the cost of one pass | unit | The guard is a rule; the cost row stays with I as a measurement. |
| test-incident.sh | 80 | the intake dedupe holds against two concurrent filers of the same ref | keep | A real race on a real store; two connections are the point. |
| test-census.sh | 31 | census against a real bd fixture for three ops-detection use cases | keep | Declared the one server-mode census suite. |
| test-census-events.sh | 78 | census reads requeue/reclaim/recur events written by `bump_*` | unit | Event classification; dominated by seeding a store. |
| test-heimdall.sh | 75 | the watchdog reconciler raises, clears and re-raises against a real bd | sim | A watcher over time; the classifier is already pure. |
| test-groom-escalation-check.sh | 71 | the groom ask label is a conf key defaulting to `groom-asked` | unit | One config key; the plan asks fixtures pin a non-default. |
| test-mimir-closed-record.sh | 32 | the closed-record INVALID-CLOSED trigger and allowlist | unit | A trigger over rows. |
| test-yield.sh | 32 | a gate red lands in the right column; an unreadable record says `?` | unit | Panel column rule. |
| test-holds.sh | 42 | holds reports open beads whose branch touches a set of paths | unit | Path-overlap set logic. |
| test-mail-aeon.sh | 36 | amend mails the live aeon; the mailbox is removed on exit | sim | A message hand-off with a lifecycle end. |
| test-verdict-flow.sh | 36 | mail sendmail: bead closing and decision-bead verdict flow back | sim | Hand-off across mail, bead and sentinel. |

### I — lifecycle store (Dolt)

| suite | wall | property | target | reason |
| --- | ---: | --- | --- | --- |
| test-lifecycle-cutover.sh | 43 | batch cascades `cut`, `land`, `settle`, `abandon-batch` as the spira_lc user | keep | Real grants; the cascades' logic is already covered by `sim`. |
| test-lifecycle-container.sh | 42 | real Dolt, real grants, and properties that need a second connection | keep | Real dependency. |
| test-ops-read-model.sh | 33 | the ops_live/ops_round/ops_recent and where-stuck views over 12,000 beads and 100,000 events | keep | The scale is the point; seed once per run, not per row. |
| test-spike.sh | 49 | the spike persona's partition is its own and its branch carries a document only | unit | A path allowlist. |

### J — gate internals

| suite | wall | property | target | reason |
| --- | ---: | --- | --- | --- |
| test-gate-tree.sh | 32 | the gate tree is locked per branch; same-branch serialise, different-branch run concurrently | keep | Needs real concurrent locks. |
| test-commit-cite.sh | 31 | the gate refuses a commit citing a nonexistent bead and never mistakes "store could not answer" for "exists" | unit | Two-valued lookup with an error state. |
| test-statute-projection.sh | 44 | rule.sh: synth-failure propagation and the enact/retire/list/show set | keep | Real statute book; fixture is the cost. |

Migrated (sp-k8z731): test-gate-verdict.sh (49 s) and test-gate-base-evidence.sh (30 s) moved to `cargo test -p gate` (whole crate 7.5 s warm, these tests sub-second). test-commit-cite.sh stays: the id extraction is `commit-cite.sh`'s grep -P over real `bd show`, not yet a Rust unit; the Rust gate tests hold only its exit-code mapping.

### K — bead tooling

| suite | wall | property | target | reason |
| --- | ---: | --- | --- | --- |
| test-bead-lint.sh | 92 | `bead.sh lint` wiring over real `bd show --json`, including the branch-label check | keep | The judgement is already T1 (`_bead_lint_judge`); the real `bd` wiring is what this holds. |

## Order of work

By seconds recovered per bead, most first: **B** (landing hand-offs, to sim), **D** (teardown, to sim and
unit), **E** and **F** (unit; ~950 s together and nearly all mechanical), **C**, **H**, **A**
(shard). **G**, **I** and **K** are mostly *keep*: the lever there is a shared fixture and fewer
fixed sleeps, not removal.

Each migration bead must report before and after wall from the same source as this table, and
delete the shell rows it replaces in the same commit, so a property is never proven in two places.
