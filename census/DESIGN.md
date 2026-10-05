# census — Maechen census: failure classes ranked, with open-remedy suppression

Replaces `spira/census.sh` (238 lines). One binary, `census`, the same `[--with-suppressed]`
flag. Written from the script's intent and its caller (bd sp-yyk47: the Maechen chamber
brief's Step 1). The SQL and the six `census/*.py` pipeline scripts are unchanged (§2) —
this crate is the bash orchestration around them, moved to Rust.

## 1. Intent

Rank failure classes (recurred/requeued/reclaimed/reopened events) by how many distinct
beads produced them since the last Maechen pass's watermark, suppressing a class that
already has an open or recently-closed remedy bead working it, so Maechen's selection never
re-files a duplicate. Two properties this bead must not weaken: **fail closed on a clock
that disagrees with UTC** (a windowed query over a skewed clock silently hides the events
inside the skew — indistinguishable from a quiet graph) and **never drop a hand-written or
deliberate-cause event silently** (they are real state, shown separately, never ranked).

## 2. What is deliberately NOT touched

- **The SQL lives in lib.sh, unmoved.** `_census_events_sql`, `_census_class_fold_map`,
  `census_events_run_sql` (with its 3-attempt retry/backoff), `census_handwritten_run_sql`
  and `census_deliberate_run_sql` are lib.sh functions, called through the one-shot seam
  (source conf.sh + lib.sh, run the one function, capture stdout/exit code) — the same
  technique `skew` and `doctor` use, for the same reason lib.sh's own comment states
  directly: *"Kept in lib.sh so that tests can call it directly without parsing census.sh."*
  Re-deriving 400-character generated SQL strings in Rust would be exactly the duplication
  that comment exists to prevent, and it buys nothing: this bead's scope is `spira/census.sh`,
  not `spira/lib.sh`.
- **The six `census/*.py` scripts** (`count.py`, `merge.py`, `covers.py`, `covers_closed.py`,
  `handwritten.py`, `deliberate.py`) keep doing the actual class-name mapping, ranking and
  JSON parsing, run as subprocesses exactly as bash ran them (same argv shapes: file-path
  arguments for `merge.py`/`covers.py`/`covers_closed.py`, stdin for the rest). This mirrors
  `forge`'s own precedent (keeping its artifact-zip Python rather than adding a zip crate,
  `forge/DESIGN.md` §2): the class-mapping logic is fully specified, already has its own
  suite (`test-census-pipeline.sh`, kept — §6), and re-deriving it in Rust risks a silent
  behavioural drift for no reader that needs it to be Rust.
- **`bdq list`** (the open- and closed-remedy lookups) is called as plain `bd` directly, not
  through lib.sh's `bdq` wrapper: `bdq`'s own guards (repo-label validation, destructive-SQL
  refusal, the czar fence) apply only to `create`/`reopen`/`update`/`close`, never to `list`,
  so for this caller `bdq list ... --json` and `bd list ... --json` are the same call. This
  also follows this wiki's own rule directly: "reading anything at all — go straight to
  `bd`" (harness statute on `bead.sh`'s scope, carried into this port).
- **Open vs closed remedy is the lifecycle row's, not bd's** (sp-mve9i, design
  bead-lifecycle-state-machine §3.4). One `bd list --all --label-pattern covers:*` reads the
  remedies' content; `spira-lc list` splits them (`split_remedies`): READY/WORKING/REWORK is
  an open remedy (covers.py, suppresses), SUBMITTED onward a closed one (covers_closed.py,
  then the landing check). A remedy with no lifecycle row is neither and is named on stderr;
  a machine that cannot answer suppresses nothing.

## 3. Contract

### 3.1 Invocation and exit codes

`census [--with-suppressed]`, unchanged. Exit 0 on a completed ranking (even an empty one),
exit 1 when the clock-skew guard refuses or the events substrate is unreachable — never a
silent empty ranking for either (law-absence-needs-a-positive-control).

### 3.2 Output, unchanged

One line per unsuppressed class: `<distinct-beads> <class> (<events> detections[, <all-time-
beads> all-time])`. A suppressed class is omitted unless `--with-suppressed`, which also
appends `[suppressed]` / `[suppressed: remedy closed, not landed]` / `[orphaned remedy
<ids>: closed, nothing in flight]`, and prints the trailing `hand-written (not ranked): ...`
and `deliberate, not ranked: ...` blocks. **A class whose only closed remedy has landed is
neither suppressed nor orphaned** — it drops out of both sets and is shown plain, exactly as
census.sh's own `case` statement had no arm at all for that outcome (the fix already
shipped; if the class recurs it should rank again, not still be hidden behind a closed
bead). This asymmetry is preserved exactly, and is the one branch easiest to "fix" by
accident when re-reading the bash — `closed_remedy_landed_is_neither_suppressed_nor_orphaned`
pins it.

## 4. Schema

```rust
trait World {
    fn census_events_run_sql(&self, since: Option<i64>) -> Result<String, String>;
    fn census_handwritten_run_sql(&self) -> String;
    fn census_deliberate_run_sql(&self, since: Option<i64>) -> String;
    fn census_class_fold_map(&self) -> String;
    fn repo_root(&self) -> Option<String>;
    fn lc_landed(&self, id: &str) -> i32;

    fn count_py(&self, tabular: &str) -> Result<String, String>;
    fn merge_py(&self, all_time: &str, since_wm: &str) -> String;
    fn covers_py(&self, bdq_json: &str, fold_map: &str) -> String;
    fn covers_closed_py(&self, bdq_json: &str, fold_map: &str) -> String;
    fn handwritten_py(&self, tabular: &str) -> String;
    fn deliberate_py(&self, tabular: &str) -> String;

    fn bd_list_all_json(&self, label_pattern: &str) -> String;
    fn lc_rows(&self) -> Result<Vec<lc_state::Row>, String>;
    fn git_branch_exists_matching(&self, repo: &str, pattern: &str) -> bool;

    fn host_utc_epoch(&self) -> i64;
    fn bd_sql_utc_now_row(&self) -> Result<String, String>;
    fn parse_utc_to_epoch(&self, s: &str) -> Option<i64>;
    fn format_epoch_utc(&self, epoch: i64) -> String;
    ...
}
```

`real.rs` implements the Python calls with a process-lifetime scratch directory (removed on
`Drop`) for the three scripts that need on-disk file arguments (`merge.py`'s two inputs,
`covers.py`/`covers_closed.py`'s fold-map file) — the same tempdir role census.sh's own
`_TMPDIR` played, just owned by the Rust process instead of a `trap ... EXIT`.
`tests.rs`'s `Fake` stands in for the SQL/Python layer directly (it has its own,
independent coverage — §6) and exercises only the orchestration: the clock guard, the
watermark fallback, the three suppression outcomes, and the `--with-suppressed` gate.

## 5. Decisions

- **`census` locates `lib.sh` via an explicit `$SPIRA_HOME` first, release-relatively
  otherwise** — the same priority `skew` and `doctor` use (their own DESIGN.md's), for the
  same reason: a caller that deliberately pins `$SPIRA_HOME` (the maechen pass, a fixture)
  must be honored, not second-guessed by a self-location scheme that always succeeds once
  running from an installed release.
- **The clock-skew guard's date parsing shells out to `date`**, not a calendar-math crate or
  hand-rolled parser: `date -u -d "<s>" +%s` and its inverse are exactly the two conversions
  census.sh itself ran, and getting UTC/DST/leap-second edge cases bit-identical to GNU
  `date` without depending on it would be strictly riskier for a two-call guard that runs
  once per pass.
- **`bd_list_all_json`'s call goes straight to `bd`, bypassing `bdq`** (§2) — a deliberate,
  narrow exception to "port every call exactly," justified because `bdq`'s own guard
  predicates are gated on the verb (`create`/`reopen`/`update`/`close`) and provably never
  fire for `list`; this is not a behavior change, it is not calling code that would not have
  run anyway.
- **No JSON crate.** `bdq`'s `--json` output is consumed entirely by `covers.py`/
  `covers_closed.py` (unchanged Python, §2); Rust never parses it.

## 6. Parity and what moved

No dedicated bash suite tests census.sh's own orchestration logic in isolation — the
closest, `test-census-pipeline.sh`, is explicitly about the **Python** decision logic
(`count.py`/`merge.py`/`covers.py`/`covers_closed.py`'s class mapping and ranking, table-
tested on canned rows) and is **kept**, repointed (`CENSUS="$HERE/census.sh"` →
`CENSUS="$(command -v census)"`), because nothing here replaces Python it doesn't run.
`test-census.sh` (the one real-`bd`-fixture suite, `UC-ops-detection-remediation-07/12/14`),
`test-census-events.sh` and `test-census-actor-filter.sh` are likewise **kept**, repointed:
real end-to-end coverage against a live Dolt fixture that a fake-backed unit test does not
replace. This crate's 17 unit tests are the new coverage for the orchestration itself: the
clock-skew guard (pass, beyond-tolerance, query failure, unparseable row), the watermark
fallback (missing file, unreadable content, valid — routed to `merge.py`), all three
suppression outcomes (open remedy, closed-and-landed-branch, closed-with-no-branch =
orphaned) plus the landed/unknown and landed-clean asymmetry (§3.2), and the
`--with-suppressed` gate for both the suppression annotations and the trailing
hand-written/deliberate blocks.

The Maechen chamber brief (`spira/chamber/maechen.md`, `bash "{{SPIRA_HOME}}/census.sh"` →
`census`) and `spira/maechen-trigger.sh`'s comments naming `census.sh` are repointed in this
same change; see the bead's report for the full caller list.

## 7. Fail-closed

The clock-skew guard refuses (exit 1, stderr) rather than rank a windowed query over a clock
that disagrees with UTC by more than `SPIRA_CENSUS_CLOCK_SKEW_TOLERANCE_S` (default 120s),
and refuses the same way when it cannot even check (the `bd sql` probe fails, or its row
does not parse as a date) — "cannot verify" and "verified clean" are never the same exit
path. An unreachable events substrate is exit 1, never a quiet empty ranking. A remedy bead
whose land status `landed()` cannot determine (exit 2) is logged and left unsuppressed, on
the same principle: an unresolved state must not be read as "resolved, so suppress it."

## 8. Test strategy

17 unit tests (`cargo test -p census`) cover every orchestration branch against a `Fake`.
Not covered here, and not fixable from this side without a live Dolt server: the real SQL
text `_census_events_sql` generates, the real `count.py`/`merge.py`/`covers.py` class-name
mapping, and `bd sql`'s actual `DATE_FORMAT(UTC_TIMESTAMP(), ...)` row shape. Those remain
`test-census.sh`, `test-census-pipeline.sh`, `test-census-events.sh` and
`test-census-actor-filter.sh`'s job, all kept and repointed (§6).
