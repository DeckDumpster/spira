# spira-world — DESIGN.md

Rust-rewrite wave 5e (sp-6onps). Replaces `spira/world.sh`, `spira/aeons.sh` and
`spira/slay.sh` (bash).

## Intent

Three operator tools, one crate, because they share state and a caller relationship bash
expressed as sourcing and this crate expresses as function calls:

- **`world.sh`** (now `world`) stops and starts Spira as a whole. Stopping Spira meant
  remembering six unit names and then hunting aeon processes by hand, so in practice
  nobody stopped it cleanly — they stopped one timer and left the rest running. A control
  that is a checklist is a control nobody uses in the moment they need it.
- **`aeons.sh`** (now `aeons`) answers "how many aeons may run at once, and how many are
  running" — a question with two separate knobs (a task pool and a per-lane cap that draws
  *outside* the pool) that "set the pool to 1" does not actually answer. `status` always
  prints both numbers and the sum they imply.
- **`slay.sh`** (now `slay`) stops one aeon cleanly and makes its bead say what is true: a
  kill is not an attempt, a slain bead is not evidence the work was bad, and the branch is
  retired, not nuked, unless asked.

## The round-VM defect this wave also fixes (sp-2bkpn)

On 2026-09-30 a `world.sh stop` interrupted an in-flight Concierge round mid-corpus on the
round VM (batcher-cut, `round-vm run`) without ever knowing one existed. Acceptance:
`world status` lists an in-flight round; `stop` names it; a round-drain option exists.
`world stop` and `world status` now read `round-vm status` (`spira_world::round`) and
report what it shows — `provisioning: pid <N>` is the one signal that command exposes for
"a round is using the VM pool right now." `stop --round-drain [--round-drain-timeout SECS]`
waits for that to clear before proceeding, the same shape `drain` already gives live
aeons; without the flag, `stop` still names what it found and proceeds (naming it was the
acceptance criterion; blocking every halt on a round already claimed the target box is not
what draining the aeon pool costs, and the flag exists for when that tradeoff is wanted).

**What this does not close.** `round-vm status`'s `provisioning` field names a VM being
acquired or actively driven; it is not a full account of round-vm's lease ownership, and a
round already past its provisioning phase with no further acquire pending would not be
named by this alone (round-vm's own `release_owned_by` reconciliation, which reclaims a
lease when its owning process dies, is the deeper mechanism and is out of this wave's four
scripts). This closes the acceptance criteria as written and the immediate blind spot —
`world stop` used to say nothing whatsoever about a round — without reaching into round-vm's
crate.

## Contract

- `world stop [--why TEXT] [--hard]`: halts timers (priority order, then everything else
  systemd reports), work services (excluding cockpit/loom/watch@), and every live aeon
  (through `slay`, never a bare kill). Exits non-zero, without printing STOPPED, if any
  work service could not be stopped.
- `world start`: reads every suspension once (`spira_ctrl::load_suspended`, in-process — no
  subprocess at all, where the bash version still spawned `ctrl.sh check`/`reason` per
  timer), starts what is due, revives non-oneshot watchers `--hard` stopped, and reports
  DEGRADED if an essential timer is disabled with no recorded suspension.
- `world drain [--timeout N | --for N | --deadline N]` / `world resume`: the stamp-file
  contract is unchanged — `drain` gates new summons without stopping anything already
  running; `--deadline` slays what remains at the deadline; `resume` lifts the gate.
- `world status`: every timer's state, the two stamps, live workers and live aeons from
  `/proc`, in the same shape `world.sh status` printed.
- `world timer-priority`: `TIMER_PRIORITY`, one name per line — the seam a caller that used
  to source `world.sh` (`WORLD_LIB=1`) now reads instead (see Decisions).
- `aeons [status]` / `aeons set <n>` / `aeons unset` / `aeons pool <n>`: unchanged CLI and
  output shape; config reads and writes go through `spira-config` directly, never conf.sh.
- `slay --bead <id> [--why TEXT] [--keep-work] [--close TEXT | --reopen]`: unchanged CLI,
  exit codes and narration; see Decisions for what moved where.

## Schema

No new persisted schema — this crate reads and writes the same stamp files (`world.halted`,
`world.draining`), pidfiles (`aeon-<fayth>-<bead>.pid`, `hold-<bead>.pid`) and control file
(`$SPIRA_CTRL`, via `spira-ctrl`) the bash tools did, byte-for-byte compatible.

## Decisions

- **Three binaries, one crate, one shared `proc`/`sysctl` module.** `live_aeons` (world's
  per-pid view, to slay one) and `aeons_live_total`'s production branch (a systemd unit
  count, for display) are genuinely different queries for different purposes — conflating
  them would be the exact drift lib.sh's own comment on `aeons_live_total` warns against.
  Both are native Rust now (`proc::live_aeons`, `fleet::live_total_systemd`/`aeon_alive`),
  each with its own unit tests, so there is no shared implementation to drift apart from a
  hand-written `.fayth` parser. `_start_action`, `_is_essential_timer`, `work_services`'s
  exclusion and `_status_timer_row` — world.sh's own `WORLD_LIB=1` pure functions — are
  ordinary Rust functions in `sysctl.rs`/`proc.rs` now, tested directly with no process
  spawn, exactly the property `WORLD_LIB=1` existed to give bash.

- **`aeons`'s lane report is the one thing here that still calls into lib.sh, through a
  seam.** `.fayth` files are *sourced* as bash (`fayth_get`'s own implementation), not
  parsed as a static format — `groomer.fayth`'s `FAYTH_LABELS` computes its value from
  other shell variables, and a hand-written Rust reader would silently disagree on the one
  fayth that does. `spira_world::seam` follows the exact pattern `sentinel/src/seams.rs`
  already established for this: a fixed script, run via `bash -c`, parameters and results
  never interpolated into the script text (law-payloads-go-on-stdin). This is the only
  lib.sh dependency in `world`/`aeons`/`slay`'s read paths.

- **`slay`'s steps 0 (bead exists), 4a (release the claim), 5 (salvage/park/destroy the
  work) and 4b/6 (the note, the final status) go through lib.sh's own chokepoints via one
  seam (`SLAY_FINISH`), not a Rust reimplementation.** `lib.sh` is explicitly out of this
  wave's scope (`remaining-bash-inventory.md` group 4: "Ryan's standing instruction during
  the cutover was 'leave lib.sh alone.'"), and `spira_destroy_worktree`/`_branch` are, by
  their own comment, "the only permitted callers of `git worktree remove` and `git branch
  -D` in the harness" — a second, Rust implementation of that chokepoint is exactly the kind
  of duplicate-writer hazard the chokepoint exists to prevent, and `bdq`'s own fencing
  (repo-label validation, the destructive-vocabulary/schema-delete refusals, a dead-Dolt-
  connection retry) would silently not apply to a direct `bd` call. What DID move to Rust:
  argument parsing and every usage/exit-2 case (the 2026-09-13 positional-argument scar),
  the marker write, the hold-vs-aeon distinction, systemd-unit-or-pid resolution, and the
  wait/escalate-to-KILL loop — the process-control half of the file, and historically the
  half with the most scars (the pidfile-vs-environment `BEAD_ID` bug, the split-checkout
  "both homes" bug). This is a real, bounded engineering judgement call, not a default —
  flagged here for review rather than left implicit.

- **`world`, `ctrl`, `aeons`, `slay` are the binaries' real names; `world.sh`, `ctrl.sh`,
  `aeons.sh`, `slay.sh` are release-packaging symlinks to them (`build-tarball.sh`).** A
  Cargo `[[bin]]` name becomes its crate name, and a dot is not a legal crate-name
  character, so the literal old names cannot be the real binaries. `world.sh` and `slay.sh`
  keep the symlink because the Concierge's own persona text and skills call them by that
  exact name (operator vocabulary, not code — a harder surface to grep-and-fix atomically
  than a call site). `ctrl.sh` and `aeons.sh` get the same symlink for the same reason
  `deps.toml`'s own comment gives for keeping compatibility names in general: whatever has
  not been repointed yet keeps working. See the delivery report for the full caller audit
  (`git grep` of each basename across the tree) and what was repointed versus left resolved
  by the symlink.

- **`WORLD_LIB=1`/`CTRL_LIB=1` sourcing is gone; two real (non-test) callers needed a
  call-site fix, not a new bash library.** `watchtower.sh`'s disabled-timer-check used to
  source both `world.sh` (for `TIMER_PRIORITY`) and `ctrl.sh` (for `ctrl_load_suspended`);
  `systemd/install.sh`'s unit-apply loop sourced `ctrl.sh` the same way. Sourcing a compiled
  binary is not a thing, so both now read one seam call each instead: `world timer-priority`
  and `ctrl suspended` (a new subcommand — `ctrl.sh list`'s own `load_suspended` data,
  machine-readable). `ctrl_is_suspended` itself — a three-line pure array lookup — is
  unchanged, just defined locally in each caller instead of sourced. No new logic; the same
  functions, read differently.

## Parity

Suites that exercised `WORLD_LIB=1`/`CTRL_LIB=1`-sourced internals directly
(`test-world-decide.sh`) are retired; their coverage is the cargo unit tests named above
and in `spira-ctrl`'s own DESIGN.md, mapped as `[use_case.uncovered]` exceptions in
`docs/test-plan/instance-lifecycle.toml` (UC-41/42/44) citing this bead, following the same
pattern `sp-pppt0`/`sp-ekkak` already used for an earlier bash-to-Rust coverage move.
Every other suite that names `world.sh`/`ctrl.sh`/`aeons.sh`/`slay.sh` (`test-world.sh`,
`test-ctrl.sh`, `test-slay.sh`, `test-work-services-exclusion.sh`, `test-world-timer-*.sh`,
`test-install-aeons.sh`, …) drives the tool as a subprocess and needed no change beyond a
`# covers:` path fix (spira-lint's `covers-entries` rule) — it now validates the Rust
binary by the same bare name, unchanged.

Manually verified end to end against the built binaries (transcripts in the delivery
report): `ctrl` suspend/list/check/reason/resume/divergence; `world` status/start/drain
--deadline 0/resume against a stubbed `$SPIRA_SYSTEMCTL`; `aeons` status/set/unset/pool
against a fixture `spira.toml` and a stub `lib.sh`; `slay`'s bead-existence refusal (an id
the store does not carry, and a prefix-matched orphan), every usage/exit-2 case, and the
full happy path (no live aeon, no resolvable repo) against a stub `lib.sh`/`bdq`/`spira-lc`.
