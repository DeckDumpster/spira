# Test plan

The tiers a suite or use case (UC) declares, where each tier runs, and the UC
id scheme the area pages use. The area pages themselves — the actual UC
catalogue — arrive with the area beads listed below; this page is the
schema they write into.

## Tiers

| Tier | Kind | Runs |
|------|------|------|
| T0 | static — no process runs; parses or greps source and config | certification |
| T1 | unit / fixture — deterministic, fast, no real dependency | certification |
| T2 | integration — a real dependency (testdb, a container), branch-scoped | batch CI, main CI |
| T3 | cross-component — exercises more than one subsystem together | batch CI, main CI |
| T4 | acceptance — exercises an installed, activated release | acceptance only |

A behaviour is tested at the cheapest tier that would catch its defect: a
static check beats a process, and a fixture beats a live system.

## Declaring tier and coverage

Every suite carries two header lines, read by `spira/suite-covers.sh`:

```
# tier: T1
# covers: spira/conf.sh UC-config-store-preflight-03
```

`# covers:` mixes two kinds of token, both readable by the same line: a path
glob (the existing convention — selects the suite when a matching file
changes) and a UC id (`UC-<area>-NN` — ties the suite to a use case in an
area page). `spira/test-plan-lint.sh` is what tells the two kinds apart.

## Use case ids

`UC-<area>-NN` — `<area>` is one of the dimension ids below, `NN` a
two-digit sequence local to that area. A use case is declared once, as one
`[[use_case]]` entry in that area's `docs/test-plan/<area>.toml`:

```toml
api_version = "test-plan/v1"
area = "<area>"

[[use_case]]
id = "UC-<area>-NN"
tier = "T<0-4>"
statement = "<one-line statement of the behaviour>"
```

A use case with no covering suite right now carries a `[use_case.uncovered]`
table naming why:

```toml
[use_case.uncovered]
reason = "<why nothing covers it yet>"
date = "2026-09-25"
bead = "sp-xxxxx"
```

The schema is the Rust types in `test-plan/src/lib.rs` (`Catalogue`,
`UseCase`, `Uncovered`), published as JSON Schema at
`test-plan/schema/catalogue.schema.json` and versioned by `api_version`. A
catalogue with an unknown field, an unrecognised tier, or a use-case id
declared twice across any two area files is refused, naming the offending
file — `test-plan validate` (or `spira/plan-lint.sh`, which calls it) is
what refuses it. This retires the older `* \`UC-<area>-NN\` [Tn] — ...`
markdown line format the area pages used to carry inline; an area page's
prose (scope, gaps, duplicate clusters, cost accounting) stays markdown —
only the machine-checked catalogue moved.

The tier on a use case is the cheapest tier that would catch a regression in
that behaviour; a covering suite's own tier need not match exactly (a T1
suite may incidentally also catch a T2 UC), but every UC declared at T0–T3
needs at least one covering suite or an `uncovered` marker.

## Dimension ids (area pages)

Each arrives with its own bead, as `docs/test-plan/<area>.md` (prose) plus
`docs/test-plan/<area>.toml` (catalogue) once that bead lands:

- dispatch
- cockpit-observability
- aeon-execution
- operator-channel
- safety-fences
- instance-lifecycle
- gate-verdict
- config-store-preflight
- landed-audit-reaping
- test-infrastructure
- landing-merge-queue
- ops-detection-remediation

## The lint

`spira/plan-lint.sh` fails a suite that lacks either header line, and fails
a `# covers:` UC id that names no use case in any `docs/test-plan/*.toml`
catalogue (via `test-plan validate`, which also refuses a malformed
catalogue file itself). It also reports, without failing, every T0–T3 use
case with no covering suite and no `uncovered` marker — that check starts
failing once the area pages land (`spira/test-plan-lint.sh` is this lint's
own fence). Run `spira/plan-lint.sh --help` for exact invocation and exit
codes.

`spira/plan-lint.sh --orphans <base-ref>` is the other hard failure: a
commit that deletes a suite which was the last cover of a use case fails
unless that same commit also marks the use case `uncovered` or names its
replacement. This is what the landing gate runs (`spira/plan-matrix-fence.sh`,
wired into `spira/gate-touched.sh`), so a suite deletion that silently drops
coverage cannot land.

## The coverage matrix

`docs/test-plan/coverage.json` is the whole catalogue joined to suite
coverage and measured runtime — every UC's tier, statement, covering
suites, their p50 (when `spira/tsd-query.sh` has data), and an
over-budget flag. It is regenerated whole by `spira/plan-matrix.sh`, never
hand-edited; `docs/test-plan/COVERAGE.md` is the markdown rendering of the
same document, also regenerated, also never hand-edited. The gate runs
`spira/plan-matrix.sh --check` and fails when either file differs from a
fresh regeneration.
