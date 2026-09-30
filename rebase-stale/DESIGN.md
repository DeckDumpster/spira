# rebase-stale — design

Replaces `spira/rebase-stale.sh` (sp-oxwvc) and the resolver scripts it calls
(`spira/mech-resolve.sh`, `spira/keylist-union.py`, `spira/comment-union.py`) with one Rust
binary. Bead: sp-x1c6k.

## Intent

A submitted branch that went stale against its landing ref is rebased **mechanically**. Only
a genuine content conflict or a red gate returns it to an aeon. Most "returned for rebase"
branches were never contested; they waited for their round while the base moved, and an aeon
session (tens of minutes, a fleet slot) was spent re-deriving a rebase a program can do.

The defect that motivated the rewrite (sp-x1c6k): the bash refused 7 of 8 real beads on
2026-09-29 as "checked out in a live worktree". A finished aeon submits and exits but leaves
its worktree checked out on `spira/<id>`, and "checked out anywhere" was read as "live".

## Contract

### Invocation

```
rebase-stale <bead-id> [repo-name]
```

* `repo-name` defaults to the home repository (`spira_home_repo`).
* Called today by exactly one site: `batcher-cut/src/io.rs::rebase_stale` (from
  `handle_base_conflicts` in `batcher-cut/src/main.rs`), as `bash $SPIRA_HOME/rebase-stale.sh
  <id> <repo>`, inheriting the conf.sh-exported environment of the batch pass. Also run by
  hand by the Concierge.

### Exit codes (unchanged; confirmed against `batcher-cut/src/io.rs:128-147`)

| code | meaning | caller's reading |
|---|---|---|
| 0 | nothing to do (already contains the land ref), or rebased (clean or mechanical) and re-certified | done |
| 1 | a real content conflict — bead reopened with the conflicting hunks quoted, branch untouched | done (script did the bookkeeping) |
| 2 | rebased but the gate was red at the new tip — branch restored, bead reopened with gate output quoted | done |
| 3 | not attempted: unknown repo/branch/landref, branch held by a LIVE holder, a leftover worktree that is dirty or outside the sanctioned root, lock timeout, scratch-tree failure | caller runs its own fallback (reopen for rebase) |

Any other code (a crash) is folded into 3 by the caller.

### Standard output / error

* stdout on 0: `rebase-stale: spira/<id> already contains <landref> — nothing to do` or
  `rebase-stale: spira/<id> rebased (<clean|mechanical>) and certified at <sha>`.
* stderr carries a one-line reason on 1/2/3.

### The log (`SPIRA_REBASE_STALE_LOG`, default `$SPIRA_RUN/rebase-stale.log`)

Exactly one line appended per call (append, never rewritten), byte-identical in shape to the
bash:

```
REBASE_STALE <UTC ts %Y-%m-%dT%H:%M:%SZ> id=<id> repo=<name> outcome=<outcome> reason=<reason>
```

`outcome` ∈ `current | busy | error | conflict | gate-red | clean | mechanical`. A call that
exits before the repo resolves (bad args) writes no line, as before.

### Side effects (all through lib.sh, the same functions the bash called)

| outcome | effects |
|---|---|
| conflict (1) | `bump_requeue <id> merge-conflict`; `bead_reopen <id> rebase-conflict <note>` (note quotes the conflicting hunks); `land_mark <id> RED <old-tip> no-rebase@<landref-sha>` |
| gate-red (2) | branch restored to old tip; `bead_reopen <id> rebase-gate-red <note>` (quotes gate output); `land_mark <id> RED <old-tip> no-rebase@<landref-sha>` |
| clean/mechanical (0) | branch moved to new tip; `queue.sh submit spira/<id> <repo>` (fences + gate; writes CERTIFIED itself); `bdq note <id> "rebase-stale: rebased … No aeon session used."` |

Payloads (notes, hunks, gate output) go to the lib.sh seam **on stdin**, never argv/env
(law-payloads-go-on-stdin). Short identifiers (id, cause, state, tip) are argv.

`queue.sh submit` is called as a subprocess exactly as today (not re-implemented): it runs the
gate, and on green writes the landstate/queue entry itself.

### Files and stores

* reads: the repository (`git`), `$SPIRA_RUN/hold-<id>.pid`, `$SPIRA_RUN/aeon-*-<id>.pid`,
  `/proc/<pid>/{cmdline,cwd}`, the bead store (status only, `bd show --json`).
* writes: `refs/heads/spira/<id>` (CAS via `update-ref <new> <old>`), the scratch worktree
  `$SPIRA_RUN/worktree/.rebase-stale.<repo-basename>` (detached HEAD only), the lock
  `$SPIRA_RUN/rebase-stale.<repo-basename>.lock`, the log, `$CARGO_TARGET_DIR` for
  regeneration (`$SPIRA_RUN/rebase-stale.target`).
* a leftover worktree that holds the branch is **synced** (`reset --hard <new-tip>`) after
  the branch moves — never removed.

### Configuration

| value | source |
|---|---|
| `SPIRA_HOME` (lib.sh, queue.sh) | environment (conf.sh exports it; required) |
| repo path, land ref, home repo | lib.sh `repo_root` / `spira_landref` / `spira_home_repo` — the repo-map resolvers every queue caller uses, the same seam `batcher-cut::find_repo` uses; never a second parser |
| `SPIRA_RUN`, `SPIRA_REBASE_STALE_LOG`, `SPIRA_DB`, `SPIRA_BD`, `SPIRA_GIT_NAME`, `SPIRA_GIT_EMAIL` | environment (conf.sh exports), falling back to the `spira-config` library (`[spira] run / rebase_stale_log / db / bd / git_name / git_email`) when unset |

## Liveness: live holder vs leftover worktree

The holder of `spira/<id>` is read from `git worktree list --porcelain`. Classification
(`Holder`):

* **None** — no worktree holds the branch, or its registration points at a directory that no
  longer exists. Rebase in the scratch tree.
* **Live** — any witness says somebody is home. Exit 3, `outcome=busy`, branch untouched,
  worktree untouched. Witnesses, any one suffices (fail closed):
  1. `hold-<id>.pid` names a live pid (hold.sh / unhold.sh protocol);
  2. `aeon-*-<id>.pid` names a live pid whose cmdline contains `aeon.sh` (the aeon's unit
     name `spira-aeon-<fayth>-<epoch>` does not carry the bead, so its pidfile is the
     per-bead witness of "the bead's aeon unit is active");
  3. the bead store says `in_progress` (a lease not released);
  4. the bead store could not be read, proven by a positive control (`bd list --limit 1`
     answers with a row): an unreachable DB reads as live (law-absence-needs-a-positive-control). Once
     the control passes, a bead `bd` cannot show reads as status "" (not in_progress), as
     `spira_bead_status` does;
  5. any process's cwd is inside the worktree (a hand session or tool working there).
  Witnesses are also checked for the worktree's own basename when it differs from `<id>`
  (sp-87csm: a child's directory can hold a parent's branch).
  Unlike lib.sh's `spira_holder_witnesses`, a stale `in_progress` is **not** relaxed by a
  spira-lc row: this tool never removes anything, so the conservative answer only costs a
  fallback reopen.
* **Leftover** — no witness, directory under `$SPIRA_RUN/worktree/`, `git status --porcelain`
  empty, no rebase/merge in progress there. Rebase in the scratch tree; afterwards sync the
  leftover with `reset --hard <new-tip>` (and back to `<old-tip>` on a red gate).
* **Dirty** — a leftover with uncommitted or untracked changes, or whose status is
  unreadable, or mid-rebase/merge. Exit 3 (`busy`, reason names it); never touched.
* **Foreign** — held outside `$SPIRA_RUN/worktree/` (an operator's checkout). Exit 3.

Liveness is re-checked immediately before the leftover is synced.

### How the one-aeon-one-worktree / collision guarantee holds

Round 123 ejected the earlier bash version because `aeon.sh` removed its own worktree at
teardown, which broke `test-aeon-worktree-collision`'s setup ("bead A got its own worktree").
This design keeps that guarantee by construction:

1. **aeon.sh is not changed.** A finished aeon's worktree stays registered exactly as the
   collision fixture requires.
2. **This tool never creates a worktree named after a bead, never removes any worktree, and
   never checks out `spira/<id>`.** All rebase work happens on a **detached HEAD** in one
   private scratch tree (`.rebase-stale.<repo>`, not a bead id), then the branch ref moves by
   compare-and-swap `update-ref`. So no second worktree ever holds a bead's branch, and
   `git worktree add` in aeon.sh can never be refused because of us.
3. The only touch to another worktree is `reset --hard` of a verified-clean, verified-not-live
   leftover **already on that branch** — its HEAD follows its own branch, which is what
   "checked out on spira/<id>" means. Its registration, path and branch are unchanged.
4. Deletion stays in lib.sh's one sanctioned site (`spira_destroy_worktree`); this binary
   calls no deleter, so the DESTRUCTION section's single-site rule still holds.

## Mechanical resolution

The rebase runs with `-c merge.conflictStyle=merge`. At each stop every unmerged path is
classified by a `ResolveRule` table (path → `ResolveKind`); if **any** path is not
mechanical, the rebase is aborted and the whole stop is reported as a conflict. During a
rebase stage `:2` ("ours") is the onto side (land ref plus already-replayed commits) and `:3`
("theirs") is the commit being replayed; unions keep ours first.

| kind | paths | rule |
|---|---|---|
| `LineSetUnion` | `.gitignore`, `spira-config/schema/spira-key-history.txt` | append-only: stage 2 lines in order, then stage-3 lines not already present (non-blank). Either stage missing (modify/delete) → not mechanical. |
| `KeyListUnion` | `spira/conf.sh` | each conflict region must be pure `SPIRA_*`/`COCKPIT_*` key lists on both sides; union ours then theirs' new keys appended to ours' last line; result must pass `bash -n`. |
| `Regenerate` | `spira-config/schema/spira.schema.json` → stdout of `cargo run -q --bin spira-config -- schema`; `Cargo.lock` → stage 2 then `cargo metadata --format-version 1 --offline` (then online) which adds only missing entries | derived files are regenerated, never merged; run after all other paths are resolved. `cargo generate-lockfile` is deliberately **not** used: it re-resolves every dependency. |
| `CommentUnion` | any other path whose comment syntax is known | every line of every region on both sides must be a comment or blank under that file's syntax; union ours then theirs' lines not already present. |
| real conflict | anything else | returned with hunks quoted |

Comment syntax by path (`CommentSyntax`): `.rs`/`.js`/`.ts`/`.c`/`.h`/`.go` → `//`;
`.sh`/`.bash`/`.py`/`.toml`/`.yml`/`.yaml`/`.conf`/`.cfg`/`Makefile`/`.gitignore`/extensionless
with a shell/python shebang → `#`; `.sql`/`.lua` → `--`. Markdown and unknown types have none
(prose is content). Deliberately narrower than `comment-union.py`, which counted `#[derive]`
in Rust and `* item` in Markdown as comments.

After resolving a stop: `git add` each path, `rebase --continue` (`GIT_EDITOR=true`); a stop
whose resolution is empty is `--skip`ped. A rebase that ends without the land ref as an
ancestor of HEAD is a failure ("refused").

Conflict report: for each non-mechanical path, the marker regions (`<<<<<<<` … `>>>>>>>`, at
most 60 lines per file) plus the reason it was not mechanical.

## Schema (Rust types, serde)

* `Outcome` — `current|busy|error|conflict|gate-red|clean|mechanical` (kebab-case).
* `LogRecord { ts, id, repo, outcome: Outcome, reason }` — `Display` renders the log line.
* `Exit` — `Ok=0, Conflict=1, GateRed=2, NotAttempted=3`.
* `Holder` — `None | Leftover{path} | Live{path, witness} | Dirty{path, why} | Foreign{path}`.
* `ResolveKind` — `LineSetUnion | KeyListUnion | Regenerate(Regen) | CommentUnion`;
  `Regen { argv, stdout_to: bool, seed_from_ours: bool, fallback_argv }`.
* `ResolveRule { path, kind }`; `CommentSyntax { prefixes }`.
* `Hunk { ours: Vec<String>, theirs: Vec<String> }`; `ConflictFile { path, reason, quoted }`.
* `BeadStatus` — `Known(String) | Unreachable` (the status witness's answer).
* `Config { home, run, log, db, bd, git_name, git_email, lock_wait_secs, target_dir }`.
* `Seam` trait — the impure bead-store/queue boundary: `repo`, `home_repo`, `bead_status`,
  `reopen`, `bump_requeue`, `land_mark`, `note`, `submit`. `LibSeam` shells to lib.sh/queue.sh;
  tests use a recording fake. git is always real.

## Tests (unit, `cargo test -p rebase-stale`, temp git repos)

Derived from the contract: stale only by appended key-history rebases mechanically with no
aeon (exit 0, both lines survive, submit called once and sees the rebased tip, log
`mechanical`); a real same-line conflict exits 1 with the hunk quoted, branch untouched,
requeue bumped, RED marked; a stop mixing a mechanical and a real conflict resolves nothing;
a red gate exits 2 and restores (a leftover follows back to the old tip); a finished
session's leftover worktree does not block and is synced to the new tip; a live holder (hold
pidfile, live aeon pidfile, in_progress, unreachable DB, a process working in it) and a dirty
leftover exit 3 untouched; a dead aeon pidfile does not block; a foreign checkout is refused;
a comment-only conflict unions; a derived file is regenerated; already-current is a no-op;
the scratch tree survives reuse and a dangling registration; the collision invariant (no
worktree gains or loses a registration, the scratch tree stays detached); `LibSeam` passes
payloads on stdin and refuses to read absence without its positive control; resolver and
log-format unit tests. The leftover tests were seen red against the old rule ("registered
anywhere = live") by mutation.

## Cutover (operator's decision — not made here)

1. `batcher-cut/src/io.rs:138-139` —
   current: `let mut cmd = Command::new("bash"); cmd.arg(env.home.join("rebase-stale.sh")).arg(id).arg(repo_name);`
   replacement: `let bin = env::var_os("SPIRA_REBASE_STALE_BIN").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("rebase-stale")); let mut cmd = Command::new(bin); cmd.arg(id).arg(repo_name);`
   (plus `SPIRA_REBASE_STALE_BIN` in conf.sh's exported key list and the install manifest).
2. Delete `spira/rebase-stale.sh`, `spira/mech-resolve.sh`, `spira/keylist-union.py`,
   `spira/comment-union.py` (no other caller: `grep -rn 'mech-resolve\|keylist-union\|comment-union' spira/` shows only rebase-stale.sh and the suite).
3. Delete `spira/test-rebase-stale.sh` (superseded by this crate's unit tests; its case 4
   asserts the leftover worktree is *removed*, which this design deliberately does not do).
4. `spira/conf.sh:1231-1234` — keep `SPIRA_REBASE_STALE_LOG`; update the comment to name the
   binary.
5. The two bash commits already on `spira/sp-x1c6k` (3f3596721, 29c4ecae1: rebase-stale.sh
   routed through `spira_destroy_worktree`, and its test) become moot once (2)/(3) are applied.
