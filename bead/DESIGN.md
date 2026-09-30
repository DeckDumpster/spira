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
- The repo-map's own format (`name | path | land | base | format | ? | lanes`, pipe-delimited,
  `#`-comments, the lanes-column heuristic) and the lane-mode expansion
  (`consume`/`develop`/`self`) — **read directly from `$SPIRA_REPO_MAP`**, not from
  `spira.toml`, because that is the format every existing suite's fixture pins (parity, not
  a migration to the typed config — that migration is `spira-config`'s own, separate,
  ongoing work).

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

## Observed but preserved (not a parity difference — inherited from the bash)

The "no lanes column — defaults to `<plan>`" refuser text in `lane_check`'s message is
unreachable with the ported logic exactly as it was unreachable in the bash: a row with no
`lanes` column (or an explicitly empty one) makes `expand_lanes`/`spira_repo_lanes` return
*every* lane, which always admits whatever partition triggered the check in the first
place, so the refusal branch that would print that text can never fire. Kept verbatim
rather than "fixed", since changing it is a behaviour change outside this bead's mandate.

## Decisions

- **The repo-map and `.fayth` parsing stay keyed on their existing file formats** (pipe rows,
  shell fragments) rather than reading `spira.toml`'s typed `RepoSection`/`PersonaSection`,
  because every existing fixture pins the legacy format and this bead's job is parity, not a
  config migration.
- **`bdq` is bridged to, not ported**, for the reasons above — it is lib.sh's function with
  many callers outside `bead.sh`, and lib.sh is explicitly deferred.
- **`--parent` always adds `--no-inherit-labels`** (unchanged from the bash): the label set
  computed above is already complete, and inheriting the parent's `branch:` label is exactly
  the defect `groomer.sh split-piece` exists to undo after the fact (sp-zs04v).
