# Test plan — Landed-truth audit and branch reaping (`landed-audit-reaping`)

Part of [[test-plan-2026-09-23]], section 5. Area id `landed-audit-reaping`; use-case ids are `UC-landed-audit-reaping-NN`.

Scope: 31 primary suites (459 CI suite-seconds on main-push run 35947142904) and 1 secondary suite (`test-cockpit-unlanded.sh`, 15 s, owned by cockpit-observability). Subjects: `sentinel.sh` CHECK 5 (lines 605–945), `lib.sh` `landed` / `content_landed` / `spira_destroy_branch` / `spira_destroy_worktree` / `mark_queue_waiters` / `check2_protect_waiting`, `sending.sh`, `pilgrimage.sh`, `held.sh`, `slay.sh` parking, and `cockpit-metrics.py:sending_metrics`.

**Boundary correction.** Two primaries do not belong here: `test-check2-reclaim.sh` tests CHECK 2's ask protection, which belongs to dispatch/reclaim, and `test-dependents.sh` tests queue-wait holding, which belongs to landing-merge-queue. Their verdicts are still given below, and their seconds are counted, so the totals reconcile with the taxonomy.

---

## 1. Intent (de facto spec)

A bead that an aeon closed is only *done* when its work is on its repository's base branch. The sentinel's CHECK 5 re-audits every closed bead in every persona's partition on each pass. It searches the base's full history, bodies included, for a commit naming the bead. When that search finds nothing, it reopens the bead with a remedy note, unless a typed exemption says no commit will ever come. The exemptions are superseded, dropped, content-landed, poisoned, a verified `delivers:` claim, a queued or landed landstate, a hold-mode repo, a branch still ahead of the base, or an unmapped repo. When the base cannot be resolved, CHECK 5 declines to judge rather than reopening.

The Sending reaps `spira/*` branches only when their content is demonstrably on the base: by `content_landed`'s merge-tree test, by fast-forward plus a naming commit, or by a merged PR at the same tip. Every deletion goes through `spira_destroy_branch`. That function refuses queued (CERTIFIED/BATCHED), held, checked-out or unlanded branches and records the refusal in the reap log. `slay` parks unlanded tips at `refs/slain/<id>` rather than losing them.

Epics close only when every child is closed and every live push-mode child branch has LANDED landstate. Hold-mode branches are reported to the operator, not judged.

## 2. Use cases

Tier key: T0 static · T1 unit · T2 component · T3 integration · T4 acceptance. Where: **cert** = certification (every commit) · **batch** = batch CI · **main** = main-push CI · **acc** = acceptance.

### Commit search (the landed question)
- **UC-landed-audit-reaping-01.** `landed <id> <repo>` and CHECK 5's cached walk find a commit naming the bead anywhere in the base's full history. There is no depth window, and a match in the body counts as well as one in the subject. — correctness, performance · **T2** (one tmp git repo, no bd) · cert.
- **UC-landed-audit-reaping-02.** The search matches the bead id as a whole token. `sp-a9g` must not be satisfied by a commit naming `sp-a9gk`. — correctness · **T2** · cert. *(Nothing covers this today; see Gaps.)*
- **UC-landed-audit-reaping-03.** When the land refs are unresolvable, the answer is "cannot tell": `landed` returns rc 2 and CHECK 5 logs "not judging". Neither may reopen. — fail-closed, observability · **T1** (decision) + **T2** (rc 2) · cert.

### CHECK 5 decision
- **UC-landed-audit-reaping-04.** A closed, aeon-worked bead (with `$SPIRA_RUN/<id>.log`) that has no naming commit, no exemption and no branch ahead is reopened exactly once per pass. The reason is `closed-without-commit`, no `sp-attempt-*` label is written, and the note names `bd supersede` as the remedy. — correctness, observability · **T1** decision + **T3** once · cert / batch.
- **UC-landed-audit-reaping-05.** A bead whose work landed is never reopened, and a second pass changes nothing and charges no attempt. — idempotency · **T3** (two passes, one seed) · batch.
- **UC-landed-audit-reaping-06.** The exemptions: superseded (a `supersedes` dependency, under either JSON spelling), `spira-dropped`, `content-landed` and `spira-poison` are never reopened. — correctness · **T1** table · cert.
- **UC-landed-audit-reaping-07.** `delivers:` typed evidence:
  - `beads` needs at least one child.
  - `note` and `report` need a path, an existing file, and an mtime after `started_at`.
  - `check:<cmd>` needs a command that exits 0.
  - `action` is accepted as is.
  - An unknown type, or a missing path or command, fails.
  - A failed delivers claim falls through to the commit check (commit OR evidence), and the reopen note names both reasons.
  - The retired `no-payload` label exempts nothing.

  — correctness, fail-closed · **T1** table over an extracted `delivers_verify` · cert.
- **UC-landed-audit-reaping-08.** Landstate handling:
  - BATCHED, GATED and REBASED are never reopened.
  - LANDED with a tip that is an ancestor of the base is not reopened.
  - A missing branch is restored from the landstate tip or from `origin/spira/<id>`, and the bead is left for CHECK 6.
  - CERTIFIED with branch and tip both gone is not reopened, and says so.

  — correctness, recovery · **T1** decision + **T2** restore (git only) · cert.
- **UC-landed-audit-reaping-09.** A repo in `land=hold` mode never reopens: a standing branch is its terminal state. A push-mode branch with at least one commit ahead is exempt ("CHECK 6 lands it"), and a zero-ahead branch is not. — correctness, config-compat · **T1** · cert.
- **UC-landed-audit-reaping-10.** A closed bead whose `repo:` is absent from the repo-map is skipped, and the skip is logged once per repo per pass. — observability, config-compat · **T1** · cert.
- **UC-landed-audit-reaping-11.** CHECK 5 enumerates every persona partition, not only `spira,plan`. Hand-closed beads (no `<id>.log`) are skipped. — correctness · **T3** row · batch.
- **UC-landed-audit-reaping-12.** The seam contract: `bd list --status closed --json` carries `labels`, the `supersedes` dependency type, the `delivers:` labels and `started_at`. The row reader keeps empty middle columns (`\x1f`, not tab). — contract · **T3** (real bd, once) + **T1** (the reader function itself) · batch / cert.
- **UC-landed-audit-reaping-13.** Sweep triggers (maechen, groom) file their beads with `delivers:note:${SPIRA_RUN}/<name>.log`. — contract · **T1** (stub-bd argv) · cert. *This belongs with the trigger suites.*

### Reaping (sending.sh) and the destroy fence
- **UC-landed-audit-reaping-14.** `content_landed repo br base` returns 0 only when the branch has commits ahead and merging them would leave the base tree unchanged. That covers squash, ancestor and empty-commit review branches. It returns 1 for zero-ahead, conflict or differing-tree branches. — correctness, fail-closed · **T2** table (git only) · cert.
- **UC-landed-audit-reaping-15.** `spira_destroy_branch` refuses in these cases, returning rc 1 with a `REFUSED` reap-log line naming the reason:
  - CERTIFIED/BATCHED landstate, with no bypass.
  - A live holder witness.
  - The branch is checked out in a worktree.
  - Content is not on the base and no caller bypass was given.

  Otherwise it deletes and verifies the ref is gone, logging `FAILED` if it survived. — fail-closed, observability · **T2** (git only, status seam) · cert.
- **UC-landed-audit-reaping-16.** The Sending gives each `spira/*` branch exactly one disposition:
  - `HELD` when a holder is live.
  - `KEEP` when unlanded.
  - `UNADOPTED` when there is no bead.
  - `SENT` for content-landed, and for zero-ahead plus a naming commit (ff).
  - `REAPED` for a squash-merged PR at the same tip, and for a superseded bead whose branch adds nothing to the base.
  - `SKIP` for CERTIFIED/BATCHED.

  It never hands an unlanded branch to the destroyer. — correctness, fail-closed · **T2** table, stub bd / status seam · cert.
- **UC-landed-audit-reaping-17.** When the Sending reaps by content, it labels the bead `content-landed`, but only when the branch had commits of its own. That lets CHECK 5 still catch a zero-commit branch, and CHECK 5 reads the label (writer→reader seam). — contract · **T2** (label argv) + **T3** row (real writer then real reader) · cert / batch.
- **UC-landed-audit-reaping-18.** Reaping a content-landed branch that has no landstate logs `ASSERT <id>` but does not block the reap. — observability · **T2** row · cert.
- **UC-landed-audit-reaping-19.** Once a branch is sent, the Sending also deletes the remote branch when one exists and removes the `branch:` label. It keeps the aeon session log, because CHECK 5 depends on it. — recovery, contract · **T2** row · cert.
- **UC-landed-audit-reaping-20.** Sending pass 2 removes orphaned worktrees whose branch is gone, unless a holder is live, and never touches the shared checkout. `spira_destroy_worktree` prunes an absent out-of-root registration and refuses an existing out-of-root directory. — recovery, fail-closed · **T2** · cert.
- **UC-landed-audit-reaping-21.** The Sending skips loudly when a repo's land ref cannot be resolved, and exits non-zero when anything failed. — fail-closed · **T2** · cert.
- **UC-landed-audit-reaping-22.** The sending counters count distinct branches, not log lines. `SP_SENT_FAILED_AGE_M` renders `?` when there is no failure. — correctness, observability · **T1** · cert.
- **UC-landed-audit-reaping-23.** `slay --bead` deletes a landed branch without parking it. An unlanded tip is parked at `refs/slain/<id>` (same SHA) and the command reports "parked". — recovery, fail-closed · **T2** (stub `bd show`) · cert.

### Epics and held branches
- **UC-landed-audit-reaping-24.** `pilgrimage.sh check` announces a completed Spira-labelled epic once (a mail naming the epic, target and children) and then closes it. It skips epics that are unfinished or not Spira's. It defers the close while any live push-mode child branch lacks LANDED landstate, and pr-mode children are exempt. — correctness, idempotency, fail-closed · **T2** (`pilgrimage_branches_landed`, git + files) + **T3** once (announce/close/dedup) · cert / batch.
- **UC-landed-audit-reaping-25.** `held.sh` classifies branches in hold repos: HELD (ahead, bead exists), EMPTY (zero ahead) or ORPHAN (no bead). `--summary` stays silent when nothing is held. A repo argument restricts output to that repo *and excludes the others*. An unreadable bead store must not be reported as "bead is gone". — correctness, fail-closed, observability · **T2** (stub bd) · cert.

### Misplaced (verdict given; move them out)
- **UC-landed-audit-reaping-26.** Handled by `test-check2-reclaim.sh`: CHECK 2 protects an in_progress bead whose only open dependency is an ask, and removes that protection when the ask closes. → dispatch.
- **UC-landed-audit-reaping-27.** Handled by `test-dependents.sh`: dependents of a closed blocker with CERTIFIED or BATCHED landstate are labelled queue-waiting and excluded from summon, and the label clears in a single pass once the blocker is LANDED. → landing-merge-queue.

## 3. Coverage map

Verdict key: **KEEP** · **MERGE-INTO** · **DEMOTE-TO-T1** · **DELETE** · **SOURCE-GREP** (replace with a behaviour test). New suites proposed:
- `test-check5-decide.sh`: T1 table over the extracted `check5_decide` and `delivers_verify`.
- `test-landed-search.sh`: T2, git only.
- `test-check5.sh`: T3, one seed, two passes, real bd, including the seam contract.
- `test-sending.sh`: T2 disposition table, stub bd plus `--status-from`.

| UC | existing tests (file::case) | level / ci_secs | verdict |
|---|---|---|---|
| 01 | check5-body-search::BODY-ONLY, ::DEEP HISTORY; landed-stays-landed::pass 1/2, ::body-only; check5-landed-assert::pass 1 (401 deep); census-window::depth 4 | T3 22 / T3 46 / T3 22 / T2 2 | body-search → **MERGE-INTO test-landed-search.sh** (search) and test-check5.sh (one landed row). census-window → **MERGE-INTO test-landed-search.sh**. |
| 01 (probe) | landed-stays-landed::probe: windowed sentinel reopens / subject-only sentinel reopens (sed-patches sentinel source) | T3 46 | **SOURCE-GREP**: replace with a seen-to-fail row in test-landed-search.sh against a deliberately windowed helper; **DELETE** the file. |
| 02 | none | — | gap → test-landed-search.sh |
| 03 | none | — | gap → test-check5-decide.sh + test-landed-search.sh |
| 04 | positive control in all 15 CHECK 5 suites; check5-drop::no sp-attempt, ::note names bd supersede; landed-no-delivers::control 'closed without landing' | T3, ×15 | **DEMOTE-TO-T1** (decide table), with one control row kept in test-check5.sh |
| 05 | check5-landed-assert::pass 1, pass 2; landed-stays-landed::pass 2 | T3 22 / 46 | landed-assert → **MERGE-INTO test-check5.sh** (second pass over the whole seed) |
| 06 | check5-drop::dropped/superseded; content-landed-no-reopen::content-landed/poisoned; check5-content-landed::CHECK 5 does NOT reopen sp-cl | T3 18 / 24 / 23 | drop → **DEMOTE-TO-T1** (its bulk-list seam checks move to test-check5.sh). content-landed-no-reopen → **MERGE-INTO test-check5.sh** (seam rows) with exemptions as T1. check5-content-landed → **MERGE-INTO test-check5.sh** (writer→reader row). |
| 07 | check5-delivers-action::*; check5-delivers-check::true/false/no-command; check5-delivers-falls-through::commit wins/none/verified; check5-nopayload::beads±child, note fresh/stale/missing, no-payload retired; check5-sweep-delivers::case 1–3 | T3 9 / 16 / 19 / 48 / 18 | action, check, falls-through, nopayload → **DEMOTE-TO-T1** (delivers_verify table; also retires test-delivers-parity.sh, 1 s, since aeon.sh and sentinel share it). sweep-delivers cases 1–3 → **DELETE** (duplicate nopayload note rows). |
| 08 | check5-landstate::BATCHED, CERTIFIED-RESTORE | T3 14 | **MERGE-INTO test-check5.sh** (restore row, real git) and **DEMOTE-TO-T1** (state table) |
| 09 | check5-hold-no-reopen::hold ahead>0 / ahead==0 | T3 22 | **DEMOTE-TO-T1**; one hold-repo row stays in test-check5.sh, because its `\|` repo-map format incidentally covers the second map parser |
| 10 | check5-unmapped-repo::stay closed, log once | T3 15 (3 identical seeds) | **DEMOTE-TO-T1** |
| 11 | check5-sweep-delivers (maechen-sweep partition) | T3 18 | **MERGE-INTO test-check5.sh** (one non-builder-partition row) |
| 12 | check5-drop::bulk listing labels/supersedes; check5-nopayload::bulk carries delivers; content-landed-no-reopen::seam; landed-no-delivers::IFS read of a literal line | T3 | seam rows → **MERGE-INTO test-check5.sh**. landed-no-delivers → **DEMOTE-TO-T1**: extract sentinel's row reader and test it. Today's case restates the bash idiom and cannot catch a separator change in sentinel.sh. |
| 13 | check5-sweep-delivers::case 4, 5 | T1-in-T3 | **MERGE-INTO test-maechen-trigger.sh / test-groom-trigger.sh** |
| 14 | content-landed-empty-branch::empty not landed, squash landed; destroy-branch::content_landed refuses/approves; sending-empty-commit::content_landed approves/refuses; sending-unlanded-guard::fixture rows; sending-certified-guard::fixture rows; sending-squash-merged::sp-sq (conflict) | T3 7 / T2 1 / T3 8 / 7 / 5 / 8 | the content_landed rows **MERGE-INTO test-destroy-branch.sh** (git only), which becomes the content_landed + fence table |
| 15 | destroy-branch::refuses unlanded, bypass, squash; reclaim-slay-branch-guard::gate ±ctrl, no-bypass refuse/allow; sending-certified-guard::sp-cert survives + REFUSED | T2 1 / T3 13 / T3 5 | destroy-branch **KEEP** (the model suite) and add the CERTIFIED/BATCHED, holder and checked-out rows. reclaim-slay gate section → **DELETE** (duplicate). |
| 16 | sending-unlanded-guard::KEEP sp-ul / SENT sp-la; sending-empty-commit::SENT sp-empty, KEEP sp-real; content-landed-empty-branch::sending reaps ff / not empty; sending-squash-merged::REAPED sp-sq, KEEP sp-post/sp-unland; sending-certified-guard::SENT sp-norm, no SENT sp-cert | T3 7 / 8 / 7 / 8 / 5 | all → **MERGE-INTO test-sending.sh** (one fixture, one pass, stub bd). Each suite rebuilds the same bare+clone+testdb today for a single pass. |
| 17 | sending-content-label::sp-cont labelled / sp-empty not; check5-content-landed::Sending applied label | T3 6 / 23 | **MERGE-INTO test-sending.sh** (argv row) plus one test-check5.sh row |
| 18 | sending-landstate-assert::ASSERT fires / not / still sent | T3 9 | **MERGE-INTO test-sending.sh** (the header's "NO DATABASE" claim is stale; it builds a testdb) |
| 19 | none | — | gap → test-sending.sh |
| 20 | worktree-absent::* | T1/T2 1 | **KEEP**; pass-2 orphan removal is a gap → test-sending.sh |
| 21 | none | — | gap → test-sending.sh |
| 22 | sending-metrics::* | T1 1 | **KEEP** (hermetic model unit test) |
| 23 | reclaim-slay-branch-guard::slay landed / slay unlanded / parked ref | T3 13 | **MERGE-INTO test-slay.sh** using a stub `bd show` (the gate cases seed a bead nothing reads) |
| 24 | pilgrimage::* (9 cases, 4 db resets) | T3 22 | **DEMOTE-TO-T1/T2**: `pilgrimage_branches_landed` becomes a git+files table with `children_ids`/`spira_repos` stubbed. **KEEP** one real-bd seed for announce, close, second pass silent, unfinished and alien. |
| 25 | held::* | T3 10 | **KEEP** at T2: replace testdb with a stub `bdjson show`, and add the missing "other repo excluded" assertion |
| 26 | check2-reclaim::case 1–5 | T2 7 | **KEEP**, reassign area (note: header says four cases, there are five; the reaper itself is never invoked) |
| 27 | dependents::1–14 | T3 18 | **DEMOTE-TO-T1** (landstate→blocker classification) plus one bd seed; reassign to landing-merge-queue |
| (sec) | cockpit-unlanded::* | T3 15 | owned by cockpit-observability; no change here |

## 4. Duplicate clusters

1. **The CHECK 5 positive control, ×15.** Every CHECK 5 suite builds an embedded-Dolt fixture and runs one or more full `sentinel.sh` passes, just to show that a bare closed bead is reopened. That covers 15 suites and 334 s. The fixture (a `testdb.sh` seed plus the stubbed pilgrimage/strand/sending/governor/reflect/ask/launch/systemctl set) is copy-pasted in all of them. *Keep:* a single control row in `test-check5.sh`.
2. **The window and body-only search (sp-d9x93, sp-a9g, sp-37q, sp-m0s7), ×4.** Evidence: `test-check5-body-search.sh`, `test-landed-stays-landed.sh` and `test-check5-landed-assert.sh` each build a 401-commit padded history (one `git commit` process per pad), and `test-census-window.sh` checks the same `landed()` window at depth 4. Cost 22 + 46 + 22 + 2 = 92 s. *Keep:* `test-landed-search.sh` (T2, git only), built from `test-census-window.sh`. Build the depth fixture with `git fast-import` or `commit-tree` in one process. It proves no-window with `SPIRA_VERDICT_WINDOW` set small, not with 401 commits.
3. **The content-landed exemption, ×3.** Evidence: `test-check5-content-landed.sh` and `test-content-landed-no-reopen.sh` both assert "bare reopened, content-landed stays closed". `test-sending-content-label.sh` asserts the writer half. *Keep:* the T1 exemption row, the argv row in `test-sending.sh`, and one writer→reader row in `test-check5.sh`.
4. **The `delivers:note` fresh/stale/missing checks, ×3.** Evidence: `test-check5-nopayload.sh` (cases 5–7), `test-check5-sweep-delivers.sh` (cases 2–3) and `test-check5-delivers-falls-through.sh` (verified note). The r1 reducer also found the delivers case block duplicated between `aeon.sh` (around line 1854) and `sentinel.sh` (lines 676–760), guarded by `test-delivers-parity.sh`. *Keep:* one `delivers_verify` in lib.sh with one T1 table. Delete the parity suite.
5. **The spira_destroy_branch gate, ×2.** Evidence: the gate section of `test-reclaim-slay-branch-guard.sh` (bypass deletes, no-bypass refuses with REFUSED, landed allowed) repeats `test-destroy-branch.sh` case for case, at 13 s against 1 s. *Keep:* `test-destroy-branch.sh`.
6. **The Sending selector fixture, ×6.** Evidence: `test-sending-{unlanded-guard,certified-guard,content-label,empty-commit,landstate-assert,squash-merged}.sh` and the sending half of `test-content-landed-empty-branch.sh` each run `testdb_up` plus bare+clone plus one `sending.sh` pass. unlanded-guard and certified-guard share an identical LANDED-squash positive control. Total 50 s. *Keep:* `test-sending.sh`, with one fixture, one pass and a row per disposition.
7. **content_landed fixture self-checks, ×5.** "content_landed sees X as landed/unlanded" appears as a precondition in destroy-branch, unlanded-guard, certified-guard, empty-commit and content-landed-empty-branch. *Keep:* the content_landed table in `test-destroy-branch.sh`.

## 5. Unit-extractable logic

| logic | where | tested today only via | seam for T1 |
|---|---|---|---|
| CHECK 5 verdict | `sentinel.sh` 632–910, inline in the `while read` loop | 15 full sentinel passes over real bd | Extract `check5_decide` taking the row fields (`superseded dropped sentcontent delivers started_at`) plus injected facts (`refs_ok commit_found ls_state ls_tip tip_is_ancestor land_mode branch_exists ahead restore_tip`). It prints `skip:<reason>`, `restore:<tip>` or `reopen:<reason>`. The loop keeps only the I/O. |
| delivers verification | `sentinel.sh` 676–760; `aeon.sh` ~1854 | sentinel passes; parity suite | `delivers_verify <delivers> <started_at> <id>` in lib.sh, with `bdjson children` behind an overridable function. Rows cover every type plus unknown, empty path and empty command. `check` runs in a subshell with a timeout, which is a new property. |
| row reader | the `\x1f` reader in `sentinel.sh` and the python emitter at 910–945 | `landed-no-delivers` restating the idiom | Move the python emitter into a named function, `check5_rows`, fed JSON on stdin. A T1 test pipes canned `bd list` JSON through the real emitter and the real reader and checks the columns, including the `dependency_type`/`type` spellings. |
| commit search | `lib.sh:landed` 2980; the inlined walk in `sentinel.sh` 813–829 | 401-commit sentinel suites | CHECK 5 re-implements `landed` over a cached `git log %B`. Make both call one `landed_in <subjects> <id>` token matcher, so T1 can table substring and body cases against a string. |
| Sending disposition | `sending.sh:sweep_repo` 182–345 | 6 suites × real bd | The disposition is a function of (holder, content_landed, superseded, closed, pr_tip == br_tip, ahead, is_ancestor, landed, has_bead). Extract `send_disposition`. bd is only read through `bdjson show`, so a stub bd returning canned JSON plus `--status-from` removes testdb entirely. |
| epic close gate | `pilgrimage.sh:pilgrimage_branches_landed` 108 | the pilgrimage suite with 4 db resets | Already a function. Stub `children_ids` and `spira_repos`; only git show-ref and landstate files remain (T2, under 1 s). |
| held classification | `held.sh:_collect` 76 | real testdb | `_bead_status` is the only bd touch; override it. The classification itself could be a T1 function of (bead_state, ahead). |
| queue-wait classification | `lib.sh:mark_queue_waiters` 946 | 5 reseeds (dependents) | The landstate→"holds dependents" predicate: CERTIFIED/BATCHED with a tip hold; none, GATED or CERTIFIED with tip=none do not. |

## 6. Gaps

1. **Prefix collision in the landed search (likely latent defect).** `lib.sh:2995` uses `git log --grep="$id"`, a regex substring, and `sentinel.sh:829` uses `grep -qF "$id"`, also a substring. A commit naming `sp-a9gk` satisfies `sp-a9g`, so a closed and unlanded `sp-a9g` is never reopened. That is the precise failure `law-closed-is-not-landed` exists to catch. No suite has a prefix row. → UC-02.
2. **The "cannot resolve the ref" branch** (`sentinel.sh:826`) and `landed` rc 2 (`lib.sh:2991`) are untested. These are fail-closed paths whose absence would reopen every bead in a repo. → UC-03.
3. **Several delivers failure rows are untested:**
   - `delivers:report`.
   - An unknown type ("is not a recognised type").
   - `delivers:note` with no path.
   - A `delivers:check` command that hangs. There is no timeout; the eval at `sentinel.sh:741` would stall the whole sentinel pass.
   - Label-supplied `eval` quoting.

   → UC-07.
4. **Landstate rows:** GATED, REBASED, LANDED-tip-ancestor, restore from `refs/remotes/origin/spira/<id>`, and CERTIFIED with the tip gone (`sentinel.sh:834–890`). Only BATCHED and CERTIFIED-restore are tested. → UC-08.
5. **Push-mode "branch ahead, CHECK 6 lands it" versus a zero-ahead branch that reopens** (`sentinel.sh:852–860`, the zero-ahead scar sp-qc4kn) is never asserted on the sentinel side. → UC-09.
6. **The hand-closed skip** (`[ -f $SPIRA_RUN/$id.log ]`) has no negative row. The every-partition enumeration and the "chamber declares no partition" note (`sentinel.sh:~905`) are untested apart from maechen-sweep. → UC-11.
7. **`spira_destroy_branch` refusals:** the holder-witness refusal, the "checked out at" refusal and the "survived deletion" FAILED path (`lib.sh:4855–4890`) have no tests. The CERTIFIED guard is only tested through sending. → UC-15.
8. **There is no base `test-sending.sh`.** These dispositions are untested:
   - `HELD` (live holder, including mid-send).
   - `UNADOPTED`.
   - Superseded-but-unsafe `KEEP` (covered at most by test-superseded.sh in landing-merge-queue).
   - Pass-2 orphaned-worktree `SENT`.
   - Legacy `.landing`/`.rebase` `RETIRED`.
   - `SKIP` for an unresolvable land ref.
   - Remote branch delete.
   - `branch:` label removal.
   - Exit status when `failed>0`.

   → UC-16, 19, 20, 21.
9. **held.sh fail-open misreport.** When bd is unreadable, `_bead_status` prints `(none)`, and `_collect` then labels every branch "ORPHAN — bead is gone" and tells the operator it is safe to drop. That is an honest-unknown violation. The named-repo filter's exclusion of other repos is not asserted either. → UC-25.
10. **Header claims the code does not honour:**
    - `test-cockpit-unlanded.sh` claims "a body mention does not count", but has no fixture for it.
    - `test-check5-delivers-action.sh` claims to cover incident.sh and watchtower.sh.
    - `test-check5-delivers-check.sh` claims to cover aeon.sh.
    - `test-check5-landstate.sh` claims to cover batch.sh and conf.sh.
    - `test-check2-reclaim.sh` claims "the reaper reclaims on the same pass".
    - `test-sending-landstate-assert.sh` says "NO DATABASE".

    These are test-integrity items; fix or trim the `# covers:` lines.
11. **Tautology.** `test-check5-nopayload.sh` passed 24/0 against the broken `\x1f` reader, as `test-check5-landed-no-delivers.sh`'s header records. Every one of its rows expects either a reopen or a delivers label, so it cannot see a wrong-route verdict. The T1 table must include the "landed, no delivers, not judged by delivers" row.

## 7. Cost

**Current total: 459 s** (31 primary files, ci_secs from `signals.tsv`):
- CHECK 5 suites (15): 22+23+9+16+19+18+22+22+18+14+48+18+15+24+46 = **334**.
- Sending, content_landed and fence (11): 7+1+13+5+6+8+9+8+7+1+1 = **66**.
- pilgrimage 22, held 10, dependents 18, check2-reclaim 7, census-window 2 = **59**.
- Check: 334 + 66 + 59 = **459**.

**Projected total after the verdicts: about 57 s.** The estimates below are for the new or reshaped suites.

| suite | tier | est. s | basis |
|---|---|---|---|
| test-check5-decide.sh (decide + delivers_verify + row reader) | T1 | 1 | pure bash functions, like sending-metrics at 1 s |
| test-landed-search.sh (absorbs census-window) | T2 | 2 | census-window is 2 s with 2 repos; depth fixture via fast-import |
| test-check5.sh (one seed, 2 passes, ~12 rows, seam contract, one real Sending run) | T3 | 15 | the cheapest current check5 file with 2 passes is 9 s (delivers-action); plus a second pass and one sending run |
| test-destroy-branch.sh (+content_landed table, +3 refusal rows) | T2 | 2 | 1 s today |
| test-sending.sh (stub bd, one pass, ~14 rows) | T2 | 4 | the git-only share of today's 5–9 s suites; the testdb_up (~4–6 s) is gone |
| slay parking rows in test-slay.sh (stub bd show) | T2 | 3 | the git worktree ops in reclaim-slay today, without the 4 reseeds |
| worktree-absent + sending-metrics | KEEP | 2 | unchanged 1 + 1 |
| pilgrimage: T2 gate table + one bd seed | T2 + T3 | 9 | 1 + one reset in place of four (22/4 ≈ 5.5, plus the fixture) ≈ 8–9 |
| held.sh with stub bd | T2 | 2 | testdb removed |
| check2-reclaim | KEEP (move area) | 7 | unchanged |
| dependents: T1 classification + one seed | T1 + T3 | 8 | 18 s at 5 reseeds, down to 1 |

Sum: 1+2+15+2+4+3+2+9+2+7+8 = **55 s**. That rounds to about **57 s** with 2 s allowed for the trigger rows added to test-maechen-trigger and test-groom-trigger, and it also retires test-delivers-parity (1 s, another area).

**Net saving ≈ 402 suite-seconds (−88%).** Of that, 334 → 18 s comes from CHECK 5 alone (decide 1 + search 2 + integration 15). Only one suite in the area (`test-check5.sh`) needs real bd for the audit. What the real-bd runs are then for is the `bd list` field-shape contract and the Sending→CHECK 5 label handoff, not re-proving decision logic.

**Tier placement.** T1/T2 suites (about 20 s) run in certification. `test-check5.sh` and the pilgrimage and dependents seeds (about 32 s) run in batch and main CI.

---

## 8. Implementation status (added by sp-cb39h, not part of the approved plan text above)

This area's three dependency beads (sp-qsona, sp-qvjzb, sp-qu948) were closed but not yet
landed on `origin/main` when this bead ran, so the ground this plan assumes — CHECK 5 reduced
to one invariant (C3), `spira/testlib.sh`, and the `docs/test-plan/` T0 lint — was not yet
present to build on. Per Ryan's review note on this bead, CHECK 5's heuristics are not
refactored here; their suites are deleted once C3 lands, not reshaped first. Landed in this
slice, with `# tier:`/`# covers:` headers already in the qu948 schema so no rework is needed
once the lint lands:

- **UC-01, UC-02, UC-03** (commit search): `spira/test-landed-search.sh`, T2, calling
  `landed()` directly. Absorbs `test-census-window.sh` (deleted). UC-02 documents the prefix-
  collision defect (gap 1) against current behaviour rather than fixing it silently — fix
  tracked as sp-ogogs.
- **UC-15** (destroy-fence refusals): three rows added to the existing `test-destroy-branch.sh`
  — CERTIFIED/BATCHED landstate, a live holder witness, and a branch checked out in a
  worktree. The "survived deletion" `FAILED` row is not covered; it needs a way to make
  `git branch -D` fail while the ref survives, which no fixture here does yet.

Deferred to follow-up beads, filed against this area with the coverage-map rows above as their
brief, and each noting the dependency it is blocked on:

- **UC-04 through UC-13** (CHECK 5 decision, `check5_decide`/`delivers_verify`/row-reader
  extraction, the 15-suite consolidation into `test-check5-decide.sh` + `test-check5.sh`):
  sp-pyowh, blocked on sp-qsona's C3 landing, per the explicit instruction not to refactor
  CHECK 5 first.
- **UC-14, UC-16 through UC-22** (Sending disposition, `send_disposition` extraction,
  `test-sending.sh`): sp-rg46a, blocked on sp-qsona. The area instructions call for "Sending
  reduces to deleting branches of LANDED beads, tested as T1" — that is sp-qsona's
  simplification, not yet on `origin/main`; building `send_disposition` against the current,
  pre-simplification `sweep_repo` would be reshaping code this plan is about to replace.
- **B2 (testlib migration)**: `spira/testlib.sh` does not exist on `origin/main` yet
  (sp-qvjzb, closed, unlanded); `test-landed-search.sh` and the `test-destroy-branch.sh`
  additions use the existing per-suite `ok/bad/is/want` convention and should be swept into
  testlib by sp-qvjzb's own migration once it lands, the same as every other suite.

### UC-24, UC-25, UC-26, UC-27 (added by sp-fcdru)

- **UC-24** (pilgrimage epic-close gate): `pilgrimage.sh` gained the same
  `BASH_SOURCE[0] != $0` guard `landing.sh` already carries, so a suite can source it and
  call `pilgrimage_branches_landed` directly. `test-pilgrimage.sh` now has a T2 section (git
  + files, `children_ids`/`spira_repos` stubbed) covering the sp-qj8n landstate assertion in
  four rows — missing entry, GATED, LANDED, pr-mode-not-checked — and a T3 section with one
  bd seed carrying all three CLI-path epics (announce/close, unfinished, alien), asserted
  across two `check` passes instead of four `testdb_reset`s.
- **UC-25** (held.sh): the fail-open misreport gap 9 describes was already fixed
  (sp-f84wv, landed before this bead ran) — only the test reshape and the missing assertion
  remained. `test-held.sh` replaces `testdb_up` with a hand-written `SPIRA_BD` stub answering
  held.sh's one bd seam (`bdjson show <id>`), and adds the "named-repo argument excludes the
  other repo" assertion the coverage map calls out as missing.
- **UC-26** (test-check2-reclaim.sh, dispatch/reclaim's CHECK 2 ask-protection): not
  reassigned to a new bead here — `docs/test-plan/dispatch.md` already exists, already
  catalogues this ground as `UC-dispatch-21`/gap D7 including an explicit note ("other area
  test-check2-reclaim"), and `sp-9ce60.2.6` is already filed and open to fold it into
  dispatch's own strand-classify table. Filing a second bead for the same suite would
  duplicate work already in flight. This bead only fixed the suite's stale header claims
  (gap 10: the "FOUR CASES" count was five, and three comments claimed the reaper reclaims
  in the same pass when the reaper is never invoked here).
- **UC-27** (test-dependents.sh, landing-merge-queue's queue-wait holding):
  `docs/test-plan/landing-merge-queue.md` does not exist on `origin/main` yet (`sp-ulr4e` was
  in_progress writing it when this bead ran), so the DEMOTE-TO-T1-plus-one-bd-seed verdict
  is filed forward as `sp-a0zfz`, the same "area page doesn't exist yet" deferral `sp-cb39h`
  used for `sp-pyowh`/`sp-rg46a`.
- **Gap 10, the rest.** Fixed independently of UC-26/27 because the bead named these
  fragments explicitly: `test-check5-delivers-action.sh` and `test-check5-delivers-check.sh`
  claimed to cover `incident.sh`/`watchtower.sh`/`aeon.sh` on their `# covers:` lines, but
  none of those files are copied into either suite's fixture (only `sentinel.sh`, `lib.sh`,
  `landing.sh`, `conf.sh` are, plus a handful of scripts stubbed to `exit 0`) — trimmed.
  `test-check5-landstate.sh` claimed `batch.sh`, which is never touched either — trimmed;
  `conf.sh` stayed, since that one genuinely is copied and sourced. `test-sending-
  landstate-assert.sh`'s "NO DATABASE" header claim was stale — it builds a testdb — and was
  reworded.
