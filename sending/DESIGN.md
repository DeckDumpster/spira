# sending — DESIGN

The Sending, as a binary (sp-arpjt; `spira/sending.sh` until then). Rewrite wave 2.

## 1. Intent

Send finished work out properly: **delete the branch and the worktree of every bead whose
work has landed, and nothing else.** Every aeon leaves a branch and a worktree behind; git
refuses to delete a branch a worktree holds, and a cleanup that does not verify its own
effect reports success on the strength of having tried (the re-land-forever loop the shell
header documented). So every deletion is lib.sh's verified chokepoint, and every branch gets
exactly one disposition, decided by reading only.

**Only content decides.** A branch may be deleted when the base already holds every change
it makes — an ancestor, or a branch whose three-way merge into the base is the base's own
tree — never on a tip comparison and never on the bead's status
(law-closed-is-not-landed). An unreachable remote leaves a stale base, which reads as
unlanded and KEEPS the branch: the failure biases toward hoarding, which is recoverable.

## 2. Contract

```
sending [--dry-run] [--no-fetch] [--status-from <file>] [--skip-queue|--queue-only] [<bead-id|branch>]
```

| exit | meaning |
|---|---|
| 0 | nothing failed |
| 1 | a deletion failed (a `FAILED` line names it) |
| 2 | usage error, or the harness context could not be read — **nothing was judged** |

**Callers.** The sentinel's CHECK 6b runs `sending --skip-queue` by bare name and parses
`^SENT <id> <repo> <branch>` (act) and `^FAILED` (log); the daily
`spira-straggler-sweep.service` runs `bin/sending --queue-only`; an operator runs
`sending --dry-run` or `sending <bead-id>` by hand (watchtower's escalations say so).

**Scope.** Every repository `spira_repos` names; `--skip-queue` drops queue/queue.local
repositories (their landed members are reaped at landing by `spira-lc close-on-land`),
`--queue-only` keeps only those. A flag partitions repositories, never a repository's
branches. A repository with no path, no checkout, or an unresolvable land ref is a `SKIP`
line — loudly, never swept against a ref that does not exist.

**PASS 1 — one disposition per `refs/heads/spira/*` branch.** `round-*` branches are
skipped (no bead). A branch the holder witnesses call occupied is `HELD` before anything is
computed. Otherwise, in order (the forge is asked only after every local check said no):

| disposition | when | action |
|---|---|---|
| SEND content-landed | `content_on_base(br, base)` | evidence (below), verified reap, close-on-land |
| KEEP superseded-unsafe | a `supersedes` edge, commits of its own, merges cleanly | keep the branch, free its worktree |
| REAP superseded-safe | a `supersedes` edge, and conflicts (or carries nothing) | verified reap |
| REAP squash-merged | closed/submitted, a MERGED PR whose head is the branch tip | verified reap, close-on-land |
| SEND ff / REAP non-code-delivers / REAP open-zero-ahead | zero ahead and an ancestor | unreachable in practice (content_on_base answered first); kept for a base that moves between reads |
| KEEP cherry-unapplied | a landing record names the bead but `git cherry` finds an unapplied commit | keep |
| SEND other-pr | a landing record names the bead and every commit is patch-equivalent upstream | verified reap, close-on-land |
| KEEP unlanded | a bead exists | keep |
| ORPHAN no-bead | no bead, real commits | write `refs/archive/<br>`, read it back, then reap |

**Evidence before a content-landed delete** (only when the branch carried commits):
`spira-lc content-on-base <id> merge-tree:<base-sha> sending`. The off-mode
`content-landed` label went with the off mode (sp-v62vn).

**PASS 2 — orphaned worktrees** (skipped for a one-bead run): a registered worktree under
`$SPIRA_RUN/worktree/`, not a dot-named harness tree, whose branch ref is gone, and whose
holder witnesses say nobody is home, is removed through `spira_destroy_worktree`; then
`spira_prune_worktrees`. `--dry-run` says `WOULD …` for every action and changes nothing.

**`sending reap-terminal [--dry-run]`** (hourly, `spira-reap-terminal.timer`) is the reaper for
the directory itself, independent of branch dispositions. State comes from one `spira-lc
list`; an empty or failed listing refuses and deletes nothing. For every entry under
`$SPIRA_RUN/worktree/` (dot-named trees are never touched):

| entry | action |
|---|---|
| a row in a terminal state (DONE, LANDED, DROPPED, SUPERSEDED) | remove the worktree through `spira_destroy_worktree` (held and dirty trees refuse or salvage there) |
| a row in any other state | keep |
| no row, older than `SPIRA_SCRATCH_MAX_AGE_HOURS` | scratch: remove |
| no row, younger | keep |

A checkout whose HEAD holds commits that neither a land ref nor any branch or archive ref
reaches is kept either way, as is one whose land ref or HEAD cannot be read. Attached to its
own branch, a worktree holds nothing unsaved: the branch keeps the commits. Branches are
`sending --all`'s business, not this verb's. `sending` with no arguments prints usage and
exits 2; the full sweep is `sending --all`.

**Output** is the shell's, line for line (`SENT`, `REAPED`, `ARCHIVED`, `KEEP`, `HELD`,
`SKIP`, `WOULD`, `FAILED`, lib.sh `log` lines), ending `… spira: sending: <n> sent,
<m> failed` unless dry. The wire token is `SENT` (test-wire-token.sh).

## 3. Structure

- `sweep.rs` — the dispositions and both passes, over a `World` (ports.rs).
- `git.rs` — every git question, read-only except `fetch` and the archive `update-ref`:
  `content_on_base`, `landed` (a landing record: `spira: land <id>` or `<id>:` subjects on
  the land refs), `cherry`, worktree porcelain.
- `real.rs` / `seam.rs` — the world: lib.sh through the seam, `gh` for the one forge
  question, `spira-lc` by bare name.

## 4. The lib.sh seam

`bash -s sending.seam.sh` reading a fixed script from stdin followed by NUL-terminated
values (the landing-pass construction, law-payloads-go-on-stdin). The first two values are
`$SPIRA_HOME` and the `--status-from` file, loaded into `spira_status_seam` before every
operation, so a suite's status map governs every bd question the chokepoint still asks
through bash — as it did when one bash process ran the whole sweep. **Since sp-9envm
(wave 4.20) the ops are only what has not moved to Rust**: context (settings and
repositories, family U), base (`spira_landref`/`spira_landrefs`/`ref_remote`, family W),
bead (`bdjson show`, family A/B), status (`spira_db_reachable` + `spira_bead_status` in one
call — the one bd question `reap::holder_witnesses` cannot answer itself), close-on-land
(`spira-lc close-on-land`, family R), label-add/label-remove (`bdq label add|remove`, family
A). Witness, send, destroy-worktree and prune are gone from this seam: `World for Real`
calls `reap.rs` in-process for all four now. lib.sh's own output (`log` lines) is still
passed through in order for the ops that remain.

## 5. Decisions

- **A crate of its own, not a verb of spira-lc.** The bead named the lifecycle crates as
  candidate homes; the Sending's domain is branch and worktree reaping through lib.sh's
  chokepoints and git. spira-lc is the lifecycle machine's client and must not grow a git
  and lib.sh dependency; the Sending calls it for one event, by bare name.
- **Decisions in Rust, destruction in lib.sh — superseded by sp-9envm (wave 4.20).** This
  decision held that porting the verified deletion a second time would recreate the
  six-site problem lib.sh's DESTRUCTION section exists to end, and said it moves "when
  lib.sh does." lib.sh's DESTRUCTION section (`spira_destroy_worktree`, `spira_destroy_branch`,
  `spira_reap_landed_branch`, `spira_prune_worktrees`, `salvage`, `hold_alive`,
  `holder_alive`, `spira_holder_witnesses`, `worktree_of`, `spira_caller`, `spira_reaplog`)
  is now this crate's `reap.rs`, called in-process by `World for Real`'s own
  `witness`/`send`/`destroy_worktree`/`prune` — no bash seam call for any of them any more.
  Those lib.sh functions are now one-line shims calling a `sending` CLI
  (`destroy-worktree`/`destroy-branch`/`reap-landed-branch`/`prune`/`salvage`/`witness`,
  plus `hold-alive`/`holder-alive`/`worktree-of`/`caller`/`reaplog`), so the 50-odd bash
  sourcers keep working unchanged, per wave4-decomposition.md's "shims, not a big bang."
  **What did not move**: `spira_landref`/`ref_remote` (family W) and
  `bdjson`/`bdq`/`spira_bead_status`/`spira_db_reachable`/`spira_status_seam` (families
  A/B) are still bash — `reap.rs` reaches them through the EXISTING `Base`/`Status`(new,
  replacing `Witness`/`Send`/`DestroyWorktree`/`Prune`)/`Bead` seam ops, parameterized via a
  small `BdProbe` trait rather than hand-rolled a second time. **One wrinkle this forced**:
  `spira_status_seam` now also materializes a real FILE (`$SPIRA_STATUS_FILE`) alongside its
  bash array, because the `sending` binary has no bash array to read; every shim threads it
  through as `--status-from`. **Other crates' own bash seams** (aeon, gate, landing-pass,
  spira-world's `slay`) that call these now-shimmed lib.sh functions by name keep working
  unchanged too — cutting them over to call `sending::reap` in-process is explicitly a
  follow-up bead (wave4-decomposition.md's plan: "the Rust seams of the crate that owns the
  family switch to in-process calls [in this bead]; other crates' seams switch in follow-up
  beads"). `held.sh --drop-empty` is the one bash caller that changed: it used to delete
  empty branches with a raw `git branch -D` under `SPIRA_REF_SANCTIONED=1`, bypassing the
  chokepoint entirely; it now calls `spira_destroy_branch` (the shim) so an empty branch
  still gets the holder-witness and reap-log treatment every other deletion gets.
- **Fails closed on its context.** The shell sourced lib.sh and carried on if that failed,
  sweeping nothing and exiting 0 — a clean-looking pass. The binary exits 2 when the
  context seam fails or names no repository.
- **Dropped: the legacy-tree retirement** (`$SPIRA_RUN/worktree/.landing` and `.rebase`,
  unsuffixed). Those trees were superseded by the per-repository `.landing.<repo>` /
  `.rebase.<repo>` long ago; none exists in production (checked 2026-09-30). An accreted
  one-time migration.
- **Dropped: the source guard.** sending.sh could be sourced by a suite to call
  `send_disposition`/`send_branch` directly; those are unit tests now
  (`every_branch_gets_the_shells_disposition`, `mid_send_hold_queue_and_failure`).
- **Kept as it was:** a HELD or CERTIFIED/BATCHED answer at the mid-send recheck still
  lets `spira-lc close-on-land` run for a landed arm (the work did land; only the ref stays),
  and the content-landed evidence is written before a delete that may then be refused.
- **Intended differences, named:** (1) OFF mode writes the `content-landed` label (§2);
  (2) the reap log's caller chain names `sending.seam.sh` where it named `sending.sh`;
  (3) exit 2 on an unreadable context (above); (4) the two repository SKIP lines say "no
  path is configured for it" / "configure its `base`" (config-fence: only spira-config names
  the config files).

## 6. Parity (sp-arpjt)

Old `spira/sending.sh` (local/main) and this binary, on identical deterministic fixtures
with every disposition planted (test-sending.sh's, plus a submitted bead closed on land and
a CERTIFIED branch the chokepoint refuses), same lib.sh, same stub bd and gh: stdout
identical after timestamp normalization, refs/remote refs/worktree registrations
identical, exit identical, for a full pass, `--dry-run`, and a one-bead run, against both
the old and the new lib.sh. The only differences are the intended ones in §5.

**sp-9envm's own intended differences**, same category as §5's existing two (the caller
chain naming `sending.seam.sh`/`sending.sh`): the reap log's `[by …]` chain now has one
MORE hop when a bash caller reaches the chokepoint through a shim (`… -> sending`, where
the bash process that ran `spira_destroy_worktree` used to be the innermost name) and one
FEWER when `sending`'s own sweep reaps a branch in-process (no `sending.seam.sh` hop at
all). No suite asserts the chain's content (checked: `git grep '\[by ' spira/test-*.sh`
finds nothing), only the REFUSED/REMOVED/FAILED/SALVAGE lines before it, which are
byte-for-byte unchanged.
