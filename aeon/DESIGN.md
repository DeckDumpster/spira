# aeon — the runner that claims one bead, works it, and accounts for it

Replaces `spira/aeon.sh` (2,839 lines). Binary `aeon`, invoked by the harness exactly where
`aeon.sh` was: as the ExecStart of a transient user unit `spira-aeon-<fayth>-<ts>.service`
(`summon_fayth`, `escape.sh`) and of `spira-ops.service` (sweep mode).

This document was written before the implementation (operator's directive: intent,
contract, schema first). The port is **not** line by line: the bash grew by scar over two
weeks and its control flow is mostly `trap` and globals. The Rust keeps the interface —
argv, exit codes, the stdout/stderr lines, the ledger, the files and bead rows — and
replaces the machinery.

## 1. Intent

One aeon is one short-lived worker. It is stateless between runs; everything it knows is in
the bead store, the lifecycle machine, git and a handful of files under `$SPIRA_RUN`. A run:

1. **Decides whether to work at all** — capacity, `world.halted`, `world.draining`, the
   account's capacity pause. Declining is a healthy outcome (exit 0, `awake <f> <reason>`).
2. **Selects and claims one bead** — `spira-claim` ranks the ready set epic-first; the
   atomic `bd update --claim` claims; the lifecycle machine (CAS Claim) authorises when
   `lifecycle_enforce` is on.
3. **Builds one workspace** — one bead, one worktree, under the sanctioned root
   `$SPIRA_RUN/worktree/<bead>`; never adopts a tree another bead holds; never deletes a
   tree (moves it aside).
4. **Renders the brief** — the persona's chamber template with `{{…}}` placeholders, the
   operator's overlays, and session-specific blocks; how the session finishes (`bd close`
   vs `work submit`) follows `lifecycle_enforce` and nothing else.
5. **Runs the model session** under a liveness lease (trace growth renews it) and a
   deliverable-progress wall (thrash), streaming its trace to `$SPIRA_RUN/<bead>.log`.
6. **Judges the outcome** — closed is not landed: the verdict fences (commit naming the
   bead, delivers evidence, eviction race, own-worktree dirty, SOP closing rule, close
   reason, groom escalations, workflow run, rebase currency), then the teardown disposition,
   which charges an attempt **only** for a named failure of the work (default-deny).
7. **Accounts** — ledger lines `born`/`awake`/`done` that the cockpit, watchtower and the
   measure scripts parse; event rows (`requeued`, `lapsed`, `reopen`) that `spira-claim`
   folds into attempts and poison.

Sweep mode (`--sweep`) runs a persona without a bead: capacity/draining/paused checks, the
ledger, the session — no claim, no worktree, no verdict.

### What it deliberately is not

- **Not the ranker.** Claim order is `spira-claim select` (epic-first rank, the ready set on
  **stdin**, never argv — the 2026-09-28 claim outage was argv E2BIG, sp-o4trx). This
  program only checks resumability (git) for the top tier, which `spira-claim` by design
  leaves to its caller.
- **Not the poison decision.** Attempts and poison are `spira-claim`'s fold over the event
  rows this program writes. The one count the aeon itself needs (prior eviction-race
  reopens) is read from `spira-claim requeues --json`, not from hand-written SQL.
- **Brief rendering is a separate module from claim selection** (`brief.rs` vs
  `claim.rs`): sp-f0qhr was ejected twice for mixing them. Nothing in `claim.rs` knows
  about chambers, `{{FINISH}}` or `lifecycle_enforce`.
- **Not lib.sh.** lib.sh stays. Broad shared machinery is called through one documented
  subprocess seam (§5); aeon-specific decisions and all rendering are Rust.

## 2. Contract

### 2.1 Invocation

```
aeon [--home DIR] <fayth>                       claim one bead and work it
aeon [--home DIR] <fayth> --dry-run             print candidates, claim nothing, write no ledger
aeon [--home DIR] <fayth> --sweep [--prompt <text> | --prompt - | -]
                                                 beadless session; `-` reads the prompt on stdin
```

`--home DIR` is the harness's `spira/` directory (where `lib.sh`, `chamber/`, `world.sh`,
`gate-run.sh`… live). New, and the only interface addition: `aeon.sh` found it from its
own location. Resolution when absent: `$SPIRA_HOME`, else `<harness>/spira` from
`~/.config/spira/harness`, else `<exe-dir>/../spira`, else `<exe-dir>/../../spira`
(a `target/<profile>/` build). The directory must contain `lib.sh` and `chamber/`.

Usage errors (no fayth): `FATAL usage: aeon.sh <fayth> [--dry-run | --sweep [--prompt <text>|-]]`
on stderr, exit 1 (wording kept: callers grep `FATAL`).

### 2.2 Exit codes

| exit | when |
|---|---|
| 0 | declined (capacity, halted, draining, paused, idle); dry run; poison raced; world-stop fence refused; lifecycle claim refused/unreachable; **bead closed or converted to submitted**; sweep that ran (outcome `unlanded`/`killed`) |
| 1 | usage; no such fayth; unfenced predicate; claim-error (ready query / epic lookup / rank failed); `lifecycle_enforce` set with a binary missing; unmapped repo; unresolvable base; worktree could not be made; pre-session death (a `rc=0` there is coerced to 1) |
| `SESSION_RC` | session ran and the bead was left open (unlanded / not-judged / yield-headless / submitted-but-open paths exit the session's own code, e.g. 1 or 124) |
| `rc` (see below) | the disposition branches capacity, slain, thrash, lapsed, gate-unfinished, decision-blocked, timeout, requeue, operator-wait, submitted, pre-session |

`rc` is what bash's EXIT trap saw: 128+signal when the aeon was signalled (TERM from
`systemctl stop`/`slay.sh`, or the heartbeat's own lease-lapse/thrash kill → 143); 1 when
setup died (`die`); otherwise 0 (the verdict block ran to its end). `SESSION_RC` is the
session's exit code, recorded **only** when the session returned without the aeon being
interrupted (bash's trap ran before `SESSION_RC=$rc` in that case, so it stayed 0).

Sweep: exit 0 when the session outcome is `unlanded` or `killed`, else the session's rc.

### 2.3 Standard output / error

Every progress line is `lib.sh`'s `log` format on **stdout**:
`YYYY-MM-DDTHH:MM:SSZ spira: <message>`. Fatal lines go to **stderr** as
`... spira: FATAL <message>`. Message texts are kept verbatim from aeon.sh — tests and
humans grep them (e.g. `resuming <id>`, `trying the next ranked candidate`,
`taking a fresh branch instead of dying`, `REOPENED`, `POISONED`, `claim-error`). The two
closing-rule lines are printed with `spira: <fayth>: <id> …` exactly as aeon.sh's
`printf` did. `--dry-run` prints `bd ready` text (filtered of bd hint lines, first 10) on
stdout.

### 2.4 The ledger — `$SPIRA_RUN/aeon-ledger.log` (pinned)

Append-only, one line per write, `O_APPEND`, no lock. Timestamp is UTC second precision.

```
<ts> born <fayth> <pid>
<ts> awake <fayth> <bead-id | idle | capacity | halted | draining | paused | sweep>
<ts> awake <fayth> claim-error <bd's first stderr line | epic_parent_lookup failed rc=N | epic_rank_rows failed rc=N>
<ts> done <fayth> <bead-id|sweep> rc=<n> status=<status> wall_s=<n|?> api_s=<n|?> turns=<n|?> in_tok=<n|?> cache_read_tok=<n|?> out_tok=<n|?> think_tok=<n|?> cost_usd=<x.xxxx|?>
```

- `born` is written right after the fence, before any check that takes time; `awake` once
  the claim resolved; `done` is the disposition. Born-without-awake is a stillbirth
  (cockpit-metrics.py `LEDGER_RE = ^(\S+) (born|awake|done) (\S+)(?: (.*))?$`).
- A dry run writes nothing.
- The file is trimmed to its last 5,000 lines when it exceeds 20,000 (before `born`).
- The eight session fields are parsed from the **last attempt segment** of the trace
  (`attempt_trace`: from the last `=== spira attempt` mark), summing per-turn fields
  (`duration_ms`, `num_turns`, `usage.*`) over every `result` record and taking the last
  value of cumulative ones (`duration_api_ms`, `total_cost_usd`); integers use Python's
  round-half-even of `v/1000` or `v`; cost `%.4f`. A missing field is `?`, never 0.
- `status` values written: `poison-raced`, `lifecycle-enforce-binary-missing`,
  `lc-claim-refused`, `lc-claim-unreachable`, `world-stop-fence`, `unmapped-repo`,
  `capacity`, `slain`, `requeue-thrash`, `requeue-thrash-charged`, `lapsed`,
  `gate-unfinished`, `decision-blocked`, `timeout`, `requeue-<cause>`, `operator-wait`,
  `submitted`, `yield-headless`, `pre-session`, `closed`, `open`, `in_progress`, `?`,
  `sweep`.
- Readers: `cockpit-metrics.py`, `cockpit.sh` (tail), `watchtower` (`$2=="awake" && $3==f`),
  `model-switch-report.sh`, `aeon::run::Run::rapid_recur_check` (`" done \S+ <id> "`,
  native since sp-8kqww — wave 4.33; lib.sh's own copy is retired),
  `full-aeon-fixture.sh`, and many suites. `lib.sh capacity_pause_set` also appends a
  `CAPACITY paused until …` line to the same file (it is called through the seam).

After each `done` line: `_tsd_aeon_session` (native since sp-27d3d — wave 4.34, tsd family
`aeon-session`; lib.sh's own copy is retired) and `rapid_recur_check` run (native,
`aeon::run::Run::rapid_recur_check`).

### 2.5 Files read and written

| path | r/w | what |
|---|---|---|
| `$SPIRA_HOME/chamber/<fayth>.fayth` | r | persona config (sourced by the seam, §5) |
| `$SPIRA_HOME/chamber/<fayth>.md` | r | brief template |
| `$SPIRA_CHAMBER_OVERLAY/<fayth>.md`, `<fayth>.<Section_Name>.md`, `<fayth>.append.md`, `blocks/{PARK,FIXTURE,DEADLINE,FINISH}.md` | r | operator overlays |
| `$SPIRA_RUN/aeon-ledger.log` | a | §2.4 |
| `$SPIRA_RUN/aeon-<fayth>-<bead>.pid`, `.name` | w/rm | this aeon's pid and name; removed **last** in teardown (strand reads both witnesses) |
| `$SPIRA_RUN/aeon-<fayth>-sweep-<pid>.pid`, `.name` | w/rm | sweep identity |
| `$SPIRA_RUN/aeon-*.pid` | r/rm | world-stop fence: peers; a pidfile whose pid has no `/proc/<pid>` is removed |
| `$SPIRA_RUN/<bead>.log` | a | the session trace; one `=== spira attempt N aeon=<name> at=<ts> kept=<bytes>` mark per attempt |
| `$SPIRA_RUN/sweep-<fayth>-<pid>.log`, `.system.md`, `.task.md` | w | sweep trace and prompt |
| `$SPIRA_RUN/<bead>.system.md`, `<bead>.task.md` | w | the split prompt |
| `$SPIRA_RUN/aeon/<bead>.lease` | w/rm | lease deadline epoch, written beside + renamed; removed at teardown |
| `$SPIRA_RUN/<bead>.lapsed` | w/r/rm | `<quiet_s>\t<last>` written by the heartbeat, consumed by teardown |
| `$SPIRA_RUN/<bead>.thrash` | w/r/rm | last action, written by the heartbeat, consumed by teardown |
| `$SPIRA_RUN/<bead>.slain` | r | written by slay.sh |
| `$SPIRA_RUN/<bead>.operator-wait` | r/rm | written by mail; honoured only if it holds this session's `SESSION_EPOCH` |
| `$SPIRA_RUN/worktree/<bead>` | w | the worktree (the sanctioned root) |
| `$SPIRA_RUN/aeon-empty-gh/` | w | empty `GH_CONFIG_DIR` for the session |
| `$SPIRA_MAIL/aeon-<bead>/{new,cur,tmp}` | w/rm | per-claim mailbox |
| `$LANDSTATE/<bead>.evict-seen` | r/w | eviction-race idempotence sidecar |
| `$SPIRA_RUN/world.halted`, `world.draining` | r | world gates |
| `$SPIRA_RUN/groom.log` | r | groom escalation check window |
| `<worktree gitdir>/spira-dirty-before` | w | pre-session dirty snapshot for the pre-commit hook |
| `$SPIRA_RUN/.epic-lookup.<pid>`, `.resumable.<pid>` | w/rm | spira-claim inputs (files, never argv) |

### 2.6 Bead-store rows (through `bd -C $SPIRA_DB`, bdq's rules: `SPIRA_DB` must be set,
`timeout ${BD_TIMEOUT:-180}`, one retry on `invalid connection`, `SPIRA_BDJSON_FIXTURE` →
`bdsim.py`, `--json` output passed through `json_only`)

| call | when |
|---|---|
| `ready <READY_ARGS> --label <FAYTH_LABELS> --exclude-label <CLAIM_EXCLUDE> --json` | selection (retried `SPIRA_CLAIM_RETRIES`×, delay `SPIRA_CLAIM_RETRY_DELAY_S`) |
| `update <id> --claim --json` | one per ranked candidate, until one returns a row |
| `show <id> --json` | poison race, world-stop label, teardown status, verdict, close reason, work-type |
| `show <id>` (text) | the brief's bead body |
| `state <id> branch` / `set-state <id> branch=<b>` | branch affinity |
| `note <id> <text>` | every disposition and fence note (texts verbatim from aeon.sh) |
| `label add <id> <label>` | `spira-submitted` conversion, `spira-poison`, ask label |
| `heartbeat <id>` | every heartbeat beat |
| `memories --json` | statutes (cached per `SPIRA_MEMORIES_CACHE`) |
| `list --type decision --label <ask> --json` | groom escalation check |

Event rows (`requeued`, `lapsed`, `reopen`), releases, reopens, holds and landstate go
through lib.sh functions (§5), never hand-written SQL.

### 2.7 Programs called

`bd` (`$SPIRA_BD`), `git`, `spira-claim`, the model (`$SPIRA_AGENT`, default `claude`),
`world.sh stop|start`, `gate-run.sh --status`,
`worktree-hooks.sh install`, `holds.sh`, `wiki-commit.sh`, `sop ledger-init|digest|log`,
`close-reason-flags.py`, `workflow-run-check.py`, the repository's own testdb library, and
`bash` for the lib.sh seam. Every Spira tool and script is invoked by its bare name on the
PATH the launcher set (sp-gypjk: the release's `bin/` and `spira/` first); there is no
`SPIRA_*_BIN` override, no `$SPIRA_ARTIFACTS` or `$SPIRA_REPO/bin` lookup, and no "binary
missing" refusal — a missing tool fails its spawn, naming itself. The briefs name tools the
same way (`{{TESTENV}}` is `testenv`, `{{SOP}}` is `sop`, …). Only the Claude Code hooks
(`spira/hooks/…`, not on PATH) are still addressed under `SPIRA_HOME`.

**The aeon carries its launcher's PATH (sp-31gtu).** Its launcher — the summon's
`systemd-run` (`summon_argv`), the ops unit — sets PATH outright from `SPIRA_RELEASE`
(`$SPIRA_RELEASE/bin:$SPIRA_RELEASE/spira:` then the system directories); the aeon hands it
to the session and its subagents unchanged, and conf.sh (through the seam) appends only the
box's tail. The worktree's git hooks are armed by `worktree-hooks.sh` with
`SPIRA_HOME=<--home>` — the release's `spira/`, which is what every launcher passes as
`--home` — so its pre-commit runs the release's hook by absolute path,
which runs `spira-lint` by name: a fresh worktree commits with no build of its own.

### 2.8 Callers (every one found)

| caller | line | how |
|---|---|---|
| `spira/lib.sh` `summon_fayth` | 2726-2730 | `systemd-run --user --collect --quiet --unit=spira-aeon-$f-<ts> <summon_argv> [--setenv=SPIRA_REQUIRE_LABEL=…] "$SPIRA_HOME/aeon.sh" "$f"` |
| `aeon --escape <fayth>` (this binary, `escape.rs`; was `spira/escape.sh`) | — | same shape, unit `spira-aeon-$FAYTH-escape-<ts>`, `[--dry-run]` |
| `systemd/spira-ops.service` | 46 | `exec @SPIRA_PROD@/aeon.sh ops --sweep --prompt - < @SPIRA_RUN@/ops-sweep-prompt.txt` |
| `spira/lib.sh` `aeon_alive` | 822 | identifies a live aeon by `aeon.sh` in `/proc/<pid>/cmdline` |
| `install.sh` `_conflict_aeon` | 246 | refuses install while a process cmdline is `<home>/aeon.sh` |
| `spira/full-aeon-fixture.sh` | 61, 80 | test fixture runs `$HERE/aeon.sh` |
| 35 suites | §7 | run or grep aeon.sh |

No `.github/workflows` file and no `round.sh` line invokes aeon.sh.

### 2.9 Environment

Read from the unit's environment: `SPIRA_REQUIRE_LABEL` (folded into `FAYTH_LABELS`),
`AEON_OWN_UNIT` (test override of `/proc/self/cgroup`), `SPIRA_LIFECYCLE_ENFORCE`
(environment wins, as conf.sh), and every key conf.sh reads. The **child** environment
(session, hooks, scripts) is conf.sh's exported environment (snapshot, §5) plus, exactly as
aeon.sh exported them: `SPIRA_AEON`, `BEADS_ACTOR=aeon-<name>`, `GIT_{AUTHOR,COMMITTER}_{NAME,EMAIL}`,
`SPIRA_INCIDENT_REPO`, `BEAD_ID`, `SPIRA_MAIL`, `SPIRA_MAIL_FROM=<Fayth> <<fayth>@spira>`,
`SPIRA_WORK`, `SPIRA_FAYTH`, `SPIRA_CZAR_CLASS`/`SPIRA_CZAR_TRIGGER_BEAD` (czar),
`TESTDB_*` (own fixture only; inherited ones are unset), `SESSION_EPOCH`,
`GH_CONFIG_DIR=$SPIRA_RUN/aeon-empty-gh`, `GIT_SSH_COMMAND=<refusal>`,
`GIT_TERMINAL_PROMPT=0`, `GIT_ASKPASS=/bin/false`; `GH_TOKEN`/`GITHUB_TOKEN` removed.

### 2.10 Guarantees (each is a unit test)

1. The ledger formats of §2.4, byte for byte; a dry run never writes one.
2. `lifecycle_enforce` alone selects the restricted path, the lifecycle CAS claim and the
   `{{FINISH}}` text; binary presence never does; enforce with a missing binary refuses
   (exit 1) before any session setup (sp-74gzo, sp-wmcvb).
3. The ready set reaches `spira-claim` on stdin; the epic lookup and the resumable set in
   files; no bead payload is ever in argv.
4. A rank or lookup failure is `claim-error` (exit 1), never `idle`.
5. Worktrees only under `$SPIRA_RUN/worktree/<bead>`; a branch held by **another bead's**
   canonical worktree is never adopted — the bead's `branch:` is reset to `spira/<bead>`
   and a fresh branch is cut; a branch held by a **non-canonical** path is moved aside
   (`<path>.prior`, never deleted, HEAD detached) and re-attached; a tree at the canonical
   path that belongs to another repository is moved aside (`<path>.<repo-dir>`).
6. Charging is default-deny: the precedence table of §4.3, identical to `aeon_disposition`.
7. The pidfile is removed after every bead operation in teardown.
8. Teardown runs once, on every exit path after the claim (normal end, `die`, signal,
   lease lapse, thrash).
9. A world stopped for a bead is started again on every exit path.

## 3. Schema (Rust types)

Every type below is in the crate as written here (module in brackets).

```rust
// [run] how the binary was invoked, and why the armed region stopped early
enum Mode { Claim, DryRun, Sweep { prompt: Option<String> } }
enum Abort { Die(String) /* FATAL, rc 1 */, Exit(i32), Signal(i32) /* rc 128+sig */ }

// [seam] conf.sh's resolution, captured once through `_aeon_snapshot`
struct Snapshot {
    env: BTreeMap<String, String>,   // conf.sh's EXPORTED environment (the children's base)
    vars: BTreeMap<String, String>,  // SNAPSHOT_VARS: set shell variables, exported or not
    ready_args: Vec<String>,         // READY_ARGS — the sentinel's own query, never copied
    claim_exclude: String,           // fayth_exclude <fayth> <FAYTH_EXCLUDE_LABELS>
}

// [conf] the persona (FAYTH_* after sourcing the .fayth) and a typed view of conf.sh
struct Fayth { name, labels, exclude_labels: String, max_concurrent: u32, elastic: bool,
    lease_minutes: Option<u32> /* 10 */, heartbeat_seconds: u64 /* 30 */,
    timeout_seconds: Option<u64>, memory_prefixes /* "law-" */, statute_core,
    tools /* "Bash,Read,Edit,Write,Glob,Grep" */, project_instructions: String,
    system_prompt: SystemPrompt /* Append | Replace */, sop_required: bool,
    groom_escalation_check: bool, graph_only: bool }
struct Conf { v: BTreeMap<String, String>, home: PathBuf, run: PathBuf }

// [bd] a bd row (show / ready / update --claim); unknown fields ignored
#[derive(Deserialize)] struct BeadRow { id: String, status: Option<String>,
    labels: Option<Vec<String>>, issue_type: Option<String>, dependencies: Option<Vec<Dep>>,
    close_reason: Option<String>, created_by: Option<String> }
#[derive(Deserialize)] struct Dep { dependency_type: Option<String>,
    #[serde(rename = "type")] typ: Option<String> }   // show and list spell it differently

// [claim]
struct TierLine { id, branch, repo, eprio, estarted, bprio: String } // "id|branch|repo|…"
enum Selection { Ranked { ids: Vec<String>, resumable: Vec<String>, tier: Option<String> },
                 ClaimError { log: String, ledger: String } }
struct Claimed { id: String, repo: String, row: BeadRow, raw: String }

// [decide] the pure decisions
enum WorldStop { None, Refuse, Stop }
enum HbTick { Ok, Renew, Lapse, Thrash }
struct DispositionIn { status: String, capacity, slain, thrash, thrash_charged, lapsed,
    gate_unfinished, decision_blocked: bool, session_rc: i32, committed: bool,
    requeue_cause: Option<String>, operator_wait, yield_headless, session_started: bool,
    outcome: Option<String>, submitted: bool }
struct Disposition { ledger_status: String, charge: bool, requeue_cause: Option<String>,
                     note: NoteKey }
enum NoteKey { Capacity, Slain, ThrashCharged, Thrash, Lapsed, GateUnfinished,
    DecisionBlocked, Timeout, Requeue, OperatorWait, Submitted, YieldHeadless, PreSession,
    Unlanded, NotJudged }
enum Eviction { None, Stale, Cap, Reopen }
enum SopVerdict { Satisfied, Decline, Poison }
struct CloseVerdict { outcome: String, reason: String, msg: String } // "outcome|reason|msg"

// [ledger]
struct SessionFields { wall_s, api_s, turns, in_tok, cache_read_tok, out_tok,
                       think_tok: Option<i64>, cost_usd: Option<f64> } // None renders "?"

// [brief]
struct FixtureInfo { name, dir, baseline, bin, started_service, mode, server_init_hash: String }
struct Tokens { single: Vec<(&'static str, String)>, bead, park, fixture, deadline,
                finish: String }

// [session]
struct SessionSpec { prog: String, args: Vec<String>, stdin_file: PathBuf, log: PathBuf,
                     cwd: PathBuf, env: BTreeMap<String, String>, timeout: Option<u64> }
struct Stop { sig: AtomicI32 /* 0 none */, pgid: AtomicI32 /* the session's group */ }

// [worktree]
enum Evict { Kept, Moved(PathBuf), Refused }
enum Act { Log(String), Note(String), SetBranch(String) }

// [run] aeon.sh's globals, now one struct
struct State { aeon, bead: String, claimed: Option<BeadRow>, repo_name: String,
    repo: PathBuf, repo_land, branch: String, pidfile, logf, work: Option<PathBuf>,
    base, base_branch, base_remote, base_fq: String, world_was_stopped: bool,
    fixture: Option<FixtureInfo>, fixture_lib: Option<PathBuf>, session_started: bool,
    session_rc: i32, committed: bool, lc_model_restricted: bool,
    requeue_cause: Option<String>, requeue_why: String, session_epoch: i64,
    rebase_conflicts: String, sop_before: Option<String>, groom_lines_before: usize }
```

Ports (`ports.rs`, `session.rs`): `Bd::bd(args) -> Out`, `Seam::call(func, args) -> Out`,
`Git::git(dir, args) -> Out`, `Exec::exec(prog, args, stdin, cwd) -> Out`,
`Launcher::run(&SessionSpec, &Stop) -> i32`, `Beat` (the heartbeat's clock, trace, fuse and
`bd heartbeat`). `Out { code, stdout, stderr }`.

## 4. Behaviour (the decisions, as pure functions)

### 4.1 Claim phase order (unchanged)

fayth file → `SPIRA_REQUIRE_LABEL` fold → fence (`FAYTH_LABELS` non-empty and, when
`SPIRA_SCOPE_LABEL` is set, containing it) → ledger trim → `born` → [sweep branch] →
capacity (`fayth_free` with the elastic pool = `SPIRA_MAX_AEONS - have`) → `world.halted`
→ `world.draining` → `capacity_paused` → [dry run] → name → ready query → `spira-claim
epics` → `spira-claim select --top-tier` → resumability (branch ahead of its base) →
`spira-claim select --resumable` → claim loop → `awake` → `aeon.claimed` event → poison
race → lifecycle gate/CAS → world-stop fence → repo → branch → pidfile/trace mark →
**teardown armed** → mailbox → heartbeat → base → worktree → rebase → briefs → session →
wiki commit → verdict → teardown.

Note the fence runs before the sweep branch, as in aeon.sh (its comment says a sweep is not
fenced; the code fences it — kept, see §8).

### 4.2 Heartbeat (`hb_tick`)

Every `FAYTH_HEARTBEAT_SECONDS` (default 30): trace mtime changed → renew (deadline = now +
lease); else now ≥ deadline → **lapse**; else fuse (`aeon_fuse_minutes`, integer) ≥ wall
(`SPIRA_THRASH_MINUTES`, 20) **and** session minutes ≥ wall → **thrash**; else ok. Then
`bd heartbeat <id>`; a failing heartbeat ends the heartbeat (as the bash subshell's
`|| exit 0`). Lapse writes `.lapsed`, thrash writes `.thrash`, then the session's process
group is sent TERM and the aeon goes straight to teardown with rc 143 (the bash killed its
own process group, so its trap ran with 143 and skipped the verdict block — same outcome).

### 4.3 Disposition (`aeon_disposition`, first match wins)

| # | input | status | charge | requeue cause | note |
|---|---|---|---|---|---|
| 1 | capacity_reset_at found | capacity | free | unjudged-capacity | capacity |
| 2 | `.slain` | slain | free | unjudged-slain | slain |
| 3 | `.thrash`, streak ≥ cap | requeue-thrash-charged | charge | thrash-stale | thrash-charged |
| 3' | `.thrash` | requeue-thrash | free | thrash | thrash |
| 4 | `.lapsed` | lapsed | charge | - | lapsed |
| 5 | gate still running | gate-unfinished | free | unjudged-gate-unfinished | gate-unfinished |
| 6 | open ask blocker | decision-blocked | free | unjudged-decision-blocked | decision-blocked |
| 7 | session rc 124, nothing committed | timeout | free | unjudged-timeout | timeout |
| 8 | harness requeue cause | requeue-<c> | free | <c> | requeue |
| 9 | own operator-wait marker | operator-wait | free | unjudged-operator-wait | operator-wait |
| 10 | lifecycle-verified or submitted label | submitted | free | - | submitted |
| 11 | yield headless | yield-headless | charge | - | yield-headless |
| 12 | session never started | pre-session | charge | - | pre-session |
| 13 | outcome `unlanded` | <bd status> | charge | - | unlanded |
| 14 | otherwise | <bd status> | free | unjudged-<outcome> | not-judged |

Inputs are gathered lazily in the same order (a later marker is not consumed when an
earlier row matched). The side effects per note key (release, `bump_requeue`, notes,
`bump_lapsed`, `write_lapse_record`, `capacity_pause_set`) and every note text are aeon.sh's.

### 4.4 Closed-bead branch (gate status, `gate-run.sh --status <branch> <repo>`)

0 recorded PASS → note; 2 still running → note; 1 FAIL → (not queued) note / (queued, no
own commit ahead) log only / (queued, own commit) `bead_reopen cert-gate-red`, st=open;
3/4/5 (none/stale/died) → note, or (queued with own commit, not already CERTIFIED at this
tip) `defer_self_cert` note. Then the submitted conversion (§4.5), the closed operator-wait
marker, pidfile removal, mailbox removal, `done` line, exit.

### 4.5 Submitted conversion

A closed bead whose persona is not graph-only, whose model was **not** restricted, whose
type is a work type (`bead_is_work_type`) and which carries no `delivers:` label: superseded
→ close stands; no commit naming it ahead of the base → close stands; else
`bead_reopen work-close-converted` + `label add spira-submitted`, st=submitted.

### 4.6 Verdict block (after the session, in order)

wiki commit → status/superseded/delivers/type from one `bd show` → `verdict_committed` →
eviction race (`eviction_reopen`; count of prior `eviction-race` reopens from spira-claim)
→ `close_verdict` (keep delivers / keep superseded / reopen delivers-mismatch / reopen
closed-without-commit unless a work type) → own-worktree dirty (reopen prod-dirty,
`SPIRA_ALLOW_PROD_DIRTY`) → SOP closing rule (`sop_rule_verdict`; poison) → close-reason
fence (`close-reason-flags.py`, `SPIRA_CLOSE_REASON_OVERRIDE`) → groom escalation check →
workflow-run fence (§8: only when `DB` is set in the environment) → rebase currency
(current / rebased / cited-on-base retire / reopen rebase-conflict).

## 5. The lib.sh seam

One helper, `LibSeam`, runs `bash -c <FIXED>` where `FIXED` is a compile-time constant
(`src/seam.rs`). **All data is on stdin**, NUL-framed:

```
<lib.sh path>\0<fayth file path or empty>\0<function>\0<arg1>\0<arg2>\0...
```

The fixed script reads the records, sources `lib.sh` (and thus `conf.sh`) and the fayth file
(then folds `SPIRA_REQUIRE_LABEL` exactly as aeon.sh did), refuses any function not on its
allowlist (exit 97), and calls it. stdout is the answer, the exit code is the function's.
The environment is the aeon's own (identity exports like `BEADS_ACTOR` included — they
are how lib.sh names the actor, as in aeon.sh). Nothing is passed in argv or as an ad hoc
env var.

Wrappers defined inside `FIXED` (because the answer is a global the plain function sets, or
because several reads belong to one call):

| wrapper | returns |
|---|---|
| `_aeon_snapshot` | `env -0`, then `\0\0`, then `NAME\0VALUE\0` for the fixed var list, then `READY_ARGS` NUL-joined, then `CLAIM_EXCLUDE` |
| `_aeon_capacity_paused` | rc of `capacity_paused`; stdout `SPIRA_CAPACITY_LEFT` |
| `_aeon_rebase` | rc of `rebase_branch`; stdout `REBASE_CONFLICTS` |
| `_aeon_thrash_meta <id>` | `bead_metadata` thrash_streak, thrash_tip, thrash_last on 3 lines |

`_aeon_repo_info <name>...` (`name\troot\tlandref` per resolvable repo, for resumability)
and `_aeon_base <repo>` (landref/`ref_branch`/`ref_remote` on three lines) are retired
(sp-o88bx, "wave 4.12"): family W now resolves in-process through `spira_config::repos` —
`claim::Selector::resumable` and `run::Run::work`'s own base block, respectively — never
reaching this seam at all.

Allowlisted lib.sh functions (the complete list — every other lib.sh behaviour aeon.sh used
is reimplemented in Rust, §6):

| function | used for |
|---|---|
| `aeon_name_take` | the instance name (cursor file, live-name scan) |
| `aeon_count`, `fayth_free` | capacity (systemd unit list / pidfiles) |
| `spira_event` | `aeon.claimed` |
| `release_own_claim` | every release (lifecycle Release + bd unassign) |
| `lc_claim_bead`, `lc_bead_verified` (the eviction-race `hold` is `spira-lc hold`, run directly since sp-arpjt) | lifecycle machine |
| `park_unmapped` | unmapped repo |
| `spira_prune_worktrees` | prune-with-repair before cutting a worktree |
| `bead_reopen` | every reopen (landstate WITHDRAWN, label removal, release, cause row, note) |
| `bump_requeue`, `bump_lapsed`, `write_lapse_record`, `thrash_streak_bump`, `requeues_of` | event rows / thrash metadata / the display count on a requeue note |
| `capacity_reset_at`, `capacity_pause_set` | the account's capacity window |
| `eviction_reopen`'s inputs: `land_state`, `land_mark` | landstate |
| `bead_is_work_type`, `bead_cited_commit_on_base`, `other_beads_on_conflicts`, `spira_destroy_branch` | helpers shared with landing |

`session_outcome`, `session_yield_headless`, `open_ask_blocker`, `verdict_committed`,
`close_verdict`, `delivers_verdict` and `rapid_recur_check`/`rapid_recur_streak` were the
last aeon-only lib.sh functions this seam reached — ported natively into `aeon::decide`,
`aeon::verdict` and `aeon::run::Run::rapid_recur_check` at sp-8kqww (wave 4.33) and deleted
from lib.sh (zero callers left once aeon stopped shelling out for them).

`trace_last`, `aeon_fuse_minutes`, `aeon_lease_minutes`, `trace_stats`, `trace_tail`,
`wiki_write_paths`, `wiki_commit_paths`, `groom_claims_verified`, `bead_named_paths` and
`_tsd_aeon_session` were the trace/heartbeat family (P), the live pair of the brief/memories
family (Q) and the aeon half of the tsd producers (AC) — ported into `aeon::trace` at
sp-27d3d (wave 4.34) and deleted from lib.sh. cockpit-collect and sentinel, which read the
same trace through `trace_stats`/`trace_tail`/`aeon_fuse_minutes`/`aeon_lease_minutes`,
now depend on this crate and call `aeon::trace` in-process instead of `io::lib_call` or a
bash seam. `aeon_fuse_minutes`'s one remaining external read — the base ref, family W — was
`spira_landref` through this seam when sp-27d3d landed; sp-o88bx ("wave 4.12"), concurrent
with it, retired that seam call too: `RealBeat::fuse` now resolves it through
`spira_config::repos::landref` directly and hands the result to `aeon_fuse_minutes` as
before, an already-resolved `Option<i64>` commit timestamp.

Cost: ~0.2 s per call (sourcing lib.sh). A claim run makes ~15-25 calls; the heartbeat makes
none for its base ref now (resolved in-process).

Other subprocess seams are the scripts aeon.sh already called (§2.7), invoked the same way.

## 6. Reimplemented in Rust (was lib.sh or inline)

`log`/`die`; `bdq`/`bdjson`/`json_only`/`claim_retry` (for the aeon's own bd calls — the czar
fence and create-time checks do not apply to any call the aeon makes itself); `aeon_own_unit`;
`fayth_fenced`; `fayth_lease_seconds`; `world_stop_decide`; `hb_tick`; `aeon_disposition`;
`outcome_charges`; `eviction_reopen` (pure part); `sop_rule_verdict`; `bead_has_label`;
`session_result_fields`; `attempt_trace`; `spira_trace_mark`; `worktree_evict_foreign`;
`worktree_move_aside`; `render_memories`; `system_prompt_split`; `aeon_settings`;
`aeon_claude_argv` (`persona_model` through the spira-config library on conf.sh's resolved
`SPIRA_TOML_FILE`); `render_resume_brief`; `render_slain_brief`; `render_deadline_brief`;
`render_holds_brief`; `bound_bead_notes`; the band/rank python (now `spira-claim`); the
chamber overlay; placeholder substitution.

`lifecycle_enforce` is read as conf.sh reads it: the unit's environment wins
(`SPIRA_LIFECYCLE_ENFORCE`, how fixtures pin it); otherwise `spira.lifecycle_enforce` from
the `spira.toml` conf.sh resolved (`SPIRA_TOML_FILE`), through the `spira-config` library
(law-config-through-the-cli-only); otherwise off. Binary presence is consulted only to
refuse an enabled-but-unbuildable configuration.

## 7. Tests

`cargo test -p aeon` — 135 lib tests + 2 bin tests (sp-8kqww, wave 4.33; count drifts as
functions keep moving over from lib.sh — read it off the actual run, not this number).
Fakes for `Bd`, `Seam`, `Exec` and `Launcher`; git is real
(temp repositories) wherever the assertion is about git's own behaviour.

| module | contract it pins |
|---|---|
| `ledger` | every ledger format byte for byte, cockpit-metrics' regex over it, dry run writes nothing, trim 20,000→5,000, session fields (sum vs last, half-even rounding, `?` never 0), `attempt_trace` segments across a 64 KiB chunk, trace-mark numbering |
| `decide` | the disposition table row by row and its precedence, hb_tick (test-aeon-lease/test-thrash rows), world-stop, eviction, SOP verdict; `open_ask_blocker`, `session_outcome`, `session_yield_headless` and `rapid_recur_streak` phrasing/fixture tables (sp-8kqww, wave 4.33 — ported from the bash suites they retired) |
| `verdict` | `eviction_race_count`; `verdict_committed` (branch then landrefs fallback), `delivers_verdict` (beads/note/report/applied.jsonl/check/action), `close_verdict` precedence (sp-8kqww, wave 4.33) |
| `brief` | FINISH follows lifecycle_enforce only; LANDING/PARK per mode; FIXTURE truthful; resume/slain/deadline/holds renderers; notes bounding (`bead_body_is_bounded`); overlays whole/section/append/absent/blocks; literal, ordered substitution; thrash banner lands in task.md; system/task split; memories tiering and budget |
| `claim` | ready set on stdin and nothing in argv; lookup/rank failure is claim-error; resumable tier; lost race falls through; claim_retry |
| `worktree` | fresh cut + resume; mislabeled branch reset (sp-om71s case 1); own branch at a previous path moved aside (case 2); test-aeon-worktree-evict-foreign.sh row for row, including the refused move |
| `session` | lease lapse and thrash trip the stop and write their markers; the trip signals the session's group, never the aeon's; timeout is 124 |
| `seam` | NUL framing; the fixed script sources lib.sh and the fayth, folds SPIRA_REQUIRE_LABEL, refuses a function off the allowlist; snapshot parsing |
| `conf` | fayth defaults, the fence, lifecycle_enforce (environment wins, toml bool read through spira-config, binaries irrelevant), persona_model |
| `run::tests` / `tests` | whole runs: fence, capacity, halted/draining/paused order, dry run, claim-error, idle, poison race, enforce refusal, binary presence never enforces, enforce restricts the model, refused lifecycle claim, world-stop fence, unmapped repo (+ world restarted), happy path to submitted (ledger, brief, argv, env scrub), unlanded exit code, slain (verdict skipped, rc 143), pre-session death, rebase-conflict requeue, sweep |
| `main` | argv grammar |

The seam's fixed script was also exercised against this branch's real `lib.sh` with an
isolated HOME/config (read-only for the harness): `_aeon_snapshot` returns the builder's
predicate, READY_ARGS and CLAIM_EXCLUDE; an off-list function is refused with 97.

The bash suites that drive `aeon.sh` end to end are retired or repointed in the cutover
(§9); the lib.sh functions they cover stay tested by their own suites.

## 8. Behaviour deliberately changed (and kept)

Changed:

1. **Placeholder substitution is literal.** aeon.sh used `sed s|{{X}}|$VAL|g` (a value
   containing `&`, `\` or `|` was mangled or broke the whole `sed`, leaving an empty
   prompt) and bash `${P/{{X}}/$VAL}` (bash ≥5.2 `patsub_replacement`: an `&` in a bead
   body was replaced by `{{BEAD}}`). Rust replaces the literal text. The multi-line
   tokens (`{{BEAD}}`, `{{PARK}}`, `{{FIXTURE}}`, `{{DEADLINE}}`, `{{FINISH}}`) still
   replace the **first** occurrence only, as before; single-line tokens replace all.
2. **A world stopped for a bead is restarted if the aeon exits before its teardown is
   armed** (the unmapped-repo exit came after `world.sh stop` and before the trap, so the
   world stayed halted).
3. **Process model.** The model session runs in its own process group; lease lapse and
   thrash signal that group instead of the aeon's own (`kill -TERM -$$`). The aeon then
   tears down exactly as the bash trap did (rc 143, verdict block skipped).
4. **`bash -e` after the session is not reproduced.** aeon.sh ran the verdict block under
   `set -e`, so any incidental non-zero command there aborted the remaining fences and
   jumped to teardown. Each fence here runs to its decision.
5. **The resumability tier** comes from `spira-claim select --top-tier` (the same lines the
   python produced; verified byte-identical by spira-claim's §6).
6. **The eviction-race prior count** comes from `spira-claim requeues --json` (reopen rows
   with cause `eviction-race`) instead of a hand-built `SELECT COUNT(*) … requeued …`
   interpolating the bead id. bead_reopen writes one such row per eviction-race reopen, so
   the count is the same; on "cannot tell" it is 0, as before.
7. **`aeon_alive`/install detect the binary** (cutover), since a Rust process's cmdline
   does not contain `aeon.sh`. Until cutover item 5 lands, lib.sh reads every binary
   aeon as dead (aeon_name_take would reuse names; pidfile-mode aeon_count would undercount),
   so items 1-5 land together.
8. **`lifecycle_enforce = true` in spira.toml now takes effect.** conf.sh exports the toml
   bool as the string `true` and aeon.sh tested `= 1`, so the key could only ever be
   switched on from the environment or spira.conf's `1`. The Rust reads the typed key
   (and accepts `1`/`true` from the environment).
9. **The early-exit disposition branches remove the pidfile, its `.name` and the mailbox**
   (capacity, slain, thrash, lapsed, gate-unfinished, decision-blocked, timeout, requeue,
   operator-wait, submitted, yield-headless, pre-session). aeon.sh `exit`ed inside the case,
   above the removal, leaving them for other scans to reap by pid liveness.
10. **TERM delivered to the aeon's pid alone is forwarded to the session's group.** bash
    deferred its trap until the model exited (slay.sh's fallback `kill -TERM <pid>` left the
    session running until it ended on its own). `systemctl stop` (the whole cgroup) is
    unchanged.
11. **Resumability skips a candidate whose repository has no resolvable land ref** (bash
    counted `git rev-list ..<branch>`, i.e. against HEAD of the shared checkout).
12. **A missing `spira-claim` is a claim-error** (`awake <f> claim-error spira-claim not
    found`, exit 1) — the ranker is required, not optional.
13. **`spira/work-env.sh` is retired, not called.** (sp-zpaq0, rewrite wave 5.) Its only
    caller was aeon.sh/aeon itself (work/DESIGN.md §2); the allow-listed restricted
    environment it built with `env -i` is now built in-process (`restrict.rs`) and applied
    directly to the model's `SessionSpec`, rather than spawned as a wrapper around the
    agent binary. The acceptance property is unchanged ("in the provided aeon environment,
    `command -v bd` fails and no credential is readable") and is now a `cargo test -p aeon`
    fact (`restrict.rs`'s own tests) instead of a bash integration fixture's.
14. **`spira/escape.sh` is retired into this binary as `aeon --escape <fayth> [--dry-run]`.**
    (sp-zpaq0.) `world_gate`, `capacity_paused`, `fayth_ready` and `summon_argv` are still
    reached through the lib.sh seam (§5) — they carry real side effects (a capacity probe
    can clear the pause file; an expired drain is lifted and logged) that a Rust
    reimplementation would have to duplicate exactly or drift from; `escape.rs` is the
    decision (`decide`, pure, unit-tested) and the `systemd-run` argv/spawn (`run`).

Kept, although they look wrong (flagged for the operator):

- **The workflow-run fence is inert in production.** aeon.sh passes `SPIRA_DB="$DB"` under
  `set -u`, and nothing sets `DB`, so the command substitution dies and the fence reads
  "OK" on every close. Rust runs the fence only when `DB` is set in the environment —
  exactly when bash would have. Activating it is a policy change (it reopens on a GitHub
  API failure), not a port decision.
- **Sweep mode is fenced** (the fence precedes the sweep branch although its comment says
  it should not).
- **`{{BEAD}}` etc. replace only the first occurrence** (concierge.md has three `{{BEAD}}`,
  but the concierge is not summoned through the aeon).
- **The six session briefs are appended to task.md with no separator** (DIRTY, RESUME,
  SLAIN, ALREADY_DONE, CLOSE, REBASE: `printf '%s'` each), so `## If you find the work is
  already done` follows the template's last line directly.
- **Three verdict reopens do not mark the bead open for the later fences**
  (delivers-mismatch, closed-without-commit, the SOP poison): aeon.sh left `st=closed`
  after them; the teardown re-reads the status from the store, as it did.
- **The `keep|delivers` log line never fires**: close_verdict prints
  `keep|delivers (<types>) verified` and aeon.sh's case matched `keep|delivers` exactly.
- **Setup still races the lease**: the heartbeat starts before the worktree, the rebase and
  the fixture, so a setup quieter than the lease lapses it, as before.

## 9. Cutover

> **Historical record.** Its `SPIRA_AEON_BIN` / `SPIRA_CLAIM_BIN` / `spira_bin` rows were later
> superseded by sp-gypjk: `aeon` and `spira-claim` are invoked by bare name on the launcher's
> PATH, and no binary resolver remains.

**Not performed** (operator's directive). Line numbers against this branch's base
(`7ce25b21b`). `$SPIRA_AEON_BIN` is resolved in conf.sh like `SPIRA_LC_BIN`.

1. **spira/conf.sh after line 2310** (`export SPIRA_LC_BIN`), add:
   ```bash
   if [ -z "${SPIRA_CLAIM_BIN:-}" ]; then
       SPIRA_CLAIM_BIN="$(command -v spira-claim 2>/dev/null)" || SPIRA_CLAIM_BIN="$(spira_bin spira-claim 2>/dev/null)"
   fi
   export SPIRA_CLAIM_BIN
   if [ -z "${SPIRA_AEON_BIN:-}" ]; then
       SPIRA_AEON_BIN="$(spira_bin aeon 2>/dev/null)" || SPIRA_AEON_BIN=""
   fi
   export SPIRA_AEON_BIN
   ```
   (spira-claim's own cutover item 1 adds the first half; apply once.)
2. **spira/lib.sh:2724-2730** (`summon_fayth`). Insert before line 2725
   (`local _sargv; mapfile -t _sargv …`):
   `    [ -x "${SPIRA_AEON_BIN:-}" ] || { log "CHECK7 $f: aeon binary not built (SPIRA_AEON_BIN) — not summoning"; return 1; }`
   and at line 2730
   current: `        "$SPIRA_HOME/aeon.sh" "$f" 2>/dev/null`
   replace: `        "$SPIRA_AEON_BIN" --home "$SPIRA_HOME" "$f" 2>/dev/null`
3. **spira/escape.sh:52-56**. Insert before line 52 (`log "escape.sh $FAYTH: $r ready …`):
   `[ -x "${SPIRA_AEON_BIN:-}" ] || die "escape.sh $FAYTH: aeon binary not built (SPIRA_AEON_BIN)"`
   and at line 56
   current: `    "$SPIRA_HOME/aeon.sh" "$FAYTH" ${DRY_FLAG} 2>/dev/null`
   replace: `    "$SPIRA_AEON_BIN" --home "$SPIRA_HOME" "$FAYTH" ${DRY_FLAG} 2>/dev/null`
4. **systemd/spira-ops.service:46**
   current: `ExecStart=/bin/bash -c 'exec @SPIRA_PROD@/aeon.sh ops --sweep --prompt - < @SPIRA_RUN@/ops-sweep-prompt.txt'`
   replace: `ExecStart=/bin/bash -c 'exec @SPIRA_REPO@/bin/aeon --home @SPIRA_PROD@ ops --sweep --prompt - < @SPIRA_RUN@/ops-sweep-prompt.txt'`
   (the binary path convention of `spira-lc.service:18`, `@SPIRA_REPO@/bin/…`.)
5. **spira/lib.sh:822** (`aeon_alive`)
   current: `    grep -qF 'aeon.sh' <<< "$cmd" || return 1`
   replace: `    grep -qE '(^|/)aeon( |$)|aeon\.sh' <<< "$cmd" || return 1`
6. **install.sh:246** (`_conflict_aeon`) — a binary aeon's cmdline is
   `<repo>/bin/aeon\0--home\0<home>\0<fayth>`; match its path as the second pattern:
   current: `    match="$(printf '%s\n' "$home/aeon.sh" \`
   replace: `    match="$(printf '%s\n' "$home/aeon.sh" "$(dirname "$home")/bin/aeon" \`
7. **spira/timeout-lint.sh:41** — drop `"$HERE/aeon.sh"` from the default list (the lint
   checks bash `timeout` wrapping; the Rust binary bounds every bd call itself).
8. **spira/config-fence-allow:26** — remove `spira/aeon.sh`.
9. **spira/full-aeon-fixture.sh:61-62** replace the `grep -q 'SPIRA_AGENT' "$HERE/aeon.sh"`
   guard with `[ -x "$SPIRA_AEON_BIN" ] || { …refusing… }` and **:80**
   `"$HERE/aeon.sh" "${1:-builder}"` → `"$SPIRA_AEON_BIN" --home "$HERE" "${1:-builder}"`.
10. **Build/release**: add `aeon` to whatever copies built binaries into
    `$SPIRA_ARTIFACTS`/`bin/` (the same list `spira-lc`, `work` and `spira-claim` are on).
11. **Delete `spira/aeon.sh`** once 1-10 have landed and one production aeon has run
    through the binary (a `done` line with `status=` from `aeon`, visible in the ledger).
12. **Suites.** Each of these executes `aeon.sh`; repoint the invocation to
    `"$SPIRA_AEON_BIN" --home "$SPIRA_HOME"` (or `$HERE`) and drop its
    `grep -q 'SPIRA_AGENT' aeon.sh` guard (the binary honours `SPIRA_AGENT` the same way):
    `test-aeon-elastic-concurrency.sh:68,79` (and drop the `fayth_free` grep at :52),
    `test-aeon-chamber-overlay.sh:54`, `test-aeon-eviction-race.sh:135`,
    `test-aeon-gate-close-silent.sh:184`, `test-aeon-gate-unfinished-attempts.sh:90`,
    `test-aeon-prod-dirty.sh:92`, `test-aeon-resume-collision.sh:47`,
    `test-aeon-resume.sh:148`, `test-aeon-prompt-layers.sh:112`,
    `test-aeon-lifecycle-cutover.sh:206`, `test-aeon-slain-attempts.sh:83`,
    `test-aeon-wiki-dirty.sh:120`,
    `test-aeon-worktree-collision.sh:57`, `test-aeon-world-stop.sh:94`,
    `test-aeon-teardown-e2e.sh:515`, `test-aeon-sweep.sh:52`, `test-cross-repo.sh:141`,
    `test-epic-claim-order.sh:261`, `test-groom-escalation-check.sh:101`,
    `test-holds.sh:210`, `test-lifecycle-enforce-gate.sh:93`, `test-mail-aeon.sh:151`,
    `test-ops-closing.sh:82`, `test-persona-model.sh:183,233`, `test-spike.sh:77`,
    `test-submitted-lands.sh:105`, `test-thrash-teardown.sh:110,192`, `test-timeout.sh:188`,
    `test-world-drain-deadline.sh:89,119`. Those that `cp "$HERE/aeon.sh" "$SPIRA_HOME/"`
    stop copying it (the binary takes `--home`).
13. **Structural greps that read aeon.sh's source** — retire the assertion; the property is
    a `cargo test -p aeon` case now:
    `test-brief-notes.sh:170-174` (T6 BEAD_BODY through bound_bead_notes → `brief::tests::bead_body_is_bounded`),
    `test-census-events.sh:410-432` (sp-ytw2h cause pairs: repoint its scan to `aeon/src/*.rs`,
    where the causes are `const` strings in `verdict.rs`),
    `test-thrash-teardown.sh:220-225` (process-group kill → `heartbeat::tests::trip_signals_session_group`),
    `test-attempts.sh:197`, `test-event-taxonomy.sh:55`, `test-world.sh:249`, and the
    `grep`-for-shape guards in the suites of item 12 (same rows as their exec line, per
    the list in the commit that added this file).
14. **Stay as they are** (they test lib.sh functions, which stay): `test-aeon-disposition.sh`,
    `test-aeon-lease.sh`, `test-aeon-world-stop.sh` (lib half), `test-aeon-worktree-evict-foreign.sh`,
    `test-aeon-base-ref-qualify.sh`, `test-aeon-launch-grammar.sh`,
    `test-aeon-settings-guard-allowlist.sh`, `test-aeon-dirty-commit.sh`,
    `test-aeon-wiki-concurrent.sh`. When lib.sh's aeon-only functions (`aeon_disposition`,
    `world_stop_decide`, `hb_tick`, `hb_wait_outcome`, `sop_rule_verdict`,
    `render_*_brief`, `bound_bead_notes`, `system_prompt_split`, `aeon_claude_argv`,
    `aeon_settings`) lose their last caller, retire them with these suites.
