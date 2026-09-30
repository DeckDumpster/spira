# queue — the merge queue's operator and landing tool

Replaces `spira/queue.sh` (1,925 lines). Same subcommands, same records, same output lines,
one Rust binary `queue`. This document is the contract; the code satisfies it and the unit
tests are derived from it. It was written before the code, from the script's intent, not
by porting it line by line.

## 1. Intent

The merge queue moves certified branches onto a repository's landing ref without ever
landing something no gate judged, and without two writers racing one batch. `queue` is its
**hand-operated and terminal-step half**: the automatic cut belongs to the batcher crate,
the CI settle to `queue verdict` (DESIGN-verdict.md), the periodic pass to `landing.sh`/`spira-verdict.sh`. It
answers six needs:

1. **Certify** a branch (`submit`): run the repository's gate, and record the result where
   the batch builder reads it (queue modes), or land it (push mode).
2. **Operate an open batch** (`queue.forge`): cut one by hand (`open-batch`), take it for a
   hand edit (`claim`/`release`), pull one member out (`eject`), or throw the batch away
   (`abandon`) — every mutation serialised by the repo's queue lock, every owner-claimed
   batch refused unless the actor is its owner.
3. **Land a round locally** (`land-local`, `queue.local`): fast-forward the local landing
   ref by CAS, archive the round head, mark every member LANDED and close its bead, then —
   for the harness, while a release is in force — publish the round's own binaries as
   `spira-releases/<sha>` and activate it (§8 D13). The ONLY writer of the local landing ref.
4. **Publish** (`publish`, `queue.local`): push local/main's unpublished range to the forge
   as one PR whose members are the beads that range landed.
5. **Change land mode** (`to-forge`/`to-local`): move a repository between `queue.local`
   and `queue.forge` without losing or duplicating a commit, and without doing it under a
   batch or round in flight.
6. **Undo and observe** (`rollback-local`, `stats`, `protect`, `flush`, `step`).

### What it is deliberately NOT

- **Not the batcher.** Round membership, trigger and local proving belong to `batcher`/
  `batcher-cut`; `flush`/`step` only invoke them.
- **The verdict is a separate contract.** Settling a batch or publish PR from CI is
  `queue verdict`, specified in DESIGN-verdict.md (it replaced verdict.sh, sp-flj4a);
  `to-forge` calls its publish settle in process.
- **Not lib.sh.** Landstate writes (with their TSD dual-write), bead reopen/close, claim
  release, the events log, concierge mail, forge run cancelling, the rebase machinery and
  the repo map/landref resolution stay in lib.sh and are called through one documented
  seam (§6). queue re-derives none of their side effects.
- **Not a config reader of spira.toml.** Repository rows are resolved by lib.sh (the reader
  every other component uses, §6 R1); the one config WRITE it makes (land-mode transition)
  goes through the spira-config library (law-config-through-the-cli-only).

## 2. Contract

### 2.1 Invocation

```
queue submit <branch> [<repo>]
queue protect [<repo>]
queue stats
queue flush [<repo>]
queue step <repo>
queue eject <bead> [<repo>] [--reason T | --reason-file F|-] [--suites CSV] [--red] [--dry-run]
queue abandon [<repo>] (--reason T | --reason-file F|-) [--dry-run]
queue open-batch [<repo>] [--members IDS | --members-file F|-] [--skip-pregate] [--dry-run]
queue claim [<repo>] (--reason T | --reason-file F|-) [--force]
queue release [<repo>]
queue land-local [<repo>] --head <rev> (--members LIST | --members-file F|-) [--worktree <path>]
queue publish [<repo>]
queue to-forge [<repo>]
queue to-local [<repo>]
queue rollback-local [<repo>]
queue -h | --help
```

`<repo>` defaults to the home repository (`spira_home_repo`). Option spellings `--x v` and
`--x=v` are both accepted, as before. `--reason-file`, `--members-file` are new: the stdin/
file form every caller should move to (law-payloads-go-on-stdin); the argv forms stay for
the callers that exist today. `--red` and `--worktree` are new (§8).

**Exit codes** (unchanged): `0` done (including "nothing to publish"); `1` refused or
failed — and for every refusal *nothing changed*, unless the message says otherwise; `2`
usage (missing required argument, unknown option). `submit` returns the gate's own exit
code when the gate fails (so `75` NO_VERDICT and `76` BASE_FAIL propagate). An unknown
subcommand prints the usage line to stderr and exits `2`.

**Output.** Every line queue.sh printed is printed with the same text and on the same
stream, **including its `queue.sh <cmd>:` prefix**: callers and operators grep these lines
(§2.3), and a binary rename is not a reason to break them. lib.sh log lines emitted by
seam calls (`<ts> spira: land-close …`) pass through to stdout as before.

**Environment switches read** (unchanged meaning): `SPIRA_QUEUE_LOCK_HELD=1` (land-local,
publish, rollback-local skip their own flock — the caller holds it), `SPIRA_QUEUE_ACTOR`,
`BEADS_ACTOR`, `SPIRA_AEON`, `USER` (actor), `SPIRA_QUEUE_OWNER_OVERRIDE=1`,
`SPIRA_FAYTH`/`SPIRA_CZAR_CLASS` (czar fence), `SPIRA_CERTIFY_SUITES` (submit),
`SPIRA_QUEUE_BATCH_WAIT` (flush forces 0 for the batcher), `SPIRA_PREFLIGHT_WALL_SECS`,
`SPIRA_QUEUE_TRANSITION_POLLSEC`/`_MAXSEC`, `SPIRA_PUBLISH_REMOTE[_<NAME>]`,
`SPIRA_PUBLISH_BRANCH_<NAME>`, `SPIRA_LAND_UNGATED` and `SPIRA_VERDICTS` (land-local, §8 D12),
`SPIRA_HOME` (where lib.sh and the scripts live; else spira-config `spira.prod`; else
`<exe>/../spira`).

### 2.2 Subcommands

Each row: preconditions (refusals print the quoted message to stderr and exit 1 with
nothing changed), then effects in order.

| cmd | mode | lock | effects |
|---|---|---|---|
| `submit <br> [repo]` | any | none | queue modes require `spira/*` or `spira-suite-state/*`; branch must exist. Gate: `gate.sh <br> <repo>` with `SPIRA_GATE_BEAD=<id>` `SPIRA_GATE_SUITES=${SPIRA_CERTIFY_SUITES:-on}`. Red: gate output to stderr, `QUEUE CAUGHT` line, exit gate rc. Green: `QUEUE GATE_COST` line, then queue/queue.local: `land_mark <id> CERTIFIED <tip>` + `$SPIRA_QUEUE_DIR/<id>` = `CERTIFIED <tip> <epoch>\n`; push: fetch, `rebase_branch`, push `<br>:<base-branch>`, `land_mark LANDED`, `bead_close_on_land`; pr/hold: `land_mark CERTIFIED`. |
| `protect [repo]` | queue | none | `forge branch-protect <path> <base-branch>`; receipt `$SPIRA_RUN/queue-protected-<repo>` = `<base-branch>\n`; five fixed stdout lines. |
| `stats` | — | none | reads `$SPIRA_RUN/landing.log`; five fixed lines (§3.5). |
| `flush [repo]` | queue | none | `batcher cut <repo>` (by name, on the launcher's PATH — sp-gypjk) with `SPIRA_QUEUE_BATCH_WAIT=0`. `SPIRA_BATCHER_ENABLE=0`: no cut, rc 0 (the operator cuts rounds; replaces `batcher_bin = "/bin/true"`). Ran `batch.sh <repo>`'s pre-cut sweep first, until sp-uwhx0 (batch.sh retirement, below). |
| `step <repo>` | any queued | none | the verdict pass in process (DESIGN-verdict.md); the flush pair; under queue.local then `publish <repo>` (its stderr folded into stdout, as `2>&1` did). Exit is the last step's. |
| `eject <id>` | any | queue lock (non-blocking) | member of the open batch: owner check; `land_mark RED <tip> <reason|ejected>`; then **switch OFF**: `bead_reopen <id> <cause> "" <suites>`; **switch ON**: cause event (§8 D1), `lc_returned`, `release_claim`, spira-lc `eject-member` when the record has `batch_id` (§10); bead comment; survivors `land_mark CERTIFIED`; `forge pr-close`; remove `open`; concierge mail. Not a member but CERTIFIED: `bead_reopen <id> <cause> "" <suites>` (WITHDRAWN) + comment. Otherwise refuse, naming the members. `--dry-run` prints the plan, checks the bead resolves. |
| `abandon` | any | queue lock | `--reason` required (exit 2). No open batch: refuse. Owner check. spira-lc `abandon-batch` when `batch_id` **and the switch is ON** (§10); cancel branch runs; PR comment + close; members not RED/EJECTED → `land_mark CERTIFIED`; append `reason=`/`actor=` to the record, rename it `closed-pr<n>-<stamp>`; `QUEUE ABANDON` line; `queue.abandoned` event; mail. |
| `open-batch` | queue | queue lock | refuse if a batch is open. Candidates from `--members` or ranked CERTIFIED (`queue_sort_rows`); admission (closed or submitted); assemble in `$SPIRA_RUN/worktree/.open-batch-<repo>-<pid>` with `land_subject` merges; `format_batch`; branch `spira/queue/<stamp>`; pre-flight gate unless `--skip-pregate` (wall 124 → open anyway); push; `pr-create` (body on stdin); open record; **switch ON only**: spira-lc cut → `batch_id`/`version` (§10); members `BATCHED`; clear `queue-stuck-<repo>`; `QUEUE BATCH … source=open-batch`. |
| `claim` | any | queue lock | `--reason` required (exit 2); open batch required; already concierge-owned without `--force` → refuse. Rewrite record: `owner=concierge`, `pre_claim_owner=<prev>`, `claim_reason=<one line>`. Mail. |
| `release` | any | queue lock | open batch, owned by concierge, else refuse. `owner=<pre_claim_owner>`, drop claim keys. |
| `land-local` | queue.local | queue lock unless LOCK_HELD | base must be a local branch; resolve head; divergence alarm (cached forge ref, never refuses); `base` ancestor of head, else refuse; **the head's tree carries a gate PASS or round GREEN certificate, else refuse (§8 D12; `SPIRA_LAND_UNGATED=<reason>` overrides, logged)**; the round's binaries when `--worktree` is named (§8 D2); for the harness repository while a release is in force, `--worktree` required (§8 D13); CAS `update-ref`; round-seq+1, `refs/archive/rounds/<n>`; per member `land_mark LANDED`, `gh_issue_closeout`, `bead_close_on_land`, a line; then the release (§8 D13): `release build <head> --bin-dir` → `release verify` → `release activate`, a failure a loud deploy fault (exit 1) that reverts nothing; mail; summary line. |
| `publish` | queue.local | queue lock unless LOCK_HELD | local base; forge target; refuse if a `publish` record exists; fetch; divergence check refuses; equal → "nothing to publish", exit 0; members from land commits (§8 D4), none → refuse; push `spira/publish/<stamp>`; pr-create; `publish` record; `QUEUE PUBLISH` line. |
| `to-forge` | queue.local | queue lock, whole move | agreement check (§6 R1); **in-delivery refusal** (§8 D5); local base exists, not checked out; re-read mode under lock; final publish (lock held); poll the verdict's publish settle (in process) until the record is gone (3 → refuse; deadline → refuse); fetch, forge tip == local tip; write mode `queue.forge`, base `<remote>/<branch>` (§8 D6); verify; archive `refs/archive/<local-base>`, delete local branch; mail. |
| `to-local` | queue (forge) | queue lock | agreement check; in-delivery refusal; base remote-tracking; `local/<branch>` absent and not checked out; re-read mode; fetch; an archived `refs/archive/local/<branch>` must be an ancestor of the forge tip; create `local/<branch>` at the forge tip; write mode `queue.local`, base `local/<branch>` (branch deleted if that fails); verify; mail. |
| `rollback-local` | queue.local | queue lock unless LOCK_HELD | round-seq ≥ 2; `refs/archive/rounds/<n-1>`; a release in force and `$SPIRA_RELEASES/<prev>` present; `release verify <prev>` → `release activate <prev>` (never rebuilt, §8 D13); CAS the ref back; mail. Bead state untouched. |

The czar fence (`SPIRA_FAYTH=czar` with `SPIRA_CZAR_CLASS`) runs `czar-fence.sh <class>`
first on eject, abandon, open-batch, land-local, publish and rollback-local, as before.

### 2.3 Callers

Every caller found (full search of spira/, the crates, systemd/, .github/, the operator
notes and overrides). None parses a line queue prints beyond what §2.1 keeps byte-identical;
most read the exit status only.

| caller | how | relies on |
|---|---|---|
| `spira/landing.sh:1756,1767,1845` (CHECK6, via sentinel) | `bash "$SPIRA_HOME/queue.sh" step "$repo_name" 2>&1` | lines relabelled into landing.log (`queue early: …`); status ignored |
| `spira/spira-verdict.sh:17` (`spira-verdict.service`) | `bash "$HERE/queue.sh" step "$_name" \|\| true` | nothing |
| `spira/suites.sh:468` | `bash "$HERE/queue.sh" submit "$branch" >&2` | status |
| `spira/rebase-stale.sh:142` | `gate_out="$(… submit "$br" "$name" 2>&1)"` | status; output quoted into a reopen note |
| `rebase-stale/src/seam.rs:182-198` | `bash -c 'bash "$0" submit "$1" "$2" 2>&1' queue.sh …` | status + output |
| `batcher-cut/src/io.rs:930-944` `land_local` | `bash queue.sh land-local <repo> --head H --members id:tip,…` with `SPIRA_QUEUE_LOCK_HELD=1` | status; output echoed |
| `reconciler/src/main.rs:182,814-823` | `bash $SPIRA_QUEUE_SH eject <id> --reason "reconciler: … needs rebase" <repo>` | nothing (fire and forget) |
| operator notes `round.sh:207-209` | `SPIRA_BATCH_BINS_TARGET_DIR=… bash queue.sh land-local spira --head H --members "$_mem"` | status |
| operator notes `round.sh:262` | `exec bash queue.sh publish spira` | status |
| `~/.config/spira/overrides/apply.sh:44` | greps an override's output for `holds the lock` | that phrase (kept) |
| `spira/chamber/czar.md:40,90,107,154`, `czar.fayth:90` (`Bash(*queue.sh*)`) | the czar persona runs eject/abandon/step | the tool pattern |
| `spira/hooks/aeon-fence.sh:117-122` | refuses `*/queue.sh*` for aeons except `stats` | the command shape |
| prose: `spira/watchtower.sh:1397`, `spira/skew.sh:387`, `spira/repo-map.example:36,146`, `README.md:156-158` | operator instructions naming `queue.sh <cmd>` | — |

Tests (retired or repointed in §7): `test-queue-submit`, `test-withdrawn-suites-recert`,
`test-queue-protect`, `test-queue-flush`, `test-queue-ops`, `test-queue-owner-refuse`,
`test-queue-owner-mail`, `test-queue-step-eject-race`, `test-czar-shadow`,
`test-queue-open-batch`, `test-queue-land-local`, `test-queue-publish`,
`test-queue-transition`, `test-land-local-release`, `test-skew-refresh`, `test-batcher-cut`
(case L), `test-lifecycle-cutover` (sources queue.sh for `_lc_*`), and the stubs in
`test-certify`, `test-aeon-gate-close-silent`, `test-landing-order`,
`test-landing-starvation`, `test-landing-cutover-round`, `test-reconciler`; static checks
`test-timer-templates:321`, `test-guards:159,162`, `test-ops-allowlist:150`,
`test-git-push-app:177`, `test-queue-flush:77`.

**Readers of what queue writes** (the formats are unchanged, §3): the `open` record is read
by queue verdict, lib.sh `queue_batch_owner`, landing.sh, cockpit.sh, batcher-cut,
queue-watch, czar-pass, spira-lc `legacy_files`, and the operator's round/watch scripts; the
`publish` record by queue verdict and queue-watch; the lock by queue verdict,
batcher-cut and the reconciler's stale-lock probe. `round-seq`, `refs/archive/rounds/*`,
the `$SPIRA_QUEUE_DIR/<id>` entry and `queue-protected-<repo>` have no reader
outside queue itself and the tests; landing.log's ABANDON/PUBLISH lines have none outside
the tests; CAUGHT/GATE_COST/BATCH are read only by `stats`.

### 2.4 Guarantees

- **Refusal changes nothing.** Every refusal before the CAS writes nothing. The release step
  (§8 D13) runs after the landing is recorded and does not revert it: a build, verify or
  activate failure leaves `current` where it was, is loud, and makes the exit non-zero.
- **One writer per repo.** Every mutating subcommand takes `$SPIRA_QUEUE_DIR/<repo>/lock`
  with a non-blocking exclusive flock (the same file the batcher's `try_lock` takes).
- **CAS on the landing ref.** `update-ref <ref> <new> <old>`, never a plain write.
- **Records are written atomically** (temp + rename) — `open`, `publish`, `round-seq`,
  the certified entry. (queue.sh wrote `open` and `publish` in place; §8 D7.)
- **Payloads never in argv or environment** (§5).
- **A read failure is not an empty answer.** A bd read that fails where the answer gates an
  action refuses (open-batch admission, §8 D8). A spira-lc read is made only with
  `lifecycle_enforce` ON, and there an unreachable machine refuses loudly (§10); with the
  switch OFF spira-lc is never invoked, so its absence can block nothing.

## 3. Schema

```rust
/// A repository's land mode, as lib.sh repo_land reports it (queue.forge normalised).
enum LandMode { Push, Pr, Hold, Queue /* = queue.forge */, QueueLocal }

/// One resolved repository (lib.sh seam R1, one bash call per invocation).
struct RepoCtx { name: String, path: PathBuf, mode: LandMode,
                 landref: Option<String>,          // spira_landref, None when it fails
                 map_land: String, map_base: String, // repo_field land/base (agreement check)
                 publish: Option<(String, String)>, // spira_publish_forge: remote, branch
                 remotes: Vec<String> }

/// Settings as conf.sh exports them (seam R1).
struct Settings { home: PathBuf, run: PathBuf, queue_dir: PathBuf, landstate: PathBuf,
                  releases: Option<PathBuf>, forge: PathBuf, repo_map: Option<PathBuf>,
                  batcher_bin: Option<PathBuf>, lc_bin: Option<PathBuf>, db: String, bd: String,
                  submitted_label: String, home_repo: String,
                  transition_pollsec: u64, transition_maxsec: u64, preflight_wall_secs: u64 }

/// `id:tip` — a batch or round member. Serialised "id:tip"; a bare id is tip = head.
struct Member { id: BeadId, tip: String }

/// `$SPIRA_QUEUE_DIR/<repo>/open` — key=value lines, order preserved, unknown keys kept.
struct OpenRecord { pr: String, head: String, base: String, members: Vec<Member>,
                    opened: u64, branch: String, owner: Option<String>,
                    pre_claim_owner: Option<String>, claim_reason: Option<String>,
                    batch_id: Option<String>, version: Option<String>,
                    extra: Vec<(String, String)> }

/// `$SPIRA_QUEUE_DIR/<repo>/publish`.
struct PublishRecord { pr: String, head: String, base: String, members: Vec<Member>,
                       opened: u64, branch: String, remote: String, forge_branch: String }

/// `$SPIRA_RUN/landstate/<id>` — "<STATE> <tip|none> <epoch> <reason…>", no newline.
struct LandState { state: String, tip: String, at: u64, reason: String }

/// `$SPIRA_RUN/landing.log` lines queue writes (and `stats` reads).
enum QueueLine {
    Caught { at: u64, branch: String },                             // QUEUE CAUGHT <at> branch=<id>
    GateCost { at: u64, branch: String, seconds: u64 },             // QUEUE GATE_COST … seconds=<n>
    Batch { at: u64, repo: String, members: u64, gate_seconds: u64,
            verdict: Option<String>, cost: Option<u64>, source: Option<String> },
    Abandon { at: u64, repo: String, pr: String, actor: String, members: String, reason: String },
    Publish { at: u64, repo: String, pr: String, members: u64 },
    Escaped,                                                         // read only
}

/// Why a member left the queue — the bd `reopen` cause row spira-claim classifies.
enum EjectCause { Eject /* harness, rebase: not charged */, EjectRed /* red test: Judged */ }

/// One commit of the publish range (git log forge..local).
struct RangeCommit { sha: String, parents: Vec<String>, subject: String }

/// The land-mode pair a transition writes.
struct ModeRow { mode: LandMode, base: String }
```

### 3.5 `stats` output (unchanged)

```
caught:          <n>
escaped:         <n>
batches:         <n> (<members> members)
local_red_rate:  <red>/<local> (<pct>%)
cost:            <s>s avg per branch
```
`QUEUE BATCH` lines with `verdict=` are local-gate lines (cost from `gate_seconds=`); without
it, CI lines (cost from `cost=<n>s`). `GATE_COST` seconds add to cost. Average is integer
cost / members; percentage integer.

## 4. Files, refs and store rows

| what | read | written |
|---|---|---|
| `$SPIRA_QUEUE_DIR/<repo>/lock` | flock | created |
| `…/<repo>/open` | eject, abandon, claim, release, open-batch, in-delivery | open-batch, claim, release, abandon (renamed `closed-pr<n>-<stamp>`), eject (removed) |
| `…/<repo>/publish` | publish, to-forge | publish |
| `…/<repo>/round-seq` | land-local, rollback-local | land-local |
| `…/<repo>/divergence-alarmed` | lib.sh | lib.sh (via seam) |
| `$SPIRA_QUEUE_DIR/<id>` | — | submit (`CERTIFIED <tip> <epoch>`) |
| `$SPIRA_RUN/landstate/<id>` | eject, abandon, publish (tips), in-delivery | lib.sh `land_mark` / `bead_reopen` only |
| `$SPIRA_RUN/landing.log` | stats | submit, abandon, open-batch, publish, land-local `QUEUE UNGATED` (append) |
| `$SPIRA_RUN/queue-protected-<repo>` | — | protect |
| `$SPIRA_RUN/queue-stuck-<repo>` | — | open-batch (removed) |
| `$SPIRA_RELEASES/current` (is a release in force?), `$SPIRA_RELEASES/<sha>` | land-local, rollback-local | `release build` / `release activate` only (§8 D13) |
| `${SPIRA_VERDICTS:-$SPIRA_RUN/verdicts}/trees/<repo>/<tree>` (tree certificate, `gate::cert`) | land-local (D12) | the gate (PASS), batcher-cut (round GREEN) |
| `SPIRA_LAND_UNGATED` (env) | land-local (D12) | — |
| `refs/heads/<local-base>` | | land-local, rollback-local (CAS), to-local (create), to-forge (delete) |
| `refs/archive/rounds/<n>`, `refs/archive/<local-base>` | rollback-local, to-local | land-local, to-forge |
| spira.toml `[repo.<name>] mode/base` | agreement check (spira-config lib) | to-forge, to-local (spira-config lib) |
| repo-map row (legacy, while the file exists) | via lib.sh | to-forge, to-local (§8 D6) |
| bd: comment, reopen, close, assignee, events `reopen <cause>` | `bd show --json` | via lib.sh seam only |
| spira-lc (switch ON only, §10): batch rows, `list --state IN_DELIVERY` | yes | cut, create-bead, abandon-batch, eject-member, Returned |

## 5. Subprocess handoffs (law-payloads-go-on-stdin)

- **lib.sh seam** (§6): every value — ids, refs, reasons, notes, mail bodies — goes on the
  child's **stdin**, NUL-separated after a fixed script; argv is only `bash`; the
  environment is the caller's own, unchanged.
- **Free text** (PR bodies, bead comments, abandon reasons, mail): stdin (`forge pr-create`,
  `bd comment --stdin` through the seam, mail through the seam).
- **Identifiers** (bead id, branch, ref, sha, repo name, PR number, path) may appear in argv
  of git, `bd show`, forge.sh and the harness scripts whose interface is argv (`gate.sh`,
  `batch.sh`, `verdict.sh`, `release`, `czar-fence.sh`), **only after
  validation** against a fixed grammar (§3 `ident.rs`: `[A-Za-z0-9._/:@+-]`, no leading
  `-`, ≤ 255 bytes) — so argv is bounded by construction, never by the backlog. A value that
  fails validation is refused, never passed.
- **Two argv-only interfaces carry text and are bounded, not removed**: `forge.sh pr-comment`
  (abandon's one-line comment) and `spira-lc … --reason`. Both get the reason flattened to
  one line and cut to 200 bytes. §7 lists the stdin forms to add; until then this is the
  only text queue puts in argv, and it cannot grow with the store.
- **`bd show`** is chunked at 100 ids per call.

## 6. lib.sh seams

One mechanism: `bash` reading a **fixed script from stdin**, compiled into the binary per
operation, followed by that operation's NUL-terminated values. The script reads its values
with `read -r -d ''`, then sources `lib.sh` (and `lc.sh`, and `batch.sh`
where named) with stdin redirected from `/dev/null`, calls exactly one function, and exits
with its status. Nothing is passed in argv or the environment. The script text is fixed per
operation (the function name is part of the text, never data).

| seam | lib.sh function(s) | why not Rust |
|---|---|---|
| R1 `context` | `spira_home_repo`, `repo_root`, `repo_land`, `repo_field land/base`, `spira_landref`, `spira_publish_forge`, plus conf.sh's exported settings | the one resolver every component reads; a second one is exactly the drift law-config-through-the-cli-only names. Returns one NUL-separated record. |
| R2 `land_mark` | `land_mark <id> <state> <tip> [reason]` | landstate + TSD dual-write |
| R3 `bead_reopen` | `bead_reopen <id> <cause> "" <suites>` | reopen, WITHDRAWN, submitted label, release, cause event |
| R4 `cause_event` | `_bump_write_event <id> reopen <cause>` | the events row spira-claim counts |
| R5 `release_claim` | `release_claim <id>` | bd assignee clear |
| R6 `bead_close_on_land` | `bead_close_on_land <id> <sha>` | close + LANDED + branch reap |
| R7 `gh_issue_closeout` | `gh_issue_closeout <id> <sha> <repo>` | GitHub issue close-out |
| R8 `bead_comment` | `bdq comment <id> --stdin` (text piped inside the script) | bdq's retry, czar and fixture logic |
| R9 `notify` | `queue_notify_concierge <repo> <subject> <body>` | mail.sh wiring |
| R10 `event` | `spira_event <kind> - <title> <detail>` | rate-limited events log |
| R11 `divergence` | `queue_local_check_divergence <repo> <path> <forge> <local>` | one alarm per foreign tip, state file |
| R12 `push` | `spira_git_push <path> -q <remote> <src:dst>` | GitHub App credentials |
| R13 `rebase` | `rebase_branch <br> <base> <path> <repo>` (push-mode submit) | scratch worktrees, salvage, formatter |
| R14 `land_subject` | `land_subject <id>` | merge subject with title |
| R15 `sort_rows` | `queue_sort_rows <path> <base>` with `PRIO_JSON` set **inside** the script from a value read on stdin; rows piped in | the ranking batch.sh also uses |
| R16 `cancel_runs` | `queue_cancel_branch_runs <forge> <path> <branch> QUEUE` | RUN_CANCEL log lines |
| R17 `lc_returned` | `lc_returned <id> <reason>` (switch ON only) | the Returned event CAS |
| R18 `batch_fns` | `format_batch`, `base_conflict`, `pf_gate` — open-batch only, the queue.forge assembly primitives | **inlined, sp-uwhx0**: same bodies, no longer sourced from batch.sh (deleted); `pf_gate`'s `_PF_DEADLINE` shared-wall bookkeeping dropped, since open-batch calls it exactly once (the wall is just its own `wall_secs` argument) |
| ~~R19 `settle_publish`~~ | retired: the settle is Rust (DESIGN-verdict.md) | — |
| R23 `create_bug` | `BEADS_ACTOR=<a> bdq create <title> --type bug … --body-file <tmp> --silent` (body read from stdin into the temp file inside the script) | the verdict's fix-forward bead; bdq's retry and fixture logic |
| R20 `readback` | `repo_land`, `spira_landref` after a transition's write | the post-write verification reads what every other component will read |
| R21 `toml_path` | `spira_toml_resolve` (transitions only, as before) | conf.sh's own resolution of which document is in force |

**Not seams — Rust, against the same data:** the `open`/`publish`/`round-seq` records and
the landstate *reads* (a documented line format, §3); the queue lock (flock, same file);
`stats`; the publish-range derivation; the owner check (`queue_owner_refused`, four lines);
the actor rule; `spira_gate_outcome`; `queue_certified_list`; git, bd reads, forge.sh,
spira-lc, the harness scripts (subprocesses, §5). **The config writes** go through
spira-config's library: `spira_config::set_paths_in_file` (both keys, validated, atomic;
added here, the library form of `spira-config set`) and `spira_config::legacy_map::
set_row_in_file` (the repo-map row, moved into spira-config because nothing else may name
or write that file — config-fence.sh).

## 7. Cutover

**Not performed** (operator's directive: no bash, unit or workflow edits). Line numbers are
against this branch's base (7ce25b21b). `$SPIRA_QUEUE_BIN` is resolved in conf.sh like its
neighbours. Apply in one landing, in this order.

### 7.1 Resolve the binary

1. **`spira/conf.sh` after line 1541** (`: "${SPIRA_BATCHER_BIN:=$(spira_bin batcher 2>/dev/null)}"`), add:
   ```bash
   # THE MERGE QUEUE TOOL (queue crate): replaces spira/queue.sh.
   : "${SPIRA_QUEUE_BIN:=$(spira_bin queue 2>/dev/null)}"
   ```
   and add `SPIRA_QUEUE_BIN` to the key list beside `SPIRA_BATCHER_BIN` (conf.sh:102).

### 7.2 Callers

| # | file:line | current | replacement |
|---|---|---|---|
| 2 | `spira/landing.sh:1756` | `bash "$SPIRA_HOME/queue.sh" step "$repo_name" 2>&1 \` | `"$SPIRA_QUEUE_BIN" step "$repo_name" 2>&1 \` |
| 3 | `spira/landing.sh:1767` | same | same |
| 4 | `spira/landing.sh:1845` | same | same |
| 5 | `spira/spira-verdict.sh:17` | `bash "$HERE/queue.sh" step "$_name" \|\| true` | `"$SPIRA_QUEUE_BIN" step "$_name" \|\| true` |
| 6 | `spira/suites.sh:468` | `bash "$HERE/queue.sh" submit "$branch" >&2 \|\| {` | `"$SPIRA_QUEUE_BIN" submit "$branch" >&2 \|\| {` (and the message on :469 says `queue submit`) |
| 7 | `spira/rebase-stale.sh:142` | `gate_out="$(bash "$HERE/queue.sh" submit "$br" "$name" 2>&1)"` | `gate_out="$("$SPIRA_QUEUE_BIN" submit "$br" "$name" 2>&1)"` |
| 8 | `rebase-stale/src/seam.rs:182-198` `submit` | `Command::new("bash").arg("-c").arg(r#"bash "$0" submit "$1" "$2" 2>&1"#).arg(self.home.join("queue.sh"))…` | run `$SPIRA_QUEUE_BIN` (resolved like its other binaries) with args `submit <branch> <repo>`, stdout+stderr combined into one capture; same `(success, output)` |
| 9 | `batcher-cut/src/io.rs:930-944` `land_local` | `bash queue.sh land-local <repo> --head H --members id:tip,…` | `$SPIRA_QUEUE_BIN land-local <repo> --head H --members-file - --worktree <round worktree>` with the `id:tip` lines on **stdin**, keeping `SPIRA_QUEUE_LOCK_HELD=1`. The round worktree is the one testenv built in (testenv/DESIGN.md §5); `--worktree` is **required** once `$SPIRA_RELEASES/current` exists (§8 D2/D3). |
| 10 | `batcher-cut/src/io.rs:605-620` `bins_present` | `${SPIRA_BATCH_BINS_TARGET_DIR:-$run/cargo-target-bins}/<tree>/release` | the round worktree's `target/release`, same rule as `queue::ops::land::round_bins` (tree match + one executable) |
| 11 | `reconciler/src/main.rs:182` | `queue_sh: env::var("SPIRA_QUEUE_SH").unwrap_or_else(\|_\| format!("{}/queue.sh", spira_home))` | `queue_bin: env::var("SPIRA_QUEUE_BIN")…` (the binary) |
| 12 | `reconciler/src/main.rs:814-823` | `program: "bash", args: [queue_sh, "eject", id, "--reason", "reconciler: … needs rebase", repo]` | `program: cfg.queue_bin, args: ["eject", id, "--reason", "…needs rebase", repo]` — a rebase eject: cause `eject` (harness), which is right; no `--red` |
| 13 | operator notes `round.sh:207-209` | `SPIRA_BATCH_BINS_TARGET_DIR="$w/.runtime/spira/cargo-target-bins" bash "$H/spira/queue.sh" land-local spira --head "$head" --members "$_mem"` | `"$SPIRA_QUEUE_BIN" land-local spira --head "$head" --members-file "$M" --worktree "$w"` (the members file already holds one `id:tip` per line) |
| 14 | operator notes `round.sh:212-213` | `_bins="$w/.runtime/spira/cargo-target-bins/<tree>/release"` | `_bins="$w/target/release"` (as testenv/DESIGN.md §8 already lists) |
| 15 | operator notes `round.sh:262` | `exec bash "$H/spira/queue.sh" publish spira` | `exec "$SPIRA_QUEUE_BIN" publish spira` |
| 16 | `spira/chamber/czar.fayth:90` | `Bash(*queue.sh*)` | `Bash(*queue *)` |
| 17 | `spira/chamber/czar.md:40,90,107,154` | `queue.sh eject / abandon`, `queue.sh step`, "`queue.sh eject` already wrote landstate=RED", "`queue.sh` reachable" | `queue eject / abandon`, `queue step`, same sentence with `queue eject` (it now also records the cause: pass `--red` when the member broke a test), "`queue` reachable (`queue stats`)" |
| 18 | `spira/hooks/aeon-fence.sh:117-122` | matches `*"/queue.sh"*`, allows `*"/queue.sh stats"*` | also match the binary: `*"/queue.sh"*\|*"queue "*\|*"$SPIRA_QUEUE_BIN"*`, allow only `* stats` for either |
| 19 | `spira/watchtower.sh:1397` | `queue.sh abandon <repo> --reason "conflict"` | `queue abandon <repo> --reason "conflict"` |
| 20 | `spira/skew.sh:387` (and the comments at :10, :439, :471, :493) | `queue.sh land-local` / `queue.sh rollback-local` | `queue land-local` / `queue rollback-local` |
| 21 | `spira/repo-map.example:36,146`, `README.md:156-158` | `queue.sh protect`, `queue.sh publish`, `queue.sh to-forge` / `to-local` | `queue protect`, `queue publish`, `queue to-forge` / `to-local` |
| 22 | `spira/config-fence-allow:65` | `spira/queue.sh` | delete the line (the file is deleted; the fence list is shrink-only) |
| 23 | `spira/batch.sh:11,484`, `spira/verdict.sh:15,20,26,1315,1317`, `spira/landing.sh:33,36`, `spira/lib.sh:2007,4783,6681,6694`, `spira/gate.sh:390`, `spira/gate-lib.sh:39,67`, `spira/lc.sh:159`, `spira/attribute.sh:59`, `spira/build-tarball.sh:138`, `spira/publish-backlog.sh:85`, `spira/conf.sh:1540,1871,1873,1943`, `spira/spira-verdict.sh:4,8` | comments naming `queue.sh` | name `queue` (comments only; no behaviour) |
| 24 | `batcher-cut/src/main.rs:362,390`, `batcher-cut/src/io.rs:563,573,606,874,921-926`, `queue-watch/src/io.rs:71-73`, `lifecycle/src/delivery.rs:615` | comments/strings naming `queue.sh` (queue-watch's also misnames `_lc_publish_step`) | name `queue`; `lifecycle` keeps its actor string `"queue.sh land-local"` unless the lifecycle history's actor names are migrated too |
| 25 | `~/.config/spira/overrides/landstate-keep-unpublished.override` and its landing.sh hunk | keeps LANDED landstate until the forge has the tip, for publish | **retire** once this lands (publish no longer reads membership from landstate, §8 D4); the bead `sp-bauwt` closes with this landing |

### 7.3 Delete

26. `spira/queue.sh`.

### 7.4 Bash suites to retire or repoint

The per-decision cases are unit tests now (`cargo test -p queue`, 93 tests). What still
needs one end-to-end run each is the wiring: the real lib.sh seam against a real fixture
store, and the real forge seam.

| suite | action |
|---|---|
| `test-queue-ops.sh`, `test-queue-owner-refuse.sh`, `test-queue-owner-mail.sh`, `test-queue-open-batch.sh`, `test-queue-publish.sh`, `test-queue-transition.sh`, `test-queue-land-local.sh`, `test-land-local-release.sh`, `test-queue-protect.sh`, `test-queue-flush.sh` | **repoint** to `$SPIRA_QUEUE_BIN` and cut to one case per subcommand that proves the seam (a real landstate write, a real mail, a real forge stub call); the decision cases are covered by the unit tests named in §9. `test-queue-publish.sh` gains sp-bauwt's red: land two rounds while a publish PR is open, delete their landstate, settle, publish — the second PR carries the second round's members. `test-queue-transition.sh` gains the in-delivery refusal. `test-queue-land-local.sh`/`test-land-local-release.sh` pass `--worktree` and drop `SPIRA_BATCH_BINS_TARGET_DIR`. |
| `test-queue-submit.sh`, `test-withdrawn-suites-recert.sh`, `test-queue-step-eject-race.sh`, `test-czar-shadow.sh`, `test-skew-refresh.sh`, `test-batcher-cut.sh` case L | **repoint** the invocation only (`bash "$REPO/spira/queue.sh"` → `"$SPIRA_QUEUE_BIN"`); `test-czar-shadow` expects the cause `eject` on its unflagged ejects |
| `test-lifecycle-cutover.sh:540-612` (sources queue.sh for `_lc_cut_batch`/`_lc_eject_member`/`_lc_abandon_batch`) | **repoint** to `queue open-batch`/`eject`/`abandon` against a seeded record carrying `batch_id`, or **retire** those cases: the CAS shape is `ops::batch::lc_cas`, unit-tested (`eject_with_a_batch_id_ejects_on_spira_lc_too`, switch ON) |
| stubs: `test-certify.sh:52,352`, `test-aeon-gate-close-silent.sh:112-136`, `test-landing-order.sh:62`, `test-landing-starvation.sh:73`, `test-landing-cutover-round.sh:54`, `test-reconciler.sh:253-256,450` | stub `SPIRA_QUEUE_BIN` instead of writing a fake `queue.sh` (`SPIRA_QUEUE_SH` → `SPIRA_QUEUE_BIN` for the reconciler) |
| static: `test-timer-templates.sh:321`, `test-queue-flush.sh:77`, `test-guards.sh:159,162`, `test-ops-allowlist.sh:150`, `test-git-push-app.sh:177` | grep for the new call shapes (`"$SPIRA_QUEUE_BIN" step`, `queue eject`); `test-git-push-app` checks `spira_git_push` is used — it now is, through seam R12, not in queue.sh |

### 7.5 Interfaces to widen later (findings, not blockers)

27. **`forge.sh pr-comment <repo> <pr> <body>`** takes its body in argv. Add a stdin form
    (`pr-comment <repo> <pr> -`, body on stdin) and switch `real::RealForge::pr_comment` to
    it; until then queue sends one line of at most 200 bytes (§5).
28. **`spira-lc … --reason R`** (abandon-batch, eject-member) takes free text in argv. Add
    `--reason-file -`; until then queue sends one bounded line.
29. **`lc_returned <id> <reason>`** (lc.sh:163) ignores its reason and always sends
    `batch-ejected`. queue passes the reason anyway; a manual eject is not a batch
    ejection, and the lifecycle's `ReturnedReason` has no manual-eject variant yet.

### 7.6 Config: the dual write

While repo-map exists (until the config epic deletes it), `to-forge`/`to-local` write the
land row **twice**, both through spira-config's library: spira.toml first
(`set_paths_in_file`, mode + base together), then the repo-map row
(`legacy_map::set_row_in_file`); a repo-map failure restores spira.toml's previous row. The
post-write verification then reads back through lib.sh (`repo_land`/`spira_landref`), i.e.
through repo-map, which is what every bash reader sees. When the config epic removes
repo-map: `SPIRA_REPO_MAP` resolves to nothing (or a missing file), queue skips the second
write with no change, and `spira_config::legacy_map` and the agreement check's repo-map half
can be deleted.

**Live finding (read-only, 2026-09-29):** production's spira.toml says
`[repo.spira] mode = "queue", base = "origin/main"` while repo-map says
`queue.local | local/main`. The two already disagree, so `to-forge`/`to-local` refuse today
— in bash and in this binary alike — until they are reconciled by hand (the fix is in the
data, not the code; the refusal is the contract).

## 8. Decisions and deliberate changes

- **D1 — eject records its cause (tonight's ruling).** `bead_reopen`'s cause row is what
  spira-claim classifies (spira-claim/DESIGN.md §3, §7). An eject is `eject-red` when the
  member broke a test — `--red`, or a non-empty `--suites` (the suites it reddened) — and
  `eject` otherwise (a harness or rebase eject, e.g. the reconciler's "needs rebase").
  The CERTIFIED-withdrawal path passes it to `bead_reopen`, as queue.sh did with the fixed
  `eject`. The open-batch path, which queue.sh left with **no** cause row at all (it moved
  to `lc_returned` in sp-rlyl0, so a batch eject was invisible to spira-claim): with the
  switch OFF it goes back to its pre-sp-rlyl0 form, `bead_reopen <id> <cause> "" <suites>`,
  which writes the cause row itself; with the switch ON it writes the row with
  `_bump_write_event <id> reopen <cause>` beside the Returned event, without a bd reopen.
  Explicit rather than inferred from `--reason` prose, because a classifier on free text is
  the silent-valve class spira-claim's own design refuses.
- **D2 — land-local ships the round worktree's own `target/release`** (testenv's
  SPIRA_ARTIFACTS contract), named with `--worktree`, verified by tree
  (`<worktree> HEAD^{tree}` = `<head>^{tree}`) and by holding at least one executable. The
  tree-keyed `cargo-target-bins/<tree>/release` cache and `SPIRA_BATCH_BINS_TARGET_DIR` are
  gone. A named worktree that fails either check refuses before the ref moves. Those bytes
  become the release's `bin/` (§8 D13, `release build --bin-dir`), never a rebuild
  (law-deploy-the-tested-artifacts).
- **D3 — retired by D13** (it packaged a tarball with build-tarball.sh and activated it with
  activate.sh, reverting the ref on failure). What survives: with no `$SPIRA_RELEASES/current`
  nothing runs a release, the release step is skipped and `--worktree` is not required.
- **D4 — publish members come from the land commits (sp-bauwt, option b).** Every
  `spira: land <id>[ — title]` commit in `forge..local` is a member; the tip is the LANDED
  landstate tip when that tip is in the range, else the land merge's second parent, else the
  commit itself. A landing-pass prune can no longer empty a publish. A range with new
  commits but no land commit still refuses (message now names land commits).
- **D5 — transitions refuse while the repository has work in delivery**: an open batch
  record or a BATCHED landstate whose tip is a commit of this repository — and, only with
  `lifecycle_enforce` ON, an IN_DELIVERY lifecycle row whose tip is (spira-lc unreachable =
  loud refusal). With the switch OFF spira-lc is never asked, so it can never block a
  transition. Checked under the lock, after the mode re-read, before the final publish. The lock itself already excludes a round in flight (batcher-cut holds it).
- **D6 — land-mode writes go through spira-config** (law-config-through-the-cli-only):
  spira.toml via the library form of `spira-config set` (`set_paths_in_file`, both keys in
  one validated atomic write — the CLI takes one key per call, which would leave the
  document half-written between two calls), and repo-map via `spira_config::legacy_map`
  (moved there: config-fence.sh forbids any other crate from naming or writing it). Order
  changed: spira.toml first, then repo-map, with a restore of spira.toml on a repo-map
  failure (queue.sh wrote repo-map first and left the pair disagreeing on a toml failure).
  A missing repo-map file is skipped rather than refused (the config epic's end state).
  A repo-map with no row for the repository now refuses before writing (the awk printed the
  file unchanged and only the verification caught it).
- **D7 — records are written atomically** (`open`, `publish`, `round-seq`, the certified
  entry): temp + rename, so verdict.sh/batcher-cut/queue-watch never read half a record.
- **D8 — open-batch does not admit a bead whose status bd cannot report.** queue.sh's own
  comment said "an empty bd answer is unknown, never admit" and its code admitted it.
- **D9 — the eject `--suites` sidecar is written in the batch path too.** queue.sh's dry-run
  promised `would write suites=… to <id>.ejected` for a batched member and never did;
  gate.sh reads that sidecar to force the suites on recertification.
- **D10 — identifiers are validated before any argv** (§5); free text never reaches argv
  except the two bounded, one-line cases in §5. `--reason-file`/`--members-file` (stdin
  with `-`) are the payload-safe forms; the argv forms remain for today's callers.
- **D11 — retired by D13 (sp-gkfg1).** The checkout deploy — stage-and-swap of the running
  harness checkout, the binary install and `bin/` links (sp-ma9uh), the `conf.sh` smoke (seam
  R23) and `SPIRA_LAND_DEPLOY_ALLOW` — is deleted. The running system executes only a
  release, so nothing is ever edited, built or linked in a checkout.
- **D12 — land-local lands only a tree something certified (incident 2026-09-29 17:52Z).**
  Two branches each passed the gate alone; the Concierge merged local/main into one of them
  and landed that merge with `queue land-local` after only unit tests. No gate ever judged the
  merged tree, and local/main went base-red. A promise not to do that again is not a
  mechanism, so land-local now refuses it.
  1. **The certificate.** A certificate is a small key=value file,
     `${SPIRA_VERDICTS:-$SPIRA_RUN/verdicts}/trees/<repo>/<tree>`. It has one writer per
     source, and the format lives in `gate::cert`:
     - **the gate** writes `verdict=PASS source=gate` on every PASS (`pass`, `cached`,
       `syntax-only`) for **the merged tree `T` it judged** (gate/DESIGN.md "The merge"). When
       the landing ref is an ancestor of the branch, `T` is the branch's own tree, so it is
       also the tree a fast-forward lands. Otherwise `T` is the tree of
       `merge(landing ref, branch)`, which is the tree any clean merge of the same pair
       lands.
     - **the round** (batcher-cut `finish_local_round`) writes `verdict=GREEN source=round`
       for the tree of the head its full corpus just ran green on (`stabilize_round`). This is
       done immediately before it calls land-local. A round head is a merge of many members
       that no per-branch gate ever judged. The round's own full-corpus green is its
       certification (law-a-round-takes-certified-tips), and a certificate makes that green
       durable. A flag on the call would only be the caller's word.
  2. **The check, before the CAS** (after the fast-forward check and before the D2 binaries):
     resolve `<head>^{tree}` and read the certificate for `(repo, tree)`. It counts only
     when its own `tree=` and `repo=` equal the head's tree and repository, and it says
     either `verdict=PASS source=gate` or `verdict=GREEN source=round`. Anything else
     refuses, changes nothing and exits 1:
     `queue.sh land-local: no gate PASS or round GREEN for <head>'s tree <T> in <repo> —
     nothing certified what would land; run: bash $SPIRA_HOME/gate.sh <head-arg> <repo>, then
     retry (or SPIRA_LAND_UNGATED=<reason> to land it ungated, logged); refused, nothing
     changed`. "Anything else" covers no file, an unreadable file, a mismatched tree or repo,
     and any other verdict. An accepted certificate is named on stderr with its source, the
     branch or head it judged, and when.
  3. **Matching is by tree alone. The gate binary and harness hash are not part of it.** The
     verdict cache key (gate/DESIGN.md) hashes the gate binary, so a gate upgrade retires
     every cached key. A certificate records the `harness=` hash for the reader and is
     matched without it. The content is what was certified, and a newer gate does not make
     an old PASS of the same tree false. The certificate also carries no TTL: a tree is
     immutable. A cache entry written before the certificate existed has no `tree=` and
     cannot be matched, so such a tree is re-gated once (a cached PASS rewrites its
     certificate).
  4. **What does not count:** a PASS for any other tree. The pre-merge branch tree is the
     incident's exact case: sp-0tpcs's PASS was for its own tree, and the landed head was
     `merge(local/main, sp-0tpcs)`, a different tree. The same goes for a certificate for
     the tree in another repository, and for a round's `local-verdict` file (it records no
     tree).
  5. **The exit (law-a-refusal-names-its-exit):** `SPIRA_LAND_UNGATED=<reason>` lands without
     a certificate. The reason is bounded to one line and must not be empty; an empty value
     is the same as unset. The override is loud in four places: a `queue.sh land-local:
     UNGATED LANDING …` line on stderr; `ungated: <reason>` as every member's LANDED
     landstate reason (the record read later; re-written after `bead_close_on_land`, whose own
     `LANDED … Closed by landing pass` write would otherwise replace it); a `QUEUE UNGATED <at> repo=… head=… tree=…
     reason=…` line in `landing.log`; and the reason in the landing mail.
  **Rejected:** accepting a PASS for a commit instead of a tree. A merge of the same pair
  with a different message or date is the same content under another id. **Rejected:**
  scanning `verdicts/` for a key; the key is a hash that includes inputs land-local cannot
  know (the changed-file list, the bead, the ejected suites). **Rejected:** a
  `--certified-by-round` flag, for the reason in (1).
- **D13 — a landing publishes a release (sp-gkfg1; brain
  `wiki/projects/spira/designs/runtime-is-a-release-2026-09-29.md`).** Applies when the landing
  repository is the harness (`spira.home_repo`, the repository releases are made from — its
  name is the MANIFEST's `repo`) **and** a release is in force (`$SPIRA_RELEASES/current` is a
  symlink). Any other repository lands with no release step at all. With no release in force
  (before the cutover) the step is skipped with a loud line: `release step skipped: no release
  is in force (…/current is absent) — production does not run <head> until a release is
  activated`. The first activation is the cutover's (sp-6p20x), never a routine landing's.
  1. **Before the CAS:** `--worktree` is required and passes D2's tree check (refusal:
     `--worktree <round worktree> is required to land <repo>: its release ships the round's own
     tested build (law-deploy-the-tested-artifacts), never a rebuild`).
  2. **The landing is recorded first:** CAS, round-seq, archive ref, every member LANDED and
     closed — before the slow part, so a killed or failed deploy never loses the record.
  3. **Then, by name on the launcher's PATH, each with `SPIRA_DB` set** (verify's
     pre-activate store check needs it) and `--releases $SPIRA_RELEASES --run $SPIRA_RUN`:
     `release build <head> --repo <repo> --bin-dir <worktree>/target/release` (must answer
     `<head>`), `release verify <head>`, `release activate <head> --repo <repo> --landed-ref
     <landing ref>`. Activation re-renders the installed units against
     `spira-releases/<head>`, swaps `current`, daemon-reloads and restarts what changed; a
     standing hotfix is superseded only if `<head>` and the landing ref contain it, else
     activation refuses (the release crate's rule; land-local passes the landing ref).
  4. **A failure is a deploy fault, never a partial deploy:** `LAND DEPLOY FAILED for <head>:
     release <step> exited <rc>: <its last line> — current is untouched (still <sha>); <base> is
     at <head> and the landing stays recorded`, the same in the landing mail, and exit 1.
     Nothing is reverted: the ref is the record of what landed (D11's reasoning, kept).
  5. `rollback-local` re-activates the previous round's release as it stands (`release verify`
     → `release activate`, never rebuilt; refused when pruned or when no release is in force),
     then moves the ref back.

  **Why `--bin-dir` and not `release build`'s own cargo build:** the statute
  law-deploy-the-tested-artifacts (a rebuild is a different artifact from the one that
  passed). `release build --bin-dir` is the same build, MANIFEST and verify with the round's
  tested bytes as `bin/`; a hotfix or a hand-built release still compiles. It also keeps the
  landing fast: no cargo on the round's critical path.
  **Who installs new and retires old units: `systemd/install.sh`, not activation.** Which
  units a host runs is host policy (units.sh: `inotifywait` present, `SPIRA_DOLT_DATA`, the
  broker opt-in, the watcher manifest), and it already has one reader; a second in Rust would
  be two readers that drift (exactly how `render.py` came to exist). Activation keeps every
  installed unit on the release it switched to; a unit a release adds or retires is a design
  change whose bead names `install.sh` in its cutover steps. `render.py` stays for that reason:
  install.sh and unit-ensure.sh render through it.
  **Rejected:** building when no release is in force (a routine landing would do the
  cutover's first activation, or build a release nothing runs); keeping the tarball and
  `activate.sh` for local landings (two ways to make a release); reverting the ref on a
  failed activation (the members are already closed; a ref behind its own records is worse).
- **Kept deliberately:** every message's `queue.sh <cmd>:` prefix and text (operators and
  one override grep them); `step`'s status 0 on a non-queue.local repo whatever the cut
  returned (a bash `if` without `else`); `submit` falling back to `$SPIRA_REPO` when the map
  does not carry the repo; `rollback-local` activating before moving the ref (and reporting
  the split if the CAS then fails); the owner check's semantics and override.
- **Rejected:** reading the repository row from spira.toml directly (the other components
  read repo-map through lib.sh today, and the live pair disagrees — §7.6: queue alone
  switching sources would split the harness); inferring `eject-red` from the reason text;
  porting `queue_sort_rows`, `rebase_branch`, `format_batch` or the pre-flight gate (shared
  machinery with other callers — seams R13-R18); running `spira-config set` as a
  subprocess (two argv writes, not atomic as a pair).

## 9. Tests

`cargo test -p queue` — 121 unit tests; `cargo test -p spira-config` covers the two library
additions (`set_paths_in_file_writes_both_or_neither`, `legacy_map` row rewrite). Derived
from §2.2/§8:

| contract | tests (`queue/src/tests.rs` unless named) |
|---|---|
| submit: branch rule, red gate rc + CAUGHT, green certify + entry + GATE_COST, push, pr | `submit_*` (6) |
| protect/flush/step | `protect_writes_the_receipt_only_for_queue_forge`, `flush_refuses_without_a_batcher_and_forces_wait_zero_with_one`, `step_queue_local_publishes_with_stderr_folded_into_stdout`, `step_on_queue_forge_is_status_zero_whatever_the_cut_did` |
| eject: RED + cause + survivors + PR + mail; red cause + sidecar; withdrawal; stranger; owner; dry-run; spira-lc; lock; ident; czar | `eject_*` (10), `czar_fence_refusal_stops_eject_before_anything` |
| the lifecycle switch, both modes (§10) | OFF: `eject_member_with_lifecycle_off_reopens_through_bead_reopen_and_never_touches_spira_lc`, `eject_dry_run_writes_nothing`, `abandon_keeps_red_members_archives_and_audits`, `open_batch_assembles_admits_opens_and_marks_batched`, `land_local_without_a_release_in_force_lands_marks_and_archives`, `submit_green_in_a_queue_mode_certifies_and_writes_the_entry`, `publish_members_come_from_land_commits_even_when_landstate_was_reaped`, `transitions_refuse_while_work_is_in_delivery` (OFF case) — each asserts spira-lc was never invoked, not even probed; ON: `eject_member_marks_red_records_the_harness_cause_and_returns_survivors`, `eject_with_a_batch_id_ejects_on_spira_lc_too`, `abandon_with_lifecycle_on_abandons_the_batch_on_spira_lc`, `open_batch_with_lifecycle_on_cuts_on_spira_lc_and_records_the_batch_id`, `transitions_refuse_while_work_is_in_delivery` (ON cases), `lifecycle_on_with_spira_lc_unreachable_refuses_loudly_and_changes_nothing`; resolution: `the_environment_pins_the_switch_over_the_config` |
| abandon | `abandon_with_lifecycle_on_abandons_the_batch_on_spira_lc`, `abandon_requires_a_reason`, `abandon_keeps_red_members_archives_and_audits`, `abandon_dry_run_prints_the_audit_line_and_changes_nothing` |
| claim/release | `claim_and_release_hand_the_batch_back`, `claim_without_reason_is_usage`; `records::tests::claim_then_release_restores_the_owner` |
| open-batch | `open_batch_*` (5) |
| land-local: land + archive + members + cached divergence; ff refusal; base/mode; D2; lock-held; stdin members | `land_local_*` (8, each on a certified tree — `local_repo` writes the gate PASS) |
| land-local certification (D12): refuses an ungated tree; a gate PASS for the tree; a round GREEN for the tree; a PASS for another tree (the pre-merge branch) or another repo does not count; the override lands and records its reason (landstate, landing.log, stderr) | `land_local_refuses_a_tree_no_gate_or_round_certified`, `land_local_accepts_a_gate_pass_for_the_head_tree`, `land_local_accepts_a_round_green_for_the_head_tree`, `land_local_ignores_a_pass_for_another_tree_or_repo`, `land_local_ungated_override_lands_and_records_the_reason`, `land_local_a_pass_from_an_older_gate_binary_still_counts`; `gate::cert::tests` |
| land-local publishes a release (D13): build `--bin-dir` → verify → activate with `SPIRA_DB`, `--releases`, `--run`, `--landed-ref`, recorded before the release step; a build, verify or activate (hotfix) failure is a deploy fault that leaves current, keeps the landing and exits 1; a build answering another sha; no release in force skips it, loudly, with no worktree needed; the harness needs `--worktree` while a release is in force; another repository never runs it | `land_local_publishes_the_rounds_tested_build_as_a_release_and_activates_it`, `land_local_build_failure_leaves_current_alone_keeps_the_landing_and_reports_a_deploy_fault`, `land_local_verify_or_activate_failure_is_a_deploy_fault_too`, `land_local_over_a_standing_hotfix_is_refused_by_release_activate_and_reported`, `land_local_answer_for_another_commit_is_a_fault`, `land_local_with_no_release_in_force_skips_the_release_step_and_needs_no_worktree`, `land_local_of_the_harness_requires_the_round_worktree_while_a_release_is_in_force`, `land_local_of_another_repository_publishes_no_release`; `release::tests::build_with_a_bin_dir_ships_those_binaries_without_cargo_and_still_refuses_a_partial_set`; suite `test-land-local-release.sh` (the real queue and release binaries, a scratch releases dir, a mock systemctl) |
| publish: D4 (reaped landstate), landstate tip, nothing-to-publish, refusals | `publish_*` (4); `publish_range::tests` (6) |
| transitions: wait/verify refusal, happy path, red, timeout, D5 (4 sources + other repo), agreement, D6 restore, archive ancestry | `to_forge_*` (4), `to_local_*` (2), `transitions_*` (2) |
| rollback-local | `rollback_local_reactivates_the_previous_rounds_release_without_rebuilding` |
| stats, CLI, records, idents | `stats::tests`, `cli::tests`, `records::tests`, `ident::tests`, `model::tests`, `lock::tests` |
| the seam mechanism itself, for real through bash with a stand-in lib.sh | `seam::tests::values_travel_on_stdin_with_newlines_and_empties_intact`, `real::tests::context_seam_round_trips_through_bash`, `real::tests::answer_seams_ignore_log_lines_and_carry_failures` |

## 10. Lifecycle switch

**Finding (operator, 2026-09-29):** the lifecycle machine was never deployed on this host —
no `spira_lifecycle` database, no `spira_lc` grant (`spira-lc`: "Access denied"), no
service or socket. **Decision:** `lifecycle_enforce` is THE switch for everything that
touches the lifecycle machine.

**Resolution** (`ops::lifecycle_on`, the aeon crate's rule, `aeon/src/conf.rs`
`lifecycle_enforce`): the process environment's `SPIRA_LIFECYCLE_ENFORCE` wins (a unit's
`Environment=` or a fixture pins it; `1`/`true` = on, anything else = off); else the typed
`spira.lifecycle_enforce` in the document conf.sh resolved (`spira_toml_resolve`, seam R21),
read through the spira-config library; else **off**. Binary presence is never consulted —
a `spira-lc` on disk does not turn anything on.

**OFF (production today):** queue never invokes spira-lc — no `show-batch`, `create-bead`,
`cut`, `abandon-batch`, `eject-member`, `list`, and no `lc_returned` (lc.sh). Nothing that
depends on spira-lc can block: "cannot tell" does not exist in this mode. Behaviour is the
pre-lifecycle contract, recovered from history where a subcommand changed.

**ON:** spira-lc is authoritative. Every subcommand that would talk to it first probes it
(`spira-lc list --state IN_DELIVERY`, parsed); unreachable or not executable is a loud
refusal, exit 1, before the lock and before anything changes:
`queue.sh <cmd>: lifecycle_enforce is on and spira-lc is unreachable (<why>) — refused,
nothing changed; fix the lifecycle machine or turn lifecycle_enforce off`. A CAS refusal
from a reachable machine (an illegal transition) stays what it was in queue.sh: reported
on stderr, the queue-side action proceeds (the machine's own state is the record of it).

| subcommand | OFF | ON |
|---|---|---|
| `eject` (in the open batch) | pre-sp-rlyl0 (ca5672b3c^): `land_mark RED`, then `bead_reopen <id> <eject\|eject-red> "" <suites>` — reopen, submitted label off, assignee cleared, `.ejected` sidecar, cause row; comment, survivors CERTIFIED, PR closed, record removed, mail. Dry-run says "would reopen bead … and clear assignee". | probe; `land_mark RED`; sidecar; cause row (`_bump_write_event`); `lc_returned`; `release_claim`; `eject-member` CAS when the record carries `batch_id`; the rest as OFF. Dry-run says "would return bead … via a Returned event". |
| `eject` (CERTIFIED, not batched) | `bead_reopen` (unchanged in every era; sp-xtk2l has no lifecycle transition for it) | probe; same |
| `abandon` | pre-sp-o7nbr.5 (d6ecf08ca^): no `abandon-batch`; members returned by landstate, archive, audit, event, mail | probe; `abandon-batch` CAS when `batch_id`; the rest as OFF |
| `open-batch` | pre-sp-o7nbr.5: no `create-bead`/`cut`; the open record carries no `batch_id`/`version` | probe; `create-bead` per member, `cut`; `batch_id`/`version` appended on success |
| `to-forge`, `to-local` (D5) | in delivery = open batch record or BATCHED landstate of this repo; spira-lc never asked | probe; also every IN_DELIVERY row of this repo; a failed read refuses |
| `submit`, `protect`, `stats`, `flush`, `step`, `claim`, `release`, `land-local`, `publish`, `rollback-local` | no lifecycle call in any era | same — none. queue emits no `stack`, `land` or `settle` events: those belong to batcher-cut (`stack`/`land` of a round) and verdict.sh (`settle`), which read the same switch in their own rewrites |

`flush`/`step` run `batch.sh` and the batcher, which make their own lifecycle
calls: those components must honour the same switch (their own cutovers), and pass
`SPIRA_LIFECYCLE_ENFORCE` through the unit environment unchanged — queue does not set it.

**Cutover addition:** nothing to change in the units for OFF (the default resolution is
off and conf.sh already defaults `SPIRA_LIFECYCLE_ENFORCE=0`, conf.sh:1087). When the
lifecycle machine is deployed, turning it on is `spira-config set spira.lifecycle_enforce
true <doc>` — queue needs no change.
