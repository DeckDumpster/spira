# Test plan — Landing and merge queue (`landing-merge-queue`)

Part of [[test-plan-2026-09-23]], section 5. Area id `landing-merge-queue`; use-case ids are `UC-landing-merge-queue-NN`.

**Area id:** `landing-merge-queue`. **Primary files:** 51 suites (plus the three Rust crates that have no tests). **Secondary:** test-czar-shadow, test-pr-notify, test-timer-templates, test-verdict-timer.
**Current cost:** 1,506 suite-seconds on main push 35947142904. **Projected:** about 765 s (section 7).
**Sources:** mapper records in `map/*.jsonl`, reducer proposals r0 (landing-pipeline, merge-queue, ci-wait-gates, forge-integration), r1 (merge-queue-certification), r2 (red-attribution), r3 (merge-queue-assembly, merge-queue-verdict, forge-adapter), `signals.tsv`, and read-only greps of `spira/{landing,batch,verdict,queue,forge}.sh` and `lib.sh`.

---

## 1. Intent

This area turns a closed bead's branch into a commit on its repository's base branch. It does that without losing work and without saying something landed when it did not. `landing.sh` does most of this:

- In **push** mode, it lands a branch that passes its gate, and retries a lost push race.
- In **pr** mode, it opens one PR per branch and refreshes it when the base moves.
- In **queue** mode, it **certifies** a branch: it runs the gate and writes `CERTIFIED` or `RED` into landstate.

It reopens a bead only for a failure it can attribute to that branch: a gate fail on the branch, or a real conflict naming the file. It holds branches, and files one incident, when the base itself is red. It escalates instead of looping.

`batch.sh` builds certified branches into one batch PR when a trigger fires. The triggers are: the count reaches max, the oldest branch has waited long enough, CI is idle, or a bead carries the express label. Before it builds, it reconciles the landstate (already-in-base, orphan, stale certification, base conflict). `verdict.sh` then settles the batch against forge CI:

- **green on the same base:** it fast-forwards the base and marks the members `LANDED`
- **base moved or head SHA changed:** it requeues the members
- **harness or provision fault:** it reruns the workflow, within a retry budget
- **red:** it replays the red suites per member to eject only the guilty member, and halves the batch when no single member is guilty

All of this depends on the forge adapters (`forge.sh`, the broker, `pve.sh`). They must report `?` and never 0 when they cannot read an answer. They must also turn gh JSON into the one-line protocol exactly.

## 2. Use cases

The **Dim** tag uses the taxonomy's dimension ids: correctness (corr), fail-closed (fc), observability (obs), idempotency (idem), concurrency (conc), recovery (rec), config-compat (cfg), contract (ctr), performance (perf).

**Where** is one of:

- **cert**: the certification gate, meaning local and per-branch
- **batch**: batch CI
- **main**: main-push CI
- **nightly**: a proposed new slot on main that is not on the per-push critical path
- **accept**: `acceptance.yml`

Each use case is declared once, as the schema in docs/test-plan/README.md requires: the bracketed tier is the cheapest tier that would catch a regression; a fuller tier breakdown (when the design called for more than one) and the dim/where tags are carried in the trailing parenthetical rather than a second declaration.

### A. Certification (queue mode, landing.sh)

* `UC-landing-merge-queue-01` [T3] — A closed queue-mode branch whose gate passes is `CERTIFIED` at its tip. Main is unchanged and the bead stays closed. *(dim: corr, ctr; where: batch)*
* `UC-landing-merge-queue-02` [T1] — A certified tip is not gated again. A moved tip invalidates the record and is certified again. A tip that has not moved since its `RED` mark is skipped (CHECK6). *(tier: T1, a classifier over landstate + tip; dim: idem, corr; where: cert)*
* `UC-landing-merge-queue-03` [T3] — A gate FAIL or a failed rebase at certification reopens the bead and marks it `RED`. Main is unchanged. The reopen note (the tail of the gate output) names the offending file:line. *(tier: T3, one case; dim: fc, obs; where: batch)*
* `UC-landing-merge-queue-04` [T1] — Certification order is (priority ASC, closed_at ASC). In the phase-2 tiers, never-gated branches come before moved-tip `RED` branches. *(dim: corr; where: cert)*
* `UC-landing-merge-queue-05` [T1] — When the pass budget cannot fit a gate, the pass certifies nothing. It logs one budget cut naming the first deferred branch and the deferred count. Successive passes cover disjoint sets. *(tier: T1 for `gate_fits` + order, plus T2 for the submitted marker; dim: perf, obs; where: cert)*
* `UC-landing-merge-queue-06` [T3] — Gates run in parallel up to `SPIRA_CERTIFY_PAR`, and each branch is certified in completion order. *(dim: conc, perf; where: nightly)*
* `UC-landing-merge-queue-07` [T1] — When CI is idle and nothing is certified, a branch is certified as sole batch member without a gate. When CI is busy, or `?`, the gate runs. *(dim: corr, fc; where: cert)*
* `UC-landing-merge-queue-08` [T1] — With `SPIRA_GATE_SUITES=off`, certification is fences-only and uses a distinct gate cache key. Push mode never gets the switch. *(tier: T1 for the key, plus T2 for the env reaching the gate stub; dim: cfg, ctr; where: cert)*
* `UC-landing-merge-queue-09` [T1] — The gate lock wait defaults to 2 × `SPIRA_GATE_TIMEOUT`. An operator value is honoured. When the pass budget is shorter, the wait is capped and logged as rc=75 contention. *(dim: corr, cfg, obs; where: cert)*
* `UC-landing-merge-queue-10` [T2] — When the base is red (`BASE_FAIL`), branches are held, not reopened. The pass files exactly one incident per red base (with suite, repo, "no attempt charged", and gate output) and logs any recurrence. Held branches land once the base is green. *(tier: T2 for the hold/incident decision, plus T3 for one full pass; dim: fc, idem, rec, obs; where: batch)*
* `UC-landing-merge-queue-11` [T1] — A `basefail:<repo>:<suite>` bead is gated first, is exempt from the budget, and is certified while the base is red. Without one, nothing is certified. *(tier: T1 for selection, plus T3 for one case; dim: corr, rec; where: batch)*
* `UC-landing-merge-queue-12` [T1] — The watchtower admission throttle behaves as follows: at depth ≥ engage with drain, it writes the stamp and files an incident with a stable ref. On a stall, it files a fault instead. It lifts with hysteresis. The `off` override clears it, and a halted world skips it. Depth counts only live, unlanded `CERTIFIED` records. *(tier: T1 for the decision, plus T2 for the git depth filter; dim: corr, fc, idem; where: cert)*

### B. Push and PR land modes, rebase, and escalation (landing.sh, lib.sh)

* `UC-landing-merge-queue-13` [T3] — An uncontested branch lands by ancestry and advances the operator checkout. The pass records `bead.landed`. *(dim: corr, obs; where: batch)*
* `UC-landing-merge-queue-14` [T3] — A push rejected because the base moved is retried and lands the branch's own commits. The next pass does not land it again. A rejection that is not a race is not called a race, and neither case reopens the bead. *(tier: T3, via the pre-receive hook; dim: conc, rec; where: batch)*
* `UC-landing-merge-queue-15` [T3] — A pass reopens a bead only for a failure it can attribute: a failed gate (`bead.reopened`) or a real conflict naming the file. A parallel-duplicate note names the bead that already landed. *(dim: fc, obs; where: batch)*
* `UC-landing-merge-queue-16` [T2] — `rebase_branch` classifies `no-branch`, `no-base`, `conflict` and `rebase-refused` (keeping git's reason), and names the conflicting files. It succeeds without an ambient git identity by using `SPIRA_GIT_NAME/EMAIL`. Every arm in landing.sh reads `REBASE_FAILURE`. *(tier: T2, git only, plus T0 for the arm fence; dim: corr, cfg, ctr; where: cert)*
* `UC-landing-merge-queue-17` [T3] — A branch reaped or slain mid-pass, or between a race and its retry, is never reported as a conflict. The pass says whether the work is on the base. *(dim: rec, fc; where: batch)*
* `UC-landing-merge-queue-18` [T3] — After a landing moves the base, withheld survivors are rebased at once. A survivor that truly conflicts is reopened, naming the landing. A branch held by a live aeon is never rewritten or landed. *(dim: conc, rec; where: batch)*
* `UC-landing-merge-queue-19` [T2] — Repeated rebase failures escalate at `SPIRA_REBASE_ESCALATE_AT` instead of reopening, exactly once. A merge-conflict event is written once per (tip, base). *(dim: idem, rec; where: batch)*
* `UC-landing-merge-queue-20` [T2] — PR mode opens one PR per branch and posts once. It refreshes on base movement without opening a second PR. It leaves a MERGED PR alone. It never rewrites a branch when gh cannot say the PR state. Past the refresh cap it escalates once. *(tier: T2, a PR state machine over a gh fake + git; dim: corr, idem, fc; where: batch)*
* `UC-landing-merge-queue-21` [T3] — The pass skips a superseded closed bead's branch instead of reopening it. The other branches still land. *(dim: corr, rec; where: batch)*
* `UC-landing-merge-queue-22` [T2] — The bead scan issues one bulk `bd show`. A branch with no bead is never landed. A bead without a `repo:` label lands in the swept repo. A bead reopened during its gate is not landed and not reopened again. *(dim: perf, fc; where: batch)*
* `UC-landing-merge-queue-23` [T1] — The pass prunes verdict-cache entries older than `SPIRA_VERDICT_TTL` and reports a reused verdict. *(dim: perf, obs; where: cert)*
* `UC-landing-merge-queue-24` [T2] — `landing.sh halt` stops a running pass: it kills the pass, writes an interrupt record and removes the run state. It deletes orphan `spira/queue/*` branches and keeps the one named by the open record. Dry-run and idle forms exit honestly. *(dim: rec, obs; where: batch)*
* `UC-landing-merge-queue-25` [T2] — After a land, the post-land step rebuilds a cargo binary when it is absent and its unit is enabled. It never rebuilds without cargo. *(dim: rec, cfg; where: batch)*
* `UC-landing-merge-queue-26` [T0] — Every land mode declared in the header is handled in `land_repo`. `land_mark`, `land_state` and `land_mark_at` have one implementation (lib.sh). The landing worktree is never rebased. Every fetch uses `--no-write-fetch-head`. *(dim: ctr; where: cert)*

### C. Queue operations (queue.sh, lib.sh)

* `UC-landing-merge-queue-27` [T1] — `submit` accepts only `spira/<id>` and `spira-suite-state/*` branches. It runs the gate against the named repo (or the home repo) and certifies the branch with a queue record. A refusal writes no state. A gate fail writes no landstate. `suites.sh` quarantine/disable/activate each submit a transition branch. *(tier: T1 for name/repo selection, plus T2 for one transition; dim: fc, corr, cfg; where: cert)*
* `UC-landing-merge-queue-28` [T1] — `flush` runs the builder now with wait=0 and refuses push-mode and unknown repos. `step` runs the verdict before the batch. Landing's queue pass calls `queue.sh step`. *(dim: corr, ctr; where: cert)*
* `UC-landing-merge-queue-29` [T1] — `eject` writes `RED` at the tip, reopens the bead (clearing the assignee, with a comment), closes the PR, removes the record and returns the survivors to `CERTIFIED` at their tips. `abandon` does the same, except that it leaves `EJECTED`/`RED` members untouched, archives the record as `closed-pr<N>-<ts>Z` and passes `--reason` to the forge. Each refuses a non-member or a missing batch. *(tier: T1 for landstate/forge argv over a recording bd, plus T2 for one real-bd reopen; dim: corr, rec, obs; where: cert)*
* `UC-landing-merge-queue-30` [T2] — Every queue operation (eject, abandon, batch, verdict) refuses and changes nothing while another holds the per-repo lock. Stderr written after taking the lock reaches the caller. `--dry-run` prints the plan and changes nothing. *(tier: T2, a real flock; dim: conc, fc, obs; where: cert)*
* `UC-landing-merge-queue-31` [T1] — `queue_sort_rows` keeps every row when the priority JSON is over 128 KiB, orders by priority with suite-state transitions first, and fails open on bad JSON. *(dim: corr, perf, fc; where: cert)*
* `UC-landing-merge-queue-32` [T1] — `queue.sh protect` sets branch protection on the repo's base branch and writes a receipt. Doctor warns on a queue repo that has no receipt and is silent for push mode. *(dim: obs, cfg; where: cert)*
* `UC-landing-merge-queue-33` [T1] — `queue.sh stats` reports the local red rate, the batch and member counts, cost, and caught/escaped counts from the meter lines. *(dim: obs; where: cert)*

### D. Batch assembly (batch.sh)

* `UC-landing-merge-queue-34` [T1] — A batch is cut when any trigger fires: count ≥ `BATCH_MAX`, the oldest branch has waited ≥ `BATCH_WAIT`, CI is idle (`runs-active` = 0, with idle-cut on), or a member carries `SPIRA_EXPRESS_LABEL`. `?` and non-numeric answers count as busy. Otherwise the batcher waits. *(tier: T1, a predicate table; dim: corr, fc, cfg; where: cert)*
* `UC-landing-merge-queue-35` [T2] — An open batch record, a held flock, or an unrecorded open `spira/queue/*` PR prevents a second batch. The operator is mailed once. *(dim: conc, idem; where: cert)*
* `UC-landing-merge-queue-36` [T2] — An open batch PR reported DIRTY is abandoned: the PR is closed, the record removed, BATCHED members return to `CERTIFIED`, `RED` stays `RED`, and the operator is mailed. A CLEAN PR is left alone. *(dim: rec, obs; where: batch)*
* `UC-landing-merge-queue-37` [T1] — Before a batch, and even while one is open, landstate is reconciled: tip in base → `LANDED`; branch gone → `LANDED` if its tip is in base or it appears in reap.log, else `LOST`, never a member, with mail; a branch advanced after certification → a stale-certification log with both SHAs, and `LANDED` only when the live tip equals base; a closed bead with `RED`/`EJECTED` and a live branch → closed-red-live mail. Landstate records without a trailing newline are read. *(tier: T1 for the classifier, plus T2 for one git repo; dim: corr, rec, obs; where: cert)*
* `UC-landing-merge-queue-38` [T2] — Only a declared (`landed as` / `hand-landed`) or named citation that is an ancestor of base counts as landing evidence. A bare SHA does not. A base-conflicting branch without a valid citation, or with unlanded extra commits, is reopened, keeps its branch, and is stamped merge-conflict with the conflicting file named. Base movement without overlap rebases and batches. *(tier: T2, git + stub notes; dim: fc, rec; where: cert)*
* `UC-landing-merge-queue-39` [T2] — Members are ordered with suite-state transitions first. A member that conflicts with an earlier member is skipped and stays `CERTIFIED`. *(dim: corr; where: cert)*
* `UC-landing-merge-queue-40` [T2] — The local gate runs once on the combined tree, or is skipped with a log line when `SPIRA_QUEUE_LOCAL_GATE=0`. On red with a reproducing member, that member is ejected (`EJECTED`, QUEUE CAUGHT, PR comment) and the batch is rebuilt and gated again. On red with no reproducer, the PR still opens. A meter line is always written. *(dim: corr, obs; where: batch)*
* `UC-landing-merge-queue-41` [T1] — The PR body lists `id — title`, or `(title unavailable)`. The title says `beads for <repo>`. A declared formatter adds a format commit on the batch branch only, and a formatter failure never blocks the PR. *(tier: T1 for `format_batch`, plus T2 for the rest; dim: obs; where: cert)*
* `UC-landing-merge-queue-42` [T1] — A queue with no BATCHED/LANDED movement for longer than `SPIRA_QUEUE_STUCK_AGE` mails once and sets a flag. The flag clears when movement resumes, and the alert fires again on a new stall. Depth alone never alerts. A queue with no history is skipped, with a log line. *(dim: obs, idem; where: cert)*

### E. Verdict and red attribution (verdict.sh)

* `UC-landing-merge-queue-43` [T1] — With no open batch, the forge is not called. A pending run within CI max is left alone. A run past CI max (per-repo override honoured) with no recent activity is cancelled, not rerun. A run is aged by max(run start, step activity). *(tier: T1, an age classifier; dim: corr, fc, cfg; where: cert)*
* `UC-landing-merge-queue-44` [T1] — A harness fault or provision fault reruns the workflow and bumps retries, with no eject or bisect. When retries run out, the PR is closed, members return to `CERTIFIED`, and the operator is mailed once. *(tier: T1 for the decision, plus T2; dim: rec, idem; where: batch)*
* `UC-landing-merge-queue-45` [T3] — A green batch on an unchanged base fast-forwards. Members become `LANDED` and flaky annotations are recorded. The batch record and the batch branch (local and remote) are deleted. *(dim: corr, ctr; where: batch)*
* `UC-landing-merge-queue-46` [T2] — When a green batch's base has moved, the PR is closed and members return to `CERTIFIED`, or become `LANDED` when their tips are in the new base. When the CI head SHA differs from the sealed head, nothing is pushed and the operator is mailed. *(dim: fc, rec; where: batch)*
* `UC-landing-merge-queue-47` [T2] — For a red batch, the red suites are replayed per member, with an all-suites fallback and diff attribution limited to the member's own change. Only the guilty member is ejected, with the failing assertion in its note. Survivors are merged again onto the same PR. A together-only or unannotated red halves the batch. An unreproduced red requeues, and a second unreproduced red at the same tip ejects. *(dim: corr, rec, obs; where: batch)*
* `UC-landing-merge-queue-48` [T2] — Attribution is not fooled by flaky or odd input. A suite that fails and then passes on retry does not eject, and one that is always red does. A gate-classified red is not quarantined as flaky. A could-not-judge member (exit 2) does not exonerate a guilty one and is named in the mail. The ejected set is deterministic. *(dim: fc, corr; where: batch)*
* `UC-landing-merge-queue-49` [T3] — Replays run in parallel up to MAXPAR. A TERM signal logs `interrupted after`. *(dim: conc, perf, rec; where: nightly)*
* `UC-landing-merge-queue-50` [T0] — The verdict timer runs `spira-verdict.sh`, which calls `queue.sh step` for each repo. The service has `TimeoutStartSec` ≥ 3600 and no CPUQuota. *(dim: cfg; where: cert)*

### F. CI-wait gates and forge adapters

* `UC-landing-merge-queue-51` [T1] — CI park is classified from the land mode: in pr mode it is `watch` until `SPIRA_CI_PARK_MAX` and then `expired`; push, hold and unmapped repos are `no-ci`; an unparseable timestamp gives `watch` with rc 2; 0 disables the deadline; junk falls back to 5400. *(dim: corr, fc, cfg; where: cert)*
* `UC-landing-merge-queue-52` [T1] — `gate-check` discovers `--branch` once per distinct unbound-gate branch and never calls a bare discover or discovers for push repos. It then runs `gate check`. It reports a gate with no await_id as stuck. It parses ESCALATE lines for any `SPIRA_ID_PREFIX`. Its stderr is silent on a clean pass. *(tier: T1 over a stub bd; dim: corr, ctr, obs; where: cert)*
* `UC-landing-merge-queue-53` [T2] — bd's own gh:run gates behave as the harness expects: a gated bead leaves `ready`, and resolution is scoped to `metadata.repo`. *(tier: T2 contract; dim: ctr; where: on bd pin change)*
* `UC-landing-merge-queue-54` [T1] — `forge.sh` maps gh JSON to the line protocol for every verb Spira consumes: check-status (`pending`/`green`/`red`/`harness_fault`/`provision_fault`, `red-suite:`, `head-sha:`), runs-active (`?` never 0), run-metadata (max activity), workflow-rerun (never `--failed`, cancels in-progress), and also pr-mergeability, pr-list-queue, batch-ci-status, queued-since, run-id and pr-number. *(tier: T1, a recorded-fixture contract; dim: ctr, fc; where: cert)*
* `UC-landing-merge-queue-55` [T1] — The broker refuses verbs outside its policy table, and refuses czar-only verbs from any other fayth, without calling gh. Shadow mode records CZAR-WOULD. Act mode executes and audits. The read verbs shape gh argv exactly. *(tier: T1 via cargo `#[test]`, plus T2 for one bash smoke; dim: fc, ctr; where: cert)*
* `UC-landing-merge-queue-56` [T1] — `pve.sh` refuses to make an HTTP call when credentials or the CA are missing. It always passes `--cacert` and never `--insecure`, and routes each verb to its API path. *(dim: fc; where: cert)*
* `UC-landing-merge-queue-57` [T2] — When a bead with a GitHub ref lands, its issue is commented (short SHA + link) and closed once, with a marker. An unlanded bead's issue is never closed. Backfill decides landed-ness by ancestry of `spira: land <id>`. The unlanded scan asks once, logs refusals, and marks issues already closed on the forge. *(dim: idem, fc, obs; where: batch)*

---

## 3. Coverage map

The **ci_secs** column is per file, from signals.tsv. Where one file covers several use cases, its full cost is shown on its main row only.

| UC | Existing tests (file::case) | Level now / ci_secs | Verdict |
|---|---|---|---|
| 01 | test-certify::queue entry; test-cert-lint::clean certified | integration / 79 + 14 | KEEP test-certify (one case); cert-lint MERGE-INTO test-certify |
| 02 | test-certify::second pass same tip, tip move; test-landing-phase2-order::same-tip RED skip, control moved tip | integration / (79), 12 | DEMOTE-TO-T1 (classifier); keep one tip-move case in test-certify; phase2-order DELETE after T1 |
| 03 | test-certify::gate failure, rebase conflict; test-cert-lint::scratch reopened/RED/main unchanged/note names file; test-cert-gate-reopen-note::offender in tail-20 | integration / (79), 14, 2 | KEEP certify gate-failure; MERGE-INTO test-certify the note-names-file assertion; cert-gate-reopen-note SOURCE-GREP (it tests an in-test copy of `tail -20`) |
| 04 | test-landing-order::order (d<b<a<c); test-landing-phase2-order::tier order, tier log | integration / 29, (12) | DEMOTE-TO-T1 (sort + tier function over synthetic rows) |
| 05 | test-landing-order::budget cut logged, tight budget certifies nothing, pass 1/2 disjoint; test-landing-basefail-fix::budget cut for others | integration / (29), (57) | DEMOTE-TO-T1 (`gate_fits` + first-deferred); keep the disjoint-pass case at T2 |
| 06 | test-certify::parallel par=2, completion order par=2/par=1 | integration / (79) | KEEP, move to nightly (sleep- and wall-clock-sensitive) |
| 07 | test-certify::idle precondition, idle skip, busy | integration / (79) | DEMOTE-TO-T1 (idle-skip predicate); this removes the order dependency that failed main gates 35940444737 and 35940777548 |
| 08 | test-certify-suites-off::(all) | unit + grep / 6 | KEEP the behavioural half; SOURCE-GREP sections 3–4 (env -i block, `grep -c` of landing.sh call sites) → gate stub that records `SPIRA_GATE_SUITES` |
| 09 | test-landing-gate-wait::default/explicit/clamped | integration / 17 | DEMOTE-TO-T1: `gate_lock_wait` is pure arithmetic. 4 landing passes are spent to observe one integer |
| 10 | test-landing-base-fail::(all 13) | integration (server Dolt) / 34 | KEEP, switch to embedded testdb, and delete the ~70-line copied gate stub that is never used |
| 11 | test-landing-basefail-fix::fix certified (pass path, green path), control, budget bypass | integration / 57 | KEEP, shrink from 11 branches to 3 per scenario; the ×10 `nowant` loops add no coverage. Selection logic DEMOTE-TO-T1 |
| 12 | test-watchtower-throttle::(all) | component / 14 | DEMOTE-TO-T1 (decision table); keep the stale-filter git case at T2; the sentinel line-order grep is SOURCE-GREP |
| 13 | test-landing::control land; test-landing-race::control uncontested | integration / 249, 21 | KEEP test-landing; race control DELETE (redundant) |
| 14 | test-landing-race::race retried, lands original commits, no double land, non-race rejection; test-landing::race + reap | integration / (21), (249) | KEEP test-landing-race (owner of hook-based races) |
| 15 | test-landing::gate failure, real conflict, parallel duplicate, plain conflict note; test-landing-rebase::sweep reopens real conflict | integration / (249), 129 | KEEP in test-landing |
| 16 | test-landing-rebase::classify ×4, rebase-refused, conflict names file, identity ×2, fence arms read kind; test-git-identity::(all) | integration (server Dolt) / (129), 1 | DEMOTE-TO-T2 as a new git-only `test-rebase-branch.sh`; test-git-identity MERGE-INTO it, with its tautological "landing merge" half DELETED; fence → T0 |
| 17 | test-landing::reaped-landed, slain-with-work, race + reap; test-landing-race::missing landing worktree | integration / (249) | KEEP |
| 18 | test-landing-rebase::survivor sweep, sweep reopens, aeon-taken untouched, loop defers | integration / (129) | KEEP (T3, embedded testdb) |
| 19 | test-landing::escalate at threshold; test-landing-pr::merge-conflict event dedupe | integration / (249), (71) | KEEP escalate in test-landing; MERGE-INTO test-landing the dedupe case (unrelated to PR mode) |
| 20 | test-landing-pr::PR opened … escalate once | integration (server Dolt) / 71 | KEEP, switch to embedded testdb |
| 21 | test-superseded::plain lands, superseded not reopened, both closed | integration / 14 | KEEP (the sending half belongs to landed-audit-reaping) |
| 22 | test-landing::bulk scan, ghost branch, no repo label, reopened during gate ×2 | integration / (249) | KEEP; bulk scan could be a T1 stub-count test |
| 23 | test-landing::verdict TTL prune, reused verdict | integration / (249) | DEMOTE-TO-T1 (prune); keep the reused-verdict line |
| 24 | test-landing-halt::(all) | component / 11 | KEEP; DELETE the vacuous "positive control" (a string that greps itself) |
| 25 | test-landing-build::(all) | component / 2 | KEEP |
| 26 | test-landing-mode-map::(all); test-landing-race::fence rebase, fence fetch; test-landing-rebase::fence arms | static / 6, (21), (129) | move to T0 lint stage; mode-map's per-mode check is unscoped (any `pr)` arm in 2,138 lines passes) and must be scoped to `land_repo` |
| 27 | test-queue-submit::(all 7); test-submit::(all 6) | component / 10; integration / 5 | MERGE test-submit INTO test-queue-submit; table-drive the three copy-pasted transition blocks; drop the testdb (bead-less path) |
| 28 | test-queue-flush::(all) | component / 5 | KEEP as T1; SOURCE-GREP the `grep -A3` landing wiring check (replace it with a landing pass using a stub queue.sh that records `step`); assert the rc of the push-mode refusal |
| 29 | test-queue-eject::(all 17); test-queue-abandon::(all 13) | component / 14, 6 | MERGE-INTO a new `test-queue-ops.sh` (recording bd stub), plus 1 real-bd case for reopen/assignee/comment |
| 30 | test-queue-eject::lock held, dry-run; test-queue-abandon::lock held, dry-run; test-batch-lock::a–e | component / (14), (6), 25 | KEEP test-batch-lock as the lock owner with `BATCH_MAX=1` and its positive control deleted (duplicates test-batch case 1); the eject/abandon lock rows go in the merged table |
| 31 | test-queue-sort-large::(all) | unit / 14 | KEEP; profile it (14 s for a sourced function points to conf.sh startup cost) |
| 32 | test-queue-protect::(all) | component / 15 | DEMOTE-TO-T1 once doctor has a `--section` seam; add the failure path (gap G5) |
| 33 | test-batch::d (stats); test-attribution::5 (meter) | integration / (75), (115) | DEMOTE-TO-T1 (planted landing.log → `queue.sh stats`) |
| 34 | test-batch::1, 2 (max/wait); test-batch-express::(all); test-batch-idle-cut::(all) | integration / (75), 4, 9 | DEMOTE-TO-T1: one predicate table replaces express + idle-cut + batch 1/2; keep test-batch case 1 as the assembly control |
| 35 | test-batch::5 open batch blocks; test-batch-lock::a, b | integration / (75), (25) | KEEP in test-batch-lock |
| 36 | test-batch-conflicting-pr::(all) | integration / 4 | KEEP; drop the testdb (bd seeded but never asserted); CLEAN arm = test-batch::5 → DELETE there |
| 37 | test-batch::7, 8, A, B, landing-shaped records; test-batch-certified-landed::(all); test-batch-certified-orphan::(all); test-batch-closed-red-live::(all); test-batch-stuck::g; test-verdict::13 | integration / (75), 3, 4, 5, (16) | MERGE-INTO a new `test-batch-reconcile.sh` table (one git repo, no bd, one row per shape); DELETE the three single-shape files |
| 38 | test-batch-cited-commit::1–9; test-batch::g, h, i, m | integration / 11, (75) | cases 1–6 DEMOTE-TO-T2 (git + stub notes, no testdb); keep 7–9 + batch g/i/m as one conflict table |
| 39 | test-batch::transition first, conflict | integration / (75) | KEEP |
| 40 | test-batch::b, c, e, f, 9 | integration / (75) | KEEP |
| 41 | test-batch::j, k, l, n, o | integration / (75) | `format_batch` DEMOTE-TO-T1 (j, k); keep l, n, o |
| 42 | test-batch-stuck::(all 7) | integration / 16 | DEMOTE-TO-T1 (last-movement over a planted landstate dir) |
| 43 | test-verdict::1, 2, 3, 12, 14, 17, 18; test-forge-run-metadata::(all) | integration / 236, 5 | DEMOTE-TO-T1 (age/idle classifier over metadata lines); keep 1 and 3 at T2 |
| 44 | test-verdict::4, 5, 27 | integration / (236) | KEEP 5 at T2; 4 and 27 go into the T1 status→action table |
| 45 | test-verdict::6, 9, 11 | integration / (236) | KEEP (the only real fast-forward → LANDED check) |
| 46 | test-verdict::7, 10, 13 | integration / (236) | KEEP |
| 47 | test-verdict::8, 8.5, 15, 16; test-attribution::1–4, 6–15 | integration / (236), 115 | MERGE test-attribution INTO test-verdict (same forge-fixture/repro-stub harness); DELETE test-attribution::14 (= verdict::8); one shared testdb |
| 48 | test-verdict::19, 20, 24, 25, 26; test-attribution::13 | integration / (236), (115) | KEEP |
| 49 | test-verdict::21, 22, 23 | integration / (236) | KEEP, move to nightly; replace the 8 s sleeps with a concurrency counter (max simultaneous replays) plus 1 s stubs |
| 50 | test-verdict-timer::(all); test-timer-templates::(verdict rows) | static / 2, 10 | KEEP only the ExecStart chain and the limits in verdict-timer; DELETE its UNITS/_ENABLE_TMPL/periodic rows (subsumed) |
| 51 | test-ci-park::classifier table ×16 | integration / 24 | split: the table → T1 file (<1 s); the brief and ops-pane sections go to aeon-execution and cockpit |
| 52 | test-gate-discover-branch::(all); test-gh-run-gate::part 3; test-gate-check-stuck::(all); test-gate-check-stderr::(all) | unit / 1; integration / 7, 6, 3 | KEEP discover-branch; DELETE gh-run-gate part 3 (near-verbatim copy); stuck → T1 with canned gate JSON; stderr MERGE-INTO stuck |
| 53 | test-gh-run-gate::parts 1–2, 4 | integration / (7) | KEEP as a bd contract test, run on bd pin change; part 4 → T0 |
| 54 | test-forge-check-status; test-forge-run-metadata; test-forge-runs-active | component / 5, 5, 4 | MERGE into one `test-forge-contract.sh` over recorded JSON (no git init; add env -i to runs-active); extend it to the untested verbs (gap G1) |
| 55 | test-broker::(all); broker crate has no tests | component / 12 | DEMOTE-TO-T1 via cargo `#[test]` (policy table, intent serde, argv); keep one bash smoke; DELETE the "FAILS OPEN" tautology; SOURCE-GREP the conf/build greps → T0 allowlist lint |
| 56 | test-pve::(all) | unit / 3 | KEEP; fix the vacuous control; assert the clone poll |
| 57 | test-gh-issue-closeout::(all 12) | integration (server Dolt) / 33 | KEEP; switch to embedded testdb; stop copying all of spira/*.sh; fix case 4 (it passes on both branches) |

## 4. Duplicate clusters

1. **Red-batch bisect by halving.** Found in test-verdict::8 "red no suite: bisect halved" and test-attribution::14 "no suite annotations multi-member: bisect halved". Both files use the same `forge-fixture.sh` and `repro-*.sh` stubs, and each copies all of `*.sh` into a temp SPIRA_HOME (r2 and r3 both flag this). **Keep test-verdict::8**, and fold the rest of test-attribution into test-verdict under one testdb.
2. **Already-in-base → LANDED.** Found in test-batch::7, test-batch::B, test-batch-certified-landed (whole file), test-batch-cited-commit::9, test-batch-certified-orphan::orphan-in-base and test-verdict::13. The no-trailing-newline read bug (land_mark shape) is guarded in four places: test-batch "landing-shaped records", certified-landed, certified-orphan and test-batch-stuck::g. **Keep one row each** in a new `test-batch-reconcile.sh` table, and verdict::13, which is the verdict-side path.
3. **Branch gone → LOST / LANDED.** Found in test-batch::8 and test-batch-certified-orphan. **Keep the orphan file's cases as table rows.** test-batch-closed-red-live uses the identical fixture and log+mail assertion shape (its own mapper notes call it a "natural merge candidate"). Merge it into the same table.
4. **Eight certified branches → PR opened.** Found in test-batch::1 and the positive control of test-batch-lock (8 branches with `plant_bead` each, 25 s total). **Keep test-batch::1.** test-batch-lock sets `BATCH_MAX=1` and asserts a single PR.
5. **Batch trigger predicates.** Found in test-batch::1, test-batch::2, test-batch-express (3 batch.sh runs), test-batch-idle-cut (5 runs) and test-queue-flush::forces wait=0. The trigger logic is 25 lines at batch.sh:394–447. **Keep one T1 table.**
6. **Open-batch survivors → CERTIFIED, RED/EJECTED untouched, lock refusal, dry-run changes nothing.** Found in test-queue-eject, test-queue-abandon and test-batch-conflicting-pr (DIRTY abandon). test-queue-abandon's positive control and its "no open batch refuses" case are the same invocation. **Keep a merged `test-queue-ops.sh`.** batch-conflicting-pr keeps only the mergeability trigger.
7. **Gate FAIL at certification → reopen, RED, main unchanged; clean → CERTIFIED.** Found in test-certify and test-cert-lint. **Keep test-certify**, plus cert-lint's one unique assertion (the note names `sp-clnt-drt.txt`).
8. **Budget cut `MAXSEC=1 RESERVE=2` "budget cut at".** Found in test-landing-order and test-landing-basefail-fix. **Keep a T1 on `gate_fits`.** basefail-fix keeps only the "base-fix exempt from budget" row.
9. **Certification ordering.** Found in test-landing-order, test-landing-phase2-order and test-certify::completion order. **Keep a T1 on the ordering function**, and keep test-certify completion-order in nightly.
10. **Uncontested land + skew refresh.** Found in test-landing::control, test-landing-race::control and queue repo checkout advanced, and test-landing-rebase::refresh ×4. **Keep test-landing::control.** The refresh cases move to the skew suite (config-store area). The race file keeps only race cases.
11. **rebase_branch committer identity.** Found in test-git-identity and test-landing-rebase::identity ×2. **Keep one git-only T2 file.** The "landing merge" half of test-git-identity runs plain `git -c user.name=… merge` supplied by the test itself, so it is tautological. DELETE it.
12. **Escalation at threshold / db-91ox guard.** Found in test-landing::escalate at threshold and test-landing-pr::merge-conflict event dedupe. **Keep both assertions in test-landing** (dedupe is a push-mode concern that was bundled into the PR file).
13. **gate-check discover `--branch`.** test-gh-run-gate part 3 is a near-verbatim copy of test-gate-discover-branch (same stub bd, same asserts). **Keep discover-branch** (1 s).
14. **gate-check clean pass on real bd.** Found in test-gate-check-stderr and test-gate-check-stuck part 2. **Fold the stderr-silent assertion into stuck.**
15. **Forge parser vs consumer.** check-status, run-metadata and runs-active each have their own file with the same gh-stub harness. **Keep one contract file.** Consumers (verdict, attribution, idle-cut) keep their stand-in forges.
16. **Timer install/enable/periodic.** test-verdict-timer's UNITS/_ENABLE_TMPL/OnUnitActiveSec rows repeat test-timer-templates. **Keep timer-templates** for those rows, and verdict-timer for its specific limits.
17. **Submit.** test-submit and test-queue-submit both drive `queue.sh submit`. test-submit's three transition blocks are copies of the same 4 assertions, and test-queue-submit cases 2 and 5 are near-duplicates. **Merge them into test-queue-submit** as one table.

## 5. Unit-extractable logic

| Logic | Where it lives | Tested today through | Seam that enables a T1 test |
|---|---|---|---|
| `gate_lock_wait`, `gate_fits` | landing.sh:299–333 | test-landing-gate-wait (4 full passes, 17 s); landing-order; basefail-fix | They sit **below** the source guard at landing.sh:97, so `. landing.sh` returns before defining them. Move them (with `PASS_START`/`LAND_MAXSEC` as parameters) into lib.sh or above the guard. *(Done by sp-ulr4e: both now take their globals as optional parameters and are defined above the guard.)* |
| Certification order and phase-2 tiering (priority, closed_at, never-gated vs moved-RED vs same-tip-RED skip) | landing.sh `land_repo` inline | test-landing-order (29 s), test-landing-phase2-order (12 s) | Extract `certify_order <rows>` and `certify_tier <landstate-line> <tip>` as functions over text rows. |
| Idle-skip decision and tip-move invalidation | landing.sh certification path | test-certify (79 s, and order-dependent) | Extract `certify_needs_gate <landstate> <tip> <runs-active> <certified-count>`. |
| Base-fix selection and budget exemption | landing.sh `_basefail_fix_check` + selection loop | test-landing-basefail-fix (57 s, 11 worktrees × 4) | A sort key function over bead JSON (`external_ref`). |
| Verdict-cache TTL prune | landing.sh | test-landing (249 s pass) | Extract `verdict_cache_prune <dir> <ttl>`. |
| Batch trigger (max/wait/idle/express) | batch.sh:394–447 inside `main` | test-batch 1–2, test-batch-express, test-batch-idle-cut (≈9 batch.sh runs on testdb) | batch.sh ends in a bare `main "$@"` (line 827) with no source guard. Add `[[ ${BASH_SOURCE[0]} == "$0" ]] && main "$@"` and extract `batch_should_cut <count> <age> <runs-active> <prio_json>`. *(Done by sp-ulr4e: source guard added; the predicate is three functions — `batch_cut_reason_cheap`, `batch_cut_idle`, `batch_cut_express` — to preserve the "ask the forge/bd last, only when it can change the answer" short-circuit the original inline code depended on.)* |
| Stuck-queue check | batch.sh:~355–391 | test-batch-stuck (16 s, 8 batch.sh runs with worktrees) | Extract `queue_last_moved <landstate-dir>` and the flag/threshold function, then plant files. *(Done by sp-ulr4e: `queue_last_moved` and `queue_stuck_action`.)* |
| Landstate reconciliation (in-base, orphan, reaped, stale-cert, closed-red-live) | batch.sh `_certified_orphans`, `_closed_red_live`, reconcile loop | 5 files, each with testdb + 2 repos | Same source guard. Take `bead_status` as an injectable function so bd is not needed. |
| `format_batch` PR body | batch.sh:23 | test-batch j, k | Source guard. It is already a function. |
| `bead_cited_commit_on_base` | lib.sh:3054 | test-batch-cited-commit (testdb only because the notes are in bd) | Pass the notes text in, or stub `bdq show`. One tiny git repo is enough. |
| CI age / idle / retry classification, status→action | verdict.sh:630–700 inside `main` | test-verdict 1–5, 12, 14, 17, 18, 27 | Same `main` source guard. Extract `verdict_action <status> <started> <last-activity> <maxsec> <retries>`. *(Done by sp-s088v.14: source guard, `verdict_action` classifier, and the `_verdict_process_attributing` split.)* |
| Red attribution member selection (`_any_suite_in_selection`, `_suite_directly_in_diff`) | verdict.sh:147–170 | test-attribution 6–10 | Already functions. They need the source guard and a git repo only (T2, no bd). |
| `queue.sh` eject/abandon planning | queue.sh:234–440 | test-queue-eject, test-queue-abandon (real bd seeded, never read in abandon) | `SPIRA_BD` pointed at a recording stub. Keep one real-bd row. |
| Doctor queue-protection check | doctor.sh repositories section | test-queue-protect (3 full doctor runs, 15 s) | Needs a `doctor.sh --section repositories` selector (r2 cross-cutting: 16 suites pay for this). |
| Throttle decision | watchtower.sh `--throttle-check` | test-watchtower-throttle (17 invocations) | Extract `throttle_decide <depth> <since_land> <stamp> <engage> <release> <override>`. |
| Broker policy, intent serde, verb→argv | broker/src/{policy,submit,read,execute}.rs | test-broker (bash, may run cargo build in CI) | Plain `#[cfg(test)]` modules. No CI workflow runs `cargo test` today (mapper: gate.yml has none); add it to certification. |
| forge.sh JSON parsing | forge.sh verbs | 3 component files with an unnecessary `git init` | Feed recorded JSON through `SPIRA_GH` in an empty dir. Already near T1. |
| `spira_ci_park_state` | lib.sh:4216 | test-ci-park (pure table, but the file pays 24 s for the aeon and cockpit halves) | Split the file. The function is already pure. |

## 6. Gaps

- **G1. Most forge.sh verbs have no parser contract test.** Only `check-status`, `run-metadata`, `runs-active` and `workflow-rerun` run the real parser (the only files that set `SPIRA_GH`). `pr-mergeability` (DIRTY/CLEAN, forge.sh:57), `pr-list-queue` (:44), `batch-ci-status` (:184), `queued-since` (:241), `run-id` (:178), `pr-number` (:39), `run-cancel`, `branch-protect` and `branch-protection-status` (:381, referenced by no test) are exercised only through `forge-fixture.sh` stand-ins. test-forge-check-status exists because this exact shape hid a "stray brace" parser bug. **Need:** recorded-JSON rows per verb, including a `?` row for API failure. *(Deferred — see the follow-up beads filed by sp-ulr4e.)*
- **G2. A missing head SHA fails open in verdict.sh.** At verdict.sh:770 the check is `[ -n "${ci_head:-}" ] && [ "$ci_head" != "$batch_head" ]`, so when check-status omits `head-sha:` (headRefOid absent is a tested *producer* case in test-forge-check-status), the sealed-head guard is skipped and a green batch fast-forwards unchecked. test-verdict::10/11 cover only mismatch and match. **Need:** a row for green with no head-sha. Decide whether that should refuse (fail-closed dimension) or is intended. *(Closed: main now refuses — test-verdict.sh case 33, "GREEN, CHECK-STATUS OMITS HEAD-SHA", holds the batch for retry rather than fast-forwarding. sp-g68ek, filed to record that fail-closed-vs-intended decision, stays open to carry the decision itself.)*
- **G3. No test runs with a `master` base branch.** No suite in this area contains `master`. CLAUDE.md lists "the base branch is not always main" as fixed four times before it held, and 3 of 7 repos use it. **Need:** one config-compat row each for batch.sh (merge onto `spira_landref`), verdict.sh fast-forward, and landing push, with base `master`. *(Deferred — see the follow-up beads filed by sp-ulr4e.)*
- **G4. The `spira: land <id>` commit message is an untested seam.** It is written at batch.sh:485/539/683, verdict.sh:540 and landing.sh:1660, and read by gh-issue backfill (UC-57) and by the landed-audit CHECK 5 commit search. No test asserts the writer and reader against one fixture. **Need:** a contract test (CLOSED ≠ LANDED depends on it). *(Deferred — see the follow-up beads filed by sp-ulr4e.)*
- **G5. The failure path of `queue.sh protect` is untested.** When the forge exits non-zero there must be no receipt and a named error (test-queue-protect mapper note). *(Deferred — see the follow-up beads filed by sp-ulr4e.)*
- **G6. `queue.sh submit` with a failed queue-dir write.** The test-queue-submit header promises that this must not print `certified`, but no case exists. *(Deferred — see the follow-up beads filed by sp-ulr4e.)*
- **G7. `queue.sh abandon` bead side effects.** The testdb is seeded but no bead state is read back, so whatever abandon does to beads is unverified. *(Deferred — see the follow-up beads filed by sp-ulr4e.)*
- **G8. A bd failure while batching is invisible.** At batch.sh:422, `prio_json="$(bdjson show …)" || prio_json="[]"` means a bd outage silently disables express and priority ordering. test-queue-sort-large covers the sort's own fail-open, but not batch.sh logging it (observability dimension). *(Deferred — see the follow-up beads filed by sp-ulr4e.)*
- **G9. Real container teardown in `landing.sh halt`.** Teardown is asserted only in the dry-run listing, because the registry is emptied before the real halt (test-landing-halt note). *(Deferred — see the follow-up beads filed by sp-ulr4e.)*
- **G10. The source-changed trigger in `land-build-ensure.sh`** (reflog `@{1}`) is deliberately neutralised and untested.
- **G11. The Rust crates `broker`, `czar-pass` and `supervise` have zero `#[test]`**, and no workflow runs `cargo test`. The broker's refusal of czar-only verbs is a security fence tested only through a bash suite that contains a tautology ("FAILS OPEN"). *(Deferred — see the follow-up beads filed by sp-ulr4e.)*
- **G12. The queue-mode repo map is missing in test-landing-race's early cases.** They never write `$SH/repo-map`, so they pass through the `SPIRA_REPO` fallback. The explicit-map path of push-mode races is untested.
- **G13. Concurrency between landing and a queue operation on one repo.** Each lock is tested alone (batch, verdict, eject, abandon). Nothing runs a landing pass's `queue.sh step` against an operator `eject` at the same moment to show that exactly one wins with a consistent landstate. Propose one T3 nightly row.
- **G14. No test uses a standard reporting format.** Five or more summary wordings appear in this area: `N passed, M failed`, `results:`, `<file>: …`, the em-dash variant in test-broker and test-landing-mode-map, and `ASSERTIONS N` in test-submit. Skip handling also varies: six server-Dolt files exit 77, others exit 1 on a testdb failure. This is a cross-area gap, but it hides this area's largest suites. *(Cross-area; unaddressed by this slice — the two new T1 files source testlib.sh from the start, but the pre-existing suites in this area are untouched.)*

## 7. Cost

**Current** (sum of `ci_secs` for the 51 primary files; the crate row has no timing):

| Group | Files (ci_secs) | Sum |
|---|---|---|
| Verdict + attribution | verdict 236, attribution 115 | 351 |
| Batch | batch 75, lock 25, stuck 16, cited-commit 11, idle-cut 9, closed-red-live 5, conflicting-pr 4, express 4, certified-orphan 4, certified-landed 3 | 156 |
| Certification | certify 79, queue-early 59, basefail-fix 57, base-fail 34, landing-order 29, gate-wait 17, cert-lint 14, phase2-order 12, certify-suites-off 6, cert-gate-reopen-note 2 | 309 |
| Landing core | landing 249, rebase 129, pr 71, race 21, superseded 14, halt 11, mode-map 6, build 2, git-identity 1 | 504 |
| Queue ops | protect 15, eject 14, sort-large 14, queue-submit 10, abandon 6, flush 5, submit 5 | 69 |
| CI-wait gates | ci-park 24, gh-run-gate 7, gate-check-stuck 6, gate-check-stderr 3, discover-branch 1 | 41 |
| Forge / broker / closeout / throttle | gh-issue-closeout 33, broker 12, throttle 14, check-status 5, run-metadata 5, runs-active 4, pve 3 | 76 |
| **Total** | | **1,506** |

The biggest single lever is the **server-Dolt pin**. Six files export `SPIRA_TESTDB_MODE=server` and together cost 249 + 129 + 71 + 59 + 34 + 33 = **575 s**, 38% of the area. None of them states why. The evidence that the fixed cost is large: test-landing-queue-early takes 59 s for 2 landing passes over one branch, while test-landing-order, on the embedded store, takes 29 s for 4 passes over 16 branches.

**Projected, after the verdicts in section 3.** All estimates assume embedded testdb in place of server, about 5 s per landing pass on embedded (from landing-order: 29 s / 4 passes, with worktrees), and under 1 s per T1 file.

| Group | Projection and arithmetic | Projected |
|---|---|---|
| Verdict + attribution | Merge attribution into verdict with one shared testdb (−~40 of 115); drop attribution::14; timing cases 21–23 (≈50 s of 8 s sleeps ×3 ×2) to nightly with 1 s stubs (−~40); status/age rows to T1 (−~20). 351 − 40 − 40 − 20 − 51 (shared fixture and copy-of-*.sh removed) | 200 |
| Batch | T1 trigger table 1 + T1 stuck 1 + reconcile table (T2, one repo, no bd) 3 + cited-commit (T2) 5 + batch-lock (`BATCH_MAX=1`) 8 + conflicting-pr (no testdb) 4 + test-batch after its trigger, stats, reconcile and format cases leave (≈25 → 15 cases) 45 | 67 |
| Certification | gate-wait T1 1 + order T1 + one disjoint case 8 + phase2 T1 1 + basefail-fix (3 branches) 15 + base-fail (embedded) 15 + cert-lint merged into certify 0 (+2 there) + certify (idle to T1, timing to nightly) 60 + suites-off 5 + reopen-note 0 + queue-early (embedded, 2 passes ≈ 10) 10 | 117 |
| Landing core | landing (embedded ≈ 20 passes × 5 s + setup ≈ 110, plus merged dedupe case +5, minus TTL/bulk lifts) ≈ 150 + rebase-branch T2 3 + rebase survivors/defer T3 embedded ≈ 40 + refresh cases moved out (counted in skew) 10 + pr (embedded, 13 passes) 35 + race (fences to T0) 18 + superseded 14 + halt 11 + build 2 + mode-map at T0 1 + git-identity 0 | 284 |
| Queue ops | queue-ops (eject + abandon, stub bd + 1 real) 10 + flush 3 + protect T1 4 + sort-large (after conf.sh profiling) 3 + submit merged 8 | 28 |
| CI-wait gates | ci-park area share 22 (the T1 table < 1 s; the brief and pane stay until those areas split them) + gate-check-stuck T1 2 + discover 1 + gh-run-gate contract 5 | 30 |
| Forge / broker / closeout / throttle | forge-contract (3 files → 1, plus new verbs) 5 + pve 3 + broker (cargo test + smoke) 6 + gh-issue-closeout (embedded, no *.sh copy) 12 + throttle T1 + 1 git case 5 | 31 |
| New T1 files (≈10 at < 1 s) | 10 × 0.8 | 8 |
| **Total** | 200 + 67 + 117 + 284 + 28 + 30 + 31 + 8 | **≈765** |

That is **1,506 → ≈765 suite-seconds (−49%)**. Separately, about 90 s of timing-sensitive cases (verdict 21–23, certify par/completion-order) move off the per-push path into nightly, where they cost the same but no longer gate a push or flake it.

**Verdict counts:**

- **DELETE or MERGE: 17 files.** attribution, cert-lint, cert-gate-reopen-note, submit, queue-abandon, git-identity, gate-check-stderr, batch-certified-landed, batch-certified-orphan, batch-closed-red-live, batch-express, batch-idle-cut, landing-phase2-order, forge-run-metadata, forge-runs-active, landing-gate-wait (all absorbed into T1 tables or into siblings), plus gh-run-gate part 3.
- **DEMOTE-TO-T1: 14 logic units** (section 5 rows, excluding the T2 ones).
- **SOURCE-GREP to replace: 7.** cert-gate-reopen-note, certify-suites-off §3–4, queue-flush wiring, throttle sentinel order, broker conf/build greps, landing-mode-map unscoped arms, gh-run-gate part 4.

---

## 8. Slice status (sp-ulr4e)

This area's plan is large enough that sp-ulr4e's brief explicitly allows landing it in
slices, and most of it landed through sibling beads under [[test-plan-2026-09-23]]
(sp-s088v) rather than through sp-ulr4e's own branch — sp-s088v.14 through .17 landed on
main while this branch was carrying an unrelated multi-day rebase, and between them cover
most of sections 3–6: the verdict.sh/attribution merge and T1 classifier split (UC 43-49),
the landing-core embedded-testdb flip and merges (UC 01-26), the queue-ops/batch-reconcile
merges and gaps G5-G8 (UC 27-33, 34-42), and the forge/broker contract tests plus rust
`#[test]`s (UC 51-57, gap G11). Gap G2 (a green batch with a missing head-sha) is already
fixed fail-closed on main (test-verdict.sh case 33) rather than merely pinned — that landed
ahead of sp-g68ek, the bead this plan filed to decide it, which stays open to record the
decision itself.

What sp-ulr4e's own branch landed, on top of that already-landed work:

- A source guard on batch.sh (verdict.sh's own guard, and its
  `_verdict_process_attributing` split, arrived already on main via sp-s088v.14 — this
  branch's original copy of that hunk was dropped as redundant during the rebase).
- Two of the section 5 seams extracted and T1-tested: the batch trigger predicate plus the
  stuck-queue check (`batch_cut_reason_cheap`, `batch_cut_idle`, `batch_cut_express`,
  `queue_last_moved`, `queue_stuck_action` — UC-34, UC-42), and `gate_fits`/`gate_lock_wait`
  (UC-05, UC-09), each paired with a new T1 suite (test-batch-trigger.sh,
  test-landing-gate-fits.sh) that drives the functions directly with synthetic inputs.
  `queue_stuck_action` grew a 5th, optional parameter (`stall-from`) to carry sp-w4tyd's
  stall-clock adjustment, which landed on main after this seam was first designed.
- This page itself.

**Not done, and why:** the original slice also planned to trim test-batch-express.sh,
test-batch-stuck.sh and test-landing-gate-wait.sh down to their non-predicate cases and
delete test-batch-idle-cut.sh and test-batch.sh, folding the predicate cases into the new
T1 files. That did not happen here: by the time this branch's rebase-conflict history
resolved, main had already decomposed test-batch.sh into over a dozen focused files
(sp-z2wsr, "round 17" of an unrelated minification pass), and test-batch-express.sh /
test-batch-stuck.sh / test-landing-gate-wait.sh / test-batch-idle-cut.sh still exist in
their pre-plan integration shape. Re-triaging the merge/delete verdicts against that
current file set is real work, not a rebase fix, so this slice leaves those suites
untouched (still covering the same behaviour at their original tier) and adds the T1
files alongside them rather than replacing anything. That re-triage is filed as sp-lxoyd.

Measured in testenv (isolated `testenv-batch.sh` runs against this branch):

| Suite | Seconds |
|---|---|
| test-batch-trigger.sh (new) | 1s |
| test-landing-gate-fits.sh (new) | 1s |

Nothing existing was demoted or deleted in this slice, so there is no "before" figure for
it beyond the projection in section 7 — that reduction is unlocked by the re-triage noted
above, not by this commit.

- sp-s088v.14 — landed: verdict.sh + attribution merge and T1 classifier extraction (UC 43-49)
- sp-s088v.15 — landed: landing core: embedded-testdb flip (server-Dolt pin) + merges (UC 01-26)
- sp-s088v.16 — landed: queue-ops + batch-reconcile merges (UC 27-33, 34-42), gaps G5-G8
- sp-s088v.17 — landed: forge/broker contract tests + rust `#[test]`s (UC 51-57), gaps G1/G11
- sp-s088v.18 — open: verdict-timer dedup, remaining gaps G3/G4/G9/G10/G12/G13/G14
- sp-lxoyd — open: re-triage the batch.sh/test-batch.sh minification verdicts against the post-round-17 file set
