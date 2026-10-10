# Test plan — Detection, incidents and remediation (`ops-detection-remediation`)

Area id `ops-detection-remediation`; use-case ids are `UC-ops-detection-remediation-NN`,
declared in `docs/test-plan/ops-detection-remediation.toml`. Supersedes `sp-fhzib`, whose
branch became a shared catch-all across five beads and was never merged — see
[[sp-n1twd]] below for exactly what that bead's own work still owed and how this page
differs from its draft.

Subjects: `incident.sh`, `census.sh` (and the `cluster.py`/`cluster_merge.py`/`covers.py` files it
calls), `maechen-trigger.sh` + `chamber/maechen.md`, `auron.sh` + `auron-classify.py`,
`strand.sh` + `strand-classify.py`, `watchtower.sh`, `czar-pass` → `czar-pass/src/main.rs` (on
the reconciler-engine, sp-pu7v6), `czar-fence.sh`, `groomer`, `groom-trigger.sh`,
`hold.sh`/`unhold.sh`, `cockpit.sh livelock` + `close-reason-flags.py`, `archivist`.

---

## 1. Intent (the de facto spec)

The harness watches its own pipeline and turns each observed problem into **exactly one**
piece of tracked work. It does not stay silent, and it does not flood.

Detectors each read one kind of evidence:
- **watchtower**: cockpit.env, landstate, gate.log and lapse records.
- **Auron**: the sentinel log, DB reachability and the systemd restart count.
- **strand**: ready beads against live aeons.
- **czar-pass**: landing.log, forge CI state and strands.json, through the reconciler
  engine's hysteresis (`reconciler-engine/src/core.rs`) rather than a hand-rolled
  first-seen marker file.
- **census**: bump_* and reopen events.
- **livelock**: bead predicates and close reasons.

The detectors share three rules:
- An unreadable input renders `?` (`RawStatus::Unobservable` in czar-pass's case), never
  `0`, and is never treated as all-clear.
- Deliberate states suppress the alarm they would otherwise cause: halted, draining,
  capacity-paused, pool-paused, fleet-saturated and pass-truncated.
- An alert needs confirmation and hysteresis before it acts.

Every finding goes through `incident.sh`, whose intake contract is:
- A stable ref deduplicates filings to one bead, across callers, labels, repos and
  concurrent filers.
- A recent close is reopened rather than duplicated.
- Recurrences are counted as events with bounded notes.
- At `SPIRA_SIN_AT` recurrences the incident escalates once, unless the ref is exempt.

The census ranks failure classes by distinct beads since a watermark. A remedy that has not
landed suppresses its class. Maechen is triggered by a landing count or a time gap, and it
must not blind its own window.

Mechanical remedies are applied idempotently, and a dry run reports without writing: the
groomer sweep (and its refusal to close a bead as "unwanted"); the czar, staged
shadow→act per class; hold liveness witnesses; archivist capacity back-off.

---

## 2. Use cases

Tier key: T0 static · T1 unit · T2 component · T3 integration · T4 acceptance. The 40
declarations live in `ops-detection-remediation.toml`; see that file for id/tier/statement.
Four of them (37-40 — the lapse writer/reader contract, trigger-lock concurrency, the
Auron-to-panel label contract, and strand partition isolation) carry an
`[use_case.uncovered]` marker: they name behaviour this page's design calls for that no
suite exercises yet, all deferred to `sp-fhzib.4`, which was reopened separately from this
bead and has not landed. Every other declared use case has at least one covering
suite today, whether or not that suite has yet been demoted to the cheapest tier that would
catch its regression (tier is normative — "the cheapest tier that WOULD catch it" — not a
description of where the current covering suite happens to run; a T2/T3 suite over-covering
a T1-rated use case is not a gap).

36 behaviours were identified against the mapper records, `signals.tsv` and a read-only read
of the subject sources (no suite was run to write this analysis). UC-01 merges what were
four separate dedup/recurrence/cross-label/cross-repo cases.

---

## 3. Coverage map

The verdicts below are the original design's, carried forward from the analysis that
produced the 36 use cases. Columns marked **applied** reflect code that is actually on
`origin/main` today; everything else describes the target shape and its own follow-up bead.

| UC | Verdict | Status |
|---|---|---|
| 01 | Merge `test-incident-dedup.sh` into `test-incident.sh`; demote the decision part to T1 against a stub bd. | **Applied** (`sp-fhzib.2`): `incident-dedup-decision.py` extracted and table-tested; `test-incident-dedup.sh` deleted. |
| 02 | Deterministic barrier instead of relying on Dolt latency. | **Applied**: a `SLOW_BD` wrapper sleeps inside the flock. |
| 03 | Add an outside-lookback row and the BSD `date -v` fallback. | **Applied**: both covered in `test-incident.sh` / the stub. |
| 04 | Demote to T1 (counting stub bd, no Dolt). | **Applied**: `spira/incident-stub-bd.py`. |
| 05 | Demote to T1 (stub bd plus a maildir). | **Applied**. |
| 06 | Demote the note-size decision to T1; keep one real row; delete `test-incident-recur-bounded.sh`. | **Applied**. |
| 07 | Demote `test-sin-exempt.sh` to T1 against a stub-`bd sql` count; merge the real-events row into the census integration suite; move the watchtower DRAINING-exempt assertion to `test-watchtower.sh`. | **Applied** except the real-events row, which was deferred by `sp-fhzib.3` until `sp-fhzib.2` landed. **Landed by this bead** (`sp-n1twd`): `test-census.sh` gained the Sin real-events row — see §8. |
| 07b | Delete the backfill-recur-causes test; propose retiring the migration itself. | **Retired** (`sp-76ec4`): rows removed from `test-incident-recur-cause.sh` and `test-incident-migrations.sh`; the migration itself deleted from `incident.sh` — `sp-lzt` removed every `sp-recur-N` label write, so nothing bare is left to backfill. |
| 08 | Rewrite `test-incident-delivers-satisfiable.sh` from source-grep to a real T1 behaviour test; merge `test-batch-repeat-refused-delivers.sh` in. | **Applied**. |
| 09 | Move the REF/CAUSE pairing check to the gate's T0 fence stage. | **Applied**: `spira/incident-cause-lint.sh`, wired into `gate-spira.sh`. |
| 10 | New suite against a genuinely unreachable bd (gap G1). | **Applied**: `test-incident-spool-drain.sh`. |
| 11-15 | Extract census's heredoc python to `spira/census/*.py`; table-test on canned rows; keep one server-mode suite for the SQL-writes-events and since-filter contract. | **Applied** (`sp-fhzib.3`): `spira/census/{count,merge,covers,covers_closed}.py`; `test-census-pipeline.sh` (T1) + `test-census.sh` (T3, 3 rows + the Sin row landed by this bead). `test-census-reopen.sh`, `test-census-watermark.sh` deleted. |
| 16-17 | Keep `test-maechen-trigger.sh`; delete `test-maechen-blind-window.sh` and `test-maechen-sibling-blind.sh`. | **Applied** (`sp-fhzib.3`). |
| 18-22 | Split Auron's classifier half into its own T1 suite; merge the five `test-strand-*` classifier files into one table; keep `test-strand-lock.sh`/`test-strand-truncated.sh` for their own T2 properties. | **Not applied.** `test-auron.sh` and the five `test-strand-*.sh` files are unchanged. `sp-fhzib.4` did this work on the retired branch; it was reopened as its own bead and has not landed on `main`. |
| 23 | Port the four log-pattern detectors plus CI-STALLED/CI-RED/STARVED to Rust `#[test]`s against the real reconciler-engine code; retire `test-watchtower-queue.sh` once ported. | **Applied, then superseded.** czar-pass was rewritten onto the reconciler engine (`sp-pu7v6`) after the original Rust port landed, which deleted the marker-file helpers (`fs_record`/`fs_get`/`fs_clear`/`fs_path`/`latency_secs`) those tests exercised; 5 of the 22 tests named functions that no longer exist, so `cargo test -p czar-pass` did not even compile on `main`. **This bead removes those 5 dead tests and adds 25 new ones,** bringing the file to 42 passing tests, exercising every `detect_*` function's fire/silent boundary against the current `Config`/`StateMap`/reconciler-engine shape — see §8. `test-watchtower-queue.sh` is gone (`sp-fhzib.5`); `test-czar-pass.sh` gained a `cargo test -p czar-pass` case (case 0) so the unit layer is verified as part of the suite, which is what would have caught the compile failure. |
| 24 | Keep the smoke rows in `test-czar-pass.sh`; fix its two tautological exit-code assertions (gap G5). | **Applied**, landed separately in Concierge round 37 (commit `40eeb1dba`, "test-czar-pass reads czar's real exit code on the halted pass"). |
| 25-36 | Demote watchtower's czar-outcome classifier, snapshot renderers and sweep thresholds to T1; merge livelock/close-reason/groomer/hold/archivist predicate logic out of their T2/T3 hosts. | **Not applied**, same as 18-22: `sp-fhzib.4`'s scope, reopened separately, not landed. The one exception is the watchtower render/sweep decomposition itself, which even the retired branch's own author found overstated (`snapshot()` is one heredoc reading ~50 globals, not callable functions) and deferred further, to `sp-m0qeh`. |

---

## 4. Duplicate clusters (as designed; D5/D6/D9/D12 already resolved)

| # | Behaviour | Files | Status |
|---|---|---|---|
| D1 | Incident dedup: N filings → 1 bead + N-1 recurrences | test-incident.sh, test-incident-dedup.sh, test-incident-recur-bounded.sh, test-incident-recur-cause.sh | Resolved (`sp-fhzib.2`) |
| D2 | Sin escalation at SIN_AT | test-sin-exempt.sh, test-incident-recur-cause.sh | Resolved (`sp-fhzib.2`) |
| D3 | NULL-cause / empty new_value distinct-bead count | test-census-events.sh ×2, test-census-reopen.sh, test-census.sh | Resolved (`sp-fhzib.3`) |
| D4 | Since-watermark counting / trigger watermark | test-census-watermark.sh, test-maechen-blind-window.sh, test-maechen-trigger.sh | Resolved (`sp-fhzib.3`) |
| D5 | czar-pass CI-STALLED/STARVED/halted | test-watchtower-queue.sh, test-czar-pass.sh | Resolved (`sp-fhzib.5`): the former deleted, its properties absorbed into `test-czar-pass.sh`. |
| D6 | Shadow vs act staging + stage-key allowlist | test-czar-shadow.sh, test-czar-pass.sh | Resolved: conf keys live at T0, czar-shadow keeps the fence. |
| D7 | strand-classify.py harness + `starved` positive control | 6 test-strand-* files | **Open** (`sp-fhzib.4`) |
| D8 | `strand.sh check --from` + mail stub | strand-capacity, strand-pool-paused, strand-partition, strand-lock | **Open** (`sp-fhzib.4`) |
| D9 | Halted-world skip + stable-ref pattern | watchtower.sh, watchtower-czar-outcome.sh, watchtower-queue.sh, czar-pass.sh | Partially resolved: watchtower-queue.sh is gone; the remaining three still each carry their own halt-guard assertion (`sp-fhzib.4`'s shared T1 table is `sp-fhzib.5`'s further scope, undone). |
| D10 | Livelock categories + detector re-run | test-livelock.sh | **Open** (`sp-fhzib.4`) |
| D11 | Close-reason phrase flags | test-livelock.sh, test-ops-closing.sh | **Open** (`sp-fhzib.4`) |
| D12 | "Every snapshot section present" loop | test-watchtower.sh, test-watchtower-lapse.sh | **Open** (`sp-fhzib.4`) |
| D13 | Watermark prose greps of maechen.md | test-maechen.sh, test-maechen-sibling-blind.sh | Resolved (`sp-fhzib.3`) |
| D14 | Trigger shape (dedup, threshold, lane guard) | test-groom-trigger.sh, test-maechen-trigger.sh | **Open** (`sp-fhzib.4`) |

---

## 5. Gaps

Gaps opened by the original analysis, and their current state:

1. **G1 (spool/drain recovery)** — closed (`sp-fhzib.2`, `test-incident-spool-drain.sh`).
2. **G2 (`incident.sh systemd`)** — closed (`sp-fhzib.2`, `test-incident-systemd.sh`).
3. **G3 (lookback boundary)** — closed (`sp-fhzib.2`).
4. **G4 (`_counter_events_query` fails open to `0`, not `?`)** — still open. Filed
   standalone as `sp-b5blz` (a product defect, not a test gap — the area plan's own
   instruction is not to fix a gap-exposed defect silently inside a test-plan bead).
5. **G5 (czar-pass's two tautological exit-code assertions)** — closed, landed in
   Concierge round 37 independently of this bead's branch.
6. **G6 (strand partition filtering untested)** — still open (`sp-fhzib.4`).
7. **G7 (aeon teardown ordering)** — out of this area's scope; belongs to
   aeon-execution, cross-referenced only.
8. **G8 (lapse writer/reader contract)** — still open (`sp-fhzib.4`).
9. **G9 (incident migrations untested)** — closed (`sp-fhzib.2`,
   `test-incident-migrations.sh`).
10. **G10 (trigger concurrency: neither trigger locks its dedup check)** — still open
    (`sp-fhzib.4`).
11. **G11 (deterministic concurrency proof)** — the incident half closed (`sp-fhzib.2`'s
    `SLOW_BD` barrier); the strand-lock half still open (`sp-fhzib.4`).
12. **G12 (census case 9 cannot go red)** — closed by deletion (`sp-fhzib.3`), not by a
    new test — the case asserted a property no longer worth a suite of its own once the
    duplicate rows it shared with `test-census-window.sh` were removed.
13. **G13 (verify-asks timeout/side-effect/hang)** — still open, secondary to this area.
14. **G14 (Auron → operator path checked from one side only)** — still open
    (`sp-fhzib.4`).

---

## 6. Cost

The original per-file `ci_secs` baseline (39 files, 1,399 s on main-push run 35947142904)
and the projected post-verdict total (320 s, −77%) are `sp-fhzib`'s own analysis and are not
repeated here verbatim — most of the files that baseline priced no longer exist in their
priced form (`sp-fhzib.2`/`.3` already deleted or rewrote a third of them). What this bead
measures is its own slice; see §8 for the before/after this bead is directly responsible
for. The remaining lever — `sp-fhzib.4`'s Auron/strand/watchtower/livelock/groomer/hold/
archivist demotions — is unmeasured until that bead lands, since none of its file changes
are on `main`.

---

## 7. What this bead is not

Out of scope here, per this bead's own brief: migrating the area's remaining suites onto
`testlib.sh` (`sp-l2be6`'s migrate-or-exempt decision; `sp-qvjzb` did the mechanical
migration on the retired branch, closed-not-landed) and `sp-fhzib.4`'s detector demotions
(reopened separately, tracked on its own bead, not this one's to carry).

---

## 8. Implementation status (`sp-n1twd`, this slice)

This bead replaces `sp-fhzib` itself (not `.2`/`.3`/`.4`/`.5`, which are separate beads with
their own history — `.2` and `.3` and `.5` landed; `.4` was reopened separately and has not).
What `sp-fhzib` itself still owed, on a fresh branch from `origin/main`:

- **This page and its typed catalogue.** `docs/test-plan/ops-detection-remediation.md` (not
  copied from the retired branch's draft — that draft used the retired inline-declaration
  format and its own §8/§10 claimed some `sp-fhzib.4` work as landed that is not on `main`)
  plus `docs/test-plan/ops-detection-remediation.toml`, validated clean by
  `test-plan validate --catalogue-dir docs/test-plan`. `test-census.sh` and
  `test-census-pipeline.sh`, the two suites whose `# covers:` lines already named
  `UC-ops-detection-remediation-NN` ids with no catalogue to resolve them against (a hard
  `plan-lint.sh` failure once those files are the ones being checked), now resolve clean.
- **UC-23, rewritten against current code — and a compile break found and fixed along the
  way.** `cargo test -p czar-pass` did not compile on `main`: 5 of its 22 tests
  (`fs_path_replaces_class_separators_that_would_escape_the_directory`,
  `fs_record_is_first_write_wins`, `fs_clear_removes_the_marker`,
  `latency_secs_is_zero_with_no_recorded_first_sighting`,
  `latency_secs_measures_from_the_first_sighting`) called `fs_record`/`fs_get`/
  `fs_clear`/`fs_path`/`latency_secs`, the marker-file helpers the reconciler-engine port
  (`sp-pu7v6`) deleted when it moved hysteresis into `HysteresisState`/`StateMap`. Those 5
  were dead tests for dead code and are removed. `czar-pass/src/main.rs` also gained a
  `test_config()` helper (a `Config` literal pointed at a scratch dir, avoiding
  `Config::from_env()`'s ambient-env reads) and 25 `#[test]`s exercising `detect_deadlock`,
  `detect_attribution_failed`, `detect_sort_failed`, `detect_loop_stalled`, `detect_ci`
  (both its ci-stalled and ci-red halves), `detect_base_red` and `detect_starved` at their
  fire/silent boundary: a silent case with the raw condition absent, a fire case just past
  the detector's own threshold, and — where the detector has one — an `Unobservable` case
  (a forge call that fails, a strands.json that fails to parse) proving the detector
  reports `?` and takes no remedy action rather than reading the failure as clear. All of
  it runs in Shadow stage (the default with no `SPIRA_CZAR_STAGE_*` set), so no test spawns
  a real `forge.sh workflow-rerun` or files a real incident — `det_action`/`infer` only log
  `CZAR-WOULD` in that stage, which is what a unit test wants from a side-effecting
  detector.
- **`test-czar-pass.sh` gained a case 0**: `cargo test -p czar-pass`, run against the
  suite's own isolated `CARGO_TARGET_DIR` before the release binary is built, so the unit
  layer's 42 tests (17 surviving helper tests + 25 new detector tests) are verified as part
  of this suite's own run — which is exactly the run that would have caught the compile
  break above, had it existed before now.
- **The Sin real-events row (UC-07/UC-12), deferred by `sp-fhzib.3` until `sp-fhzib.2`
  landed.** `test-census.sh` — the one suite already holding a real `testdb_up` fixture for
  UC-12/UC-14 — gained a fourth section: three Sin-escalation events for one ref (below
  `SPIRA_SIN_AT`) leave the class's detection count at 3 with no `sin` label reachable
  through the census path, and the same fixture is reused rather than a fifth
  `testdb_up`/`testdb_drop` round trip.

**Suite-seconds, this slice** (measured via `spira/testenv-batch.sh` on a shared embedded
fixture, parallel mode, single run — not averaged; never on the host):

```
Before (§7's own baseline, this slice's touched suites only, main-push run 35947142904):
  test-czar-pass.sh    14 s  (no cargo test; 5 of its 22 Rust tests did not compile —
                              cargo test -p czar-pass was never run as part of this suite)
  test-census.sh        9 s  (3 rows: bump writes, cause column, since-filter)

After (measured this run):
  test-czar-pass.sh    40 s  (+26 s: cargo test -p czar-pass — 42 tests, <0.2s wall itself;
                              the rest of the delta is a second full cargo compile of the
                              czar-pass/reconciler-engine crates under a scratch
                              CARGO_TARGET_DIR, separate from the release-binary build the
                              suite already paid for)
  test-census.sh       73 s  (+64 s: this run built its own fresh embedded testdb baseline
                              rather than reusing a warm one, which section 1-5's original
                              9 s baseline did not have to pay; the Sin row itself is one
                              `recurs_of` shell-out against data section 1 already wrote)
```

Also run this slice: `test-census-pipeline.sh` (44 s, unchanged in shape — its `# covers:`
line now resolves against a real catalogue instead of an absent one), `test-plan-lint.sh`
(33 s) and `test-plan-matrix.sh` (33 s), both exercising the new catalogue file itself.

Verified: `bash spira/testenv-batch.sh --suites test-czar-pass.sh,test-census.sh,
test-census-pipeline.sh spira/sp-n1twd` and `bash spira/testenv-batch.sh --suites
test-plan-lint.sh,test-plan-matrix.sh spira/sp-n1twd` in testenv — all five green.
`spira/plan-matrix.sh` regenerated `coverage.json`/`COVERAGE.md`;
`spira/plan-matrix.sh --check` and `test-plan validate --catalogue-dir docs/test-plan`
both confirm the committed files match.
