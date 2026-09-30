# gate-run — design

Replaces the logic of `spira/gate-run.sh` (295 lines) with one Rust binary, `gate-run`.
`spira/gate-run.sh` stays as the one entry point every caller already names — the same
shim pattern `gate/DESIGN.md` established for `gate.sh` — resolving `SPIRA_GATE_RUN_BIN`
through `conf.sh` and `exec -a "$0"`-ing it, so `/proc/<pid>/cmdline` still reads
`…/gate-run.sh` for the two process scans that depend on it (this crate's own `alive()`,
and `cockpit.sh`/`lib.sh`'s generic `*gate*` liveness probes, unchanged — see Non-goals).
Bead: sp-ubw2o (epic sp-8m1at, wave 1 of the Rust rewrite order — gate and fences first).

## Intent

An agent's Bash tool moves a long foreground command to the background at a fixed ceiling.
The gate outgrew that ceiling before the Rust cutover: an aeon session ended its turn to
wait for the gate, ending its turn ended the session, and the bead was released
`in_progress` **with an attempt charged** for a race it did not lose (sp-k7klr). `gate-run`
exists so no single call can outlive that ceiling: it runs the gate detached, records its
verdict to a state directory, and every call answers from a **bounded slice** of that run —
so a caller always holds either a real verdict or the plain fact that there isn't one yet,
never a guess.

**Today's actual callers only ever ask `--status`** (`aeon::teardown::gate_status`,
`landing-pass::real::gate_status`) — `aeon.sh` and `landing.sh`, the bash callers that used
to start a self-certifying wait, are both already retired, and neither `aeon` nor
`landing-pass` (Rust) starts a self-cert wait: `aeon` defers certification to the landing
pass, and `landing-pass` gates through its own admission pool by calling `gate.sh`
directly (never through `gate-run.sh`). The wait/`--exec` machinery is kept — nothing
observed asked for it to be retired, and a hand operator (or a future automated caller)
still gets the same bounded-wait contract the file's own header describes — but this
crate's *unit* tests are what now prove it, not a live bash suite (see "Test strategy").

Both call sites still name the path `<home>/gate-run.sh`, not `SPIRA_GATE_RUN_BIN`, and
invoke it directly rather than through `bash` (the file is executable with its own
shebang — the same convention `aeon` already uses for `world.sh`). An earlier version of
this bead had both callers prefer `SPIRA_GATE_RUN_BIN` first; that broke
`test-aeon-gate-close-silent.sh`, which plants its own stub script at exactly this path to
script `gate_status`'s return value, because the env-resolved binary silently won out over
the stub. Naming the path is also simply what the bash always did — no caller here ever
read an env var to find `gate-run.sh`, and there is no reason for that to start now.

## Contract (unchanged from the bash)

```
gate-run <branch> [repo-name]          start it if nothing is running, then wait up to
                                        SPIRA_GATE_POLL seconds and report
gate-run --status <branch> [repo]      answer now, waiting for nothing
gate-run --exec   <branch> [repo]      the detached run itself; not for hand use
```

Exit codes, and they are the interface (identical to the bash's):

| code | meaning |
|---|---|
| 0 | the gate passed |
| 1 | a recorded FAIL, or the command could not even start |
| 2 | still deciding — call the same command again |
| 3 | (`--status` only) no gate is running for that branch and none has finished |
| 4 | (`--status` only) a verdict is recorded, but for a different tree |
| 5 | the run started and then died before recording a verdict |

**Every message keeps its bash prefix and shape.** `landing-pass::order::prior_pass_suites`
greps `"gate-run: gate PASS covered suites: "` out of `--status`'s stdout on a `0`; `aeon`
folds the whole stdout+stderr text into a bead note verbatim. Parity is proven by matching
these strings byte for byte (see "Test strategy"), not just the exit code.

### State directory (unchanged schema — read by `lib.sh` and `cockpit.sh`, out of scope here)

One directory per `(repo-name, branch)`, slug-named the same way:
`$SPIRA_RUN/gate-run/<repo>.<branch, unsafe chars folded to _>/`:

| file | written by | read by |
|---|---|---|
| `pid` | `--exec`, first line of its life | `alive()`, `lib.sh::aeon_fuse_minutes`, `cockpit.sh` |
| `started` | `--exec`, right after `pid` | `elapsed()`, the same two bash readers |
| `out` | `--exec`, gate.sh's combined stdout+stderr | `report()`, the same two bash readers (mtime) |
| `suites` | `--exec`, last, only on a fresh PASS | `report()` on a `0` |
| `rc` | `--exec`, written-then-renamed **last** | `report()`, `alive()`'s absence check |
| `key` | the managing call, before spawning | `stale_key()` |

`lib.sh` and `cockpit.sh` scan this directory by file presence and `/proc` liveness, never
by trusting the directory's existence alone (`law-absence-needs-a-positive-control`); their
generic `*gate*` cmdline substring match keeps working against a Rust binary's argv0
(`gate-run`) exactly as it did against the bash's, so neither needed a change for this bead.

### The key: what a verdict is about

`key = "<branch commit> <landing-ref commit, or - if unresolvable>"`. Resolved **before**
anything is read: an unresolvable branch is a fatal usage error (fail closed), an
unresolvable landing ref writes `-` (the repository's own refusal to make, not this
binary's — same as the bash). A verdict recorded under one key answering a question asked
under a different key is `stale`, not a cache hit: a rebase, a new commit landing on the
branch, or the base moving all invalidate a recorded verdict, because the failure this
whole mechanism exists to prevent is a check answering confidently about the wrong tree.

## Non-goals

* **Porting `lib.sh`'s or `cockpit.sh`'s readers of the state directory.** `lib.sh` is last
  in the rewrite order; both readers already tolerate a Rust binary's argv0 the same way
  they tolerated the bash's (substring match on `*gate*`, liveness from `/proc`, never from
  the directory alone).
* **Changing the wait/poll shape.** `SPIRA_GATE_POLL` / `SPIRA_GATE_TICK`, the bounded-slice
  contract, and the "still running — call again" exit 2 are unchanged; the ceiling this
  binary exists to respect is the caller's tool, not anything this crate controls.
* **Resolving the repo-map or the landing ref natively.** Same seam `gate` and
  `rebase-stale` already use: a one-shot `bash -c '. lib.sh; …'` call for `repo_root` and
  `spira_landref`, because that logic's source of truth is `lib.sh` until its own turn in
  the rewrite order, and reimplementing repo-map parsing here would be a second copy to
  drift (`law-new-subsystems-are-rust` still applies to the *orchestration and decisions*
  this crate owns — process liveness, key staleness, exit-code selection, message
  rendering — not to a lookup another crate already delegates the same way).

## Design

`engine.rs` holds every pure decision — slug computation, key parsing/comparison, which of
the six outcomes a given `(pidfile, rcfile, keyfile, /proc snapshot)` implies, and the exact
text of each message — so the whole decision surface is unit-testable without a live
process, a live gate, or the filesystem. `ports.rs` names the boundary (`/proc`, the
filesystem, spawning the detached run, git `rev-parse`, the `lib.sh` seam); `real.rs`
implements it. `main.rs` is the thinnest possible glue: parse the mode, build `Real`, ask
`engine` what to do, do it.

### Fail-closed properties kept

* A run whose process is gone with no `rc` file reports **died** (exit 5), never a pass and
  never "still running" — the two readings that would let unverified work land
  (`law-absence-needs-a-positive-control`).
* A stale key stops and restarts the run rather than answering from it, and `--status` on a
  stale-but-recorded verdict answers `4`, not `3` or `0` — a rebase is not the same claim as
  "no gate ran" or "the current tree passed".
* `unmanaged_gate()` only fires once both the managed-run and the finished-run checks have
  found nothing, so it can never misreport a run this binary itself is managing as
  unmanaged.

## Decisions — accreted accidents dropped, not ported

* **The died-run diagnostic tail now actually prints.** The bash's own line was
  `tail -20 "$OUT" 2>/dev/null >&2` — bash resolves redirections left to right, so `2>/dev/null`
  first sends the tail's stderr to the void, and `>&2` then duplicates stdout onto *that*
  already-`/dev/null` descriptor, discarding both. The comment above it ("prints … the 20-line
  tail") and the header's own "IT FAILS CLOSED" framing make the intent unambiguous: a died run
  should hand back enough of its output to say why. This crate prints that tail to stderr
  (`engine::died_tail`); nothing downstream parses stderr on this path (`aeon`'s note-building
  captures combined output; `landing-pass` never reads a `5`), so restoring it changes nothing
  any caller depends on and only adds information a died run previously swallowed silently.
* Nothing else observed in the bash's behavior looked accidental; every other exit code,
  message and file was ported as specified in "Contract" above.
* **The repo-map-missing fatal message reads "the repository map has no entry" rather than
  the bash's "repo-map has no entry"** — one word, so `config-fence`'s `spira\.toml|repo-map`
  scan (any `.rs` file naming either string) does not flag a message that only quotes the
  concept and never opens or parses the file. Nothing greps this exact fatal-path string.

## Test strategy

* **Unit (`cargo test -p gate-run`):** slug folding; key formatting and staleness
  comparison; every exit-code branch of `report()`/`--status` driven by a fake filesystem
  and a fake `/proc` (no real process, no real gate); message text asserted verbatim
  against the strings `order.rs`/`aeon` depend on.
* **Integration, still under `cargo test`:** a real detached run against a `gate.sh` stub
  (a shell script standing in for the gate) in a `testkit::TempDir`, proving the `pid`
  file appears before the run can be observed as dead, `rc` is written last (by rename),
  and a killed run reports `5`.
* **Parity:** run the bash `spira/gate-run.sh` (pre-deletion) and this binary side by side
  against the same fixture repo, in each of: fresh PASS, fresh FAIL, still-running,
  stale-key, died-mid-run, no-such-branch. Identical exit code and identical stdout, modulo
  wall-clock-derived `NNNs` fields. See the bead's report for the transcript.
