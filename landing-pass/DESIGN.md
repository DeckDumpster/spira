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
| `landing.lock` (new) | — | `land` flock (§8 D9) |
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
| `spira/sentinel.sh:1311-1325` CHECK 6 | `systemd-run --user --collect --unit=spira-landing … "$SPIRA_HOME/landing.sh"` | unit name as mutex; `landing.status`, `landing.progress`; stdout → `landing.log` |
| `systemd/spira-landing-pass.service:8` | `@SPIRA_LANDING_PASS_BIN@ --pass` (timer every 90 s) | pr pass |
| `spira/canary.sh:160` | `bash "$SPIRA_HOME/landing.sh"` | a real pass; `landing.status` |
| `spira/world.sh:174-175` `live_workers` | argv match on `landing.sh` | halt completeness |
| `spira/chamber/czar.fayth:90`, `czar.md:122` | `landing.sh halt` | the verb |
| ~45 bash suites (§7.5) | run the pass, source landing-lib.sh, grep landing.sh | — |

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

**Not performed** (operator's directive). Line numbers are against this branch's base
(`4764d03ec`); production's checkout additionally carries the `landstate-keep-unpublished`
hunk in landing.sh. Apply **after** the queue crate's cutover (this pass calls
`$SPIRA_QUEUE_BIN step`), in one landing, in this order.

See §7.1–§7.6 below.

(§7 is filled in by the implementation commit, from the final code.)

## 8. Decisions and deliberate changes

(Filled in by the implementation commit.)
