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
- `mail.sh send` (`amend`'s live-aeon notify).

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
- **`bdq` is bridged to, not ported** — it is `lib.sh`'s function with many callers outside
  `bead.sh`, and `lib.sh` is explicitly deferred (group 4).
- **`--parent` always adds `--no-inherit-labels`** (unchanged from the bash): the label set
  computed above is already complete, and inheriting the parent's `branch:` label is exactly
  the defect `groomer.sh split-piece` exists to undo after the fact (sp-zs04v).
