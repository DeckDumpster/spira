# spira-claim — bead attempt accounting, poison decision, claim selection

Beads: **sp-j1q6o** (attempts/requeues must not count rebase returns), **sp-ytbma**
(test-poison.sh takes 6-8 minutes; its arithmetic belongs in unit tests), **sp-f0qhr**
(claim selection on the machine, stacked-dependents semantics).

## 1. Intent

One program answers three questions the harness asks about a bead, from the same records,
the same way every time:

1. **How many times has this bead failed?** (attempts, for poison) and **how many times has
   it been sent back for something it did wrong?** (requeues, for the "requeued N times,
   never landed" ask). The answer counts the *work*, never the harness
   (law-attempts-count-the-harness): a return because the base moved, a batch eject that
   did not judge the work, a worker killed before judging, are not failures.
2. **What should CHECK 4 do about it?** poison / clear / ask / requeue-mail / reclaim-mail /
   none — a pure function of the counts, thresholds and dedup state. Deterministic and
   instant, so its tests are unit tests and not six-minute sentinel passes (sp-ytbma).
3. **Which bead should this persona claim next?** The ranked claimable candidates for a
   fayth (epic-first rank, sp-ns46j), with the stacked-dependents claim rule (a `blocks`
   edge onto a same-repository work bead is satisfied at CERTIFIED) when the lifecycle
   machine is the source (sp-f0qhr).

It replaces the scattered bash: `_attempts_sql_query`, `attempts_of`, `reopens_of`,
`_requeue_return_causes_sql`, `_check4_bulk_sql`, `check4_bulk_data`, `check4_decide`,
`epic_parent_lookup`, `epic_rank_rows` (lib.sh) and aeon.sh's band/rank python.

### What it is deliberately NOT

- **Not the brief.** sp-f0qhr was ejected twice because its aeon.sh change bypassed
  sp-wmcvb's lifecycle-aware brief rendering (test-aeon-chamber-overlay "lifecycle_enforce=1
  tells the model it has no bd") and the `lifecycle_enforce` misconfiguration refusal
  (test-lifecycle-enforce-gate case C). Brief rendering, `{{FINISH}}` selection and the
  enforce gate are about *how the claimed bead is worked*; this program only decides
  *which bead*. It has no knowledge of `lifecycle_enforce`, briefs, chambers or `bd` in the
  aeon environment, and its cutover touches none of those lines.
- **Not the claim itself.** The atomic `bd update --claim` and the lifecycle CAS
  (`lc_claim_bead`) stay where they are; `select` only orders candidates.
- **Not a writer — with one named exception.** Every read verb (`attempts`, `requeues`,
  `counts`, `decide`, `poison-decide`, `epics`, `select`) never writes the bead store, the
  lifecycle store or any file; every event they count is written by someone else
  (`bead_reopen`, `bump_*`, bd itself). The one writer is `unpoison` (§8), the operator's
  and groomer's clear of a poison. It lives here because it writes exactly the records
  `attempts` floors on and must be judged by `decide`, and a clear verified by a different
  count from the one CHECK 4 uses is what broke unpoison.sh (sp-r66qd).
- **Not resumability.** Whether a top-tier candidate already has commits ahead of its base
  needs `repo_root` + `spira_landref` + git; that stays in aeon.sh. `select --top-tier`
  hands aeon.sh the tier to check and `select --resumable FILE` takes the answer back.

## 2. Contract

Binary `spira-claim`. Exit codes, shared by every verb:

| exit | meaning |
|---|---|
| 0 | answered; stdout is the answer |
| 1 | usage error (unknown verb, missing/invalid argument) |
| 2 | **cannot tell** — a store read failed, returned non-JSON, or input was malformed. stdout is EMPTY. Callers must treat this as "no decision this pass", never as zero (law-a-control-that-cannot-check-must-refuse; sp-rp4g4, sp-418h5) |

Diagnostics go to stderr, one line, prefixed `spira-claim:`.

### Data in: where it comes from

- **Events** — bd's `events` table, read with ONE `bd -C <db> sql --json` per ≤200 ids
  (chunked so the query argv is bounded no matter how many beads are asked about; bd sql
  takes its query only as argv). Only the columns and event types this program folds are
  selected, and `new_value` is truncated to 120 chars (a close reason can be kilobytes).
  `--events FILE|-` replaces the fetch with rows the caller already has (tests, replay).
- **Ready set** — bd `ready`/`list --json` rows, from `--ready FILE` or stdin. Never argv
  (law-payloads-go-on-stdin; sp-o4trx's E2BIG outage at 142 beads).
- **Epic lookup** — `{"prio":{epic:prio}, "started":[epic,...]}`, from `--epics FILE` or
  fetched (one `bd list --id` per ≤100 distinct epics, one `bd children` per distinct epic).
- **Lifecycle snapshot** (machine mode only) — `spira-lc list` rows, from `--lifecycle FILE`
  or fetched by running `$SPIRA_LC_BIN list`.
- **Blocker records** (machine mode only) — bd rows for every `blocks` target, from
  `--blocker-records FILE` or fetched with chunked `bd list --id ... --status all`.

bd binary: `$SPIRA_BD` else `bd` (the same seam bash uses). Database: `--db` else
`$SPIRA_DB` (exported by conf.sh from the resolved config) else spira-config `spira.db`.
`stack_max_depth` and `submitted_label` come from spira-config (`spira.stack_max_depth`,
default 4, clamped to the schema's ceiling of 4; `spira.submitted_label`, default
`spira-submitted`); flags override for tests. Thresholds are flags (`--poison-at`,
`--requeue-at`, `--reclaim-at`, defaults 3/5/5) because they are sentinel.sh variables
(`SPIRA_POISON_AT` …), not spira-config keys. Every bd call has a timeout
(`--timeout-s`, default 60; a timed-out call is exit 2).

### Verbs

```
spira-claim attempts <bead> [--events F] [--json]
    -> "<n>\n". --json: the full ledger (every attempt and every return, classified).
spira-claim requeues <bead> [--events F] [--json]
    -> "<n>\n": judged returns (the count behind "requeued N times, never landed").
spira-claim counts [--events F]                 (ids on stdin, "<id>[\t...]" per line)
    -> "<id>\t<attempts>\t<requeues>\t<reclaims>\n" for EVERY input id (zeros included),
       input order, deduplicated. Replaces check4_bulk_data.
spira-claim decide <n> <requeues> <reclaims> <labels-csv> <stamp> [poisoned]
                   [--poison-at N] [--requeue-at N] [--reclaim-at N]
    -> check4_decide, exactly: space-separated subset of
       "requeue-mail reclaim-mail poison clear ask", or "none".
spira-claim poison-decide <bead> [--labels CSV] [--asked RQ:RC:PO[:PL]] [--poisoned 0|1]
                   [thresholds] [--events F] [--json]
    -> counts from the events, then `decide`. Same output as decide; --json adds counts.
spira-claim epics [--ready F]                   (ready JSON on stdin by default)
    -> the epic lookup JSON (epic_parent_lookup's output shape).
spira-claim select --fayth NAME [--ready F] [--epics F] [--resumable F]
                   [--top-tier | --count | --json]
                   [--blockers bd|machine] [--lifecycle F] [--blocker-records F]
                   [--stack-max-depth N]
    -> default: TSV, best first, epic_rank_rows's 7 columns:
       epic_priority  epic_started(0|1)  bead_priority  resumable(0|1)  created_at  id  epic_id
       --top-tier: aeon.sh's band lines "id|branch|repo|eprio|estarted|bprio" for the
                   candidates in the best (eprio, estarted, bprio) tier only.
       --count:    the number of claimable candidates (ready_count parity: counts and
                   claims come from the same function over the same rows).
       --json:     [{"id","epic_id","epic_priority","epic_started","bead_priority",
                     "resumable","age"}...] in rank order.
```

`--fayth` is required on `select` (attribution in diagnostics, and so a caller cannot
forget which predicate built the ready set); the predicate itself (labels, exclusions,
`-u`, `--exclude-type epic,event`, scope, no-loop) is applied by the caller's bd query,
exactly as `READY_ARGS` does today. Rows of type epic/event are dropped defensively anyway.

**`--blockers bd`** (default): the input is `bd ready` output — bd already applied its
blocker rule; nothing is re-filtered. This is today's behaviour, bit for bit.

**`--blockers machine`**: the input is `bd list` output (open, unassigned, predicate
filtered, blockers NOT filtered), and claimability is decided here from the lifecycle
machine (stacked-dependents-2026-09-28 §1-2):

1. The candidate's own lifecycle row must exist (fail closed, as `lc_bead_row`), be READY
   or REWORK, and carry no hold other than `wait` (the `wait` hold is CHECK 3b's legacy
   dual-write computed against LANDED for every blocker; this rule supersedes it — the
   reasoning sp-f0qhr's ready.rs recorded, kept).
2. Every `blocks` dependency of the candidate must be satisfied:

   | blocker | satisfied when |
   |---|---|
   | work bead (issue_type not epic/decision/event/molecule/gate), **same `repo:`**, has a lifecycle row, `stack_max_depth > 0` | CERTIFIED or IN_DELIVERY (stacked), or LANDED/DONE |
   | anything else with a lifecycle row (epic, decision, other repo, or stacking off) | LANDED/DONE, or terminal and bd-closed |
   | no lifecycle row | bd status `closed` |

3. Stack depth = 1 + max(`stack_depth` of the stacked, not-yet-landed blockers), 0 when
   none are stacked. A candidate whose depth exceeds `stack_max_depth` is not claimable
   (the hold stays). `stack_max_depth = 0` reproduces today: dependents wait for LANDED.

### Guarantees

- **NULL-safe.** A bead with no events, or none after its last `poison.cleared`, is 0 —
  not `<nil>`, not a failure (sp-r66qd: attempts_of printed `<nil>` right after a clear).
- **Fail closed.** A failed or non-JSON store read is exit 2 with empty stdout, never 0.
- **Deterministic.** Same rows in, same bytes out. Events are ordered by `created_at` then
  a fixed type precedence (claim < close < requeue < reopened < reopen), then input order.
- **No argv payloads.** Ready sets, lookups and snapshots come on stdin or in files; the
  only variable-length argv this program produces is a bd query over ≤200 (events) /
  ≤100 (list --id) ids.
- **Same function for counting and claiming** (`select --count` vs `select`).

## 3. Schema (Rust types, serde)

```rust
/// One bd `events` row as `bd sql --json` returns it (the only columns selected).
struct EventRow { issue_id: String, event_type: String, new_value: Option<String>,
                  created_at: String /* "YYYY-MM-DDTHH:MM:SSZ" or "YYYY-MM-DD HH:MM:SS…" */ }

/// The event kinds the fold reads; everything else is ignored.
enum EventKind { Claim /* claimed | status_changed ~ in_progress */, Close /* closed */,
                 Requeued(String) /* requeued <cause> */, Reopened /* bd's own, no cause */,
                 Reopen(String) /* bead_reopen's cause row */, Reclaimed,
                 PoisonCleared }

/// What a return (a requeued/reopen cause) says about the work.
enum ReturnClass {
    NotAReturn,     // work-close-converted, recurrence, closed-while-live, alert-recur
    RebaseReturn,   // base moved: *conflict*, rebase-*, *stale* (not *-red), base_withdrawn*
    HarnessReturn,  // did not judge the work: thrash, unjudged-*, eject, queue-eject, ejected,
                    // eviction-race, slain, closed-never-landed-batch-ready
    Judged,         // everything else, including every *-red / *fail* cause, batch-eject,
                    // and any cause this table does not know (fail toward visibility)
}

struct Attempt { claimed_at: String, outcome: AttemptOutcome }
enum AttemptOutcome { Succeeded /* closed */, Exempt(String) /* harness or rebase cause */,
                      Charged(String) /* re-claimed, or still open */ }
struct Return { at: String, cause: String, class: ReturnClass }

/// The per-bead answer (`attempts --json`, `requeues --json`).
struct Ledger { bead: String, floor: Option<String>, attempts: u32, requeues: u32,
                reclaims: u32, attempt_log: Vec<Attempt>, returns: Vec<Return>,
                legacy_credits: u32 }

/// check4_decide's inputs and output.
struct Thresholds { poison_at: u32, requeue_at: u32, reclaim_at: u32 }
struct AskedStamp { requeue: bool, reclaim: bool, poison_at_n: bool, lifted_at_or_above_n: bool }
enum Token { RequeueMail, ReclaimMail, Poison, Clear, Ask }

/// A ready/list row (only the fields read; unknown fields ignored).
struct ReadyRow { id: String, priority: Option<i64>, parent: Option<String>,
                  created_at: Option<String>, updated_at: Option<String>,
                  labels: Vec<String>, issue_type: Option<String>, status: Option<String>,
                  dependencies: Vec<Dependency> }
struct Dependency { issue_id: Option<String>, depends_on_id: Option<String>,
                    #[serde(alias = "dependency_type")] r#type: Option<String> }

struct EpicLookup { prio: BTreeMap<String, i64>, started: Vec<String> }

/// `spira-lc list` row. Columns come back string-valued from dolt, so every field
/// accepts either a string or its native JSON type.
struct LifecycleRow { bead_id: String, state: String /* lifecycle::bead::BeadState */,
                      holds: Vec<String> /* JSON array, or a JSON-encoded string of one */,
                      stack_depth: u32 /* absent → 0 */ }

struct Ranked { epic_priority: i64, epic_started: u8 /* 0 = started */, bead_priority: i64,
                resumable: u8 /* 0 = resumable */, age: String, id: String, epic_id: String }
```

### The attempt fold (sp-j1q6o's rule, exact)

Events after the bead's last `poison.cleared` (strictly later, as today) are walked in order:

- **Claim**: if an attempt is already open it becomes *Charged* ("re-claimed without a
  close"); a new attempt opens.
- **Close**: closes the open attempt as *Succeeded*; with none open, one legacy credit.
- **Requeued `thrash` / `unjudged-*`**: closes the open attempt as *Exempt*; with none open,
  one legacy credit (an operator's hand-written offset row, law-attempts-count-the-harness).
- **Requeued / Reopen with a RebaseReturn or HarnessReturn cause** (NEW): closes the open
  attempt as *Exempt*; with none open, nothing (a return of already-closed work is not an
  attempt and must not forgive an earlier one).
- Everything else (a Judged return, a release to open) leaves the open attempt open.
- At the end an open attempt is *Charged* (a live claim counts, as today).

`attempts = max(charged - legacy_credits, 0)`. For any stream containing none of the new
rebase/harness causes this is algebraically `max(claims - closes - thrash/unjudged, 0)`,
i.e. exactly `_attempts_sql_query` — pinned by a unit test.

### The requeue count

`requeues` = the number of `reopen` cause rows classed **Judged**, plus every bd `reopened`
row that has no `reopen` cause row within 120 s after it (a bare `bd reopen` by hand, which
the old count always charged). Not floored by `poison.cleared` (as today: REQUEUE_AT is
independent of poison). Why not "reopened minus excluded causes" (the first sp-j1q6o
commit): a return of a *submitted* bead reopens an open bead, bd refuses, and only the
cause row is written — so subtraction removes returns that were never added and can hide a
real red (measured on sp-vd9dn: 5 `reopened`, 7 `reopen` rows).

`reclaims` = count of `reclaimed` rows (unchanged).

## 4. test-poison.sh → unit tests (sp-ytbma)

| test-poison.sh case | becomes |
|---|---|
| valve covers the dispatchable set (goal_open_children vs dispatchable_open) | **stays end-to-end** — it is a property of `dispatchable_open`'s bd predicate, not arithmetic |
| bead at threshold is poisoned; below is left alone | unit: `decide_poison_at_threshold`, `decide_below_threshold_none` |
| ask mail title/BRANCH/Default/log excerpt, events.log record | **stays end-to-end** (sentinel.sh's mail rendering, not this program) |
| already-poisoned emits nothing new | unit: `decide_already_poisoned_no_poison_token` |
| excluded label / epic never poisoned | **stays end-to-end** (predicate); `select` drops epic rows defensively (unit) |
| held bead keeps its claim; CHECK 7 declines to summon | **stays end-to-end** (sentinel side effects) |
| stale poison clear | unit: `decide_clear_below_threshold_when_poisoned` |
| thrash requeues do not poison | unit: `thrash_is_net_zero`, `unjudged_is_net_zero` |
| POISON_AT=0 edge: zero attempts never poisons, one does | unit: `poison_at_zero_needs_a_charged_attempt` |
| ask once per (bead, count); a new count asks again | unit: `ask_suppressed_by_stamp`, `ask_again_at_new_count` (dedup state is the stamp) |
| closed mid-pass never poisons | **stays end-to-end** (a race in sentinel's loop) |
| empty chamber | **stays end-to-end** |
| G11 reclaim cap | unit: `reclaim_mail_at_cap`, `counts_reclaims` |
| G13 requeue cap for another partition | unit: `unpaired_reopened_counts`, `decide_requeue_mail_at_cap`; partition half stays e2e |
| sp-wiyr2 lift survives next pass | unit: `lift_at_or_above_n_suppresses_poison`, `new_failure_after_lift_poisons` |
| sp-qd2ul clear sticks; fresh failures poison again; ask cites count since clear | unit: `poison_cleared_floor`, `fresh_run_after_clear_counts_from_zero`, `same_second_as_floor_is_excluded` |
| sp-rp4g4 bulk query fails → no decision | unit: `events_read_failure_is_cannot_tell_with_empty_stdout`, `store_events_parses_and_fails_closed`, `poison_decide_fetch_failure_decides_nothing` (exit 2, empty stdout) |
| sp-418h5 stale-clear scan fails closed | unit: same, on `attempts` |
| (new) sp-r66qd NULL after clear | unit: `null_safe_no_events`, `null_safe_nothing_after_floor`, `attempts_null_safe_right_after_clear` |
| (new) sp-j1q6o rebase returns | unit: `rebase_returns_do_not_count_as_requeues`, `rebase_returns_do_not_count_as_attempts`, `three_red_returns_count`, `rebase_returns_raise_no_ask_red_returns_do`, `sp_vd9dn_real_stream_is_zero` |

What still needs one end-to-end sentinel pass: that CHECK 4 wires `counts`/`decide` output
to a real hold, mail and event; the dispatchable-set predicate; the closed-mid-pass race.
That is one seeded store and **one** audit pass with a handful of beads — the per-decision
cases above no longer need their own seed + pass each, which is where the 6-8 minutes went.

## 5. Cutover

**Not performed** (operator's directive: no bash edits). Each line below is the exact
replacement; `$SPIRA_CLAIM_BIN` is resolved in conf.sh beside `SPIRA_LC_BIN`.

Line numbers are against local/main at 99f41abed (this branch's base plus the first
sp-j1q6o commit).

1. **spira/conf.sh** after line 2310 (`export SPIRA_LC_BIN`), add:
   ```bash
   if [ -z "${SPIRA_CLAIM_BIN:-}" ]; then
       SPIRA_CLAIM_BIN="$(command -v spira-claim 2>/dev/null)" || SPIRA_CLAIM_BIN="$(spira_bin spira-claim 2>/dev/null)"
   fi
   export SPIRA_CLAIM_BIN
   ```
2. **spira/lib.sh:3225-3266 `_attempts_sql_query`** — delete (only attempts_of used it;
   test-attempts-sql.sh's SQL-text assertions retire with it).
3. **spira/lib.sh:3275-3284 `attempts_of`** →
   `attempts_of() { "$SPIRA_CLAIM_BIN" attempts "$1"; }`
4. **spira/lib.sh:3286-3308 `_requeue_return_causes_sql` + `reopens_of` (and their comment block from 3286)** →
   `reopens_of() { "$SPIRA_CLAIM_BIN" requeues "$1" || printf '0'; }`
   (keeps its old fail-open interface; no caller outside lib.sh uses it today.)
5. **spira/lib.sh:5664-5667 `_check4_bulk_sql`** — delete.
6. **spira/lib.sh:5680-5719 `check4_bulk_data`** →
   ```bash
   check4_bulk_data() {
       local input="${1:-}"; [ -n "$input" ] || return 0
       printf '%s\n' "$input" | "$SPIRA_CLAIM_BIN" counts
   }
   ```
   (sentinel.sh:433 and :709's `_rc` check is unchanged: nonzero still means "decide
   nothing this pass". test-poison.sh's `*"as rcl"*` failure seam becomes
   `*"sql --json"*` or `SPIRA_BD` pointing at a failing stub.)
7. **spira/lib.sh:5751-5793 `check4_decide`** →
   ```bash
   check4_decide() {
       "$SPIRA_CLAIM_BIN" decide --poison-at "${POISON_AT:-3}" --requeue-at "${REQUEUE_AT:-5}" \
           --reclaim-at "${RECLAIM_AT:-5}" -- "${1:-0}" "${2:-0}" "${3:-0}" "${4:-}" "${5:-0:0:0:0}" "${6:-0}"
   }
   ```
8. **spira/lib.sh:1198-1246 `epic_parent_lookup`** →
   `epic_parent_lookup() { printf '%s' "$1" | "$SPIRA_CLAIM_BIN" epics; }`
9. **spira/lib.sh:1264-1299 `epic_rank_rows`** →
   ```bash
   epic_rank_rows() {
       local _lkf _rsf _rc; _lkf="$(mktemp)" || return 1; _rsf="$(mktemp)" || { rm -f "$_lkf"; return 1; }
       printf '%s' "$2" > "$_lkf"; printf '%s' "${3:-}" > "$_rsf"
       printf '%s' "$1" | "$SPIRA_CLAIM_BIN" select --fayth "${FAYTH:-any}" --epics "$_lkf" --resumable "$_rsf"
       _rc=$?; rm -f "$_lkf" "$_rsf"; return $_rc
   }
   ```
10. **spira/aeon.sh:371-395 (the `band_lines=… python3` block)** →
    ```bash
    _lkf="$SPIRA_RUN/.epic-lookup.$$"; printf '%s' "$epic_lookup" > "$_lkf"
    band_lines="$(printf '%s' "$ready_json" | "$SPIRA_CLAIM_BIN" select --fayth "$FAYTH" --epics "$_lkf" --top-tier 2>/dev/null)"
    rm -f "$_lkf"
    ```
    (Lines 336-358 — the ready query and `epic_parent_lookup` call — and 397-460 — the
    resumability loop, `epic_rank_rows`, the claim loop — stay as they are and pick up
    items 8-9. Nothing from aeon.sh:461 on is touched: the poison race
    check, `lc_claim_bead`, `lifecycle_enforce` and brief rendering are outside this
    component.)
11. **Machine selection (sp-f0qhr), when the operator enables it**: aeon.sh:337's
    `claim_retry "${READY_ARGS[@]}" …` becomes, behind the existing
    `lifecycle_selection_available` gate from sp-f0qhr's branch (not added here),
    `claim_retry list --status open --no-assignee --exclude-type epic,event --limit 0 --label "$FAYTH_LABELS" --exclude-label "$CLAIM_EXCLUDE"`
    and the rank/tier calls gain `--blockers machine`. `ready_count` (lib.sh:1167)
    becomes the same list query piped to `select --fayth "$f" --blockers machine --count`.
    `fayth_ready`/`bulk_ready_by_fayth` (lib.sh:1513/1575) and ready-bucket.py count through
    that `ready_count`.
12. ~~spira/unpoison.sh:123~~ — **superseded by §8.6**: unpoison.sh is deleted outright and
    `spira-claim unpoison` takes the requeue count from the same fold CHECK 4 uses.
13. **spira/test-poison.sh** — reduce to the "stays end-to-end" rows of §4; the unit cases
    are `cargo test -p spira-claim`. Lift its quarantine (`suites.sh activate
    test-poison.sh`) in the same landing (sp-ytbma's obligation 1).
14. **spira-lc/src/main.rs `cmd_list`**: add `stack, stack_depth` to its SELECT (as `cmd_show`
    has). Until then `spira-lc list` rows carry no `stack_depth` and machine mode reads every
    stacked blocker as depth 0, so the depth cap cannot bite past depth 1.
15. Build: add `spira-claim` to the workspace (done here) and to whatever copies built
    binaries into `$SPIRA_ARTIFACTS` (same list `spira-lc` is on).

## 6. Verification against the live store (read-only, 2026-09-28)

- **Rank parity.** The live `bd ready` set for `spira` (163 beads, 1.87 MB — 14x
  MAX_ARG_STRLEN) ranked by lib.sh's own `epic_rank_rows` python and by `select`: byte-identical
  TSV; same for `--resumable`; aeon.sh's band python vs `--top-tier`: identical.
- **Count parity.** 400 beads with claims/reopens since 2026-09-20, legacy `_check4_bulk_sql`
  (pre-sp-j1q6o) vs `counts` (0.6 s for all 400): reclaims identical; attempts identical but
  one (sp-bf31a 1 → 0: a `queue-eject` ended an open claim); requeues differ on 116 — beads
  at or over REQUEUE_AT=5 go from 45 to 20. A few go *up* (sp-uzwul 4 → 5): red returns of a
  submitted, open bead (`cert-gate-red`, `closed-without-commit`) write no bd `reopened`
  row, so the old count never saw them.

## 7. Decisions

- **One fold, not SQL arithmetic.** The bash counts are three `sum(case …)` expressions; a
  rule like "a rebase return forgives the attempt it ended, but not an earlier one" cannot
  be written as a sum. The fold keeps the legacy algebra for legacy causes (tested) and adds
  the rule only where it applies. (Rejected: another SQL subtraction — the first sp-j1q6o
  commit — which over-subtracts on submitted beads.)
- **Unknown causes count as judged.** A cause this table has never seen is more likely a new
  failure mode than a new harness exemption; hiding it would repeat the silent-valve class.
- **`eject`/`queue-eject` are harness returns; `batch-eject` is judged.** `batch-eject`
  is written only when a batch gate failure was attributed to the bead (landing.sh CHECK 6).
  A manual `queue.sh eject` records the cause `eject` whatever its reason, and on the
  2026-09-28 evidence (sp-vd9dn) it was used to return for rebase; if Ryan wants a red
  manual eject to count, queue.sh should record `eject-red` (contains "red" → Judged),
  which this classifier already handles.
- **Recurrence reopens are not returns** (`recurrence`, `closed-while-live`, `alert-recur`):
  a recurred incident is a new occurrence, not a failure to land; incident.sh counts it
  under `recurred` already.
- **`select` does no git.** Resumability needs the repo map and landref resolution, which
  belong to lib.sh; the tier goes out, the answer comes back in a file.
- **Machine mode fails closed on a missing own row**, as sp-f0qhr's ready.rs and
  `lc_bead_row` do; a bead filed but never registered with the machine is not claimable
  in machine mode (it is in bd mode, the default until cutover).

## 8. `unpoison` — clear a poison so it stays cleared, and prove it

Replaces **spira/unpoison.sh** (157 lines). Beads: sp-qd2ul (the poison.cleared floor),
sp-i2m7y (the hold, not the label), sp-r66qd (verify read `<nil>` and failed every genuine
clear).

### 8.1 Intent

A poison is CHECK 4's lifecycle `poison` hold, put on a bead whose charged attempts reached
`POISON_AT`. Clearing one by hand failed a different way every time: removing the label
left the count at the threshold and the next pass re-poisoned (2026-09-26, six beads);
crediting attempts worked only when every step was remembered. `unpoison` is the one path.
For each bead it:

1. writes the `poison.cleared` event the attempt count is floored on (§3 "The attempt
   fold") — **first**, so a CHECK 4 pass racing it sees the reset count before the released
   hold;
2. drops the bead's poison-ask history (`$SPIRA_POISON_ASKED/<id>`), so a genuinely new
   poisoning at the same count is asked about again;
3. releases the lifecycle `poison` hold through spira-lc (and best-effort removes the
   vestigial `spira-poison` label, which nothing reads any more);
4. notes the bead with the cause (the next aeon reads it);
5. closes the operator ask the poisoning raised
   (`Spira bead <id> — … — change the approach or drop it?`);
6. **verifies** — re-reads the events, the hold and the labels and asks `decide` (CHECK 4's
   own function, same counts) what the next pass will do. Anything but "not poison, hold
   gone, attempts below threshold" is a FAIL.

`--watch` then waits for one complete **audit** pass (CHECK 4 runs in the audit worker, which
logs to `$SPIRA_RUN/audit.log` — sentinel/DESIGN.md §2.1/§4 on concierge/rw-sentinel) that
started after the clear, and confirms the hold did not come back.

It never touches live work: a bead an aeon holds is refused, and the refusal names the exit.

### 8.2 Contract

```
spira-claim unpoison --bead <id> [--bead <id>...] --cause "<evidence>"
                     [--watch] [--watch-timeout-s N] [--dry-run]
                     [--credit <cause-slug>] [--actor NAME]
                     [--poison-at N] [--db PATH] [--timeout-s N]
```

| flag | meaning |
|---|---|
| `--bead` | required, repeatable. Named only: a positional is a usage error (slay.sh's 2026-09-13 lesson). Ids must match `[A-Za-z0-9._-]+`. |
| `--cause` | required, non-empty. The evidence that the poison was wrong. The full text goes to the note and the ask's close reason (on stdin); the event carries a bounded form (§8.3). |
| `--watch` | after clearing, wait for one complete audit pass that started after the clear; fail if none completes in time or the hold came back. Only when every bead cleared and verified, and not with `--dry-run`. |
| `--watch-timeout-s` | default 2400 (the audit unit's own `RuntimeMaxSec=1800` plus a dispatch interval; the bash script's 25 min was shorter than one audit pass can legally take). |
| `--dry-run` | print `WOULD` and change nothing. Preconditions (and refusals) are still evaluated. |
| `--credit` | groomer's harness credit: also write a `requeued` / `unjudged-<slug>` event immediately before the floor (same second, so the fold floors it away; it exists for the census vocabulary). `<slug>` matches `[a-z0-9-]{1,64}`. Written only once every precondition passed. |
| `--actor` | who is clearing: the lifecycle event's actor and the note's attribution. Default `unpoison`. The bd event's `actor` column is `$BEADS_ACTOR` else `harness` (as `_bump_write_event`). |
| `--poison-at` | the threshold the precondition and verify use. Default `$SPIRA_POISON_AT` else 3 — sentinel.sh:41's own resolution, and the audit worker gets it via `--setenv=SPIRA_POISON_AT` (sentinel.sh:1144). |

**Stdout, one line per outcome** (humans and groomer.sh read these; the prefix is fixed
width, as unpoison.sh's):

```
OK   <id>: cleared — attempts <n> -> <m>, check4 decides "<tokens>"
SKIP <id>: not poisoned and attempts <n> < <P> — nothing to clear
FAIL <id>: held by <holder> (<where>) — live work is never touched: let that aeon finish (or stop it: spira/slay.sh --bead <id>), then re-run
FAIL <id>: no such bead
FAIL <id>: cannot tell (<what>) — nothing was written
FAIL <id>: could not write the poison.cleared floor (<err>) — nothing else was changed
FAIL <id>: did not verify:<reason>... (attempts <m>, check4=<tokens>)
WOULD <id>: attempts <n>, poisoned=<0|1> — write poison.cleared, reset ask history, release the lifecycle poison hold, note, resolve ask
     resolved ask <ask-id>
     warn <id>: <step> failed: <err>          (note / label / ask close: best effort, reported)
watch: waiting for an audit pass that starts after <ts> (up to <N>s)...
OK   watch <id>: still clear after a full audit pass
FAIL watch <id>: re-poisoned by the pass
FAIL watch: no audit pass completed in <N>s
```

Verify reasons: `lifecycle-hold-still-present`, `lifecycle-unreadable`, `attempts-still-<m>`,
`events-unreadable`, `check4-would-repoison`, `ask-history-still-present`.

**Exit codes** — this binary's table (§2) plus one:

| exit | meaning |
|---|---|
| 0 | every bead is `OK` or `SKIP` (and, with `--watch`, stayed clear) |
| 1 | usage (missing `--bead`/`--cause`, a positional, bad id, unknown flag) |
| 2 | cannot tell — `SPIRA_RUN` unresolvable, so neither the ask history nor the audit log can be found; nothing was written |
| 3 | at least one bead `FAIL`ed (refused, not cleared, or did not verify), or the watch failed |

unpoison.sh used 1 for "a bead failed" and 2 for usage; that collides with this binary's
table, where 1 is usage and 2 is cannot-tell, so the failure code moves to 3. The only
callers that read the code are test-unpoison.sh (repointed in §8.6) and groomer.sh (which
reads the `OK` line, §8.6).

**Preconditions, per bead, before any write** (in this order):

1. `bd show <id>`: absent → `FAIL no such bead`; the read failing → `FAIL cannot tell`.
2. The events (`Store::events`, the same chunked `bd sql --json` as `counts`): failing →
   `FAIL cannot tell`.
3. `spira-lc show <id>`: exit 0 → the row; exit 1 with `{}` → no row (not classified: not
   poisoned); anything else → `FAIL cannot tell` (unpoison.sh read a failed lc read as "not
   poisoned" and would SKIP or falsely verify).
4. **Live work.** bd `status == in_progress` with an assignee, **or** a lifecycle row in
   `WORKING` with a holder → `FAIL held by …`. Nothing is written.
5. **Nothing to clear.** No poison hold and `attempts < P` → `SKIP`.

**Writes, per bead, in order** (after the preconditions pass; each is its own store call):

| step | store | how | failure |
|---|---|---|---|
| (credit) | bd events | `bd -C <db> sql "INSERT … 'requeued' … 'unjudged-<slug>' …"` | FAIL, stop |
| 1 floor | bd events | `bd -C <db> sql "INSERT … 'poison.cleared' … '<bounded cause>' …"` | FAIL, stop — without the floor, releasing the hold only invites the next pass to re-poison |
| 2 ask history | file | `rm $SPIRA_POISON_ASKED/<id>` (absent is fine) | reported by verify |
| 3 hold | spira-lc | `spira-lc show` → `spira-lc event bead <id> --expect <state> --version <v> --actor <actor> --kind '{"Unhold":{"kind":"Poison"}}'`; a refusal (exit 3, lost CAS race) retries once from a fresh read | reported by verify |
| 3b label | bd | `bd -C <db> label remove <id> spira-poison` | ignored (vestigial) |
| 4 note | bd | `bd -C <db> note <id> --stdin` ← `Poison cleared by spira-claim unpoison (<actor>; attempts were <n>): <cause>` | `warn` |
| 5 ask | bd | `bd -C <db> list --status open --label <ask-label> --limit 0 --json`; for each title matching §8.3's rule, `bd -C <db> close <ask> --reason-file -` ← `Resolved by spira-claim unpoison: <id>'s poison was cleared — <cause>` | `warn` |

**Verify** re-reads events (fold → attempts, requeues, reclaims — the fold's, never the raw
`requeued` row count unpoison.sh passed), the lifecycle row and the bd labels, then
`decide(attempts, requeues, reclaims, labels, stamp 0:0:0:0, poisoned)`. It fails on: the
hold still present; the lifecycle row unreadable; attempts ≥ P; the events unreadable; a
`poison` token; the ask-history file still present.

**Watch.** Records the audit log's length and the clock after the last bead is verified,
then polls every 10 s up to the timeout. Only bytes appended after the recorded length are
read (the log is never re-read whole; a log shorter than the recorded length was rotated and
is read from 0). Lines are `<YYYY-MM-DDTHH:MM:SSZ> spira: <msg>`; lines without that shape
(bash noise such as `sentinel.sh: line 705: _phase: command not found`) are ignored. A pass:

- **starts** at `CHECK4 examining <n> dispatchable bead(s), poison=<P> …` stamped strictly
  after the recorded clock;
- is **disqualified** by `CHECK4 bulk attempts query failed` (that pass decided nothing, so
  it proves nothing) — the watch keeps waiting for the next one;
- **completes** at `audit pass complete — …`.

Every poll also reads each cleared bead's hold; a poison hold back is `FAIL watch` at once.
After a complete, qualifying pass each bead's hold is read once more for the verdict. If the
pass reported a `poison=<P>` different from `--poison-at`, the watch says so on stdout
(`watch: note — the audit pass ran with poison=<P>, this clear verified against <p>`).

**Where things are.** bd: `$SPIRA_BD` else `bd`; db: `--db`, `$SPIRA_DB`, spira-config
`spira.db` (§2). spira-lc: `$SPIRA_LC_BIN` else `spira-lc`. `SPIRA_RUN`: `$SPIRA_RUN`, else
spira-config `spira.run`; neither → exit 2. Ask history: `$SPIRA_POISON_ASKED` else
`$SPIRA_RUN/poison-asked` (lib.sh's resolution). Ask label: `$SPIRA_ASK_LABEL`, else
spira-config `spira.ask_label`, else `needs-operator` (conf.sh:1027). Audit log:
`$SPIRA_RUN/audit.log`.

**No lib.sh seams.** Every lib.sh function unpoison.sh sourced is reimplemented against the
same data: `attempts_of`/`requeues_of`/`reclaims_of` → the fold; `check4_decide` → `decide`;
`bump_poison_cleared` → the same INSERT; `poison_asked_clear` → the same `rm`;
`lc_held`/`lc_holds`/`lc_unhold` → `spira-lc show`/`event` directly (lc.sh is a thin shell
over exactly these two calls); `bdq`/`bdjson` → `bd -C <db>`. bdq's create-time fences do
not apply (no `create`), and its connection retry is not reproduced (a failed write is
reported, and verify catches what matters).

### 8.3 Schema

```rust
/// `bd show <id> --json` (first element), only what unpoison reads.
struct BeadRecord { id: String, status: String, assignee: Option<String>,
                    labels: Vec<String>, title: Option<String> }

/// `spira-lc show <id>`'s `bead` object. dolt returns every column as a string, so
/// `version` accepts a number or a numeric string and `holds` a JSON array or its
/// JSON-encoded text (the same double parse as `rank::parse_lifecycle`).
struct LcRow { state: BeadState, version: u64, holds: BTreeSet<HoldKind>,
               holder: Option<String> }

enum LcApply { Applied, Refused(String) /* exit 3 */, CannotTell(String) /* anything else */ }

/// An open operator ask: `bd list … --json` rows.
struct AskRow { id: String, title: String }

/// Per-bead outcome, printed as §8.2's line.
enum BeadOutcome { Ok { before: u32, after: u32, decision: String, asks: Vec<String>, warns: Vec<String> },
                   Skip { attempts: u32 }, Would { attempts: u32, poisoned: bool },
                   Fail(String) }

/// One parsed audit.log line.
enum AuditLine { Examining { at: i64, poison_at: Option<u32> }, CountsFailed { at: i64 },
                 Complete { at: i64 }, Other }
```

**Ask match.** An ask is this poisoning's when its title starts with `Spira bead <id> — `
and ends with `— change the approach or drop it?` (sentinel.sh:669's subject). unpoison.sh
matched the substring `bead <id> —`, which also closed the *requeue* and *reclaim* asks
(`Spira bead <id> — completed and requeued …`), a different problem the clear does not
resolve.

**Bounded event value.** The event's `new_value` is the cause with `'`, `"`, `\` and control
characters removed, cut to 200 characters (unpoison.sh's `tr -d | cut -c1-200`, plus control
characters). `bd sql` takes its query only as argv (no `--file`, no stdin — probed), so this
is the one argv payload, and it is bounded. The full cause goes to the note and close
reason on stdin (law-payloads-go-on-stdin).

### 8.4 Tests (`cargo test -p spira-claim`, fakes, no store)

`src/unpoison.rs` runs the whole flow against a `World` trait (bd, events, spira-lc, the
ask-history file, the audit log, clock, sleep); the tests implement it in memory.

| contract | test |
|---|---|
| clear then verify OK with **zero events after the floor** (sp-r66qd) | `clear_then_verify_ok_with_zero_events` |
| floor written before the hold is released | `floor_is_written_before_the_hold_is_released` |
| refuse when an aeon holds it (bd) / (lifecycle WORKING holder), nothing written, exit names slay | `refuse_when_held_by_bd_assignee`, `refuse_when_lifecycle_holder` |
| ask closed by title match; requeue ask and another bead's ask untouched | `ask_closed_by_title_match_only` |
| SKIP a healthy bead | `skip_when_not_poisoned_and_below_threshold` |
| cannot tell on unreadable events / lifecycle → FAIL, no write | `cannot_tell_writes_nothing` |
| floor write failure stops the bead | `floor_write_failure_stops_before_unhold` |
| a lost CAS race retries once | `unhold_refused_retries_once` |
| verify catches a hold that stayed | `verify_fails_when_hold_remains` |
| dry run writes nothing | `dry_run_writes_nothing` |
| `--credit` precedes the floor and is floored away | `credit_written_before_floor_and_not_counted` |
| watch reads the audit log line; noise ignored; a pass that started before the clear or whose counts failed does not count | `watch_reads_audit_log_line`, `watch_ignores_pass_started_before_clear`, `watch_disqualifies_failed_counts_pass` |
| watch fails on re-poison / timeout | `watch_fails_when_repoisoned`, `watch_times_out` |
| usage: positional, missing cause, bad id, bad slug (dispatch level) | `tests::unpoison_usage_errors` |
| event value is bounded and quote-free | `bounded_cause_strips_quotes_and_bounds` |
| no events at all; at threshold with no hold; count that did not drop; one bead of several failing | `clear_a_bead_that_never_had_events`, `at_threshold_without_hold_is_cleared`, `verify_fails_when_count_does_not_drop`, `several_beads_one_failure_fails_the_run` |
| watch survives log rotation; is not started after a failure; line parsing | `watch_survives_rotation`, `watch_not_started_when_a_bead_failed`, `audit_line_parsing` |
| the live seams: note/close text on stdin and never in argv; the INSERT's bounded argv; bd "no such bead"; spira-lc show exit 0/1/2 and `event` 0/3/other with the exact Unhold argv; ask-history file; audit log length/offset read | `live_bd_writes_pass_text_on_stdin`, `live_event_insert_is_bounded`, `live_bead_and_asks`, `live_lifecycle_show_and_unhold`, `live_ask_history_and_audit_log` (fake `bd`/`spira-lc` scripts) |
| store JSON shapes (bd warnings before JSON, dolt string-encoded columns) | `parse_store_outputs` |

Smoke, read-only, 2026-09-28 (the loop shut down): `spira-claim unpoison --bead sp-n9z --cause … --dry-run`
under conf.sh read the live events (attempts 26 by the fold) and refused with
`FAIL sp-n9z: cannot tell (spira-lc show: exit 2: … Access denied for user 'spira_lc' …) — nothing was written`
— the spira-lc socket service is down with the loop, and the fallback connection has no
grant. That is the fail-closed path working: unpoison cannot run while the lifecycle store is
unreachable, which is correct (it cannot know whether it released the hold).

### 8.5 Behaviour deliberately changed from unpoison.sh

1. **Verify is NULL-safe** (sp-r66qd): the fold returns 0 for a bead with no events after the
   floor; unpoison.sh's `attempts_of` printed `<nil>` and every genuine clear FAILed.
2. **Watch reads the audit log**, not sentinel.log: CHECK 4 and CHECK 5 run in the audit
   worker. It now also requires the pass to have *started* after the clear (unpoison.sh
   accepted any `CHECK5:` line after the clear, which a pass already running could print),
   and a pass whose counts failed does not count.
3. **Fail closed on unreadable lifecycle/events**: unpoison.sh read a failed `lc_held` as
   "not poisoned".
4. **A failed floor write stops the bead** instead of carrying on to release the hold.
5. **Live-work refusal also covers a lifecycle `WORKING` row with a holder**, and names the
   exit (slay.sh).
6. **Ask match is exact** (§8.3), so requeue/reclaim asks are no longer closed.
7. **Requeue count** for verify comes from the fold (§5 item 12's defect).
8. **Exit codes** 1→3 for failure, 2→1 for usage (§8.2).
9. **Watch timeout** 25 min → 40 min, configurable.
10. **`--credit` and `--actor`** are new, for groomer.sh's cutover.

### 8.6 Cutover (not performed; the Concierge applies it)

Line numbers against this branch's base (4764d03ec).

1. **spira/unpoison.sh** — delete the file.
2. **spira/conf.sh** — `SPIRA_CLAIM_BIN` export: §5 item 1 (same line, needed here too).
3. **spira/groomer.sh:476-492** (from `_up_labels="$(…label list…)"` through
   `printf 'UNPOISONED …'`) →
   ```bash
       _up_out="$("${SPIRA_CLAIM_BIN:?SPIRA_CLAIM_BIN is unset — source conf.sh}" unpoison \
           --bead "$id" --cause "$cause: $evidence" --credit "$cause" --actor groomer)"; _up_rc=$?
       printf '%s\n' "$_up_out"
       case "$_up_out" in
           "OK   $id:"*) ;;
           *) printf 'groomer: unpoison: %s was not cleared (rc=%s) — see above\n' "$id" "$_up_rc" >&2; exit 1 ;;
       esac
       printf 'UNPOISONED %s cause=%s\n' "$id" "$cause"
   ```
   and delete groomer.sh:473-474 (`. "$HERE/lib.sh"` is then unused by this branch — keep
   it if a later edit needs it). Why: groomer's own copy checked the vestigial
   `spira-poison` **label** (so every hold-only poison since sp-i2m7y was refused as
   "nothing to lift") and never released the lifecycle hold (so CHECK 4 kept it). The
   `--cause` slug must match `[a-z0-9-]{1,64}`; groomer's documented causes all do.
   Comment block groomer.sh:433-449: replace "Credits the charge … the charge does not carry
   forward." with "Delegates to `spira-claim unpoison --credit <cause>` (spira-claim/DESIGN.md
   §8): the unjudged credit, the poison.cleared floor, the lifecycle hold, the note and the
   ask, verified by CHECK 4's own decision."
4. **spira/test-groomer-unpoison.sh** — its stub bd cannot answer `spira-lc`/`bd sql --json`
   events; repoint: the two refusal cases (no `--cause`, no `--evidence`) stay as they are
   (groomer refuses before calling anything); the "does not carry spira-poison" case becomes
   "a bead that is not poisoned is refused" with `SPIRA_CLAIM_BIN` pointed at a stub printing
   `SKIP <id>: …` (groomer exits 1 because there is no `OK` line); the full-trail case uses a
   stub `SPIRA_CLAIM_BIN` that records its argv and prints `OK   poisoned-1: cleared …`, and
   asserts `--credit yield-headless`, `--actor groomer` and the evidence in `--cause`. The
   write trail itself is covered by `cargo test -p spira-claim` (§8.4).
5. **spira/test-unpoison.sh** (the e2e against a real spira-lc and a server testdb) —
   repoint rather than retire; it is the one test of the real INSERT, Unhold and close:
   - :20-26 also build spira-claim beside spira-lc (:83-85):
     `"$CARGO_BIN" build --manifest-path "$REPO/spira-claim/Cargo.toml" --quiet` and
     `export SPIRA_CLAIM_BIN="$CARGO_TARGET_DIR_FOR_BUILD/debug/spira-claim"`; export
     `SPIRA_RUN="$TMP/run"` (mkdir) so exit 2 is not hit.
   - :104 `UNPOISON="$HERE/unpoison.sh"` → `UNPOISON="$SPIRA_CLAIM_BIN"`, and every
     `bash "$UNPOISON" ` (:127, :146, :147, :152, :155) → `"$UNPOISON" unpoison `.
   - :146 expected code `"2"` → `"1"` (usage); :148 `"1"` → `"3"`; :155 `"2"` → `"1"`.
   - :11 `covers:` → `spira-claim/* spira-lc/* lifecycle/*`.
   - :3 and :126 comment/echo: `unpoison.sh` → `spira-claim unpoison`.
6. **spira/lib.sh:3264** comment ("a caller right after unpoison.sh") — deleted with
   `_attempts_sql_query` by §5 item 2.
7. **sentinel/DESIGN.md (concierge/rw-sentinel) §2.2 row `CHECK5: …` and cutover row 18** —
   both become moot (nothing parses `CHECK5:` any more); drop them when that branch lands.
8. **Operator guidance** (not in this repository):
   - `~/.claude/projects/-home-ryan-spira-brain/memory/feedback_unpoison_tool.md` — every
     `spira/unpoison.sh --bead <id> --cause "<evidence>" [--watch]` →
     `spira-claim unpoison --bead <id> --cause "<evidence>" [--watch]`; "If it prints FAIL"
     stays.
   - `MEMORY.md` line "Clear poison with unpoison.sh" → "Clear poison with spira-claim
     unpoison".
   - brain wiki pages that mention unpoison.sh are dated records (notes, log) and stay as
     written.
9. **spira/chamber/groomer.md:184-187** — unchanged: it invokes `groomer.sh unpoison`,
   whose interface does not change.
10. Build/install: `spira-claim` is already on the workspace and §5 item 15's artifact list.
