# landing-pass — the one landing pass, every land mode

Replaces `spira/landing.sh` (1,893 lines; CHECK 6's worker) and `spira/landing-lib.sh`,
and keeps the pr-mode pass this crate already was. One binary, `landing-pass`, four verbs.
This document is the contract; it was written from the script's intent and its callers,
not by porting it line by line. The code satisfies it and the unit tests are derived from it.

## 1. Intent

A bead's work is finished when its branch is closed; it is **landed** when its commit is on
the repository's landing ref (law-closed-is-not-landed). The landing pass is the stretch in
between. For every repository, for every `spira/<id>` branch whose bead is done, it moves
the branch one step toward the landing ref by the repository's **own** land mode, judges it
exactly once per tip, charges the branch only for faults that are the branch's own, and
leaves a record of every transition so the next pass never re-derives what this one knew.

| mode | what the pass does with a done branch |
|---|---|
| `pr` | rebase, confine, force-push, open/refresh the PR; observe merged/closed. No local gate — the PR's CI is the gate. (`landing-pass --pass`, its own 90 s timer.) |
| `push` | rebase (re-cut on conflict), confine, gate, merge in a private landing worktree, push to the base; after the repository's walk, rebase the survivors **once**. |
| `hold` | rebase, confine, gate, note the bead "gated and held", never advance the base. |
| `queue` / `queue.forge` | **certify**: gate the tip once, mark `CERTIFIED`. No local rebase (law-a-round-takes-certified-tips). Settling/cutting rounds is the `queue` binary's `step`. |
| `queue.local` | same certification as `queue`. The round lands via `queue land-local`, publishes via `queue publish`. |

Around the walk the pass also: settles each queued repository's open round before and after
certifying (`queue step`), prunes the landstate records nothing will read again, advances
the checkout humans read (`skew.sh refresh`) for push/queue, asks about closed beads whose
GitHub issue has no landing (`_gh_unlanded_scan`), installs unit templates that landed
(`unit-ensure.sh`), and rebuilds binaries whose source landed (`land-build-ensure.sh`).

### Four outcomes, one of them the branch's fault

`gate.sh` returns PASS (0), FAIL (anything else), BASE_FAIL (76) or NO_VERDICT (75).
Nothing lands on the last three. Only FAIL reopens and charges. BASE_FAIL files one
incident per repository per pass (a base-fix branch that is green on the red suite is
certified instead). NO_VERDICT is counted and escalated by `spira_land_noverdict`.

### What it is deliberately NOT

- **Not the round.** Cutting, proving and landing a round is `batcher`/`queue`; this pass
  calls `queue step` and nothing else of the queue.
- **Not the gate.** `gate.sh` stays bash and is called as a subprocess with its argv/env
  contract (§2.5). The gate string (repository's own: fences, then the budgeted
  `gate-touched.sh` selector of sp-vq2za, then `testenv-batch.sh`/`testenv`) is the gate's
  business; this pass never selects suites.
- **Not the rebase machinery, the reopen, the asks or the delivery machine.** `rebase_branch`,
  `recut_onto`, `bead_reopen`, `bead_close_on_land`, the `spira_ask_*` family, the
  `lc_deliver_*` wrappers, `land_mark` (with its TSD dual-write) stay in lib.sh and are
  reached through one seam (§6).
- **Not a config reader.** Every setting and every repository row comes from conf.sh/lib.sh
  through the context seam — the one resolver (law-config-through-the-cli-only). No Rust
  here parses the repository map or the config document; the pr pass's private parser and
  its private `spira-lc` path resolver are removed (§8 D1).
- **Not the poison ledger.** The rebase-escalation counter is lib.sh's raw `requeues_of`
  (how many times this branch conflicted). spira-claim's `requeues` deliberately excludes
  rebase returns (sp-j1q6o) and answers a different question; this pass makes no attempt or
  poison decision.

## 2. Contract

### 2.1 Invocation

```
landing-pass --pass                 pr-mode pass (unchanged; spira-landing-pass.timer). Alias: `landing-pass pr`.
landing-pass land                   the gated pass: push, hold, queue, queue.local (+ steps around it)
landing-pass halt [--reason T | --reason-file F|-] [--dry-run]
landing-pass sweep-red
landing-pass -h | --help
```

`land` replaces `landing.sh` with no arguments; `halt`/`sweep-red` replace `landing.sh halt`
/ `landing.sh sweep-red`. `--reason-file` (stdin with `-`) is new: the payload-safe form
(law-payloads-go-on-stdin); `--reason T`/`--reason=T` stay for today's callers.

**Exit codes.**

| verb | 0 | 1 | 2 | 143 |
|---|---|---|---|---|
| `--pass` | pass ran or was skipped (halted world, lock held, no pr repos) | could not open its lock | usage | — |
| `land` | pass completed (whatever it moved) | the context seam failed (no lib.sh, no conf) — nothing was touched | usage | SIGTERM/SIGINT (RuntimeMaxSec or `halt`) |
| `halt` | halted (or `--dry-run`: a pass is running) | no pass running | usage | — |
| `sweep-red` | listed (or "no RED" line) | landstate dir missing | — | — |

### 2.2 Environment read

`SPIRA_HOME` (required: where lib.sh lives), and whatever conf.sh reads — the sentinel's
dispatch passes `PATH HOME SPIRA_HOME SPIRA_RUN SPIRA_DB SPIRA_REPO SPIRA_REPO_MAP
SPIRA_HOME_REPO SPIRA_BD SPIRA_GH SPIRA_BATCH_MAXPAR SPIRA_LAND_MAXSEC`. Everything else is
read from the context seam's answer: `SPIRA_RUN`, `SPIRA_REPO`, `SPIRA_DB`, `SPIRA_BD`,
`SPIRA_LAND_MAXSEC` (3600), `SPIRA_LAND_GATE_RESERVE` (conf 2700), `SPIRA_GATE_LOCK_WAIT`,
`SPIRA_VERDICT_TTL` (86400), `SPIRA_VERDICTS`, `SPIRA_DEFERRAL_ESCALATE_AT` (5),
`SPIRA_EXPRESS_LABEL`, `SPIRA_CUTOVER_ROUND_LABEL`, `SPIRA_SUBMITTED_LABEL`,
`SPIRA_REBASE_ESCALATE_AT` (3), `SPIRA_GIT_NAME/EMAIL`, `SPIRA_INCIDENT`,
`SPIRA_SCOPE_LABEL`, `SPIRA_ID_PREFIX`, `SPIRA_QUEUE_BIN`, `SPIRA_QUEUE_DIR`,
`SPIRA_LC_BIN`, `SPIRA_PROD`, `SPIRA_HALT_GRACE` (30), `BD_TIMEOUT` (180),
`SPIRA_BDJSON_FIXTURE` (tests only). The pr pass also honours
`SPIRA_PR_PASS_BRANCH_SH`, `SPIRA_LANDING_PASS_LOG` as before.

### 2.3 Output

stdout carries lines `<UTC %Y-%m-%dT%H:%M:%SZ> spira: <message>` (lib.sh `log`'s shape);
the unit appends stdout+stderr to `$SPIRA_RUN/landing.log` (land) or
`$SPIRA_RUN/landing-pass.log` (pr). Message texts are landing.sh's, byte for byte, where
the behaviour is kept — operators and suites grep them. Lines printed by a seam call (lib.sh
`log`) pass through unchanged. The ones with readers:

| line | reader |
|---|---|
| `landing: starting a pass over [<repo> …]` / `landing: pass complete — N branch(es) seen, M movement(s)[, S survivor(s) rebased after a landing, C conflicted]` | operators, suites |
| `CHECK6 <id>: …` per-branch lines | operators, suites, cockpit tails |
| `queue early: <line>` / `queue late: <line>` | operators |
| `landing: budget cut at <br> — Ns left, K branch(es) deferred in <repo>` | operators, watchtower tail |
| `landing: pruning landstate/<id> — closed bead with no branch (state was S)` | operators |

### 2.4 Files, records and store rows

All paths under `$SPIRA_RUN` unless absolute. Formats unchanged (§3).

| file | read | written |
|---|---|---|
| `landing.status` | sentinel CHECK 6, cockpit, canary | every exit path of `land` (atomic): `SP_LAND_AT/RC/BRANCHES/MOVED` |
| `landing.progress` (mailbox) | sentinel `land_drain`, cockpit | appended once per movement (`progress`) |
| `landing.run` | `halt` | `land` at each phase (atomic); removed on exit |
| `landing.containers` | `halt` | truncated at start; appended by testenv (via `SPIRA_LANDING_CONTAINERS`); removed on exit |
| `landing.cursor` | `land` (rotation) | on a budget cut: the repo name |
| `landing.deferred/<br with / → _>` | `land` | per-branch deferral count; removed when visited |
| `landing.interrupted` | operators | `halt` (atomic) |
| `landing.lock` (new) | — | `land` flock (§8 D7) |
| `landstate/<id>` | every branch (WITHDRAWN/CERTIFIED/RED/EJECTED), prune, `sweep-red` | only through lib.sh `land_mark` (seam), removed only by the prune (§5) |
| `landstate/<id>.ejected` | — | removed on LANDED/CONTENT and by the prune |
| `submitted/<id>` | hold (`submitted`) | `mark_submitted` shape, atomic (hold, certified) |
| `noverdict/<branch-key>*` | — | removed on a PASS |
| `verdicts/*` | — | files older than `SPIRA_VERDICT_TTL` deleted, one by one |
| `worktree/.landing.<repo-basename>` | push | created detached at the base (after `spira_prune_worktrees`) |
| `landing-pass.lock`, `landing-pass.log`, `world.halted` | pr pass | pr pass |
| bd (via `bd show --json`, chunked 100) | status, labels, deps, `closed_at`, `priority`, `external_ref` | never directly; reopen/close/note via seam |
| git: `refs/heads/spira/*` | enumerate, tip | moved only by `rebase_branch`/`recut_onto` (seam); base pushed by push mode |

### 2.5 Programs called (argv carries identifiers only)

| program | argv / env | from |
|---|---|---|
| `$SPIRA_HOME/gate.sh <branch> <repo>` | env `SPIRA_GATE_LOCK_WAIT=<secs>`, `SPIRA_GATE_BEAD=<id>`; stdout+stderr captured together | push/hold/queue walk |
| `$SPIRA_HOME/gate-run.sh --status <branch> <repo>` | read-only | cert-gate-red note |
| `$SPIRA_HOME/confine.sh <id> <branch> <repo-path> <base> <labels>` | labels are the bead's own label words | push/hold |
| `$SPIRA_QUEUE_BIN step <repo>` | stdout+stderr combined, relabelled | queued repos, before and after the walk |
| `$SPIRA_HOME/skew.sh refresh <repo-path>` | | push, queue |
| `$SPIRA_REPO/systemd/unit-ensure.sh`, `$SPIRA_REPO/spira/land-build-ensure.sh` | | once per pass, when executable |
| `$SPIRA_PROD/testenv.sh down --name <c> --volumes --force-foreign` | | `halt` |
| `$SPIRA_PR_PASS_BRANCH_SH <repo> <br> <id> <base> <name> <tip>` | exits 0–8 as documented there | pr pass (unchanged) |
| `$SPIRA_LC_BIN show/event delivery …` | | pr pass content proof (unchanged) |
| `bd -C $SPIRA_DB show <ids…> --json` under `timeout $BD_TIMEOUT` | one retry on "invalid connection" | scan, re-reads, prune |
| `git` | | everything else |

### 2.6 Callers (every one found)

| caller | how | relies on |
|---|---|---|
| `sentinel/src/dispatch.rs:325-366` CHECK 6 (the sentinel crate; sentinel.sh is gone) | `systemd-run --user --collect --unit=spira-landing … --setenv=SPIRA_LIFECYCLE_ENFORCE=… [--setenv=SPIRA_LC_BIN=<disabled>] $SPIRA_HOME/landing.sh` | unit name as mutex; `landing.status`, `landing.progress`; stdout → `landing.log` |
| `systemd/spira-landing-pass.service:8` | `@SPIRA_LANDING_PASS_BIN@ --pass` (timer every 90 s) | pr pass |
| `spira/canary.sh:160` | `bash "$SPIRA_HOME/landing.sh"` | a real pass; `landing.status` |
| `spira/world.sh:174-175` `live_workers` | argv match on `landing.sh` | halt completeness |
| `spira/chamber/czar.fayth:90`, `czar.md:122` | `landing.sh halt` | the verb |
| ~45 bash suites (§7.4) | run the pass, source landing-lib.sh, grep landing.sh | — |

### 2.7 Guarantees

1. **A LANDED record whose tip is not on the forge is never deleted** (§5; absorbs the
   `landstate-keep-unpublished` override, sp-bauwt).
2. **Never rebase, reopen or land under a live holder** (`hold-<id>.pid` live, or
   `aeon-*-<id>.pid` live with `aeon.sh` in its cmdline) — checked before the walk's rebase
   and again before each survivor's.
3. **Only FAIL reopens.** BASE_FAIL/NO_VERDICT never charge; a rebase failure other than
   `conflict` never reopens.
4. **Re-read before acting.** After a gate, the bead's status is read again; not closed
   (closed = `closed` or carries `SPIRA_SUBMITTED_LABEL`) → no certify, reopen or land.
5. **One judgement per tip.** CERTIFIED at the current tip is skipped; WITHDRAWN at the
   current tip stays withdrawn; RED `no-rebase@<base>` at the same tip and base is not
   re-attempted.
6. **Never begin a gate the pass cannot finish**: with `SPIRA_LAND_MAXSEC > 0`, a gate
   starts only while `maxsec - elapsed ≥ reserve`; base-fix branches are exempt.
7. **The status file is written on every exit**, including SIGTERM (exit 143).
8. **survivors are rebased once per repository per pass** (sp-4hs0i), after its walk,
   only if something landed there.

## 3. Schema

```rust
enum LandMode { Push, Pr, Hold, Queue /* queue | queue.forge */, QueueLocal }

/// One repository as lib.sh resolves it (context seam).
struct RepoRow { name: String, path: PathBuf, mode: LandMode,
                 landref: Option<String>,        // spira_landref; None → skipped loudly
                 base_fq: Option<String>,        // qualify_base_ref(landref)
                 base_remote: Option<String>,    // ref_remote(landref), None for a local base
                 base_branch: String,            // ref_branch(landref)
                 forge_ref: Option<String> }     // queue.local: refs/remotes/<publish remote>/<branch>; else base_fq

/// conf.sh's settings, as the context seam exports them.
struct Settings { home, run, repo, db, bd, bd_timeout, home_repo, id_prefix,
                  land_maxsec: i64, gate_reserve: i64, gate_lock_wait: Option<u64>,
                  verdict_ttl: u64, verdicts: PathBuf, deferral_escalate_at: u32,
                  express_label, cutover_label, submitted_label, rebase_escalate_at: u32,
                  git_name, git_email, incident: PathBuf, scope_label, queue_bin: Option<PathBuf>,
                  prod: PathBuf, halt_grace: u64, bdjson_fixture: Option<PathBuf> }

/// One bead from the bulk scan (bd show --json).
struct BeadRow { id, status /* submitted label ⇒ "closed" */, repo /* repo: label or home */,
                 labels: Vec<String>, superseded: bool /* dependency_type|type == supersedes */,
                 closed_at: String /* "9999-99-99" when absent */, priority: i64 /* 9999 */,
                 external_ref: Option<String> }

/// `landstate/<id>`: "<STATE> <tip|none> <epoch> [reason…]", no trailing newline.
struct LandState { state: String, tip: String, at: u64, reason: String }

/// `submitted/<id>`: "<tip> <epoch> <state> <refreshes>\n".
struct Submitted { tip: String, at: u64, state: String, refreshes: u32 }

enum GateOutcome { Pass, Fail, BaseFail, NoVerdict }        // from the exit status
struct GateRun { rc: i32, outcome: GateOutcome, out: String,
                 reason: Option<String>,   // last "gate: VERDICT=X reason=R …"
                 suite: String }           // last "… suite=S", "-" when none

/// The certify order row (landing-lib.sh certify_order).
struct OrderRow { id, branch, priority: i64, closed_at, external_ref, express: bool }

struct StatusFile { at: u64, rc: i32, branches: u64, moved: u64 }        // landing.status
struct RunRecord  { pid: u32, started: u64, repo, branch, phase }         // landing.run
struct Interrupt  { halted: String, reason, pid, elapsed, repo, branch, phase } // landing.interrupted

/// What rebase_branch / recut_onto report (their globals, through the seam).
struct Rebase { ok: bool, failure: String /* conflict|rebase-refused|no-base|no-branch|no-worktree */,
                conflicts: String, refused_reason: String }
struct Recut  { ok: bool, applied: u32, conflicts: String }

/// Every per-branch decision the walk can reach, for tests and the log.
enum BranchVerdict { Gone, NotClosed, WrongRepo, Superseded, CutoverRound, Ejected, Content,
                     Held, Withdrawn, AlreadyCertified, BudgetCut, Certified, Red, BaseHeld,
                     NoVerdict, Submitted, Landed, Deferred, Reopened, Escalated }
```

## 4. The gated pass, step by step (`land`)

1. Resolve settings + every repository through the context seam (one bash). Failure → exit 1
   with nothing touched.
2. Take `landing.lock` (non-blocking; held → log "landing: already running — skip", exit 0).
   Install SIGTERM/SIGINT → forward TERM to the running child, write the status, exit 143.
   Write `landing.run` (pid, started), truncate `landing.containers`, export
   `SPIRA_LANDING_CONTAINERS` to children.
3. Log the start line. Delete `verdicts/*` older than `SPIRA_VERDICT_TTL` (one file at a time).
4. **Queue step, before**: for each queued repo, `queue step <repo>`; each output line logged
   as `queue early: <line>`.
5. Rotate the repository list to start at `landing.cursor` if it names one.
6. For each repository — the walk (§4.1 push/hold, §4.2 queue).
7. **Prune** (§5).
8. **Queue step, after**: `queue late: <line>`.
9. `skew.sh refresh <path>` for push and queue(forge) repositories (queue.local's base is
   local; its refresh belongs to land-local, as before).
10. `_gh_unlanded_scan` (seam). `unit-ensure.sh`, `land-build-ensure.sh` (lines logged).
11. The pass-complete line; status file; remove `landing.run`/`landing.containers`.

### 4.1 Per repository: enumerate, scan, order

- Enumerate `refs/heads/spira/*` with tips (count them all into `branches`, pr included).
  None → next repository. pr → nothing more here.
- No landref → "CHECK6 <name>: cannot resolve the ref its branches land on — skipped. …".
  Fetch the base's remote if it has one. push: ensure the landing worktree.
- One bulk bead read for every id. Order by `certify_order` (bucket 0 base-fix
  `external_ref = basefail:<name>:*`, 1 express label, 2 rest; each by priority then
  closed_at). Log the base-fix and express fronts.

For each branch, in order (`landing.run` repo/branch):

1. Gone since enumeration → one of three lines (tip on base / not on base / unknown); skip.
2. Not closed → "not landed — its bead is S, held by a live aeon | and no aeon holds it".
3. Bead's repo path ≠ this path → WrongRepo line. Superseded → line. Labelled
   `SPIRA_CUTOVER_ROUND_LABEL` → line.
4. landstate `EJECTED` → `land_mark RED <tip> ejected-not-requeued`, `bead_reopen batch-eject`,
   progress "reopened <id> — ejected-not-requeued".
5. `content_landed(branch, base_fq)` → "<base> already contains every change on <br> —
   nothing to land", `land_mark CONTENT`, drop `.ejected`.
6. Live holder → "a live aeon still holds <br> — deferring the land".
7. **queued** → §4.2. **push/hold** → §4.3.

### 4.2 Queue certification (queue, queue.local)

tip; landstate: WITHDRAWN@tip → "withdrawn at <tip> — staying WITHDRAWN until the tip
changes"; CERTIFIED@tip → silent skip. Budget (base-fix exempt; cut → §4.4). `landing.run`
phase=gate; `land_mark GATING tip`; run the gate. Then by outcome:

- **FAIL/BASE_FAIL/NO_VERDICT** → log `certification gate <OUTCOME> …`, `land_mark GATED tip
  <OUTCOME>:<reason>`.
  - BASE_FAIL: "held — the base fails its own gate (suite S)"; a base-fix branch green on its
    suite (`basefail_fix_decision`) → re-read status, closed → `CERTIFIED` + submitted
    `certified` + progress "certified <br> in <name> — base-fix (suite S)"; otherwise file the
    base incident **once per repository per pass**.
  - NO_VERDICT → `spira_land_noverdict` (seam; its `progress` lines cross the mailbox).
  - FAIL → re-read; not closed → "bead is now S … — not reopening"; else `bead_reopen
    cert-gate-red` with the note (commits on the branch, the prior-PASS scope note from
    `gate-run.sh --status`, the gate's last 20 lines), progress, `spira_event bead.reopened`,
    `land_mark RED tip gate`.
- **PASS** → cached note; clear `noverdict/<key>*`; re-read; closed → `land_mark CERTIFIED`,
  submitted `certified`, progress "certified <br> in <name> — gate passed, round and CI are
  the remaining judges".

### 4.3 push and hold

hold: submitted at this tip → skip. RED `no-rebase@<current base sha>` at this tip → skip.
`rebase_branch`; on failure: not `conflict` → log (and `spira_ask_rebase_refused` when
refused), skip; `pr_merged` → skip; RED at same tip → refresh mark, skip; else
`bump_requeue merge-conflict`, `requeues_of`, reopen note, other beads, `recut_onto`:
- recut clean → "re-cut … — falling through to gate";
- partial (applied > 0) → `spira_ask_rebase_loop` + progress "escalated … re-cut conflicted";
- total, previous RED was no-rebase → `spira_ask_red_recurring` + progress;
- total, requeues ≥ `SPIRA_REBASE_ESCALATE_AT` → reopen + `spira_ask_rebase_loop` + progress;
- total otherwise → reopen `rebase-conflict` + progress + event;
  the last three mark `RED <tip> no-rebase@<base sha>`.

Judged-set add. `confine.sh`: 1 → reopen `confine-fail`, progress, `RED confine`; other
non-zero → defer. Budget. Gate (as §4.2 but log `gate …`, reopen cause `gate-red`, note
"failed <name>'s landing gate"). Re-read. hold → note "Gated and held …", submitted `hold`,
act line. push → the land (§4.5).

### 4.4 Budget cut

At the first branch refused a gate: stop this repository; log the budget-cut line; write
`landing.cursor = <name>`; every branch from the cut onward gets its deferral counter +1
(≥ `SPIRA_DEFERRAL_ESCALATE_AT` → `spira_ask_budget_deferred`); every branch before it has its
counter removed. A repository walked to the end removes every counter it walked.

### 4.5 The push land

No landing worktree → defer. Up to 3 attempts: checkout `-B landing <base_fq>` (fails →
wedged); merge `--no-edit -m "$(land_subject <id>)"` (conflict → record files, abort);
HEAD unmoved → nothing; `spira_git_push <land> -q <remote> landing:<base-branch>`; stderr
classified: not a race (`non-fast-forward|fetch first|rejected`) → blocked; race but the
remote base did not move after a fetch → blocked; else sleep `attempt`, rebase the branch,
re-read the tip, content-landed → nothing.

Outcomes: wedged / no-rebase / nothing → one log line each. **Landed** → progress "landed
<br>", `land_mark LANDED tip <name>` first, drop `.ejected`, `lc_deliver_push_delivered`,
`gh_issue_closeout`, `bead_close_on_land`, `spira_event bead.landed`, remove from the judged
set, mark the repository "moved". Merged-but-unpushed → reset the worktree; blocked or
`lc_deliver_push_requeued`. Conflict → fetch; ancestor or 0/`?` commits ahead → "introduces
nothing new … not reopening" + landed event; else "genuinely conflicts", note naming other
beads, `lc_deliver_push_returned`, `bead_reopen rebase-conflict`, progress, event.

After the repository's walk, if it moved: **`rebase_survivors` once** over the judged set
(§8 D3).

## 5. The landstate prune — what it is for now

**Purpose.** Bound `landstate/` for beads that no reader will look up again. Its readers
(gate.sh's `.ejected`/EJECTED lookup, the batch builder's CERTIFIED scan, watchtower's
throttle depth and LANDED drain, CHECK 5's corroboration, verdict.sh's attribution, the
publish tip lookup) all key on a bead that still has a branch **or** a LANDED record the
forge does not yet carry. The queue binary's `publish` now derives members from the land
commits in `forge..local` (queue/DESIGN.md §8 D4), so publish no longer depends on the
record surviving — but the record still carries the tip publish prefers and the drain
watchtower meters, so it must outlive its publication, not its branch.

**Rule.** A record `landstate/<id>` (sidecars `<id>.*` are never candidates) is removed,
with its `.ejected`, only when **all** hold:

1. its bead is `closed` in bd (read in bulk; unreadable → keep);
2. no repository has `refs/heads/spira/<id>`;
3. if its state is `LANDED`: its tip resolves in some repository **and** is an ancestor of
   that repository's **forge ref** — `refs/remotes/<publish remote>/<publish branch>` for
   queue.local, the base ref otherwise. A tip that resolves nowhere, or is on no forge ref,
   is kept (never delete a LANDED record whose tip is not on the forge).

The forge refs are read as last fetched; a stale one only delays a prune.

## 6. lib.sh seams

One mechanism (the queue crate's, §6 there): `bash` with **no arguments**, reading from stdin
a fixed script (compiled into the binary per operation) followed by the operation's values,
each NUL-terminated. The script reads every value, detaches stdin, sources `lib.sh` (and
`lc-delivery.sh` where named), defines `progress` (prints `\x1f<line>`, which the binary
turns into a mailbox movement) and `act` (= `log`), calls exactly one function, and prints
its answer after `\x1e`. Nothing in argv or the environment. Payloads (notes, gate output)
are values like any other.

| seam | lib.sh function(s) | why not Rust |
|---|---|---|
| S1 `context` | conf.sh settings; `spira_repos`, `repo_root`, `repo_land`, `spira_landref`, `qualify_base_ref`, `ref_remote`, `ref_branch`, `spira_publish_forge`, `spira_home_repo` | the one resolver every component reads |
| S2 `land_mark` | `land_mark <id> <state> <tip> [reason]` | landstate + TSD dual-write |
| S3 `reopen` | `bead_reopen <id> <cause> <note>` | WITHDRAWN, submitted label, release, cause event |
| S4 `event` | `spira_event <kind> <id> <title> <detail>` | rate-limited events log |
| S5 `noverdict` | `spira_land_noverdict …` | per-class counters, machinery ask |
| S6 `incident` | `incident.sh file <title> -` with `SPIRA_INCIDENT_*` set **inside** the script | spooled, deduped intake |
| S7 `ask_*` | `spira_ask_rebase_loop / _red_recurring / _rebase_refused / _budget_deferred` | mail wiring, dedupe |
| S8 `rebase` / `recut` | `rebase_branch`, `recut_onto` (+ their globals) | scratch worktrees, salvage, formatter |
| S9 `requeue` | `bump_requeue <id> merge-conflict`; `requeues_of <id>` | events table |
| S10 `conflict_note` / `other_beads` | `conflict_reopen_note`, `other_beads_on_conflicts` | shared with pr-pass-branch.sh |
| S11 `pr_merged` | `pr_merged <repo> <br>` | ghq wiring |
| S12 `note` | `bdq note <id> <text>` | bdq retry/czar/fixture |
| S13 `push` | `spira_git_push <tree> -q <remote> <refspec>` (stderr returned) | GitHub App credentials |
| S14 `land_subject` | `land_subject <id>` | merge subject with title |
| S15 `deliver_*` | `lc_deliver_push_delivered / _requeued / _returned` (lc-delivery.sh) | delivery CAS |
| S16 `closeout` | `gh_issue_closeout`; `bead_close_on_land` | issue close + close + reap |
| S17 `prune_worktrees` | `spira_prune_worktrees <repo>` | the one destruction site |
| S18 `gh_unlanded_scan` | `_gh_unlanded_scan` | GitHub asks |

**Rust, against the same data:** the landstate and submitted records (reads; submitted
writes), the certify order, `basefail_fix_decision`, `prior_pass_suites`, the gate outcome
mapping (`spira_gate_outcome`/`spira_gate_blames_branch`), `gate_fits`, `gate_lock_wait`,
`content_landed`, `holder_alive`, the bead scan (bd reads), the status/run/mailbox/cursor/
deferral files, the prune, `halt`, `sweep-red`, the verdict-cache prune, the push merge loop
(git), the pr pass (unchanged apart from §8 D1-D2).

## 7. Cutover

**Not performed** (operator's directive: no bash, unit or workflow edits). Line numbers are
against this branch after its merge of `concierge/batch-rust` (queue, sentinel, aeon,
testenv and spira-claim already cut over there). Production's checkout additionally carries
the `landstate-keep-unpublished` hunk in landing.sh (landing.sh:1834-1839 there), which
dies with the file. Apply in one landing, in this order. `$SPIRA_LANDING_PASS_BIN` is
already resolved by conf.sh (conf.sh:1531) and already in its key list (conf.sh:73).

**Urgent finding:** on `concierge/batch-rust` `spira/queue.sh` is deleted but
`spira/landing.sh:1756,1767,1845` still run `bash "$SPIRA_HOME/queue.sh" step`, so a
`landing.sh` pass on that tree settles and cuts no round at all (each step logs a bash
"No such file" line and is ignored). This cutover removes those calls with the file; if
landing.sh must run one more time first, repoint them to `"$SPIRA_QUEUE_BIN" step "$repo_name"`.

### 7.1 The worker the sentinel dispatches

| # | file:line | current | replacement |
|---|---|---|---|
| 1 | `sentinel/src/cfg.rs:399` (after `lc_bin: bin("SPIRA_LC_BIN", "spira-lc"),`) and `:252` | — | `landing_bin: bin("SPIRA_LANDING_PASS_BIN", "landing-pass"),` and the field `pub landing_bin: Option<String>,` beside `lc_bin` |
| 2 | `sentinel/src/dispatch.rs:365` | `a.push(self.script("landing.sh").to_string_lossy().into_owned());` | `a.push(self.cfg.landing_bin.clone().unwrap_or_else(\|\| "landing-pass".into())); a.push("land".into());` (the `--setenv` list above it is unchanged: `SPIRA_LIFECYCLE_ENFORCE` and, OFF, the disabled `SPIRA_LC_BIN` are exactly what landing-pass reads, §9; a missing binary makes the launch fail, which the existing "could not dispatch the landing worker" escalation already reports) |
| 3 | `sentinel/src/dispatch.rs:3,266` | comments naming `landing.sh` | name `landing-pass land` |
| 4 | `sentinel/src/tests.rs:441,1569` | `.find(… s.args.iter().any(\|x\| x.ends_with("/landing.sh")))` | `.find(… s.args.iter().any(\|x\| x == "land"))` (and give the fixture a `SPIRA_LANDING_PASS_BIN`) |
| 5 | `sentinel/DESIGN.md:132,133,206` | "written by landing.sh", "`spira-landing` runs `$SPIRA_HOME/landing.sh`" | "written by `landing-pass land`", "`spira-landing` runs `$SPIRA_LANDING_PASS_BIN land`" |

The pr timer is unchanged: `systemd/spira-landing-pass.service:8` keeps
`ExecStart=@SPIRA_LANDING_PASS_BIN@ --pass`. Only its text changes:

| # | file:line | current | replacement |
|---|---|---|---|
| 6 | `systemd/spira-landing-pass.service:2` | `Description=Spira landing pass — pr-mode repository landing, no local gate` | `Description=Spira landing pass — pr mode (the gated modes run as landing-pass land under spira-landing)` |
| 7 | `systemd/spira-landing-pass.service:3` | `Documentation=file://@SPIRA_HOME@/pr-pass-branch.sh` | unchanged (the helper still does the per-branch pr work) |

### 7.2 Other callers

| # | file:line | current | replacement |
|---|---|---|---|
| 8 | `spira/canary.sh:160` | `bash "$SPIRA_HOME/landing.sh" 2>&1 \| sed 's/^/  landing: /' \|\| true` | `"$SPIRA_LANDING_PASS_BIN" land 2>&1 \| sed 's/^/  landing: /' \|\| true` |
| 9 | `spira/canary.sh:18`, `spira/stage.sh:27-30,148-154` | comments: "canary.sh runs landing.sh directly" | "canary.sh runs `landing-pass land` directly" |
| 10 | `spira/world.sh:175-176` `live_workers` | `argv_has "$p" "$SPIRA_HOME/gate.sh" "$SPIRA_HOME/landing.sh" "${SPIRA_PROD:-$SPIRA_HOME}/gate.sh" "${SPIRA_PROD:-$SPIRA_HOME}/landing.sh"` | `argv_has "$p" "$SPIRA_HOME/gate.sh" "${SPIRA_PROD:-$SPIRA_HOME}/gate.sh" "$(dirname "${SPIRA_PROD:-$SPIRA_HOME}")/bin/landing-pass" ${SPIRA_LANDING_PASS_BIN:+"$SPIRA_LANDING_PASS_BIN"}` (as the aeon cutover did for `bin/aeon`; note it also matches a 90 s pr pass in flight) |
| 11 | `spira/world.sh:165,598` | comments naming landing.sh | "landing-pass" |
| 12 | `spira/chamber/czar.fayth:90` | `Bash(*landing.sh halt*)` | `Bash(*landing-pass halt*)` |
| 13 | `spira/chamber/czar.md:122` | ``Halt the landing pass with `landing.sh halt` `` | ``Halt the landing pass with `landing-pass halt --reason-file -` `` |
| 14 | `spira/timeout-lint.sh:41` | `files=("$HERE/landing.sh")   # aeon.sh and sentinel.sh are Rust now; …` | the default file set is now empty: **retire** `timeout-lint.sh` and `test-timeout-lint.sh` (no caller, in no gate string), with their lines `spira-lint/fence-scripts-allow:36,40` and `spira/tier-budget-allowlist:409` |
| 15 | `spira/lc-delivery.sh:4,14,106,115,122,125` | header names landing.sh's push mode; actor string `landing.sh` | keep the file (pr-pass-branch.sh sources it); header "pr-pass-branch.sh (landing-pass's pr helper) and landing-pass's push mode"; actor strings may stay `landing.sh` (lifecycle history actor names) or become `landing-pass` — the actor is a label, nothing matches on it |
| 16 | `spira/pr-pass-branch.sh:4`, `spira/land-build-ensure.sh:13`, `spira/confine.sh:13,52`, `spira/gate.sh:41,341,415-416,630`, `spira/gate-lib.sh:39,67`, `spira/lib.sh:774,4784,4837,6682,7442,8267,8843,9200-9202`, `spira/sending.sh:232,594`, `spira/watchtower.sh:343,429`, `spira/conf.sh:1104` | comments / `covers:` naming `landing.sh` | name `landing-pass` (comments only). **One is text a human acts on:** `lib.sh:774` `run \`$SPIRA_HOME/landing.sh\` by hand` → `run \`$SPIRA_LANDING_PASS_BIN land\` by hand` |
| 17 | `spira/config-fence-allow:18` | `landing-pass/src/main.rs` | **delete** (no .rs file in the crate names the config files any more; shrink-only list) |
| 18 | `spira/config-fence-allow:55` | `spira/landing.sh` | **delete** (the file is deleted) |
| 19 | `spira/binary-path-fence-allow:22-26` | the comment block and `landing-pass/src/main.rs` | **delete** (the private bin/-then-target resolver is gone; `SPIRA_LC_BIN` comes from conf.sh) |
| 20 | `spira/test-ops-allowlist.sh:151` | `bash spira/landing.sh halt\|landing.sh halt` | `landing-pass halt\|landing-pass halt` (and :143's comment) |

### 7.3 Delete

21. `spira/landing.sh`.
22. `spira/landing-lib.sh` (only landing.sh and three T1 suites sourced it; its three
    functions are `order.rs` now: `certify_order`, `basefail_fix_decision`,
    `prior_pass_suites`).
23. `~/.config/spira/overrides/landstate-keep-unpublished.override` (+ its `.py`): retired —
    the prune rule (§5) is the structural form. Close sp-bauwt with this landing. (The
    `.override`'s `needed()` greps landing.sh for its marker; with landing.sh gone it
    would try to patch a missing file, so it must go in the same step.)
24. sp-4hs0i closes with this landing (§8 D3).

`spira/pr-pass-branch.sh` and `spira/lc-delivery.sh` stay: the pr pass still delegates each
branch to the helper (kept pr behaviour).

### 7.4 Bash suites to retire or repoint

The decision cases are unit tests now (`cargo test -p landing-pass`, §10). What still needs
an end-to-end run is the wiring: the real lib.sh seam against a real fixture store, the real
gate.sh, the real rebase machinery.

**Invocation**: every `bash "$X/landing.sh"` becomes `"$SPIRA_LANDING_PASS_BIN" land` with
`SPIRA_HOME="$X"` in its environment (the binary sources `$SPIRA_HOME/lib.sh`; the suites that
copy scripts into `$SH` must keep copying lib.sh/conf.sh/gate.sh/confine.sh there and must
build or locate the binary — the testenv container already builds the workspace). Every
`cp … landing.sh landing-lib.sh …` drops those two names.

| suite | line(s) | action |
|---|---|---|
| `test-landing.sh` | 177 | **repoint** (the push-mode end-to-end: keep as the one real-git+real-lib.sh push case) |
| `test-landing-order.sh`, `test-express-cert-order.sh`, `test-landing-starvation.sh` | 79; 63; 89 | **retire** — ordering, express, rotation and deferral are unit tests (`express_branches_…`, `the_walk_starts_at_the_cursor`, `the_budget_cut_…`, `order::tests`) |
| `test-landing-gate-wait.sh` | 40, 88 | **retire** — `budget::tests` |
| `test-landing-base-fail.sh`, `test-landing-basefail-select-t1.sh` | 88; 18 | **repoint** base-fail once (the real incident.sh intake) and **retire** the T1 (`order::tests::basefix_…`) |
| `test-landing-cert-order-t1.sh`, `test-landing-prior-pass-t1.sh` | 14; 16 | **retire** (source landing-lib.sh; `order::tests`) |
| `test-cert-gate-reopen-note.sh` | 48 | **retire** (greps landing.sh's text for `tail -N`; `a_red_gate_reopens_with_the_gates_own_words…` asserts the 20-line window) |
| `test-landing-mode-map.sh` | 165, 171, 260, 279, 307 | **retire** (static greps over landing.sh's `land_repo` body and `# land-modes:`; the modes are `LandMode` and the walk's dispatch) |
| `test-queue-flush.sh` | 80 | drop the landing.sh grep (the queue step call is `Tools::queue_step`, tested by `the_queue_step_runs_before_and_after…`) |
| `test-landing-queue-early.sh` | 83, 112-115 | **repoint** the positive control to a `SPIRA_LANDING_PASS_BIN` stub that exits 255; the "early" assertion now expects one `queue early:` block, not two (§8 D5) |
| `test-landing-halt.sh` | 40, 227, 283 | **repoint** to `"$SPIRA_LANDING_PASS_BIN" halt …`; the pass under test is `landing-pass land` (its pid is the binary's) |
| `test-landing-race.sh`, `test-landing-rebase.sh`, `test-landing-red-recurring.sh`, `test-superseded.sh`, `test-spike.sh`, `test-certify.sh`, `test-landing-cutover-round.sh`, `test-closed-strand.sh`, `test-submitted-lands.sh`, `test-config-compat-master-base.sh`, `test-land-mode-local.sh`, `test-skew-refresh.sh`, `test-landing-pr.sh` | 56/85; 95; 72; 43/61; 223/233; 100; 77; 42/89; 109; 239/258; 132/184; 276/292; 309 | **repoint** the invocation (above). `test-landing-rebase.sh` expects survivor lines worded "after this pass's landings" and one sweep per pass (§8 D3) |
| `test-landing-pass.sh` | 4, 11, 101 | **repoint** :101 to `landing-pass land`; property 1 ("landing.sh skips gate.sh for pr repos") is now `a_pr_repository_is_counted_and_left_to_the_pr_pass` — keep the end-to-end half |
| `test-check5-invariant.sh`, `test-poison.sh`, `test-sentinel-check5-subsumed.sh` | 68; 178; 52 | drop `landing.sh` from the `cp` list (they never run it) |
| `test-lifecycle-delivery-cutover.sh` | 4, 27, 50 | drop `landing-lib.sh` from the `cp`; its push-mode legs run `landing-pass land` with `SPIRA_LIFECYCLE_ENFORCE=1` |
| `test-canary.sh` | 53 | drop the "landing.sh symlink" check; add `exists "landing-pass" "$SPIRA_LANDING_PASS_BIN"` |
| `test-world.sh` | 179-185 | the fake worker is a script named `…/bin/landing-pass` (what `live_workers` matches now) |
| `test-timeout-lint.sh` | — | **retire** with timeout-lint.sh (#14) |
| `test-ops-allowlist.sh` | 143, 151 | as #20 |

## 8. Decisions and deliberate changes

- **D1 — one resolver.** Repository rows and settings come from conf.sh/lib.sh through the
  context seam (S1), for the pr pass too. The pr pass's private map parser and its
  `bin/`-then-`target/` spira-lc resolver are gone (so are their fence allow-list entries,
  §7 #17, #19). The base is `spira_landref` — the same three-step algorithm the Rust copy
  had. Two messages that named the map file are reworded ("…in the repository map"),
  because no .rs file may name it (config-fence).
- **D2 — pr pass fixes, found while unifying it with the gated pass.**
  (a) Its bead scan read raw `status == closed`, so a builder's submitted-labelled open bead
  never reached `pr-pass-branch.sh`; the shared scan reads it as done, as landing.sh and
  `bead_land_status` always did. (b) Its repository check let any bead whose `repo:` was the
  home repository into any pr repository; now the bead's repository must resolve to this
  checkout's path, as landing.sh required. (c) Its `content_landed` said an *ancestor* branch
  was not landed (it tested `ahead == 0` first); it now uses lib.sh's rule (ancestor →
  landed → CONTENT). (d) A delivery row's `version` may be a number (lc-delivery.sh already
  accepted one). (e) Each line was printed to stdout *and* appended to landing-pass.log,
  which the unit also appends stdout to — every line appeared twice; it now prints once.
- **D3 — survivors once per pass (sp-4hs0i).** `rebase_survivors` ran after every push
  landing over the whole judged set (k landings × n survivors). It runs once per repository
  per pass, after the walk, only if something landed there. Safe because every survivor is
  re-checked individually (gone, holder, content) before its replay, and branches walked
  after the last landing already sit on the latest base (the ancestry skip). Log wording:
  "…after this pass's landings".
- **D4 — the prune (§5, sp-bauwt)**: never deletes a LANDED record whose tip is not on the
  forge; absorbs the `landstate-keep-unpublished` override, generalised from the home
  repository's `origin/main` to each repository's forge ref.
- **D5 — the queue step runs twice, not three times.** landing.sh called `queue.sh step`
  before certification twice in a row ("early", then the sp-len2q "verdict first" hotfix —
  the same call), and once after. Now: once before (`queue early:`), once after
  (`queue late:`). The step is `$SPIRA_QUEUE_BIN step <repo>`; a missing binary is logged
  per repository, never a fallback to a deleted queue.sh.
- **D6 — base-fix certification once.** landing.sh's push/hold BASE_FAIL arm carried the
  certification block twice, the first copy certifying on the scan's (stale) status. One
  copy remains, after a re-read.
- **D7 — `landing.lock`.** The unit name is still the systemd mutex; a hand-run
  `landing-pass land` beside the unit now declines instead of racing it.
- **D8 — SIGTERM is immediate.** bash deferred its TERM trap until the foreground gate
  returned (halt's 30 s grace, then SIGKILL, which skipped the status write). The binary
  forwards TERM to the child's process group (gate.sh and its suites), writes the status
  (rc 143) and exits. Every child runs in its own process group with the default mask.
- **D9 — push-mode conflict path fetches the base's own remote** (bash hard-coded `origin`).
- **D10 — records written atomically**: `submitted/<id>` (was a plain `>`), `landing.run`,
  `landing.status`, `landing.cursor`, `landing.deferred/*`.
- **D11 — halt**: `--reason-file F|-` added (payload on stdin); a multi-line reason is
  flattened to one line in `landing.interrupted` (a newline broke its key=value format).
- **D12 — gate.sh / confine.sh are subprocesses with their argv contract**; the queue-mode
  certification gate is the repository's own gate string (fences + sp-vq2za's budgeted
  selector + testenv) run by gate.sh — this pass never picks suites. `confine.sh` still gets
  the bead's labels as its fifth argument (its interface; bounded by the bead's label set).
- **D13 — `lifecycle_enforce` (§9).**
- **Kept deliberately:** every per-branch message text and `CHECK6` prefix; one judgement
  per tip (CERTIFIED/WITHDRAWN/RED@base skips); re-read before acting; only FAIL reopens;
  the budget reserve and its base-fix exemption; queue modes never rebase here
  (law-a-round-takes-certified-tips); `skew.sh refresh` for push and queue(forge) only;
  the pr pass still writes its CONTENT record directly (no lib.sh on a 90 s timer) and still
  delegates each branch to pr-pass-branch.sh.
- **Rejected:** porting `rebase_branch`/`recut_onto`/`bead_reopen`/the asks (shared lib.sh
  machinery with other callers — seams S3-S16); spira-claim for the rebase-escalation
  counter (it deliberately excludes rebase returns; this counter *is* rebase returns);
  reading config directly (D1); a fallback to queue.sh (deleted on batch-rust).

## 9. Lifecycle switch

**Finding (operator, 2026-09-29):** the lifecycle machine is not deployed on this host (no
`spira_lifecycle` database, no `spira_lc` grant, no service or socket). **Decision:**
`lifecycle_enforce` is THE switch for everything that touches it.

**Resolution:** `spira_config::lifecycle_enforce(<the document conf.sh resolved>)` — the
one rule the crates share (27000cbf9): the process environment's `SPIRA_LIFECYCLE_ENFORCE`
wins (`1`/`true` on, anything else off), else the typed `spira.lifecycle_enforce`, else
**off**. Binary presence is never an input. The document is conf.sh's `SPIRA_TOML_FILE`,
carried in the context answer.

**OFF (production today):** landing-pass never invokes spira-lc, not even a probe. It also
pins `SPIRA_LIFECYCLE_ENFORCE=0` and `SPIRA_LC_BIN=/nonexistent/spira-lc-disabled-by-lifecycle_enforce=0`
(the sentinel's own `LC_DISABLED`) into its environment before any child starts, so the
bash it still runs — pr-pass-branch.sh's `lc_deliver_pr_merged/closed`, the lib.sh seams —
cannot reach spira-lc either (lc.sh/lc-delivery.sh treat a non-executable path as absent).
Behaviour is the pre-lifecycle contract: landstate and labels only.

| path | OFF | ON |
|---|---|---|
| pr: content already on base | the `CONTENT` landstate record, nothing else (f031f6dee's OFF semantics, kept) | the same, plus `Delivered` when the delivery row is `PR_OPEN` — best-effort additive; no row / not PR_OPEN is quiet; a machine that cannot be asked or refuses is a loud `landing-pass: <id>: LIFECYCLE: …` line on stderr and the pass goes on (f031f6dee's ON semantics) |
| pr: helper exits 7/8 | helper's lc calls hold a dead `SPIRA_LC_BIN`: no-ops | helper records Delivered/Returned as before |
| push: landed / lost the race / conflict | no `lc_deliver_push_*` call at all | `lc_deliver_push_delivered / _requeued / _returned`; spira-lc probed once per pass (`list --state IN_DELIVERY`) before the first push landing — **unreachable refuses the landing loudly** (`landing: lifecycle_enforce is on and spira-lc is unreachable (<why>) — not landing <br> this pass; …`), because the landing would be a delivery the authoritative machine never saw |
| queue / queue.local certification, hold, prune, halt | never touches spira-lc | never touches spira-lc (gate.sh's own certification event is gate.sh's) |

This rewrite **supersedes** f031f6dee's code path (its `main.rs` is replaced), and keeps its
OFF semantics exactly and its ON semantics for the pr content proof.

## 10. Tests

`cargo test -p landing-pass` — 56 unit tests; `cargo build --workspace` clean. Derived from
§2-§5 and §9, with recording fakes of bd, git, the lib.sh seam, the harness programs,
liveness, the clock, spira-lc and the halt ports (signals, podman, testenv teardown):

| contract | tests (`src/tests.rs` unless named) |
|---|---|
| queue certification: green → GATING, CERTIFIED, submitted, one movement | `a_green_gate_certifies_once_and_the_movement_crosses_the_seam` |
| one judgement per tip | `certified_or_withdrawn_at_the_same_tip_is_not_gated_again` |
| FAIL reopens with the gate's words; re-read | `a_red_gate_reopens_with_the_gates_own_words_and_marks_red`, `a_bead_reopened_while_its_gate_ran_is_not_reopened_or_certified` |
| BASE_FAIL: one incident per repo per pass; green base-fix certified first, budget-exempt | `a_red_base_files_one_incident_…`, `a_base_fix_green_on_its_suite_…` |
| NO_VERDICT; PASS clears counters | `no_verdict_is_counted_by_the_seam_…`, `a_pass_clears_the_branchs_noverdict_counters` |
| budget cut, cursor, deferral escalation, rotation | `the_budget_cut_defers_the_rest_…`, `the_walk_starts_at_the_cursor`, `budget::tests` |
| every early exit says why; EJECTED; gone mid-pass | `every_early_exit_says_why`, `an_ejected_record_on_a_closed_bead_is_reopened`, `a_branch_gone_mid_pass_…` |
| queue step before/after, missing binary, skew only push/queue | `the_queue_step_runs_before_and_after_…` |
| order, express, pr repos left alone | `express_branches_are_announced_…`, `a_pr_repository_is_counted_…`, `order::tests` |
| prune (§5) | `the_prune_never_deletes_a_landed_record_the_forge_does_not_have`, `an_unreadable_store_prunes_nothing`, `prune::tests` |
| push: LANDED first, close, survivors once (sp-4hs0i) | `push_lands_records_first_and_closes_and_rebases_survivors_once_per_pass` |
| push: conflict/escalation/refused/confine/blocked push/already-landed | `a_real_conflict_reopens_once_…`, `escalation_at_the_threshold_…`, `a_rebase_that_could_not_be_attempted_…`, `confinement_is_asked_before_the_gate`, `a_push_blocked_…`, `a_merge_conflict_on_already_landed_work_…` |
| push against real repositories | `push_mode_lands_on_a_real_remote` |
| hold | `hold_gates_notes_and_does_not_advance_the_base` |
| halt / sweep-red | `halt_dry_run_…`, `halt_terms_then_kills_…`, `halt_refuses_…`, `sweep_red_lists_red_records_only`, `cli::tests` |
| pr pass | `the_pr_pass_hands_done_branches_to_the_helper_…`, `delivery_rows_accept_a_numeric_version` |
| lifecycle OFF/ON (§9) | `off_push_mode_never_invokes_spira_lc`, `off_queue_certification_never_invokes_spira_lc`, `on_push_mode_records_deliveries_…`, `on_with_the_machine_unreachable_a_push_landing_is_refused_loudly`, `on_the_pr_pass_proves_content_deliveries_and_is_loud_…`; the OFF pr case asserts no delivery call in `the_pr_pass_hands_done_branches_…` |
| the seam mechanism, for real through bash | `seam::tests` (values on stdin intact; progress vs log vs answer; the context script against a stand-in lib.sh; a missing lib.sh is 96) |
| records and parsing | `records_keep_their_shell_formats`, `the_context_answer_parses_…`, `util::tests` |

Also run by hand (not a suite, not production): `landing-pass land` against a stand-in
`SPIRA_HOME` (stub lib.sh/gate.sh/bdsim.py, a temp git repository) — certified, then skipped
at the same tip, then SIGTERM mid-gate → `SP_LAND_RC=143`, the gate's own child gone.
