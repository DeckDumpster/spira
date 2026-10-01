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
other bead suite) caught this the first time the shim ran against a real fixture. Fixed by
re-exporting the four names in `bead.sh` immediately after sourcing `conf.sh`; see the shim
itself for the full explanation. Any later bead in this area (or the eventual `lib.sh`/
`conf.sh` rewrite) should widen that export list rather than removing it.

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
