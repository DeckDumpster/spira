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
repositories (their landed members are reaped at landing by `bead_close_on_land`),
`--queue-only` keeps only those. A flag partitions repositories, never a repository's
branches. A repository with no path, no checkout, or an unresolvable land ref is a `SKIP`
line — loudly, never swept against a ref that does not exist.

**PASS 1 — one disposition per `refs/heads/spira/*` branch.** `round-*` branches are
skipped (no bead). A branch the holder witnesses call occupied is `HELD` before anything is
computed. Otherwise, in order (the forge is asked only after every local check said no):

| disposition | when | action |
|---|---|---|
| SEND content-landed | `content_landed(br, base)` | evidence (below), verified reap, close-on-land |
| KEEP superseded-unsafe | a `supersedes` edge, commits of its own, merges cleanly | keep the branch, free its worktree |
| REAP superseded-safe | a `supersedes` edge, and conflicts (or carries nothing) | verified reap |
| REAP squash-merged | closed/submitted, a MERGED PR whose head is the branch tip | verified reap, close-on-land |
| SEND ff / REAP non-code-delivers / REAP open-zero-ahead | zero ahead and an ancestor | unreachable in practice (content_landed answered first); kept for a base that moves between reads |
| KEEP cherry-unapplied | a landing record names the bead but `git cherry` finds an unapplied commit | keep |
| SEND other-pr | a landing record names the bead and every commit is patch-equivalent upstream | verified reap, close-on-land |
| KEEP unlanded | a bead exists | keep |
| ORPHAN no-bead | no bead, real commits | write `refs/archive/<br>`, read it back, then reap |

**Evidence before a content-landed delete** (only when the branch carried commits):
`lifecycle_enforce` ON → `spira-lc content-on-base <id> merge-tree:<base-sha> sending`;
OFF → `bd label add <id> content-landed`, the exemption CHECK 5 still reads. This closes
the OFF gap sentinel/DESIGN.md §2.9 recorded (cutover row 39): `dc3e364bf` had made the
shell write only the lifecycle event, a no-op with the switch off.

**PASS 2 — orphaned worktrees** (skipped for a one-bead run): a registered worktree under
`$SPIRA_RUN/worktree/`, not a dot-named harness tree, whose branch ref is gone, and whose
holder witnesses say nobody is home, is removed through `spira_destroy_worktree`; then
`spira_prune_worktrees`. `--dry-run` says `WOULD …` for every action and changes nothing.

**Output** is the shell's, line for line (`SENT`, `REAPED`, `ARCHIVED`, `KEEP`, `HELD`,
`SKIP`, `WOULD`, `FAILED`, lib.sh `log` lines), ending `… spira: sending: <n> sent,
<m> failed` unless dry. The wire token is `SENT` (test-wire-token.sh).

## 3. Structure

- `sweep.rs` — the dispositions and both passes, over a `World` (ports.rs).
- `git.rs` — every git question, read-only except `fetch` and the archive `update-ref`:
  `content_landed`, `landed` (a landing record: `spira: land <id>` or `<id>:` subjects on
  the land refs), `cherry`, worktree porcelain.
- `real.rs` / `seam.rs` — the world: lib.sh through the seam, `gh` for the one forge
  question, `spira-lc` by bare name.

## 4. The lib.sh seam

`bash -s sending.seam.sh` reading a fixed script from stdin followed by NUL-terminated
values (the landing-pass construction, law-payloads-go-on-stdin). The first two values are
`$SPIRA_HOME` and the `--status-from` file, loaded into `spira_status_seam` before every
operation, so a suite's status map governs every witness a destruction asks — as it did
when one bash process ran the whole sweep. The ops are the chokepoints only: context
(settings and repositories), base (`spira_landref`/`spira_landrefs`/`ref_remote`),
witness (`spira_holder_witnesses`), bead (`bdjson show`), send (the mid-send witness
recheck **and** `spira_reap_landed_branch`, in one process so nothing can claim the bead
between them), close-on-land, destroy-worktree, prune, label-add. lib.sh's own output
(`log` lines, salvage notes) is passed through in order.

## 5. Decisions

- **A crate of its own, not a verb of spira-lc.** The bead named the lifecycle crates as
  candidate homes; the Sending's domain is branch and worktree reaping through lib.sh's
  chokepoints and git. spira-lc is the lifecycle machine's client and must not grow a git
  and lib.sh dependency; the Sending calls it for one event, by bare name.
- **Decisions in Rust, destruction in lib.sh.** The verified deletion (salvage, the two
  liveness witnesses, the CERTIFIED/BATCHED guard, the content fence, the reap log) is the
  one chokepoint every deleter shares; porting a second copy here would recreate the
  six-site problem lib.sh's DESTRUCTION section exists to end. It moves when lib.sh does.
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
  lets `bead_close_on_land` run for a landed arm (the work did land; only the ref stays),
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
