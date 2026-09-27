---
type: note
created: 2026-09-05
updated: 2026-09-27
tags: [spira, ops, sop, runbook, generated]
aliases: [SOPs, Standard operating procedures, The shelf]
---

# Standard operating procedures

**Generated — do not edit.** Regenerated whole by the harness's `spira/sop.sh synth` from the Spira beads database, which is the source of truth. Editing this page has no effect; the next run overwrites it. Amend an SOP instead:

```bash
spira/sop.sh write <slug> -   # text on stdin
```

Statutes are how to behave; SOPs are how to fix. They share one mechanism, split by prefix — `law-` and `sop-` — so the [[spira]] Ops persona reads its runbooks exactly the way every agent already reads [[common-law]]. Ops is summoned by an incident bead filed from a failed systemd unit, matches the payload against the `MATCH:` lines below, and executes the first one that fires.

**58 SOP(s)** on the shelf as of 2026-09-27.

## The closing rule

**An incident resolved without an SOP must produce one.** This is `law-bake-rules-into-tools` applied to production, and it is enforced rather than asked for: writing an SOP is what regenerates this page, the regenerated page is the commit that names the incident bead, and a bead closed with no commit naming it is reopened by `aeon.sh`. An incident fixed by hand and forgotten does not close.

## The shelf

### Aeon swept uncommitted work

`sop-aeon-swept-uncommitted-work`

**Symptom** — An aeon session committed files that were uncommitted work from a concurrent session, violating law-commit-only-paths-you-changed. The commit message belongs to the aeon's own work but the commit tree includes files the aeon did not touch. Detected by: file filter on working tree lost after commit, unapproved drafts now on main under another actor's message, or direct finding via commit content verification.

**Check** — cd brain && git show c7c79af --name-only | grep -E '\.(jsonl|md)$' | wc -l; git show c7c79af --format='%an' | head -1

**Fix** — This requires OPERATOR DECISION. Escalate with three options: (1) Amend commit to remove swept files, preserving the intended work; (2) Revert commit entirely and recommit only the aeon's changes; (3) Keep as-is if the swept pages were intended for main. Do not guess at operator intent. File a decision bead and send mail to operator with decision request before closing this incident.

**Escalate** — When an aeon session's commit includes files it did not change. Root cause: aeon did not verify commit contents before pushing, and prior session's uncommitted work was not cleaned. Operator must decide whether to amend, revert, or keep. This is not Ops's fix—it is a decision gate.

**Reference** — wiki/notes/standard-operating-procedures.md

**Matches** `(swept|sweeping|committed.*uncommitted|commit.*included|commit.*contained).*\b(concierge|session|work|draft|test-plan)\b|law-commit-only-paths-you-changed`

### Batch abandon break glass

`sop-batch-abandon-break-glass`

**Symptom** — An open batch must be cancelled before its CI settles — main moved under it, an express or P0 bead must go now, or the batch is holding the queue. Landstate shows members BATCHED and a record at $SPIRA_QUEUE_DIR/<repo>/open.

**Check** — queue.sh abandon <repo> --dry-run — names the open PR and prints each member's disposition. If it says "no open batch" or "another queue operation holds the lock", this is not the case; stop.

**Fix** — This is break glass: never routine, always reasoned. Run queue.sh abandon <repo> --reason '<who authorised it, what it unblocks, what replaces it>'. The reason is mandatory here even though the tool accepts its absence. Innocent members return to CERTIFIED at the tips they were batched at; RED and EJECTED members are left as they are, because those are verdicts an abandon must not undo. The open record is archived to $SPIRA_QUEUE_DIR/<repo>/closed-pr<N>-<stamp>. Then let the next queue step rebuild, or queue.sh flush <repo> to cut the replacement batch immediately. Record the abandon on the bead it unblocked.

**Escalate** — The lock is held by a live queue operation — wait for it, never break the lock; a batch built against a half-abandoned record is the two-PRs-one-batch state. If the PR cannot be closed the members are still returned, so state that in the escalation.

**Reference** — sp-xpj0w

**Matches** `(base moved|abandon the batch|cancel the batch|in-flight batch|batch is blocking|express bead waiting on an open batch)`

### Batched stranded branch

`sop-batched-stranded-branch`

**Symptom** — Watchtower escalates "SENDING: BATCHED branch absent from open batch". A branch has BATCHED landstate but its ID is not in any open batch members= line. sending.sh refuses to reap BATCHED branches (certified-queued guard), so the branch ages in SP_UNSENT_OLDEST_H indefinitely and any unsent-age incident that reads only the bead status will call it "normal queue state".

**Check** — _id=<id from SP_BATCHED_STRANDED_NAMES>; cat $SPIRA_RUN/landstate/$_id; grep "^members=.*$_id:" $SPIRA_QUEUE_DIR/*/open 2>/dev/null || echo "STRANDED: not in any open batch"

**Fix** — if the branch still points to the BATCHED tip (_tip=$(git -C $SPIRA_REPO rev-parse spira/$_id); _btip=$(awk '{print $2}' $SPIRA_RUN/landstate/$_id); [ "$_tip" = "$_btip" ]), recertify: printf 'CERTIFIED %s %s' "$_tip" "$(date +%s)" > $SPIRA_RUN/landstate/$_id. If the tip moved, escalate — the branch has diverged from what was batched.

**Escalate** — when the branch tip differs from the BATCHED tip (the branch has been updated since batching), or when recertifying the landstate is outside Ops's authority.

**Reference** — wiki/notes/standard-operating-procedures.md

**Matches** `SENDING.*BATCHED.*absent.*open batch|Stranded BATCHED|SP_BATCHED_STRANDED=[1-9]|BATCHED.*landstate.*no.*open batch`

### Builder work reclaimed by ops

`sop-builder-work-reclaimed-by-ops`

**Symptom**

```
A bead's body says it "needs a branch through the normal gate, not an Ops
hand-patch" or requires multi-item research. Each Ops wall (~7-8min) is too short for
any item, so each session re-derives "not Ops work" and exits with no commit. Every
such note counts toward the poison threshold with zero code ever attempted.
```

**Check**

```
`bd show <id>` — count notes matching "Ops session (". Two or more reaching the
same "not safe under an Ops wall" conclusion, no code diff between them, confirms this
is dispatch, not a fresh incident.
```

**Fix**

```
Do not attempt partial builder work. Check `mail.sh list operator` for a reply; if
none and one was already sent, do not resend (law-repeating-conditions-escalate-once).
Fix is outside Ops: re-type to a builder lane with a wall long enough for research plus
repro. Past ~10 identical reclaims, stop writing a full note each time -- record
`sop.sh applied --held yes` (background it, sp-dnu2c) and leave one short line instead.
```

**Escalate**

```
On first sighting, send one question and stop; record recurrence count only.
Past 20+ reclaims with no reply, send one further, distinct escalation about the
dispatch mechanism itself (why does the loop keep summoning Ops for a bead Ops can
never close?), then go quiet the same way.
```

**Reference** — wiki/notes/sop-builder-work-reclaimed-by-ops-match-gap.md

**Matches** `Ops session \([0-9]+(st|nd|rd|th)`

### Closed branch cleanup missed

`sop-closed-branch-cleanup-missed`

**Symptom** — Bead is CLOSED but its local branch ref and/or worktree persist, causing repeated "unsent branch" alerts (sp-kogm recurrence, sp-pvafz recurrence). Commit is on origin/main (landed) but cleanup after merge did not run. May also indicate incomplete reap rules in sending.sh.

**Check**

```
bd show $ID | grep "CLOSED"; git merge-base --is-ancestor $COMMIT origin/main && echo "on main"; git rev-parse refs/heads/spira/$ID; ls $SPIRA_RUN/worktree/$ID; sending.sh --dry-run to verify reap rules match
```

**Fix** — Delete local branch ref and worktree using git branch -D spira/$ID && git worktree remove -f $SPIRA_RUN/worktree/$ID. For recurring incidents (held=no): run sending.sh --dry-run to identify problematic beads not matching reap rules (non-code-delivers, batch-landed, superseded). File sp-ob3ts-style bead for developer review of sending.sh reap logic. Add cleanup to queue.sh after merge success to prevent recurrence.

**Escalate** — Ops cleanup for symptom. Systemic fix (sending.sh reap rules) is developer responsibility.

**Reference** — sp-0yqei (incident), sp-xruxc (systemic cleanup bead), sp-pvafz (recurrence+root cause filing), sp-ob3ts (developer fix)

**Matches** `CLOSED.*branch|branch.*CLOSED.*sp-.*not.*cleaned|sp-kogm.*CLOSED.*unsent|SP_CLOSED_STRANDED_OLDEST`

### Closed not landed false alarm

`sop-closed-not-landed-false-alarm`

**Symptom** — A watcher-filed incident reports that a closed bead has no LANDED record in the landing state database.

**Check**

```
Verify the bead's commit is actually on base.
```
bead_id=sp-vcobo  # substitute the bead from the incident title
commit=$(git log --all --oneline | grep "$bead_id" | head -1 | awk '{print $1}')
git merge-base --is-ancestor "$commit" origin/main
```
0 → landed: false alarm from the systemic bug, not a missing commit.

Nonzero AND `bd show` finds the bead OPEN → doesn't apply, likely flapped closed->open (sp-2d14d). File a bead; don't apply FIX.

Nonzero, `$commit` empty, close reason is a direct ops/infra action (prune, host config, manual mitigation) → no-commit-ever case, see REF.
```

**Fix**

```
ESCALATE if sp-2dvyh (land_mark fix) not yet landed — add as blocker dep, leave open (guard sp-kz8ob).

Once sp-2dvyh IS landed: blocked incidents self-resolve; close citing their reasons.

No-commit-ever case: condition is PERMANENT, watcher refiles forever. Do NOT
close as false alarm. File/link a bead against the watcher's own predicate and
`bd dep add` this incident onto it; leave OPEN. Worked example: REF.

Dependency rot: if that blocker bead is later closed as SUBSUMED/DUPLICATE/
"tracked in epic X" rather than landed, `bd dep` reads it as satisfied anyway
and the incident recurs. Re-read the blocker's close reason; re-point the dep
at whatever successor is still open. Repeat down the chain. Full narrative: REF.
```

**Escalate** — Only the land_mark-bug case, while sp-2dvyh is unlanded.

**Reference** — wiki/notes/standard-operating-procedures.md, wiki/notes/sp-b8ot4-closed-not-landed-resolution.md, wiki/notes/sp-4xb7m-no-commit-ever-case.md

**Matches** `CLOSED NOT LANDED.*has no LANDED record|CHECK 5.*closed.*not.*landed`

### Closed not landed systemic

`sop-closed-not-landed-systemic`

**Symptom** — Watcher-filed incidents report closed beads with no LANDED landstate record. Systemic flood: ~143 incidents filed during CHECK 5's first production run (sp-qsona, 2026-09-26 10:35-11:06Z) before fix 359016dc5 landed. Fix prevents NEW floods but CHECK 5 cannot close incidents it previously filed.

**Check** — Verify if systemic by checking for multiple incidents in close succession and sp-42xw9 (bug analysis bead) being OPEN. Verify if individual false alarm by: `bead_id=sp-XXXXX; commit=$(git log --all --oneline | grep "$bead_id" | head -1 | awk '{print $1}'); git merge-base --is-ancestor "$commit" origin/main` (returns 0 if landed).

**Fix** — File sp-0wr0f (or verify it's filed): CHECK 5 must close incidents for beads proven landed via commit graph (sentinel.sh lines 739-767). When a bead is proven landed, close the corresponding external_ref incident (closed-not-landed:<bead-id>). Block sp-7xec0 on sp-0wr0f landing. For individual false alarms, create missing LANDED record retroactively using land_mark LANDED.

**Escalate** — Systemic flood requires sp-0wr0f implementation work.

**Reference** — wiki/notes/standard-operating-procedures.md sp-42xw9 sp-0wr0f

**Matches** `recurrence.*closed.*LANDED|143.*incidents.*CHECK 5|sp-0wr0f.*CHECK 5.*close`

### Conf schema grep q pipefail

`sop-conf-schema-grep-q-pipefail`

**Symptom** — test-suites.sh or any suite that sources conf.sh fails with "spira: bd migrate schema failed — Warning: dolt_server_port in metadata.json is deprecated", even though the database schema is fine and the warning is expected.

**Check** — In conf.sh, locate the schema check block (around "SCHEMA CHECK CACHE"). Find elif branches that use `printf '%s\n' "$var" | grep -q 'pattern'`. These are broken under pipefail: grep -q exits after the first match, printf gets SIGPIPE (141), and pipefail propagates 141 rather than 0, causing the elif to evaluate false when the pattern is present.

**Fix** — Replace `printf '%s\n' "$var" | grep -q 'pattern'` with `grep -q 'pattern' <<< "$var"`. The herestring form has no pipe and no SIGPIPE risk (law-no-grep-q-under-pipefail).

**Reference** — sp-m2zdm

**Matches** `bd migrate schema failed.*dolt_server_port|schema.*failed.*deprecated|conf.sh.*grep.*pipefail`

### Delivers action invalid

`sop-delivers-action-invalid`

**Symptom** — Bead reopened with sentinel error "delivers:action is not a recognised type (beads, note, report, check). Set delivers:TYPE labels that match the evidence actually produced."

**Check**

```
bd show $bead_id | grep -i "delivers:action"
```

**Fix** — Remove the invalid label and close with correct evidence type. bd label remove $bead_id delivers:action, then bd close $bead_id with the actual outcome (beads, note, report, or check).

**Escalate** — none

**Reference** — Only valid delivers labels are: beads (for child beads), note (for narrative findings), report (for metrics/data), check (for verification). delivers:action will cause sentinel to reject the close and reopen the bead.

**Matches** `delivers:action is not a recognised type.*beads.*note.*report.*check`

### Disk full container image accumulation

`sop-disk-full-container-image-accumulation`

**Symptom** — df -h / shows root LV above the 90% warn threshold while /tmp (tmpfs) is fine, ruling out sp-q7d72's /tmp-quota mode; the consumer is on the root LV itself.

**Check** — du -xh --max-depth=1 "$HOME/.local" on the host; if .local/share/containers/storage is tens of GB, confirm with `podman images -a | wc -l` (dozens-to-hundreds is the signature) and check CREATED ages of localhost/spira-testenv tags for a steady one-per-1-2h drip with no pruning.

**Fix** — Ops does not run this, file it for a builder/operator. Stopgap (reversible, images rebuild on next suite run): `podman image prune -a -f` on the host, typically reclaims tens of GB. Permanent: add `podman image prune -f` to the container-build path used by suites.sh/test-container harness so every build stops leaving a dangling ~2-4GB layer, capped per law-fence-loops-on-shared-hardware. Also glance at checkpoint/old-beads and stale cargo-target-bins-r* dirs under the run directory for smaller free wins.

**Escalate** — reclaiming disk via podman prune is a bulk delete on shared host storage an Ops aeon cannot fully verify is safe against in-flight runs -- send the operator a decision mail with a default action rather than running it in-session.

**Reference** — wiki/notes/sp-q3lgs-disk-full-container-images.md

**Matches** `(Disk on /.*(9[0-9]|100)% used)|(root filesystem.*near capacity)|(FAULT \([0-9]+%, warn at)`

### Doctor installing guards

`sop-doctor-installing-guards`

**Symptom** — doctor.sh exits non-zero with SPIRA_DOCTOR_INSTALLING=1 set, meaning at least one expected installation-phase check is still emitting FAIL instead of WARN

**Check** — SPIRA_DOCTOR_INSTALLING=1 doctor.sh 2>&1 | grep "FAIL"; if any FAILs are present the issue is confirmed

**Fix** — Find FAILs in doctor.sh that are expected during installation (SPIRA_DOLT_DATA missing, dolt-beads.service inactive, repo-map missing, home-repo not mapped). Wrap each FAIL with: if [ -n "${SPIRA_DOCTOR_INSTALLING:-}" ]; then WARN ...; else FAIL ...; fi

**Escalate** — none — this is a code fix

**Reference** — spira/doctor.sh lines 324-330, 337-347, 548-554, 576-586

**Matches** `doctor\.sh.*SPIRA_DOCTOR_INSTALLING.*FAIL.*downgrade`

### Dolt groupby leak blocks shutdown

`sop-dolt-groupby-leak-blocks-shutdown`

**Symptom** — dolt-beads.service is ActiveState=active (0% CPU, threads parked on futex) but :3307 stops accepting. Clients see "circuit breaker is open". Goroutine dump shows connections stuck in go-mysql-server GROUP BY (groupByGroupingIter -> errguard -> WaitGroup.Wait): client timed out and left but the server-side query goroutine never exits, leaking a slot. Enough leaks stop the listener; a later restart/SIGTERM then blocks forever on the leaked queries.

**Check** — `systemctl --user is-active dolt-beads` reads "active" through the whole outage -- not sufficient alone. Confirm with `ss -tln | grep 3307`: active unit + empty result = this bug.

**Fix** — Recovery only, does not fix the leak. Snapshot store if wanted. `restart` is likely refused by a RefuseManualStop drop-in -- check first. `kill --signal=TERM` will NOT end it. `kill --signal=QUIT` does, and Restart=always brings it back; capture `journalctl --user -u dolt-beads -n 500` right after for the dump. Verify with `ss -tln | grep 3307` plus a real bd call. Move any snapshot off root once done -- root has a 90% watcher.

**Escalate** — Real fix is sp-ynow8, a builder task, not an Ops hand-patch -- confirmed it does not fit an Ops wall. dolt-server.yaml has no execution-timeout key (only connection read/write_timeout_millis, which won't kill a stuck query). The health check to fix, $SPIRA_RUN/concierge-notes/cert-pool-watch.sh:38, is a live hand-deployed loop with no git backing -- locate its canonical source before editing.

**Reference** — incident sp-nmzok (goroutine dump); sp-ynow8 (builder follow-up, in_progress)

**Matches** `circuit breaker is open|client connection went away while a query was executing|groupByGroupingIter|errguard.*WaitGroup`

### Duplicate incident host cores

`sop-duplicate-incident-host-cores`

**Symptom** — Incident filed claiming host_cores() function is missing from lib.sh or gate.sh

**Check** — grep -q "^host_cores() {" /path/to/lib.sh && /path/to/test-governor-host-cores.sh

**Fix** — No fix needed — the function was already implemented in sp-79ww9 (commit 818c218, 2026-09-17). Verify it exists and the test passes. This is a duplicate incident.

**Escalate** — None — this is incident triage, not a defect in the code.

**Reference** — wiki/notes/spira-duplicate-incidents.md

**Matches** `host_cores.*function.*missing|function.*undefined.*host_cores`

### False alarm no cause

`sop-false-alarm-no-cause`

**Symptom** — Incident filed without SPIRA_INCIDENT_CAUSE, resulting in empty systemctl/journalctl output

**Check**

```
bd -C $SPIRA_DB list | grep sp-yuiuc; verify the incident has no_cause in the description and empty payload
```

**Fix** — Close as false alarm when the incident title does not match a real systemd unit and no actual blocker exists

**Escalate** — If a real systemd unit is unreachable, escalate as a service availability issue

**Reference** — wiki/notes/false-alarms-from-misconfigured-probes.md

**Matches** `^The intake gathered NO payload.*systemctl.*produced nothing`

### File io buffering subshell

`sop-file-io-buffering-subshell`

**Symptom** — File appends inside subshells (created by pipes) are not flushed before the subshell exits, leaving files empty or incomplete when the parent shell tries to read them. Manifests as empty output files despite printf/append operations, or subsequent reads seeing no data.

**Check** — strace -e openat,write the script to verify buffer state; or add explicit sync/flush after the subshell; or run the test suite (test-census.sh 10, 10b).

**Fix** — Move file I/O outside the subshell. Have the function output classification markers to stdout, then process in a while loop in the parent shell where appends are guaranteed to flush before the next operation. Avoid direct file appends inside pipelines; instead pipeline the data first, then append in parent context.

**Escalate** — None—this is a Bash scripting pattern issue, fixable at implementation level.

**Reference** — wiki/notes/bash-subshell-io-buffering.md

**Matches** `orphaned_closed|buffering|subshell.*pipe`

### Flake observer quarantine accountability

`sop-flake-observer-quarantine-accountability`

**Symptom** — Flake observer quarantines suites without proper threshold validation or accountability checks, potentially disarming real failure detection.

**Check** — bash spira/test-observe-flake-threshold.sh && bash spira/test-observe-flake-bead-required.sh

**Fix** — Code fixes applied in spira/suites.sh: threshold validation rejects threshold <= 1, quarantine fails if bead cannot be filed.

**Escalate** — Defect 1 (red-red suite detection) requires gate integration to mark suites in suite-state when they fail twice. Coordinate with engineering.

**Reference** — sp-zmd3u

**Matches** `quarantine.*threshold|quarantine.*no bead|auto-quarantine.*none filed`

### Gate locking functional

`sop-gate-locking-functional`

**Symptom** — Investigation of gate tree locking mechanism under concurrent execution

**Check** — test-gate-tree.sh && systemctl show spira-gate 2>/dev/null || echo "gate service not active" && journalctl -u spira-gate.service -n 20 --since "1 hour ago" 2>&1 | grep -i "lock\|timeout" || echo "No lock issues in logs"

**Fix** — None needed — locking mechanism is functional

**Escalate** — N/A — this is a verification that the mechanism works

**Reference** — None

**Matches** `gate tree locking|gates not serialized|flock timeout`

### Gate orphan race

`sop-gate-orphan-race`

**Symptom** — A gate or runner reports "left background jobs after exit" for a suite that exited cleanly with no children, causing intermittent test failures under load.

**Check** — Compare the orphan detection block in the failing runner against suites.sh. Look for a two-step pattern with sleep 0.2 before the second kill -0 check. A runner with sleep 0.05 (the earlier value) will produce false positives under CPU contention — 50ms was found insufficient; transient PGID entries can persist that long on a loaded box.

**Fix** — Wrap the orphan kill -0 check in a two-step pattern: sleep 0.2, then re-check. Real survivors (backgrounded processes) persist well beyond 200ms; transient PGID entries clear within that window even under CPU contention from concurrent suites. The 50ms pattern (sleep 0.05) was insufficient — it was observed holding beads open and causing test-suites.sh to flake at 9+ recurrences. NOTE: If the 200ms pattern is already present and flakes continue, check for the separate conf.sh grep -q SIGPIPE issue (sop-conf-schema-grep-q-pipefail) — the two failures are independent and both can cause test-suites.sh to flake.

**Reference** — sp-m2zdm

**Matches** `left background jobs after exit|gate harness.*orphan|orphan.*gate.*false positive`

### Gh intake no issues

`sop-gh-intake-no-issues`

**Symptom** — gh-intake.sh exits with code 1 when repository has no open issues to ingest

**Check** — curl -s "https://api.github.com/repos/DeckDumpster/spira/issues?state=open" | python3 -c "import sys,json; d=json.load(sys.stdin); prs=[i for i in d if 'pull_request' in i]; issues=[i for i in d if 'pull_request' not in i]; print(f'PRs: {len(prs)}, Issues: {len(issues)}')"

**Fix** — Change line 160 of gh-intake.sh from die to exit 0. This allows the service to complete successfully when there are no issues (including cases where all open items are pull requests, which are filtered by line 144). The store verification at line 99 still catches misconfiguration.

**Escalate** — N/A — this is a policy decision (approved as Option A: exit 0 when no issues)

**Reference** — wiki/notes/gh-intake-no-issues.md

**Matches** `fetched 0 open issue.*gh-intake`

### Landed citation mismatch

`sop-landed-citation-mismatch`

**Symptom**

```
bead closed OUTCOME:landed citing a real origin/main commit unrelated to the
  bead's content/authorship; the bead's own problem stays unfixed after closure.
```

**Check**

```
git log -1 --format='%an %ae %s' <cited-sha>; compare to bead owner/description.
  If unrelated but real (git merge-base --is-ancestor <cited-sha> origin/main), this is it.
```

**Fix**

```
Diagnosis only. Two mechanisms found (sp-9geby/sp-dyw7l), neither patched yet:
  (1) lib.sh bead_cited_commit_on_base()'s "declared" branch (~3927) accepts a
      self-declared `landed as <sha>` note once it's an ancestor of base, with no
      id-in-message/content check (the "bare" branch below it does check — copy that).
  (2) landing.sh:1663-1667 land_mark cites $tip correctly, then bead_close_on_land is
      passed a fresh `git rev-parse HEAD` re-read instead — vulnerable if the shared land
      worktree advances between the two reads. Pass $tip everywhere, never re-read HEAD.
  Grep bead_close_on_land for all call sites before assuming one fix covers every path.
```

**Escalate**

```
pinning which mechanism fired for a given bead needs the landing pass's own log
  near its close time (may be rotated); grep bd notes for "landed as"/"hand-landed" first —
  absence rules out mechanism #1 for that bead.
```

**Reference** — wiki/notes/sp-9geby-landed-citation-mismatch.md

**Matches** `closed .* 'landed'.*citing|citing.*(unrelated|wrong).*commit|orphan-work.*count unchanged`

### Mail send loom splice hang

`sop-mail-send-loom-splice-hang`

**Symptom**

```
mail.sh send operator backgrounds/hangs then exits 124 under a short timeout.
  Reproduces deterministically with a fresh throwaway message.
```

**Check**

```
strace -f -tt -o /tmp/trace.log timeout 8 mail.sh send operator --from X --subject Y --kind question --default Z
  Confirms if trace ends in a bare `cat` (execve(.../cat,["cat"])) whose fd0 is
  S_IFSOCK, dup2'd from a bash-coproc fd (pipe2, fd>=10), blocked forever in
  splice(0,NULL,1,NULL,...) past strace's own ceiling.
```

**Fix**

```
Not Ops-actionable — code defect. loom.sh/loom binary were confirmed running,
  port 8788 open, no stale lock, mailboxes small: this is not an environment fault.
  Builder fix: wrap the coprocess-output read/cat step in mail.sh's loom-backed send
  path with a real deadline; SPIRA_LOOM_BUDGET_MS=1500 is set but not enforced on
  this path. See REF for full trace analysis.
```

**Escalate**

```
File a bug bead for a builder. If mail.sh send operator is the only
  escalation channel and it's down, escalate via `bd note` directly; don't retry.
```

**Reference** — wiki/notes/mail-send-loom-splice-hang.md

**Matches** `mail\.sh send.*(hang|timed? ?out|exit 124)|mail\.sh.*>?120s`

### Orphaned branches blocking sending

`sop-orphaned-branches-blocking-sending`

**Symptom**

```
Bead-less branches accumulate because sending.sh reap requires a
bead. Cockpit splits them: SP_UNADOPTED (commits on base, safe to discard) vs
SP_ORPHAN_WORK (commits not on base, unlanded - deleting destroys work).
```

**Check** — grep -E 'SP_UNADOPTED|SP_ORPHAN_WORK|SP_UNSENT_OLDEST_H' "$SPIRA_RUN/cockpit.env"

**Fix**

```
Do not delete ORPHAN_WORK branches. Archive-not-delete (move ref to
refs/archive/<name>, publish to origin, then delete branch ref) is the
demonstrated fix - see REF for mechanism and the local-vs-remote durability
gap. Aeons have no push credentials; this is diagnosis, not self-service.

Once escalated, immediately upgrade the incident<->decision-bead relation to
`blocks` (`bd dep remove`, then `bd dep add ... --type blocks`) so `bd ready
--claim` stops resurfacing the incident every tick. Four sessions re-derived
the same finding and died at the wall because this was left as relates-to.

Confirm mail delivery before trusting a prior session's claim that it sent -
`mail.sh send` can hang and get backgrounded; a task output of only
"[killed]" means it never sent. Resend backgrounded, don't retry foreground.
```

**Escalate**

```
Whether sending.sh should adopt archive-then-publish as standing
behavior for bead-less branches is an open operator decision (sp-vazlj).
Until answered, leave ORPHAN_WORK alone and keep the incident blocked on it.
```

**Reference** — wiki/notes/orphaned-branches-archive-mechanism.md, sp-cc7gs, sp-vazlj

**Matches** `SP_UNSENT_OLDEST_H remains high.*orphaned.*branch|sending\.sh.*blocked|round-[0-9]+.*no.*bead`

### Partition label no match

`sop-partition-label-no-match`

**Symptom** — Sentinel detects a ready bead with no partition label match from any persona. The bead is either missing required labels or has been filed with a parked partition.

**Check** — spira/lib.sh detect_unclaimable_ready 2>/dev/null | grep -q "^UNCLAIMABLE" && exit 0 || exit 1

**Fix** — Check detect_unclaimable_ready output: if the bead is claimable by a parked persona (defined in chamber but not in SPIRA_FAYTHS), no incident should be filed. If truly unclaimable, add required partition labels or fix the bead's fayth: preference. The fix is in the bead, not in the detection logic.

**Escalate** — If a parked partition bead repeatedly triggers incidents despite the improved detection logic, escalate to review whether SPIRA_FAYTHS narrowing is intended or whether the partition should be activated.

**Reference** — sp-l57d (parked partition detection fix), spira/lib.sh:3074-3211 (detect_unclaimable_ready and file_unclaimable_incidents)

**Matches** `^UNCLAIMABLE .* — spira with no matching partition`

### Presession death test regression

`sop-presession-death-test-regression`

**Symptom** — Test-aeon-presession-death.sh fails with FATAL line not present, attempt not charged, or error not propagated when run against sp-dd785 or similar pre-session death fixes.

**Check** — Run test-aeon-presession-death.sh directly; grep output for "FAIL.*FATAL" and "status=pre-session" in ledger

**Fix**

```
The fix is incomplete in the target branch. Examine aeon.sh error handling path for:
  1. Worktree creation errors being swallowed rather than captured
  2. Attempt counting not triggering on pre-session deaths
  3. Error message propagation (git errors not appearing in FATAL line)
```

**Escalate** — Route to whoever is working on aeon.sh pre-session death handling. Not Ops to fix.

**Reference** — wiki/notes/standard-operating-procedures.md

**Matches** `test-aeon-presession-death.sh.*FAIL.*FATAL`

### Queue eject

`sop-queue-eject`

**Symptom** — One branch in the open batch reproduces red; other members are clean. Landstate shows BATCHED, bead still in_progress.

**Check** — queue.sh eject <id> --dry-run confirms the id is in the open batch and bd can resolve the bead.

**Fix** — queue.sh eject <id> --reason '<failing suites and assertion lines>' — writes RED to landstate, reopens bead with assignee cleared, posts comment with evidence, removes id from batch members field. Then rebuild the batch separately.

**Escalate** — If the batch record cannot be rewritten (disk full, lock held), check batch lock at $SPIRA_QUEUE_DIR/<repo>/lock and retry. If bd refuses reopen, check bead status manually via bd show <id>.

**Reference** — sp-sggv8

**Matches** `member of open batch fails CI or gate; must be removed without closing the batch`

### Queue throttle depth filtering

`sop-queue-throttle-depth-filtering`

**Symptom** — Queue throttle remains engaged because depth calculation never filters stale CERTIFIED records. SPIRA_TC_REPO env var is unset in production, causing the filtering logic (branch existence + merge-base check) to be skipped, so all CERTIFIED records are counted as depth regardless of staleness.

**Check** — grep -A 20 "Filter stale records" spira/watchtower.sh | grep -E "(rev-parse|merge-base)" | head -2 | wc -l

**Fix** — Move filtering logic outside the `if [ -n "$_tc_repo" ]` conditional. When _tc_repo is unset (production case), use auto-detection (bare `git` in current directory) instead of requiring explicit repo path. Filtering now applies unconditionally: for each CERTIFIED record, verify the branch exists and tip is not yet merged to the land ref, dropping stale records from depth count.

**Escalate** — N/A

**Reference** — sp-ihvh8, sp-5kwhr (prior attempt), incident:Queue-throttle--depth-calculation-never-filters-stale-CERTIF

**Matches** `Queue throttle.*depth calculation.*SPIRA_TC_REPO|depth.*stale CERTIFIED.*throttle|watchtower.*_tc_repo.*empty`

### Queue throttle engaged

`sop-queue-throttle-engaged`

**Symptom** — Builder pool held; depth >= 12 and drain active (< 50m since landing). Root cause was watchtower miscounting stale CERTIFIED records as queue depth. Fixed in sp-5kwhr: watchtower.sh --throttle-check now filters stale CERTIFIED, batch.sh cleanup moved before _batch_is_open guard.

**Check** — Confirm SP_QUEUE_DEPTH in cockpit.env. If >= 12 and recent landings active, throttle is engaged. If depth < 6 after queue drains, throttle lifts (watcher files sop-queue-throttle-lifted).

**Fix** — Monitor queue drain naturally. If depth remains >= 12 for > 60m with no landings, escalate - this indicates a blocked batch or other systemic issue. Watchtower throttle logic now correctly excludes stale CERTIFIED records.

**Escalate** — If throttle remains engaged after 60m of no landings, or if queue depth rises despite landing activity - indicates batch stall or new throttle logic defect.

**Reference** — wiki/notes/queue-management.md

**Matches** `^Queue (admission )?throttled:.*CERTIFIED depth (\d+).*threshold.*drain`

### Queue throttle lifted

`sop-queue-throttle-lifted`

**Symptom** — Watchtower files an informational incident when queue depth drops below the release threshold after a throttle period, indicating recovery to normal operation

**Check** — Confirm queue depth in payload is below the release threshold (typically 6) and that builder admission can proceed

**Fix** — None — this is a recovery notification, not a failure state. Throttle lifted is the desired outcome.

**Escalate** — None — normal operation

**Reference** — wiki/notes/queue-management.md

**Matches** `^Queue throttle lifted:.*CERTIFIED depth now (\d+).*Builder admission is no longer throttled`

### Queue throttle lifted recurrence

`sop-queue-throttle-lifted-recurrence`

**Symptom** — Watchtower repeatedly files "Queue throttle lifted" incidents (multiple times over days) even though the condition is normal queue recovery

**Check** — Confirm queue depth is below release threshold (normal state), then check watchtower logs for repeated throttle-check runs with same CERTIFIED depth < release_at condition

**Fix** — watchtower.sh --throttle-check lines 198-216 fire the incident on every timer interval when depth < release_at, not just on transition. Guard the incident filing (lines 202-215) to fire only once when the stamp is first removed. Option: check if stamp existed before removal, or remove the incident entirely since it's normal recovery with no action.

**Escalate** — Feature request for watcher predicate; requires code change to watchtower.sh

**Reference** — wiki/notes/queue-management.md

**Matches** `^Queue throttle lifted:.*CERTIFIED depth now.*filed (\d+) times`

### Server testdb suite budget

`sop-server-testdb-suite-budget`

**Symptom** — Server-mode fixture test times out after ~30s wall when it declares # timeout: 240 or higher, because suites.sh (timed suite runner) allocates time from a shared 420s budget competing with 46 other tests; late in a run, insufficient time remains.

**Check** — grep "SPIRA_TESTDB_MODE=server" spira/test-*.sh | head -3 && grep "^BUDGET=" spira/suites.sh && grep "# timeout:" spira/test-landing-rebase.sh

**Fix** — Move the server-mode test to gate-suites with a justification in the file explaining that it needs a dedicated per-suite timeout (600s) to accommodate fixture initialization. Each test in gate-suites gets its own timeout slice rather than competing for the shared budget. Update gate-suites comment explaining budget constraints: "A test using server-mode testdb (+84s init) with a 240s+ # timeout: declaration cannot reliably complete in the 420s shared timed-run budget when queued late; promote to gate-suites for dedicated per-suite timeout."

**Escalate** — Infrastructure decision (budget vs. gate) if test cannot be removed from timed suite or converted to embedded mode.

**Reference** — wiki/notes/server-testdb-suite-budget.md

**Matches** `(SIGTERM|timeout|watchdog.*killed).*test.*server.*testdb|testdb.*server.*\(slow\|84s\|overhead\)`

### Sopsh applied ledger contention

`sop-sopsh-applied-ledger-contention`

**Symptom** — `sop.sh applied` takes 60-120s+ or hits a caller's short `timeout` (exit 124) when called against a bead Ops has reclaimed many times (applied.jsonl has 25+ entries for one bead, ~230KB/579+ lines total). A short-wall session can burn its budget on one call. On repeat, calls can hit 124 with NO entry appended (verified via wc -l before/after) -- killed before append, not just slow.

**Check** — Run two `sop.sh applied` calls on the same bead with short timeouts back to back; diff applied.jsonl line count before/after. Both timing out with no line added confirms this.

**Fix** — Ops cannot edit sop.sh (production, out of scope here). Mitigate: don't wrap `applied` in a short timeout on a hot bead -- background it and poll instead; size timeouts 150s+. Escalate to a builder to profile for lock contention on the growing ledger and/or a wiki regen+commit step run every call; candidate fixes: append-only/sharded ledger, batched wiki regen, or documented latency. See sp-dnu2c.

**Escalate** — If profiling finds a different mechanism (stuck flock, network call), amend this SOP rather than filing a new one.

**Reference** — wiki/notes/sop-sopsh-applied-ledger-contention.md

**Matches** `sop\.sh applied.*(hang|timeout|124|slow).*(concurrent|lock|contention)|applied\.jsonl.*(lock|contention|hang)`

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
git log origin/main | grep -qE 'sp-hrkwa.*disable.*local.*gate|SPIRA_QUEUE_LOCAL_GATE' && echo "pass" || echo "fail"
```

**Fix**

```
Disable local pre-flight gate via SPIRA_QUEUE_LOCAL_GATE=0 (sp-hrkwa). Operator decision 2026-09-17: local gates buy latency, CI buys coverage. Let CI be the gate; batch PR opens without local pre-flight. Resolves landing loop freeze.

WATCHER NOTE: watchtower.sh --throttle-check fires "deep+stalled" incident while landing loop is legitimately frozen pending async gate implementation. This creates recurring false-positive incidents (sp-pw5qw pattern). Fix: watchtower.sh should consult the same CHECK before escalating. If async gate commits not on main, the stall is deliberate—suppress alert. Filed as sp-f2h4h.
```

**Escalate** — none — this is an Ops incident with operator decision documented 2026-09-17 20:11. Solution implemented as sp-hrkwa and landed.

**Reference** — docs/sp-hsxk8-sop.md

**Matches** `(landing loop|batch.*gate.*mutex|landing pass.*freeze.*gate)`

### Sp kogm batched lifecycle gap

`sop-sp-kogm-batched-lifecycle-gap`

**Symptom** — Oldest unsent branch exceeds 24h threshold (SP_UNSENT_OLDEST_H). Branches remain unsent indefinitely. Root cause: ~130 pure ancestor branches (LANDED landstate, ahead=0) are rejected by sending.sh because lib.sh content_laden() checks ahead count BEFORE testing merge-base ancestry. These branches have content on main but fail the "ahead" test, causing sending.sh to skip them.

**Check** — Verify SP_UNSENT_OLDEST_H threshold via cockpit.env. Identify old unsent branches with git branch -r --contains origin/main. Verify lib.sh content_laden() orders checks: ahead before ancestry = bug.

**Fix** — Reorder lib.sh content_laden() to check ancestry FIRST via merge-base --is-ancestor, then check ahead count. This allows sending.sh to correctly identify pure ancestor branches as "landed" and reap them. Fix deployed via sp-val2d (commit 3a1673e: "lib.sh: fix content_laden to check ancestry before rejecting ahead=0"). Verified with tests: test-check5-content-laden.sh 12/12 passing, test-sending-certified-guard.sh 9/9 passing.

**Escalate** — Fix is deployed in sp-val2d CLOSED bead but NOT YET on origin/main—blocked on queue/batch pipeline landing. If sp-val2d has not landed within 24h of this session, escalate to queue/batch team or operator to merge sp-val2d to main. This unblocks sending.sh branch reaping and clears alert condition.

**Reference** — sp-val2d (commit 3a1673e), incident sp-kogm (184+ recurrences over 7 days), lib.sh:233 content_laden()

**Matches** `sp-kogm|oldest.*unsent|sending.*oldest.*unsent|content_laden`

### Sp kogm direct requeue ops

`sop-sp-kogm-direct-requeue-ops`

**Symptom** — Stranded beads (CLOSED, commits not on main, no open batch) with escalations to queue/batch team that close without action. Alert recurs 65+ times. Escalation mechanism broken at "queue/batch acts" step.

**Check** — Verify beads are CLOSED, commits not on origin/main, no open batch_id. Branches exist with landing-ready commits.

**Fix** — Ops decision (sp-nmu4v): Proceed with Option A — Ops directly re-queues/lands stranded beads. Default recommendation accepted; escalation mail sent to operator with this default; proceeding with immediate action. This is reversible.

**Escalate** — none — decision made via prior escalation, default recommendation now implemented

**Reference** — wiki/notes/stranded-bead-detection.md, sp-nmu4v (escalation bead)

**Matches** `sp-kogm|stranded.*bead|requeue.*escalat`

### Sp kogm sending age

`sop-sp-kogm-sending-age`

**Symptom** — Oldest unsent branch in spira/* refs exceeds 24h threshold

**Check** — Identify the oldest unsent branch: (1) Extract name and timestamp from cockpit SP_UNSENT_OLDEST_H; (2) Find which branch is actually oldest by sorting spira/* refs by timestamp; (3) Determine its type: STRANDED (closed + not on main + not in open batch), BACKUP/SAFETY (backup-* prefix or queue/*), or QUEUED (active bead, not closed). Only STRANDED and certain QUEUED states are Ops's to fix; BACKUP branches are noise.

**Fix** — For STRANDED beads: escalate to queue/batch team WITH EXPLICIT REQUIREMENT that they must re-queue or manually land the bead—not just close the escalation. Verify escalation was acted upon before closing this incident. For QUEUED but BLOCKED: investigate queue system. For BACKUP branches: file separate incident for backup-branch retention (sp-86cne or equivalent).

**Escalate** — STRANDED bead with commits not on main and no open batch holding it — needs queue/batch team to manually re-queue or amend landing state. Backup branch policy — whether old backup branches should be preserved or reaped — is operator decision, not Ops's to implement.

**Reference** — wiki/notes/sending.md, incident:sp-8jany-stranded-batched, sp-3tnua-stranded-sp-zmd3u, sp-86cne-backup-branch, sp-vnjhj-escalation-systemic-failure

**Matches** `sending.*oldest unsent branch.*threshold`

### Sp rfdx9

`sop-sp-rfdx9`

**Symptom** — test-install-dolt-breaker.sh is a red-green flake: rc=1 after 6s under parallel load, green in 9s serially. Assertion fails with "wanted [bd did not accept]" but haystack is empty (output not yet flushed).

**Check** — bash spira/test-install-dolt-breaker.sh 2>&1 | tail -1 | grep -q "0 failed"

**Fix**

```
Two issues:
  1. Buffering race: add wait_for_output() polling function that waits for expected strings to appear in output file (handles async flushing under concurrent load).
  2. Runtime environment: when running in Ops context with production dolt-beads.service active, set SPIRA_INSTALL_CONFLICT_CONSIDERED=1 to bypass install.sh's conflict check (test uses mock bd with temp data dir, real dolt server is irrelevant).
  3. Port selection: use original port 19142 (not 31415, which conflicts with production).
```

**Escalate** — if other suites also fail with port conflicts, escalate to infra to assign test port ranges.

**Reference** — wiki/notes/test-install-dolt-breaker.md

**Matches** `test-install-dolt-breaker\.sh.*red-green|rc=1.*parallel.*buffering|want.*bd did not accept.*\[.*\]`

### Stale worktree ref cleanup

`sop-stale-worktree-ref-cleanup`

**Symptom** — After beads close and commits land on origin/main, local branch refs (refs/heads/spira/<bead-id>) and worktrees persist indefinitely. Verified on sp-xhc15: CLOSED, c26bd5b on origin/main, but refs and worktree still exist 81h+ old. Pattern affects 151+ beads, causes sp-kogm watcher to repeatedly alert on "unsent" branches.

**Check** — Verify beads are CLOSED and their commits are on origin/main; confirm local refs and worktrees still exist. Example: git merge-base --is-ancestor <commit> origin/main && ls -d $SPIRA_RUN/worktree/<bead-id>

**Fix** — ESCALATE to operator — design decision on cleanup ownership: (A) queue.sh: after merge succeeds, delete local branch before bead closes; (B) landing.sh: after landing, delete branch+worktree; (C) batch.sh: add branch cleanup to post-landing phase; (D) bead close: delete branch/worktree when bead closes if commit is on origin/main. Recommended: queue.sh after merge succeeds (point of state convergence). Default accepted, implementation deferred to operator's selection.

**Escalate** — Operator chooses component ownership; builder implements in that component; no Ops action beyond escalation

**Reference** — wiki/notes/local-ref-cleanup.md, sp-cw7rn

**Matches** `stale.*ref|stale.*worktree|151.*bead|unsent.*branch.*cleanup`

### Stranded backup branch

`sop-stranded-backup-branch`

**Symptom** — Old unsent branch unowned by any bead, blocking unsent-age alerts. Branch appears to be a safety copy created before rebasing work.

**Check** — (1) Verify the bead the backup references exists and landed (grep bead id from commit message in main's log). (2) If it landed as rebased commits with different hashes, it's safe to delete. (3) If no corresponding landed work exists, the backup may contain stranded commits needing investigation.

**Fix** — Delete the backup branch with git branch -D <branch> once you confirm the work it backed up has landed on main.

**Escalate** — If the backup branch has commits not present on main, investigate why the work didn't land before deletion.

**Reference** — wiki/notes/

**Matches** `backup-\w+-preRebase`

### Template apt drift

`sop-template-apt-drift`

**Symptom** — Provision refuses with "::error::provision.sh: template <VMID> has apt automation enabled; reseal the template before provisioning" and exits 1. Every job in every repo pinning DeckDumpster/ephemeral-ci/provision@v1 fails at the provision stage. Live since v1 moved to 487a7d5 on 2026-09-23 03:58Z. Also the runbook for a planned reseal.

**Check** — The provision log line "provision.sh: masking apt automation in guest N" must be PRESENT; if absent, the action that ran predates the check and the run proves nothing either way. A guest-diag line reporting the units masked is NOT evidence: spira's gate.yml masks all three in-job before the diagnostic runs.

**Fix** — Operator console work, not an aeon's. Drain, confirm no runner clone is live with qm list, full-clone template 9110 to a new VMID, boot it, run 'sudo bash scripts/template-substrate.sh' from ephemeral-ci main inside it, confirm 'systemctl is-enabled unattended-upgrades.service apt-daily.timer apt-daily-upgrade.timer' prints masked three times, poweroff, convert to template, set org variable TEMPLATE_VMID to the new id. Prove it with one dispatched run whose provision log shows the masking line and no ::error::; cite that run id. Roll back by setting TEMPLATE_VMID to 9110. Not one-off: reseal again whenever the substrate script changes.

**Escalate** — Always — Proxmox console and organisation-variable write are Ryan's alone. Raise it on sp-400g1 rather than filing new work. Set PVE_CA_CERT in the same sitting: teardown now requires it with no insecure fallback, and a failing teardown leaks one VM per run while the reaper is red.

**Reference** — brain wiki/notes/reseal-ci-runner-template.md

**Matches** `apt automation enabled|resea(l|ling)(\s+[A-Za-z0-9-]+){0,3}\s+template|template(\s+[A-Za-z0-9-]+){0,2}\s+resea(l|ling)|template[\s-]reseal|apt-daily(-upgrade)?\.timer.*enabled|unattended-upgrades=enabled`

### Test answers premise rejected watcher silent

`sop-test-answers-premise-rejected-watcher-silent`

**Symptom** — test-answers-premise-rejected.sh fails in suite run with "the watcher printed the verdict: wanted [ANSWERED] in []" and "it woke the reader exactly once: wanted [1] got [0]". Test passes consistently when run in isolation with 20/20 pass.

**Check** — bash spira/test-answers-premise-rejected.sh and verify it passes (20 pass, 0 fail)

**Fix** — Test passes in isolation, so the issue is environmental or a race condition specific to suite execution. Likely causes: (1) mark file contention if tests run in parallel, (2) cockpit_attention_beads timing issue with concurrent database access, (3) emit() output buffering in subshell under timeout. Since issue does not reproduce in isolation, it is a suite-environment issue, not a code defect. Suite-level fix: ensure serial test execution or isolate database per test.

**Escalate** — Not Ops work — this is a suite/CI environment configuration issue for the team to address.

**Reference** — wiki/notes/watcher-suite-timing.md

**Matches** `test-answers-premise-rejected.sh.*the watcher printed the verdict.*wanted \[ANSWERED\] in \[\]`

### Test sop

`sop-test-sop`

**Symptom** — test symptom

**Check** — echo test

**Fix** — echo test

**Matches** `test`

### Test suite batch isolation

`sop-test-suite-batch-isolation`

**Symptom** — Test suite passes all cases when run individually but fails intermittently in batch verdict mode. Failures are transient and trigger repeat-refused logic.

**Check** — Run `bash suite.sh` individually (passes); then run two instances concurrently in background. If concurrent runs fail while individual passes, this SOP applies.

**Fix** — Audit for five common batch isolation gaps: (1) shared temp state, (2) HOME dir not created, (3) symlink race conditions, (4) file mutations without locking, (5) no per-instance isolation. Apply in order: create directories explicitly; copy shared dirs per run; use atomic symlink updates; wrap mutations in flock or mktemp -d; or mark serial-only.

**Escalate** — If fixing requires rewriting test framework, escalate to operator: "this test gate needs parallel-safety refactoring".

**Reference** — wiki/notes/test-suite-batch-isolation-gaps.md

**Matches** `test suite.*pass.*individual.*fail.*batch|batch.*environment.*isolation.*test`

### Testenv basic target transient retry

`sop-testenv-basic-target-transient-retry`

**Symptom** — testenv-batch.sh fails to start container with "systemd did not reach basic.target" timeout. Occurs sporadically under high concurrent container load (7+ containers); same containers boot successfully on retry.

**Check** — podman run --systemd=true ubuntu:24.04 /lib/systemd/systemd; sleep 2; podman exec <container> systemctl is-active basic.target

**Fix** — Wrap systemd basic.target check in retry loop with 2s backoff. If transient (retries succeed), distinction from permanent misconfiguration is established. If both attempts fail, issue is real. testenv.sh lines 333-356: add _wait_for retry on first timeout before giving up.

**Escalate** — If retries still fail after code fix, escalate to infrastructure for resource limit diagnosis (pids, inotify, cgroup, systemd-in-podman limits under concurrent container load).

**Reference** — wiki/notes/standard-operating-procedures.md

**Matches** `testenv.*system systemd did not reach basic.target`

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

### Unclaimable missing scope label

`sop-unclaimable-missing-scope-label`

**Symptom** — A ready bead is unclaimable because it lacks the required scope label (spira)

**Check**

```
bd -C $SPIRA_DB show <bead-id> --format json | jq '.[] | .labels | contains(["spira"])'
```

**Fix**

```
bd -C $SPIRA_DB label add <bead-id> spira
```

**Escalate** — If the bead type is not task/incident, verify with the operator before adding the label; a decision bead with no scope label may indicate a misfiled bead

**Reference** — wiki/notes/unclaimable-ready.md

**Matches** `external_ref contains unclaimable: and description mentions missing scope label`

### Unclaimable partition label

`sop-unclaimable-partition-label`

**Symptom** — Bead is marked UNCLAIMABLE because it lacks a required partition label from the persona system

**Check**

```
bd show <bead> | grep -E '\b(groom|incident|maechen-sweep|plan|spike|czar-trigger)\b'
```

**Fix**

```
1. bd label add <bead> incident
   CRITICAL: label must be bare name (incident, plan, spike, etc.) — conf.sh's SPIRA_*_LABEL
   defaults carry no prefix, and every FAYTH_LABELS in spira/chamber/*.fayth reads the bare form.
2. If a bead-creation call site is still writing a dimension-prefixed label, that site is the
   recurring root cause, not this bead — fix the call site (sp-e2u7d fixed the last three:
   gate-spira.sh, review.sh, incident.sh).
```

**Escalate** — If partition type other than incident, pick the correct persona and use its bare partition name. If recurrence, code is creating beads without calling partition labeling.

**Reference** — wiki/notes/standard-operating-procedures.md

**Matches** `unclaimable.*no matching partition|no persona's partition labels`

### Verdict cache collision

`sop-verdict-cache-collision`

**Symptom** — Bead closes successfully but cached verdict file persists on disk. When a different bead later produces the same tree hash (via rebase without code changes, or collision), it reads the stale cached verdict and incorrectly rejects a valid pass or accepts a failed one.

**Check** — 1. Verify gate_key() in gate.sh line 365-378 omits bead ID and branch name. 2. Verify landing.sh line 383-386 only cleans verdicts by mtime TTL, never on bead close. 3. Count verdict cache files in $SPIRA_RUN/verdicts; old entries persist despite beads closing.

**Fix** — Add bead ID to gate_key() cache key (eliminates cross-bead collision) OR add explicit cache invalidation in bead-close logic to rm $VERDICT_DIR/$GATE_KEY when bead closes. First option preferred: makes each bead have isolated cache entry.

**Escalate** — Developer to implement cache key redesign (add $SPIRA_BEAD_ID to gate_key()) or close-time invalidation logic.

**Reference** — wiki/notes/gate.sh#verdict-cache-key-design

**Matches** `(gate\.sh verdict cache|verdict cache|gate_key|GATE_KEY|tree hash collision)`

### Verdict cache stale on closed bead

`sop-verdict-cache-stale-on-closed-bead`

**Symptom** — A closed bead shows a RED verdict in the cache or recurrence check, contradicting its VERDICT=PASS close reason. The repeat-refused mechanism detects this and blocks re-runs.

**Check** — (1) Verify the bead is CLOSED with VERDICT=PASS in close reason. (2) Check $SPIRA_RUN/verdicts/ for cached verdict with .RED or failure markers. (3) Verify current code/branch passes all tests. If current code passes but cached verdict is RED, cache stale state detected.

**Fix** — (1) Identify the commit hash that produced the RED verdict. (2) Correlate with the verdict cache key in $SPIRA_RUN/verdicts/<hash>. (3) Review verdict.sh's cache invalidation on bead close, rebase, or conflict resolution. (4) If bead was rebased after close, stale cache entry should have been invalidated. (5) Clear the verdict cache entry: rm $SPIRA_RUN/verdicts/<hash> (6) Re-run the bead's tests to verify pass, or escalate to verdict.sh code review if pattern is systematic.

**Escalate** — If multiple beads exhibit this pattern, root cause is in verdict.sh cache key generation or invalidation logic. File child bead to review verdict.sh for hash collisions, branch-aware cache keys, and close-time invalidation.

**Reference** — wiki/notes/sop-verdict-cache-stale-on-closed-bead.md

**Matches** `(VERDICT=PASS.*later.*RED verdict|verdict.*persists.*after.*close|closed.*cached.*RED)`

### Verdict notify red

`sop-verdict-notify-red`

**Symptom** — Batch PR fails CI, verdict.sh runs and requeues or halves members, but operator receives no mail. Landstate shows CERTIFIED members. Batch file is gone. No mail in the queue.

**Check** — grep for 'verdict.*red\|together-only red' in landing.log; grep for 'red in PR' in mail queue ($SPIRA_MAIL_DIR). If the log shows a red verdict but no mail file exists for it, the notification path failed. Confirm verdict.sh version has _attr_notify_red: grep -n '_attr_notify_red' $HERE/verdict.sh

**Fix** — _attr_notify_red is now called from _q_attribute on every red verdict path (ejection, together-only halve, requeue-all). No manual intervention is needed once the fixed verdict.sh (sp-uu8oy, commit 43a136f) is in force. To confirm: run test-verdict.sh via testenv-batch.sh — case 8.5 covers the together-only path.

**Escalate** — If notifications are still absent after the fix, check mail.sh is reachable from the verdict context and that SPIRA_MAIL_DIR is set. A stuck mail-deliver service would queue the notification without delivering it — check spira-mail-deliver.service status.

**Reference** — sp-uu8oy

**Matches** `red queue batch verdict produces no operator notification; together-only break (ejected=0, requeued=N) goes unreported`

### Verdict repeat flaky

`sop-verdict-repeat-flaky`

**Symptom** — Suite failed initially, repeat attempt refused because SPIRA_VERDICT_REPEAT_CONSIDERED env var not set

**Check** — SPIRA_VERDICT_REPEAT_CONSIDERED="diagnostic reason" bash spira/testenv-batch.sh --suites <suite> spira/<branch> && grep -E "all suites passed|PASS" <output>

**Fix** — Set SPIRA_VERDICT_REPEAT_CONSIDERED environment variable when requesting verdict repeat to determine if failure is repeatable or transient

**Escalate** — Not applicable — this is procedural

**Reference** — sop-verdict-repeat-refused

**Matches** `SPIRA_VERDICT_REPEAT_CONSIDERED not set.*test.*failed.*Repeat attempt was refused`

### Verdict repeat flaky tests

`sop-verdict-repeat-flaky-tests`

**Symptom** — Verdict cache refuses re-run; tests pass individually but fail intermittently in batch

**Check** — Run each failing suite individually (--suites <suite1> --suites <suite2> etc) and verify they pass in isolation; then check batch execution for flakiness pattern

**Fix** — Do NOT override SPIRA_VERDICT_REPEAT_CONSIDERED. Verdict mechanism is correct. File a bead for test team to investigate environmental instability (resource contention, timing, state leakage). Identify suite-specific cleanup/isolation gaps.

**Escalate** — When verdict repeat-refused blocks landing and test team confirms intermittent failures, this is correct behavior — flaky tests must be fixed before they land

**Reference** — wiki/notes/verdict-repeat-refused-flaky-tests.md

**Matches** `repeat.*refused.*test.*fail`

### Verdict repeat refused

`sop-verdict-repeat-refused`

**Symptom** — A re-run of the same test suite on the same tree was refused because SPIRA_VERDICT_REPEAT_CONSIDERED was not set or too short (min 10 chars). Multiple recurrences indicate environmental rather than code defects.

**Check** — bash spira/testenv-batch.sh --suites <suite> <branch> and check if it passes. If it passes, the original red was environmental. NOTE: Aeons cannot run this CHECK due to lack of SSH credentials for remote branch access — escalate to operator for re-run or diagnose from available evidence.

**Fix** — Re-run the suite with SPIRA_VERDICT_REPEAT_CONSIDERED="environmental: <reason>" if you need to test the same tree again, or commit a fix if there's a code defect. For recurrences with documented environmental causes (e.g., runner OOM), escalate to operator for re-run once condition clears.

**Escalate** — If the suite continues to fail after re-run with justification, file a new bead documenting the defect. For aeon-claimed beads with remote branch access needed, escalate to operator for re-run coordination.

**Reference** — wiki/notes/standard-operating-procedures.md

**Matches** `repeat.refused|repeat-refused`

### Verdict requeue on repro fail

`sop-verdict-requeue-on-repro-fail`

**Symptom** — Batch fails red; verdict requeues all members rather than ejecting culprit. Sign: ejected=0 in verdict log after red batch.

**Check** — Manual repro of failing suites against broken commit passes, but verdict failed to eject. This means testenv-batch.sh mounts production (main) rather than the branch under test.

**Fix** — testenv-batch.sh must check out the branch before running suites. Until sp-2f51e lands, hand-eject via batch.sh: write RED <sha> to landstate with reason, re-run batch.

**Escalate** — None — sp-2f51e tracked.

**Reference** — sp-2f51e, sp-uu8oy

**Matches** `verdict requeues all batch members instead of ejecting the broken one`

### Watchtower unit bullet extraction

`sop-watchtower-unit-bullet-extraction`

**Symptom** — watchtower filed incidents with unit name "●" (UTF-8 bullet character) instead of the actual systemd unit name. Caused invalid journalctl commands in incident descriptions and SPIRA_INCIDENT_REF pointing to "incident:failed-unit-●".

**Check** — journalctl --user -u spira-watch-answers-prod.service -n 5 >/dev/null 2>&1 && echo "Real unit is queryable"

**Fix** — watchtower.sh lines 528-535: replace unit name extraction `${_fu_line%% *}` with sed-based extraction that skips non-alphanumeric leading characters (including systemctl's bullet). The fix: `sed -E 's/^[^[:alnum:]]+ //; s/ .*//'`

**Escalate** — none

**Reference** — sp-ezeiy: watchtower was extracting systemctl's formatting bullet as a unit name

**Matches** `Invalid unit name "●" escaped|FAILED UNIT: ● failing`

### Worktree accumulation

`sop-worktree-accumulation`

**Symptom** — Root filesystem at critical capacity; accumulated worktrees from completed beads consuming significant storage

**Check** — du -sh $SPIRA_RUN/worktree && ls -d $SPIRA_RUN/worktree/* 2>/dev/null | wc -l

**Fix** — Identify completed/closed beads with corresponding worktrees and remove stale worktrees; implement automated cleanup after bead closure

**Escalate** — Implement automated worktree cleanup mechanism — storage management requires operator decision on retention policy and deployment

**Reference** — wiki/notes/standard-operating-procedures.md

**Matches** `filesystem 8[0-9]% used|filesystem 9[0-9]% used|Root .* filesystem.*approaching critical`

### World drain stall

`sop-world-drain-stall`

**Symptom** — World is in DRAINING state for extended period, blocking new aeon summons while in-flight aeons complete

**Check** — SPIRA_AEON_OVERRIDE=1 world.sh drain --timeout 0

**Fix**

```
If 16+ aeons have been live for >15 minutes during drain, summons are likely unnecessarily blocked.
  Resume with: SPIRA_AEON_OVERRIDE=1 world.sh resume
  Resuming does not kill in-flight aeons—they complete normally while new summons proceed.
```

**Escalate** — None (Ops owns this)

**Reference** — wiki/notes/standard-operating-procedures.md

**Matches** `DRAINING.*summons gated|world.sh.*drain.*NOT DRAINED`

Related: [[spira]], [[common-law]], [[codified-judgement]]
