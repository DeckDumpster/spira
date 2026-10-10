# cockpit-collect — design

Replaces `spira/cockpit.sh` (3,015 lines) and `spira/collect.sh` (393 lines) with one Rust
binary, `cockpit-collect`. Bead: sp-kt4l3 (rewrite wave 5c, world stopped — brain
`wiki/projects/spira/remaining-bash-inventory.md`).

## Intent

`cockpit.sh` is the ops pane's only source of truth: eighteen `*_keys` functions that turn
`bd`, `git`, `/proc` and a handful of other tools' own output into the flat
`KEY='value'` snapshot (`cockpit.env`) `cockpit/health.sh` renders. `collect.sh` is the
supervisor that runs those eighteen functions on three cadences (5s/60s/600s) as isolated
subprocesses, each writing its own fragment, merged into the snapshot every 5s so a fast
probe's freshness is never held hostage by a slow one.

The two scripts are retired together because they are one machine: the probe library has
no caller except the supervisor (and the handful of external tools — `sop.sh`'s METRIC
seam, this suite's own fault-injection tests — that call one probe by name), and the
supervisor has no meaning without the probes it schedules.

**RETIRE rather than port** (the wave's rule): every `lib.sh` function `cockpit.sh` called —
`bdjson`/`bdq`, `spira_repos`, `repo_root`, `spira_landref`, `fayth_get`, `aeon_alive`,
`detect_livelocked`, `queue_sort_rows`, and a dozen more — stays exactly what it was: a
`lib.sh` function, called the same way (wave 4 proposes `lib.sh` last, and "leave `lib.sh`
alone" is still the standing instruction). This crate owns the orchestration (which probe
runs when, the fragment lifecycle, the merge) and the *data processing* that used to be
`python3 -c`/`awk`/`sed` fragments inline in the bash — sanitizing, date arithmetic,
sorting, dedup, formatting. It does not re-derive `lib.sh`'s own logic; it bridges to it
exactly as `gate-run`'s `Real` already bridges to `repo_root`/`spira_landref` (a generic
`io::lib_call(home, func, args)` generalizes that one seam to every `lib.sh` function this
bead's probes need, rather than growing one bespoke snippet per call site).

Two genuinely separate external tools this bead does not own or change, called exactly as
the bash called them: `cockpit-metrics.py` (`sending_keys`, wholesale) and
`cockpit-sparklines.py` (the throughput row inside `core_detail_keys`).

## Scope: the collector, not the pane

`spira/cockpit.sh cockpit.sh spira/collect.sh` all appear in this bead's own filing, but
root `cockpit.sh` (build/repair the tmux cockpit, attach the operator — zero data
collection, delegates entirely to `cockpit/rebuild.sh` and `cockpit/layout.sh`) is a
*different file sharing a basename* with `spira/cockpit.sh`, not a second target of this
bead: the bead's own title names only `spira/cockpit.sh` and `collect.sh`, and the
inventory doc that generated this wave flagged exactly this basename-collision hazard.
Root `cockpit.sh` is left untouched; see "Cross-bead dependency" below for the one caller
of *this* bead's retired file that lives in `cockpit/layout.sh` (sp-llbmi's assigned file,
concurrent — not edited here).

## Design

### The probe library (`src/probes/`)

One function per `spira/cockpit.sh` `*_keys` function, same name, same subcommand string
(`now`, `core`, `core_detail`, `slots`, `unsent`, `queue`, `reachable`, `sphere`,
`repo_labels`, `livelock`, `dup_refs`, `strands`, `ratelim`, `sops`, `statute`, `mail`,
`czar_triggers`, `sending`). Each returns a `Vec<(String, String)>` of unquoted `KEY=value`
pairs — exactly what `cockpit.sh <subcommand>` printed to stdout. The one layer of shell
quoting (`shq()` in the bash) happens once, at merge time, not per probe.

Fetching (bd/git/lib.sh/external tools, all impure) lives in `src/io.rs`. The sanitizing/
formatting/sorting that used to be inline `python3 -c` lives in `src/quoting.rs` (pure,
unit tested against literal fixtures) and inline in each probe module using
`serde_json::Value` in place of the interpreter.

`unsent_keys` and `core_detail_keys` (the two largest bash functions, ~545 lines each) are
each their own module (`probes/unsent.rs`, `probes/core_detail.rs`) with one function per
named section (NEXT, RECENT, INFLOW, AWAITING CI, THROUGHPUT, TOKENS for `core_detail`; the
branch backlog, `yield.sh`/`tsd-query.sh` passthrough, the 24h landing funnel, ACCEPTANCE
and the GATE/`landing.progress` block for `unsent` — note these five belong to
`unsent_keys` in the bash, not `core_detail_keys`; they sit inside `unsent_keys`'s own
closing brace at `spira/cockpit.sh:1551`).

### The supervisor (`src/supervisor.rs`)

A 1:1 port of `collect.sh`'s registry (`PROBES`, a real Rust `const` now — 18 entries,
unit-tested for shape), fragment lifecycle (`never`/`ok`/`stale`/`error`/`timeout`) and
merge (first-wins by fragment name, meta keys always, value keys only from `ok`/`stale`).
Fully self-contained: no `bd`/`git` calls of its own, only the filesystem and spawning this
same binary's `probe` subcommand with a `timeout`. The per-tick scheduling decision —
which due, not-already-running probes to start, respecting `SLOW_CONCURRENT` *cumulatively
within one tick* — is its own pure function (`due_probes`), unit tested directly (this is
where `sp-ctag9`'s unadopted-ref false positive and the missing `queue` subcommand arm both
trace back to; it is the highest-value part of this bead to have real tests over).

### `main.rs` — CLI dispatch, same subcommand names

| subcommand | replaces | notes |
|---|---|---|
| `once` | `cockpit.sh once`/bare `cockpit.sh` | full serial pass; writes the snapshot if supervised, else prints to stdout + warns. No concierge-attach (that stayed root `cockpit.sh`'s job; dead in production here already — nothing but `cockpit.sh once` called this path). |
| `history` | `cockpit.sh history` | `append_history` from the snapshot already on disk. |
| `sweep-temps` | `cockpit.sh sweep-temps` | sweep `.cockpit.*` orphans. |
| `probe <name>` | `cockpit.sh <name>` | one probe, raw `KEY=value` lines. |
| `collect` | `collect.sh loop` | the supervisor loop; refuses unsupervised. |
| `merge` | `collect.sh merge` | one-shot fragment merge, no supervision check. |
| `--supervised-run <name> <timeout> <cmd>` | (collect.sh's own child invocation) | internal: one probe pass out-of-process. |
| `_probe_body_test <name> <timeout> <cmd>` | `collect.sh _probe_body_test` | test-only seam, exit code relays the probe's own rc. |
| `--test-may-write` / `--test-loop-guard` | sourcing `cockpit.sh` to call `cockpit_may_write`/`_loop_guard` directly | a compiled binary cannot be sourced; these are the CLI-level replacement, calling the exact same `supervisor::may_write`. |

`COCK` (env var) still overrides the probe binary for tests, matching `collect.sh`'s own
seam: unset, it is this binary's own `probe` subcommand; set, it is invoked directly with
no subcommand prefix (a test's mock script, same shape the bash mock scripts always were).

### Formatting fidelity

Ten keys (`SP_HOTFIX_LINE`, `SP_HOTFIX_ALERT`, `SP_OVERRIDES_LIST`, `SP_AURON_KEYS`, and
the six `*_NAMES` space-lists, `SP_PROTECTED_NAMES` included) wrapped their own value in a
literal single quote in the bash *before* the merge's own `shq()` quoted the line a second
time — so an empty value round-tripped as the four-character string `''''` rather than the
two-character `''` every other empty key produces, and a reader that plain-sources the
snapshot (the new Rust pane, sp-llbmi) sees that as a non-empty string, not absence. Found
in production's live `cockpit.env` (the operator, 2026-09-30) and fixed here, not replicated:
these ten keys now go through the exact same single `push`/one-`self_quote`-at-merge path
every other key does. No reader depends on the doubled shape — it was never intentional,
just two independent quoting steps neither knew about the other.

That first fix could not be the whole story: it stops the PROBES from pre-quoting going
forward, but a fragment the "stale" path had already copied forward from before the fix
took effect kept its old, already-quoted value verbatim — `run_probe_body`'s stale branch
carries a fragment's previous value lines forward unchanged on every failing pass, by
design. Merge then quoted that already-quoted value a second time anyway, reproducing the
exact same production symptom from a Rust collector release (51228489c, the operator,
2026-09-30) the first fix had supposedly already closed. The real fix is at the one place
every fragment value is read, `parse_fragment`: `quoting::unquote_shell_single` undoes a
value that is already wrapped the way `self_quote` wraps it, so merge is idempotent
against whatever produced the fragment — a probe from before the fix, a hand-edited
fragment, anything — not just correct as long as every probe always behaves.

`sanitize_title`'s trailing `.replace("=", "-")` (mirroring the bash's
`re.sub(...)[:80].replace("=", "-")`) can never fire: the allowlist already maps `=` to a
space before that call runs. Ported as-is for the same reason.

### The re-entry command: `self_exe`, never `probe_exe`

`run_loop` schedules a probe by re-entering this same binary with `--supervised-run <name>
<timeout> <subcmd>` — a TOP-LEVEL dispatch flag `main.rs`'s own `run()` matches on, not a
probe invocation. `run_probe_body` separately spawns `<probe_exe> <probe_exe_args...>
<subcmd>` to run one probe's actual logic (COCK-overridable in tests, defaulting to this
binary's own `probe` subcommand). These are two different commands for two different
purposes, and conflating them was a real production defect: `run_loop` built the re-entry
command from `probe_exe`/`probe_exe_args` too, so the real argv was `<self> probe
--supervised-run now 30 now` — `main.rs` parsed that as subcommand `probe` with probe-name
`--supervised-run`, printed usage, and exited 1, before ever reaching `run_probe_body` or
writing a fragment. No probe the supervisor scheduled ever ran; `now`'s 5s cadence
surfaced it first, because the pane's own staleness check keys on `SP_AT`, which only
`now_keys` emits (the operator, 2026-10-01: production's `now.env` frozen at the
pre-cutover bash collector's last pass, `cockpit.env` rewritten every tick by `merge` but
`SP_AT` never advancing, `SP_PROBE_FAIL=0` because nothing ever got far enough to fail).

`Config` now carries `self_exe` (always `current_exe()`, never `COCK`) separately from
`probe_exe`/`probe_exe_args`, and `supervised_run_command` builds the re-entry command from
`self_exe` alone, with nothing before `--supervised-run`. `run_loop`'s reap loop also now
logs a scheduled child that exits non-zero — the class of failure this defect was, which
previously left no trace anywhere (a fragment that is never written looks identical to a
probe that has simply never been due).

## Decisions

- **Root `cockpit.sh` is out of scope** (see "Scope" above) — a basename collision in the
  bead's own filing, not a second target.
- **`lib.sh` functions are bridged, never re-implemented**, through one generic
  `io::lib_call`, generalizing the pattern `gate-run`'s `Real` already used for two calls
  to about twenty.
- **The nine self-quoting keys keep their double-quote accretion.** Fixing it is a
  `health.sh`-side change this bead does not make.
- **`sending_keys` and the `core_detail` throughput row stay fully delegated** to
  `cockpit-metrics.py`/`cockpit-sparklines.py` — separate components, unowned by this bead.
- **The bash `loop` subcommand (the pre-`collect.sh` serial forever-loop) is dropped**, not
  ported: nothing calls `cockpit.sh loop` in production (only `collect.sh loop`, this
  crate's `collect`), and no test suite drives it either.
- **The kill-mid-probe temp-file leak class is now structural**, not merely fixed:
  `cockpit-collect once` computes the full pass in memory, then opens, writes and renames
  the snapshot temp in one tight sequence, instead of holding a `mktemp`'d file open,
  redirected, for the whole probe pass the way the bash did. `fs::rename` is atomic on
  POSIX. `test-cockpit-history-leak.sh` and the kill-mid-probe half of `test-cockpit-tmp.sh`
  are retired on this basis (rung 4 of the ladder — impossible, not refused); sweep-temps
  itself stays tested, since a previous process's own crash between write and rename is
  still possible and still needs clearing at startup.
- **`_run_probe_body`'s bash-specific deferred-`local`-in-a-trap hazard (sp-2575x) has no
  Rust analogue** — no traps, no unbound-variable class at all. The third section of
  `test-cockpit-collector-quota.sh` asserting this is retired; the other two (unit
  `CPUQuota`, the pass-log-line format) are kept and repointed.
- **Probe-registry integrity is a Rust unit test, not a bash source-grep.**
  `test-cockpit-collect-probes.sh`'s registry-shape half and
  `test-cockpit-collect-concurrency.sh` in full are retired; `supervisor::tests::
  probes_registry_is_well_formed` and `due_probes_caps_slow_tier_concurrency_cumulatively_
  within_one_tick`/`due_probes_starts_only_one_slow_probe_per_tick_when_both_are_due`
  check the same properties directly against the real `PROBES` const and the real
  scheduling decision, not an extracted copy of either.

- **A refusal-path double-print in `sphere_keys`/`czar_triggers_keys` is not replicated.**
  The bash's embedded python prints its own `?` fallback keys in its `except` handler and
  then `raise SystemExit`s non-zero, which makes the *shell's* `||` fallback fire too and
  print the same keys a second time — confirmed by parity testing (`sphere`/`czar_triggers`
  against the retired bash emit each refusal key twice). Harmless (the merge is first-wins,
  so the duplicate never reaches `cockpit.env`) and clearly accidental, not a behaviour
  worth keeping; this port emits each key once.

## Cross-bead dependency (sp-llbmi, concurrent — not edited here)

`cockpit/layout.sh`'s `restart_spira_collector_if_stale` compares the running collector
unit's start time against `${SPIRA_PROD}/cockpit.sh`'s mtime to decide whether a stale
build is still running. `SPIRA_PROD` is `.../spira-releases/current/spira`, so this is
`spira/cockpit.sh` — this bead's retired file, not root `cockpit.sh`. `cockpit/layout.sh`
is sp-llbmi's assigned file (wave 5d, concurrent); editing it here risks a collision with
their own in-flight rewrite. Whoever lands second should repoint this one reference — to
`${SPIRA_PROD_ROOT}/bin/cockpit-collect`'s mtime (or, better, the release's own build/
install timestamp) — as part of that landing. Flagged in the delivery report, not fixed
unilaterally.

## Test strategy

- **Unit** (41 tests): the supervisor's fragment lifecycle and merge (never/ok/stale/
  error/timeout, first-wins on a key clash), the scheduling decision (`due_probes`, the
  SLOW_CONCURRENT cap enforced cumulatively within one tick), the registry's shape, every
  quoting/date/sanitize helper, the `reachable` BFS (seeds, stoppers, suspended partitions,
  scope), `strand_keys`' ghost/other classification, `aeon_alive`'s cmdline match.
- **Parity**: every rewritten `test-cockpit-*.sh` (and the handful of other suites that
  called a probe by name — `test-mail-pane.sh`, `test-maechen-closed-record.sh`,
  `test-livelock.sh`, `test-law-synth.sh`, `test-statute-projection.sh`,
  `test-watchtower.sh`, `test-closed-strand.sh`,
  `test-tsd-producers.sh`) is repointed to call the real compiled binary
  (`cockpit-collect probe <name>` / `once` / `_probe_body_test` / `merge`) with the same
  fixtures, the same `SPIRA_BDJSON_FIXTURE`/fake-`systemctl`/mock-`COCK` seams the bash
  suites already used — never a copy of the logic under test.
- **Cannot be verified without production**: the live collector's actual probe timings
  under the real fleet (the tiered cadences, `SLOW_CONCURRENT`'s real contention). Verified
  by deploying and watching `collector.log`/the watchdog, same as any other collector
  change.
