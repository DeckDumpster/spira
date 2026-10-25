# Sim batch 1: what the eleven suites pin that the existing scenarios do not

Finding for sp-21p9ee. The eleven suites do not map onto the existing scenarios by adding
`[[expect]]` queries: the scenarios walk the happy line of the loop (submit, certify, round,
land, tag, publish) and their `bead_state` view carries lifecycle state, tips and landstate
only. Every suite below pins something the world does not yet produce or observe. Nothing is
deleted by this bead.

Each migration must show its scenario costs less wall time than the suite it deletes, measured
after sp-wfcvbp (a world is ~65 s before it; `PROFILE.md`). Wall times are the recorded
seconds from the bead; `?` is unmeasured.

| suite | s | property it pins | scenario covers today | coverage the world lacks | new scenario needed |
|---|---|---|---|---|---|
| test-certified-withdraw | 18 | reopening a certified, unbatched bead withdraws it from the next cut's draw; recertifying a new tip readmits it; the submitted label is stripped for every cause but the admission-exempt one | `resubmit-moved-tip` (tip move on SUBMITTED) | a reopen step; an expect on the cut's certified list; label state in the view | `reopen-certified`: certify, reopen, assert the cut skips it, resubmit, assert it lands |
| test-rebase-escalation | 8 | a rebase loop mails the Concierge (never the operator) naming conflicting files, GENERATED flags, the base beads that changed them; no needs-ryan bead is filed | `conflicting-siblings` (loser returned to REWORK) | the mail the loser's rebase loop sends; mailbox contents as a queryable table; bd bead listing | extend `conflicting-siblings` with a mailbox table and the repeat-conflict threshold, or a `rebase-loop` variant with a generated file in the conflict |
| test-queue-submit | 4 | `queue submit` certifies any branch the batcher can adopt and refuses the rest (non `spira/<id>`, bead-less, `spira-suite-state/*`) before any state is written | `happy-path`, `submit-from-other-cwd` (accepted submit only) | refusals: exit code and "no row written" for each refused shape | `submit-refused`: one step per bad shape, `exit` expected non-zero, invariant that no row appears |
| test-landing-queue-early | 56 | a green forge verdict is landed at the top of the pass (`queue early:`), a pending one at the end (`queue late:`) | `long-gate-interleaving` (ordering of gate vs. landing) | a forge whose verdict changes mid-pass; the pass log as an event stream | `forge-green-early` and `forge-green-late`: scripted forge verdict flip, expect the landing seq before/after the gate event |
| test-landing-rebase | 64 | survivors are rebased when the base moves; the loop-defer guard (live aeon holds the bead) defers; the checkout refresh runs unconditionally; the escalate path reopens | `conflicting-siblings` (conflict resolution order) | a withheld branch rebased on a base move; a live-aeon hold; operator-checkout state (skew); the reopen count | `survivor-rebase`, `aeon-holds-bead` (needs an aeon-claim actor), and a skew probe on the checkout |
| test-certify | 89 | one gate per tip; PASS certifies without a local rebase; FAIL reopens and does not certify; NO_VERDICT neither certifies nor reopens; ten branches in one pass cost ten gate runs | `certify-twice` (repeat verdict refused), `resubmit-moved-tip` (re-gate on a moved tip) | gate verdict kinds FAIL and NO_VERDICT; a gate-invocation count; the fan-out of ten | gate-worker modes `fail` and `no-verdict`, an expect on invocations per tip, and a ten-branch scenario (the heaviest world; weigh it against the 89 s) |
| test-lifecycle-cert | ? | a GatePass whose tip mismatches is refused; moved tip on CERTIFIED returns to SUBMITTED with the gate key cleared; GateRed to REWORK; GateInfra stays SUBMITTED but advances the version; WORKING is auto-submitted before a verdict | `certify-twice`, `landstate-lifecycle-agree` | a tip-mismatched verdict; GateInfra; the row version and gate key in the view | gate-worker mode `infra`, a mismatched-tip step, and `lc_version` / `gate_key` columns in `bead_state` |
| test-submitted-lands | ? | a bead submitted with `work submit` lands and closes in push mode; a RED gate sends it to REWORK and the next aeon reclaims and resubmits; pr/hold mode closes a bead found landed off-box; a CERTIFIED row is still landed by the same pass | `happy-path` (queue.local only) | the non-queue modes (push, pr, hold); a red then a re-claim by an aeon; an off-box merge; bead close state | a repo-mode parameter in the world and four scenarios (`push-lands`, `red-then-reclaim`, `offbox-merge`, `certified-in-pass`) |
| test-queue-land-local | ? | a green round fast-forwards the landing ref, marks members LANDED, closes beads citing the commit, archives the head; a non-fast-forward head, a repo not in queue.local mode and a held per-repo lock are refused; `SPIRA_QUEUE_LOCK_HELD=1` skips the wait; a quarantine edit lands as a change bead | `happy-path` (the success path) | refusals and their exit codes; lock contention between two actors; bead close reason; the archive | `land-refused`: non-ff head, wrong mode, held lock, lock-held bypass; assert ref unchanged |
| test-land-local-release | ? | landing the harness builds, verifies and activates a release from the round's own corpus; no corpus is refused before anything changes; a failed build keeps `current` and the ref; no release in force skips activation; rollback re-activates and resets the ref; skew refresh only checks; an empty commit lands cleanly | `happy-path`, `cut-in-bare-env` (a build that succeeds) | build failure; absent corpus; rollback; skew match and mismatch; the empty-commit landing; the mock systemctl calls | `release-build-fails`, `release-rollback`, `release-skew`, and an empty-commit step (the first needs a failing build step in the cut actor) |
| test-queue-publish | ? | publish pushes local/main's new commits to one forge branch and opens one PR; green fast-forwards the forge to identical SHAs with no bead close; red files one fix-forward bead without reopening a member; a PR closed unmerged is never green; a diverged forge refuses, alarms the Concierge once, and never blocks a local cut | `publish-then-cut` (green publish) | PR states (open, red, closed unmerged); divergence; the alarm's mail; the fix-forward bead | `publish-red`, `publish-closed`, `publish-diverged`: a forge actor with scripted PR states and a foreign push |

Ordered by what to migrate first (suite seconds, then how little world each needs):

1. test-certify (89 s), test-landing-rebase (64 s), test-landing-queue-early (56 s): the
   largest recorded times. test-certify needs only gate-worker verdict modes. The other two
   need an actor each and are the least certain to beat their suite.
2. test-certified-withdraw (18 s), test-rebase-escalation (8 s), test-queue-submit (4 s): small
   suites. Migrate only if a scenario world costs less than the suite, which after sp-wfcvbp
   it may not for the 4 s one.
3. The five unmeasured suites: measure them first (`suites.sh list` reports the times once a
   run exists), then order by seconds.

Shared gaps across rows, worth building once: a gate-worker verdict mode per kind
(`fail`, `no-verdict`, `infra`); a scripted forge actor (PR state, verdict flips, foreign
push); a mailbox table and a bd bead table the invariants can query; an exit-code expect for
refused commands; a repo-mode parameter for the world.

## Retirement check (sp-twvl8e)

A suite is deleted only after its sim twin goes red on the defect the suite exists to catch. None
of the ten present suites qualifies, so none is deleted and no suite-seconds are removed.

No scenario has a reopen step, a non-zero expected exit, a gate verdict other than PASS, a
mailbox or bead-listing table, or a scripted forge, and those are what the suites pin. A defect
planted in the withdraw-on-reopen, refusal, FAIL/NO_VERDICT/GateInfra, rebase-mail, early-landing,
release-build or publish-state paths leaves every existing scenario green, so the twin cannot be
seen red. The "coverage the world lacks" column above is the work that must land first; each suite
is retired by the bead that adds its scenario and shows it red.

test-queue-publish is already gone (moved to the queue crate), so only ten of the eleven remain.

# Sim batch 2: two scenarios built and their suites deleted

The retirement rule is that the scenario was seen red on the suite's planted defect; the sim buys
realism and seed replay, not speed.

| suite | scenario | suite-seconds removed | red on the planted defect |
|---|---|---|---|
| test-supersede-duty | `supersede` | 2 | the duty supersedes on any successor row (`only_a_landed_successor_supersedes`, the bd-note check) |
| test-batch-claim-race | `claim-vs-batch` | 1 | IN_DELIVERY accepts a claim (`claim-vs-batch.sh` exit 9) |

The scenarios add 38 s to the sim lane (a world costs about 10 s before a scenario acts); the sim lane,
14 suites, ran 127 s inside a 160 s testenv wall.

Not built; each needs a world capability (the gap roadmap's D2, world parameters):

- `red-gate-holds`, `push-race` (test-landing 137 s, test-landing-race 33 s, test-landing-base-fail
  56 s): need a repo-mode parameter on the world (push mode with a bare remote a step can move) and a
  gate whose BASE_FAIL verdict a scenario decides.
- `eject-mid-step` (test-reopen-queue-eject, test-withdrawn-suites-recert): needs testenv inside the
  world, because `work submit` after an eject runs `testenv --suites` itself.
- `spira-claim reopen` was seen to die of SIGBUS once inside a sim container; not reproduced.

## Bead rows as data (sp-d5joss)

A scenario files beads with `[[seed]]` (persona or kind, priority, labels, body, deps on earlier
seeds; `{bead:<handle>}` in a `command` step is the id `bead.sh` allocated), and `bead_state` carries
`title`, `priority`, `labels` and `deps` (comma-joined, sorted), `close_reason` and `body`, read from
`bd list` on every snapshot of a scenario that seeds or sets `bead_rows = true`. A named step
(`name = ...`) is its own `actor` in `events`; a check step exits 0 when it held.

Moved, each retired after `bead-contract` went red on its planted defect (the refusals removed from
`bead.sh label` and `bead.sh amend`): test-bead-label, test-bead-amend.

Left, each blocked on more than a seed and a column:

| suite | what it still needs |
|---|---|
| test-bead-dep-add | an epic-typed seed and a raw `bd dep add` of a pre-existing edge; the incident label from config |
| test-destructive-bead, test-bead-file, test-bead-pilgrimage, test-express-lane | `bead.sh file` refusals asserted on stderr text and the lane guard's config |
| test-census*, test-livelock, test-deadlock-sweep, test-groomer*, test-unclaimable*, test-maechen-*, test-groom-trigger, test-watchtower-czar-outcome, test-cockpit-reachable | an actor row for the detector (G15) and a bead aged in the store |
| test-hold, test-unpoison, test-queue-ops, test-verify-landed | the lifecycle verbs and a deploy step (G14, G7) beside the seeds |

## Repository set and modes (sp-gqrlv6)

`sim world up <dir> --repo <name>:<mode>[:<trunk>]` (repeatable; also `sim run --repo`, and
`repos = [...]` in a scenario) registers repositories in any of push, pr, hold, queue,
queue.forge, queue.local. The world's own repository (`sim`) is queue.local unless named; any
other name is a repository of its own under `repos/<name>/` with the given trunk (`master`), and
a `file`/`claim` step routes a bead to it with `repo = "<name>"`. The probe holds each bead
against its own repository's refs.

Moved: the push-mode landing row of test-config-compat-master-base (a master-based repo; its
bash row is deleted) — scenario `repo-modes`, seen red when the push lands on `main` whatever the
base — and the push landing itself, `push-lands`, seen red when the push goes to the wrong ref.

Left, and why:

| suite | why it stays |
|---|---|
| test-config-compat-master-base (batcher cut, queue verdict rows) | a queue.forge cut and verdict on a master base need the forge round trip (G10) |
| test-landing-modes | its pr and queue sections need a forge that opens PRs and a certify step observed in the pass log (G10); its push section is the positive control that makes their silence mean something |
| test-land-mode-local | four of five properties are `lib.sh` function checks, not world behaviour; the fifth reads a git-call log |
| test-landing, test-landing-race, test-landing-base-fail | not G7 alone: G6 verdict kinds and G15 actors |
| the bead.sh, repo-map, held, cockpit and chamber label suites | they read the registry and the bead CLI, not a running loop; a world adds nothing they lack |
