# Which bash suites a sim scenario can replace

Status: proposed. Nothing here is rewritten yet. Classified 2026-10-09 against `spira/test-*.sh` on `local/main`.
Planning deliverable W12 of the R1 release. Rewriting tests onto the sim is after R1 (design Non-goals); this note only classifies and proposes an order.

## Verdict

**450 suites on `local/main` (`spira/test-*.sh`, 2026-10-09): 19 replaceable, 44 partly, 384 no, 3 already sim.**
The design says 426; the tree has grown by 24 since it was written. `test-sim-scenarios` is the destination, not a row.

The sim replaces little by count and a bounded amount by time. Its subject is the **hand-offs** between loop actors (submit, certify, round, land, publish, tag) and the interleavings between them. Almost every other suite tests one tool's contract (a lint, a renderer, a parser, an installer), and a world of actors adds nothing to those.

| class | suites | recorded median seconds | suites with no timing |
|---|---|---|---|
| replaceable | 19 | 473 | 8 |
| partly | 44 | 658 | 20 |
| no | 384 | 4151 | 104 |
| already sim | 3 | 0 | 3 |

Timings are the median of each suite's runs in `suite-times.log` (batch results through 2026-09-26, parallel mode, so they are what a suite costs inside a round, not alone). A `?` means the suite has no recorded run, because the suite is newer than those runs; the totals are therefore lower bounds. The full-run figure in the design (410–800 s on the round VM) is wall time; the sums here are CPU-seconds across suites and are larger.

**What the sim saves is the replaceable seconds, not the partly ones**, and not all of those: the sim suite costs its own wall time (budget: under 5 minutes for the whole suite, per the design). So the saving is the replaceable suites' seconds minus the marginal seconds of the scenarios that absorb them. The strongest case for moving a suite is not its seconds but that the sim is deterministic: the same seed gives the same trace, so a suite that flips on a loaded box stops flipping.

## How a suite was judged

- **Replaceable**: the suite's property is a hand-off or an interleaving among the loop's actors, and a scenario (existing, or one of the ten proposed below) plus `[[expect]]` queries and world invariants states it. The bash suite can then be deleted.
- **Partly**: some rows are hand-off rows the sim can state, and others are tool-level (output wording, a refusal's text, real-git conflict mechanics, a real-Dolt grant). The hand-off rows move; the rest stay as a smaller bash suite. A suite's name in the table's scenario column says which scenario takes its hand-off rows.
- **No**: the suite tests one tool or a static property: lints, renderers, install and unit contracts, config, mail formatting, detectors over their own fixtures. Nothing in it is a hand-off, or it needs fault injection, which the design excludes.

The classes for **replaceable and partly** were set from each suite's header, its `# covers:` line and what it exercises. The **no** reasons are by family (the pattern of the name and the covers line), not a reading of every body; a suite that looks misfiled is worth a second look before it is acted on, and the cost of a wrong **no** is only a missed saving.

What the stub agent cannot do also bounds the table: the sim runs `sim summon`, not `aeon`, so suites about the real aeon session (brief, resume, teardown, fences) stay out. A scenario that runs the real aeon under a scripted model is a possible later extension and would pull about 50 of the **no** suites toward **partly**; it is not proposed here.

## The scenarios

Existing nine (spira/sim/scenarios): `happy-path`, `certify-twice`, `resubmit-moved-tip`, `submit-from-other-cwd`, `conflicting-siblings`, `long-gate-interleaving`, `landstate-lifecycle-agree`, `publish-then-cut`, `cut-in-bare-env`.

Proposed ten, each named by the suites that need it:

| scenario | what it walks | suites it takes |
|---|---|---|
| `red-gate-holds` | gate-worker returns a red verdict for a member; the branch is withdrawn and attempts counted; a red base holds every pending branch and files one incident | landing, landing-base-fail, withdrawn-suites-recert, gate-base-evidence, attempts, attempts-sql, timeout |
| `push-race` | a second landing moves `local/main` between a pass's verdict and its push | landing-race |
| `claim-vs-batch` | a member bead is claimed while its batch's `delivered` event lands | batch-claim-race |
| `eject-mid-step` | an operator `queue eject` while `queue step` runs on the same repo | queue-step-eject-race, reopen-queue-eject, queue-ops |
| `supersede` | a bead asks to be superseded by a landed successor and by an unlanded one | supersede-duty, superseded |
| `claim-order` | several READY beads (priorities, epics, an open child, an express bead); the order they are claimed in | dispatch-open-children, epic-claim-order, express-lane, branch-collision-park, claim-retry |
| `second-repo` | a bead for a non-home repo, one with a `master` base | cross-repo, config-compat-master-base, landing-modes |
| `forge-batch` | queue.forge: open batch, PR checks, verdict, owner mutations, via the fake GitHub | queue-open-batch, queue-owner-*, queue-transition, lifecycle-delivery-cutover |
| `reap-after-land` | closed and superseded branches after landing; what is reaped | sending, sending-closed-reap, sending-mode-filter |
| `stuck-pipeline` | a stub agent that stalls and a bead that strands; detectors run as actors | watchtower, watchtower-lapse, closed-strand, strand-submitted |

The first five are what batch 2 needs; the other five are worth building only once the first two batches are moving.

## Proposed first batch

**Batch 1: eleven suites, no new scenario.** Each maps onto an existing scenario; the work is adding `[[expect]]` queries and invariants, not new worlds.

| suite | scenario | recorded s |
|---|---|---|
| test-submitted-lands | happy-path | ? |
| test-queue-submit | happy-path, submit-from-other-cwd | 4 |
| test-queue-land-local | happy-path | ? |
| test-land-local-release | happy-path, cut-in-bare-env | ? |
| test-queue-publish | publish-then-cut | ? |
| test-certify | certify-twice, resubmit-moved-tip | 89 |
| test-certified-withdraw | resubmit-moved-tip | 18 |
| test-lifecycle-cert | certify-twice, landstate-lifecycle-agree | ? |
| test-landing-queue-early | long-gate-interleaving | 56 |
| test-landing-rebase | conflicting-siblings | 64 |
| test-rebase-escalation | conflicting-siblings | 8 |

Recorded total 239 s, with 5 of the 11 unmeasured, so it is a floor. They are the loop's main path (certify, land, publish, tag) and its two best-known interleavings (conflicting siblings, a long gate), which is why they go first: the sim suite already walks them.

**Batch 2: eight suites that need five new scenarios** (`red-gate-holds`, `push-race`, `claim-vs-batch`, `eject-mid-step`, `supersede`).

| suite | scenario | recorded s |
|---|---|---|
| test-landing | conflicting-siblings + new:red-gate-holds | 137 |
| test-landing-race | new:push-race | 33 |
| test-landing-base-fail | new:red-gate-holds | 56 |
| test-batch-claim-race | new:claim-vs-batch | ? |
| test-queue-step-eject-race | new:eject-mid-step | 5 |
| test-withdrawn-suites-recert | resubmit-moved-tip + new:red-gate-holds | ? |
| test-reopen-queue-eject | resubmit-moved-tip + new:eject-mid-step | 3 |
| test-supersede-duty | new:supersede | ? |

Recorded total 234 s, 3 unmeasured. These are the interleavings that are slow or hard to reproduce in bash today; they gain the most from a seed.

**Rule for moving any suite** (the alignment with the design's "a suite moves when a scenario covers its property faster and without flipping"):
1. Add the scenario's `[[expect]]` queries for the suite's assertions, and keep the bash suite running beside it for several rounds.
2. Plant the defect the bash suite exists for (its header names it) and see the scenario go red naming it. A scenario that has not been seen red does not retire a suite.
3. Only then delete the bash suite, in the same commit that records the scenario as its replacement.

**Budget check before batch 2:** the nine scenarios already run side by side up to eight at a time; five more scenarios, each with the fixed seed list and a fresh seed, must keep the suite under the design's 5 minutes. Measure before building the fifth; if it does not fit, split into a second suite that is not a hard gate rather than raising the cap.

**Not proposed, and why:** suites for the lifecycle store against real Dolt (grants, migrations, read-model timing) stay container tests: the store is the sim world's dependency. Detectors that need an injected fault (a dropped connection, a crashed unit) stay bash because the sim excludes fault injection.

## The table

Every `spira/test-*.sh` on `local/main`, by name. Scenario `new:` names refer to the proposed scenarios above; `-` means none.

| suite | class | scenario | s | note |
|---|---|---|---|---|
| acceptance-agent | partly | happy-path | ? | the stub agent acceptance uses; the sim's stub agent v2 now does this job |
| acceptance-ci-scratch-queue | partly | publish-then-cut | ? | acceptance-ci's queue.local scratch repo gets a local base ref |
| acceptance-local-stop-live | no | - | ? | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| acceptance-local-stop | no | - | ? | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| acceptance-local | no | - | 1 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| acceptance-prev-tag | no | - | 1 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| aeon-base-ref-qualify | no | - | 4 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-chamber-overlay-mech | no | - | ? | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-chamber-overlay | no | - | 33 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-dirty-commit | no | - | 4 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-elastic-concurrency | no | - | 4 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-eviction-race | no | - | 55 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-fence-exec-ctx | no | - | ? | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-gate-close-silent | no | - | 30 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-gate-unfinished-attempts | no | - | ? | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-launch-grammar | no | - | 2 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-lease | no | - | 3 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-lifecycle-cutover | no | - | ? | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-prod-dirty-rest | no | - | ? | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-prod-dirty | no | - | 49 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-prompt-layers-sticking | no | - | ? | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-prompt-layers | no | - | 41 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-resume-collision | no | - | 17 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-resume | no | - | 54 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-slain-attempts | no | - | ? | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-sweep | no | - | 26 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-teardown-e2e-exit | no | - | ? | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-teardown-e2e-wired | no | - | ? | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-teardown-e2e | no | - | ? | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-watch | no | - | ? | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-wiki-concurrent | no | - | 8 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-wiki-dirty | no | - | 41 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-worktree-collision | no | - | 33 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| aeon-world-stop | no | - | 29 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| archive-growing | no | - | ? | static, configuration or single-tool contract: no hand-off between loop actors |
| archive-no-sessions | no | - | ? | static, configuration or single-tool contract: no hand-off between loop actors |
| archivist-conf | no | - | 18 | single-tool or static contract: no hand-off between loop actors |
| archivist | no | - | 23 | single-tool or static contract: no hand-off between loop actors |
| artifact-install | no | - | 150 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| asks | no | - | ? | operator mail channel and ask flow: Maildir and lint behaviour, no loop hand-off |
| attempts-sql | partly | new:red-gate-holds | 44 | attempt counting SQL over a real events trail |
| attempts | partly | new:red-gate-holds | 42 | what counts as an attempt; counts need a reopened-by-red bead |
| auron-classify | no | - | 6 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| auron | no | - | 105 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| bare-main | no | - | ? | static, configuration or single-tool contract: no hand-off between loop actors |
| batch-claim-race | replaceable | new:claim-vs-batch | ? | a batch landing racing an aeon's claim: the claim is refused |
| batch-owner | no | - | 2 | static, configuration or single-tool contract: no hand-off between loop actors |
| batch-timing | no | - | 3 | static, configuration or single-tool contract: no hand-off between loop actors |
| batcher-cut-land | partly | conflicting-siblings | ? | land-mode half of the cut suite; real-git conflict cases stay bash |
| batcher-cut | partly | conflicting-siblings, long-gate-interleaving | 34 | whole round through a stub round-vm; the binary seam rows stay bash |
| bd-contract | no | - | ? | static, configuration or single-tool contract: no hand-off between loop actors |
| bd-schema-stamp | no | - | 3 | static, configuration or single-tool contract: no hand-off between loop actors |
| bdq-invalid-connection | no | - | ? | static, configuration or single-tool contract: no hand-off between loop actors |
| bead-contract | no | - | 2 | static, configuration or single-tool contract: no hand-off between loop actors |
| bead-dep-add | no | - | ? | static, configuration or single-tool contract: no hand-off between loop actors |
| bead-file | no | - | 8 | static, configuration or single-tool contract: no hand-off between loop actors |
| bead-lint | no | - | 23 | static, configuration or single-tool contract: no hand-off between loop actors |
| bead-repo-guard | no | - | 7 | static, configuration or single-tool contract: no hand-off between loop actors |
| beads-push-commit | no | - | 20 | static, configuration or single-tool contract: no hand-off between loop actors |
| beads-push-verify | no | - | 4 | static, configuration or single-tool contract: no hand-off between loop actors |
| beads-sparklines | no | - | 3 | cockpit/pane rendering and its probes: no loop actor changes state |
| bin-manifest | no | - | 14 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| boundary | no | - | 2 | static, configuration or single-tool contract: no hand-off between loop actors |
| branch-collision-park | partly | new:claim-order | 15 | a bead whose branch is held by another bead's worktree is parked |
| branch-guard | no | - | 3 | static, configuration or single-tool contract: no hand-off between loop actors |
| branch-sweep | no | - | ? | single-tool or static contract: no hand-off between loop actors |
| brief-mail-options | no | - | 3 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| broker-allowlist-lint | no | - | 2 | static, configuration or single-tool contract: no hand-off between loop actors |
| broker-units | no | - | 1 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| broker | no | - | 20 | static, configuration or single-tool contract: no hand-off between loop actors |
| build-bd-release | no | - | 2 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| build-fence | no | - | 2 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| build-sh | no | - | ? | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| build-tarball | no | - | 6 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| builder-qa-proposed | no | - | 3 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| cadence-seams | no | - | 2 | static, configuration or single-tool contract: no hand-off between loop actors |
| cadence-tool | no | - | 2 | static, configuration or single-tool contract: no hand-off between loop actors |
| canary | partly | happy-path | 36 | `release stage` + end-to-end pipeline canary; the sim world is a cheaper stage, stage/canary mechanics stay bash |
| census-actor-filter | no | - | ? | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| census-burst | no | - | ? | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| census-events | no | - | 137 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| census-pipeline | no | - | 9 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| census-tz-boundary | no | - | 56 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| census | no | - | 64 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| certified-withdraw | replaceable | resubmit-moved-tip | 18 | reopen strips the submitted label; resubmit is the re-entry |
| certify | replaceable | certify-twice, resubmit-moved-tip | 89 | gated once per tip, re-certified when the tip moves |
| chamber-core-check | no | - | ? | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| chamber-repo-labels | no | - | 4 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| citations | no | - | 16 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| claim-retry | partly | new:claim-order | 6 | transient bd failure retried; the failure itself is injection, stays bash |
| close-reason-flags | no | - | 3 | single-tool or static contract: no hand-off between loop actors |
| closed-strand | partly | new:stuck-pipeline | 22 | stranded-bead detection and recovery needs a world that strands one |
| cockpit-accept | no | - | 5 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-bd-contract | no | - | 20 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-collect-probes | no | - | 4 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-collector-quota | no | - | 3 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-czar-triggers | no | - | 3 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-dup-refs | no | - | 8 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-funnel | no | - | 28 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-fuse | no | - | 13 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-gate | no | - | 20 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-health-restart | no | - | 5 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-health-term-size | no | - | ? | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-hotfix | no | - | ? | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-layout-ensure-focus | no | - | 2 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-layout-mail | no | - | 8 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-layout | no | - | 6 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-probe-fault | no | - | 12 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-queue-section | no | - | 23 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-queue-wait | no | - | 3 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-reachable | no | - | 7 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-ready | no | - | 8 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-rebuild | no | - | 21 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-remote | no | - | 2 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-repo-labels | no | - | 12 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-runtime-path | no | - | 5 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-self | no | - | 4 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-snap-absent | no | - | 3 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-sop | no | - | 9 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-sphere | no | - | 12 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-strand-keys | no | - | 2 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-tmp | no | - | 10 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-tmux-scope | no | - | ? | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-unclaimable | no | - | 7 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-unlanded | no | - | 25 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-unsent | no | - | 48 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit-watcher-owner | no | - | 4 | cockpit/pane rendering and its probes: no loop actor changes state |
| cockpit | no | - | 13 | cockpit/pane rendering and its probes: no loop actor changes state |
| commit-cite | no | - | ? | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| concierge-acceptance | no | - | 4 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| concierge-inbox | no | - | ? | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| concierge-roster | no | - | 6 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| concierge | no | - | 14 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| conf-key-merge | no | - | ? | static, configuration or single-tool contract: no hand-off between loop actors |
| conf-registry-merge | no | - | ? | static, configuration or single-tool contract: no hand-off between loop actors |
| conf-registry-parity | no | - | ? | static, configuration or single-tool contract: no hand-off between loop actors |
| conf-toml | no | - | 35 | static, configuration or single-tool contract: no hand-off between loop actors |
| conf-watch | no | - | 7 | static, configuration or single-tool contract: no hand-off between loop actors |
| conf-writeback | no | - | ? | static, configuration or single-tool contract: no hand-off between loop actors |
| conf | no | - | 3 | static, configuration or single-tool contract: no hand-off between loop actors |
| config-compat-master-base | partly | new:second-repo | 18 | a master-based repo through the whole loop; needs a second repo in the world |
| configure | no | - | 4 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| containment | no | - | 5 | static, configuration or single-tool contract: no hand-off between loop actors |
| content-landed-empty-branch | partly | happy-path | 14 | zero-commit branch is not reported landed; the world's invariant is "LANDED means content landed" |
| covers-seam-closure | no | - | ? | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| cross-repo | partly | new:second-repo | 15 | a non-home repo's worktree and commits; worktree plumbing stays bash |
| ctrl | no | - | 45 | static, configuration or single-tool contract: no hand-off between loop actors |
| ctx-pointer | no | - | 5 | cockpit/pane rendering and its probes: no loop actor changes state |
| cutover-deploy | no | - | ? | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| czar-lint | no | - | 1 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| czar-partition | no | - | 5 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| czar-pass | no | - | 19 | single-tool or static contract: no hand-off between loop actors |
| czar-shadow | no | - | 5 | single-tool or static contract: no hand-off between loop actors |
| deadlock-sweep | no | - | ? | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| deploy-preflight-new-unit | no | - | 13 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| deploy | no | - | 18 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| destroy-branch | no | - | 6 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| destructive-bead | no | - | 6 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| dispatch-open-children | partly | new:claim-order | ? | open parent-child bead is not selected until children close |
| dolt-tmp-prune | no | - | ? | static, configuration or single-tool contract: no hand-off between loop actors |
| drain-banner | no | - | 3 | cockpit/pane rendering and its probes: no loop actor changes state |
| drift | no | - | ? | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| epic-claim-order | partly | new:claim-order | ? | claim rank by epic priority; table rows stay bash |
| escape-census-units | no | - | ? | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| escape-classify | no | - | ? | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| exclude | no | - | 1 | static, configuration or single-tool contract: no hand-off between loop actors |
| express-lane | partly | new:claim-order | 20 | express label bypasses sentinel admission |
| fayth-project-instructions | no | - | 5 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| fayth | no | - | 3 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| first-run | no | - | ? | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| freshclone | no | - | 7 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| gate-admission-suites-only | no | - | ? | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| gate-base-evidence | partly | new:red-gate-holds | 5 | red base names its own red suites |
| gate-base-selection | no | - | 3 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| gate-cap | no | - | ? | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| gate-check-flaky | no | - | 13 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| gate-check-red-twice | no | - | 16 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| gate-check-stderr | no | - | 3 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| gate-check-stuck | no | - | 6 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| gate-ci-diag | no | - | 3 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| gate-locks | no | - | 3 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| gate-metering | no | - | 4 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| gate-preflight | no | - | 3 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| gate-retry-structural | no | - | 2 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| gate-retry | no | - | 2 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| gate-sweep | no | - | 5 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| gate-tree | partly | long-gate-interleaving | 18 | same-branch gates serialise, different branches run concurrently |
| gate-unit | no | - | 3 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| gate-universal | no | - | 3 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| gate-verdict | partly | certify-twice | 11 | verdict computed once, reused for the same question |
| gate-vitals | no | - | 2 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| gate-wait | no | - | ? | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| gh-intake | no | - | 15 | static, configuration or single-tool contract: no hand-off between loop actors |
| gh-issue-closeout | no | - | 40 | static, configuration or single-tool contract: no hand-off between loop actors |
| gh-run-gate | no | - | 12 | static, configuration or single-tool contract: no hand-off between loop actors |
| git-env-isolation | no | - | ? | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| git-push-app | no | - | 3 | static, configuration or single-tool contract: no hand-off between loop actors |
| governor-deleted | no | - | 3 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| groom-escalation-check | no | - | 43 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| groom-trigger | no | - | 6 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| groomer-conf | no | - | 7 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| groomer-litter-predicate | no | - | 7 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| groomer-poison-triage | no | - | ? | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| groomer-split-piece | no | - | 11 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| groomer-unpoison | no | - | ? | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| groomer | no | - | 5 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| guards | no | - | 11 | static, configuration or single-tool contract: no hand-off between loop actors |
| held | no | - | 15 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| hold-liveness | no | - | 2 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| hold | no | - | 12 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| holds | no | - | 11 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| host-reason | no | - | 14 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| id-prefix | no | - | 4 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| incident-cause-rule | no | - | ? | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| incident-decisions | no | - | 16 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| incident-delivers-satisfiable | no | - | 4 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| incident-spool-drain | no | - | 42 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| incident-systemd | no | - | 8 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| incident | no | - | 81 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| install-aeons | no | - | 65 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| install-conf-seed | no | - | 40 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| install-dolt-breaker | no | - | 12 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| install-dolt-port-wait | no | - | 2 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| install-dolt-ready | no | - | 15 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| install-dolt-refuse-stop | no | - | ? | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| install-dolt | no | - | 3 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| install-exec | no | - | 26 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| install-hooks-artifact | no | - | 8 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| install-instance | no | - | 55 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| install-landref | no | - | 7 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| install-migrate | no | - | 39 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| install-naming | no | - | 4 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| install-order | no | - | ? | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| install-populate-lifecycle | no | - | ? | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| install-rehearsal | no | - | 2 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| install-unit-ensure | no | - | 19 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| install-unit-prune | no | - | 39 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| key-history-merge | no | - | ? | static, configuration or single-tool contract: no hand-off between loop actors |
| land-local-release | replaceable | happy-path, cut-in-bare-env | ? | landing publishes a release; cut from a bare env |
| land-mode-local | no | - | ? | static, configuration or single-tool contract: no hand-off between loop actors |
| landing-base-fail | replaceable | new:red-gate-holds | 56 | red base holds every pending branch; one incident |
| landing-cutover-round | no | - | ? | static, configuration or single-tool contract: no hand-off between loop actors |
| landing-halt | no | - | 18 | static, configuration or single-tool contract: no hand-off between loop actors |
| landing-modes | partly | new:forge-batch + new:second-repo | ? | one repo per land mode; push/pr modes need extra world kinds |
| landing-pass-map | no | - | ? | static, configuration or single-tool contract: no hand-off between loop actors |
| landing-queue-early | replaceable | long-gate-interleaving | 56 | a batch green before the gates run lands at the top of the pass |
| landing-race | replaceable | new:push-race | 33 | losing the push race to a concurrent landing |
| landing-rebase | replaceable | conflicting-siblings | 64 | survivor sweep rebases a withheld branch when the base moves |
| landing | replaceable | conflicting-siblings + new:red-gate-holds | 137 | work goes back on the board only when it disagrees with the base |
| landstate-checks-deleted | no | - | ? | single-tool or static contract: no hand-off between loop actors |
| lanes | no | - | 4 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| launchers-as-launched | no | - | ? | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| launchers-bare-env | no | - | ? | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| law-synth | no | - | 8 | static, configuration or single-tool contract: no hand-off between loop actors |
| lc-hold | no | - | ? | lifecycle store, schema and read models against a real Dolt: the store is the sim world's dependency, not its subject |
| lc-list-timing | no | - | ? | lifecycle store, schema and read models against a real Dolt: the store is the sim world's dependency, not its subject |
| lib-callers-defined | no | - | ? | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| lifecycle-cert | replaceable | certify-twice, landstate-lifecycle-agree | ? | certify/resubmit events on a real lifecycle store |
| lifecycle-classify | no | - | ? | lifecycle store, schema and read models against a real Dolt: the store is the sim world's dependency, not its subject |
| lifecycle-consistency-read | no | - | ? | lifecycle store, schema and read models against a real Dolt: the store is the sim world's dependency, not its subject |
| lifecycle-container | no | - | ? | lifecycle store, schema and read models against a real Dolt: the store is the sim world's dependency, not its subject |
| lifecycle-cutover | partly | landstate-lifecycle-agree, conflicting-siblings | ? | batch cascades cut/land/settle/abandon; real-Dolt grants stay container |
| lifecycle-delivery-cutover | partly | landstate-lifecycle-agree + new:forge-batch | ? | pr/push delivery machines; push-mode wiring stays bash |
| lifecycle-guard-gate | no | - | ? | lifecycle store, schema and read models against a real Dolt: the store is the sim world's dependency, not its subject |
| lifecycle-migrate | no | - | ? | lifecycle store, schema and read models against a real Dolt: the store is the sim world's dependency, not its subject |
| lifecycle-stack-migration | no | - | ? | lifecycle store, schema and read models against a real Dolt: the store is the sim world's dependency, not its subject |
| limits | no | - | 16 | cockpit/pane rendering and its probes: no loop actor changes state |
| lint-chamber | no | - | 7 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| livelock | no | - | 71 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| local-pass | partly | cut-in-bare-env | ? | cut refuses without a recorded local pass; override rows stay bash |
| loop-readonly | partly | happy-path | 21 | sentinel claims from a read-only release; needs the world to run read-only |
| maechen-closed-record | no | - | 27 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| maechen-trigger | no | - | 16 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| maechen | no | - | 6 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| mail-aeon-hook | no | - | 2 | operator mail channel and ask flow: Maildir and lint behaviour, no loop hand-off |
| mail-aeon | no | - | 16 | operator mail channel and ask flow: Maildir and lint behaviour, no loop hand-off |
| mail-bead-render | no | - | 7 | operator mail channel and ask flow: Maildir and lint behaviour, no loop hand-off |
| mail-deliver | no | - | 11 | operator mail channel and ask flow: Maildir and lint behaviour, no loop hand-off |
| mail-dismiss-sweep | no | - | 20 | operator mail channel and ask flow: Maildir and lint behaviour, no loop hand-off |
| mail-health | no | - | 8 | operator mail channel and ask flow: Maildir and lint behaviour, no loop hand-off |
| mail-mailbox-arg | no | - | ? | operator mail channel and ask flow: Maildir and lint behaviour, no loop hand-off |
| mail-pane | no | - | 5 | cockpit/pane rendering and its probes: no loop actor changes state |
| mail-real-senders | no | - | 3 | operator mail channel and ask flow: Maildir and lint behaviour, no loop hand-off |
| mail-sh-compat | no | - | ? | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| mail-tidy-units | no | - | 2 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| mail-tidy | no | - | 15 | operator mail channel and ask flow: Maildir and lint behaviour, no loop hand-off |
| mail | no | - | 8 | operator mail channel and ask flow: Maildir and lint behaviour, no loop hand-off |
| maintenance-units-home | no | - | ? | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| migrate-ask | no | - | 6 | operator mail channel and ask flow: Maildir and lint behaviour, no loop hand-off |
| model-switch-report | no | - | 2 | cockpit/pane rendering and its probes: no loop actor changes state |
| noverdict-class | no | - | ? | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| ops-allowlist | no | - | 1 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| ops-closing | no | - | 128 | single-tool or static contract: no hand-off between loop actors |
| ops-read-model | no | - | ? | lifecycle store, schema and read models against a real Dolt: the store is the sim world's dependency, not its subject |
| ops-repo-orientation | no | - | 1 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| orphan-test | no | - | 2 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| overrides | no | - | 46 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| park-unmapped | no | - | 3 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| persona-model | no | - | 35 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| pilgrimage | no | - | 22 | operator mail channel and ask flow: Maildir and lint behaviour, no loop hand-off |
| plan-lint | no | - | 2 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| plan-matrix-merge | no | - | ? | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| plan-matrix | no | - | 27 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| pr-notify | no | - | 3 | operator mail channel and ask flow: Maildir and lint behaviour, no loop hand-off |
| pr-stall | no | - | 9 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| pre-activate | no | - | 4 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| pre-commit-release-path | no | - | ? | single-tool or static contract: no hand-off between loop actors |
| priority-bands | no | - | 5 | static, configuration or single-tool contract: no hand-off between loop actors |
| publish-backlog | partly | publish-then-cut | ? | backlog alarm needs a world with unpublished commits; threshold rows stay bash |
| pve | no | - | 4 | static, configuration or single-tool contract: no hand-off between loop actors |
| queue-flush | partly | long-gate-interleaving | 4 | flush opens a batch without waiting; the refusal rows stay bash |
| queue-land-local | replaceable | happy-path | ? | local round GREEN fast-forwards local/main and closes members |
| queue-open-batch | partly | new:forge-batch | ? | assemble/push/open-PR against the fake GitHub; real-conflict merges stay bash |
| queue-ops | partly | new:eject-mid-step + new:forge-batch | 12 | eject/abandon; refusal wording stays bash |
| queue-owner-mail | partly | new:forge-batch | ? | owner mutations mail the concierge; mail rendering stays bash |
| queue-owner-refuse | partly | new:forge-batch | ? | one writer per open batch; refusal text stays bash |
| queue-protect | no | - | 7 | static, configuration or single-tool contract: no hand-off between loop actors |
| queue-publish | replaceable | publish-then-cut | ? | publish PR from local/main commits, verdict settled apart |
| queue-sort-large | no | - | 6 | static, configuration or single-tool contract: no hand-off between loop actors |
| queue-step-eject-race | replaceable | new:eject-mid-step | 5 | `queue step` racing an operator eject on one repo |
| queue-submit | replaceable | happy-path, submit-from-other-cwd | 4 | `queue submit` certifying a branch from any cwd |
| queue-transition | partly | publish-then-cut + new:forge-batch | ? | to-forge move holds the lock and publishes; lock rows stay bash |
| queue-watch | no | - | 55 | static, configuration or single-tool contract: no hand-off between loop actors |
| ready-timers | no | - | ? | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| rebase-branch | no | - | 4 | single-tool or static contract: no hand-off between loop actors |
| rebase-escalation | replaceable | conflicting-siblings | 8 | a rebase loop tells the Concierge, naming the conflicting files |
| reclaim-slay-branch-guard | no | - | 24 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| reconciler-alert | no | - | 29 | lifecycle store, schema and read models against a real Dolt: the store is the sim world's dependency, not its subject |
| reconciler-flow | no | - | 34 | lifecycle store, schema and read models against a real Dolt: the store is the sim world's dependency, not its subject |
| reconciler | no | - | 33 | lifecycle store, schema and read models against a real Dolt: the store is the sim world's dependency, not its subject |
| ref-guard | no | - | 3 | static, configuration or single-tool contract: no hand-off between loop actors |
| release-workflow | no | - | 2 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| release | partly | cut-in-bare-env, publish-then-cut | 8 | cut names the beads landed since the previous tag; show/format rows stay bash |
| released-defects | no | - | 6 | cockpit/pane rendering and its probes: no loop actor changes state |
| render-memories | no | - | 13 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| reopen-queue-eject | replaceable | resubmit-moved-tip + new:eject-mid-step | 3 | ejected-suites sidecar survives a failed re-cert |
| repo-map | no | - | ? | static, configuration or single-tool contract: no hand-off between loop actors |
| resolve-output | no | - | 1 | single-tool or static contract: no hand-off between loop actors |
| review | no | - | 17 | single-tool or static contract: no hand-off between loop actors |
| round-duty | no | - | ? | static, configuration or single-tool contract: no hand-off between loop actors |
| round-vm-e2e | no | - | ? | static, configuration or single-tool contract: no hand-off between loop actors |
| rule-concierge-db-down | no | - | ? | static, configuration or single-tool contract: no hand-off between loop actors |
| runner-deps | no | - | 1 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| rust-toolchain-pin | no | - | 0 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| schema-apply | no | - | 161 | static, configuration or single-tool contract: no hand-off between loop actors |
| schema | no | - | 7 | static, configuration or single-tool contract: no hand-off between loop actors |
| scope-label | no | - | 5 | static, configuration or single-tool contract: no hand-off between loop actors |
| script-lint-rules | no | - | ? | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| script-running-guard | no | - | ? | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| sending-closed-reap | partly | new:reap-after-land | 14 | branch reaping after close; shapes needing hand-made branches stay bash |
| sending-metrics | no | - | 2 | cockpit/pane rendering and its probes: no loop actor changes state |
| sending-mode-filter | partly | new:reap-after-land | ? | skip-queue/queue-only sweeps; needs a mixed-mode world |
| sending | partly | new:reap-after-land | ? | disposition per branch; pure send_disposition rows stay bash |
| sentinel-pass | partly | happy-path | 9 | real end-to-end sentinel passes; audit dispatch rows stay bash |
| session-hook | no | - | 17 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| set-state-semantics | no | - | 4 | static, configuration or single-tool contract: no hand-off between loop actors |
| set-state-writers | no | - | 14 | static, configuration or single-tool contract: no hand-off between loop actors |
| sim-happy-path | already sim | happy-path | ? | already a sim suite (runs one scenario) |
| sim-world-lc | already sim | - | ? | already a sim suite (world up runs its own spira-lc) |
| sim-world-prebuilt | already sim | - | ? | already a sim suite (world up uses a prebuilt release) |
| sin-exempt | no | - | 66 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| single-selector | no | - | ? | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| skew-check-release | no | - | 3 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| skew-copies | no | - | 10 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| skew-escalate | no | - | 3 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| skew-foreign | no | - | 5 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| skew-local-release | partly | cut-in-bare-env | ? | skew under queue.local needs a landed-but-untagged world; skew logic stays bash |
| skew-refresh | no | - | 4 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| skip-allowlist-podman | no | - | ? | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| slay | no | - | 67 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| sop-gate-wired | no | - | 22 | static, configuration or single-tool contract: no hand-off between loop actors |
| sop | no | - | 32 | static, configuration or single-tool contract: no hand-off between loop actors |
| spike | no | - | 33 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| spira-config-cutover | no | - | 31 | lifecycle store, schema and read models against a real Dolt: the store is the sim world's dependency, not its subject |
| spira-config-only-door | no | - | ? | lifecycle store, schema and read models against a real Dolt: the store is the sim world's dependency, not its subject |
| statute-projection | no | - | 12 | static, configuration or single-tool contract: no hand-off between loop actors |
| strand-lock | no | - | 2 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| strand-submitted | partly | new:stuck-pipeline | ? | a submitted-only partition is not starved |
| strand-throttle | no | - | 2 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| strand-truncated | no | - | 5 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| stranded-runner | no | - | ? | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| submitted-lands | replaceable | happy-path | ? | submit → certify → land → close is the scenario's whole walk |
| suite-assert | no | - | ? | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| suite-select-cli | no | - | ? | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| suite-state-fence | no | - | 6 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| suite-state-rules | no | - | ? | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| summon-fast-path | partly | happy-path, long-gate-interleaving | ? | a freed slot refills in seconds; needs virtual-time assertion |
| summon-fayth | no | - | 4 | single-tool or static contract: no hand-off between loop actors |
| supersede-duty | replaceable | new:supersede | ? | supersede request adjudicated within one pass against a real spira-lc |
| superseded | partly | new:supersede | 24 | landing leaves superseded beads alone; sending reaps them |
| tarball-bins | no | - | 2 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| testdb-concurrent | no | - | 12 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| testdb-failsafe | no | - | 21 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| testenv-curl-bounds | no | - | ? | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| testenv-doctor-check | no | - | 2 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| testenv-guard | no | - | 1 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| testenv-systemctl | no | - | 1 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| testenv-wait | no | - | ? | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| testenv | no | - | 1 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| testlib | no | - | 2 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| thrash-teardown | no | - | 22 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| thrash | no | - | 3 | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| timeout | partly | new:red-gate-holds | 37 | lane-cap kill charges no attempt; needs a stub agent that overruns |
| timer-restart-safety | no | - | ? | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| timer-templates | no | - | 2 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| tmux-env | no | - | 1 | cockpit/pane rendering and its probes: no loop actor changes state |
| tokens | no | - | 2 | cockpit/pane rendering and its probes: no loop actor changes state |
| tsd-ingest | no | - | 12 | lifecycle store, schema and read models against a real Dolt: the store is the sim world's dependency, not its subject |
| tsd-lifecycle-export | no | - | ? | lifecycle store, schema and read models against a real Dolt: the store is the sim world's dependency, not its subject |
| tsd | no | - | 13 | lifecycle store, schema and read models against a real Dolt: the store is the sim world's dependency, not its subject |
| unclaimable-worktree | no | - | 6 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| unclaimable | no | - | 3 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| uninstall | no | - | 18 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| unit-binary | no | - | 1 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| unit-drift | no | - | 16 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| unit-name | no | - | 3 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| units-lint | no | - | 18 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| units-optional | no | - | 3 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| unpoison | no | - | ? | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| verdict-flow | no | - | 25 | operator mail channel and ask flow: Maildir and lint behaviour, no loop hand-off |
| verify-asks | no | - | 8 | operator mail channel and ask flow: Maildir and lint behaviour, no loop hand-off |
| verify-landed | no | - | ? | static, configuration or single-tool contract: no hand-off between loop actors |
| warden | no | - | ? | persona, brief or real-aeon session wiring: the stub agent does not run aeon, so the sim cannot see it |
| watch-notify-rearm | no | - | ? | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| watch-refresh | no | - | 7 | static, configuration or single-tool contract: no hand-off between loop actors |
| watchd-unit-name | no | - | 5 | static, configuration or single-tool contract: no hand-off between loop actors |
| watchtower-czar-outcome | no | - | 17 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| watchtower-failed-units | no | - | 10 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| watchtower-hotfix | no | - | ? | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| watchtower-lapse | partly | new:stuck-pipeline | 122 | lapsed-aeon section; needs a stub agent that stalls |
| watchtower-throttle | no | - | 14 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| watchtower | partly | new:stuck-pipeline | 107 | the pipeline detector reading the far end of its own queue |
| wiki-commit-dir-sweep | no | - | 1 | static, configuration or single-tool contract: no hand-off between loop actors |
| wiki-lint-links | no | - | ? | static, configuration or single-tool contract: no hand-off between loop actors |
| wire-token | no | - | 1 | cockpit/pane rendering and its probes: no loop actor changes state |
| withdrawn-suites-recert | replaceable | resubmit-moved-tip + new:red-gate-holds | ? | withdrawn-for-red re-certifies against the suites |
| work-container | partly | happy-path, submit-from-other-cwd | ? | the work client over a real socket; refusal/cannot-tell rows stay container |
| work-crate | no | - | ? | single-tool or static contract: no hand-off between loop actors |
| work-services-exclusion | no | - | 4 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| workflow-run-check | no | - | ? | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
| workspace-dist | no | - | 2 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| worktree-absent | no | - | 3 | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| worktree-branch-mismatch | no | - | ? | ops detector or janitor over its own fixture: fault and anomaly paths the sim excludes (no fault injection) |
| world-deadline-notice | no | - | ? | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| world-degraded | no | - | ? | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| world-drain-deadline | no | - | 3 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| world-timer-names | no | - | 4 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| world-timer-service-result | no | - | 4 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| world | no | - | 8 | install, activation, unit and packaging contracts: acceptance A–D and the unit lints own them |
| yield | no | - | 31 | gate engine, selector or test-infrastructure logic: tests the tooling, not the loop it guards |
