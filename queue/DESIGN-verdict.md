# queue verdict — settling a CI result (replaces spira/verdict.sh)

`verdict.sh` (1,875 lines) is deleted. Its job moves into the `queue` crate as
`queue verdict <repo>`, and `queue step` runs it **in process** instead of spawning a
script. This document was written from the script's intent before the code, as
DESIGN.md is (law-rust-rewrites-start-from-intent). Sp-flj4a.

## 1. Intent

A queued repository hands its landing work to CI and must act on what CI says, exactly
once, without two writers racing one record:

1. **queue.local — settle the publish PR** (`queue/<repo>/publish`, written by
   `queue publish`). Green: fast-forward the forge target to the identical SHAs local/main
   already carries, close the PR, retire the record. Red: never roll anything back and
   never reopen a member — attribute the published range locally, file **one** fix-forward
   bead, retire the record, and leave a `publish-red` marker so `queue publish` does not
   republish the same head (law-a-retry-must-change-an-input). Pending: nothing.
2. **queue.forge — settle the open batch PR** (`queue/<repo>/open`, written by the batcher
   or `queue open-batch`). Green on the sealed head with an unmoved base: fast-forward the
   base, mark every member LANDED and close it. Green on a moved base: rebuild onto the
   new base and force-push (CI re-runs), or close and return members (LANDED for any
   already in the new base). Pending: wait, or cancel a stuck run. Harness fault: re-run
   up to a budget, then close and return members. Red: hand the red to the batcher
   persona (`batcher judgement-ci`) — the judgement belongs to it, not to this pass.

Production today (2026-09-30) is one repository, `spira`, in `queue.local`. queue.forge is
still a legal mode (`queue to-forge`), so its settle stays, minus the machinery whose
producers are gone (§4).

## 2. Contract

```
queue verdict <repo>
```

- `<repo>` is required (`queue.sh verdict: repo required`, exit 2).
- **Exit:** `0` settled, waiting, or nothing to do (no record; another operation holds the
  lock); `1` a refusal or a fault the operator must see (not a registered repository, unopenable
  lock, malformed record, fast-forward refused, base unresolvable, unknown status,
  judgement-ci failed); `2` usage. `queue step` ignores it, as it ignored verdict.sh's.
- **Lifecycle switch** (DESIGN.md §10): resolved once, like `step`. OFF (production):
  spira-lc is never invoked. ON: the green fast-forward also walks the batch
  OPEN → CI_RUNNING → GREEN → LANDED on spira-lc when the record carries
  `batch_id`/`version` (best-effort, never blocks the land — verdict.sh's
  `_lc_land_batch`).
- **Output:** every line verdict.sh printed on the paths kept, with the same text
  (`verdict <repo>: …`), on the same stream. `landing.log` lines kept byte-identical:
  `QUEUE PUBLISH_GREEN`, `QUEUE PUBLISH_RED`.

### 2.1 queue.local

Queue lock (`queue/<repo>/lock`), **non-blocking**: busy → `verdict <repo>: another queue
operation holds the lock` (stdout), exit 0. Under the lock, `settle_publish`:

| record | effect |
|---|---|
| absent | nothing, 0 |
| any of `pr branch remote forge_branch head` empty | stderr `publish record is missing a field — leaving it for a hand look: <path>`, 1 |
| status `green` | push `<head>:refs/heads/<forge_branch>` to `<remote>` (a plain, non-forced push through `spira_git_push`, seam R12); refused → stderr + concierge mail, record kept, 1. Else `pr-close`, remove record, `QUEUE PUBLISH_GREEN <epoch> repo= pr= head=`, concierge mail, stdout line, 0 |
| status `red` | 3 — **not settled under the lock** |
| anything else (pending, harness_fault, empty) | `verdict <repo>: publish PR <n>: <status> — waiting`, 0 |

The status is `forge check-status <path> <pr> <branch>`'s first line, `provision_fault`
normalised to `harness_fault`. On 3 the lock is dropped and `settle_publish_red` runs
unlocked (attribution runs real suites; it must not hold the queue lock): red suites and
run URL from the same check-status output (`red-suite: `, `run-url: ` lines, suites
de-duplicated in order); `attribute.sh --round <branch> --base <base> --suites <csv>
--members <ids> --repo <path>` when both are non-empty; `pr-close`; one bead through
`bdq create` (seam R23: actor `${SPIRA_QUEUE_ACTOR:-queue.sh}`, type bug, priority
`SPIRA_INCIDENT_PRIORITY`, labels `spira,plan,repo:<repo>`, body the same text
verdict.sh wrote); remove the record; write `publish-red` = `head=<head>\nfix_forward=<id>`
atomically; `QUEUE PUBLISH_RED` line; concierge mail; stdout line.

`to-forge` calls the same `settle_publish` in process (it already holds the lock), where
it was seam R19 sourcing verdict.sh.

### 2.2 queue.forge

Queue lock, **waiting** up to `SPIRA_QUEUE_LOCK_WAIT` (default 90 s). Not acquired: the
`lock-skips` counter increments, and the tick that reaches `SPIRA_QUEUE_LOCK_STARVE_MAX`
(default 5) prints `queue lock starvation — skipped <n> consecutive ticks waiting for lock`
instead of the plain busy line; exit 0. Acquired: the counter file is removed.

With an `open` record: `pr head base` all required (else `malformed batch record`, 1).
**Owner check** (DESIGN.md, `owner_refused`): a concierge-claimed batch is refused whole —
actor `${SPIRA_QUEUE_ACTOR:-verdict}`. The base ref must resolve (`spira_landref`, from the
context seam) else `cannot resolve base ref`, 1. Status = check-status's first line
(a failed call, or an empty line, is `pending`), normalised.

| status | decision (pure: `classify_pending` / `classify_fault`) | effect |
|---|---|---|
| pending | run age unknown with a run id → `wait-unknown`; age < `CI_MAXSEC` → `wait-running`; idle < `CI_IDLE_SEC` → `wait-progressing`; else `cancel` | a line; on cancel `run-cancel <run>` then `run stuck (<age>s, idle <idle>s) — cancelled; will retry on next pass` |
| harness_fault | `retries < SPIRA_QUEUE_INFRA_RETRIES` (default 2) → `rerun` else `close` | rerun: `workflow-rerun`, `retries=n+1` rewritten; close: `pr-close`, members `land_mark CERTIFIED <tip>`, record removed, operator mail |
| green, `head-sha:` missing or ≠ sealed head | refused as a harness fault (law-fail-closed-at-the-source) | `pr-close`, members CERTIFIED, record removed, operator mail naming which |
| green, base unmoved | fast-forward | push `<head>:<base-branch>`; failed → stderr, 1. Else line; lifecycle land (switch ON); concierge mail; per member `land_mark LANDED`, `gh_issue_closeout`, `bead_close_on_land`; `testenv suites observe-flake <suite> <head>` per `flaky:` line; record removed; batch branch deleted on the remote and locally; stale `spira/queue/*` refs already on the base reaped |
| green, base moved | rebuild when no member tip is already in the new base: worktree at the new base, `land_subject` merges of every tip in order, force-push to the batch branch, reseal `head`/`members`/`base`, `retries=0`, concierge mail. Otherwise (a conflict, or a tip already in base) | `pr-close`; per member LANDED `already-in-base` (+ closeout/close) if its tip is in the new base, else CERTIFIED; record removed |
| red | — | `judgement=` already on the record → a line, 0. No `red-suite:` lines → a line, 0 (the batcher re-checks). `SPIRA_BATCHER_ENABLE=0` → stderr, 1. Else `batcher judgement-ci <repo> --suites <csv> --members <ids> --evidence "PR <n>[ — <run-url>]"`; `id=` answered → `judgement=<id>` appended atomically, a line; none → stderr, 1 |
| anything else | — | stderr `unknown check status: <s>`, 1 |

After the pass the lock is dropped. A stashed `attributing-<pr>` record (§4 D2) is never
silently ignored: each is named on stderr and the exit is 1.

## 3. Schema

Records are the ones DESIGN.md §3 already specifies (`open`, `publish`), read with
`records::Kv` (last occurrence of a key wins, as `grep | cut` did for single keys). Keys
this pass writes: `retries=` (rewrite), `head= retries=0 members= base=` (reseal),
`judgement=` (append). New files: none. `publish-red` as before.

Settings read through the context seam (R1), resolved by conf.sh exactly as verdict.sh saw
them: `SPIRA_QUEUE_CI_MAXSEC[_<NAME>]` (3600), `SPIRA_QUEUE_CI_IDLE_SEC[_<NAME>]` (600),
`SPIRA_QUEUE_INFRA_RETRIES` (2), `SPIRA_QUEUE_LOCK_WAIT` (90),
`SPIRA_QUEUE_LOCK_STARVE_MAX` (5), `SPIRA_INCIDENT_PRIORITY` (conf.sh's default).

Forge calls (the forge program, `$SPIRA_FORGE`): `check-status <path> <pr> <branch>`,
`run-id <path> <branch>`, `run-metadata <path> <run>` (`started-at: <epoch>`,
`last-activity: <epoch>` lines, the latest activity wins), `run-cancel`, `workflow-rerun`,
`pr-close`.

## 4. Decisions — what was dropped, and why

- **D1 — verdict-owned red attribution is retired, not ported (~560 lines:
  `_q_attribute`, `_repro_*`, `_suites_red_on_base`, the suite-overlap/diff heuristics,
  `_attr_eject` and its three mails, `_meter_write`, `attribution/results.jsonl`,
  `_lc_settle_batch`, `queue_bisect_split/advance/resolve` calls, the TERM trap).** Every
  queue.forge red now goes to `batcher judgement-ci`, which verdict.sh already did for
  `owner=batcher`. Why: its producers are gone. The batches it attributed were cut by
  `batch.sh` (retired, wave 2b) and it dispatched per-member reproduction to forge CI
  (`forge dispatch`) — the round trip the local round and `attribute.sh` replaced
  (sp-hvtgs, sp-q8xs9). The approved adaptive-batching design already hands a round's red
  to the batcher persona; the Concierge is the batcher (2026-09-24). No repository is in
  queue.forge. A hand-cut (`queue open-batch`, owner=operator) red therefore also goes to
  judgement rather than being split by this pass — the one behavioural change for a live
  mode, named here.
- **D2 — the express-takeover stash (`attributing-<pr>`, `_verdict_process_attributing`)
  is retired.** Its only writer was batch.sh. Fail closed: a stash on disk is reported, not
  silently skipped or silently attributed.
- **D3 — the lifecycle land walk honours the switch.** verdict.sh fired
  `_lc_land_batch` whenever the record carried `batch_id`/`version`, whatever the switch
  said. It now runs only with the switch ON, as every other spira-lc call in this crate
  (DESIGN.md §10). Only batcher-cut writes those keys, and only with the switch ON, so the
  observable difference is nil in both modes.
- **D4 — the publish-green push goes through `spira_git_push`** (seam R12), not a bare
  `git push`: the forge push uses the same credentials every other queue push uses.
- **D5 — `queue verdict` returns 1 on a refusal or fault** (a malformed record, a refused
  fast-forward, an unknown status, a red publish whose fix-forward bead could not be
  filed), where verdict.sh's `main` returned 0 after any settle attempt. `step` ignores the
  status either way; a hand run now sees the fault in `$?`. Also named: git's own push and
  `branch -D` chatter is no longer echoed (the fault lines verdict.sh printed are), and
  verdict.sh's "no repo-map entry" reads "not a registered repository (no checkout path)":
  only spira-config may name that file (config-fence).
- **D6 — `_verdict_trap_*` and `SPIRA_QUEUE_REPRO_CI_*`** are gone with D1: nothing in the
  pass blocks long enough to need a TERM log line.

Kept deliberately although small: the lock-starvation counter (a gridlock signal that
fires once per event, conf.sh `SPIRA_QUEUE_LOCK_STARVE_MAX`), `_reap_stale_queue_refs` (stale `spira/queue/*` refs are otherwise
never cleaned), the head-sha verification (the sp-quu2w class of bug).

## 5. Tests and cutover

**Unit tests** (`cargo test -p queue`, 158 in all): `queue/src/tests/verdict.rs` (34) covers
each row of §2.1 and §2.2, the pure classifiers, the lock wait and starvation counter, the
lifecycle walk, the stash refusal and `step` running the pass in process;
`real::tests::context_seam_carries_the_verdict_thresholds_with_the_per_repo_override` and
`real::tests::create_bug_seam_files_through_bdq_with_the_body_in_a_file` run the two seam
changes through real bash.

**Parity** (before deletion): a temporary suite ran `bash verdict.sh` and `queue verdict`
on 25 identically-built fixtures (8 queue.local, 17 queue.forge) and compared exit, output,
queue files, landstate, landing.log, forge calls, helper calls and every ref. 20 identical;
5 intended differences, each asserted: L2/L4/F1/F17 exit 1 (D5), F13 judgement-ci now gets
`--home/--run/--db`, F16 a hand-cut red goes to judgement (D1).

**Retired suites** (subject now the unit tests above, or retired behaviour):
`test-verdict.sh`, `test-verdict-action.sh` (UC-landing-merge-queue-43/44 marked, with the
unit tests named), `test-verdict-replay.sh` (UC-49, D1/D6), `test-eject-unattributed.sh`
and `test-queue-eject-mail.sh` (`_attr_eject`, D1), `test-queue-lock-wait.sh` (verdict half:
`a_held_lock_is_waited_for_then_counted_toward_starvation`; its batch.sh case goes with
batch.sh, wave 2b). **Repointed:** `test-queue-publish`, `test-queue-transition`,
`test-config-compat-master-base` (run `queue verdict`), `test-queue-flush` (step's verdict
seen through the forge), `test-landing-queue-early` (stub renamed), `test-reopen-queue-eject`
(writer is the lib.sh pair `queue eject --red --suites` calls), `test-lifecycle-cutover`
(verdict section retired, D3 unit test), `test-guards`/`aeon-fence.sh` (`queue verdict` is
fenced as a queue verb), `test-git-push-app` (verdict.sh out of the grep list).
