# Test plan — Aeon execution and failure accounting (`aeon-execution`)

Part of [[test-plan-2026-09-23]], section 5. Area id `aeon-execution`; use-case ids are `UC-aeon-execution-NN`.

Scope: one bead inside one `aeon.sh` run (claim → worktree → briefs → agent → heartbeat/lease/thrash → teardown disposition), the attempt accounting that disposition feeds (`session_outcome`, `outcome_charges`, `attempts_of`/`requeues_of`/`reopens_of`), and the terminal valve: sentinel CHECK 4 (poison, requeue cap, reclaim cap). `slay.sh` is included because its `.slain` marker is one of the teardown inputs. 37 primary files; 35 ran on main push 35947142904; `test-aeon-heartbeat.sh` and `test-aeon-launch-grammar.sh` did not run (NA).

Sources: mapper records `map/*.jsonl` (37 files), reducers r1 (`aeon-session-lifecycle`, `failure-accounting`), r2 (`aeon-outcome-and-intervention`), r3 (`session-outcome-and-poison`), `signals.tsv`, and read-only greps of `spira/aeon.sh` (2347 lines), `spira/lib.sh`, `spira/sentinel.sh`. Line numbers below are from the harness checkout on 2026-09-23.

---

## 1. Intent

An aeon claims exactly one dispatchable bead, cuts a worktree for it from a freshly fetched base in the repository its `repo:` label names, and gives the agent a system prompt and a task brief. The brief carries the resume, slain, deadline and already-done context, and none of it is left as an unrendered `{{placeholder}}`. While the session runs, a lease renews on trace growth and lapses on silence. A thrash wall trips only when the deliverable has stopped moving and the session itself is older than the wall. Both paths kill the whole process group. At exit the aeon decides one disposition for the bead and records it in three places: the bead note, the ledger `done` line (with spend fields that are `?`, never 0, when unknown) and the process exit code. The possible dispositions are: stays closed, reopened by a close guard, released without charge, requeued by the harness, or charged an attempt. **Only a session that ran to its own end and left the bead open is charged.** Capacity loss, slay, thrash, the gate still running, decision or operator waits, timeouts, harness requeues and unjudged deaths are all free. Attempts are counted from the bd events trail, never from labels. The sentinel poisons a bead at `POISON_AT` and asks the operator once per attempt count. It escalates a requeue or reclaim cap once per bead, and it clears stale poison.

## 2. Use cases

Dimensions: C=correctness, FC=fail-closed, O=observability, I=idempotency, Cc=concurrency, R=recovery, CF=config-compat, K=contract, P=performance, TI=test-integrity.
Where it runs: **cert** = certification/every commit, **batch** = batch CI, **main** = main-push CI, **acc** = acceptance, **doctor** = `doctor.sh` preflight on the box.

| ID | Requirement | Dimensions | Tier | Runs |
|---|---|---|---|---|
| UC-aeon-execution-01 | A bead is worked in a worktree cut from freshly fetched `origin/<landref>` (never stale local main) of the repo its `repo:` label maps to. The `spira/<id>` branch and the commit live only in that repo, and a stale foreign worktree at the path is moved aside, never removed. | C, R | T3 (one e2e case) + T2 for `worktree_evict_foreign` | batch |
| UC-aeon-execution-02 | A worktree-creation failure is a pre-session death. FATAL carries git's own error, an attempt is charged, the ledger reads `status=pre-session` with rc≠0, and each repeat charges again (no free resummon loop). | C, FC, O | T3 | batch |
| UC-aeon-execution-03 | A bead found to carry `spira-poison` right after the claim is released before workspace setup, with ledger `poison-raced` (aeon.sh:430-446). | FC, Cc | T1 | cert |
| UC-aeon-execution-04 | World-stop fence. An unlabelled bead never calls `world.sh`. A `world-stop` bead with a live aeon is refused: the claim is released, the note and log name the live aeon and `SPIRA_WORLD_STOP_SKIP`, and the ledger reads `world-stop-fence`. With no live aeon it runs stop → session → start. The override proceeds and is logged. | C, FC, O | T1 decision + T3 one case | cert / batch |
| UC-aeon-execution-05 | Sweep mode claims nothing and sets no `branch:` label. It writes born/awake/done, delivers the prompt (including from stdin), exits 0 if the session acted and non-zero if the API refused it, and refuses when capacity is paused or the world is draining. | C, O | T1 gating + T3 one case | cert / batch |
| UC-aeon-execution-06 | Launch argv is a function of the fayth: `FAYTH_SYSTEM_PROMPT` picks `--system-prompt-file` for replace and `--append-system-prompt-file` otherwise; statutes go to system.md and the bead id to stdin; `FAYTH_PROJECT_INSTRUCTIONS=none` gives `--setting-sources user` **in both sweep and bead mode**; `--settings` names `aeon-fence.sh` only when the hook exists. Shipped fayths declare every knob. | C, CF, K | T1 argv builder + T0 fayth-schema lint | cert |
| UC-aeon-execution-07 | Brief rendering. RESUME names the prior-commit count after rebasing onto a moved base. SLAIN appears only when the last commit is a slay wip salvage. DEADLINE gives the kill time and remaining seconds, or says "no wall-clock deadline". ALREADY_DONE instructs `bd supersede` plus successor verification. No `{{` reaches the model. | C, K | T1 render + T2 temp-git for the count | cert |
| UC-aeon-execution-08 | Lease: trace growth renews; silence past `FAYTH_LEASE_SECONDS` writes a `.lapsed` marker (quiet seconds, trailing trace) and kills `-$$`. A trailing `result` line is not activity. The pane countdown renders `?` for a missing, empty or garbage lease. | C, R, O | T1 (after extracting the tick) | cert |
| UC-aeon-execution-09 | Thrash wall. The fuse probe reads `?` with no worktree, 0 when fresh, N minutes when idle, and `gate` while a live gate pid exists (not for a dead pid). It trips only when fuse≥wall **and** session age≥wall (sp-sv34w). `?` never trips. A trip kills the process group. | C, FC, R | T1 predicate + T2 probe | cert |
| UC-aeon-execution-10 | Session outcome: empty, 429 or refused-after-acting → `refused`; missing trace → `unknown`; tool calls with no terminal record → `killed`; ran to its end → `unlanded`; `api_error_status:null` is not a refusal. Only the last trace segment is judged. A headless session waiting for a background task → `yield-headless`. `outcome_charges` is true only for `unlanded` (and yield-headless, pre-session and lapsed via their branches), and anything unenumerated is free (default deny). | C, FC | T1 | cert |
| UC-aeon-execution-11 | Open-bead teardown disposition in precedence order (aeon.sh `cleanup()` 647-930): capacity → slain → thrash → lapsed → gate-unfinished → decision-blocked → timeout(rc 124, nothing committed) → harness REQUEUE_CAUSE → operator-wait → yield-headless → pre-session → `session_outcome`. Each branch releases the claim, writes its note, log and ledger status, and either charges (lapsed, yield-headless, pre-session, unlanded) or records a `bump_requeue` event (thrash, unjudged-*, the requeue cause). `cleanup` disarms errexit first. | C, FC, O, R | **T1 table** + T3 wiring (4 rows) | cert / batch |
| UC-aeon-execution-12 | A harness reopen after a rebase conflict on a closed, committed bead is a requeue, not an attempt. The note reads `Requeue N (rebase-conflict)`, the ledger reads `requeue-rebase-conflict`, and the branch keeps the aeon's commit. | C, R | T3 | batch |
| UC-aeon-execution-13 | Close verdict. A commit naming the bead (branch walk, or landrefs within `SPIRA_VERDICT_WINDOW`) keeps it closed. No commit → reopened with the claim released and a note. Superseded, `delivers:action`, `delivers:beads` with children and `delivers:check` passing all stay closed. **Aeon and sentinel decide delivers types identically.** | C, FC, K | T1 shared classifier + T2 git landref walk | cert |
| UC-aeon-execution-14 | Eviction race: closed while landstate is `EJECTED` or `RED` with an eviction reason at the current tip → reopen with an `eviction-race` note. A stale tip, CERTIFIED, a non-eviction RED or no landstate → stays closed. More than 2 reopens per hour → capped, labelled with the ask label. | C, FC, R | T1 | cert |
| UC-aeon-execution-15 | Closing with the gate still running, with a FAIL verdict or with no gate run leaves a distinct note and log line but does not reopen. | O | T1 | cert |
| UC-aeon-execution-16 | Close guards: (a) a dirty **own** worktree reopens the bead naming the paths and `SPIRA_ALLOW_PROD_DIRTY`, and a dirty shared checkout does not; (b) a statute phrase in the close reason reopens, a quoted mention does not, and the note names `SPIRA_CLOSE_REASON_OVERRIDE`; (c) `FAYTH_SOP_REQUIRED=1` with no SOP write, amend or pass reopens and poisons, and a corrupt ledger fails open; (d) with `FAYTH_GROOM_ESCALATION_CHECK`, an `ESCALATED sp-X` not backed by an in-session ask bead reopens and poisons. The checks bind to the knob, not the persona name. | C, FC, CF | T1 predicates + T3 one case for (a) | cert / batch |
| UC-aeon-execution-17 | Wiki writes made by the aeon are committed at exit under an `aeon-` author with the bead id. Files already dirty before the session and `wiki/tasks.md` are excluded. Concurrent `wiki-commit.sh` callers each land their own commit. | C, Cc, I | T1 selection + T2 concurrency | cert |
| UC-aeon-execution-18 | Exit and ledger. The aeon exits 0 when the bead closed, whatever claude's rc, so the systemd unit never goes FAILED. It exits non-zero when the bead is unclosed and rc≠0. The `done` line carries the real rc, the status and 8 spend fields from the attempt's own trace segment: summed per-turn fields, last cumulative cost/api_s, `wall_s ≥ api_s`, and `?` never 0 when a field is missing. | C, O | T1 | cert |
| UC-aeon-execution-19 | Attempts = in_progress transitions from the events trail, minus claim-then-close, thrash requeue and unjudged requeue. A label containing `in_progress` is not counted. `check4_bulk_data` equals the per-bead queries. No script writes counter labels. | C, K, P | T2 (bd contract) + T0 lint | batch |
| UC-aeon-execution-20 | Claim release is compare-and-swap: `bd unclaim --if-assignee` must name the claiming aeon actor, and naming the fayth leaves it held. | Cc, K | T2 (bd contract) | batch |
| UC-aeon-execution-21 | CHECK 4 poison valve. It examines every dispatchable bead (fayth predicates, not goal children) and poisons at `POISON_AT` (`POISON_AT=0` edge). A held bead is poisoned but keeps its claim. Epics, partition-excluded beads and beads closed mid-pass are skipped. Stale poison below the threshold is cleared. Thrash events do not count. CHECK 7 does not summon a poisoned bead. | C, FC, I | T1 `check4_decide` + T3 one sentinel pass | cert / batch |
| UC-aeon-execution-22 | Poison ask is sent once per (bead, attempt count). The label clear does not re-arm it; a higher count re-asks. The title reads "N in_progress transition(s) without landing"; BRANCH shows the commit count or "no commits"; the body has `## Default` and a log excerpt; `bead.poisoned` is logged to events.log, not mailed. | I, O | T1 | cert |
| UC-aeon-execution-23 | Requeue cap stays silent below `REQUEUE_AT` and sends one mail at the cap ("completed and requeued N times"), deduplicated per bead across higher counts. `delivers:action` is exempt. It never adds poison. The reclaim cap (`RECLAIM_AT`, sentinel.sh:382) does the same with "aeons died holding" wording, and the checks are not hard-coded to one partition. | C, I, O | T1 | cert |
| UC-aeon-execution-24 | `attempts.sh deadlocked` lists poisoned-but-cleanly-mergeable beads (WOULD) and keeps branchless ones (KEEP with a reason). A dry run changes nothing. `--apply` lifts poison, keeps the attempt record and notes why. | C, R | T2 | batch |
| UC-aeon-execution-25 | A repeated lane-cap timeout files an ask saying "timed out" (lane too small), not "change the approach". | O | T1 (stub mail) | cert |
| UC-aeon-execution-26 | `slay.sh` writes the `.slain` marker first. The default reopens, unassigns and removes the worktree and branch. `--close` adds `spira-dropped`. `--keep-work` keeps. Uncommitted work is saved as a patch plus a wip commit, and unique work is parked at `refs/slain/<id>`. Bad ids, positional args and a duplicate `--bead` exit 2 and change nothing. | C, R, FC | T2 + T1 args | batch / cert |
| UC-aeon-execution-27 | The installed agent CLI accepts `--system-prompt-snapshot on` (the external contract `aeon.sh`/`archivist.sh` rely on). | K | doctor preflight (not CI) | doctor |


Machine-readable declarations (read by `spira/plan-lint.sh` once sp-qu948 lands — see §9):

* `UC-aeon-execution-01` [T3] — a bead is worked in a worktree cut from freshly fetched `origin/<landref>`; a stale foreign worktree is moved aside, never removed
* `UC-aeon-execution-02` [T3] — a worktree-creation failure is a pre-session death: FATAL, attempt charged, ledger `pre-session`, no free resummon loop
* `UC-aeon-execution-03` [T1] — a bead poisoned right after claim is released before workspace setup, ledger `poison-raced`
* `UC-aeon-execution-04` [T1] — the world-stop fence refuses a `world-stop` bead with a live aeon and releases the claim; with none it runs stop → session → start
* `UC-aeon-execution-05` [T1] — sweep mode claims nothing, sets no `branch:` label, and refuses when capacity is paused or the world is draining
* `UC-aeon-execution-06` [T1] — launch argv is a pure function of the fayth's system-prompt, project-instructions and settings knobs
* `UC-aeon-execution-07` [T1] — brief rendering leaves no unrendered `{{placeholder}}`; RESUME/SLAIN/DEADLINE/ALREADY_DONE each render correctly
* `UC-aeon-execution-08` [T1] — the lease renews on trace growth and lapses on silence past `FAYTH_LEASE_SECONDS`, killing `-$$`
* `UC-aeon-execution-09` [T1] — the thrash wall trips only when the fuse and the session age both clear the wall
* `UC-aeon-execution-10` [T1] — session outcome classification (refused/unknown/killed/unlanded/yield-headless) and its charge default-deny
* `UC-aeon-execution-11` [T1] — open-bead teardown disposition runs its 13 branches in precedence order and charges only `unlanded`-family outcomes
* `UC-aeon-execution-12` [T3] — a harness reopen after a rebase conflict on a closed, committed bead is a requeue, not an attempt
* `UC-aeon-execution-13` [T1] — the close verdict (commit-naming keeps closed; no commit reopens) is decided identically by aeon and sentinel
* `UC-aeon-execution-14` [T1] — an eviction-race reopen fires only for a live eviction reason at the current tip, capped at 2/hour
* `UC-aeon-execution-15` [T1] — closing with the gate still running, FAIL, or no gate run leaves a distinct note but does not reopen
* `UC-aeon-execution-16` [T1] — the four close guards (prod-dirty, close-reason override, SOP, groom-escalation) bind to their knob, not the persona name
* `UC-aeon-execution-17` [T1] — wiki writes at exit are committed under an `aeon-` author, excluding pre-dirty files and `wiki/tasks.md`
* `UC-aeon-execution-18` [T1] — the aeon's exit code and ledger `done` line reflect the real rc, status and spend fields, `?` never 0 when missing
* `UC-aeon-execution-19` [T2] — attempts are counted from the bd events trail only, never from a label, and `check4_bulk_data` agrees with per-bead queries
* `UC-aeon-execution-20` [T2] — claim release is compare-and-swap on the claiming actor; naming the fayth leaves it held
* `UC-aeon-execution-21` [T1] — CHECK 4 poisons at `POISON_AT`, clears stale poison, and skips epics/partition-excluded/mid-pass-closed beads
* `UC-aeon-execution-22` [T1] — the poison ask is sent once per (bead, attempt count); a label clear does not re-arm it
* `UC-aeon-execution-23` [T1] — the requeue cap and reclaim cap each send one deduplicated mail at their threshold and never add poison
* `UC-aeon-execution-24` [T2] — `attempts.sh deadlocked` lists mergeable poisoned beads and `--apply` lifts poison without losing the attempt record
* `UC-aeon-execution-25` [T1] — a repeated lane-cap timeout asks about the lane, not the approach
* `UC-aeon-execution-26` [T2] — `slay.sh` writes `.slain` first; its default/`--close`/`--keep-work` modes and argument refusals are exact
* `UC-aeon-execution-27` [doctor] — the installed agent CLI accepts `--system-prompt-snapshot on`, checked as a doctor preflight, not in CI

---

## 3. Coverage map

Cost is the CI seconds of the whole file (main push). "e2e" means the proposed single `test-aeon-teardown-e2e.sh` (one bd fixture and one bare origin, seeded once, one aeon run per wiring row). "check4-unit" means the proposed T1 table over an extracted `check4_decide`.

| UC | Existing tests (file::case) | Level now / cost | Verdict |
|---|---|---|---|
| 01 | test-cross-repo.sh::{commit in second repo, second owns branch, no commit in home, log says repo:second}; test-aeon-resume.sh::{stale base setup, branch from fresh origin} | T3 / 7 + (part of) 33 | MERGE-INTO e2e (one cross-repo run asserting base = fresh origin). `worktree_evict_foreign` has no test (gap G10). |
| 02 | test-aeon-presession-death.sh::{FATAL names git error, ledger pre-session, second death, positive control turns=1} | T3 / 15 | MERGE-INTO e2e (keep the ambiguous-ref run; the success control is already covered by every other e2e row) |
| 03 | test-timeout.sh::poison-after-claim guard precedes workspace setup (line-order grep) | grep / (in 156) | SOURCE-GREP → T1 row in the disposition table (G7) |
| 04 | test-aeon-world-stop.sh (19 asserts, 4 runs) | T3 / 20 | DEMOTE-TO-T1 (`world_stop_decide`: label, pidfiles, override) + 1 e2e row (stop precedes start) |
| 05 | test-aeon-sweep.sh (6 runs); test-aeon-exit.sh::{sweep rc=1 but ran, refused sweep} | T3 / 10 + (part of) 16 | DEMOTE-TO-T1 (capacity, draining, settings JSON) + 1 e2e sweep row |
| 06 | test-aeon-prompt-layers.sh (4 runs + fayth greps); test-fayth-project-instructions.sh (sweep only) | T3 / 23, T2 / 2 | DEMOTE-TO-T1 argv builder. The fayth-value greps move to T0 fayth-schema lint (SOURCE-GREP). The "positive control" in prompt-layers is vacuous: DELETE it. |
| 07 | test-aeon-resume.sh (7 runs); test-aeon-verdict.sh::{walled persona deadline, no-wall persona, already-done brief} | T3 / 33 + part of 77 | DEMOTE-TO-T1 (render from commit list) + T2 temp-git for the count after rebase; stale-base → e2e |
| 08 | test-aeon-lease.sh (7 real `aeon_lease_minutes` cases; 5 cases test `hb_check`, **a copy inside the test**); test-aeon-heartbeat.sh (not run; tests `heartbeat_model_idle`, `youngest_in_subtree` and `subtree_has_flock`, none of which is called anywhere outside lib.sh) | T1 / 4; NA | KEEP test-aeon-lease.sh, rewritten to call an extracted `hb_tick`. DELETE test-aeon-heartbeat.sh along with the three dead lib.sh functions. |
| 09 | test-thrash-wall.sh (9/10 asserts test `thrash_check`, a copy); test-thrash.sh::{no worktree ?, just-written 0, stale ~90m, live gate suppresses, dead gate pid} + 9 grep/line-order checks + cockpit-metrics cases | T1 / 3; T1+grep / 30 | MERGE test-thrash-wall INTO test-aeon-lease (`hb_tick` table). KEEP the probe cases of test-thrash.sh as T2 (~3 s). SOURCE-GREP: replace its 9 greps with disposition rows. Move the metrics cases to the cockpit area. |
| 10 | test-attempts.sh::{empty trace refused … charging rule over every outcome} (~20 pure asserts); test-aeon-yield-headless.sh (2 aeon runs, 3 asserts, one phrasing); test-aeon-exit.sh / test-aeon-ledger.sh (indirect) | T1-in-T2 file / 30; T3 / 15 | KEEP as T1 (split test-attempts.sh: classifier half → `test-session-outcome.sh`, T1). DEMOTE-TO-T1: `session_yield_headless` gets a phrasing table (today it has no direct test). |
| 11 | test-aeon-decision-blocked.sh (2 runs); test-aeon-operator-wait.sh (2 runs + 2 greps); test-aeon-presession-death.sh; test-aeon-yield-headless.sh; test-timeout.sh (8 line-order greps); test-thrash.sh::{.thrash after .slain, no increment, requeue-thrash present}; test-attempts.sh::{cleanup disarms errexit, requeue path exits before classification, not-judged bump_requeue grep} | T3 / 11+14+15+15, grep | DEMOTE-TO-T1: a 13-branch disposition table plus precedence rows. MERGE-INTO e2e: the Unlanded control, decision-blocked, operator-wait and pre-session. SOURCE-GREP (all line-order greps): replace with table rows. For operator-wait's `mail.sh` marker grep (`grep -c` == 1), run `mail.sh send kind=question` with BEAD_ID set as a T1 test. |
| 12 | test-requeue.sh::{rebase-conflict requeue ×7 asserts, session did not close control, pair} | T3 / 294 (server mode) | MERGE-INTO e2e (one rebase-conflict run on an embedded store; the "did not close" control duplicates the e2e Unlanded row) |
| 13 | test-aeon-verdict.sh (13 runs, ~45 asserts); test-delivers-parity.sh (awk-extracts `case "$_dtype"` from both scripts and evals it) | T3 / 77; T1 / 1 | DEMOTE-TO-T1: extract `close_verdict` / `delivers_verdict` into lib.sh and call it from aeon.sh and sentinel.sh. The landref walk becomes T2 (git only). DELETE test-delivers-parity.sh (SOURCE-GREP; the extraction makes parity true by construction). Keep 2 e2e rows (committed, no-commit reopen). |
| 14 | test-aeon-eviction-race.sh (7 runs) | T3 / 43 | DEMOTE-TO-T1 (`eviction_reopen <landstate> <tip> <recent>` table, 7 rows + cap row) + 1 e2e row |
| 15 | test-aeon-gate-close-silent.sh (3 runs) | T3 / 16 | DEMOTE-TO-T1 (rc→note switch; rows in the disposition table) |
| 16a | test-aeon-prod-dirty.sh (4 runs, 4 repos) | T3 / 33 | MERGE-INTO e2e (own-dirty reopen + shared-dirty stays); override row → T1 |
| 16b,c | test-ops-closing.sh (15 runs) | T3 / 102 | DEMOTE-TO-T1: the close-reason fence becomes a direct unit of `close-reason-flags.py`; the SOP rule becomes a predicate over a fake ledger. SOURCE-GREP: the 3 static greps go to T0. **The SOP mechanism is OFF for every shipped persona (sp-q27cp).** It needs a keep-or-retire decision (see Gaps). |
| 16d | test-groom-escalation-check.sh (5 runs) | T3 / 36 | DEMOTE-TO-T1 (claims × ask-bead JSON × session epoch) + the conf-key greps to T0 |
| 17 | test-aeon-wiki-dirty.sh (4 runs, 6 repos); test-aeon-wiki-concurrent.sh | T3 / 28; T2 / 1 | DEMOTE-TO-T1 (snapshot set difference minus tasks.md) + 1 e2e row (author, message). KEEP wiki-concurrent (T2) but force an interleave: today it can pass without the lock. |
| 18 | test-aeon-exit.sh (5 runs); test-aeon-ledger.sh (8 runs, ~46 asserts) | T3 / 16; T3 / 36 | DEMOTE-TO-T1: the exit-code mapping, and the spend parser on fixture traces (40 of 46 asserts). The segment-boundary case → e2e (second run on the same bead). |
| 19 | test-attempts.sh::dolt fixture b1..b8 + SQL greps; test-check4-events.sh::{criterion 1 greps, criterion 2 counts}; test-check4-batch.sh::{criterion 1 format, criterion 2 agreement}; test-timeout.sh::{fresh bead 0, one timeout 0, in_progress → 1}; test-requeue.sh counters | mixed / 30+127+103+156 | MERGE-INTO new `test-attempts-sql.sh` (T2, one embedded store, one seed; b1..b8 + bulk≡per-bead + label-with-in_progress). The counter-label ban and bump_* no-op checks go to one T0 lint (SOURCE-GREP, asserted 3 times today). |
| 20 | test-attempts.sh::claim records aeon / release as fayth fails | T2 / (in 30) | MERGE-INTO test-attempts-sql.sh; run it as a **bd contract** test (on a bd version bump + batch), not per commit |
| 21 | test-poison.sh (all, 304); test-poison-edge.sh::{stale cleared, thrash no poison, POISON_AT=0 ×2, CONTROL three failures}; test-poison-ask.sh::{closed mid-pass, empty chamber}; test-check4-events.sh::criterion 3; test-check4-batch.sh::criterion 3; test-requeue-cap-accept.sh::{no attempt events never poisoned, at threshold still poisoned}; test-requeue-cap.sh::thrash no poison | T3 server mode / 304+228+179+127+103+235+213 | DEMOTE-TO-T1 `check4-unit`. KEEP test-poison.sh as the **only** T3 sentinel CHECK 4 pass: one seed with events inserted by SQL (as poison-edge already does), one bead per row, 2 passes. MERGE poison-edge and poison-ask; DELETE criterion 3 of both check4 files. |
| 22 | test-poison.sh::{operator asked, ask body, event recorded, event not mailed, already-poisoned}; test-poison-edge.sh::{ask title, BRANCH line ×2}; test-poison-ask.sh::{asks once, cleared label no re-arm, fourth attempt re-asks} | T3 / (above) | DEMOTE-TO-T1 (ask formatting and the `poison-asked` dedup key); the ask-title assertion is duplicated verbatim in 2 files |
| 23 | test-requeue-cap.sh (6 asserts, 213 s); test-requeue-cap-accept.sh (7 asserts, 235 s) | T3 / 448 | MERGE both INTO check4-unit (the cap/dedup/exemption rows) + 1 row in the T3 poison pass. The reclaim cap has no test (G11). |
| 24 | test-requeue.sh::{deadlock sweep dry run, --apply} | T3 / (in 294) | Move to T2 `test-deadlock-sweep.sh` (stub the landability check or use a 1-commit temp repo; no aeon runs) |
| 25 | test-timeout.sh::{at-limit call produces an ask, ask says timed out} | T2 / (in 156) | DEMOTE-TO-T1 (stub mail.sh; no bd needed). DELETE the `ops_age_of`/`age_of` cases: they test copies of cockpit.sh inlined in the test. Re-home them to the cockpit area against the real function. Also DELETE the `timeouts_of`/`bump_timeout` dead-stub asserts. |
| 26 | test-slay.sh (16 groups, ~60 asserts) | T2 / 60 | KEEP (T2). Arg refusals and `-h` → T1 (~-10 s). SOURCE-GREP: the marker-first grep on a section comment becomes a behaviour check (a stub `kill` records whether `.slain` exists). |
| 27 | test-aeon-launch-grammar.sh (NA: needs the real claude binary) | T4-ish / NA | DELETE from the suite list; move the 2 checks into `doctor.sh` |

## 4. Duplicate clusters

| # | Behaviour | Files (evidence) | Keep |
|---|---|---|---|
| D1 | "3 attempts → spira-poison; 2 → not" | test-poison.sh::dispatchable bead at threshold; test-poison-edge.sh::CONTROL three real failures; test-check4-events.sh::sentinel poisons at threshold; test-check4-batch.sh::criterion 3 poison; test-requeue-cap-accept.sh::at attempt threshold still poisoned; test-attempts.sh::two events do not poison, three do. **Six files, each asserting it on a real Dolt store.** The three poison files copy the same ~100-line fixture (mapper notes), and requeue-cap/-accept copy a ~70-line fixture. | `check4-unit` rows + one row in the test-poison.sh T3 pass |
| D2 | Requeue-cap per-bead dedup | test-requeue-cap.sh::count 4 sends no mail; test-requeue-cap-accept.sh::count 11/12/13 no mail. The file was split "purely for wall time" (header). `REQUEUE_AT=3` is pinned, so "count 10" is not a boundary. | `check4-unit` rows (at cap, cap+1, cap+10) |
| D3 | Stale poison clear | test-check4-events.sh::stale poison cleared / live kept; test-poison-edge.sh::stale poison cleared below threshold | `check4-unit` row |
| D4 | Ask-title wording "N in_progress transition(s) without landing" | test-poison.sh::operator asked; test-poison-edge.sh::ask title (verbatim identical) | T1 ask-format row |
| D5 | attempts_of counts in_progress events | test-attempts.sh (dolt b1..b8); test-check4-events.sh criterion 2; test-check4-batch.sh criterion 2; test-timeout.sh::in_progress transition charges an attempt; test-requeue.sh counter asserts | `test-attempts-sql.sh` (T2, one store) |
| D6 | Counter-label ban / bump_* writes no label | test-attempts.sh (5+1 greps); test-check4-events.sh criterion 1 (5+1 greps); test-timeout.sh::no script outside lib.sh calls bump_timeout, bump_timeout writes no label | One T0 lint |
| D7 | `cleanup()` first line is `set +e` | test-attempts.sh::cleanup disarms errexit first; test-timeout.sh::cleanup disarms errexit first (identical check) | T0 lint (or made moot by the extracted disposition fn) |
| D8 | "Released, No attempt charged" vs "Unlanded" control | test-aeon-decision-blocked.sh and test-aeon-operator-wait.sh (same template and same 4 note assertions, per the mapper overlap); test-requeue.sh::session did not close control | Disposition table + 1 e2e Unlanded row |
| D9 | Close verdict aeon-side vs sentinel-side | test-aeon-verdict.sh; test-delivers-parity.sh; (other area) test-check5-drop.sh, test-check5-delivers-*.sh. The logic is duplicated in two scripts, so the parity test exists. | Shared lib fn `delivers_verdict` + one unit table used by both areas |
| D10 | `aeon_fuse_minutes` on a backdated worktree | test-thrash.sh::stale worktree ~90m; test-thrash-wall.sh::backdated worktree reads fuse ≥20m | test-thrash.sh probe (T2) |
| D11 | Liveness classification | test-aeon-heartbeat.sh (model_idle, superseded by the lease per sp-9ix, aeon.sh:975-985) vs test-aeon-lease.sh | test-aeon-lease.sh (calling real code) |
| D12 | Ledger `status=` field | test-aeon-ledger.sh, test-aeon-presession-death.sh, test-aeon-yield-headless.sh, test-aeon-exit.sh::ledger records closed | Disposition table (the status column) |
| D13 | Rendered prompt text | test-aeon-resume.sh, test-aeon-verdict.sh (DEADLINE/ALREADY_DONE), test-aeon-prompt-layers.sh | One T1 brief-render suite |
| D14 | Fayth knob declarations grep | test-aeon-prompt-layers.sh (FAYTH_SYSTEM_PROMPT ×6), test-fayth-project-instructions.sh (×6), test-ops-closing.sh (FAYTH_SOP_REQUIRED), test-groom-escalation-check.sh (conf keys) | One T0 fayth-schema lint |
| D15 | Full-aeon fixture pattern | 20 suites each build a fresh bd store, a bare origin, a clone and a claude shim per case (r1 note: "17 of the aeon suites"). test-aeon-verdict.sh, test-aeon-eviction-race.sh and test-cross-repo.sh each copy every non-test `*.sh` into SPIRA_HOME. | One shared fixture library + one e2e suite |

## 5. Unit-extractable logic

Every seam below turns a full `aeon.sh` or `sentinel.sh` run on a real Dolt store into a table of pure-function rows.

| Logic | Where now | Tested only via | Seam that enables T1 |
|---|---|---|---|
| Teardown disposition (13 branches + precedence) | aeon.sh `cleanup()` 647-930 | 7 T3 suites (~110 s) + ~20 line-order greps | `aeon_disposition` in lib.sh. Inputs: status, capacity_reset_at rc, marker files present (`.slain .thrash .lapsed .operator-wait`), gate_unfinished rc, bd-show JSON (open ask deps), SESSION_RC, committed, REQUEUE_CAUSE, yield flag, SESSION_STARTED, session_outcome. Output: `<ledger-status> <charge|free> <requeue-cause> <note-key>`. `cleanup()` keeps only the side effects. |
| Heartbeat tick (lease renew/lapse + thrash trip) | aeon.sh 985-1044 (inline subshell) | copies in test-aeon-lease.sh (`hb_check`) and test-thrash-wall.sh (`thrash_check`): **the tests exercise their own code** | `hb_tick prev_mtime cur_mtime now deadline fuse wall session_start` → `ok\|renew\|lapse\|thrash` in lib.sh; the subshell loop calls it |
| Close verdict + delivers types | aeon.sh ~1834-2021; sentinel.sh `case "$_dtype"` | test-aeon-verdict.sh (13 runs), test-delivers-parity.sh (awk + eval of source) | `close_verdict <bd-show JSON> <committed yes/no>` and `delivers_verdict` in lib.sh, called by both scripts |
| Eviction-race decision | aeon.sh 1899-1930 | test-aeon-eviction-race.sh (7 runs) | `eviction_reopen "<state tip at reason>" <cur_tip> <recent_count>` → reopen/stale/cap/none |
| CHECK 4 decision | sentinel.sh ~311-400 | 7 server-mode suites (~1,400 s) | `check4_decide attempts requeues reclaims labels asked_stamp` + env thresholds → `poison\|clear\|ask\|requeue-mail\|reclaim-mail\|none`; `check4_bulk_data` already supplies the inputs in one query |
| Poison ask/requeue mail formatting + dedup key | sentinel.sh | test-poison*.sh via mail.sh stub | `poison_ask_body id attempts branch_commits log_excerpt`; dedup = `run/poison-asked/<id>.<n>` existence |
| Session spend parser | lib.sh python block ~2190-2236 + aeon.sh per-attempt segmenting | test-aeon-ledger.sh (8 aeon runs) | call the parser directly on fixture trace files; expose `trace_segment <log> <attempt>` |
| `session_yield_headless` | lib.sh | 2 aeon runs, one phrasing | direct call on fixture logs (already a pure predicate; no test calls it) |
| Brief rendering | aeon.sh 1183-1222 (RESUME/SLAIN), 1549-1580 (DEADLINE), 1634 (ALREADY_DONE) | test-aeon-resume.sh, test-aeon-verdict.sh (20 runs) | `render_resume_brief <count> <last_subject>`, `render_deadline_brief <at> <now>`; the count stays T2 (temp git) |
| Launch argv | aeon.sh 235 (sweep) and 1786 (bead): two copies of `--setting-sources` | test-aeon-prompt-layers.sh, test-fayth-project-instructions.sh (sweep only) | `aeon_claude_argv <mode>` used by both call sites, which also closes gap G12 |
| World-stop fence | aeon.sh 448-504 | test-aeon-world-stop.sh (4 runs, a `sleep 300` fake aeon) | `world_stop_decide <labels> <live-aeon-list> <skip>` |
| Read-after-claim poison | aeon.sh 430-446 | grep only | `bead_has_label <json> spira-poison` |
| Wiki commit selection | aeon.sh wiki section (~1304, ~1819) | test-aeon-wiki-dirty.sh (4 runs, 6 repos) | `wiki_new_since <snapshot> <porcelain>` minus tasks.md |
| Close guards (SOP, groom, close-reason, prod-dirty override) | aeon.sh post-session; `close-reason-flags.py` | test-ops-closing.sh (15 runs), test-groom-escalation-check.sh (5 runs) | `close-reason-flags.py` is already standalone, so unit it directly; `sop_rule_verdict <ledger> <bead> <epoch>`; `groom_claims_verified <log> <ask-json> <epoch>` |
| Timeout ask loop | lib.sh `spira_ask_timeout_loop` | test-timeout.sh on server-mode Dolt (156 s) | already a function; stub mail.sh, no bd |
| slay argument parsing | slay.sh | real testdb per case | parse before `testdb`/bd is touched; unit with no store |

## 6. Gaps

| # | Uncovered behaviour | Evidence |
|---|---|---|
| G1 | **Lease-lapse teardown**: `bump_lapsed`, the `$SPIRA_RUN/lapsed/<id>-<ts>` record, the note, claim release, ledger `lapsed` and the attempt being charged | aeon.sh 754-770. No test greps `bump_lapsed` or `ledger_done … lapsed`. test-aeon-lease.sh tests a copy of the tick, not cleanup. |
| G2 | Thrash teardown **behaviour**: requeue event `thrash`, no attempt charged, note, ledger `requeue-thrash` | aeon.sh 736-747; only line-order and awk greps in test-thrash.sh |
| G3 | Timeout disposition: rc 124 with nothing committed → no charge, note, ledger `timeout`; and rc 124 **with** a commit falls through | aeon.sh 826-836; test-timeout.sh checks only line order |
| G4 | Capacity lost mid-session in a bead aeon → `capacity_pause_set`, release, ledger `capacity`, no charge | aeon.sh 715-726; `capacity_reset_at` is referenced only by test-archivist.sh |
| G5 | Aeon-side handling of a slain bead (release, ledger `slain`, no charge) | aeon.sh 727-734; test-slay.sh tests slay.sh, not the aeon's cleanup |
| G6 | Gate still running when the bead is **open** → released, ledger `gate-unfinished`, no charge | aeon.sh 772-790; test-aeon-gate-close-silent.sh covers only the closed-bead switch |
| G7 | Read-after-claim poison race → release, ledger `poison-raced` | aeon.sh 430-446; grep in test-timeout.sh only |
| G8 | **Precedence** between teardown branches (e.g. `.thrash` marker plus an open decision blocker; slain during capacity loss) | ordering exists only as `if` order in `cleanup()`; nothing asserts it |
| G9 | Eviction-race cap: more than 2 `eviction-race` reopens per hour → no reopen, note "Guard defect suspected", ask label | aeon.sh 1918-1925; flagged untested by the mapper for test-aeon-eviction-race.sh |
| G10 | `worktree_evict_foreign`: a stale foreign worktree is moved aside (rc 0) or refused (rc 2) | aeon.sh 1092-1100; no test references it |
| G11 | Reclaim cap `RECLAIM_AT` ("aeons died holding") | sentinel.sh 382-394; the test-requeue-cap.sh header promises it but no case asserts it, and `SPIRA_RECLAIM_AT` is set in -accept but never exercised |
| G12 | Bead-mode `--setting-sources` (aeon.sh:1786) | test-fayth-project-instructions.sh drives only the sweep call site (235) |
| G13 | Requeue cap across partitions: the `tinc` persona is written "to prove the check is not hard-coded" but never seeded | test-requeue-cap.sh (mapper note) |
| G14 | CHECK 4 performance: "no per-bead queries" (sp-f1m7f motive) is claimed but never asserted | test-check4-batch.sh. Add a T1 row: `check4` loop with `attempts_of` stubbed to fail if called. |
| G15 | `unjudged-<cause>` requeue for killed, refused and unknown outcomes (note "Not judged", no charge) is asserted only by grep | aeon.sh 916-921; test-attempts.sh::not-judged branch grep |
| G16 | Test integrity: test-attempts.sh's exit-77 skip can fire after earlier failures and hide them, and wiki-concurrent can pass without the lock (no forced interleave) | mapper notes. Fix within the suites. |
| G17 | **Decision for Ryan, not a test gap:** the SOP closing rule (`FAYTH_SOP_REQUIRED`) is OFF for every shipped fayth (sp-q27cp), yet it holds about 11 of the 15 aeon runs in test-ops-closing.sh. Either retire the mechanism with its tests or keep it at T1 only. | test-ops-closing.sh; chamber/ops.fayth |

## 7. Cost

**Current** (sum of `ci_secs` over the 35 primary files that ran; 2 are NA):

```
poison 304 + requeue 294 + requeue-cap-accept 235 + poison-edge 228 + requeue-cap 213
+ poison-ask 179 + timeout 156 + check4-events 127 + check4-batch 103 + ops-closing 102   = 1,941
+ aeon-verdict 77 + slay 60 + eviction-race 43 + groom-escalation 36 + aeon-ledger 36
+ aeon-resume 33 + aeon-prod-dirty 33 + thrash 30 + attempts 30 + aeon-wiki-dirty 28
+ prompt-layers 23 + world-stop 20 + gate-close-silent 16 + aeon-exit 16
+ yield-headless 15 + presession-death 15 + operator-wait 14 + decision-blocked 11
+ aeon-sweep 10 + cross-repo 7 + aeon-lease 4 + thrash-wall 3 + fayth-project-instr 2
+ delivers-parity 1 + wiki-concurrent 1                                                   =   564
                                                                              TOTAL      = 2,505 suite-s
```

That is 26% of the 9,587 suite-seconds on main push. 77% of it (1,941 s) sits in the 10 CHECK 4, requeue and timeout suites that run server-mode Dolt, and those suites assert about 40 behaviours.

**Projected** after the verdicts above:

| Tier | Suite (new or kept) | Est. s | Basis |
|---|---|---|---|
| T0 | counter-label ban, `set +e`, fayth-schema, conf-key lint (folded into the existing lint) | 2 | greps |
| T1 | disposition table; `hb_tick`; close/delivers/eviction verdict; session-outcome + spend parser + yield; brief + argv render; check4-decide + ask format; slay args; timeout ask | 8 | ~1 s each, 8 files (lib.sh source ≈ 0.5 s, per test-aeon-lease.sh) |
| T2 | test-attempts-sql (one embedded store: b1..b8, bulk≡per-bead, unclaim CAS) | 30 | test-attempts.sh today is 30 s including the classifier half |
| T2 | test-slay.sh minus arg/`-h` cases | 45 | 60 − ~15 |
| T2 | fuse probe (test-thrash.sh remnant), resume count + landref walk on temp git, wiki-concurrent, deadlock sweep | 3 + 5 + 1 + 15 = 24 | no aeon runs, no Dolt server |
| T3 | test-aeon-teardown-e2e: 1 fixture + ~16 aeon runs (closed+commit, no-commit reopen, Unlanded, decision-blocked, operator-wait, pre-session, rebase-conflict, cross-repo/fresh-base, world-stop, sweep, prod-dirty ×2, eviction reopen, wiki commit, second-attempt segment, one close guard) | 90 | 16 × ~5 s (verdict 77/13 = 5.9 s; ledger 36/8 = 4.5 s; exit 16/5 = 3.2 s) + ~10 s setup |
| T3 | test-poison.sh rewritten as the sole CHECK 4 pass (one SQL-seeded store, one bead per row, 2 passes) | 60 | poison-edge already seeds by SQL; today's cost comes from ~16 `bd update` cycles per seed × many seeds |
| | **Total** | **259** | 2 + 8 + 30 + 45 + 24 + 90 + 60 |

**Savings: 2,505 − 259 = 2,246 suite-seconds (−90%), about 23% of the whole main-push budget.** The T0 and T1 parts (10 s) move into certification, and the T3 parts (150 s) can run on batch/main only. The gap tests G1–G15 are almost all rows in the T1 disposition, `hb_tick` and `check4_decide` tables, so closing them adds seconds, not minutes.

**Proposed change to the tier model:** add a **bd-contract** slot between T2 and T3. It covers the few tests that assert bd's own semantics that Spira depends on: `unclaim --if-assignee` CAS, the events-trail query and `supersede`. They should run when the pinned bd version changes and on batch CI, not on every harness commit. Today they ride inside `covers: spira/*.sh` suites (test-attempts.sh, test-check4-events.sh, test-timeout.sh), so **any** script edit selects them.


---

## 9. Implementation status (sp-eq8a4, this slice)

This worktree forked from `origin/main` before its own listed dependencies actually reached
`main`: `spira/testlib.sh` (sp-yivi7), the mechanical testlib migration (sp-qvjzb), and the
`docs/test-plan/` schema plus `spira/plan-lint.sh` (sp-qu948) are all closed but none is
reachable from `origin/main` in this tree (`git branch --contains <their commits>` names only
their own branches). Closed is not landed. Concretely: `spira/testlib.sh` does not exist to
source, and there is no `spira/plan-lint.sh` to read this page's declarations. This exact gap
was already discovered and escalated epic-wide by sp-9ce60's aeon (sp-s088v.1, mail sent to
the operator); this bead hit the identical blocker rather than a new one.

Landed in this slice, none of which needs the above:

- This page, `docs/test-plan/aeon-execution.md`, verbatim per the approved design, with the
  `spira/plan-lint.sh`-readable `UC-aeon-execution-NN` declarations in §2 added.

**UC-aeon-execution-27 verdict not applied — reopened on rebase.** A prior slice of this bead
deleted `spira/test-aeon-launch-grammar.sh` and added its missing positive control to
`spira/doctor.sh`'s AGENT LAUNCH GRAMMAR section, on the premise that the "on is accepted"
half already lived there. Rebasing onto `origin/main` picked up `sp-utt1i`'s "doctor.sh
becomes runtime health only", which deleted that section along with every other host-preflight
check in the same pass (preflight is `pre-activate.sh`'s job now); neither `doctor.sh` nor
`pre-activate.sh` on `origin/main` runs this check at all. `test-aeon-launch-grammar.sh`
already carried both checks itself (positive control included, pre-dating this bead), so it
was restored rather than deleted — deleting it now would have left the contract untested.
UC-27 is deferred to `sp-eq8a4.2`, which needs a decision on whether the check's new home is
`pre-activate.sh` (its release-gate shape fits the "K"/not-CI intent) before mechanically
porting it.

Deferred, as a follow-up bead under this bead (`sp-eq8a4.2`, `delivers:beads` applied here),
because they need the testlib/plan-lint dependencies actually present in `origin/main` first:

- The seam extractions Ryan's review named directly: `aeon_disposition` (teardown decision),
  `hb_tick` (heartbeat tick — also collapses the six 'poisoned at 3 attempts' files and
  replaces test-aeon-lease.sh/test-thrash-wall.sh's self-testing copies with real-function
  tests), `close_verdict`/`delivers_verdict` (close verdict), and `check4_decide` (CHECK 4
  decision — the largest single cost cluster in §7).
- The rest of §3/§4's file consolidations and §5's other seams, all of which require
  `spira/testlib.sh` to exist for the AC's "suites all use testlib" requirement.
- The gap tests in §6, fail-closed rows first (G1, G4-G7, G9-G11, G13-G14), and G17's decision
  for Ryan (the SOP mechanism is off for every shipped fayth but its tests still hold most of
  test-ops-closing.sh's runs).

Verified: `bash spira/testenv-batch.sh --suites test-host-reason.sh,test-gate-verdict.sh
spira/sp-eq8a4` in testenv (the two gate suites that scan the whole tree for exactly this kind
of change — a deleted suite, an edited doctor.sh, a new doc).

### sp-eq8a4.2.6's slice

D11 (delete `test-aeon-heartbeat.sh` and lib.sh's three functions it alone exercised —
`heartbeat_model_idle`, `youngest_in_subtree`, `subtree_has_flock`, superseded by the lease
per sp-9ix) is applied: it touches neither `spira/lib.sh`'s seam regions nor any file the
seam-extraction slices below touch, so it does not wait on them. Verified: `bash
spira/testenv-batch.sh --suites test-attempts.sh spira/sp-eq8a4.2.6` (28s) as the regression
check on the rest of `lib.sh`.

D2, D3, D5-D8, D10, D12-D15 are NOT applied this slice. Each targets a table or suite this
bead's own dependencies (sp-eq8a4.2.1-.2.5) build — `aeon_disposition`, `hb_tick`,
`close_verdict`/`delivers_verdict`, `check4_decide`, the brief-render and fayth-schema seams —
and each of the five files those commits touch (`test-attempts.sh`, `test-check4-events.sh`,
`test-timeout.sh`, `test-aeon-prompt-layers.sh`, `test-fayth-project-instructions.sh`,
`test-ops-closing.sh`, `test-groom-escalation-check.sh`, `spira/lib.sh` itself) is also
touched by one of those branches. All five beads show closed via `bd show`, but closed is
not landed: `git merge-base --is-ancestor <branch-tip> origin/main` says no for all five branch
tips as of 2026-09-25, and each closed with "no gate ran or finished for this branch." Doing
this slice's D-items now, against the pre-seam tree, would duplicate work these branches
already did and guarantee a conflict against it once it lands. Deferred to a follow-up bead
under sp-eq8a4.2 that re-checks landing status (git merge-base --is-ancestor, not `bd show`)
before claiming.
