# bead — design

Replaces the logic of `spira/bead.sh` (497 lines) with one Rust binary, `bead`.
`spira/bead.sh` stays as the one entry point every caller already names —
`batcher-cut/src/io.rs`, every aeon/chamber brief that names `bead.sh file`/`bead.sh dep
add`, `concierge.sh`'s `bead_tool="bead.sh"`, and the suites — the same shim pattern
`gate/DESIGN.md` established for `gate.sh`: `exec -a "$0" bead --home "$(dirname "$0")"
"$@"`. Bead: sp-g9mhe (rewrite wave 5f).

## Intent

`bead.sh file` is the one sanctioned way to create a bead ("never call `bd create`
directly"): it computes the partition labels from a persona's `.fayth` (so the filing tool
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
bead [--home <spira-dir>] amend <id> [--note "<text>"] [--body-file F] [--express]
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
- `amend`'s live-aeon liveness check (`aeon_alive`: `/proc/<pid>/cmdline` against the
  `aeon`/`aeon.sh` pattern) — ordinary Rust, no shell needed.

**Routed through `spira-config` (not hand-rolled — `config-fence` reserves the repository
map's format to the crate that owns it):** the map named by `$SPIRA_REPO_MAP` is read with
`spira_config::convert::repo_sections`, into `spira-config`'s own typed `RepoSection`/`Lane`
— the same parser `spira-config convert` uses, not a second one written here. A raw
`FAYTH_LABELS`/lane token is read into `Lane` with one `serde_json` round trip
(`label_to_lane`/`lane_label`) rather than a hand-kept name table, so the lane vocabulary
has exactly one spelling in this crate: `spira-config`'s. See "Decisions" for the two named
differences this introduces against the bash's own `spira_repo_lanes`.

**Stayed bash, invoked as a subprocess (out of scope — lib.sh/conf.sh/schema.sh/mail.sh are
rewrite-wave group 4 and group 5's own later beads; Ryan's standing instruction during the
Rust cutover was "leave lib.sh alone"):**
- **Every `bd`-touching call goes through `bdq`** (the *bash function* in `lib.sh`), not
  through `bd` directly: `bash -c '. "$home/lib.sh"; bdq "$@"' bdq <argv...>`. `bdq` carries
  three create-time safety fences (`_bdq_check_repo_label`, `_bdq_check_destructive`,
  `_bdq_check_schema_delete`), a czar-fence check, a connection retry loop and
  `BEADS_NO_AUTO_IMPORT`/`SPIRA_DB` guards — none of which are `bead.sh`'s own logic, all of
  which have other live callers throughout the bash harness. Reimplementing them here would
  duplicate safety-critical code that this bead has no mandate to audit; bridging to the
  unmodified function gets exact parity by construction and zero drift risk. The bridge is
  the ONE subprocess boundary in this crate that crosses into bash logic, and it is named
  here so the eventual `lib.sh` rewrite (group 4) knows exactly what replaces it.
- `.fayth` files are read the same way `fayth_get` reads them: `bash -c '. "$1" 2>/dev/null;
  eval "printf %s \"\${$2:-\$3}\""' -- <fayth-file> FAYTH_LABELS ""`. A `.fayth` file is an
  arbitrary shell fragment (the fixtures use `${SPIRA_SCOPE_LABEL:+...}` parameter
  expansion) — the same shape `aeon/src/seam.rs`'s `BashSeam` already treats as "the chamber
  format is shell, not a value to be reparsed," so this follows that precedent rather than
  writing a second, partial shell-fragment parser.
- `schema.sh type-of <kind>` / `schema.sh kinds` (kind→bd-type mapping; schema.sh is group 4).
- `mail send` (`amend`'s live-aeon notify; `mail.sh` itself was rewritten and retired by
  sp-ooh1k — a compat symlink remains for callers outside this tree, but this crate calls
  the real `mail` binary by bare name).

## Parity

See the sibling bead report: the bash `bead.sh` and the shimmed `bead` binary run against
`test-bead-file.sh`, `test-bead-lint.sh`, `test-bead-dep-add.sh`, `test-bead-contract.sh`
and `test-bead-repo-guard.sh`'s fixtures (repo-map, chamber, stub `bd`), output and exit
code compared line for line. Every one of these suites continues to run unmodified through
`testenv` — they name `bead.sh` by its unchanged path, and the shim makes that
transparently the new binary.

**The one real parity defect this proof caught:** `conf.sh` derives several values
(`SPIRA_REPO_MAP`'s default, `SPIRA_GROOMER_LABEL`/`SPIRA_MAECHEN_LABEL`/`SPIRA_CZAR_LABEL`)
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
argv, so `chamber_dir`/`fayth_names` read that parameter directly instead of
`$SPIRA_HOME`. `SPIRA_REPO_MAP` and the three fayth labels are resolved by calling
`spira_config::resolve::resolve_for_process(home, repo, &env)` where they were needed:
`load_repos` for the repo map, and `fayth_get`'s bash subshell (which sources a `.fayth`
file whose `FAYTH_LABELS` references the three labels by parameter expansion) gets them
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
  of `(no repo-map)`. `test-bead-file.sh` and `test-bead-contract.sh` were repointed to the
  new wording in the same commit (their own header comments say so); every other assertion
  in both suites — repo, lane, override-variable, exit code — is untouched.
- **`bdq` was bridged to, not ported, as of this crate's own first landing** — it was
  `lib.sh`'s function with many callers outside `bead.sh`, and `lib.sh` was explicitly
  deferred (group 4). **Superseded by sp-w3h16** (wave 4.14, see "bdq" below): `lib.sh`'s
  `bdq` family is now ported into this same crate, as `bead::bdq` plus a sibling `bdq`
  binary. `cmd_file`/`cmd_amend`'s own `BDQ_SCRIPT` bridge above is UNCHANGED by that
  bead — it still shells to bash's `bdq`, which is now itself a one-line shim onto the
  compiled binary, so behaviour is unaffected end to end. Switching this crate's own bridge
  to an in-process call (`bead::bdq::...`, trivial now that both live in the same crate) is
  sp-pwmlj's job (wave 4.15, "collapse the bdq copies onto bead::bdq"), done together with
  aeon/bd.rs, cockpit-collect's io::bdq, incident/decide.rs's fences, auron's `_auron_bdq`
  seam and the sop seam — all independent reimplementations or bridges this bead
  deliberately left alone, so sp-w3h16 could land by itself.
- **`--parent` always adds `--no-inherit-labels`** (unchanged from the bash): the label set
  computed above is already complete, and inheriting the parent's `branch:` label is exactly
  the defect `groomer split-piece` exists to undo after the fact (sp-zs04v).

## bdq (sp-w3h16, wave 4.14)

`spira/lib.sh`'s `bdq` — the harness's one chokepoint for invoking `bd` — plus its three
create-time safety fences (`_bdq_check_repo_label`, `_bdq_check_destructive`,
`_bdq_check_schema_delete`) and the small family bundled with it in the decomposition
(`bdjson`, `json_only`, `json_count`, `ghq`) are now `bead::bdq` (pure decision logic,
`bead/src/bdq.rs`) plus a sibling binary, `bdq` (`bead/src/bin/bdq.rs`). `log`/`die` stay in
`lib.sh` as a permanent prelude; `host_cores` moves to `gate` in a separate bead — neither is
part of this crate.

**Contract, unchanged byte for byte:**

```
bdq <bd-argv...>                                   # the chokepoint itself
ghq <gh-argv...>                                   # == bdq, but for gh
bdjson <bd-argv...>    == bdq <bd-argv...> --json 2>/dev/null | json_only
json_only              # stdin filter: sed -n '/^[[{]/,$p'
json_count             # stdin filter: array length, 1 for any other JSON value, 0 unparseable
```

**Why a `bdq` binary, not just more of `bead`'s own CLI:** every caller — 31+ bash scripts,
35+ suites, `aeon`'s/`cockpit-collect`'s/`incident`'s own seams — types `bdq` (or `bdjson`/
`ghq`/`json_only`/`json_count`) by that bare name. `lib.sh` keeps one-line shims with that
exact name so none of those callers change; each shim execs into the compiled binary via
`command bdq ...` (never a bare `bdq`, which bash's own function-lookup would recurse into
forever) with a hidden `__fence`/`__json_only`/`__json_count`/`__ghq` subcommand for anything
that is not the main create/read/update/close path.

**The EXEC-BOUNDARY TRAP, concretely.** `conf.sh` deliberately never exports `SPIRA_HOME`,
`SPIRA_REPO`, `SPIRA_REPO_DERIVED`, `SPIRA_HOME_REPO` or `SPIRA_REPO_MAP` — each a per-copy
fact, not configuration. The old in-process `bdq` read them as ordinary (unexported) shell
variables; the repo-label fence now resolves the repo registry in-process via
`spira_config::repos::Registry` (never a second `spira-config repo ...` shell-out), so the
compiled binary needs those five IN ITS OWN ENVIRONMENT. The `bdq`/`_bdq_check_repo_label`
shims in `lib.sh` thread them through explicitly on every call, the same shape
`_spira_config_repo` already established. Every other value the binary reads — `SPIRA_DB`,
`SPIRA_BD`, `SPIRA_ASK_LABEL`, `SPIRA_RUN` — is already on `conf.sh`'s own export list, and
`SPIRA_FAYTH`/`SPIRA_CZAR_CLASS`/`SPIRA_CZAR_TRIGGER_BEAD`/`SPIRA_BDJSON_FIXTURE`/
`BD_TIMEOUT`/`SPIRA_BDQ_CONN_RETRIES`/`SOP_APPLIED_TRACE*`/`GH_TIMEOUT`/`SPIRA_GH` are never
`conf.sh` keys at all (set, if at all, by an already-exported caller environment — an aeon's
session env, a test fixture's `export`, a systemd unit's `Environment=`), so none of those
need re-threading. `spira/test-repo-label.sh`'s "EXEC BOUNDARY" section sets the five
unexported names via plain assignment (no `export`) ahead of sourcing `lib.sh`, proving the
shim carries them across regardless.

**What ported vs what stayed bash, this time:** everything — all three fences, the czar-fence
dispatch (still a subprocess call to `czar-fence.sh`, unchanged), the
`SPIRA_BDJSON_FIXTURE` → `bdsim.py` route (also still a subprocess, unchanged), the
empty-`SPIRA_DB` refusal, the `SOP_APPLIED_TRACE` wrapper and the invalid-connection retry
loop. `czar-fence.sh` and `bdsim.py` remain separate scripts, invoked as subprocesses from
the binary exactly as `bdq` invoked them from bash — porting either is out of this bead's
scope (czar-fence.sh guards the czar's own queue mutations, a different family entirely;
bdsim.py is the fixture engine for several `cockpit.sh` suites, group 5).

**Safety (c1), carried over fence for fence:** the repo-label refusal names every valid key in
its error (not just "refused"); the destructive-vocabulary refusal still bypasses only on
`needs-ryan` (or whatever `SPIRA_ASK_LABEL` is) already being present, and still refuses with
the ORIGINAL-case matched phrase; the schema-delete refusal is still unconditional — it does
not consult `needs-ryan` at all, on purpose (sp-1khst, approved three times while wrong).
`bdq`'s own fence dispatch still short-circuits: the first refusal returns before the next
fence runs, matching the bash's `_bdq_check_x "$@" || return 1` chain.

**`sp-ab2z1` folded in — moot, not applied.** sp-ab2z1 asked to move a duplicate, argv-based
`bead_has_label` (lib.sh's OWN copy, a different function from anything in this family) onto
stdin. `sp-j89pd` (wave 4.2, "retire dead aeon-side lib.sh functions") already deleted both
`bead_has_label` definitions as dead code before this bead started — grepped: no definition
survives in `spira/lib.sh` on this branch. Nothing to fold in; `sp-ab2z1` should close as
superseded rather than land.

**Four other `bdq` copies are deliberately untouched by this bead** (collapsing them is
sp-pwmlj, wave 4.15, which depends on this one): `aeon/src/bd.rs`'s own native
reimplementation (no create-time fences — the aeon never creates), `cockpit-collect/src/
io.rs`'s own `bdq`/`bdjson`/`json_only` (read-only, same reason), `incident/src/decide.rs`'s
pure ports of the three fences (`invalid_repo_label`/`destructive_phrase`/
`contains_schema_delete` — close in spirit to this bead's own, but a separate, pre-existing
copy; collapsing it onto `bead::bdq` is the whole point of 4.15, not this bead), and
`auron/src/seam.rs`'s `_auron_bdq() { bdq "$@"; }`, which already calls the bash `bdq` and so
is unaffected either way — it exercises the new shim without any change here. `bead`'s own
`BDQ_SCRIPT` bridge (`src/main.rs`) is the fifth; see the Decisions note above.

## event (sp-ogu8x, wave 4.24)

`spira/lib.sh`'s `spira_event` — the harness's one rate-limited outcome stream, `$SPIRA_RUN/
events.log` plus a per-(kind, target) cooldown file in `$SPIRA_RUN/events/` — is now
`bead::event::emit` (`bead/src/event.rs`) plus a `bead event` subcommand. `lib.sh` keeps a
one-line shim with the same name, since every caller (the `aeon`, `gate-check`,
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

**No env re-threading needed, unlike `bdq`'s shim:** `SPIRA_RUN`, `SPIRA_EVENT_COOLDOWN` and
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
never reading `SPIRA_NOW`) stamps the log line. **Found and fixed in the same bead:**
`strand/src/check.rs`'s own prior Rust port of this function (written before this family had
an owning crate — see below) used its one `now` for both, which is indistinguishable from
correct in production (`SPIRA_NOW` is never set outside a test) and wrong only under a
clock-seam test that neither suite ever asserted the timestamp column's value, so it was
never caught.

**`strand`'s own duplicate now calls through here instead of keeping a second copy.**
`strand/src/check.rs` wrote a complete, independently-tested native port of `spira_event`
(its own `pub fn spira_event(cfg, kind, target, title, detail)`, using its own
`src/timefmt.rs`) to emit `branch.reclaimed`, predating this family's assignment to an
owning crate. Two writers of the same `events.log`/`events/<key>` file shapes is exactly the
drift this bead exists to retire — census/tsd/cockpit readers parse what either one writes,
so a hand-kept second copy is a second thing to drift the day one of them changes. `strand`
now depends on `bead` as a library and `spira_event` is a thin wrapper: same name, same call
site (`strand/src/check.rs:299`, `spira_event(cfg, "branch.reclaimed", id, ...)`, which is
also spira-lint's `event-taxonomy` WIRED needle for that kind — untouched, so the rule still
finds it), body now `bead::event::emit(run_dir, cfg.event_cooldown, now, kind, target, title,
detail)`.

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
