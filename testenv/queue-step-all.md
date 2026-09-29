# `queue step --all` — addendum to queue/DESIGN.md (branch concierge/rw-queue)

Replaces `spira/spira-verdict.sh` (18 lines), the body of `spira-verdict.service` /
`spira-verdict.timer`: "settle open merge-queue batches on a two-minute cadence — for every
queue-mode repository run `queue.sh step`". It belongs in the queue binary: it is `step`,
repeated over the repositories whose land mode makes `step` meaningful.

The implementation is `queue-step-all.patch` beside this file, against
`concierge/rw-queue` (628a72dfe), with unit tests. It is not applied on this branch because
the queue crate does not exist here; the Concierge applies it (`git apply
testenv/queue-step-all.patch` on a checkout of concierge/rw-queue, then
`cargo test -p queue`).

## Intent

Every two minutes, for each registered repository in queue mode (`queue` = queue.forge, or
`queue.local`), do what `queue step <repo>` does — settle CI (`verdict.sh`), sweep and cut
(`batch.sh` + batcher), and under queue.local publish — so that no queued repository waits
on a human to be stepped.

## Contract

```
queue step --all
queue step <repo>          # unchanged
```

`--all` and `<repo>` are exclusive: both, or `--all` twice, or an unknown option →
`queue.sh step: --all takes no repository` / `queue.sh step: unknown option: --x` on
stderr, exit 2. No argument at all keeps `queue.sh step: repo required`, exit 2.

**Order.** `spira_repos`: the home repository first, then every other repo-map name, each
once — read through a new lib.sh seam **R22 `repos`** (fixed script, prints `\x1e` then
`name\0` per repository; no values in argv or env). A repository whose context (R1) fails to
resolve is reported (`queue.sh step: cannot resolve the harness configuration: …`) and
skipped; a repository not in a queue mode is skipped silently (as `repo_land_queued ||
continue` did).

**Per repository**, in order:

1. `queue.sh step --all: <repo>` on stdout — the line that tells the journal (verdict.log)
   which repository the lines after it belong to (spira-verdict.sh printed nothing and
   left the log unattributable).
2. Take `$SPIRA_QUEUE_DIR/<repo>/step.lock` with a **non-blocking** exclusive flock. Busy →
   `queue.sh step: another step holds the step lock for <repo> — skipped` on stderr, and
   go on to the next repository. It is **not** the queue lock (`<repo>/lock`): `verdict.sh`
   and `batch.sh` take that one themselves (a 90 s `flock -w`), and holding it across the
   step would make every step refuse itself. The step lock only stops two steppers (this
   timer and landing.sh's CHECK6, or a hand run) from settling the same repository at the
   same time; the one that holds it is doing the same work.
3. Exactly `step(<repo>)` — the same function `queue step <repo>` runs, same output.

`queue step <repo>` takes the same step lock (same refusal line, **exit 0** — a skipped
step is not a failure; landing.sh ignores the status either way).

**Exit status.** `0` once every queued repository has been stepped or skipped; a
per-repository step's own status is **not** propagated (spira-verdict.sh's `|| true`: a
red batch settled by verdict.sh is the step working, not failing). `1` when the repository
list cannot be read (the seam fails or answers nothing) — deliberately changed:
spira-verdict.sh turned a failing `spira_repos` into an empty loop and a green unit, which
is absence reading as success. The unit then goes `failed`, which the watchtower's
failed-units check reports.

**Last line** (stdout):
`queue.sh step --all: stepped=<n> busy=<k> unresolved=<j>[ stepped:<a,b>][ busy:<c>][ unresolved:<d>]`
— counts always, each name list only when non-empty. A repository not in a queue mode is
in none of them (it was never a candidate).

## Schema

```rust
enum Cmd { …, Step { repo: String }, StepAll }
trait Lib { …; fn repos(&self) -> Result<Vec<String>, String>; }   // seam R22
// lock.rs: try_lock_file(queue_dir, repo, "step.lock") — same flock as try_lock.
```

## Tests (in the patch; `cargo test -p queue`)

| test | pins |
|---|---|
| `step_all_parses_and_refuses_a_repository_beside_it` | CLI grammar, exit-2 messages, usage line |
| `step_all_steps_every_queue_mode_repo_in_order_and_skips_the_rest` | order, queue/queue.local only, push/hold skipped silently, unresolved reported and skipped, per-repo header, queue.local publish still inside the step, summary line |
| `step_all_does_not_propagate_one_repos_failed_step` | a failing step (positive control) does not fail `--all`, nor stop the next repository |
| `step_all_without_a_repository_list_is_a_failure_not_an_empty_pass` | exit 1, nothing stepped |
| `step_all_skips_a_repo_another_stepper_holds_and_steps_the_others` | busy → skipped with the message; freed → stepped |
| `a_single_step_under_a_held_step_lock_is_skipped_with_status_zero` | `step <repo>` honours the step lock |
| `a_step_never_holds_the_queue_lock_verdict_and_batch_take` | the step runs with `<repo>/lock` held elsewhere |
| `lock::the_step_lock_is_a_different_file_from_the_queue_lock` | step.lock ≠ lock |
| `real::repos_seam_lists_home_first_once_each_and_ignores_log_lines`, `real::repos_seam_that_answers_nothing_is_an_error_not_an_empty_list` | seam R22 through real bash: order, dedupe, blank names dropped, logs before the mark ignored, empty/failed answer = Err |

Result on concierge/rw-queue + patch: 98 tests, all pass, except a **pre-existing** flake
in the base crate (`to_local_derives_the_local_branch_and_restores_the_config_if_the_legacy_map_fails`,
2 of 12 runs on the unpatched base; `claim_and_release_hand_the_batch_back` seen once): the
queue lock is an `flock` on an open file description, and a sibling test thread that
forks a child for a bash-seam test shares that description until the child execs, so a
test that drops a lock and immediately retakes it can find it held. Test-only (production
`queue` is single-threaded); the new tests avoid the pattern (own world per lock holder, or
wait out the window). Worth a serialising mutex around the bash-seam tests in the queue
crate.

## Cutover (for the Concierge)

| # | file:line | current | replacement |
|---|---|---|---|
| 1 | `systemd/spira-verdict.service:3` | `Documentation=file://@SPIRA_HOME@/spira-verdict.sh` | `Documentation=file://@SPIRA_PROD_ROOT@/queue/DESIGN.md` |
| 2 | `systemd/spira-verdict.service:8` | `ExecStart=@SPIRA_PROD@/spira-verdict.sh` | `ExecStart=@SPIRA_QUEUE_BIN@ step --all` and, beside it, `Environment=SPIRA_HOME=@SPIRA_PROD@` (the binary finds lib.sh through `SPIRA_HOME`; the script found it beside itself) |
| 3 | `systemd/render.py:34,53` | — | `p.add_argument("--queue-bin", default="")` and `"SPIRA_QUEUE_BIN": args.queue_bin,` (the `--landing-pass-bin` pattern) |
| 4 | `systemd/install.sh:219`, `systemd/unit-ensure.sh:44` | `--landing-pass-bin "$SPIRA_LANDING_PASS_BIN" \` | add `--queue-bin "$SPIRA_QUEUE_BIN" \` (resolved by queue/DESIGN.md §7.1's conf.sh line) |
| 5 | `systemd/units.sh:94,114` | `spira-verdict.service spira-verdict.timer` unconditional | keep; if `SPIRA_QUEUE_BIN` is not executable, move both to `OPTIONAL`/`UNBUILT` with the landing-pass note (units.sh:196-203 pattern) — an empty placeholder renders `ExecStart= step --all` |
| 6 | queue/DESIGN.md §7.2 row 5 (`spira/spira-verdict.sh:17`) | `"$SPIRA_QUEUE_BIN" step "$_name" \|\| true` | superseded: the unit calls `queue step --all`; **delete `spira/spira-verdict.sh`** |
| 7 | `spira/test-timer-templates.sh:8,275-327` | asserts ExecStart invokes `spira-verdict.sh`, and greps the driver for `lib.sh`, `queue.sh" step`, `spira_repos` | assert ExecStart is `@SPIRA_QUEUE_BIN@ step --all` and `Environment=SPIRA_HOME=`; keep the TimeoutStartSec ≥ 3600 and no-CPUQuota checks; drop the driver-source checks and `spira/spira-verdict.sh` from `# covers:` (the behaviour is `cargo test -p queue step_all_`) |
| 8 | `spira/test-install-hooks-artifact.sh:126` | stubs `spira-verdict.sh` | drop it from the list |
| 9 | `spira/test-cadence-seams.sh:46,99-106` | uses `spira-verdict-prod.timer` as a fixture name | unchanged (the timer keeps its name) |
| 10 | queue/DESIGN.md §1 ("the periodic pass to `landing.sh`/`spira-verdict.sh`"), `spira/queue.sh`/`verdict.sh`/`batch.sh` comments naming spira-verdict.sh (queue/DESIGN.md §7.2 row 23 lists `spira/spira-verdict.sh:4,8`) | — | name `queue step --all` |

The timer (`spira-verdict.timer`, `OnUnitActiveSec=2min`) is unchanged, and so are the
unit's `TimeoutStartSec=3600`, `Nice=10` and its log (`verdict.log`).
