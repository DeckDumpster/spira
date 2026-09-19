---
type: note
created: 2026-09-05
updated: 2026-09-18
tags: [spira, ops, sop, runbook, generated]
aliases: [SOPs, Standard operating procedures, The shelf]
---

# Standard operating procedures

**Generated — do not edit.** Regenerated whole by the harness's `spira/sop.sh synth` from the Spira beads database, which is the source of truth. Editing this page has no effect; the next run overwrites it. Amend an SOP instead:

```bash
spira/sop.sh write <slug> -   # text on stdin
```

Statutes are how to behave; SOPs are how to fix. They share one mechanism, split by prefix — `law-` and `sop-` — so the [[spira]] Ops persona reads its runbooks exactly the way every agent already reads [[common-law]]. Ops is summoned by an incident bead filed from a failed systemd unit, matches the payload against the `MATCH:` lines below, and executes the first one that fires.

**11 SOP(s)** on the shelf as of 2026-09-18.

## The closing rule

**An incident resolved without an SOP must produce one.** This is `law-bake-rules-into-tools` applied to production, and it is enforced rather than asked for: writing an SOP is what regenerates this page, the regenerated page is the commit that names the incident bead, and a bead closed with no commit naming it is reopened by `aeon.sh`. An incident fixed by hand and forgotten does not close.

## The shelf

### Flake observer quarantine accountability

`sop-flake-observer-quarantine-accountability`

**Symptom** — Flake observer quarantines suites without proper threshold validation or accountability checks, potentially disarming real failure detection.

**Check** — bash spira/test-observe-flake-threshold.sh && bash spira/test-observe-flake-bead-required.sh

**Fix** — Code fixes applied in spira/suites.sh: threshold validation rejects threshold <= 1, quarantine fails if bead cannot be filed.

**Escalate** — Defect 1 (red-red suite detection) requires gate integration to mark suites in suite-state when they fail twice. Coordinate with engineering.

**Reference** — sp-zmd3u

**Matches** `quarantine.*threshold|quarantine.*no bead|auto-quarantine.*none filed`

### Partition label no match

`sop-partition-label-no-match`

**Symptom** — Sentinel detects a ready bead with no partition label match from any persona. The bead is either missing required labels or has been filed with a parked partition.

**Check** — spira/lib.sh detect_unclaimable_ready 2>/dev/null | grep -q "^UNCLAIMABLE" && exit 0 || exit 1

**Fix** — Check detect_unclaimable_ready output: if the bead is claimable by a parked persona (defined in chamber but not in SPIRA_FAYTHS), no incident should be filed. If truly unclaimable, add required partition labels or fix the bead's fayth: preference. The fix is in the bead, not in the detection logic.

**Escalate** — If a parked partition bead repeatedly triggers incidents despite the improved detection logic, escalate to review whether SPIRA_FAYTHS narrowing is intended or whether the partition should be activated.

**Reference** — sp-l57d (parked partition detection fix), spira/lib.sh:3074-3211 (detect_unclaimable_ready and file_unclaimable_incidents)

**Matches** `^UNCLAIMABLE .* — spira with no matching partition`

### Queue eject

`sop-queue-eject`

**Symptom** — One branch in the open batch reproduces red; other members are clean. Landstate shows BATCHED, bead still in_progress.

**Check** — queue.sh eject <id> --dry-run confirms the id is in the open batch and bd can resolve the bead.

**Fix** — queue.sh eject <id> --reason '<failing suites and assertion lines>' — writes RED to landstate, reopens bead with assignee cleared, posts comment with evidence, removes id from batch members field. Then rebuild the batch separately.

**Escalate** — If the batch record cannot be rewritten (disk full, lock held), check batch lock at $SPIRA_QUEUE_DIR/<repo>/lock and retry. If bd refuses reopen, check bead status manually via bd show <id>.

**Reference** — sp-sggv8

**Matches** `member of open batch fails CI or gate; must be removed without closing the batch`

### Sp 214zs queue open batch

`sop-sp-214zs-queue-open-batch`

**Symptom** — No atomic command to assemble batch from CERTIFIED branches, push, open PR, write record

**Check** — queue.sh cmd_open_batch exists and is dispatched

**Fix** — Implement queue.sh open-batch [<repo>] [--members <ids>] [--skip-pregate] [--dry-run] (lines 240-504)

**Escalate** — If the command fails to open a batch due to lock contention, check queue status; if push fails, verify remote branch state; if PR creation fails, check forge credentials

**Reference** — sp-214zs implementation committed

**Matches** `queue.sh open-batch command missing`

### Sp hsxk8

`sop-sp-hsxk8`

**Symptom** — Landing loop frozen for 14-90 minutes while batch pre-flight gate runs. No landing pass executes during gate run — verdict settlement and certification blocked, even for green PRs.

**Check**

```
git log --oneline origin/main | grep -E '(sp-c8w16|sp-74gwk)' | wc -l | grep -qE '^2$'
```

**Fix** — Implement sp-c8w16 (async gate) and sp-74gwk (marginal-risk narrowing). Gate must not hold landing mutex; subsequent pass collects result.

**Escalate** — none — this is an Ops incident with operator decision documented 2026-09-17 20:11.

**Reference** — docs/sp-hsxk8-sop.md

**Matches** `(landing loop|batch.*gate.*mutex|landing pass.*freeze.*gate)`

### Sp kogm sending age

`sop-sp-kogm-sending-age`

**Symptom** — Oldest unsent branch exceeds 24h threshold, indicating branches stuck without being deleted

**Check** — Verify two things: (1) SP_UNSENT_OLDEST_H < 24, and (2) the bead owning the oldest unsent branch is not CLOSED+STRANDED. A CLOSED bead with landstate=BATCHED and no open batch_id means it's stuck: closed but never reached the queue to land. Use: bd show <bead> | grep -E "^(CLOSE REASON|batch_id)" and check if batch_id exists or if the bead is queued.

**Fix** — If oldest unsent is below 24h AND the owning bead is either in_progress, queued, or landed — normal. If the owning bead is CLOSED+STRANDED (no batch holding it), escalate or manually re-queue it via batch.sh. Branches are held while beads are in_progress, kept if they haven't landed, or retained if deletion would lose work. 

⚠️ CRITICAL: Prior escalations (sp-3tnua for sp-zmd3u, sp-rl5ve re-escalation, and sp-vnjhj recurrence notice) show escalation beads being CLOSED by queue/batch WITHOUT re-queuing or landing the stranded beads. This breaks the escalation mechanism and allows the alert to recur (63+ times since 2026-09-15). Escalation beads must include an EXPLICIT REQUIREMENT that queue/batch must confirm in the close reason that the stranded bead has been re-queued, landed, or explicitly rejected with justification.

**Escalate** — If a CLOSED bead owns the oldest unsent branch but is not in any open batch and has not landed — the bead is stranded and needs queue/batch to explicitly re-queue or land it. File escalation with requirement that CLOSE REASON must include: (1) stranded bead re-queued with new batch_id, OR (2) stranded bead manually landed to origin/main with commit SHA, OR (3) stranded bead rejected as wont-do with explicit justification. Do NOT accept escalation close without evidence of action on the stranded bead itself.

**Reference** — wiki/notes/sending.md, incident:sp-8jany-stranded-batched, sp-vnjhj-escalation-failure-systemic, sp-kogm-recurrence-63-times

**Matches** `sending.*oldest unsent branch.*threshold`

### Test answers premise rejected watcher silent

`sop-test-answers-premise-rejected-watcher-silent`

**Symptom** — test-answers-premise-rejected.sh fails in suite run with "the watcher printed the verdict: wanted [ANSWERED] in []" and "it woke the reader exactly once: wanted [1] got [0]". Test passes consistently when run in isolation with 20/20 pass.

**Check** — bash spira/test-answers-premise-rejected.sh and verify it passes (20 pass, 0 fail)

**Fix** — Test passes in isolation, so the issue is environmental or a race condition specific to suite execution. Likely causes: (1) mark file contention if tests run in parallel, (2) cockpit_attention_beads timing issue with concurrent database access, (3) emit() output buffering in subshell under timeout. Since issue does not reproduce in isolation, it is a suite-environment issue, not a code defect. Suite-level fix: ensure serial test execution or isolate database per test.

**Escalate** — Not Ops work — this is a suite/CI environment configuration issue for the team to address.

**Reference** — wiki/notes/watcher-suite-timing.md

**Matches** `test-answers-premise-rejected.sh.*the watcher printed the verdict.*wanted \[ANSWERED\] in \[\]`

### Timer repair focus neutral

`sop-timer-repair-focus-neutral`

**Symptom** — Timer-driven repair function unconditionally moves operator focus to session pane, disrupting workflow every 60 seconds even when no repairs are needed

**Check** — With healthy cockpit and operator focus on non-session pane, execute repair_dashboards or cockpit ensure. Assert active pane unchanged: tmux display-message -t WINDOW -p #{pane_id} before and after

**Fix** — Record active pane at entry (line 462), restore if still exists after repairs (lines 486-487), select session pane only if original was destroyed (lines 488-489). Regression test: spira/test-cockpit-layout-ensure-focus.sh passes with 1 ok 0 fail

**Escalate** — Check all timer-driven paths in cockpit/layout.sh for unconditional select-pane, select-window, or switch-client — same defect elsewhere causes same operator disruption

**Reference** — sp-gyl8n, cockpit/layout.sh:462-490, spira/test-cockpit-layout-ensure-focus.sh

**Matches** `repair_dashboards.*unconditional select-pane|timer.*active pane.*unconditional|ensure.*focus.*session pane`

### Unadopted refs stale db

`sop-unadopted-refs-stale-db`

**Symptom** — spira/* branches exist that do not resolve to beads in the database, blocking cleanup rites and accumulating SP_UNADOPTED metric

**Check**

```
git -C <repo> for-each-ref --format='%(refname)' refs/heads/spira/ | while read b; do suf="${b#refs/heads/spira/}"; case "$suf" in queue/*|suite-state/*) echo "PROTECTED (harness namespace): $b"; continue;; esac; open_f="${SPIRA_QUEUE_DIR:-$SPIRA_RUN/queue}/<repo>/open"; [ -f "$open_f" ] && grep -qF "branch=$b" "$open_f" && { echo "PROTECTED (open batch): $b"; continue; }; bd -C <db> show "$suf" 2>/dev/null || echo "UNADOPTED: $b"; done
```

**Fix** — For each UNADOPTED (not PROTECTED) branch: verify no aeon holds it (bd show fails), then delete through the chokepoint: SPIRA_REF_SANCTIONED=1 git -C <repo> branch -D <branch>. Do NOT delete PROTECTED branches — queue/* and suite-state/* are load-bearing harness state, not strays. Remove dead worktrees with: git -C <worktree-root> worktree remove <path> --force if needed.

**Escalate** — Root cause is aeon.sh creating branches before beads exist. The queue/* and suite-state/* namespaces are intentional harness infrastructure, never reapable by this SOP.

**Reference** — sp-n9z incident history, sp-ctag9 root cause (queue branch deleted mid-CI)

**Matches** `unadopted refs detected in git for-each-ref`

### Unclaimable partition label

`sop-unclaimable-partition-label`

**Symptom** — Bead is marked UNCLAIMABLE because it lacks a required partition label from the persona system

**Check**

```
bd show <bead> | grep -E 'partition:(groom|incident|maechen-sweep|plan|spike)'
```

**Fix**

```
1. Immediate symptom fix: bd label add <bead> partition:incident
2. Root cause fix: ensure all bdq create calls include partition labels. Key places to check:
   - conf.sh: SPIRA_PLAN_LABEL and SPIRA_INCIDENT_LABEL must include the 'partition:' prefix
   - auron.sh: alert and probe bdq create calls must have SPIRA_INCIDENT_LABEL
   - gate-spira.sh: budget chore bdq create must have SPIRA_PLAN_LABEL
   - review.sh: finding bdq create must have SPIRA_PLAN_LABEL
   - incident.sh: LABELS variable must include SPIRA_INCIDENT_LABEL (partition label for incident beads)
```

**Escalate** — If the partition type needs to be something other than 'incident', determine which persona should claim this bead and add its partition label instead. If this recurs (same failure fingerprint within 7 days) after all fixes above, look for NEW bdq create calls that lack partition labels.

**Reference** — wiki/notes/standard-operating-procedures.md

**Matches** `unclaimable.*no matching partition|no persona's partition labels`

### Verdict requeue on repro fail

`sop-verdict-requeue-on-repro-fail`

**Symptom** — Batch fails red; verdict requeues all members rather than ejecting culprit. Sign: ejected=0 in verdict log after red batch.

**Check** — Manual repro of failing suites against broken commit passes, but verdict failed to eject. This means testenv-batch.sh mounts production (main) rather than the branch under test.

**Fix** — testenv-batch.sh must check out the branch before running suites. Until sp-2f51e lands, hand-eject via batch.sh: write RED <sha> to landstate with reason, re-run batch.

**Escalate** — None — sp-2f51e tracked.

**Reference** — sp-2f51e, sp-uu8oy

**Matches** `verdict requeues all batch members instead of ejecting the broken one`

Related: [[spira]], [[common-law]], [[codified-judgement]]
