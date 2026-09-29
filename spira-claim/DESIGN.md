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
- **Not a writer.** It never writes the bead store, the lifecycle store or any file. Every
  event it counts is written by someone else (`bead_reopen`, `bump_*`, bd itself).
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
12. **spira/unpoison.sh:123** `"$(requeues_of "$id")"` → `"$("$SPIRA_CLAIM_BIN" requeues "$id")"`
    (it passed the RAW `requeued` row count to check4_decide's requeue slot, a different
    number from what CHECK 4 uses; this makes them agree).
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

## 6a. Lifecycle switch: claim selection

**Finding (operator, 2026-09-28):** the lifecycle machine was never deployed on this host.
There is no `spira_lifecycle` database, no `spira_lc` grant, and no service or socket.
**Decision:** `lifecycle_enforce` is THE switch for everything that touches the lifecycle
machine. `unpoison` records the same switch for itself in §8.7. This section covers
`select`, and the two must agree: what off-mode unpoison clears (the `spira-poison` label)
is exactly what off-mode `select` refuses.

**Resolution** (main.rs `lifecycle_on` → `spira_config::lifecycle_enforce`, the aeon
crate's rule and unpoison's):
- `SPIRA_LIFECYCLE_ENFORCE` wins: `1`/`true` is on, and anything else, including empty, is off.
- Else `spira.lifecycle_enforce`.
- Else **off**.

`SPIRA_LC_BIN`'s existence is never consulted.

| | **off** (production today) | **on** |
|---|---|---|
| spira-lc | **never run**; `--lifecycle F` is ignored with a stderr note | `spira-lc list` (or `--lifecycle F`) in machine mode |
| the poison | the `spira-poison` bd label: a labelled row is dropped in **both** blockers modes | the lifecycle `poison` hold (machine mode, §2 rule 1) |
| `--blockers bd` (default) | today's rows, less any `spira-poison`-labelled row. Normally that is none, because the caller's `bd ready` excludes the label already (`exclude_default`) | unchanged, bit for bit (§2) |
| `--blockers machine` | `rank::claimable_legacy`. It uses legacy records: not labelled `spira-poison`, and every `blocks` target bd-`closed`. There is no stacking (depth 0), because stacking exists only in the machine | `rank::claimable`, the stacked-dependents rule (§2) |
| unreachable machine | irrelevant | `cannot tell` (exit 2, empty stdout): `… lifecycle snapshot: … (lifecycle_enforce is on, so the machine must answer)` |
| `attempts`/`requeues`/`counts`/`decide`/`poison-decide`/`epics` | no lifecycle call in any era. `poison-decide --poisoned` is the caller's read | same |

**Tests** (tests.rs pins the switch per thread through `ENFORCE`, so neither the host's
environment nor its spira.toml leaks in):
- `off_machine_mode_is_the_legacy_rule_and_never_reads_the_lifecycle`: with no
  `--lifecycle`, and with a garbage one, it still answers, so neither source was read.
- `off_spira_poison_label_is_not_claimable_in_either_mode`
- `on_bd_mode_is_unchanged_and_the_label_is_not_the_poison`
- `on_machine_mode_unreachable_machine_is_cannot_tell`
- The three earlier machine-mode tests now run with the switch on.

**Cutover addition:** §5 item 11 (machine selection) is gated on `lifecycle_enforce` by the
caller. Under off, `--blockers machine` is still safe: it gives the legacy answer and never
contacts the machine.

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
