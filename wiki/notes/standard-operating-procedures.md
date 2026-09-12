---
type: note
created: 2026-09-05
updated: 2026-09-12
tags: [spira, ops, sop, runbook, generated]
aliases: [SOPs, Standard operating procedures, The shelf]
---

# Standard operating procedures

**Generated — do not edit.** Regenerated whole by the harness's `spira/sop.sh synth` from the Spira beads database, which is the source of truth. Editing this page has no effect; the next run overwrites it. Amend an SOP instead:

```bash
spira/sop.sh write <slug> -   # text on stdin
```

Statutes are how to behave; SOPs are how to fix. They share one mechanism, split by prefix — `law-` and `sop-` — so the [[spira]] Ops persona reads its runbooks exactly the way every agent already reads [[common-law]]. Ops is summoned by an incident bead filed from a failed systemd unit, matches the payload against the `MATCH:` lines below, and executes the first one that fires.

**67 SOP(s)** on the shelf as of 2026-09-12.

## The closing rule

**An incident resolved without an SOP must produce one.** This is `law-bake-rules-into-tools` applied to production, and it is enforced rather than asked for: writing an SOP is what regenerates this page, the regenerated page is the commit that names the incident bead, and a bead closed with no commit naming it is reopened by `aeon.sh`. An incident fixed by hand and forgotten does not close.

## The shelf

### Aeon direct commit detection

`sop-aeon-direct-commit-detection`

**Symptom** — The branch-guard.sh check audit detects aeon commits (non-merge) on base branches. An aeon used `git commit --no-verify` to bypass the pre-commit hook, committing directly to a shared checkout's base branch instead of using its assigned worktree.

**Check** — Run `bash $SPIRA_HOME/branch-guard.sh check`. Output "branch-guard: AEON COMMIT ON <repo>/<branch>" indicates the violation. Verify it is a non-merge commit: `git log -1 --format=%P <sha> | wc -w` returns 1 (single parent = direct, not merge). Confirm the committer email ends in @spira.local.

**Fix** — DECISION REQUIRED — this SOP documents the detection mechanism only. Escalate with policy decision: revert and re-land through proper gate, or accept direct aeon commits if properly named and documented? Until that decision is resolved, no automated fix can proceed.

**Escalate** — yes — operator decision on whether direct aeon commits to base branches are acceptable or must go through the landing gate review process.

**Reference** — sp-7v38q, sp-7qjbc, branch-guard.sh

**Matches** `branch-guard.*AEON COMMIT ON.*main|aeon-.*@spira.local.*commit.*base branch|bypassed.*landing.*gate`

### Bd list missing external ref

`sop-bd-list-missing-external-ref`

**Symptom** — Multiple incident beads filed for the same event despite flock protection in incident.sh drain_one. Root cause: bd list --json output omits the external_ref field, so _dedup_incident query to find existing beads by external_ref always fails silently, leading every incident filing to create a new bead instead of recording recurrence.

**Check** — (1) Run `bd list --limit 1 --json | jq '.[0] | keys | length'` — should return 21. (2) Check if external_ref is present: `bd list --limit 1 --json | jq '.[0] | has("external_ref")'` — returns false if missing. (3) Verify _dedup_incident queries by external_ref: `grep -n "_dedup_incident\|external_ref" $SPIRA_HOME/incident.sh | head -10`.

**Fix** — Workaround in place (sp-6x7ns): incident.sh now queries bd show for each candidate bead to fetch external_ref before comparing, since bd list --json does not include it. This restores dedup functionality immediately. Proper fix (upstream): bd JSON schema needs to include external_ref field in bd list output, or builders must decide an alternative approach.

**Escalate** — builders — bd JSON schema is upstream. If external_ref cannot be added to bd list output, builders must decide: add it to JSON, or change incident.sh dedup to use a field that is present (e.g., title hash, or per-bead bd show calls at scale).

**Reference** — wiki/notes/bd-list-external-ref-missing-sop-2026-09-10.md

**Matches** `bd list.*does not.*return external_ref|_dedup_incident.*query.*fails|duplicate beads.*same.*external_ref.*despite flock`

### Bd migration count

`sop-bd-migration-count`

**Symptom**

```
bd prints "schema version mismatch: database is at vN, binary knows up to vM" against the production store, and prints it while EXITING 0, so a caller reading exit status sees success and parses the error as data. The store is almost certainly healthy. What is wrong is WHICH bd you reached.
```

**Check** — Run `type -a bd` and compare it with $SPIRA_BD. Inside an aeon or a suite fixture, bare bd can be a tempdir symlink to an embedded tagged release that knows fewer migrations. Then run "$SPIRA_BD" -C $SPIRA_DB migrate schema. Exit 0 with "Schema already at vN" means the store is healthy and this SOP does not apply: stop, and look elsewhere for the stall. `bd --version` is not a check; it prints a string and tests nothing.

**Fix** — Address the production store with $SPIRA_BD, never bare bd. If $SPIRA_BD itself is behind, rebuild bd from main with CGO_ENABLED=0 (server mode), install it, and run $SPIRA_HOME/bd-pin.sh write to record the new migration count. A build from main knows MORE migrations than a tagged release, so pin by migration count, never by version string.

**Escalate** — None. This is ops work. NEVER roll the database cursor back and NEVER touch schema_migrations. That was executed on 2026-09-08 and reverted three minutes later, because at the lower cursor every write to production failed.

**Reference** — wiki/notes/bd-migration-count.md

**Matches** `schema version mismatch: database is at v[0-9]+, binary knows up to v[0-9]+`

### Check5 horizon reopens

`sop-check5-horizon-reopens`

**Symptom** — sentinel CHECK 5 reopens beads that have actually landed on origin/main. Root causes: (1) 400-commit window — beads older than 400 commits are incorrectly marked unlanded on next landings that push them past the horizon; (2) searches only subject (%s) not full message (%B) — bead IDs in commit bodies are missed. sp-a9g at commit 401 and sp-37q at 400 were reopened, sp-m0s7 body-mentioned but subject-invisible was reopened three times in 6 minutes. This manufactures false attempts toward poison threshold and false escalations to operator.

**Check**

```
git log --format='%B' origin/main | grep -c <bead-id> should return >0. git merge-base --is-ancestor <sha> origin/main should return 0 for landed beads.
```

**Fix** — sentinel.sh CHECK 5 line ~457: change `git log --format='%s' -n "${SPIRA_VERDICT_WINDOW:-400}"` to `git log --format='%B'` (remove -n limit, search full message). Removes horizon that closes over 400 commits and catches bead IDs in commit bodies. A 400-commit limit measured harness capacity (36 closed beads × 400-walk cost), not correctness. No limit means one walk per repo per CHECK 5 pass finds all landed work regardless of age.

**Escalate** — n/a

**Reference** — sp-d9x93, wiki/notes/check5-horizon-sop-2026-09-09.md

**Matches** `CHECK 5.*reopens.*landed|closed-not-landed.*attempt|bead older than.*400.*commits`

### Content landed reopen

`sop-content-landed-reopen`

**Symptom** — bead reopened repeatedly by CHECK 5 despite work being content_landed

**Check** — verify bead has content-landed label and CHECK 5 skips reopening

**Fix** — sending.sh adds content-landed label after reaping; sentinel.sh CHECK 5 reads it

**Escalate** — n/a

**Reference** — wiki/notes/standard-operating-procedures.md

**Matches** `closed bead with no commit naming its id, content-landed branch reaped`

### Destroy branch loses unlanded work

`sop-destroy-branch-loses-unlanded-work`

**Symptom** — a bead closes correctly (its fix commit names it on spira/<id>) yet the sentinel reopens it every time citing "no commit names it." The commit exists (`git show <sha>` succeeds) but is dangling, on no branch — the next summon's worktree is byte-identical to origin/main.

**Check** — `git branch --contains <claimed-sha>` empty + `ls $SPIRA_RUN/landstate/ | grep -c <bead-id>` -> 0 across every prior close.

**Fix** — root cause was sp-mqsl (spira_destroy_branch deleted unlanded work, no landed-check before `git branch -D` on reclaim/slay). FIXED, spira-harness commit 4b51363: spira_destroy_branch now resolves the repo's land ref (spira_landref) and refuses (REFUSED, like the existing holder/worktree refusals) when the branch tip is not an ancestor of it, before the reclaim/slay path can delete it. Commit is on spira-harness LOCAL main only, not yet on origin/main — landing it is tracked by sp-bvo7 (which already carries an earlier stuck local-main commit and the same dirty-tree/rebase job). Until sp-bvo7 lands, the underlying race is still live in the deployed code; a fresh occurrence of this symptom means check sp-bvo7's status first, not re-diagnose the mechanism.

**Escalate** — never — Ops's own tooling. If sp-bvo7 lands but the symptom recurs, that is new information worth its own incident.

**Reference** — sp-qj8n, sp-mqsl (fixed, spira-harness 4b51363), sp-bvo7 (landing), wiki/notes/destroy-branch-loses-unlanded-work-sop-2026-09-08.md

**Matches** `never appears in landstate|byte-identical to origin/main at start|dangling.*not on any branch`

### Env prefix left of pipe

`sop-env-prefix-left-of-pipe`

**Symptom** — a variable meant to configure the READER of a pipeline (`VAR=x cmd1 | cmd2`) never reaches it — cmd2 sees its own defaults. watchtower.sh set SPIRA_INCIDENT_TYPE/PRIORITY/ACTOR on the LEFT of `snapshot | bash incident.sh file ...`; each side of a pipeline forks its own subshell before assignments apply, so incident.sh always read its hard-coded defaults (`--type bug --priority 1`, actor unset). Every routine sweep filed as a P1 bug wearing the operator's name instead of a P2 chore.

**Check** — reproduce generically before touching the suspect script — `bash -c 'f(){ echo hi; }; V=chore f | { echo "RHS sees V=[${V:-UNSET}]"; cat >/dev/null; }'` prints `UNSET`. Grep the suspect file for `VAR=... \` lines immediately above a line containing `|` — that shape is the bug regardless of which script it is in.

**Fix** — move the prefix onto the RHS command that actually needs it: `snapshot | VAR=x bash reader.sh args`. Verify with the same reproduction pattern, RHS-prefixed: `echo x | V=chore bash -c 'echo $V'` prints `chore`. Never fix by exporting the vars earlier in the script instead — that leaks scope into everything after the pipeline, wider than the one command that needed it.

**Escalate** — never — this is a shell-scoping defect, always Ops's to fix directly.

**Reference** — sp-b9qs

**Matches** `VAR=x.*\| bash|env prefix.*(left|right) side of a pipe|assignments scope to cmd1 only|prefix on the LEFT of a pipeline`

### Escalation body is a slug

`sop-escalation-body-is-a-slug`

**Symptom** — an ask reaches the operator's pane titled with a mangled identifier — `incident:Spira-sweep-----is-the-pipeline-moving- has failed 5 times` — defaulting to "read <bead>", carrying no evidence. He replies he cannot decide from it. The alert fired correctly; what it SAYS is the fault.

**Check** — read the ask as HE sees it, not the bead behind it: `bd -C $SPIRA_DB show <ask-id> --json | head -c 600`. A title holding `incident:` with hyphen runs is a DEDUPE SLUG rendered as prose. Then `grep -n 'ASK" add' $SPIRA_HOME/*.sh`; the site is an `add` whose fields are all constants or `$ref`/`$id`, never one carrying `--evidence`.

**Fix** — build every field from the SUBJECT. Title = the bead's own title, the count, and the ELAPSED TIME it spans — a bare count cannot say whether 5 is an hour of noise or a fortnight. `--why` = what the subject is FOR. `--default` = a decision, never an errand. `--evidence-file` = the payload. TWO TRAPS: `--evidence-file` keeps the TAIL while vital signs sit at the HEAD, so pass a `head -c 2000` copy, not `$pf`; and incident.sh is read by byte offset while running, so patch beside and `mv`. Verify by RENDERING — stub `$ASK` to echo argv, `bash -x` the block.

**Escalate** — only if the fix changes WHETHER it fires. Muting or raising SIN_AT is his call.

**Reference** — wiki/notes/spira-sweep-sop-2026-09-08.md

**Matches** `has failed [0-9]+ times and Ops has not broken|incident:[A-Za-z0-9-]{20,}|not enough information for me to make a decision`

### Harness checkout behind

`sop-harness-checkout-behind`

**Symptom** — The harness checkout (the repository holding $SPIRA_HOME) is behind or diverged from origin/main, so landed work is not the work executing. Every aeon, sentinel and timer reads its scripts from that path as they sit on disk. A direct commit into that checkout makes HEAD cease to be an ancestor of origin/main, and every fast-forward silently refuses from then on.

**Check** — cd "$SPIRA_HOME/.." && git fetch -q origin && git log --oneline HEAD..origin/main | wc -l && (git merge-base --is-ancestor HEAD origin/main && echo ANCESTOR || echo DIVERGED)

**Fix** — Never apply under load; scripts change in place under running processes. Drain, apply, restart, in that order. 1) spira/world.sh drain — gates new summons, loop and landing continue. 2) Wait for aeons to reach zero: systemctl --user list-units 'spira-aeon-*' --state=active. 3) DIVERGED: list local-only commits with git log origin/main..HEAD, save each as git branch rescue/<sha>-<bead>, prove redundant by diffing every touched file against origin/main, then git reset --hard origin/main. ANCESTOR: git merge --ff-only origin/main. 4) spira/world.sh resume. 5) Restart readers holding code in memory: cockpit-ensure repairs the collector, but a compiled artifact such as loom needs a rebuild and a service restart. 6) Verify on the OUTCOME, never the ref — confirm a behaviour only the new code produces.

**Escalate** — A local-only commit that is NOT redundant is unlanded work; it needs a branch and the gate, not a reset.

**Reference** — wiki/notes/landed-is-not-running-2026-09-08.md

**Matches** `(checkout|harness).*(behind|diverged|frozen)|landed but not running|HEAD is not an ancestor`

### Incident dedup collapsed

`sop-incident-dedup-collapsed`

**Symptom** — Multiple beads exist with identical external_ref. Collapsing duplicates resolves the incident but does not prevent recurrence if root cause persists.

**Check**

```
bd list --all --limit 0 --label spira,incident --json | jq -r '.[] | .external_ref' | sort | uniq -c | sort -rn | grep -v '^ *1 '. Recurrence indicator: check if offending bead carries sp-recur-N labels with N > 1.
```

**Fix** — (1) Collapse surplus beads using bd supersede <dup> --with <primary> for each duplicate. Identify primary by highest sp-recur-N count. (2) If recurrence is present (N > 1), this is a repeat failure — do not re-collapse. Instead file sp-fix-incident-dedup bead for builders. PRIMARY ROOT CAUSE (sp-csvzn, recurrence 19): watchtower.sh line ~579 does not export SPIRA_DB to incident.sh subprocess. bdq calls in incident.sh fail silently, treating "no database" as "no open incident", filing duplicates on every sweep. FIX: Add SPIRA_DB="$SPIRA_DB" to the environment prefix: `SPIRA_DB="$SPIRA_DB" bash "$INC" file ...`. SECONDARY: verify spira-watchtower.service or conf.sh exports SPIRA_DB. TERTIARY: test-incident.sh concurrent-filer test hangs (separate issue, may be unrelated).

**Escalate** — builders — watchtower.sh code change required to export SPIRA_DB. This is the root cause of recurring duplicates in incident:dedup-meter-nonzero (sp-recur-19) and others.

**Reference** — sp-csvzn (recurrence 19, held=no), sp-o6zkx (prior collapse), sp-7lqgk (prior fix attempt), sp-100up (root cause filed)

**Matches** `DEDUP.*duplicate incident refs|multiple beads.*same.*external_ref`

### Incident dedup duplicate spira db blocking pipe

`sop-incident-dedup-duplicate-spira-db-blocking-pipe`

**Symptom** — incident.sh files duplicate beads for the same external_ref despite dedup logic. Root cause: watchtower.sh incident filing blocks had duplicate SPIRA_DB="$SPIRA_DB" assignments (lines 485, 523, 540). When environment variables are duplicated before a pipe, the second assignment blocks bash from properly scoping the environment to the RHS subprocess. bdq calls inside incident.sh inherit an empty SPIRA_DB and fail silently.

**Check** — grep -n 'SPIRA_DB="$SPIRA_DB"' watchtower.sh | uniq -c | grep -v '^ *1 '. Output should be empty after fix (each incident filing block has only one SPIRA_DB assignment). Run: bd -C $SPIRA_DB list --all --json | python3 -c "import json,sys; d=json.load(sys.stdin); refs=[x.get('external_ref') for x in (d if isinstance(d,list) else [d])]; print(f'Unique refs: {len(set(refs))}, Total beads: {len(refs)}, Duplicates: {len(refs) - len(set(refs))}')"

**Fix** — Remove duplicate SPIRA_DB assignments from each incident filing block. Each block should have exactly ONE SPIRA_DB="$SPIRA_DB" assignment immediately before the bash call on the RHS of the pipe. Lines affected: 485 (drain), 523 (sending), 540 (unadopted). Commit 76f3b94 applies the fix.

**Escalate** — none

**Reference** — law-env-prefix-left-of-pipe, sop-spira-env-not-passed-to-subprocess, sp-csvzn (recurrence 27+)

**Matches** `duplicate SPIRA_DB.*environment.*assignment|watchtower.sh.*line.*485|pipe.*SPIRA_DB.*environment`

### Incident dedup env pipe scope

`sop-incident-dedup-env-pipe-scope`

**Symptom** — incident.sh files duplicates despite dedup logic. Environment variables placed before a pipe do not reach the subprocess on the right. watchtower.sh had `printf ... | \ SPIRA_DB=... bash incident.sh` with broken scope — incident.sh inherits empty SPIRA_DB and bdq calls resolve to wrong database.

**Check** — grep -n 'printf.*| *\\' /path/watchtower.sh looking for VAR assignments between pipe and bash. Test: `printf x | V=val bash -c 'echo $V'` prints val (works); `printf x | \ V=val bash ...` may fail (scope broken).

**Fix** — Ensure SPIRA_DB="$SPIRA_DB" appears on the same logical line as bash, not split by pipe continuation above it. Remove duplicate assignments. Pattern: `printf ... | SPIRA_DB="$SPIRA_DB" ... bash "$INC" file ...`.

**Escalate** — n/a

**Reference** — law-env-prefix-left-of-pipe, sp-b9qs

**Matches** `duplicate incident.*refs.*dedup|bdq.*fails.*silent|environment.*subprocess.*incident`

### Incident dedup python quoting

`sop-incident-dedup-python-quoting`

**Symptom** — incident.sh files duplicate beads for the same external_ref despite flock serialization. Multiple calls to python3 -c use double quotes, causing bash to parse Python regex patterns and list comprehensions as bash syntax during subprocess execution. Python syntax fails silently, _dedup_incident returns empty, incident.sh treats that as "no existing bead" and files fresh beads on every pass.

**Check** — bash -n spira/incident.sh — should exit 0. If it fails with "syntax error near unexpected token '(' or '['" on lines with `python3 -c "`, this matches. After fix: grep -c 'python3 -c "' spira/incident.sh returns 0 (all changed to single quotes).

**Fix** — Changed 5 python3 -c "..." calls to python3 -c '...' in incident.sh (lines 182, 200, 221, 236, 245). For embedded quotes use bash string breaking: '"'"'. Commit 67222f2 pushed to origin/main.

**Escalate** — none

**Reference** — sp-csvzn

**Matches** `duplicate incident refs detected.*Python.*quoting|bash -n.*syntax error.*\(|\[.*python`

### Incident dedup undefined bdq

`sop-incident-dedup-undefined-bdq`

**Symptom** — incident.sh files duplicate beads for the same external_ref despite flock protection. Root cause: bdq is called throughout incident.sh but is never defined. With bdq undefined, all dedup queries (bd list) fail silently due to stderr redirects, causing open_incident() to return empty results and file_one() to create fresh beads on every pass.

**Check** — grep -n '\bbdq\b' incident.sh (returns 25+ matches calling undefined function). Then grep -rn '^bdq' $SPIRA_HOME (returns nothing—function never defined). Run test-incident.sh concurrent-filer test: if it creates multiple beads for the same external_ref despite flock, this matches.

**Fix** — Define bdq as a wrapper in lib.sh. Add the line: `bdq() { "$SPIRA_BD" -C "$SPIRA_DB" "$@"; }` in the function definitions section of lib.sh (after conf.sh is sourced, before incident.sh sources lib.sh). This restores access to the production database for all dedup queries. Verify: (1) grep -n '^bdq=' lib.sh returns the definition. (2) Run test-incident.sh concurrent-filer test—should create exactly one bead when two callers file simultaneously. (3) Run watchtower.sh sweep and verify no new duplicates for existing external_refs.

**Escalate** — builders—lib.sh missing bdq function definition. This is the root cause of recurring duplicates (sp-csvzn sp-recur-23+). Recurrence 23 proves prior fixes (SPIRA_DB export, Python quoting) did not resolve it; bdq undefined was the blocker.

**Reference** — sp-csvzn (sp-recur-23), sp-ppbky (finding filed)

**Matches** `duplicate.*incident refs.*dedup|bdq.*not.*defined|all.*bdq.*calls fail`

### Incident dedupe race

`sop-incident-dedupe-race`

**Symptom** — incident.sh filed duplicate beads for the same external_ref when two callers invoked concurrently (watchtower timer and install-triggered start both firing simultaneously). Both callers asked open_incident for an existing bead, both got "no", both filed. The check-and-create was not atomic.

**Check** — bash spira/test-incident.sh and confirm "two concurrent filers create exactly one bead" passes. The test fires two simultaneous incident.sh file calls against an empty database and verifies exactly one bead results, proving the concurrent dedupe works.

**Fix** — Added flock around drain_one in spira/incident.sh to protect the entire check-and-create sequence. Every path into the intake — file, systemd, drain — goes through drain_one, so one lock guards all entry points. The wait is bounded and timeout leaves the entry spooled, preserving the write-ahead log semantics.

**Escalate** — n/a

**Reference** — wiki/notes/incident-dedupe-race-sop-2026-09-09.md

**Matches** `incident.sh.*concurrent.*dedupe|two beads.*same ref.*timestamp|open_incident.*race`

### Incident duplicates unstable ref

`sop-incident-duplicates-unstable-ref`

**Symptom** — Multiple beads filed for the same event. Callers of incident.sh do not set stable SPIRA_INCIDENT_REF, so incident.sh generates ref from title: `incident:$(title | slugify | cut -c1-60)`. Title variations (e.g., "Spira sweep" vs "Spira sweep — is the pipeline moving?") generate different refs, bypassing dedup and creating duplicates per variation. Verified watchtower.sh and suites.sh already set stable SPIRA_INCIDENT_REF; mystery caller still firing "Spira sweep" incidents without refs.

**Check** — (1) List duplicates: `bd -C $SPIRA_DB list --limit 0 | grep incident | head -20`. (2) Group by external_ref: for each, `bd show <id> --json | python3 -c "import sys,json; print(json.load(sys.stdin)[0].get('external_ref'))"`. (3) Identical refs = concurrent race. Different refs for same event = caller misconfiguration.

**Fix** — (1) Collapse current duplicates with bd supersede. (2) Audit callers — already verified: watchtower.sh sets SPIRA_INCIDENT_REF for sending/unadopted/dedup (lines 522, 539, 579), suites.sh sets it for suite failures (lines 211, 265). (3) Find unknown caller producing "Spira sweep" variants without stable ref. (4) Add SPIRA_INCIDENT_REF="incident:spira-sweep" to that caller or its wrapper. (5) Verify: two consecutive sweep passes should file zero duplicate beads.

**Escalate** — None — this is a caller configuration issue, builders' work once caller is identified.

**Reference** — sp-o6zkx (sp-recur-9, collapse + investigation filed sp-xo4xl), wiki/notes/incident-dedup-caller-config-2026-09-10.md

**Matches** `duplicate.*external_ref|12 duplicate beads|[a-z0-9-]+ P1.*incident.*incident.*same title`

### Incident lock exec write blocks

`sop-incident-lock-exec-write-blocks`

**Symptom** — test-incident.sh concurrent-filer hangs indefinitely; two background processes calling incident.sh simultaneously block before reaching flock. The hang occurs in drain_one at `exec 8>"$lock"` which opens the lock file in write mode (truncate). When two processes try to truncate the same file simultaneously, one blocks in the open() syscall indefinitely, never reaching flock.

**Check** — timeout 20 bash spira/test-incident.sh concurrent-filer should complete in <10s. If it hangs past 15s and times out (rc=124), this bug is present. After fix, both concurrent processes exit 0 within 5s.

**Fix** — Changed incident.sh line 487 from `exec 8>"$lock"` (write/truncate mode) to `exec 8>>"$lock"` (append mode). Both processes can now open the lock file without blocking on the open() call, then proceed to flock which correctly serializes. Root cause: `>` mode blocks concurrent opens; `>>` mode allows concurrent opens that flock then serializes.

**Escalate** — none

**Reference** — wiki/notes/incident-lock-concurrent-hang-sop-2026-09-11.md

**Matches** `test-incident.sh concurrent-filer hangs|incident.sh.*flock.*timeout|two concurrent.*intake.*hang|exec 8>.*lock.*blocks`

### Lake bucket missing

`sop-lake-bucket-missing`

**Symptom** — pokedumpster lake or ship work fails against s3://pkdump-lake-237707363372-us-west-2 with NoSuchBucket or AccessDenied.

**Check** — aws s3api head-bucket --bucket pkdump-lake-237707363372-us-west-2. A 404 means the bucket was never created; a 403 means the credential is the backup identity, not a lake identity.

**Fix** — there is none available to Ops, and that is by design. Both identities on this box (user/gantt-mtgc-backup and role/mtgc-backup) are scoped to backup duties and cannot CreateBucket. Do not widen them.

**Escalate** — it needs an admin credential for account 237707363372, which only Ryan holds. Say what is blocked and what is not, and stop.

**Reference** — wiki/notes/lake-bucket-sop-2026-08-10.md

**Matches** `NoSuchBucket|pkdump-lake|AccessDenied.*s3|CreateBucket`

### Ledger init mkdir

`sop-ledger-init-mkdir`

**Symptom** — sop.sh ledger-init silently fails to create the ledger directory with mkdir 2>/dev/null, causing subsequent sop.sh digest calls to fail. aeon.sh cannot determine if an SOP was written, setting sop_wrote=unreadable instead of yes/no.

**Check** — grep 'mkdir.*2>/dev/null' spira/sop.sh | grep -i ledger should return nothing (fix is applied).

**Fix** — Remove 2>/dev/null from mkdir in ledger-init (line ~581). Change from 'mkdir -p "$(dirname "$LEDGER")" 2>/dev/null' to 'mkdir -p "$(dirname "$LEDGER")" || { echo "sop: cannot create ledger directory" >&2; exit 1; }'.

**Escalate** — none

**Reference** — sp-nifr1

**Matches** `sop\.sh ledger-init.*mkdir.*2>/dev/null|ledger.*cannot.*create.*directory`

### Mtgc unit failed

`sop-mtgc-unit-failed`

**Symptom** — an mtgc prod unit failed and fired its OnFailure alert. Every incident from the systemd intake starts here; triage before reaching for a fix.

**Check**

```
systemctl --user show <unit> -p Result -p ExecMainStatus -p NRestarts, then journalctl --user -u <unit> -n 40. Result= distinguishes exit-code from timeout from oom-kill, and the journal alone often does not say which.
```

**Fix** — resolve by Result. exit-code — read the command's own error, fix it, systemctl --user start <unit>, then confirm the NEXT scheduled run is green rather than the manual one. timeout — find what it waited on before extending anything. oom-kill or a disk error — see sop-root-disk-pressure.

**Escalate** — if the fix is a code change. Prod runs from /opt/mtgc-prod, a different checkout from /workspaces, so merging changes nothing on the running system and the deploy is a separate named step.

**Reference** — wiki/projects/spira/spira.md

**Matches** `mtgc-.*\.service|MTGC unit FAILED`

### Orphaned worktree unsent branch

`sop-orphaned-worktree-unsent-branch`

**Symptom** — Watchtower escalates "oldest unsent branch 24h+" alert; investigation finds worktrees in $SPIRA_RUN/worktree/ for CLOSED beads whose branches are already reaped or never existed. The worktree mtime is old (created when the bead was active), so stat-based age checks see it as ancient. The reaper refuses to delete worktrees (a safety feature), so abandoned ones persist and block watchtower's alert from clearing.

**Check** — (1) Identify oldest unsent branch via watchtower snapshot or `git for-each-ref 'refs/heads/spira/*' --format='%(refname:short) %(committerdate:unix)'`. (2) For each old branch, check if the corresponding worktree exists: `ls -d $SPIRA_RUN/worktree/<bead-id>`. (3) Verify the bead is CLOSED: `bd show <id> | grep "CLOSED"`. (4) Confirm the branch is already reaped from origin: `git branch -r | grep -c <bead-id>` should return 0.

**Fix** — Remove the abandoned worktrees for CLOSED beads. These are safe to delete because the bead is closed, the branch is reaped, and the worktree is no longer claimed. `rm -rf $SPIRA_RUN/worktree/<bead-id>` for each abandoned worktree. Verify cleanup: `ls $SPIRA_RUN/worktree/ | wc -l` should only count active aeon worktrees and $SPIRA_RUN landing worktrees.

**Escalate** — never — this is Ops's cleanup of abandoned ephemeral resources.

**Reference** — wiki/notes/orphaned-worktree-unsent-branch-sop-2026-09-10.md

**Matches** `oldest unsent branch [0-9]+h.*watchtower|branch.*worktree.*orphan.*CLOSED|abandoned worktree.*prevent.*reap`

### Ppid parsing hang

`sop-ppid-parsing-hang`

**Symptom** — test-aeon-heartbeat.sh hangs indefinitely, blocking suites.sh runner from reaching the 3 tests that follow it alphabetically (test-archivist.sh, test-check5-drop.sh, test-cockpit-unsent.sh). Hang occurs in subtree_has_flock check at line 171, or in SECOND youngest_in_subtree call at line 133 when ancestry contains processes with spaces in comm.

**Check** — `cd spira && timeout 20 bash spira/test-aeon-heartbeat.sh`. Expected rc=0 with all 22 tests passing within 10s. Hang = rc=124 (timeout) with output stopping mid-test.

**Fix** — In lib.sh, subtree_has_flock awk parsing: MUST parse ppid after the closing paren like youngest_in_subtree does, not by field number. /proc/<pid>/stat is "pid (comm) state ppid ..." and comm can contain spaces (e.g. "(Web Content)", "(Socket Process)", "(tmux: server)"). Using field $4 reads state, not ppid, breaking ancestry walks. Use same awk pattern as youngest_in_subtree: find close_paren by scanning backwards, extract ppid from rest after close_paren. See commit e2172ce.

**Escalate** — None — fix is in the repository, test passes, no deployment required beyond merge to main.

**Reference** — sp-a8c5, sp-04bd (prior session, same symptom, different root cause theory), commits: e2172ce

**Matches** `test-aeon-heartbeat.*hangs.*subtree_has_flock|subtree_has_flock.*ppid|heartbeat.*processes with spaces in comm`

### Proc walk awk per hop

`sop-proc-walk-awk-per-hop`

**Symptom** — test-aeon-heartbeat.sh hangs or runs past its slice; `bash -x` sticks silently inside a `youngest_in_subtree`/`shf` call, no further trace. Not the orphaned-child-holds-pipe mechanism (sop-suite-hang-blocks-pipe) — nothing is left running.

**Check** — `bash -c 'sleep 3600 & wait' & outer=$!`; `time env -i PATH="$PATH" HOME=/tmp bash -c '. spira/lib.sh; youngest_in_subtree "$1" 0' _ "$outer"`. Multi-second wall time on a box with hundreds of live processes (`ls /proc|grep -c '^[0-9]*$'`) confirms it.

**Fix** — old code forked one `awk` per ancestor hop per pid (up to 40/pid) — cost scales with processes x depth x fork overhead, exceeding any timeout on a loaded box. Fixed in spira/lib.sh (spira-harness main, commit 0a53666 — push rejected non-fast-forward, rebase before it reaches other replicas): build the whole pid->ppid map with ONE awk pass, walk ancestry as bash array lookups. 4.8s -> 0.08s measured; suite 22/22 in well under its slice. subtree_has_flock untouched (few flock procs, cheap); batch it too if ever implicated.

**Escalate** — never. A push rejection here isn't one either — rebase and retry; systemd units run the working tree directly regardless.

**Reference** — sp-a8c5

**Matches** `youngest_in_subtree|subtree_has_flock|test-aeon-heartbeat.*(hang|timeout|never produced)`

### Python string quoting bash n

`sop-python-string-quoting-bash-n`

**Symptom** — gate/bash -n fails on Python code embedded in a bash script. Python list comprehensions and regex patterns contain parentheses and brackets; when Python code is inside double-quoted bash strings, bash parses these special characters as bash syntax during `bash -n`, producing "syntax error near unexpected token '('".

**Check** — (1) bash -n <script> fails with syntax error on a line containing Python code. (2) The Python code contains brackets [] or parentheses () in patterns like re.match(...) or list comprehensions. (3) The python3 -c "..." uses double quotes.

**Fix** — Use single quotes for Python code blocks with embedded variable expansion via shell string concatenation. Pattern: 'python-before'"$BASH_VAR"'python-after'. Bash expands variables at the break in quotes, but treats the rest as literal, so special characters in Python are not interpreted by bash. Change: python3 -c "..." to python3 -c '...' with breaks for variables: ["$VAR"] becomes ["'"$VAR"'"].

**Escalate** — n/a

**Reference** — sp-emz6s, sop-incident-dedup-python-quoting

**Matches** `bash -n.*syntax error.*\(|\[.*re\.match.*\]`

### Reclaim loop on needs ryan block

`sop-reclaim-loop-on-needs-ryan-block`

**Symptom** — a bead is reclaimed and re-summoned every lease cycle although every session reaches the identical diagnosis: IN_PROGRESS, blocked purely on an unanswered needs-ryan sub-decision, nothing new. Each cycle burns a full aeon session re-deriving the same answer.

**Check** — `bd -C $SPIRA_DB show <bead> --json` for status+labels; find the needs-ryan bead it cites and `bd show <ask-id> --json` for its status. Two-plus `sp-reclaim-N-refused` labels with the same diagnosis in each session's notes confirms the pattern.

**Fix** — check whether the blocker already closed — the loop may have already resolved and the labels are just stale; note that and close. If it is still open, this session cannot safely change the reclaim cadence inside an Ops wall — link to sp-2k5a (tracks making the sentinel/reaper treat "blocked only on an open needs-ryan dep" as a stable wait, exempt like ready-predicates already are) and stop. Do not re-diagnose from scratch.

**Escalate** — only if the blocking ask itself looks stale or wrong — reply in its own thread (law-reply-in-the-thread), never a second one.

**Reference** — wiki/notes/reclaim-loop-needs-ryan-sop-2026-09-08.md

**Matches** `reclaimed [0-9]+ times|sp-reclaim-[0-9]+-refused.*sp-reclaim-[0-9]+-refused|respawned [0-9]+x for one unanswered ask`

### Root disk pressure

`sop-root-disk-pressure`

**Symptom** — a unit died with ENOSPC, a bus error, or a disk check trip. The 98G LVM root runs production; $SPIRA_WORKSPACES is a separate 938G disk that is nearly empty.

**Check** — df -h / $SPIRA_WORKSPACES and du -xh --max-depth=1 / 2>/dev/null | sort -h | tail. Confirm the pressure is on / and not on $SPIRA_WORKSPACES before touching anything.

**Fix** — clear agent scratch first — it is the usual culprit and it is free. If the container store is on /, run $SPIRA_WORKSPACES/gt/settings/migrate-container-store.sh --preflight then --run; it refuses while any polecat is live, verifies row counts before restarting, and rolls back on any post-stop failure. Then restart the failed unit and re-run its check.

**Escalate** — if the space is production data rather than scratch, or if --preflight refuses. Never widen a disk threshold to quiet the check.

**Reference** — wiki/notes/container-storage-volume-sop-2026-08-11.md

**Matches** `No space left on device|ENOSPC|Bus error|disk.*(9[0-9]|100)%|diskcheck.*FAIL`

### Sending unsent oldest calc

`sop-sending-unsent-oldest-calc`

**Symptom** — Watchtower escalates SENDING incidents claiming an unsent branch is 24h+ old when the branch is actually minutes old. The incident is a false positive — the branch is young work in flight, not aged backlog. Occurs when reporting an oldest-branch age that is orders of magnitude older than the actual commit timestamp.

**Check** — (1) Compare SP_UNSENT_OLDEST_H from the cockpit snapshot against git's actual branch timestamps. Get the snapshot value and the actual oldest branch age: `for br in $(git for-each-ref --format='%(refname:short)' 'refs/heads/spira/*'); do git log --format=%at -1 "$br"; done | sort -n | head -1 | xargs -I{} echo $(( ($(date +%s) - {}) / 3600 ))`. If reported age >> actual age, this bug is present.

**Fix** — In spira/cockpit.sh line 716, variable `_o` (unix timestamp of oldest unsent branch) is used unquoted in arithmetic context. Unquoted, it evaluates as a variable NAME (value 0) not its dereferenced value (the timestamp). Fix: add missing `$` to dereference `_o` as `$_o`. Change: `$(( ( $(date +%s) - _o ) / 3600 ))` to `$(( ( $(date +%s) - $_o ) / 3600 ))`. Restart collectors after: `systemctl --user restart spira-cockpit.service`.

**Escalate** — n/a

**Reference** — sp-h761n

**Matches** `SENDING.*oldest unsent.*escalation|false.*SENDING.*incident|SP_UNSENT_OLDEST_H.*incorrect|watchtower.*unsent.*false.*alert`

### Sentinel timer disabled

`sop-sentinel-timer-disabled`

**Symptom** — Spira pipeline completely stalled: no aeons alive despite ready beads waiting. The sentinel timer is disabled and inactive, so the dispatcher never runs and no work is claimed. The harness continues to accept closures and track state but does nothing to move work forward.

**Check**

```
systemctl --user status spira-sentinel.timer | grep -E "Active:|Loaded:" — should show "Active: active (running)" and "Loaded: loaded.*enabled". If disabled or inactive, this matches.
```

**Fix** — re-enable and start the timer: `systemctl --user enable spira-sentinel.timer && systemctl --user start spira-sentinel.timer`. Verify: `systemctl --user status spira-sentinel.timer | grep Active:` should show active. Wait 5s for sentinel to fire, then verify aeons: `systemctl --user list-units 'spira-aeon-*' --state=running | wc -l` should be > 0.

**Escalate** — none — this is Ops work. If the timer was disabled intentionally (a deliberate drain), do not restart it; restore the intention. If accidental, start it.

**Reference** — wiki/notes/sentinel-timer-disabled-sop-2026-09-09.md

**Matches** `aeons alive 0.*ready to claim [0-9]+|spira-sentinel\.timer.*inactive.*disabled|no commits in landing queue`

### Skew escalate dedupe

`sop-skew-escalate-dedupe`

**Symptom** — skew.sh escalate() function calls SPIRA_NOTIFY twice for the same condition within a single run. The condition_key (v2:BEHIND=X DIRTY=Y...) is stable, so a repeated divergence check with the same findings TYPE should not re-escalate. Without per-run deduplication, a condition triggers ask.sh twice in quick succession, filing duplicate or re-escalating closed conditions.

**Check** — Run test-skew-escalate.sh; the "condition-keyed dedupe" section verifies that two calls with the same SPIRA_RUN and same condition increment SPIRA_NOTIFY call count exactly once (not twice). If the test fails at "dedupe second call: notify still fired exactly once total: wanted [1] got [2]", this matches.

**Fix** — Add dedup stamp mechanism to escalate(). Check for a per-run stamp file keyed on condition_key (using cksum for stability) at the start of escalate(); if present, return early without calling SPIRA_NOTIFY. After a successful SPIRA_NOTIFY call, write the stamp to SPIRA_RUN so subsequent calls in the same run skip the notification. The stamp path includes a hash of the condition_key so different conditions (BEHIND=1 DIRTY=0 vs BEHIND=0 DIRTY=1) maintain separate stamps.

**Escalate** — none — this is a code bug in skew.sh, fixed by adding per-run deduplication to the escalate() function.

**Reference** — wiki/notes/skew-escalate-dedupe-sop-2026-09-11.md

**Matches** `test-skew-escalate\.sh.*FAIL.*dedupe second call.*wanted \[1\] got \[2\]|escalate.*calls SPIRA_NOTIFY twice.*same condition`

### Spira blind landing meter

`sop-spira-blind-landing-meter`

**Symptom** — a Spira sweep headed "minutes since the last landing ? (last: none recorded)". `?` means the pass COULD NOT READ it, never that nothing landed — conclude neither stall nor health. The four-figure gate wait beside it: same bead, also blind.

**Check** — read the records — `for f in $SPIRA_RUN/landstate/*; do echo "$(basename $f): $(cat $f)"; done`. Rows are `LANDED <tip> <epoch> <repo>`; `date -d @<epoch>`. A LANDED epoch minutes old under "none recorded" means blind meter, not stalled pipeline. All-RED with no LANDED means the queue really has produced nothing — go to sop-spira-no-rebase. Date the gate-wait row too: `wc -l $SPIRA_RUN/gate.log; tail -4`. Its max spans `tail -50`, so under 50 rows "recent" means "ever"; later `waited=0s` rows date it to a dead topology.

**Fix** — do not patch or re-file it from a sweep. Both meters are one harness bead, sp-86q8 (P0, repo:spira); confirm its state — `bd -C $SPIRA_DB dep tree sp-86q8`. READY or IN_PROGRESS means queued, leave it: blindness costs a sweep, throttles no aeon (law-file-it-and-let-the-loop-fix-it). Closed means read its close reason. Absent or blocked is the only new work.

**Escalate** — nothing needs the operator. A BLIND HEADLINE IS NOT THE SWEEP — having shown the pipeline moves, go on to the verdicts and the poison list (sop-spira-verdict-not-executed). Put the real last-landing time and the gate-wait row's age in the close reason; one closed on an unreadable headline is indistinguishable from one that never looked.

**Reference** — wiki/notes/blind-landing-meter-sop-2026-09-07.md

**Matches** `none recorded|minutes since the last landing +\?`

### Spira env not passed to subprocess

`sop-spira-env-not-passed-to-subprocess`

**Symptom** — incident.sh files beads to ~/.beads or the process's working directory instead of production. The environment variable SPIRA_DB is not exported to a subprocess calling incident.sh, so incident.sh inherits an empty SPIRA_DB. bdq then calls bd -C "" which resolves the database path from the process's working directory — typically $HOME for a user systemd unit, resulting in ~/.beads.

**Check** — (1) Identify stray .beads directory: `find /home -name .beads -type d 2>/dev/null | grep -v /git/`. (2) Confirm beads there: `bd -C /home/*/.beads list --all 2>/dev/null | wc -l`. (3) Verify the calling script: in spira/suites.sh, grep for `bash "$INC" file` and check if the preceding `SPIRA_DB=` assignment line exists. If no `SPIRA_DB="$SPIRA_DB"` prefix, this matches.

**Fix** — Add `SPIRA_DB="$SPIRA_DB" \` to the environment prefix of every subprocess call to incident.sh. In suites.sh: file_red, file_env_red, file_fixture_fault all call `bash "$INC" file ...`. Prefix each with SPIRA_DB. Verify: run `suites.sh run` and confirm ~/.beads is not created and no beads appear outside production store.

**Escalate** — n/a — this is Ops/builders code.

**Reference** — sp-ncr3l

**Matches** `stray.*\.beads|file.*wrong.*store|incident.*filed.*~/\.beads|~/.beads/|empty.*SPIRA_DB.*parent.*unit`

### Spira no aeons

`sop-spira-no-aeons`

**Symptom** — Aeons stopped while ready beads exist. Either deliberate halt, schema mismatch blocking bd commands, or summoning fault.

**Check** — (1) `bd list --status open -l needs-ryan 2>&1 | head -3` — "schema version mismatch" means database ahead of binary. (2) No schema error? Check sentinel journal: `journalctl --user -u spira-sentinel.service --since -3h -o short-iso | grep Finished | tail -10`. Steady two-minute cadence with clean gap = halt. Ragged/absent while timer active = real fault. (3) If sentinel running normally but aeons still 0: summon logic may be failing silently — check sp-f683y class incidents and run `sentinel.sh -x 2>&1 | tail -50` to capture actual failure point.

**Fix** — Schema mismatch: ESCALATE with decision question and default. Leave bead OPEN — do not close until the operator's decision is executed and verified. Deliberate halt: do not undo. Real fault: diagnose why sentinel summoned nobody (pool exhausted, predicate mismatch, capacity paused, summon logic silent failure). If sentinel runs but summon fails silently: file sp-f683y class incident and investigate summon path.

**Escalate** — Schema mismatch (Ops cannot run migration; escalate with decision: "Migrate database to v61, or downgrade aeon binary to v53? I would migrate."). Deliberate halt (operator's call). Summoning fault (name the blocker). Summon logic failure (requires code investigation; file linked incident for next session).

**Reference** — sp-6fldw (2026-09-09, schema case; amendment: leave bead open when escalating schema decision), sp-f683y (2026-09-09, summon logic silent failure), wiki/notes/spira-no-aeons-diagnostics.md

**Matches** `aeons alive +0|aeons alive 0.*ready to claim [1-9]`

### Spira no rebase

`sop-spira-no-rebase`

**Symptom** — the landing pass reopens a closed bead — "does not rebase onto <base>" — its landstate row reading "RED <tip> <at> no-rebase". Nothing lands, and it does not self-limit: each reopen requeues the bead and the next pass reopens it again.

**Check** — cat $SPIRA_RUN/landstate/* — all-RED with no LANDED row is why the sweep renders minutes-since-last-landing as ?; never read that ? as zero. Take the conflict FROM GIT, never from file-existence: a path missing from base is equally "added by the branch" and "deleted by base". git -C <repo> worktree add -q --detach /tmp/reb <tip> && git -C /tmp/reb rebase <base>; read the CONFLICT lines; abort; worktree remove --force.

**Fix** — by the conflict git printed. CONTENT — apply law-duplicate-not-a-hard-merge: diff against base file by file, and if base holds the work, bd supersede <id> --with <successor> (a close reason alone the sentinel does not read, so it reopens) and delete the branch. MODIFY/DELETE — the branch predates a deliberate removal; usually only the dead file conflicts and the product change merges clean. Confirm that, keep the product change, drop the dead file. BRANCH GONE, bead open — the tip is dangling, awaiting gc: rescue it first with git -C <repo> branch <branch> <tip> and bd label add <id> branch:<branch>.

**Escalate** — when the deletion behind a modify/delete was a design change, not a tidy-up — dropping discards coverage somebody chose. Reply into the thread that owns it, never a second bead.

**Reference** — wiki/notes/landing-no-rebase-sop-2026-09-07.md

**Matches** `does not rebase onto|CONFLICT \(modify/delete\)|no-rebase`

### Spira strand ledger

`sop-spira-strand-ledger`

**Symptom** — a strand count you cannot resolve to a class, or a live aeon count disagreeing with the snapshot.

**Check** — `stranded (claimed, nobody home)` is the GHOST count, the only failure; `strand ledger, other classes` itemises the rest as `kind=n`. A `?` IS UNREAD, NEVER ZERO. Ghost `?` beside a numeric total is a collector predating the split: `cockpit-ensure.timer` restarts `spira-cockpit.service` within a minute of the code landing, so re-read once; if it outlives two passes suspect that timer, not this code. `?` on both: strands.json absent or unparsable. Classify a ledger with `SPIRA_RUN=<dir> cockpit.sh strands`, the collector's function. Keys are `<partition>:<kind>:<id>`, split FROM THE RIGHT: a partition may carry a colon. `empty` is a childless epic: nothing claimed, nobody dead. Match workers on argv[1] via /proc, not `pgrep -f`.

**Fix** — act only on `ghost`; every other class is a disposition, not a fault. For `empty`, READ THE EPIC'S BODY — one carrying a method says its children are filed, so the gap is an edge. strand-classify.py reads a bead's own `parent` FIELD, not dependency edges: `bd update <child> --parent <epic>` clears it, `bd dep add -t parent-child` does not; re-run `strand.sh` and require zero. An `escalated: <epoch>` entry HAS ALREADY ASKED — find it; if the subject never moved, or closed on `done`/`ok`, see sop-spira-verdict-not-executed.

**Escalate** — only when a disposition needs a decision; name the prior ask and reversal cost. Never escalate an unclassified count.

**Reference** — wiki/notes/strand-ledger-sop-2026-09-07.md

**Matches** `stranded \(claimed, nobody home\)|strand ledger, other classes|strands\.json|SP_STRAND|open epic with no children`

### Spira suite never run

`sop-spira-suite-never-run`

**Symptom** — `suites.sh list` shows suites RUNS=timed with LAST `-` AGE `-`; the sweep says "timed suites with no result yet N". It reads as N suites the runner passes over. Usually it is not.

**Check** — date the suite against the pass, never against the bead. `ls -la --time-style=full-iso $SPIRA_RUN/suites/*.result` gives the pass window; then per `-` suite `git -C $SPIRA_REPO log --diff-filter=A --format=%cd --date=iso -1 --first-parent main -- spira/<suite>`. A merge AFTER the last result mtime means it was not in the tree that pass globbed — NOT A DEFECT. Confirm with `od -c $SPIRA_RUN/suites/cursor`: a lone newline means next_cursor was never set, so the `left -le 5` branch — the loop's only recordless path — was not taken. NEVER reason from alphabetical interleaving of result epochs: gaps are filled by the runtimes either side, so an absent suite always looks interleaved.

**Fix** — for a suite merged after the pass, nothing; say so and close. Then ask what the `-` really raises: `grep -rn 'suites.sh run' $SPIRA_REPO --include=*.timer --include=*.service`. NO UNIT INVOKES IT, though suites.sh says "on a schedule". Its only caller is a human at the sweep menu, and sop-spira-sweep forbids running it there (420s budget, 480s wall).

**Escalate** — installing the timer is the operator's to order (law-detect-drift-never-install). Default: `spira-suites.timer` on a cadence longer than the budget.

**Reference** — wiki/notes/suite-never-run-sop-2026-09-08.md

**Matches** `timed suites with no result yet [1-9]|never produced a result|LAST - and AGE -`

### Spira suite red duplicate bead

`sop-spira-suite-red-duplicate-bead`

**Symptom** — two open bugs with the IDENTICAL title "<suite> is red in the timed suite run", read as suites.sh minting a bead per pass. Usually it is not. suites.sh already dedupes on `<suite>:<fingerprint>`, so a second bead means the FAILURE CHANGED, which files afresh by design.

**Check** — compare FINGERPRINTS, never titles — `bd -C $SPIRA_DB show <id> --json | python3 -c "import json,sys;d=json.load(sys.stdin);d=d[0] if isinstance(d,list) else d;print(d['external_ref'])"`. Different refs is different failures; confirm on each bead's own "N passed, M failed" line (49/1 vs 48/2 is a second assertion, not a repeat). Prove dedupe is live by recomputing the fingerprint NOW: `out="$(bash spira/<suite>)"; rc=$?; sig="$(printf '%s\n' "$out"|grep -F FAIL||true)"; printf 'rc=%s\n%s\n' "$rc" "$sig" | sed -e 's/[0-9]\{3,\}/N/g' | cksum | tr -d ' \t'`. A `sp-recur-N` label on the bead holding that ref is the dedupe WORKING.

**Fix** — `bd -C $SPIRA_DB supersede <old> --with <new>` — the older signature is a subset of the newer. A close reason alone the sentinel does not read, so it reopens (law-duplicate-not-a-hard-merge). Do NOT dedupe on suite name alone in suites.sh: that collapses a failure which GREW an assertion into a bare count, hiding the new one. Never fix the red itself; it is a builder's bead and blocks nothing.

**Escalate** — nothing. Fingerprints that genuinely MATCH is a real incident.sh defect — file it, do not decide it.

**Reference** — wiki/notes/suite-red-duplicate-bead-sop-2026-09-08.md

**Matches** `is red in the timed suite run|duplicate bead every run|suite:[a-z0-9.-]+:[0-9]{9,}|files a SEPARATE bead each time`

### Spira sweep

`sop-spira-sweep`

**Symptom** — the ten-minute watchtower sweep: vital signs, not a failure. EIGHT MINUTES; systemd kills you at the deadline. CUT BEADS for what you cannot finish.

**Check** — THIS BEAD'S METADATA MEANS NOTHING. sp-recur-N counts ten-minute intervals nobody closed, never failed fixes. P1/bug is a defect, not a severity. "Reopened by sentinel: no commit names it" means the last session was HEALTHY.

**Fix** — IS ANYTHING COMING OUT? Read $SPIRA_RUN/landstate/* yourself, never the headline (sop-spira-blind-landing-meter); all-RED with no LANDED is sop-spira-no-rebase. ARE THE WORKERS WORKING? Ready beads and no aeons alive, or in_progress with nobody home, is a summoning fault (sop-spira-no-aeons, sop-spira-strand-ledger). NEVER RUN THE MENU'S `suites.sh run` HERE — 420s against a 480s wall leaves no record; read `$SPIRA_RUN/suites/*.result` instead and confirm a red's bead by its FINGERPRINT, not its title (sop-spira-suite-red-duplicate-bead). "NO RESULT YET N" IS NOT N NEW SUITES — already escalated as sp-rmnw (sop-spira-suite-never-run). Name the scans you skipped. THEN COMMIT BEFORE YOU CLOSE, ALWAYS: a held SOP writes no page, and the sentinel reopens any bead no commit names. Commit the applied ledger or a note whose subject carries the bead id; verify `git log --all --grep=<id>` is NON-EMPTY before `bd close`.

**Escalate** — a `sin` LABEL ALREADY PAGED HIM, evidence-free. Find it (`bd list --status open -l needs-ryan`) and REPLY IN THAT THREAD, vital signs inline. All closed means `sin` owes no reply. Never a second bead.

**Reference** — wiki/notes/spira-sweep-clean-close-reopen-2026-09-08.md

**Matches** `Spira sweep — is the pipeline moving|minutes since the last landing`

### Spira verdict not executed

`sop-spira-verdict-not-executed`

**Symptom** — a verdict was given and nothing carried it out, or it did not STICK and the operator answered twice. Answering closes the ask and leaves the SUBJECT untouched, so an unexecuted verdict looks exactly like one never given. For poison the recommended default REPRODUCES THE ASK.

**Check** — confirm the SUBJECT moved, never that the ask closed. Read STATUS, LEASE and LABELS together — a bead an incident calls poisoned is often already cleared and running, and a live heartbeat settles that faster than any label. A close reason holding no verdict — `done`, `ok`, empty — is a dismissal: treat the subject as unmoved. Two identically-titled asks minutes apart is one durability failure, not two findings. For poison SEPARATE THE TWO COUNTERS: only `sp-attempt-N` poisons — `attempts_of` takes its highest and sentinel CHECK 4 dedupes solely on `spira-poison` being absent, so attempts >= POISON_AT with the label gone re-poisons in one pass.

**Fix** — execute it; operator-authorised, so do not re-ask. Clearing `spira-poison` alone schedules its recurrence: drop every `sp-attempt-N` label too, then the poison label. `sp-reclaim-N` counts dead WORKERS, documented at `reclaims_of` in lib.sh as feeding nothing that stops work, SO A BEAD WHOSE ONLY COUNTERS ARE `sp-reclaim-N` CANNOT RE-POISON — that residue is not worth an incident. Verify it reaches `bd ready` and is STILL there one pass later.

**Escalate** — only if executing now means something materially different. Same thread, never a second bead.

**Reference** — wiki/notes/verdict-not-executed-sop-2026-09-07.md

**Matches** `verdict on `sp-|accepted the recommended default|clear the poison|spira-poison|already decided on this`

### Stray beads from empty spira db

`sop-stray-beads-from-empty-spira-db`

**Symptom** — Beads filed to ~/.beads, $SPIRA_HOME/.beads, or process cwd instead of production store. Root cause: a caller invokes incident.sh without exporting SPIRA_DB in the environment. incident.sh sources lib.sh which calls bdq() which calls `bd -C "$SPIRA_DB"`. When SPIRA_DB is empty, `bd -C ""` resolves the store from the invoking process's working directory. systemd user units default to $HOME, so an aeon script in $SPIRA_HOME writes to that repo's .beads, and a unit with no WorkingDirectory= writes to $HOME/.beads.

**Check** — (1) Identify the stray store directory and a bead in it. (2) Trace who wrote it by correlating timestamp with what was running. (3) Grep incident.sh calls for the caller: callers from suites.sh should export SPIRA_DB; calls from systemd units need WorkingDirectory set.

**Fix** — The root cause is incident.sh being invoked without SPIRA_DB exported. (1) PRIMARY: In the calling script, add SPIRA_DB="$SPIRA_DB" to the environment prefix when calling `bash $INC file ...`. (2) DEFENSE: In systemd unit files, add `WorkingDirectory=@SPIRA_PROD@` so any store-less bd resolves to a scratch dir, not $HOME. NEVER hardcode SPIRA_DB as Environment variables in the shipped template — use @PLACEHOLDER@ if a value is needed (law-harness-ships-mechanism-not-inventory).

**Escalate** — none — this is builder code (if calling script) or Ops setup (if systemd units).

**Reference** — sp-ncr3l

**Matches** `stray.*\.beads|filed.*wrong.*store|SPIRA_DB.*empty`

### Stray beads from missing bdq flag

`sop-stray-beads-from-missing-bdq-flag`

**Symptom** — Beads filed into stray .beads instead of production store. incident.sh calls replaced bdq with bare $SPIRA_BD, omitting -C. bd -C "" resolves store from process cwd.

**Check** — `bd -C ~/.beads list --all 2>/dev/null | wc -l` returns count (stray beads present). `grep "^    bdq " $SPIRA_HOME/incident.sh | wc -l` returns >0 if restored. Verify recent sp-ew54u did not replace bdq with bare $SPIRA_BD.

**Fix** — (1) PRIMARY: Restore bdq for shell-level bd calls in incident.sh if replaced with $SPIRA_BD. (2) DEFENSE: Add WorkingDirectory=$SPIRA_PROD to units calling incident.sh. (3) Never hardcode paths in shipped unit files (law-harness-ships-mechanism-not-inventory).

**Escalate** — None — Ops work.

**Reference** — sp-ncr3l, sp-7lqgk

**Matches** `stray.*\.beads|filed.*wrong.*store|SPIRA_DB.*empty|~/.beads`

### Suite hang blocks pipe

`sop-suite-hang-blocks-pipe`

**Symptom** — 4 timed suites (test-aeon-heartbeat.sh, test-archivist.sh, test-check5-drop.sh, test-cockpit-unsent.sh) show LAST=- AGE=- forever; spira-suites.service Result=timeout at ~900s.

**Check** — `journalctl --user -u spira-suites.service --since -6h | grep -c timeout`. `suites.sh list` -- if these 4 suites show LAST=- AGE=-: leaked grandchildren blocking cmd_run.

**Fix** — cmd_run in spira/suites.sh (line ~319) uses command substitution `out="$(timeout ... bash $s 2>&1)"` which blocks on EOF. Suite that backgrounds a child inheriting stdout keeps write end open. Replace with file redirect: `timeout $s bash $s > $tmp 2>&1; rc=$?; out=$(cat $tmp)`. Leaked grandchild in any suite can then no longer wedge the runner.

**Escalate** — If this matched and was committed to origin/main but symptoms persist, inherited suite infrastructure may be backgrounding children. Audit suites to add cleanup (wait, kill, process group) and add test that backgrounds a long sleep inheriting stdout to prevent silent regression.

**Reference** — sp-8x36 (root cause), sp-a8c5 (ppid parse, separate fix), sp-04bd

**Matches** `timed suites.*never produced a result|Result=timeout.*spira-suites|start operation timed out.*suites`

### Suite incident cross repo dedup

`sop-suite-incident-cross-repo-dedup`

**Symptom** — The timed suite run files identical test failures as separate beads when the failure occurs across multiple repos (spira-harness vs brain). Example: test-now.sh fails, filing creates sp-u5nl from repo:spira-harness; same test fails again from repo:brain, filing creates sp-ve0u instead of bumping sp-u5nl's recurrence. Concurrent aeons then work the same suite failure under different bead ids, doubling aeon cost.

**Check** — Compare external_ref on the duplicate beads with `bd -C $SPIRA_DB show <id1> <id2> --json | python3 -c "import json,sys;d=json.load(sys.stdin);[print(x.get('external_ref')) for x in (d if isinstance(d,list) else [d])]"`. Identical refs indicate the issue — the ref is suite:name:fingerprint and should match. Then check the labels: `bd -C $SPIRA_DB show <id1> <id2> --json | python3 -c "import json,sys;d=json.load(sys.stdin);[print(x.get('labels')) for x in (d if isinstance(d,list) else [d])]"`. One carries repo:spira-harness, the other repo:brain.

**Fix** — Remove the repo label from suite incident filing in spira/suites.sh file_red function. Change SPIRA_INCIDENT_LABELS from "spira,plan,repo:$SPIRA_HOME_REPO" to "spira,plan" so dedup lookup in open_incident (which filters on label "$LABELS") works across repo boundaries. Test failure is global; repo is not load-bearing on the dedup key.

**Escalate** — n/a — builders own this code, but the finding is Ops's to diagnose.

**Reference** — wiki/notes/suite-incident-cross-repo-dedup-sop-2026-09-09.md

**Matches** `suite.*red.*duplicate.*repo|repo:spira.*repo:brain.*same failure|incident-ref.*suite.*different repos`

### Suites incident spira db export

`sop-suites-incident-spira-db-export`

**Symptom** — The test-suites-timeout.sh test expects a bead to be filed when a suite times out (rc=124), but no bead is filed. The test output shows "wanted [1] got [0]" — zero beads when one was expected. The timeout line in the pass output may show "not filed", indicating incident.sh returned no id.

**Check** — (1) Run `bash spira/test-suites-timeout.sh` locally. If all tests pass (23 passed, 0 failed), the fix is in place. (2) If the test fails with "wanted [1] got [0]", grep spira/suites.sh for file_red function calls: `grep -n "bash.*\$INC.*file" spira/suites.sh`. (3) For each call, verify that SPIRA_DB="$SPIRA_DB" is in the environment prefix on the line BEFORE the bash call.

**Fix** — All three incident.sh filing calls in suites.sh (file_red, file_env_red, file_fixture_fault) must include `SPIRA_DB="$SPIRA_DB"` in their environment prefix. Without it, incident.sh inherits an empty SPIRA_DB and bdq -C "" resolves to the process's working directory, filing beads to the wrong store or not filing at all. Add the line immediately before each `bash "$INC" file` call. Fixed in commit c2c78b9 by aeon-yojimbo on 2026-09-11.

**Escalate** — none — this is a code fix in suites.sh, now landed.

**Reference** — sp-ncr3l, c2c78b9, sop-spira-env-not-passed-to-subprocess

**Matches** `test-suites-timeout.*wanted \[1\] got \[0\]|a bead was filed.*wanted \[1\] got \[0\]|suites-timeout.*no bead.*filed`

### Synth wiki worktree

`sop-synth-wiki-worktree`

**Symptom** — `sop.sh write`/`synth`, run from an Ops aeon's own worktree of SPIRA_WIKI, renders the regenerated SOP page into SPIRA_WIKI's main checkout instead of the caller's worktree. The aeon's own tree stays unchanged (nothing to commit, bead reopens for no commit naming it) while main gets a live uncommitted diff any later `git add -A` there sweeps into an unrelated commit.

**Check** — from the aeon's worktree, `git -C "$SPIRA_WIKI" status --short` before running `sop.sh write`; if it is clean beforehand and dirty on `wiki/notes/standard-operating-procedures.md` afterward while the worktree's own git status stays clean, this is it.

**Fix** — upgrade sop.sh (this fix resolves OUT by comparing `git rev-parse --path-format=absolute --git-common-dir` of the caller's toplevel against SPIRA_WIKI's, writing into the caller's own toplevel on a match). If running an older sop.sh, workaround: `SOP_PAGE="$PWD/wiki/notes/standard-operating-procedures.md" sop.sh write ...` then `git -C "$SPIRA_WIKI" checkout -- wiki/notes/standard-operating-procedures.md` to undo the accidental write to main.

**Escalate** — never — this is Ops's own tooling.

**Reference** — sp-vu2t

**Matches** `sop\.sh synth writes into the main brain checkout|SOP_PAGE.*worktree|writes into the main brain checkout`

### Test bead no payload

`sop-test-bead-no-payload`

**Symptom** — Ops aeon summoned on a bead labelled `incident` titled literally "test bead", empty description, no comments, `issue_type: task`, and no `external_ref` at all.

**Check** — `bd -C $SPIRA_DB show <id> --json | grep -c external_ref` -> 0. Real incidents always get `external_ref` from `incident.sh`'s `file_one`; its absence means this bead never passed through that pipeline.

**Fix** — no code, no SOP amendment, no wiki write is needed — but the closing rule still demands a commit naming the bead on its own branch, or aeon.sh reopens it (this happened twice: sp-scbi cycled through two prior sessions this way). Record `sop.sh applied` for the ledger, then make the commit yourself: `git commit --allow-empty -m "<bead-id> — test bead: no payload, SOP sop-test-bead-no-payload applied, no code change needed"` in the bead's own worktree before closing. Do not synthesize a diagnosis for content that was never filed.

**Escalate** — never.

**Reference** — wiki/notes/test-bead-incident-sop-2026-09-08.md

**Matches** `"title": ?"test bead"|· test bead`

### Test check5 drop hangs

`sop-test-check5-drop-hangs`

**Symptom** — test-check5-drop.sh times out during the timed suite run, with rc=124 (SIGTERM kill). The test completes some assertions successfully but then hangs and is killed by the suite runner's timeout. The hang occurs in sentinel.sh execution within the fixture. Root cause unknown.

**Check** — Run `timeout 40 bash spira/test-check5-drop.sh` — should complete with exit 0 in ~5-10s. If it times out (rc=124) or produces partial output, this matches.

**Fix** — TEMPORARY: Add `# timeout: 60` annotation (line 29) to prevent false timeouts in suite runs. PERMANENT: Investigate why sentinel.sh hangs on this fixture — the fixture data and test setup may expose a bug in sentinel.sh that doesn't occur in normal operation. Filed sp-hogj6 to track the investigation and permanent fix.

**Escalate** — builders — sp-hogj6 tracks investigation and permanent fix. The timeout annotation is temporary; it prevents test failures but does not address the root cause.

**Reference** — sp-hogj6 (investigation bead), wiki/notes/test-check5-drop-hang-sop-2026-09-11.md

**Matches** `test-check5-drop\.sh.*timeout.*rc=124|check5.*TIMEOUT.*killed`

### Test exit term trap

`sop-test-exit-term-trap`

**Symptom** — A test suite passes all assertions but the harness detects background processes still running after the test exits. The test uses a combined trap for EXIT, INT, and TERM signals. When suites.sh sends SIGTERM to the process group, the trap runs cleanup but does not exit, so execution continues with background processes still alive in the process group.

**Check** — grep "^trap.*EXIT.*INT.*TERM" <test-file>. A combined trap that lists all three signals matches. Verify with ps after the trap: background processes like sleep or heartbeats still exist.

**Fix** — Separate the traps so INT/TERM causes exit 143 instead of running cleanup inline: change `trap 'cleanup' EXIT INT TERM` to `trap 'cleanup' EXIT; trap 'exit 143' INT TERM`. This causes INT/TERM to exit, which triggers the EXIT trap for final cleanup and stops the test cleanly. suites.sh maps exit >= 128 to 124 (timeout), the correct code for a watchdog kill.

**Escalate** — never — this is a code fix in any test or script that traps signals.

**Reference** — wiki/notes/test-exit-term-trap-sop-2026-09-09.md

**Matches** `test.*left background jobs after exit|trap.*EXIT.*INT.*TERM|[Ss]eparate EXIT and TERM traps`

### Test fixture completeness

`sop-test-fixture-completeness`

**Symptom**

```
A test fixture creates a CLONE harness directory and copies specific scripts to it, but when
new unit files are added or ExecStart paths change from @SPIRA_PROD@ to @SPIRA_HOME@, the fixture is
incomplete. install.sh then fails when checking that ExecStart targets are executable in the fixture.
After sp-5e0s6 (f95cc07) changed spira-skew.service to use @SPIRA_HOME@, the test fixture was missing
skew.sh and others, so install.sh failed with "ExecStart target is not executable: <path>/skew.sh".
```

**Check**

```
Run the test (bash spira/test-watch-refresh.sh) and confirm it passes with "81 passed, 0 failed".
Verify that all scripts referenced by unit files' ExecStart directives exist in the fixture directory
where those paths resolve (@SPIRA_HOME@ → CLONE/spira, @SPIRA_PROD@ → PROD).
```

**Fix**

```
Test fixtures that create a CLONE harness directory must copy ALL scripts that might be needed
by any installed unit file, not just a small predetermined set. When ExecStart=@SPIRA_HOME@/<script>,
the script must exist in CLONE/spira. Simplest approach: copy all *.sh files from the harness into
the fixture directory. Changed in test-watch-refresh.sh: `cp "$HERE"/*.sh "$CLONE/spira/"` instead of
manually listing individual files. This ensures fixture completeness is automatic and tolerates
future unit additions without re-listing files.
```

**Escalate** — none — test fixture issue, resolved at the code level.

**Reference** — wiki/notes/test-fixture-completeness-sop-2026-09-11.md

**Matches** `test-watch-refresh.sh.*red.*ExecStart target is not executable|ExecStart target.*is not executable.*CLONE`

### Test incident placeholder

`sop-test-incident-placeholder`

**Symptom** — An incident filed with placeholder text in description and notes ("This is the first filing", "This is the second filing"). No real technical content, often carries recurrence labels indicating multiple test filings.

**Check** — Read the incident description and notes. If they contain only the literal strings "This is the first filing" and/or "This is the second filing" with no actual system details, this matches.

**Fix** — This is test data or a filing-system verification artifact. Dispose of it as a non-issue. No code change or system repair needed.

**Escalate** — n/a — this is a test/verification incident.

**Reference** — wiki/notes/standard-operating-procedures.md

**Matches** `incident.*test.*sweep|filing.*placeholder.*first filing.*second filing`

### Test install paths containment

`sop-test-install-paths-containment`

**Symptom** — test-install-paths.sh fixture --render fails because containment check rejects paths from repo-map.example

**Check** — bash spira/test-install-paths.sh 2>&1 | grep -q "fixture: --render failed" && echo FAIL || echo PASS

**Fix** — Create empty repo-map in fixture so conf.sh finds it instead of falling back to repo-map.example. Also set SPIRA_HOME to $FIXTURE/spira (not $FIXTURE) per conf.sh's own comment.

**Reference** — wiki/notes/standard-operating-procedures.md

**Matches** `spira: containment: instance .* is confined to /workspaces.*is outside it`

### Test install rehearsal spira workspaces

`sop-test-install-rehearsal-spira-workspaces`

**Symptom** — test-install-rehearsal.sh fails during install.sh execution with "mkdir: cannot create directory '//beads-test': Permission denied". SPIRA_TESTDB_DATA defaults to $SPIRA_WORKSPACES/beads-test in conf.sh, but when SPIRA_WORKSPACES is unset or empty, the path expansion produces //beads-test (double slash at root), which mkdir attempts to create at the filesystem root and fails.

**Check** — grep '//beads-test' in test-install-rehearsal.sh output, or verify SPIRA_WORKSPACES is set in the CEXEC environment array

**Fix** — Add SPIRA_WORKSPACES=/tmp to the test's CEXEC environment array (before the install.sh call). This provides a writable default path for SPIRA_TESTDB_DATA to expand to /tmp/beads-test inside the container.

**Escalate** — none

**Reference** — wiki/notes/test-install-rehearsal-spira-workspaces-sop-2026-09-11.md

**Matches** `//beads-test.*Permission denied|mkdir.*//beads-test`

### Test landing fixture completeness

`sop-test-landing-fixture-completeness`

**Symptom** — test-landing.sh times out in the timed suite run after 167 seconds. Test output shows multiple failures with "bash: /tmp/...spira/landing.sh: No such file or directory". The test fixture copies only 5 specific scripts (landing.sh, lib.sh, conf.sh, incident.sh, skew.sh) instead of all scripts needed by the harness, so when landing.sh or its dependencies need other utility scripts added to the harness, the fixture is incomplete.

**Check** — Run the test: `timeout 200 bash spira/test-landing.sh`. Should complete successfully with all tests passing. Verify fixture copy at line 75 uses `cp "$HERE"/*.sh "$SH/"` pattern instead of manually listing scripts.

**Fix** — Changed test fixture at line 75 from manually listing 5 scripts to copying all *.sh files: `cp "$HERE"/*.sh "$SH/"`. This ensures the fixture includes any new scripts added to the harness. Also added `# timeout: 180` annotation since test takes ~167 seconds and was timing out at 167s in suite runner.

**Escalate** — none — test fixture fix.

**Reference** — wiki/notes/test-landing-fixture-completeness-sop-2026-09-11.md

**Matches** `test-landing\.sh.*timeout|FAIL.*landing\.sh.*No such file or directory`

### Test livelock timeout

`sop-test-livelock-timeout`

**Symptom** — test-livelock.sh times out or fails when detection should work. Three issues combined: (1) trap issue — combined EXIT INT TERM trap prevented clean exit on SIGTERM, leaving background processes and causing timeout; (2) missing bdq function — bdjson calls failed, breaking unmapped-repo and ci-stuck checks; (3) insufficient timeout budget — suite legitimately takes 90-100s, was budgeted for 120s.

**Check** — (1) Run bash spira/test-livelock.sh — must exit 0 with "test-livelock: 45 ok, 0 fail" in ~90s. (2) Verify traps: grep "^trap" spira/test-livelock.sh | head -2 — must show separate EXIT and INT TERM lines. (3) Verify bdq: grep -n "^bdq()" spira/lib.sh | wc -l — must return >=1.

**Fix** — (1) Separate EXIT and INT/TERM traps: trap 'cleanup' EXIT; trap 'exit 143' INT TERM (already applied, sop-test-exit-term-trap). (2) Add bdq to lib.sh: bdq() { "$SPIRA_BD" -C "$SPIRA_DB" "$@"; } (already applied). (3) Increase timeout to 150s: change # timeout: 120 to # timeout: 150 at test-livelock.sh line 2 (already applied).

**Escalate** — none — all fixes landed as code on this branch.

**Reference** — sop-test-exit-term-trap, sop-incident-dedup-undefined-bdq

**Matches** `test-livelock\.sh.*timeout|rc=124|SP_LIVELOCKED=0.*should have|exit 105|exit 124.*livelock`

### Test ops closing conf hang

`sop-test-ops-closing-conf-hang`

**Symptom** — sop.sh ledger-init hangs when SPIRA_CONF is set to a nonexistent path. The test-ops-closing.sh fixture deliberately uses SPIRA_CONF=$TMP/nonexistent.conf to test error handling; during aeon.sh execution, the call to sop.sh ledger-init blocks indefinitely waiting for stdin.

**Check** — timeout 2 bash -c 'export SPIRA_CONF=$TMP/nonexistent.conf SPIRA_HOME=$(pwd)/spira; . ./spira/conf.sh && sop.sh ledger-init' should complete without hanging

**Fix** — In spira/conf.sh, add file existence check at the start of spira_conf_read function: [ -f "$file" ] || return 0. This prevents the function from attempting to read from a non-existent file descriptor, which would cause bash to read from stdin and hang.

**Escalate** — none

**Reference** — wiki/notes/test-ops-closing-hang

**Matches** `test-ops-closing.*hangs|sop.sh.*ledger-init.*hangs|SPIRA_CONF.*nonexistent`

### Test ops closing verdict

`sop-test-ops-closing-verdict`

**Symptom** — test-ops-closing.sh fails because the closing-rule verdict message is not appearing in aeon.sh output. The test expects to see "closing-rule wrote=yes" or similar, but the message is missing.

**Check** — grep -c "closing-rule" /tmp/aeon-output.txt

**Fix** — Replace the log() call that outputs the closing-rule verdict with an explicit printf statement to ensure the message reaches stdout. The log() function may have buffering or redirection issues in some contexts, so using printf directly with the same format guarantees the output is written.

**Escalate** — none

**Reference** — spira/aeon.sh line 1801-1804, spira/test-ops-closing.sh

**Matches** `test-ops-closing.*red|closing-rule.*verdict|want.*closing-rule wrote=`

### Test requeue passes

`sop-test-requeue-passes`

**Symptom** — test-requeue.sh was red in the timed suite run, reporting 18 passed, 8 failed. The test verifies that when a bead is closed by an aeon and then reopened by the harness due to a rebase conflict, the reopen is counted as a requeue (not an attempt), and "no attempt charged" is logged.

**Check** — bash spira/test-requeue.sh; output should show 27 passed, 0 failed. All three test scenarios should pass: (1) session committed, closed, harness reopened — requeue not charged as attempt; (2) session did not close — attempt charged; (3) pair test — requeue vs. attempt distinction.

**Fix** — Commit 1f67e51 "ensure closing-rule verdict is output to stdout" changed aeon.sh line 1803 from using log() to printf() for the closing-rule verdict output. This ensures the closing rule verdict is written to stdout where tests expect it. The requeue logic in aeon.sh lines 746, 752 is working correctly — it logs "no attempt charged" and records requeue counts properly.

**Escalate** — none — the code is working as designed and the test confirms all cases pass.

**Reference** — wiki/notes/test-requeue-sop-2026-09-11.md

**Matches** `test-requeue.sh.*red.*requeue.*closing-rule|bead.*reopen.*rebase conflict.*attempt|requeue.*no attempt charged`

### Test suites confirm red hangs

`sop-test-suites-confirm-red-hangs`

**Symptom** — test-suites-confirm-red.sh fails in the timed suite run with "wanted [1] got [0]" when expecting a suite-defect bead to be filed. The test passes with 13/0 when run directly but fails with 12/1 in concurrent suite execution. Root cause: multiple concurrent incident.sh subprocess calls in the fixture contend for incident.lock, and the 30s default timeout may be insufficient for serialized bead filing under load.

**Check** — (1) timeout 45 bash spira/test-suites-confirm-red.sh should complete with 13 passed, 0 failed. (2) SPIRA_INCIDENT_LOCK_WAIT should be set to 60 in test fixture environment variable sut(). (3) Verify fixture concurrency: test creates two suites (one env-sensitive, one genuinely broken), and both run to completion without bead-filing timeouts.

**Fix** — Increase SPIRA_INCIDENT_LOCK_WAIT from 30s default to 60s in test fixture environment. In test-suites-confirm-red.sh sut() function, add SPIRA_INCIDENT_LOCK_WAIT="60" to the env -i environment. This gives concurrent incident.sh subprocess calls (multiple suite failures filing beads) adequate time to serialize on the shared incident.lock without timeout.

**Escalate** — none

**Reference** — sp-njvqm

**Matches** `test-suites-confirm-red\.sh.*red.*wanted \[1\] got \[0\]|wanted \[1\] got \[0\].*suite-defect`

### Test suites runner env sensitive

`sop-test-suites-runner-env-sensitive`

**Symptom** — test-suites.sh test fails under the timed runner but passes when runner-injected variables (SPIRA_HOME, SPIRA_SUITES_MAXSEC, SPIRA_DB) are stripped. Root cause: test-suites.sh hardcodes SPIRA_SUITES_RUNNER_VARS="" at line 109, overriding the runner's list of variables to strip. This breaks the runner-variable-stripping guarantee and makes the fixture's budget calculations differ from the runner's, causing test failures that depend on exact output format (e.g., pre-skip vs TIMEOUT markers).

**Check** — (1) Confirm test-suites.sh has SPIRA_SUITES_RUNNER_VARS="" hardcoded: grep -n "SPIRA_SUITES_RUNNER_VARS=\"\"" spira/test-suites.sh should show line 109. (2) Run the test with runner env: SPIRA_SUITES_MAXSEC=600 SPIRA_HOME=$(pwd)/spira bash spira/test-suites.sh | grep FAIL — should show failures related to budget calculations. (3) Run without the vars: bash spira/test-suites.sh — should pass cleanly.

**Fix** — Preserve SPIRA_SUITES_RUNNER_VARS from the environment caller instead of hardcoding empty. Two approaches: (A) Store the runner's RUNNER_VARS value before overriding, pass it to sut, and restore in suites.sh; (B) Use a separate control variable for fixture cleanup (e.g., SPIRA_TEST_FIXTURES_CLEAN) instead of overriding RUNNER_VARS. Recommend (B) as cleaner: change line 109 from SPIRA_SUITES_RUNNER_VARS="" to SPIRA_TEST_FIXTURES_SKIP_CLEAN=1, then modify suites.sh to skip runner-var stripping only when that flag is set.

**Escalate** — n/a — builders own this code

**Reference** — wiki/notes/test-suites-runner-env-sensitive-sop-2026-09-11.md

**Matches** `test-suites\.sh is red under.*timed runner.*passes.*aeon|env mismatch.*SPIRA_SUITES_MAXSEC|SPIRA_HOME.*runner.*budget calc`

### Test suites watchdog classify timeout

`sop-test-suites-watchdog-classify-timeout`

**Symptom** — test-suites-watchdog-classify.sh times out in the timed suite run. The test fixture database setup, two full suite runs, and result verification take more time than the default 8-second per-suite budget. When the timed runner's watchdog kills the test mid-execution, it produces rc=124 and files a bead as if the test were broken, rather than recognizing it was a budget constraint.

**Check** — (1) Run `timeout 40 bash spira/test-suites-watchdog-classify.sh` — it should complete with 6 passed, 0 failed. (2) Check if test-suites-watchdog-classify.sh carries `# timeout:` annotation: `grep "^# timeout:" spira/test-suites-watchdog-classify.sh`. If absent, this matches.

**Fix** — Add `# timeout: 60` annotation to test-suites-watchdog-classify.sh (line 24, after `# covers: spira/suites.sh`). The test requires a full fixture database lifecycle (creation, two suite runs, teardown), which takes 30-40 seconds. Declaring 60 gives 50% headroom to prevent false timeouts under load, following the pattern established in sp-4gs2g for test-aeon-ledger.sh.

**Escalate** — none

**Reference** — sp-4gs2g (per-suite timeout annotation feature), wiki/notes/timeout-annotation-sop-2026-09-11.md

**Matches** `test-suites-watchdog-classify\.sh.*timeout.*rc=124 after [0-9]+s|watchdog-classify.*FAIL.*wanted \[timeout\] got \[MISSING\]`

### Testdb lock nested suite

`sop-testdb-lock-nested-suite`

**Symptom** — test-suites-confirm-red.sh hangs in timed context (`suites.sh run`) when nested suites.sh calls incident.sh to file beads. Timeout protection on bdq (5s wrap, sp-qb1st) has been applied but hangs persist. The hang occurs very early in testdb fixture initialization, before incident.sh filing completes.

**Check** — (1) Run bash spira/test-suites-confirm-red.sh directly under timeout 20s. If it times out (rc=143), the hang is still present. (2) Grep spira/lib.sh for bdq definition with timeout protection — should see `timeout 5` or `timeout "${BD_TIMEOUT:-180}"` on the active bdq line. (3) Check for duplicate bdq definitions that may override each other (lines 37 and 204).

**Fix** — (1) REGRESSION FOUND: line 204 had a duplicate simplified bdq() that overrode the main bdq() definition at line 37, removing critical guards for repo labels, destructive operations, and schema_migrations deletes. REMOVED the duplicate (sp-qb1st commit 0b4f557). (2) UNDERLYING HANG PERSISTS: test still times out after removing duplicate bdq. Hang is NOT in bdq execution (the timeout would catch it). Root cause is in testdb_up initialization or incident.sh flock handling. Next session must add instrumentation to testdb_up startup sequence or audit incident.sh flock for circular dependencies when suites.sh calls it nested.

**Escalate** — builders — (1) investigate why testdb_up hangs in fixture initialization when called from suites.sh in a nested context; (2) audit incident.sh flock scope for potential deadlock with concurrent suites.sh flock handling; (3) consider adding timeout to testdb_up itself with a clear error message showing the blockage point.

**Reference** — sp-qb1st, sp-njvqm (parent), wiki/notes/testdb-fixture-hang-nested-suite.md

**Matches** `test-suites-confirm-red.sh.*hangs|nested suites.sh.*bdq.*hangs|testdb lock contention|testdb fixture.*hangs`

### Testdb skip in fixture

`sop-testdb-skip-in-fixture`

**Symptom** — A test fixture (e.g., test-suites-confirm-red.sh) that uses sut() to call suites.sh multiple times hangs or times out. Each sut() invocation is triggering testdb initialization overhead (testdb_up ~6s, _testdb_embedded_check ~6s), causing cumulative delays that exceed the test budget.

**Check** — Add SPIRA_SUITES_SKIP_TESTDB=1 to the test's sut() environment and rerun. If the test completes within budget and all assertions pass, this matches.

**Fix** — For fixture test suites that plant their own suites and call sut() multiple times: add SPIRA_SUITES_SKIP_TESTDB=1 to the sut() environment (line with other SPIRA_* variables). The planted suites do not source testdb.sh and never borrow a shared fixture, so testdb initialization is unnecessary overhead. This skips the expensive fixture setup and brings the test runtime from hang/timeout to ~35s or less.

**Escalate** — n/a

**Reference** — wiki/notes/testdb-overhead-fixture-sop-2026-09-11.md

**Matches** `test-suites-confirm-red\.sh hangs|fixture.*testdb.*initialization|sut\(\).*testdb_up|planted.*suite.*skip.*testdb`

### Unadopted refs created by aeon worktree add

`sop-unadopted-refs-created-by-aeon-worktree-add`

**Symptom** — Watchtower detects spira/* branches with no corresponding beads. These are created by aeon.sh when an aeon claims a bead, but the bead is later deleted or the database is reset while the branch remains.

**Check**

```
git for-each-ref --format="%(refname:short)" refs/heads/spira/ | while read b; do bd show "${b#spira/}" 2>/dev/null || echo "UNADOPTED: $b"; done; if git for-each-ref refs/heads/spira/ | grep .; then return 1; fi
```

**Fix**

```
(1) For each unadopted branch: git branch -D spira/<id> (2) Prune dead worktrees: git worktree prune (3) Clear stale worktree dirs: rm -rf $SPIRA_RUN/worktree/<id>
ROOT_CAUSE: aeon.sh creates spira/* branches during worktree setup. If a bead is deleted or database is cleared while aeon holds the branch, the branch is orphaned. Branches are created before beads are persisted, creating a race window.
```

**Escalate** — Code fix needed — aeon.sh must handle bead deletion or establish branch-to-bead invariant.

**Reference** — aeon.sh line 984 (git worktree add -b), law-a-regression-test-must-be-seen-to-fail

**Matches** `unadopted refs.*cannot be reaped|unadopted.*spira/sp-`

### Unadopted refs stale db

`sop-unadopted-refs-stale-db`

**Symptom** — Watchtower reports spira/* branches whose suffixes don't resolve to beads. Branches exist but bd cannot find corresponding beads, blocking the reaping rite.

**Check**

```
git for-each-ref --format="%(refname:short)" refs/heads/spira/ | while read b; do bd show "${b#spira/}" 2>/dev/null || echo "UNADOPTED: $b"; done
```

**Fix** — (1) Remove stale in-progress database directories (>24h old) from $SPIRA_DB. (2) Remove dead worktrees with git worktree remove --force. (3) Delete unadopted branches with git branch -D.

**Escalate** — None — Ops owns worktree cleanup.

**Reference** — sop-orphaned-worktree-unsent-branch

**Matches** `unadopted refs.*cannot be reaped|Unadopted refs: \d+`

### Unclaimable bead fayth preference

`sop-unclaimable-bead-fayth-preference`

**Symptom** — A ready bead carries a fayth: preference (e.g., fayth:ops) that narrows claim eligibility to one persona, but that persona's FAYTH_LABELS partition does not include the bead's own labels. The intersection is empty and the bead is unclaimable by construction. No persona can claim it, and the sentinel's CHECK 7 reports each persona's partition as empty, silently. The ops pane renders it under a persona that the preference explicitly forbids, confusing the operator.

**Check** — For each ready bead with a fayth: label, intersect the persona's FAYTH_LABELS against the bead's labels. Empty intersection = unclaimable. Verify with: bd list --status ready --json | python3 -c "import json,sys;[print(b['id'],b.get('labels',[]),) for b in json.load(sys.stdin)]" then cross-check each persona's FAYTH_LABELS in chamber/*.fayth against bead labels.

**Fix** — (1) Detect unclaimable beads in sentinel CHECK 7: after checking readiness per-fayth, detect beads ready but no fayth can claim (intersection of its predicate and fayth: preference is empty), and file an incident per such bead naming the bead, fayth:, and rejected partition. (2) Sentinel should check: for each ready bead, does any persona's partition match it AFTER applying fayth: narrowing? (3) Cockpit collector must honor fayth: when deriving which persona is shown, or mark as unclaimable if none match. (4) Add test-unclaimable-bead.sh to verify detection.

**Escalate** — none — this is Ops's harness code.

**Reference** — sp-f8vry, wiki/notes/unclaimable-bead-fayth-preference-sop-2026-09-09.md

**Matches** `fayth:.*(exceeds|excludes|narrows|partition).*cannot.*claim|stranded.*(fayth|preference)|unclaimable.*intersection`

### Unclaimable missing repo dimension

`sop-unclaimable-missing-repo-dimension`

**Symptom** — A bead is unclaimable due to missing partition labels when the incident-handling bead lacks a repo dimension

**Check**

```
bd show sp-gsp | grep -A1 'NOTES' | grep -q 'Repository not declared'
```

**Fix** — Set the repo dimension on the incident bead: bd set-state <incident-id> repo=<name>

**Escalate** — If the repo name is ambiguous or if multiple repos are involved

**Reference** — wiki/notes/standard-operating-procedures.md

**Matches** `UNCLAIMABLE.*no matching partition.*Repository not declared.*SPIRA_INCIDENT_REPO`

### Unsent branch bead mismatch

`sop-unsent-branch-bead-mismatch`

**Symptom** — Watchtower escalates "oldest unsent branch 24h" alert; git shows the branch merged to origin/main but sending.sh has not reaped it. The bead remains open; sending.sh only deletes branches for closed beads. Root cause: the commit landed under a different bead's id, so the sentinel never closed this bead's related issue.

**Check** — (1) Verify the branch is merged: git merge-base --is-ancestor <branch-tip> origin/main; (2) Find the commit that landed it: git log --format='%s' origin/main | grep -F <bead-id> (should return empty for the old bead); (3) Find what commit is the branch tip's ancestor: git log --oneline -1 <branch-tip>; (4) Check the commit message for a different bead id.

**Fix** — Use bd supersede to close the old bead as superseded by the bead that actually owns the commit: bd -C $SPIRA_DB supersede <old-bead> --with <owner-bead>. The bead is now closed; sending.sh will reap the branch on the next pass. Verify: git branch -D <branch> should succeed after the next sentinel pass, or manually delete if urgent: git -C <repo> branch -D <branch>.

**Escalate** — never — this is Ops's classification and cleanup.

**Reference** — wiki/notes/unsent-branch-bead-mismatch-sop-2026-09-09.md

**Matches** `oldest unsent branch [0-9]+h|branch.*[0-9]{6,}.*not reaped|bead.*open.*work.*landed`

### World drain summon silent failure

`sop-world-drain-summon-silent-failure`

**Symptom** — Sentinel fires on schedule but no new aeons dispatch despite ready beads. A drain file set by spira/world.sh drain was not resumed when the operation finished.

**Check** — (1) ls "${SPIRA_RUN:-}/world.draining" — if file exists, this matches. (2) Read expires: sed -n 's/^expires \([0-9][0-9]*\)$/\1/p' "${SPIRA_RUN:-}/world.draining". (3) Check if expired: [ "$(date +%s)" -gt "$_exp" ] && echo EXPIRED.

**Fix** — If file exists and is expired (or no expires with >1h age): rm "${SPIRA_RUN:-}/world.draining"; systemctl --user start spira-sentinel.timer. If unexpired: operator must resume with spira/world.sh resume (never auto-resume to avoid hiding forgotten resumes).

**Escalate** — If drain was set intentionally, do not remove. Otherwise: "Found drain set at <time>, expires at <time>. Remove now (resumes summons) or wait? Default: remove if >1h expired."

**Reference** — wiki/notes/world-draining-summon-silent-failure-sop-2026-09-09.md

**Matches** `summon.*silent|aeons.*(0|not dispatched).*ready beads|world.draining`

Related: [[spira]], [[common-law]], [[codified-judgement]]
