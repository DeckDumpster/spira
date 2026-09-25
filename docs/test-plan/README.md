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
two-digit sequence local to that area. A use case is declared once, in its
area's page, as one line:

    * `UC-<area>-NN` [T<0-4>] — <one-line statement of the behaviour>

The tier on the UC line is the cheapest tier that would catch a regression
in that behaviour; a covering suite's own tier need not match exactly (a T1
suite may incidentally also catch a T2 UC), but every UC declared at T0–T3
needs at least one covering suite.

## Dimension ids (area pages)

Each arrives with its own bead, as `docs/test-plan/<area>.md`:

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

`spira/test-plan-lint.sh` (T0) fails a suite that lacks either header line,
and fails a `# covers:` UC id that names no use case in any area page. It
also reports, without failing, every T0–T3 use case with no covering suite —
that check starts failing once the area pages above exist to report against.
Run `spira/test-plan-lint.sh --help` for exact invocation and exit codes.
