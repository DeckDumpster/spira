# suite-select — design

The one suite selector, in Rust (sp-wx2tw, wave 1 of the Rust rewrite: gate and fences). It
replaces `spira/select.sh`, `spira/gate-touched.sh`, `spira/gate-budget-select.sh`,
`spira/tier-budget.sh` and `spira/select-globs.sh`, and retires `spira/fast-suites.sh`, whose
only caller was `gate-touched.sh`.

## Intent

A change to the tree has to be tested by the suites that exercise what it changed, and the
per-bead gate has to answer within its budget. Selection answers one question: **which
suites must run for this diff, here?** Three places ask it — the landing gate (through the
repository's gate string), the round's runner (`testenv`, which the batcher drives), and CI
(`.github/workflows/gate.yml`) — and they must not disagree. So there is one library, and one
binary for the callers that are not Rust.

The property this rewrite exists to add is **fail closed**. Every bash stage swallowed its
own failure: `gate-touched.sh` ran `select.sh … 2>/dev/null || true`, so a selector that
could not read a suite, could not diff, or refused an unclaimed source file produced an
**empty** selection, and the gate string's `[ -n "$_s" ] || exit 0` turned that into a PASS
having run nothing. `gate-budget-select.sh` refusing a bad budget did the same. `testenv`
read `select.sh`'s output with `unwrap_or_default()`. Here, a selection the selector cannot
compute is a refusal (exit 75), never an empty or a full selection by accident.

## Contract

### The library (`suite_select`)

| module | what |
|---|---|
| `header` | the suite header parser: `covers_of` (first `# covers:` line plus continuation lines), `tier_of`, `selects_on_of`. The one Rust copy: spira-lint and batcher-cut read it from here. |
| `glob` | `case_match`: bash `case "$f" in $pat)` matching — `*` crosses `/`. |
| `names` | `is_suite_name` (`test-<x>.sh`, `[A-Za-z0-9._-]`), `split_list` (comma or whitespace). The gate crate's re-entry check and base re-run use these. |
| `corpus` | `Corpus::load(dir)` / `Corpus::load_named(dir, names)`: every `test-*.sh` with its parsed header, sorted by name (bytes). |
| `select` | the selection algorithm, pure: `select(corpus, changes, fn_changed, buckets, opts)` and `all(corpus, opts)`. |
| `budget` | the gate's budget cut, pure: rank, then fill to `budget × width`. `tier_cap_secs`, the per-tier cost of an unmeasured suite. |
| `timing` | P90 of `wall_secs` over each suite's last N rows of `run/tsd/suite-timing.jsonl`. |
| `gate` | the landing gate's pipeline (what `gate-touched.sh` did): corpus, selection, budget, ejected suites. |
| `io` | git reads (`diff --name-status`, `diff --raw`, `diff -U0`, `show`, `ls-tree`, `ls-files`) and the one diff-mode entry point `select_diff` that `testenv` calls. |

### The binary

```
suite-select select (--all | --base <ref> --head <ref> | --files <path>)
                    [--repo <path>] [--suite-dir <dir>] [--mode-file <path>]
                    [--report-file <path>] [--no-all-fallback] [--no-nocov] [--tiers <csv>]
suite-select gate <base> <head>
suite-select budget --budget-secs <n> [--parallel-width <n>] [--runs <n>] [--suite-dir <dir>]
```

* **stdout**: suite names (basenames), one per line. `select` prints them in selection order;
  `gate` prints them sorted (bytes), as `gate-touched.sh`'s final `sort -u` did (the gate
  runs in `env -i`, so that sort was already the C locale's).
* **stderr**: what was decided and why — the fallback reasons, every suite the budget
  dropped with its predicted cost, and a last summary line (`select: N suite(s) selected`,
  `suite-select gate: selected N (…)`).
* **exit**: `0` a selection (possibly empty, meaning nothing to run); `1` a changed source
  file no suite claims (the branch's fault; `select: unclaimed source file: <path>`); `2`
  usage; **`75` refused** — the selector could not compute a selection, and says why.

### Refusals (exit 75)

The suite directory is missing or has no `test-*.sh`; a suite file cannot be read; a suite
declares a `# tier:` that is not `T0`..`T4`; `git diff` (or `ls-tree` of the base, or
`ls-files` for the report) fails; `--files`/`SPIRA_GATE_FILES` names a file that cannot be
read; `SPIRA_GATE_BUDGET`/`--budget-secs` is not a number; a `SPIRA_TIER_BUDGET_T*_MS` is
not a number; the timing file exists but cannot be read.

What is **not** a refusal, because it is conservative rather than blind:

* No timing file (or no row for a suite): the suite costs its tier's cap
  (`tier_cap_secs`) — an unmeasured cost is never free. A malformed timing row is skipped
  and counted on stderr (the family is append-only JSONL; a torn last line is normal).
* `git show` failing for a `file#function` narrowing: every suite naming a function of that
  file runs (the narrowing only ever removes suites).
* An untiered suite: always kept by a tier filter, costed as T1.

### Selection (`select`, the algorithm `select.sh` carried)

Inputs: the corpus; the changed files, each with whether it was added, deleted, or changed
mode; the file buckets; the options.

1. **Buckets** (`SPIRA_SELECT_INERT` default `*.md *.txt`; `SPIRA_SELECT_SOURCE` default
   `spira/*.sh`; `SPIRA_SELECT_PLUMBING` default `Makefile Cargo.toml Cargo.lock
   */Cargo.toml install.sh systemd/* spira/conf.sh spira/lib.sh spira/skew.sh
   spira/testenv*.sh .github/*`). Inert files are dropped. No live file left: only the
   always-run suites (no `# covers:`), mode `diff`.
2. **Covers match.** For each live file, each suite whose `# covers:` has a glob matching it
   *claims* the file. A plain glob selects the suite, unless the suite declares
   `# selects-on:` (it is selected only by its events, step 5). A `file#func` glob selects
   the suite only when `func` has a changed line in the file (`git diff -U0` hunks, mapped
   onto `name()` … `}` blocks of the head's copy); when none of the file's named functions
   changed, every suite naming a function of it runs (code outside a declared function
   cannot be narrowed). With `--files` there is no base/head to narrow against, so every
   such suite runs.
3. **Unclaimed.** A live file no suite claims, matching the source bucket and **not
   deleted**, is exit 1 naming it. Other unclaimed files are *unplaced*.
4. **Fallback.** An unplaced file, or a plumbing file (claimed or not), selects the whole
   corpus (mode `all`) — unless `--no-all-fallback`, which every per-branch caller passes.
5. **Selects-on.** A suite with `# selects-on: added|mode` is selected when an added
   (status `A`) or mode-changed file matches one of its plain globs.
6. **Always-run** suites are appended (unless `--no-nocov`); duplicates dropped.
7. **Tiers.** `--tiers T0,T1` keeps a suite whose tier is listed, and every untiered suite.

`--mode-file` receives `diff` or `all`; `--report-file` receives `unplaced:<file>` for each
unplaced changed file and `unclaimed:<file>` for each tracked `*.sh`/`*.py` (not a suite)
that no suite's `# covers:` claims.

### The gate pipeline (`gate`, what `gate-touched.sh` did)

Environment (the gate command's own, `gate/DESIGN.md`): `SPIRA_GATE_REPO` (default `.`),
`SPIRA_GATE_FILES`, `SPIRA_GATE_TIERS` (default `T0,T1`), `SPIRA_GATE_ALL`,
`SPIRA_GATE_SUITES`, `SPIRA_CERTIFY_ALWAYS_COVERS` (default `spira/lib.sh`),
`SPIRA_GATE_EJECTED_SUITES`, `SPIRA_GATE_BUDGET` (default 300), `SPIRA_BATCH_MAXPAR`, else
`SPIRA_GATE_HOST_CORES`, else 1 (the width), `SPIRA_RUN` (timings), `SPIRA_BATCH_SUITE_DIR`
(default `spira`, relative to the working directory — the gate tree, or the CI checkout).

1. `SPIRA_GATE_SUITES=off`: only the suites whose `# covers:` glob (file part) matches a
   changed file that matches `SPIRA_CERTIFY_ALWAYS_COVERS`; nothing else, no budget, no
   ejected suites; empty when the diff touches no such file.
2. `SPIRA_GATE_ALL=1`: every suite in the corpus.
3. Otherwise the selection above with `--no-all-fallback` and the tiers. With
   `SPIRA_GATE_FILES` the corpus is the suites the **base** tree has (`git ls-tree`) that the
   suite directory also has, so a suite the branch adds cannot select itself through its own
   covers; without it, the diff is `<base>...<head>`.
4. **Budget cut** (`budget`): rank the selection — tier bucket (T0/T1/untiered before T2+),
   then specificity (fewer `# covers:` tokens first; an always-run suite last), then tagged
   (`UC-` token) before untagged, then name — and add each while the running predicted total
   divided by the width stays within the budget. Predicted cost: the P90 of the suite's last
   20 timing rows, else its tier's cap. Each drop is named on stderr.
5. **Ejected suites** (`SPIRA_GATE_EJECTED_SUITES`) that exist in the suite directory are
   added after the cut, never ranked or dropped (law-a-retry-must-change-an-input).

### Callers

| caller | how |
|---|---|
| the landing gate | the repository's gate string: `"$SPIRA_SELECT_BIN" gate "$SPIRA_GATE_BASE" "${SPIRA_GATE_SELECT_HEAD:-$SPIRA_GATE_BRANCH}"`. The gate passes `SPIRA_SELECT_BIN` into the gate command's environment. |
| `testenv` (the round's default selection) | the library: `suite_select::io::select_diff` |
| `gate` crate (re-entry, base re-run) | the library: `names::is_suite_name`, `names::split_list` |
| `batcher-cut` (suspect order), `batcher` | the library: `header::covers_of`, `glob::case_match` |
| `spira-lint` | the library: `header::covers_of`, `header::tier_of` (re-exported under their old paths) |
| `verdict.sh`, `gate-spira.sh` | `"$SPIRA_SELECT_BIN" select --files … --suite-dir "$HERE" …` |
| `.github/workflows/gate.yml` | builds this crate in the select job and runs `select` / `gate` |

## Decisions

* **One crate, library first.** Rust callers link it; there is no second process and no
  output to reparse. The binary exists for the gate string, the two bash callers and CI.
* **The installed binary selects, not the tree's.** The gate string names
  `$SPIRA_SELECT_BIN`, the installed release (like `$SPIRA_LINT_BIN`). `gate-touched.sh`
  ran from the gate tree, so a branch could change the selector that judged it; now a
  selector change takes effect at deploy.
* **The file list still decides, not the diff, when both exist.** The gate always passes
  `SPIRA_GATE_FILES`; like `gate-touched.sh`, that mode does no `file#function` narrowing.
  Narrowing there would be a smaller selection than today's, which this port does not
  choose silently.

### Behaviour dropped or changed (each deliberate)

* **Swallowed failures are refusals** (the Intent). `gate-touched.sh` ignored every
  `select.sh` failure; `gate` exits 75 on a refusal and **1 on an unclaimed source file**.
  The gate string maps 1 to FAIL and anything else non-zero to NO_VERDICT. An unclaimed
  `spira/*.sh` therefore fails the gate now, as UC-test-infrastructure-05 always said
  selection should.
* **A deleted source file needs no claim.** A file the branch deletes cannot be tested; it
  still selects any suite that names it, and it is unplaced, not unclaimed, when none does.
  (Deleting `select.sh` together with `test-select.sh` would otherwise have failed its own
  gate.)
* **An unresolvable base is a refusal.** `gate-touched.sh` fell back to the whole suite
  directory when `git ls-tree <base>` failed. The gate pins `SPIRA_GATE_BASE` to a commit,
  so a failure means the repository is wrong.
* **`SPIRA_GATE_SELECT_CAP` and `SPIRA_GATE_FAST_MAX_SECS` are dropped.** Neither reaches
  the gate command: the Rust gate runs it under `env -i` with a fixed list (gate/DESIGN.md)
  that names neither, and CI never set them. The cap was the budget cut's predecessor; the
  fast filter was the batch pre-flight's, whose `gate.sh` call has gone through the Rust
  gate since sp-0tpcs. `fast-suites.sh` had no other caller and is deleted.
* **`tier-budget.sh` is deleted, not ported.** `lint-allowlist` and `check-areas` are
  spira-lint rules already (sp-ufbkh); `check` and `check-batch` lost their only caller when
  testenv dropped the tier-budget step (testenv/DESIGN.md D4). The tier caps it and
  `gate-budget-select.sh` shared live in `budget::tier_cap_secs`.
* **P90 is computed here, not by duckdb.** `tsd-query.sh suite-p90s` (duckdb
  `quantile_cont`, linear interpolation over the last N rows by `ts`) is reproduced; ties in
  `ts` are broken by file order (later line is newer), where duckdb's order was undefined.
  No duckdb, no python: the stage is CPU-bound only.
* **Ejected suites are split on commas or whitespace, and must be suite names** — the gate
  crate's re-entry rule (`names::split_list`, `is_suite_name`). The bash split on commas
  only and tested `[ -f "spira/$s" ]`, which a name with `/` or `..` could escape.
* **Order.** The corpus is sorted by bytes (bash globbed in the caller's locale).
* **Paths are not word-split.** The bash split changed-file lists on whitespace, so a path
  with a space was two paths; a rename (`R100\told\tnew`) became both paths, which is kept.
* **Specificity counts the declared tokens.** `gate-budget-select.sh` counted `# covers:`
  tokens with an unquoted `for _t in $cov`, so a glob token (`aeon/src/*`) was expanded
  against the gate tree and counted once per matching file: a suite's rank depended on how
  many files its directory held. Parity on the real tree found it (commit 9a16d5dde: the
  bash kept 29 suites, this keeps 36 at the same 300 s budget); with `set -f` added to the
  bash the two cuts are identical.
* **The report's claim check strips `#function`.** The bash matched `file#func` tokens
  verbatim against paths, so a file claimed only at function level was reported unclaimed.
