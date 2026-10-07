# incident

Rust port of `spira/incident.sh` (sp-0ekp7, rewrite wave 7b, law-new-subsystems-are-rust,
law-rust-rewrites-start-from-intent). `spira/incident.sh` becomes a thin shim —
`. lib.sh; exec incident "$@"` — the same pattern `gate.sh`, `czar.sh`, `reconciler.sh` and
`maechen-trigger.sh` (sp-0ekp7's sibling script) already use for their own Rust replacements.

## Intent

Turn a production event — a crashed systemd unit, a suite's flake finding, a base-red gate,
a reconciler gap — into a bead Ops (or, for a code defect, the builder partition) can claim,
with an identity that survives across recurrences. Three properties are load-bearing:

1. **Write-ahead, then file.** The payload is spooled to disk *before* the database is
   touched, and the spool entry is removed only once the bead exists. A production event
   arrives exactly once and cannot be re-asked for.
2. **Dedupe on `external_ref`, not on wording.** A flapping unit is one incident, not one
   bead per failure. A second filing of the same ref bumps a recurrence on the existing bead
   (reopening it first if it had been handed on) rather than filing a duplicate — unless
   that bead's lifecycle row is terminal (LANDED/DONE/SUPERSEDED/DROPPED): then the
   recurrence is a new incident, filed fresh and citing the closed one as its predecessor
   (sp-nmlna).
3. **A Sin escalates exactly once.** Past `SIN_AT` recurrences with no fix holding, the
   operator is paged once — a second page would bury the first.

## Contract

```
incident systemd <unit>           file an incident for a failed systemd user unit
incident file <title> [-|<file>]  file one from an arbitrary payload
incident drain                    file everything the spool is holding
incident list                     open incidents
```

Environment (unchanged names from the bash): `SPIRA_DB` (required — refuses if unset, never
falls through to bd's own auto-discovery), `SPIRA_RUN`, `SPIRA_SPOOL`, `SPIRA_INCIDENT_LOG`,
`SPIRA_SIN_AT` (5), `SPIRA_SIN_EXEMPT`, `SPIRA_INCIDENT_DEDUP_LOOKBACK` (7 days),
`SPIRA_WATCHER_INTERVAL_S` (1800), `SPIRA_INCIDENT_CAUSE`, `SPIRA_INCIDENT_UNIT`,
`SPIRA_INCIDENT_PATH`, `SPIRA_INCIDENT_LABELS` (default `spira,$SPIRA_INCIDENT_LABEL`),
`SPIRA_INCIDENT_REPO`, `SPIRA_INCIDENT_TYPE` (bug), `SPIRA_INCIDENT_PRIORITY`,
`SPIRA_INCIDENT_ACTOR`/`BEADS_ACTOR`, `SPIRA_INCIDENT_DELIVERS`, `SPIRA_SOP_LEDGER`,
`SPIRA_INCIDENT_REF`, `SPIRA_ASK_LABEL` (needs-ryan), `SPIRA_HOME_REPO`, `SPIRA_REPO_MAP`,
`SPIRA_BD`, `BD_TIMEOUT`, `SPIRA_BDQ_CONN_RETRIES`.

Stdout contract unchanged: `systemd`/`file`/`drain` print the filed/bumped bead id on the
last non-empty line (callers already grep for this — batcher-cut, sentinel, landing-pass);
`drain` also prints `drained N, still spooled M`, always both numbers.

Exit codes unchanged: `0` filed or deduped; `1` the database is unreachable, the guards
refused the filing, or (for `drain`) at least one entry is still stuck.

## Retired (rule one: retire rather than port)

- **`backfill-ref-labels`, `retire-unsatisfiable-delivers`, `repair-mismatch-delivers`** —
  one-time migrations, each already believed applied (their own bash comments say so — e.g.
  "this does not need to run again"), with no systemd unit, no script, and no Rust caller
  invoking them; only `test-incident-migrations.sh` (deleted) exercised them. Anything
  without a live caller is deleted, not ported.
- **`incident-dedup-decision.py`** — extracted from the bash purely so a T1 suite could test
  `_dedup_incident`'s scan without a fixture database. That scan is now `decide::dedup_scan`,
  a native Rust unit test; the Python helper had no other caller (confirmed by grep) and is
  deleted.
- **Four bash suites that reached inside `incident.sh`'s shell internals** —
  `test-incident-decisions.sh`, `test-incident-recur-cause.sh`,
  `test-counter-events-sentinel.sh` (all three `. "$HERE/incident.sh"` to call `file_one`/
  `_dedup_incident` directly) and `test-incident-migrations.sh` (drove the retired
  subcommands). A shim has nothing to source; their coverage moves to this crate's unit
  tests (`decide::tests::dedup_scan_*`, `recur_note_body_*`, `sin_thresholds`, `reopen_cause_*`,
  `invalid_repo_label_*`). Removed from `spira/config-fence-allow` and
  `spira/tier-budget-allowlist` in the same change (both are shrink-only lists).
- **The bash's age-in-hours clause in the Sin ask** silently never fired: it grepped bd's
  `--json` for a literal `"created"` key, but real bd's schema names the field
  `created_at` (verified against a live `bd create`/`bd show --json` in this session).
  `real.rs::show_created_at` reads `created_at` (falling back to `created`), so the Sin
  ask's "over Xh Ym" clause now actually appears — a named, deliberate difference, not a
  silent behaviour change (wave-brief "name any intended difference").

## Kept and ported faithfully

- **The `bd create` guards `bdq` applies underneath every filing** —
  `_bdq_check_repo_label`, `_bdq_check_destructive`, `_bdq_check_schema_delete` — are ported
  to `decide::invalid_repo_label`/`destructive_phrase`/`contains_schema_delete` and enforced
  in `run::file_new` before calling `Bd::create`. `real.rs` shells to `bd` directly (not
  through `bdq`/lib.sh), so without this port these three safety fences would silently stop
  applying to every incident filing — a real regression, not a simplification. They are
  small, self-contained, and change rarely; lib.sh's own rewrite wave can delete these three
  functions here once `bdq`'s fences live in a shared crate this one can depend on instead.
  (`bdq`'s remaining behaviour — the czar-fence gate, the `SPIRA_BDJSON_FIXTURE` seam, the
  connection-retry loop — is either inapplicable to this crate's writes or already
  reimplemented in `real.rs::RealBd::run`.)
- Everything else the bash's `file_one`/`drain_one`/`spool_write`/`_dedup_incident` did:
  the write-ahead spool (byte-identical on-disk format, so a spool directory populated by
  the bash mid-cutover still drains under the new binary), the two-pass label-keyed-then-
  fallback dedupe, the reopen-vs-recurrence classification, the payload-hash dedup on the
  recurrence note body, the Sin escalation mail, the undeclared-repo ask, the `delivers:`
  label decision.

## Schema / architecture

- `decide.rs` — pure decision logic, no IO: ref hashing, cause sanitisation, the dedupe scan,
  reopen-cause classification, the recurrence-note payload-hash dedupe, the Sin threshold,
  the three `bd create` guards, plus the small pure helpers (`home_repo`, `repo_names`,
  `provenance`, `unit_from_cgroup_line`) `main.rs` composes at the edges. Every function is a
  table-driven unit test.
- `ports.rs` — the `Bd`/`Mailer`/`Clock` traits: the seam between decision logic and
  everything stateful. `bump_recur`/`recurs_of` (the events-table counters) are built here on
  top of `Bd::sql`.
- `real.rs` — the host implementation: shells to `bd` (with the same connection-retry-on-
  "invalid connection" `bdq` does) and `mail.sh`.
- `spool.rs` — the write-ahead spool file format (byte-identical to the bash's
  `spool_write`/`spool_field`/`spool_body`).
- `run.rs` — orchestration (`file_one`, the four-pass dedupe scan) matching the bash's
  control flow, with a fake `Bd`/`Mailer`/`Clock` harness for its own unit tests.
- `main.rs` — CLI dispatch: reads the environment once, builds a `run::FileConfig`, and
  implements `systemd`/`file`/`drain`/`list` directly (the spool read/write and the
  `SPIRA_DB`-unset refusal live here, since they are this binary's own IO, not a port any
  fake needs to stand in for).

## Non-goals (out of scope for this bead)

- **`mail.sh`, `bd` themselves.** Not this wave.
- **Moving `bdq`'s fences into a shared crate.** Noted above as a `lib.sh`-wave follow-up.
- **Repointing every Rust caller off the literal name `incident.sh`.** `batcher-cut/src/io.rs`
  and `release/src/canary.rs` call `Command::new("incident.sh")` directly (not through
  `bash`); `sentinel/src/check5.rs`, `reconciler/src/main.rs` and
  `testenv/src/suites/real.rs` wrap it in an explicit `Command::new("bash").arg(path)`. The
  shim at `spira/incident.sh` satisfies both calling conventions unchanged (a two-line script
  execs a binary; `bash <script>` still works), so correctness does not depend on touching
  any of the five — and `watchtower.sh` (sp-lnmbq, concurrent, not touched by this bead) does
  `command -v incident.sh`, which only the shim's literal filename satisfies (cargo refuses a
  `.` in a binary target name, so a bare compiled `incident.sh` is not possible). Repointing
  those five call sites to invoke `incident` directly — deleting the shim once
  `watchtower.sh`'s own rewrite repoints its four call sites — is left as a cheap follow-up,
  not a blocker.

## Parity

- 38 unit tests in `decide.rs`/`run.rs`/`spool.rs` cover every decision the bash's
  `file_one`/`_dedup_incident`/`_recur_note_body`/`_reopen_cause`/the three `bdq` guards made,
  against hand-built fixtures plus the real `bd` schema fields verified in this session
  (`created_at`, `closed_at`, `external_ref`, `labels`) against a live throwaway `bd init`
  store.
- Black-box parity: the real-bd suites `test-incident.sh`, `test-incident-systemd.sh`,
  `test-incident-spool-drain.sh`, `test-incident-delivers-satisfiable.sh`,
  `test-incident-delivers-reopen-mismatch.sh`, `test-sin-exempt.sh` and
  `test-mail-real-senders.sh`'s incident section all invoke `incident.sh` by name (or via
  `SPIRA_INCIDENT_SH`) unchanged — they now exercise the shim → the compiled binary, and are
  this bead's end-to-end parity evidence once run through `testenv` (never on the host).

## Decisions

- **The binary is named `incident`, not `incident.sh`** — cargo rejects a `.` in a binary
  target name (verified: `error: invalid character '.' in crate name`). `spira/incident.sh`
  stays as a permanent two-line shim rather than being deleted, specifically so
  `watchtower.sh`'s `command -v incident.sh` (four call sites, owned by the concurrent
  sp-lnmbq rewrite) keeps resolving without editing a file this bead does not own.
- **`bd create`'s three safety guards are ported, not dropped**, despite calling `bd`
  directly instead of through `bdq` — see "Kept and ported faithfully" above. Rejected the
  alternative (skip them, since `incident.sh` "never enforced them itself, only ever called
  bdq") as a real regression: `bdq` applies them to every `bd create` incident.sh has ever
  made, so a Rust port that calls `bd` directly and skips them would let a destructive-phrase
  title or an invalid `repo:` label through where the bash never did.
- **The age-in-hours clause in the Sin ask now actually fires** (see "Retired" above) —
  reads `created_at`, the real bd field, instead of the bash's dead `"created"` grep.
- **Delivered at P1** (bead priority).
- **The dedup reads an incident's state from its lifecycle row (sp-jgjvh).** An incident bead
  is a work bead (Ops claims it), so design §3.4 holds: bd holds content, spira-lc holds
  state. The four passes ask by scope — unfinished (READY/WORKING/REWORK) or handed on (past
  the builder) — and `real.rs` takes each bead's state from one `spira-lc list`, with bd
  supplying only content (labels, `external_ref`, and `closed_at` for the lookback window).
  A bead with no row is in neither pass. A pass that cannot read bd **or** the machine is
  not "nothing found": the event stays spooled (law-a-control-that-cannot-check-must-refuse),
  where the bash skipped a failed pass and filed a fresh bead.
- **A recurrence of a terminal incident files a fresh bead (sp-nmlna).** The machine has no
  move out of LANDED/DONE/SUPERSEDED/DROPPED, so the old "reopen in bd" left the row LANDED
  and every event inside the lookback reopened the bead again, piling notes under a row
  that would never move. `decide::status_of_lc` now reads a terminal row as
  `BeadStatus::Terminal` (distinct from `Closed`, handed on but still movable), and
  `dedup_scan` returns `DedupHit::Terminal` for it — preferring any movable handed-on hit,
  then the most recently closed terminal one. `run::file_one` sends that hit through the
  ordinary `file_new` path (same guards, same labels, a fresh lifecycle row via
  `Bd::create`), with a body that opens "Recurrence of <id> (predecessor)" above the
  payload, then links the two with `Bd::relate` (`bd dep relate`, a non-blocking
  relates-to). The closed bead gets no reopen, note, label or recurrence count, and its row
  is untouched. The next event finds the fresh bead in the unfinished pass and bumps it as
  usual. Kept: a handed-on but non-terminal incident (SUBMITTED/CERTIFIED/IN_DELIVERY) is
  still reopened, since its row can still move. Rejected: giving the machine a move out of
  a terminal state, which would break "terminal" for every other consumer; and keeping
  the recurrence count on the old bead, since its Sin escalation can never be worked under
  a LANDED row (the fresh bead counts from zero and its body names the history). A failed
  `relate` is logged, not fatal: the bead exists and its body still cites the
  predecessor.

- **`incident settle` closes an incident whose fix has landed (sp-8acg4g).** The incident's
  `blocks` edges are its fixes. When every one is LANDED/DONE and, for the home repo, the
  active release carries its landing commit, the incident's own detector runs once: quiet
  closes it through the machine with the fix id, landing sha and timestamp; still firing
  notes "fix landed but the alarm persists" and labels `settle-fired:<hash of fixes>` so a
  later pass records nothing more, and Ops claims it as the unblocked bead it already is.
  The only detector today is `systemctl --user is-failed` for a `incident:<unit>` ref; any
  other incident has no probe and is never closed on a guess. While a fix is unlanded the
  `blocks` edge keeps the incident unclaimable, and `spira-claim` no longer lets an
  incident-labelled bead stack on a merely certified fix (`rank::stack_cap`). Run by
  `spira-incident-settle.timer`.
