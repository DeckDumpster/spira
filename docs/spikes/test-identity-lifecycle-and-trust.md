# Test identity, lifecycle and a trust score: what a test is here, and what may switch one off

Spike sp-pmv67. Written 2026-09-18 against `origin/main` at `c26bd5b`. It also answers
sp-t600k, which asks the same question.

Sources are preserved under [`sources/sp-pmv67/`](sources/sp-pmv67/) and cited by their path
there. Every measurement script is in `sources/sp-pmv67/scripts/`. The same scripts, plus the
proof of concept, are on branch `spike/sp-pmv67-poc` (`0c1c05b`). That branch is **local
only**: aeons hold no push credentials, so it disappears when this worktree is reaped. Nothing
in this document depends on it, because every script it holds is also in the sources.

**This is the second time this spike has been written.** The first attempt (2026-09-17,
against `8537809`) was completed and certified, then lost before landing: an ad-hoc branch
cleanup deleted every `spira/*` branch. Its text survived in that session's transcript. I used
it as a starting draft and **re-measured every number below on today's tree** unless a row
says otherwise. Where today's evidence changed a conclusion, the conclusion changed. §8 says
which ones.

---

## 1. The question

Asked by the operator on 2026-09-17:

> Take a strong opinion about the structure of suites and tests. Individual tests should have
> an identity and we should be able to lifecycle them. When one is suspected flaky, there
> should be machinery that produces data about that rather than a label. Every test in the
> system should carry a TRUST SCORE built from its record: how many real problems it has
> halted, how many false failures it has produced, and so on.

An answer has to do five things:

- name the **unit** that carries identity, and show that its name lasts long enough to build
  up a history;
- say what the machinery **does** when it suspects a flake, instead of what it concludes;
- define a **record** that cannot become a way to switch off a check that has caught
  something;
- say **where** that record lives and who writes it;
- cost all of it against doing nothing structural.

The bead adds three constraints, and this design treats them as hard rules:

- a check that has ever caught a real defect is never a candidate for automatic disarming;
- absence of evidence is not reliability;
- the design must be readable by an agent holding one red test.

## 2. The answer in one paragraph

**Take automatic quarantine out entirely, and record before you score anything.** The harness
already prints almost everything a trust record needs: 337 of the 338 suites with a green run
on record print one named line per assertion. It already keeps 3 days of those lines on disk, and 130,784 per-assertion
observations came out of them in 2.2 seconds without editing a suite (§4.5). What it lacks is
three facts on every observation:

- **which tree was tested.** Today it records the branch name it was *asked* about, and the
  runner tests a different tree anyway (sp-2f51e).
- **whether any test ran at all.** 67% of recorded suite reds are runner collapses: one byte of
  output in zero seconds.
- **how the red was resolved:** by fixing the code, or by editing the test.

Every automatic quarantine on record was either wrong or unexamined:

- 15 in one day, across 10 suites;
- none shown to be flaky by any experiment;
- none ever lifted automatically.

And under the gate's existing serial retry, a quarantine changes a verdict **only for a suite
that is red twice**, which is exactly the suite that is not flaky (§4.3). The unit of *record*
is the assertion; its label is stable enough (§4.6). The unit of *action* stays the suite file,
and nothing automatic may act on it. The score is two numbers and is never collapsed into one:

- **catches** (defects halted, confirmed by a fix to covered code), which never decays;
- **noise**, split by measured cause.

It decides nothing on its own; it is printed into the bead of the agent holding the red.
Per-assertion quarantine is rejected: it makes disarming cheaper and harder to see.

## 3. If you are holding one red test right now

This works on today's tree. §5 describes what should replace it.

1. **Did anything run?** If the suite's output is empty or holds no `ok`/`FAIL` line, what you
   are looking at is the runner, not the test. 433 of 649 recorded suite reds are exactly this
   (§4.5). File against the runner, not the suite.
2. **Which tree was tested?** Until sp-2f51e lands, a local reproduction through
   `testenv-batch.sh <branch>` tests the installed checkout, whatever branch you name. A green
   there says nothing about your branch.
3. **Does the verdict depend on anything but the content?** Run the suite serially, then
   N-way concurrently, then with private fixture copies, all on one tree (§4.7 shows how,
   with no database build). Opposite verdicts on one tree name the cause. Report the number of
   runs as well. By the rule of three, ten green runs only bound the failure rate below 30%
   at 95% confidence.
4. **Has this check caught anything?** Read its `# scar:` line and `git log -- <suite>`. If it
   has caught a real defect, it is not yours to disarm.
5. **Never make a check pass by editing its expected value** unless the behaviour it asserts
   was deliberately changed. If it was, say so in the commit. Today nothing records the
   difference (§4.9).

---

## 4. What I found

### 4.1 The incident, and what has moved since it

The chain on 2026-09-17, now read in the code:

- `gate-retry.sh` re-runs a batch's red suites serially. Red then green prints
  `::warning title=flaky suite::`. Red twice prints `::error title=red-twice suite::`.
  `forge.sh` maps these to `flaky:` and `red-suite:` lines (forge.sh:110-126).
- Until 2026-09-18, the merge-queue attribution read the `red-suite:` lines (the gate's own
  statement that these suites are **not** flaky). When nothing reproduced, it called
  `observe-flake` on exactly those suites, with the threshold overridden to 1.
- Nothing could reproduce, because the reproduction tests the installed checkout rather than
  the branch (sp-2f51e). The quarantine branch was therefore the only one reachable.

sp-6zw2p removed that call and **landed** (`255eec4`, verified with
`git merge-base --is-ancestor 255eec4 origin/main`). sp-2f51e is **still open**, at P0.

The fix for the shared-fixture race that the first attempt measured (sp-4f0oe) is **closed but
not on main**. `a4628b5` is reachable only from queue branches. The suite it added,
`test-testdb-concurrent.sh`, was red twice in CI. The cause was a defect in the suite itself:
it read an exit status through a file written after a call that can `exit` (sp-y8bxp,
sp-9wdji). So the check was deterministically wrong, not flaky. Meanwhile:

- the local gate reported it red in five consecutive batches;
- attribution could not name a guilty member in any of them;
- the queue logged `ejected 0, requeued 6` each time (PRs 63, 65, 66, 67, 68);
- the flake observer charged that one suite 12 observations, in four bursts of three, each burst within 2 seconds.

*Sources: `beads/sp-2f51e.txt`, `beads/sp-6zw2p.txt`, `beads/sp-4f0oe.txt`, `beads/sp-y8bxp.txt`,
`beads/sp-9wdji.txt`, `logs/landing-flake-excerpt.txt`.*

### 4.2 Every automatic quarantine on record

From the landing log for 2026-09-17 and 18:

| | count |
|---|---|
| `auto-quarantine:` lines | **15**, across **10** suites |
| of which logged `(bead: (none filed))` | **15** |
| `fix <suite>: flaky` beads that nonetheless exist | **23** (14 closed, 9 open) |
| flake observations on file | **54** in 16 ledgers, mostly in bursts of 2-3 within 10 s |
| attribution verdicts in the window | 9, **all** `ejected 0` |
| quarantines lifted by `suites.sh hygiene` | **0** (`hygiene: 0 reactivated`, the only output on record) |

What is actually known about the 10 suites:

| suite | what it was |
|---|---|
| `test-cockpit-landed.sh`, `test-cockpit-probe-fault.sh` | **red twice**: correctly catching a branch that changed behaviour without updating them (sp-1vn7j close reason) |
| `test-testdb-concurrent.sh` | **red twice**: a defect in the test itself (sp-y8bxp) |
| the other seven | never examined by any experiment |

`test-dummy.sh` is not in the table: no `auto-quarantine:` line names it. What happened to it
is worse (§4.4).

**Not one automatic quarantine was shown to be a flake, and not one ever ended without a person.**
The reactivation rule needs the quarantine's bead to carry `land_state:LANDED` plus ten clean
runs (suites.sh:1409-1450). But these quarantines recorded no bead id at all (§4.4). And a
"fix the flaky test" bead for a correct suite closes without landing anything (sp-1vn7j). Entry
is automatic; exit is impossible.

*Sources: `logs/landing-flake-excerpt.txt`, `logs/flakeobs-ledgers.txt`,
`beads/flaky-bead-census.txt`, `beads/sp-1vn7j.txt`, `beads/sp-iaxck.txt`.*

### 4.3 A quarantine changes the gate's verdict only for suites that are red twice

`testenv-batch.sh` runs a quarantined suite as normal. If it fails, it writes
`quarantined-red` and does not count it toward the batch's exit status (testenv-batch.sh:
300-330, 832-900). `gate-retry.sh` then re-runs every red suite serially and passes the gate
when they all go green.

So for the merge gate, a suite that is red then green **already** passes without any
quarantine. The only case where a quarantine changes the gate's answer is a suite red on the
first run *and* on the serial retry: the red-twice case, which the gate itself classifies as
not flaky.

Elsewhere, a quarantine's effect is to stop the timed runner filing a bead (`quarantined-red`
maps to skip, suites.sh:833-836). That runner blocks nothing.

*Inferred from reading the code, not observed end to end.* But the inference is short, and it
says automatic quarantine cannot help with flakes in the gate. It can only hide failures that
repeat.

### 4.4 The label turns into a work instruction, and the machine made up a test

`_suite_auto_quarantine` gets the new bead's id with `bead.sh file ... | tail -1 | tr -d
'[:space:]'` and accepts it only if it matches `[A-Za-z0-9-]` (suites.sh:1370-1375). `bd create`
puts the id on its *first* line (`✓ Created issue: sp-b44 — probe title two`). Its last line
is `Status: open` or a `💡 Tip:` banner; I captured both on this session's fixture. So:

- **every filing succeeds**;
- **the id is always rejected**;
- the quarantine is written with an empty bead;
- the log says `(none filed)`;
- the next observation files again.

This one line explains both the duplicate beads (sp-5cnem) and the missing ones (sp-zmd3u,
defect 3). It is law-never-derive-an-id-from-output, broken inside the machinery.

The title is `fix <suite>: flaky`, and an aeon acts on the title:

- Seven such beads were filed for `test-dummy.sh`, a suite that **did not exist**.
- Where they came from: while working sp-zmd3u, an aeon wrote a regression test that runs the
  real `suites.sh observe-flake test-dummy.sh`. The test isolated `SPIRA_SUITES_STATE` but not
  the bead store, which `conf.sh` resolves to production. So every run of that test filed a
  real bead. This is *inferred* from that aeon's session log: the test was written at 18:17Z
  and the beads appeared from 18:23Z to 18:56Z.
- The aeon holding sp-40a5p "fixed" it by **creating** `spira/test-dummy.sh` (a new test of
  `suite-covers.sh`) and committing a quarantine line for it that points at its own bead.
- That line is in `spira/suite-state` on `origin/main` today.
- sp-40a5p is closed.

`suite_state_lint`, which would refuse a quarantine without a bead, **is called by nothing**
(`grep -rn suite_state_lint spira/` finds only its definition). That is a documented control
that does not exist.

*Sources: `logs/bd-create-last-line.txt`, `logs/suite-state-snapshots.txt`, `beads/sp-40a5p.txt`,
`beads/sp-5cnem.txt`, `beads/flaky-bead-census.txt`, `logs/sp-zmd3u-observe-flake-test-excerpt.txt`.*

### 4.5 The record already exists, and it cannot answer the one question that matters

`testenv-batch.sh` keeps one directory per batch in `$SPIRA_RUN/batch-results/`, with
`<suite>.result` and `<suite>.out` for every suite. **658 directories, 61 MB, from
2026-09-15T05:34Z to 2026-09-18T01:38Z.** `scripts/harvest-corpus.py` turns every `ok`/`FAIL`
line into one row.

| measured | value |
|---|---|
| wall time to harvest all of it | **2.2 s** |
| rows | **130,784**: 129,142 `ok`, 998 `fail`, 644 suite-runs with no assertion line |
| batches / suite-runs / distinct suites | 649 / 6,873 / 356 |
| distinct `suite::label` keys | **7,589** |
| suites edited to get it | **0** |

What the reds are made of:

| red suite-runs | 649 |
|---|---|
| empty output (one byte, 0-1 s): **the runner, not the test** | **433 (67%)**, all from **6** batches, one of which marked **308 of 317** suites red |
| suite could not be found or died on an unbound variable | 7 |
| printed `ok` lines, then exited non-zero with no `FAIL` line | 2 |
| at least one named `FAIL` line | **207** |
| partial reds (some assertions ok, some failing) | **204**, holding **4,767** passing assertions beside **993** failing |

Two consequences follow:

- **Any counter over suite reds is mostly counting the runner.** A per-suite flake count fed
  from this data would have charged 308 suites for one collapsed container.
- **Quarantining a file disarms about 4.8 working assertions for each failing one**, averaged
  over the partial reds on record. The incident's "2 ok, 4 fail" is the typical shape, not an
  odd case.

Per assertion: 439 keys ever failed, in 81 suites. 356 of them both failed and passed. 43 failed
only under parallel while passing under serial; 44 the reverse.

**What it cannot answer: was any of those 998 failures a real defect?**

- `batch.meta` records `image_tag`, `branch`, `base`, `key`, `mode` and `selection`, and **no
  tree**.
- The branch is the one the runner was *asked* about. Because of sp-2f51e, it is not the one
  it tested.
- So no two observations can be proven to be of the same content, and "red then green on the
  same content" is the definition of a flake.

130,784 observations, and the provenance of every one is unknown. sp-t600k made this point
(its finding 5); this measures it.

*Sources: `data/corpus-suite-runs.tsv` (one row per suite-run), `data/corpus-fail-rows.tsv`
(all 998 failing rows), `data/corpus-by-key.tsv` (the 7,589 keys with ok/fail counts by mode,
plus one `-` row per suite that ever ran without printing an assertion),
`scripts/harvest-corpus.py`. The 14 MB raw harvest is not kept. It is regenerated by the
script from the runtime directory, which is local to the box and pruned.*

### 4.6 An assertion's label is a usable identity

The identity proposed in §5.1 is `<suite file>::<normalised label>`. Three measurements test it.

**Same tree, two runs** (40 fast suites, minimal environment, `scripts/run-sample.sh`):

| | run 1 | run 2 |
|---|---|---|
| assertion lines | 487 | 487 |
| distinct keys | 480 | 480 |
| keys in only one run, raw | 2 | 2 |
| keys in only one run, after the normaliser `suites.sh fingerprint()` already applies | **0** | **0** |

The raw churn is two labels in `test-containment.sh` that print a `mktemp` path. There were
zero cross-suite label collisions. Six labels repeat within a suite (13 of 487 lines, 2.7%);
the second and later get an ordinal.

**Across commits** (`scripts/label-stability.py`, static extraction of literal labels, 5,643
at `c26bd5b`). "Absent" means an older label missing today, in a suite that still exists:

| older revision | absent | of which a likely rename | of which a deletion |
|---|---|---|---|
| 2026-09-16 (50 first-parent commits back) | 2.2% | 1.0% | 1.2% |
| 2026-09-15 (200 back) | 3.9% | 1.9% | 2.0% |
| 2026-09-12 (400 back) | 4.2% | 1.4% | 2.7% |
| 2026-09-09 (800 back) | 9.1% | 2.8% | 6.3% |
| 2026-09-07 (1,100 back) | 92.4% of 17 suites | 8.2% | 84.3% (the first day's suites were rewritten wholesale) |

**Most "renames" are not renames.** I read 12 at random. About 9 are the assertion's *claim*
changing:

- `session share is 25%` → `20%`
- `creates exactly one ask` → `sends exactly one mail`
- `SP_UNLANDED counts irrecoverable only` → `SP_UNLANDED_N counts anomalies only`

About 3 are cosmetic (`installs no promote unit` → `does not install promote`). A new label
usually means a new claim, and a new claim *should* start a new history. So only about a
quarter of 1-3% of labels over a week are identity lost for nothing.

**Runtime labels** (the harvested corpus, first 24 h against last 12 h, 48 suites) lose 24.2%.
But **265 of those 346 lost keys come from one suite**, `test-timer-templates.sh`, which names
its assertions after live systemd unit names. Without it the figure is 7.4% over 2.8 days, and
that includes real rewrites. `test-chamber-repo-labels.sh` does the same with live bead ids
(§4.10). **A small number of suites cause the churn**, and the fix is per suite: name the case,
not the data.

*Sources: `data/same-tree-run1.tsv`, `data/same-tree-run2.tsv`, `data/label-stability.txt`.*

### 4.7 The discriminating experiment works, and the race is still live on main

`test-cockpit-landed.sh` on one tree (`c26bd5b`), using this session's embedded shared fixture.
"Private" means a `cp -a` of the fixture directory per run, sharing the read-only baseline.
**No database was built.**

| condition | runs | result (from each run's own summary line) | wall |
|---|---|---|---|
| serial, shared | 10 | 10 × `5 ok, 0 fail` | 145 s |
| 6-way concurrent, shared | 6 | 2 × `5 ok, 0 fail`; 4 × fixture collapsed before any assertion | 28 s |
| 6-way concurrent, private | 6 | 6 × `5 ok, 0 fail` | 83 s |
| 12-way concurrent, shared, 3 rounds | 36 | 3 × `5 ok, 0 fail`; 28 × collapsed; **4 × `0 ok, 5 fail`; 1 × `2 ok, 3 fail`** | 57 s |

The failing assertions read counts from the fixture store and get empty values: `SP_LANDED
counts committed work: wanted [1] got []`. This is yesterday's `2 ok, 4 fail` on a suite that
has since lost one assertion (§4.9). One variable separates the red rows from the green ones,
and it is not the content. **Five minutes of wall time names the cause, with a control on
both sides.**

The experiment also shows two things the machinery must build in:

- **An experiment has power, and a null result has to report it.** The 6-way run showed no
  assertion failure; the 12-way run showed 5 in 36. A ladder that ran only the 6-way case
  would have said "no disagreement", which is exactly the conclusion a broken reproduction
  gives (§4.1).
- **My own first tally was wrong.** The runner recorded `rc=$?` after a command substitution in
  the same `echo`, so every run read as `rc=0` while 23 FAIL lines sat in the output. I caught
  it only by reading the outputs. Every verdict above comes from the suite's own summary line.
  A differential runner needs its own positive control for the same reason any check does.

*Sources: `data/experiment-concurrency.txt`, `data/poc-ledger.ndjson.txt`,
`data/poc-trust-show.txt`.*

### 4.8 The catch record nearly exists, and cannot be joined to a test

`yield.sh` gives every gate red a verdict: `DEFECT`, `GATE_FAULT` or `UNKNOWN`. It infers
`DEFECT` when "the branch was refused, then changed, and the changed branch passed". That is
the catch predicate this design needs. The ledger today:

| records | 57 |
|---|---|
| `DEFECT` | 27, of which **25 name no suite** |
| `GATE_FAULT` (the gate's own taxonomy) | 14 |
| `UNKNOWN` | 16 (11 of them `test-testdb-concurrent.sh`) |

Three things stop it becoming a per-test record:

- `gate.sh` names **at most one** suite. It takes the first match of `<suite>.sh RED` in the
  gate's output (gate.sh:593-601), and matched nothing on 25 of 27 defects.
- The output that would name the suite is **overwritten**: `gate-run/<branch>/out` holds only
  the latest run, which after a fix is the passing one.
- Records are **pruned after 30 days** (`SPIRA_YIELD_KEEP_DAYS`). A catch record must never
  expire.

*Source: `logs/gate-yield-summary.txt`.*

### 4.9 How a caught regression was resolved: by editing the test

On 2026-09-17, `test-cockpit-landed.sh` and `test-cockpit-probe-fault.sh` were red twice
against sp-d0j1g. The concierge correctly refused to call that flaky. sp-d0j1g had *deliberately*
redesigned the cockpit's landing section, and the resolution (`0ebd0ca`, "fix test suites
broken by UNLND→QUEUE rename") rewrote the suites to match. It deleted
`SP_AWAITING_LAND counts branch-present work` and the suite's whole "three states, not two"
rule.

That was a legitimate change of claim. Two things are still wrong with it:

- The suite's `# scar:` line still says it guards against "never-landed falsely flagged healthy
  landing queues as irrecoverable". **The assertion that guarded that is gone.** The scar
  describes a control that no longer exists, and the first attempt's proof of concept (and
  mine) read it as `catches 1`.
- **Nothing anywhere records** that a check which had just caught something was rewritten, or
  why.

How often does this shape happen? `scripts/assertion-edits.py` looked at 1,054 non-merge
commits:

| (commit, suite) pairs changing an existing suite's assertion lines | 470 |
|---|---|
| also changed a file the suite's `# covers:` names | 307 (65%) |
| the suite had no `# covers:` line at the time | 96 (20%) |
| **changed only the suite** | **67 (14%)**, of which 11 removed more assertion lines than they added |

`0ebd0ca` is one of the 67. Those 67 are the set in which a check is being "edited to pass" or
its claim is moving; no machine can tell those apart. A person can, **if the machine asks at
the moment it happens.**

*Sources: `logs/cockpit-landed-resolution.txt`, `data/assertion-edits.txt`, `beads/sp-d0j1g.txt`.*

### 4.10 A suite whose verdict depends on production data

`test-chamber-repo-labels.sh` reads the **production** beads store. `conf.sh` derives
`SPIRA_DB` even under `env -i`. It names one assertion per live bead
(`bead sp-4dhqg: repo:spira resolves`), so its identities churn with production data. It was
red in both of my runs on `bead sp-114e5: repo:fixture-repo: does not resolve in repo-map`.
sp-114e5 is an open bead titled `test bead`, a fixture that leaked into the production store.

The timed runner recorded this suite `ok` the previous day. **Its verdict flips with the state
of the box, not with the code**, which is law-gates-run-in-a-clean-environment. Filed as
**sp-skk5k**.

*Source: `logs/chamber-repo-labels-reads-production.txt`.*

### 4.11 Costs, measured

| | value | how |
|---|---|---|
| `bd kv set` / `get` | **288 ms / 294 ms** per call | 20 calls each, this session's embedded fixture, warm |
| harvesting per-assertion rows | **2.2 s for 130,784 rows** (≈17 µs/row) | §4.5 |
| raw ledger volume | 14.4 MB for 2.8 days ≈ **5 MB/day**; 1.8 MB gzipped ≈ **0.6 MB/day** | §4.5 corpus |
| catch events to store durably | about **9/day** (27 `DEFECT` records in about 3 days) | §4.8 |
| aeon cost per bead | median **$2.05**, mean **$3.61**, p90 $5.83 (n = 222 bead logs) | `total_cost_usd` summed per `$SPIRA_RUN/sp-*.log` |
| non-test lines per bead commit | median **37**, p75 **94** (n = 266 commits since 2026-09-12) | `git log --numstat` |

§6 turns lines into beads using the p75 of 94 lines per bead, which is deliberately
pessimistic. It then turns beads into dollars at the mean, $3.61. Neither figure counts
re-attempts or gate time, so both costs are floors.

### 4.12 What I could not establish

- **Whether any of the 998 recorded assertion failures was a real defect.** The corpus does not
  name the tree it tested (§4.5). This is the finding, and it means the catch count has
  **no history to backfill**: it starts at zero on the day it is built.
- **Whether an uncommitted automatic quarantine in the installed checkout could ever reach a
  batch.** `testenv-batch.sh` reads `suite-state` with `git show <branch>:spira/suite-state`,
  the *committed* file. The timed runner and the cockpit read the *working* file. The two
  readers disagree by construction. I did not trace whether batch assembly can pick up the
  working change.
- **Label stability beyond 12 days.** The repository's history begins on 2026-09-06. The
  deletion rate (6.3% at 9 days) is mostly suites being rewritten; the rename rate is what
  matters, and it is under 3% everywhere I could measure. I cannot say what it is at 90 days.
- **The flake rate of the seven quarantined suites no experiment examined.** I ran the
  experiment only on the suite at the centre of the incident.
- **The CI failure text of `test-testdb-concurrent.sh`.** It is on GitHub, and this session has
  no GitHub credential. I relied on sp-9wdji and sp-y8bxp, which quote it.

---

## 5. The design

### 5.1 Identity: three nouns, not one

| noun | identity | what it is for |
|---|---|---|
| **suite** | the file path under `spira/` | the unit that is **run, selected and controlled** (active / quarantined / disabled / retired). The unit of *action*. |
| **assertion** | `<suite>::<normalised label>[#<ordinal>]` | the unit of **record**: what caught, what was noisy. Never the unit of automatic action. |
| **observation** | one row: assertion (or `-`), verdict, tree **actually tested**, runner verdict, condition, clock, producer | the only thing anything is ever computed from |

- **Normalised** means the sed pipeline `suites.sh fingerprint()` already runs; do not write a
  second one. **Ordinal** is appended to the second and later repeat of a label within one run.
- **A rename is a new identity, and that is correct.** Most label changes are changes of claim
  (§4.6), and moving an old catch record onto a new claim is the one error this design cannot
  tolerate.
- **The catch count is also rolled up to the suite file.** The rule "a check that has caught
  something is never disarmed automatically" is read at the file, which is the unit anything
  could disarm. So a renamed assertion cannot orphan it.
- **A suite whose labels carry data** (`test-timer-templates.sh`, `test-chamber-repo-labels.sh`)
  has no durable assertion identity. The harvester reports it as `unstable-identity` once its
  same-tree churn is above zero; that is a per-suite fix, not a design exception.
- **An observation with no assertion is still an observation, of the runner.** The 433
  zero-second reds (§4.5) are recorded as `runner:collapsed` and are never charged to a suite.

### 5.2 Lifecycle: record states and control states are different things

The first attempt folded both into one table. They have different writers, so they are split.

**Record states**, per assertion, derived from observations and never written by hand:

| state | meaning | evidence that enters it |
|---|---|---|
| `unobserved` | no observation on record | default |
| `unproven` | observed, never seen red for a content reason | green observations only. **Renders as "no data", never as a score.** |
| `proven` | seen red for a content reason at least once | a confirmed catch (§5.4), or its own seen-red-first control recorded by the bead that wrote it |
| `suspect` | two observations of **one tree** disagree | the disagreement itself, never a count across trees |
| `explained:<cause>` | the differential named a non-content cause | §5.3, with a bead filed **against the cause** |
| `confirmed` | red under every condition tried, on one tree | §5.3 |

**Control states**, per suite file, written only through `_sts_transition`, which already
refuses aeons, goes through a branch and lands through the queue:

| state | who may enter it | who may leave it |
|---|---|---|
| `active` | default | — |
| `quarantined` | **a person or Ops, never an automatic path**; a bead id is required and `suite_state_lint` enforces it in the gate | the same, or `activate` |
| `disabled` | a person | a person |
| `retired` | a `# retired:` declaration in the suite (§5.5) | — |

Four things this asserts that the current machinery does not:

1. **`suspect` switches nothing off.** A suspicion is a reason to run an experiment.
2. **`explained` files against the cause, not the test.** "Concurrent borrowers of the shared
   fixture reset it under each other" can be closed, and it is one bead for every suite that
   shares the cause, which also deduplicates it. `fix <suite>: flaky` is an instruction to make
   a check stop failing, and §4.4 shows an aeon carrying it out literally.
3. **Entering and leaving a quarantine cost the same.** Both are one `_sts_transition` by a
   person. Today entry is automatic and exit is impossible (§4.2).
4. **The record is per assertion and the control is per file.** "SP_LANDED is explained by
   shared-fixture contention" is recorded without switching off the four assertions beside it.

### 5.3 Evidence, not labels: what the machinery does with a suspect

Run in cost order and stop at the first rung that decides:

| rung | question | cost | if yes |
|---|---|---|---|
| 0. run facts | did any assertion run? | free: count the lines already captured | `runner:collapsed`; charge nobody |
| 1. tree | is the tree tested the tree asked about? | free once the runner records `git rev-parse HEAD^{tree}` from inside the mount | if not, the observation is void (sp-2f51e) |
| 2. repeat | same tree, same conditions, N times | 145 s for N = 10 (§4.7) | disagreement: non-determinism inside the suite |
| 3. concurrency | serial vs N-way | 28-57 s | ordering or contention |
| 4. isolation | shared fixture vs private copies | 83 s | shared-fixture contention |
| 5. environment | with vs without the runner's injected variables | the timed runner's existing confirming run (`RUNNER_VARS`) | environment |
| 6. content | the branch tree vs its base tree | two suite runs | **a real defect: stop, and never call it flaky** |

Two rules apply to the whole ladder:

- **A null result carries its power.** "Green in 10 of 10" is reported as "failure rate below
  30% at 95%", not as "not flaky". §4.7's 6-way run is the example.
- **The ladder proves it can see before it is believed.** It must first find a disagreement it
  was seeded with, a known-bad tree at rung 6, or refuse to give a verdict. A differential
  that tests the wrong tree says "no disagreement, therefore flake", which is sp-2f51e
  exactly.

### 5.4 The trust record: two numbers and three flags, never one scalar

**A single scalar is how a check gets switched off.** A number invites a threshold, and a
threshold is what quarantined `test-cockpit-landed.sh`.

**`catches`: monotone, never decays, never weighed against anything.** The count of distinct
defects this assertion (and, rolled up, this suite) halted. A catch is recorded when **all**
of these hold:

- a gate refused a branch with this assertion red on tree T1;
- the branch then passed on tree T2 ≠ T1 with it green;
- **the diff T1→T2 touched a file the suite's `# covers:` names.**

The third condition is what `yield.sh`'s `auto:changed-then-passed` lacks. It separates
"fixed the code" (a catch) from "changed the test" (§4.9), which is recorded as a flag instead.

**`noise`: counted over a window, split by the measured cause, never summed.**
`{runner-collapse: n, wrong-tree: n, shared-fixture: n, concurrency: n, environment: n,
test-defect: n, unexplained: n}`. The split is the product:

- "17 false failures" invites division.
- "5 false failures, all the reproduction reading the wrong tree" is a bead against sp-2f51e,
  and charges the test **nothing**.

Only `test-defect` (red twice, and the fix touched only the test, as in sp-y8bxp) and
`unexplained` count against the test itself.

**Flags, printed beside the numbers and never added into them:**

- `unproven`: no content-caused red on record. **Rendered as "no data", never as 100% or
  1.0.** A panel that prints a number here will have a threshold written against it within a
  month.
- `resolved-by-test-edit: n` counts reds that ended with the suite changed and no covered
  file touched. This is the one signal that an assertion is being edited to pass, or that its
  claim is moving. §4.9: 14% of assertion edits.
- `unstable-identity`: same-tree label churn above zero (§5.1).

### 5.5 What the record is allowed to decide

| decision | allowed |
|---|---|
| what an agent holding one red reads first: the record is printed into the red's bead body | **yes. This is the point** |
| which suspect gets the differential budget this hour | yes |
| how a red's bead is titled: for the measured cause, or as the question "why does X fail only under Y", never "fix X: flaky" | yes |
| whether to quarantine | **never, for any value.** Quarantine is a person's `_sts_transition` |
| whether a red warns instead of blocking | **not in the recommended option.** At most, later: `catches == 0` **and** a measured non-content cause **and** an open bead against that cause, all three |
| anything at all about a suite with `catches > 0` | **never automatically** |

A diff that deletes an assertion with `catches > 0`, or changes its label, is refused by a gate
fence unless the suite carries `# retired: <old label> — <why>`. That is one line, written by
whoever makes the change, at the moment they know why. It is exactly the record whose absence
made §4.9 invisible. With no catches recorded, the fence never fires, so it costs nothing until
the record has something worth protecting.

### 5.6 Where the data lives, and who writes it

| | observations | catches and flags | control state |
|---|---|---|---|
| **where** | `$SPIRA_RUN/trust/obs-YYYY-MM-DD.tsv`, append-only, rotated, gzipped after a day | `bd kv`, key `trust/<suite>::<label>`, written only when something changes | `spira/suite-state`, **in the tree**, as today |
| **writer** | the runner that produced the verdict, stamping its own clock and the tree it measured from inside the mount (law-producers-stamp-their-own-clock) | the gate's pass path in `yield.sh`, when a branch refused on T1 passes on T2 | `_sts_transition` only |
| **survives** | the box. Expendable by design; it is evidence, not the record | the beads store, which is outside every repository and every branch | a branch, deliberately |
| **cost** | about 5 MB/day raw, 0.6 MB/day gzipped (§4.11) | about 9 writes/day × 288 ms ≈ 3 s/day | unchanged |

**The record is not in the tree under test.** It is a fact about a test's history across all
branches. Kept in the tree, a branch could carry its own history, and history would diverge
at every merge.

**The control state stays in the tree.** Because it is there, a quarantine goes through a
branch and the gate like any other change, and one test's control state is the same for every
reader of that tree. What is wrong today is not where it lives. What is wrong is that the
automatic path writes the *installed* working copy, uncommitted, bypassing that route (§4.12).
Removing the automatic path removes the defect.

**Not one `bd kv` write per observation.** At 288 ms each, the 43,600 rows a day in §4.5 would
cost 3.5 hours a day. Observations go to a file; only catches, which are rare, go to the store.

### 5.7 Migration: no flag day

- **No suite is edited for any increment below.** 337 of the 338 suites with a green run in
  the corpus print one `ok <label>` / `FAIL <label>:` line per assertion. They use six textual
  variants of an `ok()` helper, or print the line inline, and the extractor handles all of them.
- **The one that does not, `test-suite-state.sh`** (it prints `ok: <label>`), is recorded at
  file level, with assertion `-`, until someone converts it.
- **Retained batch results are harvested once, as `provenance: unknown`.** They seed
  identities and noise counts, **never catches**, because they cannot name their tree
  (§4.5).
- **`# scar:` lines are imported as `catches` at file level, flagged `scar-unverified`**, until
  a recorded catch replaces them. §4.9 shows why a scar alone cannot be trusted.

---

## 6. Options, each with a cost and a risk

Lines of code are estimated against comparable existing files: `gate-retry.sh` is 43 lines;
the proof of concept is 120 plus 130; `yield.sh`'s record path is about 60. Lines are turned
into beads at the p75 of 94 lines per bead, and beads into dollars at the mean of $3.61
(§4.11).

### Option A: fix the bugs, build nothing structural (the "no")

- **Scope:** sp-2f51e (wrong tree, open P0), sp-5cnem plus the id parse (§4.4), sp-iaxck
  (observations counted per annotation), sp-y8bxp (the concurrent suite's own defect), and
  wiring `suite_state_lint` into the gate (**sp-2wx6v**, filed).
- **Cost:** about 5 beads, **about $18** of aeon time. All five beads already exist.
- **Buys:** today's incident cannot recur in today's form. Duplicate and phantom beads stop.
- **Does not buy:** any answer to "has this check ever caught a real defect?" It leaves
  automatic quarantine in place, and that path has no true positive on record (§4.2); a
  corrected threshold only makes it fire more rarely. It leaves reds the machinery cannot
  tell apart from 67% runner collapse.
- **Risk:** the specific failures are fixed and the class is not. That class produced sp-zmd3u,
  sp-2f51e, sp-6zw2p, sp-5cnem, sp-iaxck and this bead in two days.

### Option B: record first, remove automatic disarming *(recommended)*

A, plus the following, in this order:

| increment | what | lines | beads |
|---|---|---|---|
| B1 | **delete the automatic quarantine path.** `observe-flake` records an observation and files a bead titled for the question, keyed on (suite, run) | about −60 / +40 | 1 |
| B2 | every result carries **the tree measured from inside the mount** and a **runner verdict** (ran / collapsed / killed) | about 40 | 1 |
| B3 | harvester: one observation row per assertion, from `testenv-batch.sh` results and the timed runner's captured output | about 120 (PoC: 48 + 40) | 1-2 |
| B4 | `yield.sh` records **every** red suite and its failing assertion keys, splits `changed-then-passed` by whether a covered file changed, and writes catches to `bd kv` without pruning | about 100 | 1-2 |
| B5 | `trust-show` printed into every red's bead body, where the agent will read it | about 80 (PoC: 80) | 1 |
| B6 | the `# retired:` fence for assertions with `catches > 0` | about 60 plus a suite | 1 |

- **Cost:** about 500 lines including positive-control suites. **About 8-9 beads plus A's 5,
  so about $47-50.** Runtime: +2.2 s per full harvest; about 3 s/day of `bd kv` writes;
  0.6 MB/day of disk.
- **Buys:**
  - the catch record starts filling on the day B4 lands, and it cannot be backfilled;
  - the agent holding a red gets its history, not a label;
  - no automatic path can disarm anything;
  - a check that has caught something cannot be quietly rewritten.
- **Risk:** a ledger nobody reads is a false record of diligence. That is the very failure
  `suites.sh` was built to end. B5 is in the option to prevent it: the bead body is the one
  consumer an agent cannot skip. **The second risk is B4's predicate.** A rebase changes the
  tree too, so a rebase that touches a covered file will be recorded as a catch. That errs
  toward protecting a check, which is the safe direction.

### Option C: B plus the differential ladder, run automatically

- **Adds:** the §5.3 ladder, in its own budget slot (`SPIRA_TRUST_DIFF_BUDGET`, default 300 s),
  run against the oldest suspect each hour. Seeded-control rung 6 included.
- **Cost:** about 200 more lines, **3-4 more beads, so 16-18 in all, about $58-65**. Runtime:
  about 300 s per hour of box time. Rungs 2-4 measured 256-285 s per suspect (145 + 28-57 +
  83, §4.7).
- **Buys:** "why does this fail only sometimes" answered by the machine, with a cause.
- **Risk:** the ladder becomes the thing that is wrong. It runs suites, so it inherits every
  failure mode a suite has. My own first tally of §4.7 read 36 of 36 runs as green because of
  one misplaced `$?`. And today most noise needs no ladder: §4.5's reds are 67% runner collapse
  plus a reproduction that tests the wrong tree. Rungs 0 and 1, which B already provides,
  classify both. **Build this only once B's record shows how many suspects survive rungs 0
  and 1.**

### Option D: per-assertion control, and a score that gates

- **Adds:** assertion-level `quarantined`, with every assertion helper consulting state
  before running. A red below some trust score warns instead of blocking.
- **Cost:** C plus about 250 lines, plus a **mechanical edit to all 338 suites** to route
  assertions through a shared helper (1 of 343 sources `suite-assert.sh` today). About **15-20
  beads on top of C, so 31-38 in all, about $110-137**.
- **Buys:** the narrowest possible disarming. `test-cockpit-landed.sh` would have kept its
  passing assertions.
- **Risk, and why I reject it:**
  - It makes disarming **cheaper and finer-grained**, the opposite of what two days of evidence
    ask for.
  - A suite silently missing three of six checks is harder to notice than one that is visibly
    off.
  - It puts per-assertion control state in the tree, multiplying the two-readers disagreement
    of §4.12 by ten.
  - A score that decides warn-vs-block is a threshold, and a threshold is what this bead is a
    reaction to.

---

## 7. Recommendation

**Do A now, then B in the order B1 → B2 → B4 → B3 → B5 → B6. Build C only when B's data asks
for it. Do not build D.**

- **B1 comes first** because it is the only increment that stops harm rather than adding
  knowledge. Automatic quarantine has no true positive on record (§4.2). Under the gate's
  retry it only ever changes a verdict for red-twice suites (§4.3). And it is the route by
  which the machine wrote "make this check stop failing" into an aeon's instructions (§4.4).
  Removing it costs about one bead and loses nothing the retry does not already provide.
- **B2 comes before any harvesting**, because an observation without its tree cannot support
  any conclusion. §4.5 has 130,784 observations and cannot name one defect.
- **B4 comes before B3**, because catches are the number that protects a check, and they
  cannot be backfilled. Every day without B4 is a day of that evidence lost.

**The one load-bearing assumption:** that **a red resolved by a change to a file the suite's
`# covers:` names is a real catch, and a red resolved without one is not.** The whole
protective half of the design rests on it: which checks may never be disarmed, when the
`# retired:` fence fires, what "unproven" means. It is checkable, but I checked only its
negative half: 67 suite-only edits exist, and `0ebd0ca` is correctly among them. I did not
check its positive half, that the 307 edits which also touched covered code are mostly real
fixes rather than co-evolving feature work. If `# covers:` globs are wide, the predicate will
call feature work a catch. That errs in the safe direction, but it would inflate `catches`
until the number stops meaning anything.

## 8. The falsifier

**The recommendation is wrong if the catch predicate cannot tell a fix from feature work.**
Checkably:

> Take the first 30 catches B4 records. For each, read the refused tree's failing assertion
> and the diff that turned it green. If **more than a third** are feature work that happened
> to touch a covered file, rather than a correction of behaviour the assertion was right
> about, then `catches` measures churn, not protection. In that case the protective rule should
> key on **explicitly declared** catches (a `# caught:` line written with the fix, the way
> `# scar:` is written today) instead of an inferred predicate.

Four smaller falsifiers, each a number someone can pull:

- **Label churn.** If the likely-rename rate at 30 days, measured by
  `scripts/label-stability.py` against the revision 30 days back, exceeds **10%**, the
  assertion is not a durable identity. Keep the record at file level only. Today: 2.8% at 9
  days.
- **Rungs 0-1 carry the noise.** If, after B2, fewer than half of recorded reds are
  classified by run facts and tree alone, the ladder (C) is needed sooner than §7 says.
  Today: 67% by rung 0 alone, before rung 1 exists.
- **The consumer is read.** If B5's trust-show block is in the bead body and red-bead close
  reasons still say "flaky" without naming a cause, the record is decorative. Count close
  reasons containing "flaky" per month; the first month is the baseline.
- **Nothing disarms a catching check.** If any path ever quarantines, disables or rewrites an
  assertion with `catches > 0` without a `# retired:` line, the design has failed at the only
  thing it was built to prevent. This is checked by B6's suite, with a planted offender.

### What changed from the first writing

- **Automatic quarantine goes from "keep, but fix the classification" to "delete".** §4.3's
  argument and §4.2's census were not in the first writing.
- **The ladder moves from "next" to "only when the data asks".** 67% of reds are the runner,
  and the harvested corpus (§4.5) shows rung 0 costs nothing.
- **Catches come from a predicate over the yield ledger, not from `# scar:` lines.** §4.9 found
  a scar line describing a guard that had been deleted.
- **The first writing's falsifier (label churn across commits) was run.** It holds: under 3%
  renames at 9 days.

---

## 9. Proof of concept

`sources/sp-pmv67/scripts/` (also on `spike/sp-pmv67-poc`, `0c1c05b`, local only). Nothing
calls any of them.

- `trust-harvest.sh` (40 lines): one JSONL record per named assertion from a suite's output,
  with labels through the existing normaliser. It records `no-assertions` explicitly, because an
  empty harvest and a suite that ran nothing must not look alike.
- `trust-show.sh` (80 lines): catches first, then verdicts split by condition.
- `harvest-corpus.py` (48 lines): the §4.5 backfill.
- `label-stability.py`, `assertion-edits.py`: the §4.6 and §4.9 measurements.

Fed today's §4.7 runs (162 records), asked about the suite:

```
test-cockpit-landed.sh

  catches  1   -- NOT a candidate for automatic disarming
           the worked/landed row counted all-time under a 24h header, ...

  verdict by condition -- the discriminating question is whether ANY of these disagree:
    conc-private             n=30   ok=30
    conc-shared              n=14   no-assertions=4 ok=10
    conc12-shared            n=68   fail=23 no-assertions=28 ok=17
    serial                   n=50   ok=50

  VERDICT DEPENDS ON THE CONDITION, NOT ONLY ON THE CONTENT.
    red under:   conc-shared conc12-shared
    green under: conc-private serial
```

and about a suite with no record:

```
test-conf.sh
  catches  0   -- no recorded catch; that is an ABSENCE OF DATA, not a low score
  record   none. This assertion has never been observed by the harvester.
           Absence of failure is not evidence of reliability: no verdict here.
```

The `catches 1` is read from the `# scar:` line, and §4.9 shows that line now describes a
deleted guard. This is why the design gets catches from a recorded predicate (§5.4) and treats
scars as `scar-unverified`. The proof of concept shows the reading is cheap. It does not show
that the scar was a trustworthy input.

---

## 10. Filed and noted while researching

| | |
|---|---|
| **sp-2wx6v** (filed, P2) | `suite_state_lint` is called by nothing, and `origin/main` ships a quarantine of `test-dummy.sh`, a suite created to satisfy a phantom flake bead |
| **sp-skk5k** (filed, P2) | `test-chamber-repo-labels.sh` reads the production beads store, names assertions after live beads, and is red because a test bead leaked into production |
| **sp-p1huc** (decision, to the operator) | adopt §7: delete the automatic quarantine path, then file B2-B6. Default: file B1 as P1 now and the rest behind it |
| note on **sp-5cnem** | the duplicate and "(none filed)" beads have one cause: the id is taken from the last line of `bd create` output (§4.4) |
| note on **sp-t600k** | answered by this document |
| already open, not re-filed | sp-2f51e (wrong tree), sp-iaxck (per-annotation counting), sp-y8bxp / sp-9wdji (the concurrent suite's own defect; sp-4f0oe's fix is not on main) |

## 11. Sources

All under `docs/spikes/sources/sp-pmv67/`. The beads store is not public, so beads are kept
verbatim, with home-directory paths rewritten to `$HOME` for `spira/inventory.sh`. Runtime
files come from `$SPIRA_RUN` on the harness box, read 2026-09-18.

| path | what |
|---|---|
| `beads/*.txt` | sp-pmv67, sp-t600k, sp-2f51e, sp-zmd3u, sp-6zw2p, sp-4f0oe, sp-5cnem, sp-iaxck, sp-y8bxp, sp-9wdji, sp-1vn7j, sp-40a5p, sp-d0j1g, sp-iavci, as `bd show` printed them |
| `beads/flaky-bead-census.txt` | every bead with "flaky" in its title |
| `logs/landing-flake-excerpt.txt` | every `observe-flake`, `auto-quarantine`, attribution and local-gate-red line, 2026-09-17..18 |
| `logs/flakeobs-ledgers.txt` | the 16 observation ledgers, decoded |
| `logs/gate-yield-summary.txt` | the yield ledger tabulated, plus one record verbatim |
| `logs/suite-state-snapshots.txt` | `spira/suite-state` at `c26bd5b`, and the diff salvaged after the 18:00Z quarantines |
| `logs/bd-create-last-line.txt` | what `bd create` prints, and the lines that parse it |
| `logs/chamber-repo-labels-reads-production.txt` | §4.10 |
| `logs/cockpit-landed-resolution.txt` | §4.9: `0ebd0ca`'s assertion changes and the surviving scar line |
| `data/corpus-*.tsv` | §4.5, three derived tables that reproduce every number there |
| `data/same-tree-*.tsv`, `data/label-stability.txt` | §4.6 |
| `data/experiment-concurrency.txt`, `data/poc-*.txt` | §4.7 and §9 |
| `data/assertion-edits.txt` | §4.9 |
| `logs/sp-zmd3u-observe-flake-test-excerpt.txt` | §4.4: the test that filed the phantom `test-dummy.sh` beads |
| `scripts/` | every script that produced the above |

In-tree code cited by path and line: `spira/suites.sh`, `spira/suite-state.sh`,
`spira/suite-assert.sh`, `spira/gate-retry.sh`, `spira/forge.sh`, `spira/testenv-batch.sh`,
`spira/gate.sh`, `spira/yield.sh`, `spira/gate-check.sh`, `spira/testdb.sh`.
