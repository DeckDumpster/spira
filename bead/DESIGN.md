# bead — design

Replaces the logic of `spira/bead.sh` (497 lines) with one Rust binary, `bead`.
`spira/bead.sh` stays as the one entry point every caller already names —
`batcher-cut/src/io.rs`, every dwarf/runes brief that names `bead.sh file`/`bead.sh dep
add`, `concierge.sh`'s `bead_tool="bead.sh"`, and the suites — the same shim pattern
`gate/DESIGN.md` established for `gate.sh`: `exec -a "$0" bead --home "$(dirname "$0")"
"$@"`. Bead: sp-g9mhe (rewrite wave 5f).

## Intent

`bead.sh file` is the one sanctioned way to create a bead ("never call `bd create`
directly"): it computes the partition labels from a persona's `.rune` (so the filing tool
and the claim predicate cannot disagree), enforces that a persona may file only against a
repository whose lanes admit its partition (`law-a-lane-runs-only-where-the-repo-admits-it`),
and refuses a `--repo` absent from the repo-map before `bd` is ever called. `bead.sh lint`
is the store-wide judge of the same contract (repo:/partition labels, the `branch:` label
naming only its own bead, no `blocks` edge onto an ask- or incident-labelled bead).
`bead.sh contract` and `bead.sh dep add`/`amend` round out the CLI.

## Contract (unchanged from the bash)

```
bead.sh                                                     # every caller, unchanged
bead [--home <spira-dir>] file "<title>" --for <persona> --repo <name> [-p N] [--body-file F] [--express] [--json] [--parent <id>]
bead [--home <spira-dir>] file "<title>" --kind <kind> [--repo <name>] [-p N] [--body-file F] [--express] [--json] [--parent <id>]
bead [--home <spira-dir>] amend <id> [--note "<text>"] [--body-file F --replace | --append-body F] [--express]
bead [--home <spira-dir>] dep add <id> <depends-on-id> [--type <type>]
bead [--home <spira-dir>] lint [--all|<id>...]
bead [--home <spira-dir>] contract
```

Exit codes match the bash: `2` for a usage/argument refusal (including the repo-map guard,
the lane guard, the mutually-exclusive-flags refusal, an unknown kind), `1` for a lint
defect count or a `dep add` incident refusal, `0` otherwise.

## What ported vs what stayed bash

**Ported (this crate's own logic, now typed and unit-tested, `serde_json` instead of five
inline `python3 -c` scripts):**
- `file`'s kind routing, label composition, the lane-admission guard
  (`law-a-lane-runs-only-where-the-repo-admits-it`) and its repo-map lookup.
- `lint`'s pure judgement (`_bead_lint_judge`: repo:/partition by type and status) and the
  real-data sweep around it: unreadable ids, the `branch:` self-naming check, the two
  blocks-edge refusals (ask-onto-ask, any-onto-incident).
- `dep add`'s incident-blocks refusal.
- `contract`'s three-section listing.
- `amend`'s live-dwarf liveness check (`dwarf_alive`: `/proc/<pid>/cmdline` against the
  `dwarf`/`dwarf.sh` pattern) — ordinary Rust, no shell needed.

**Routed through `spira-config` (not hand-rolled — `config-fence` reserves the repository
map's format to the crate that owns it):** the map named by `$SPIRA_REPO_MAP` is read with
`spira_config::convert::repo_sections`, into `spira-config`'s own typed `RepoSection`/`Lane`
— the same parser `spira-config convert` uses, not a second one written here. A raw
`RUNE_LABELS`/lane token is read into `Lane` with one `serde_json` round trip
(`label_to_lane`/`lane_label`) rather than a hand-kept name table, so the lane vocabulary
has exactly one spelling in this crate: `spira-config`'s. See "Decisions" for the two named
differences this introduces against the bash's own `spira_repo_lanes`.

**Stayed bash, invoked as a subprocess (out of scope — lib.sh/conf.sh/schema.sh/mail.sh are
rewrite-wave group 4 and group 5's own later beads; Ryan's standing instruction during the
Rust cutover was "leave lib.sh alone"):**
- **Every `bd`-touching call goes through `bdq`** (the *bash function* in `lib.sh`), not
  through `bd` directly: `bash -c '. "$home/lib.sh"; bdq "$@"' bdq <argv...>`. `bdq` carries
  create-time safety fences (`_bdq_check_repo_label`, `_bdq_check_schema_delete`), a czar-fence check, a connection retry loop and
  `BEADS_NO_AUTO_IMPORT`/`SPIRA_DB` guards — none of which are `bead.sh`'s own logic, all of
  which have other live callers throughout the bash harness. Reimplementing them here would
  duplicate safety-critical code that this bead has no mandate to audit; bridging to the
  unmodified function gets exact parity by construction and zero drift risk. The bridge is
  the ONE subprocess boundary in this crate that crosses into bash logic, and it is named
  here so the eventual `lib.sh` rewrite (group 4) knows exactly what replaces it.
- `.rune` files are read the same way `rune_get` reads them: `bash -c '. "$1" 2>/dev/null;
  eval "printf %s \"\${$2:-\$3}\""' -- <rune-file> RUNE_LABELS ""`. A `.rune` file is an
  arbitrary shell fragment (the fixtures use `${SPIRA_SCOPE_LABEL:+...}` parameter
  expansion) — the same shape `dwarf/src/seam.rs`'s `BashSeam` already treats as "the runes
  format is shell, not a value to be reparsed," so this follows that precedent rather than
  writing a second, partial shell-fragment parser.
- `schema.sh type-of <kind>` / `schema.sh kinds` (kind→bd-type mapping; schema.sh is group 4).
- `mail send` (`amend`'s live-dwarf notify; `mail.sh` itself was rewritten and retired by
  sp-ooh1k — a compat symlink remains for callers outside this tree, but this crate calls
  the real `mail` binary by bare name).

## Parity

See the sibling bead report: the bash `bead.sh` and the shimmed `bead` binary run against
the retired bead-file suite, `test-bead-dep-add.sh`, `test-bead-contract.sh`
and `test-bead-repo-guard.sh`'s fixtures (repo-map, runes, stub `bd`), output and exit
code compared line for line. Every one of these suites continues to run unmodified through
`testenv` — they name `bead.sh` by its unchanged path, and the shim makes that
transparently the new binary.

**The one real parity defect this proof caught:** `conf.sh` derives several values
(`SPIRA_REPO_MAP`'s default, `SPIRA_GROOMER_LABEL`/`SPIRA_MIMIR_LABEL`/`SPIRA_CZAR_LABEL`)
as plain shell variables, never exported — harmless for the bash `bead.sh`, which ran
sourcing `conf.sh` inline in its own process, but invisible to a binary this file `exec`s
into, since only the environment table crosses that boundary. `test-express-lane.sh` (which
relies on `SPIRA_REPO_MAP`'s derived default rather than setting it explicitly, unlike every
other bead suite) caught this the first time the shim ran against a real fixture. Fixed at
the time by re-exporting the four names (plus `SPIRA_HOME`) in `bead.sh` immediately after
sourcing `conf.sh`.

**RETIRED (wave 4.9, sp-k80sa):** now that `spira_config::resolve_for_process` exists
in-process (wave 4.4/4.8), the re-export is gone rather than widened. `SPIRA_HOME` was
never really a config value — it is the `--home` argument already on this binary's own
argv, so `runes_dir`/`rune_names` read that parameter directly instead of
`$SPIRA_HOME`. `SPIRA_REPO_MAP` and the three rune labels are resolved by calling
`spira_config::resolve::resolve_for_process(home, repo, &env)` where they were needed:
`load_repos` for the repo map, and `rune_get`'s bash subshell (which sources a `.rune`
file whose `RUNE_LABELS` references the three labels by parameter expansion) gets them
injected explicitly via `.env(...)` on that `Command`, rather than depending on whatever
this process's own environment happened to inherit.

## Two named differences from the bash (both forced by `config-fence`, both unreachable by
## every suite this bead could run)

1. **A row with no `lanes` column** admits only `plan` through `spira-config`'s converter
   (`RepoSection.lanes = [Plan]`), where the bash's `spira_repo_lanes` admitted *every*
   lane. No suite exercises a `--for`/`--repo` filing against a columnless row — every
   fixture that reaches the lane check names an explicit `lanes` value — so this is a
   silent tightening, not an observed regression, and it is `spira-config`'s own chosen
   default, not one invented here.
2. **A map with one row naming an unrecognised lane token becomes unusable in full**:
   `repo_sections` returns one `Err` for the whole file, so `repos_by_name` reads it as an
   empty map and every `--repo` becomes "not in the repo map," where the bash's own
   `spira_repo_lanes ... || _repo_lanes=plan` fallback was per-row (only the bad row lost
   its lane restriction; other rows still resolved). Fail-closed by construction — a map
   this crate cannot fully trust is not trusted for a subset of it either — but a wider
   blast radius than the bash's per-row isolation. No suite has a multi-row map with one bad
   row, so this is unobserved too.

## Decisions

- **Repository lookups are `spira-config`'s, not a second parser here** (see "What ported
  vs what stayed bash"). This is the one place this bead's design changed mid-flight: the
  first draft hand-parsed `$SPIRA_REPO_MAP`'s pipe-delimited rows directly (matching
  `lib.sh`'s `repo_field` byte for byte) and failed the gate's `config-fence` — "Only
  spira-config … may name, parse or write spira.toml or repo-map" — which scans every new
  `.rs` file's *text* for the words `spira.toml`/`repo-map`, not just its file I/O, so even
  a comment describing the format counted. `spira-config/src/convert.rs` already carried an
  equivalent parser (`repo_sections`, written for the `spira.conf`/repo-map → `spira.toml`
  converter) with the identical mode/token semantics for every case a real fixture exercises
  (see the "two named differences" above for where they diverge); depending on it removed
  the duplicate rather than working around the fence's text match.
- **Two refusal messages changed wording for the same reason** — `config-fence` matches the
  literal text `repo-map` anywhere in a `.rs` file, including a string a test asserts on, so
  neither this crate's source nor its output may spell the map's hyphenated name.
  `lane_check`'s refusal now says "refused by its admitted lanes (…)" instead of "refused by
  repo-map (lanes=…)"; `contract`'s missing-map line now says `(no repository map)` instead
  of `(no repo-map)`. the retired bead-file suite and `test-bead-contract.sh` were repointed to the
  new wording in the same commit (their own header comments say so); every other assertion
  in both suites — repo, lane, override-variable, exit code — is untouched.
- **`--parent` always adds `--no-inherit-labels`** (unchanged from the bash): the label set
  computed above is already complete, and inheriting the parent's `branch:` label is exactly
  the defect `groomer split-piece` exists to undo after the fact (sp-zs04v).

## bdq

There is no `bdq` program: nothing runs bd except the store's own service, and writes go
through `spira-lc content`. `bead/src/bdq.rs` keeps the pure argv classifiers the write
guards (`bead/src/guard.rs`) and other crates share. The guards short-circuit in a fixed
order, the repo-label refusal names every valid key, and the schema-delete refusal is
unconditional: no ask label bypasses it. Fences and detectors still name `bdq` in their
banned lists, so a stray binary is refused.

## event (sp-ogu8x, wave 4.24)

`spira/lib.sh`'s `spira_event` — the harness's one rate-limited outcome stream, `$SPIRA_RUN/
events.log` plus a per-(kind, target) cooldown file in `$SPIRA_RUN/events/` — is now
`bead::event::emit` (`bead/src/event.rs`) plus a `bead event` subcommand. `lib.sh` keeps a
one-line shim with the same name, since every caller (the `dwarf`, `gate-check`,
`landing-pass`, `queue` and `sentinel` seams) types `spira_event` by that bare name, not
`bead event`:

```
spira_event() { command bead event "$@"; }
```

**Contract, unchanged byte for byte:** `spira_event <kind> <target|-> <title> [detail]`,
returning `1` for an empty kind/title or an uncreatable `$SPIRA_RUN/events` dir, `0`
otherwise (a log-append failure is swallowed, as the bash's own `|| true` swallowed it). One
line lands in `events.log` per (kind, target) per `SPIRA_EVENT_COOLDOWN` seconds (default
3600); a repeat inside the window increments a suppressed count that rides out, as `(+N
more since HH:MMZ)`, on the next emission the window allows.

**No env re-threading needed:** `SPIRA_RUN`, `SPIRA_EVENT_COOLDOWN` and
`SPIRA_NOW` are not `conf.sh` derivations that go unexported — `SPIRA_RUN` is on `conf.sh`'s
export list, `SPIRA_EVENT_COOLDOWN` is a plain `${VAR:-default}` read with no config step at
all, and `SPIRA_NOW` is only ever a test fixture's own temporary assignment (which crosses
an exec by the same ordinary environment-table rule a real export does). The shim calls
`command bead event "$@"` with nothing extra.

**The logged timestamp is not the injected clock, on purpose, matching the bash exactly.**
The bash's `now="${SPIRA_NOW:-$(date -u +%s)}"` governs the cooldown *decision* and what
gets written into the cooldown file, but the `events.log` line's own timestamp column comes
from a separate, fresh `$(date -u '+%Y-%m-%dT%H:%M:%SZ')` call — the real wall clock, even
under a test's `SPIRA_NOW` override. `bead::event::emit` keeps the same split: its `now`
parameter governs the cooldown math and file; `wall_clock_now()` (a real `SystemTime::now()`,
never reading `SPIRA_NOW`) stamps the log line. **Found and fixed independently in both
copies by this same bead** (see next paragraph for why there are still two):
`strand/src/check.rs`'s own prior Rust port of this function used its one `now` for both,
which is indistinguishable from correct in production (`SPIRA_NOW` is never set outside a
test) and wrong only under a clock-seam test that neither suite ever asserted the timestamp
column's value, so it was never caught; `strand`'s copy now has its own `wall_clock_now()`
too.

**`strand`'s own duplicate is NOT collapsed onto this crate — a dependency cycle, found at
merge time, not before.** `strand/src/check.rs` carries a complete, independently-tested
native port of `spira_event` (`pub fn spira_event(cfg, kind, target, title, detail)`, using
its own `src/timefmt.rs`) to emit `branch.reclaimed`, predating this family's assignment to
an owning crate. The first draft of this bead had `strand` depend on `bead` and delegate to
`bead::event::emit` — until merging `local/main` landed a concurrent bead that added
`bead -> strand` (`bead/src/main.rs`'s `cmd_amend`'s liveness check now calls
`strand::probe::dwarf_alive` instead of its own copy). `strand -> bead` the other way would
make a cycle Cargo refuses to build, and this bead has no mandate to re-architect that
landed dependency to make room for its own. Reverted to two independent copies, each with
its own `wall_clock_now()` fix. **Follow-up, not done here:** break the cycle with a third,
lower crate (or move `dwarf_alive` out of `strand` into something both can depend on) before
actually collapsing `spira_event` onto one writer; until then, the call site `strand/src/
check.rs:299`, `spira_event(cfg, "branch.reclaimed", id, ...)` — also spira-lint's
`event-taxonomy` WIRED needle for that kind — is untouched by this bead either way.

**A second, small `civil_from_days`/`utc_stamp`/`utc_hhmm` copy, not a shared crate.**
`strand/src/timefmt.rs` already has these (plus RFC 3339 parsing and a local-time variant
`bead::event` does not need); rather than carve out a new shared crate for three pure
functions neither test suite nor caller is blocked on, `bead/src/event.rs` carries its own
copy of the same algorithm (Howard Hinnant's `days_from_civil`/`civil_from_days`), with a
unit test cross-checked against `timefmt`'s own test case. Revisit if a third crate ever
needs UTC formatting without a date crate.

**One named difference, unreachable by every real caller:** `$SPIRA_RUN/events` is built by
the bash as a literal string join (`"$SPIRA_RUN/events"`), so an unset `SPIRA_RUN` resolves
to the absolute `/events`. `bead event`'s CLI reads `SPIRA_RUN` into a `PathBuf` and joins
`"events"` onto it, so an unset/empty `SPIRA_RUN` resolves to the *relative* `events`
instead. Unreached in practice — `conf.sh` always sets `SPIRA_RUN`, and every suite that
exercises this path sets it explicitly — named here because the parity rule asks for every
intended difference to be, not because it is expected to matter.

**spira-lint's `event-taxonomy` rule (`spira-lint/src/rules/event_taxonomy.rs`) needed no
table change.** Its `WIRED` list (the kinds "no suite can drive") already names five `.rs`
call sites, none of them `spira/lib.sh` or any file this bead touches; the shell-side
positive control (`any_site`, a literal `spira_event <kind> ` in a `spira/*.sh` direct
child) was already false on this tree before this bead — `rapid_recur_check`'s was the last
such literal call site, retired at sp-8kqww (wave 4.33) — so the rule already relies solely
on the `WIRED` Rust sites, unaffected by `lib.sh`'s body becoming a shim. Verified by running
both `cargo test -p spira-lint` and the real `event-taxonomy` rule against this branch's
tree: unchanged pass, same findings (none).

**`spira/test-event.sh` retired, not ported — its subject is now a Rust unit test.**
Every assertion (the positive control, the dash-for-plan target, the single-repeat and
26-storm suppression, six distinct beads, the suppressed-count message, the
faster-than-window loop, and the two refusals) has a `bead::event` test with the same name
and fixture shape, now run under `cargo test -p bead` instead of a real-time suite (the bash
suite's own header: "T1 + sleeps, 11s" per `docs/test-plan/cockpit-observability.md`'s
retirement table, row 20 — this port needs no `sleep`, since `now`/`SPIRA_NOW` were already
an injected clock, not a real one, in both the bash and the Rust). `spira/tier-budget-
allowlist`'s `test-event.sh` line is deleted with it (the allowlist only shrinks).
`test-gate-tree.sh`, `test-queue-ops.sh`, `test-landing.sh` and `test-poison.sh` still assert
on `events.log` content as a side effect of their own subject (queue/landing/poison/gate-tree
decisions) and are untouched — they exercise the shim exactly as they exercised the
function, through the real seam.
