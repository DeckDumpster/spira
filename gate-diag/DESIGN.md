# gate-diag — design

Replaces the logic of `spira/gate-diag.sh` (258 lines) with one Rust binary, `gate-diag`.
`spira/gate-diag.sh` stays as the one entry point every caller already names — the same shim
pattern `gate/DESIGN.md` established for `gate.sh` — resolving `SPIRA_GATE_DIAG_BIN` through
`conf.sh` and `exec -a "$0"`-ing it. Bead: sp-ubw2o (epic sp-8m1at, wave 1 of the Rust
rewrite order). Callers: `testenv` (`run.rs`, twice, on a red first pass) and
`.github/workflows/gate.yml`'s Diagnostics step, both unchanged by name.

## Intent

`gate-diag <results-root>` turns a finished batch's `.result`/`.out` files into the three
things every reader of a red run needs, without re-running anything: a human-readable report
(GitHub Actions annotations, or plain terminal blocks), a machine-readable verdict per suite
(`red-suites.json`, and retry-adjusted `(verdict)` rows appended to `results.jsonl`), and a
per-case record of every suite, red or green (`results.jsonl`, `junit.xml`) for whatever reads
them afterward (`forge.sh`'s attribution, a human diffing two runs). It never re-runs a suite
and never changes a verdict already on disk — it only reads `.result`/`.out` files and a
retry directory (`<root>-retry`) that a separate rerun already produced.

**This is a report, not a check.** Unlike `gate`/`spira-lint`, `gate-diag` has no pass/fail of
its own to fail closed on — the bash never returned non-zero except on a missing `<results-
root>` argument, and every red suite it names is `red` regardless of what `gate-diag` prints
about it. "Fail closed" here means: a results root that cannot be read is a hard, named
error (matching the bash's `${1:?usage: gate-diag.sh <results-root>}`), not a silent "no
reds" report that looks identical to a genuinely green run.

## Contract (unchanged from the bash)

```
gate-diag.sh <results-root>          # every caller, unchanged
gate-diag [--home <spira-dir>] <results-root>   # what gate-diag.sh execs
```

* Scans `<root>/*.result` and one level down, `<root>/*/*.result` — the same two-shape scan
  the bash used, so the same binary works for a leaf directory (`testenv-batch`'s own call)
  and a root directory (CI's call over both the parallel and serial-retry passes).
* Exit code is always `0` once the results root is readable — same as the bash.
* Reads `<root>-retry` for red/timeout suites' second verdict, classifying each as
  `red-red` (still red on a serial rerun), `red-green` (flake — the annotation-driven flaky-
  suite bead in `gate-check` reads this class from the CI-uploaded artifact), or `red`
  (no retry ran).
* `SPIRA_BATCH_TAIL_LINES` (default 50): lines of a red suite's `.out` shown.
* `GITHUB_ACTIONS` set: `::group::`/`::endgroup::`/`::error file=spira/<suite>::<msg>` per
  red suite, capped implicitly at GitHub's 10-annotation limit by `red-suites.json` — the
  artifact `forge.sh` actually reads — carrying every red, not just the first ten.
* `GITHUB_STEP_SUMMARY` set: the same markdown table appended there.

### Non-goals

* **Changing what counts as a FAIL line, a died suite, or a flake.** Every heuristic (the two-
  tier grep, the declared-timeout lookup, the retry-status mapping) is ported as specified
  below, not redesigned.

**Formerly a non-goal, now done (sp-9gd4e):** porting `tap-jsonl.sh`'s `tap_jsonl_rows` was
out of this bead's original remit because it was shared with `spira/suites.sh` — forking it
here would have split the one TAP parser the bash's own header insisted on ("ONE PARSER, ONE
PLACE"). `suites.sh` no longer exists (replaced by `testenv suites`, DESIGN-suites.md), so
this binary's `tap_jsonl_rows` shell-out was its only caller left. sp-9gd4e ported it to
`testenv::tap::jsonl_rows` (testenv/DESIGN.md §3.5a) — "one parser, one place" now means one
Rust function two crates call, not one sourced bash file. This binary calls it directly (a
`testenv` path dependency); `ports.rs::World` lost the `tap_jsonl_rows` method, since a pure
function is not an effect on this boundary. Parity: bash and Rust produce byte-identical
`results.jsonl` rows over every `spira/*.sh` file in the tree (633, tier/UC extraction) plus
planted TAP/skip-all/bail-out/escaping fixtures (sp-9gd4e's report).

## Design

`engine.rs` holds every pure decision: FAIL-line extraction, the "died with no output" and
"died with output but no FAIL line" messages, the declared-timeout lookup's fallback, retry-
status classification, the summary row and annotation text, and the JSON/JUnit XML builders.
None of it touches the filesystem. `ports.rs` names the boundary (reading a `.result`/`.out`
file, listing suites in the two-level scan, GitHub Actions environment, writing the step
summary); `real.rs` implements it. `main.rs` is the glue — it also calls
`testenv::tap::jsonl_rows` directly (a pure function, not a port).

### Suite result collection (unchanged scan and status vocabulary)

A suite is **red** for this binary's purposes when its `.result` file's first field is `red`
or `timeout` — the same vocabulary `testenv`'s `record.rs`/`skipgate.rs` already commit to
disk, and the comment there notes explicitly that `gate-diag.sh` (now this crate) is one of
the readers that vocabulary is frozen for.

### Declared timeout

`sed -n 's/^# *timeout: *//p' "$HERE/$suite" | head -1` — a suite source file may declare its
own budget in a `# timeout: NNN` comment; this binary reads the same file at the same path
(`<home>/<suite>`) the same way, falling back to `SPIRA_SUITE_TIMEOUT` (default 600) when the
comment is absent or not a plain integer.

### Retry classification

Read `<root>-retry/<suite>.result` (same two-level scan) if `<root>-retry` exists at all.
`ok` → `red-green (flake)`; `red`/`timeout` → `red-red`; missing or any other value → `red`
(no retry ran, or ran and produced something this binary does not interpret further — the
bash's own `case` statement had exactly these three arms and no `*)` fallback beyond `red`).

## Decisions — accreted accidents dropped, not ported

None found. Every branch of the bash's output (GHA vs plain, the two FAIL-line grep tiers,
the died-with-output vs died-without-output messages, the JSON shapes) is deliberate and
ported as specified.

One cosmetic difference, confirmed harmless by parity testing: `red-suites.json` is written
compact (`{"red":[...],...}`) rather than with the spaces Python's `json.dump` inserts after
`:`/`,`. Every reader (`forge.sh`) parses it as JSON, not by text match, so this changes
nothing observable; noted here only because the parity run below caught the byte diff and it
is worth naming rather than leaving silently unexplained.

## Test strategy

* **Unit (`cargo test -p gate-diag`):** FAIL-line extraction against fixtures for each of the
  three cases (a matching `  FAIL  `/`FAIL:`/`not ok ` line, a looser `FAIL`/`not ok`
  substring only, and neither — the died cases); declared-timeout parsing from a suite
  source's comment and its fallback; retry classification for all four inputs (`ok`, `red`,
  `timeout`, missing); the summary row and GHA annotation text rendered from a fixed input;
  `red-suites.json` and the appended verdict rows' exact JSON shape; the JUnit XML builder
  against a small `results.jsonl` fixture, including escaping of `<`, `&`, `"`.
* **Integration:** a `testkit::TempDir` results root with a mix of green/red/timeout
  `.result`/`.out` files (both flat and one-level-nested, the two scan shapes), with and
  without a `-retry` sibling, asserting the printed report, `red-suites.json` and the
  `results.jsonl` verdict rows the same input the bash was given would have produced.
* **Parity:** run the bash and this binary over the same fixed batch-results directory
  (green, red, timeout, and retried-green suites) and diff stdout (modulo nothing — this
  binary carries no wall-clock text of its own) and every written file.
