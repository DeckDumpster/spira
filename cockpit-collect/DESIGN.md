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
`bdjson`/`bdq`, `spira_repos`, `repo_root`, `spira_landref`, `rune_get`, `dwarf_alive`,
`detect_livelocked`, `queue_sort_rows`, and a dozen more — stays exactly what it was: a
`lib.sh` function, called the same way (wave 4 proposes `lib.sh` last, and "leave `lib.sh`
alone" is still the standing instruction). This crate owns the orchestration (which probe
runs when, the fragment lifecycle, the merge) and the *data processing* that used to be
`python3 -c`/`awk`/`sed` fragments inline in the bash — sanitizing, date arithmetic,
sorting, dedup, formatting. It does not re-derive `lib.sh`'s own logic; it bridges to it
exactly as `gate-run`'s `Real` already bridges to `repo_root`/`spira_landref` (a generic
`io::lib_call(home, func, args)` generalizes that one seam to every `lib.sh` function this
bead's probes need, rather than growing one bespoke snippet per call site).

Two ports, sp-8nv4wr, formerly Python scripts run as external tools: `metrics.rs`
(`hel_keys`, wholesale) and `sparklines.rs` (the throughput row inside
`core_detail_keys`). Both run in-process and are also reachable as `cockpit-collect metrics`
and `cockpit-collect sparklines`, which is how the suites drive them.

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
(`now`, `core`, `core_detail`, `slots`, `unsent`, `queue`, `reachable`, `well`,
`repo_labels`, `livelock`, `dup_refs`, `strands`, `ratelim`, `sops`, `statute`, `mail`,
`czar_triggers`, `hel`). Each returns a `Vec<(String, String)>` of unquoted `KEY=value`
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

### Retired: the supervisor

The supervisor (`collect`, `merge`, `--supervised-run`, the `run/cockpit.d/` fragments and the
`INVOCATION_ID` write fence) and the cockpit service unit are deleted. The scheduler's three
cockpit tier tasks call [`tiers`] in-process and are the only writer of `run/cockpit.env`.

### `main.rs` — CLI dispatch, same subcommand names

| subcommand | replaces | notes |
|---|---|---|
| `once` | `cockpit.sh once`/bare `cockpit.sh` | full serial pass printed to stdout; never writes the snapshot. |
| `history` | `cockpit.sh history` | `append_history` from the snapshot already on disk. |
| `sweep-temps` | `cockpit.sh sweep-temps` | sweep `.cockpit.*` orphans. |
| `probe <name>` | `cockpit.sh <name>` | one probe, raw `KEY=value` lines. |

### Formatting fidelity

Ten keys (`SP_HOTFIX_LINE`, `SP_HOTFIX_ALERT`, `SP_OVERRIDES_LIST`, `SP_HEIMDALL_KEYS`, and
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

## Decisions

- **Root `cockpit.sh` is out of scope** (see "Scope" above) — a basename collision in the
  bead's own filing, not a second target.
- **`lib.sh` functions are bridged, never re-implemented**, through one generic
  `io::lib_call`, generalizing the pattern `gate-run`'s `Real` already used for two calls
  to about twenty.
- **The nine self-quoting keys keep their double-quote accretion.** Fixing it is a
  `health.sh`-side change this bead does not make.
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
- **A refusal-path double-print in `well_keys`/`czar_triggers_keys` is not replicated.**
  The bash's embedded python prints its own `?` fallback keys in its `except` handler and
  then `raise SystemExit`s non-zero, which makes the *shell's* `||` fallback fire too and
  print the same keys a second time — confirmed by parity testing (`well`/`czar_triggers`
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

- **Unit**: every quoting/date/sanitize helper, the `reachable` BFS (seeds, stoppers, suspended partitions,
  scope), `strand_keys`' ghost/other classification, `dwarf_alive`'s cmdline match.
- **Parity**: every rewritten `test-cockpit-*.sh` (and the handful of other suites that
  called a probe by name — `test-mail-pane.sh`, `test-mimir-closed-record.sh`,
  `test-livelock.sh`, `test-law-synth.sh`, `test-statute-projection.sh`,
  `test-watchtower.sh`, `test-closed-strand.sh`,
  `test-tsd-producers.sh`) is repointed to call the real compiled binary
  (`cockpit-collect probe <name>` / `once`) with the same
  fixtures, the same `SPIRA_BDJSON_FIXTURE`/fake-`systemctl`/mock-`COCK` seams the bash
  suites already used — never a copy of the logic under test.
- **Cannot be verified without production**: the live collector's actual probe timings
  under the real fleet (the tiered cadences). Verified
  by deploying and watching `collector.log`/the watchdog, same as any other collector
  change.
