# spira-lc — landstate semantics

Design for moving what `$SPIRA_RUN/landstate/` means into spira-lc, so that every reader of
that directory can be deleted. Scope is the *semantics* (what a reader needs, where each fact
lives afterwards); the deletions are the beads that follow.

## 1. What the directory is

One file per bead, `<id>`: `<STATE> <tip|none> <epoch> [reason…]`, written atomically by
`landing-pass mark` (the only writer), plus sidecars `<id>.ejected` (suites, first line) and
`<id>.evict-seen`. `STATE` ∈ WITHDRAWN RED EJECTED GATING GATED CERTIFIED BATCHED CONTENT
LANDED REBASED(`pr-open:<n>` | `swept` | `recut-swept`). Each `land_mark` also appends a
`landing-event` tsd row, which is history, not state.

## 2. What readers actually need

Every reader asks one of five questions. None needs the file format.

| # | question | readers |
|---|---|---|
| Q1 | **tip** — the commit this bead was certified/delivered at | queue certified-list, land-local, publish range, in-delivery refusal, classify (`tip_ancestor_of_base`) |
| Q2 | **position** — certified / batched / pr-open / landed / swept | classify, queue eject, waiters (`active_blockers`), batcher-cut |
| Q3 | **verdict evidence** — WITHDRAWN vs RED vs EJECTED, and its tip | classify (Rework vs Ready), queue abandon (a verdict an abandon never overturns), reopen (withdraw only a still-certified tip) |
| Q4 | **ejected suites** — the suites a re-certification must force | gate re-entry check |
| Q5 | **idempotence marker** — eviction already handled | aeon eviction race |

## 3. Where each fact lives after the cutover

**Q1 and Q2 are already row facts.** The `bead` row carries `tip` and `state`; `tip` is
retained across REWORK (`GateRed`/`Returned` change only `state` and `reason`), and every
position above has a state (CERTIFIED, IN_DELIVERY + `delivery.state`/`pr`, LANDED). No new
column. Readers switch to `spira-lc show|list`; `active_blockers` already has its ON-mode
form (`active_blockers_lc`) and loses only the file variant.

**Q3 and Q4 are evidence the row does not keep**, because `reason` is overwritten by the next
event and WITHDRAWN has no event of its own. They go in one new append-only table, written
in the same transaction as the event that causes them, so the evidence and the transition
cannot disagree:

    CREATE TABLE IF NOT EXISTS verdict (
        seq      BIGINT AUTO_INCREMENT PRIMARY KEY,
        bead_id  VARCHAR(64) NOT NULL,
        kind     VARCHAR(16) NOT NULL,   -- withdrawn | red | ejected
        tip      VARCHAR(64) NULL,
        suites   TEXT NULL,              -- comma-separated; ejected/red only
        reason   TEXT NULL,
        event_seq BIGINT NOT NULL,       -- the event row that caused it
        at       BIGINT NOT NULL
    );

- Append-only and grant-limited like `event` (INSERT/SELECT only). History for free; "the
  current verdict" is the newest row for the bead **whose `tip` equals the bead row's `tip`**.
  A verdict about a superseded tip is not evidence about this one — the same tip invariant
  every event already enforces.
- `kind` is the landstate vocabulary kept on purpose: RED is the operator's verdict, EJECTED
  the automated attribution, WITHDRAWN a certification voided without fault. They map to
  different follow-ups, so they must not be collapsed into `reason` text.

**Event surface.** No new bead states. Three evidence-carrying changes to existing events:

| event | change |
|---|---|
| `Returned{reason}` | gains optional `kind` (`red`\|`ejected`), `suites`; inserts the `verdict` row |
| `GateRed{tip,reason}` | inserts a `red` row (suites when the gate knows them) |
| certified-and-not-yet-batched reopen | a **new** event `Withdraw{tip}`, legal from CERTIFIED only: → READY, tip kept, inserts `withdrawn`. Today this transition is a file write beside a bd reopen and the machine never sees it — the one place the file holds a fact the machine lacks. |

`classify` stops reading the file: `landstate` in `BeadFacts` is derived from the row plus
the newest matching `verdict`, so rules 1–3 and the Rework/Ready split are unchanged, and
the existing classifier fixtures run unmodified against the derived facts.

**Q4** becomes `spira-lc verdict <id>` (JSON: kind, tip, suites, reason) and the gate's
re-entry check reads suites from the newest verdict for the bead's current tip. This also
removes the sidecar's reason for existing: it survived overwriting by REBASED/WITHDRAWN, and
the table is never overwritten.

**Q5** needs no machine state: an eviction-race marker is a function of the event log (an
`ejected`/`returned` event for the bead at the tip). The reader derives it from `history`.

## 4. Decisions

- **Evidence table, not columns on `bead`.** Columns are overwritten by the next event; the
  reason the sidecar existed was exactly that. The table is the same shape as `event`, which
  is already the project's answer to "must survive the next transition".
- **Keep RED/EJECTED/WITHDRAWN as `kind` values, not machine states.** The machine already
  has REWORK/READY; the distinction is evidence for callers, not a different set of legal
  moves, and a state per word would double the transition tables for no new refusal.
- **`Withdraw` is an event, not a verb on the file.** The file write beside it is why the
  machine and the ledger could disagree; one transaction removes the case.
- **REBASED sub-states are not ported.** `pr-open:<n>` is `delivery.pr`; `swept` and
  `recut-swept` are what `Requeued` with a moved tip already does (SUBMITTED, tip voided).
  GATING/GATED are SUBMITTED. CONTENT is `ContentOnBase`. Nothing lost; nothing added.
- **The `landing-event` tsd family stays** as history. It is a log of the machine's events,
  not a second source of truth, and no reader branches on it.

## 5. Cutover and deletion order

Per `law-a-cutover-is-certified-per-path`, each reader path is cut and certified alone.

1. Migration `0003-verdict.sql` + schema.sql + grants; `Withdraw`; evidence on `Returned`/
   `GateRed`. Writers dual-write: the machine event *and* `land_mark`, as today.
2. `spira-lc verdict` / `show` expose the derived facts; parity suite compares, for every
   bead in a fixture ledger, file-derived and row-derived Q1–Q4 answers (a planted
   disagreement must be reported — an absent diff proves nothing without it).
3. Readers move one path at a time: classify, queue (tip/position, then abandon/eject), gate
   re-entry, waiters, reopen/unpoison, batcher-cut.
4. When none read the directory, `land_mark` loses its file write; the
   `lifecycle-guard` landstate allowlist then has nothing to allow and is deleted with the
   directory readers (the parent's deliverable).

## 6. Not decided here

Backfill of `verdict` for beads currently RED/EJECTED/WITHDRAWN in the ledger: derive from
the file at migration time (state, tip, reason; suites from the sidecar). It is a one-shot
in step 1's migration, so it belongs to that bead.
