# Test plan — Operator channel and codified knowledge (`operator-channel`)

Part of [[test-plan-2026-09-23]], section 5. Area id `operator-channel`; use-case ids are `UC-operator-channel-NN`.

Scope: everything that carries words between the harness, agents and Ryan. That covers Maildir mail (send/read/lint/tidy/deliver/done/sendmail), decision-bead filing and verdicts flowing back, the answer watchers (`answers.py`, `cockpit/watch-answers.sh`, `watchd.sh tail/notify/prune/health-ids`, `watch-refresh.sh`), the concierge session, and the stores of learned procedure (`sop.sh`, `rule.sh`, `render_memories`). The dashboards that display these words belong to cockpit-observability.

Primary files: 31 bash suites, **436 CI suite-seconds** (main-push run 35947142904, all `ok`). Secondary: test-ops-closing.sh (102 s, owned by aeon-execution), test-cockpit-watcher-owner.sh, test-mail-pane.sh, test-unit-restart-limits.sh, test-unmapped-repo-park.sh and the panel `model.rs`/`store.rs`/`main.rs` Rust tests. Their verdicts are noted here only where they overlap this area.

---

## 1. Intent (the de facto spec)

The channel gets a message from a harness component or an aeon into a mailbox exactly once. It refuses malformed or content-free messages at send time, using rules held as data in `mail/kinds/*.md`. It also refuses to spam the operator with repeats. When a question is filed it creates a separate decision bead that relates to the cited work, and that bead never blocks the work unless an override says so. The operator's reply closes that decision bead, writes the verdict onto the work bead, and reaches the right reader: the sender's mailbox, or the concierge. The reply is rendered distinctly when Ryan rejects the premise, and it wakes the reader exactly once.

Watchers deliver each event to one reader. They escalate a stale backlog or an unhealthy daemon to Ryan once per condition, not once per pass. They stay quiet during a deliberate halt. They restart themselves when their code goes stale, and they fail loudly rather than pass on bad configuration.

The knowledge stores behave as follows. SOPs are validated on write and on lint. Their applications go to an append-only ledger whose `log` has three distinct results (0 present, 1 absent, 2 unreadable). A statute is announced as live only when its wiki projection regenerated. Agents receive core statutes in full and the rest as an index, within a budget.

---

## 2. Use cases

Tier key: T0 static · T1 unit · T2 component · T3 integration · T4 acceptance. Where key: **cert** = certification/gate (every commit), **CI** = batch + main CI, **main** = main CI only, **acc** = acceptance.yml.

### A. Mail transport and message discipline (`spira/mail.sh`, `mail/kinds`)

| ID | Requirement | Dimensions | Tier / where |
|---|---|---|---|
| UC-operator-channel-01 | `send` delivers atomically via `tmp/` into `new/`; `read` prints headers+body and moves new→cur; `read` with nothing unread fails; `list [--unread]`, `count` and `unread-age` report the mailbox state (unread-age is empty when there is no unread mail). | correctness, contract | T2 / cert |
| UC-operator-channel-02 | Lint refuses a message with no From or no Subject, a subject that is or starts with a bead id, a context-free bead id in the body (key:value metadata allowed), and a non-RFC 5322 From (bare display name, RFC 6854 group). An omitted `--from` falls back to `SPIRA_MAIL_FROM`. | correctness, fail-closed, contract | **T1** / cert |
| UC-operator-channel-03 | Kinds are data. An unknown kind is refused and named. `template <kind>` prints the skeleton. The `requires:` fields in a kind file drive validation: decision/question need `--default`, an empty required section is refused and named, `--urgent` needs `## Why it is urgent`, and a body promising "the ask/question below" must carry the section. Editing a kind file changes what is accepted. | correctness, config-compat, contract | **T1** / cert |
| UC-operator-channel-04 | `SPIRA_MAIL_LINT_CONSIDERED` bypasses lint and records `X-Spira-Lint-Override: <reason>`. | fail-closed, observability | T1 / cert |
| UC-operator-channel-05 | Every message a real harness sender emits (lib.sh, sentinel, watchd, reflect, strand, pilgrimage, skew, incident, archivist) passes lint. | contract | T2 (call the real emitters with mail.sh captured) / CI |
| UC-operator-channel-06 | Operator-mailbox repeat guard: a send whose normalised subject matches one inside `SPIRA_MAIL_REPEAT_WINDOW` is refused, names the override and is recorded under `$SPIRA_RUN/mail-repeat`. `SPIRA_MAIL_REPEAT_CONSIDERED` lets it through with a header. A lint-refused send writes no stamp. The guard does not apply to other mailboxes. | idempotency, correctness | normalisation T1 + ordering T2 / cert |
| UC-operator-channel-07 | `done` sets the Maildir R flag idempotently (it fails on an unknown id), and a `sendmail` reply marks the original R. | idempotency | T2 / cert |
| UC-operator-channel-08 | `tidy` keeps open-ask mail (label from `SPIRA_ASK_LABEL`), fresh unread mail and `X-Spira-Urgent` mail younger than 7 days, plus only the newest of a repeated subject. It archives read-old and closed-ask mail, refuses and moves nothing when the bead store is unreadable, and moves nothing under `--dry-run`. | correctness, fail-closed | T2 with stubbed `bd list` / cert |
| UC-operator-channel-09 | Aged unread backlog in a registered mailbox alerts the operator once, clears when drained, and re-arms on recurrence. Fresh mail never alerts. | observability, idempotency, recovery | T2 / cert |
| UC-operator-channel-10 | The delivery daemon treats a SIGTERM exit (143) as clean, and `world.sh start` revives an enabled-but-inactive `mail-deliver` without restarting an active one. `watchd notify` escalates once when the daemon is down AND concierge mail has aged. | recovery, idempotency | T2 behavioural, systemctl mocked / CI |
| UC-operator-channel-11 | Aeon mailbox: the PostToolUse hook injects each message into the running session exactly once (new→cur) and stays silent on an empty mailbox or empty `BEAD_ID`. `send aeon:<id>` is refused when that aeon has no mailbox. | correctness, idempotency, fail-closed | T2 (no db) / cert |
| UC-operator-channel-12 | `bead.sh amend` on an in-progress bead mails its live aeon and sends nothing when no aeon is alive. The aeon's mailbox is removed when the aeon exits. | contract, recovery | T3 / CI |
| UC-operator-channel-13 | Chamber briefs never tell an agent to pass a `send` option that `cmd_send` does not accept. No caller of the retired `cockpit/ask.sh` remains. | contract | T0 / cert |

### B. Decisions and verdict flow-back (`mail.sh sendmail`, `lib.sh`, `answers.py`, `verify-asks.sh`)

| ID | Requirement | Dimensions | Tier / where |
|---|---|---|---|
| UC-operator-channel-14 | `send --kind question|decision --bead W` files a separate decision bead, names it in `X-Spira-Bead`, and links it to W as relates_to, never as a blocking dependency (sp-aybfy). It logs the refusal of the blocking edge. `SPIRA_MAIL_ALLOW_BLOCKING=1` restores the blocking edge. | correctness, contract | T3 (real bd is the observable) / CI |
| UC-operator-channel-15 | A reply to a message carrying `X-Spira-Bead` closes that bead, leaves the work bead open and writes the verdict and reply body into the work bead's notes. Under ALLOW_BLOCKING the reply also unblocks the work bead. | correctness, contract | T3 / CI |
| UC-operator-channel-16 | The mail client's accept-default key (`aerc/accept-default.sh`) closes the decision with the message's `X-Spira-Default` as the verdict, using a real spira.conf. | config-compat, contract | T3 / CI |
| UC-operator-channel-17 | Reply routing: a reply goes to the sender's mailbox when one exists. It goes to concierge when the sender is a chamber persona, when the sender has no mailbox, or when there is no In-Reply-To. | correctness | **T2** (no db) / cert |
| UC-operator-channel-18 | Suit verdict words (uphold / retire / amend) close the suit bead, each with its own close reason (`upheld`/`retired`/`amended`). | correctness, contract | T1 reason mapping + T3 one close / CI |
| UC-operator-channel-19 | If the bead close fails, `sendmail` exits non-zero naming the bead and leaves the original message unmarked. | fail-closed | T2 (stub bd close failing) / cert |
| UC-operator-channel-20 | `answers.py` renders a verdict as `RYAN ANSWERED` / `verdict on`. A premise rejection (label `premise-rejected` OR reason prefix `premise-rejected:`) renders as `RYAN REJECTED THE PREMISE` / `PREMISE REJECTED` with its reason and never as an answer. | correctness, contract | **T1** over fixture bead JSON / cert |
| UC-operator-channel-21 | Every monitor headline carries the bead's own `closed_at`, in the format `watchd.sh` parses. | contract | T1 (shared format constant) / cert |
| UC-operator-channel-22 | Answer-reader cursors: a cold start seeds silently and writes the cursor. Crash recovery (witness present, cursors absent) prints `SEEDED AT` with the count waiting. A normal pass prints no SEEDED line. | recovery, observability | T1 / cert |
| UC-operator-channel-23 | `watch-answers.sh loop` wakes the reader exactly once per pass that found answers, and an empty `SPIRA_WAKE` wakes nobody. The loop exits 1 when a pass fails (so systemd restarts it). | recovery, fail-closed | T2 (stub answers.py) / cert |
| UC-operator-channel-24 | `land_escalate` reaches the operator unless an OPEN ask with the same subject exists. A closed ask does not suppress it (sp-yki4). | idempotency | T3 (label-filter fidelity needs bd) / CI |
| UC-operator-channel-25 | Component escalations (for example `skew.sh check --escalate`) carry `## Question` and `## Default`. A missing or failing mailer is reported with its rc and output, not swallowed. Without `--escalate` nothing is sent, and the same condition is escalated once per run dir. | observability, fail-closed, idempotency | T2 / cert |
| UC-operator-channel-26 | `verify-asks.sh --apply` closes an open ask whose `VERIFY:` command exits 0 (the close reason quotes the command). It leaves failing or VERIFY-less asks open, reports but closes nothing without `--apply`, flags a mislabelled epic, never re-reports a closed ask, and reads the label from `SPIRA_ASK_LABEL`. | correctness, fail-closed, config-compat | sweep logic T1 (stub bd) + one real close T3 / CI |

### C. Watchers (`spira/watchd.sh`, `spira/watch-refresh.sh`)

| ID | Requirement | Dimensions | Tier / where |
|---|---|---|---|
| UC-operator-channel-27 | `watchd tail` allows one reader per watcher. A second reader refuses at once (exit 3) and names the holder pid and `--takeover`. Each event is delivered exactly once. `--takeover` kills the incumbent and its children. The flock is released on exit and a stale lock file never blocks a new reader. `--takeover` is refused on non-tail verbs. | concurrency, idempotency, recovery | T2 (real processes and flock) / CI |
| UC-operator-channel-28 | `notify` events half: an actionable line (matching `SPIRA_ACTIONABLE` from config) unread longer than `SPIRA_NOTIFY_AGE` is escalated once, with its text and `--default`. The first sighting only starts the clock, and an `[ISO8601]` prefix dates the line itself (the oldest stamp wins, a malformed stamp falls back to first sighting). Notify never advances the read cursor. A drain clears the clock and the stamp, so a later backlog is heard again, even a byte-identical one. A large backlog is summarised. | correctness, idempotency, observability | T2 with injected clock / cert |
| UC-operator-channel-29 | `notify` health half: a daemon watcher that is inactive, failed or failing its probe beyond the threshold is escalated once with NRestarts, its last log line and the probe stderr. Recovery clears the clock. No answer from systemd is not a fault. The events and health halves keep separate stamps. | recovery, observability, idempotency | T2 / cert |
| UC-operator-channel-30 | During a world halt (`$SPIRA_RUN/world.halted`), `status` reads HALTED rather than DEGRADED, and `notify` escalates nothing and clears the unhealthy clocks. | correctness, contract | T2 / cert |
| UC-operator-channel-31 | `notify` exits 3 rather than passing when the manifest is malformed or missing, the filter is empty, the threshold is non-numeric, it gets an argument, or there is no delivery path. A refused escalation channel stamps nothing. | fail-closed, recovery | T2 / cert |
| UC-operator-channel-32 | `watch-refresh` restarts an active daemon watcher when its target, a sibling library, the config, conf.sh, the manifest, the dispatcher or its unit's ExecStart binary is newer than its start time. It never restarts otherwise, never touches log rows or inactive units, and names the cause. It keeps a meter that counts successful restarts only and supports `--dry-run`. A steady pass costs exactly 2 execs and runs no bd/dolt/git/python3/date. | correctness, performance, idempotency | T1 (already function-level with PATH shims) / cert |
| UC-operator-channel-33 | A refresh pass fails loudly and restarts nothing when `systemctl show` fails, the start time is unreadable or the manifest is malformed. The orphan reaper SIGTERMs watcher processes outside a spira-watch cgroup and spares supervised units and session `tail` readers (sp-2bb8, sp-fcom). | fail-closed, recovery | T1 (fake /proc) / cert |
| UC-operator-channel-34 | `health-ids` reads the id prefix from the database's `issue_prefix`, not from the goal (sp-c57o), and falls back to `SPIRA_ID_PREFIX`. A state file with none of our ids, or no state file at all, is DEGRADED (exit 1). An unusable prefix exits 2. | correctness, config-compat | **T1** (stub `SPIRA_BD`) / cert |
| UC-operator-channel-35 | `prune` removes the runtime files of watcher names that are no longer in the manifest and keeps active ones. A malformed manifest removes nothing, and a missing dir exits 0. | correctness, fail-closed | **T1** / cert |
| UC-operator-channel-36 | A watcher-filed incident refiled while blocked by an open dependency stays open, records no reopen event and stays out of `bd ready`. A closed incident refiled within `SPIRA_WATCHER_INTERVAL_S` reopens with cause `closed-while-live`, and one refiled outside it reopens with cause `recurrence`. | correctness, observability | cause classifier T1; events-table write T3 / main |
| UC-operator-channel-37 | Watcher and notifier units (`spira-watch@`, `-notify`, `-refresh`, `-pr-notify`, `-mail-tidy`) render with CPUQuota, Nice, reachable restart limits, no `@` placeholders and config-derived paths only. The notify timer fits 3 passes inside the default threshold. Install enables timers, not services. | config-compat, recovery | T0 (one shared unit-render lint) / cert |
| UC-operator-channel-38 | `pr-notify` classifies each PR in a pr-mode repo as GREEN, RED or skipped (pending, null or empty checks) and ignores push/hold repos. It reports a `spira/*` branch that is ahead of base with no PR, stays silent when there is nothing to report, and appends to the log unless `--show`. | correctness, observability | classifier T1 + branch case T2 / cert |

### D. Concierge and codified knowledge (`concierge.sh`, `lib.sh render_memories`, `sop.sh`, `rule.sh`)

| ID | Requirement | Dimensions | Tier / where |
|---|---|---|---|
| UC-operator-channel-39 | A fayth with `FAYTH_SUMMON=operator` never enters the task pool or the lanes. The shipped concierge is operator-only and the builder stays summonable. | correctness, contract | T1 (fixture chamber) / cert |
| UC-operator-channel-40 | Concierge resume: the resume id is used only when the recorded cwd matches the brain dir. The SessionStart hook records the session only when `SPIRA_CONCIERGE=1` and refuses a `SPIRA_PROD` symlink that resolves elsewhere. Live-pid detection works. `start` exits 3 (HEADLESS) when a holder is live but tmux is gone. A dangling `--resume` retries fresh and clears the record. | recovery, concurrency, security-fence | T1/T2 in CI; cgroup survival and layout pane T4 / acc (host) |
| UC-operator-channel-41 | The composed brief has no surviving `{{`, names an executable mail.sh and bead.sh (the harness one when `SPIRA_WIKI` is unset), and carries the statutes. A missing persona brief, or a core set with no recognised slugs, refuses (exit 1). | fail-closed, contract | T1 over a fixture statute set (not the live book) / cert |
| UC-operator-channel-42 | `render_memories` renders `SPIRA_STATUTE_CORE` in full and every other statute as exactly one slug line. An over-budget core statute falls back to a slug. The index heading names an executable `rule.sh show`. The cache is written on a miss, served on a fresh hit and ignored when stale. | correctness, performance, config-compat | **T1** (slug→body seam) + one real-bd read T2 / cert |
| UC-operator-channel-43 | `rule.sh enact` prints "Statute is live" only when the synth hook succeeds. A failing, missing or non-executable hook is non-zero and prints "NOT regenerated". `law-synth.sh` refuses an empty or under-half store unless `LAW_SYNTH_OVERRIDE=1`. `cockpit.sh statute` reports `?` without a wiki and MISMATCH/OK otherwise. | fail-closed, observability | T2 / CI (+ brain half in acc) |
| UC-operator-channel-44 | SOP validation (write and lint): SYMPTOM/CHECK/FIX present, MATCH is a valid ERE, 250-word cap, no operator-specific `/home` path, METRIC well-formed. Each rule flags only its own violation, and lint on an unreadable DB fails closed. | correctness, fail-closed, security-fence | **T1** validator over strings + one T2 fail-closed read / cert |
| UC-operator-channel-45 | The SOP ledger: `applied` appends one complete JSON record at the configured path, append-only in epoch order, and refuses bad arguments while writing nothing. A beadless record is keyed by `--pass`. An unreadable shelf still records `shelf=unreadable` and exits non-zero. `--why` is capped in the ledger and kept whole on the bead. `log` exits 0, 1 or 2. `digest`, `ledger-init`, `match` and the METRIC downgrade behave as specified. | correctness, fail-closed, observability, idempotency | T2 against a fake shelf + one real-bd note case / cert |

---

## 3. Coverage map

The "file::case" entries use the mapper's case names. The cost column shows ci_secs for the whole file (from signals.tsv).

| UC | Existing tests | Level & cost now | Verdict |
|---|---|---|---|
| 01 | test-mail.sh::send lands in new/, read, list, unread-age; test-mail-kinds.sh::count 0/1/3; test-mail-aeon.sh (send lands) | T2, 5 s + 3 s | **KEEP** in test-mail.sh (Maildir half) |
| 02 | test-mail.sh::missing From/Subject, subject bead id, sparse bead id, bare display name, group syntax, --from fallback | T2 fork-per-case, 5 s | **DEMOTE-TO-T1**: table-driven over `_lint_check` |
| 03 | test-mail-kinds.sh::unknown kind, template, decision without --default, empty ## Note/## Grounds, urgent, ask-below, custom kind; test-mail.sh::decision/question default, ask-below promise | T2, 3 s + dup in 5 s | **MERGE-INTO test-mail.sh** as rows of the same T1 table (DEMOTE-TO-T1) |
| 04 | test-mail.sh::lint override; test-mail-kinds.sh::lint override + header | T2, dup | MERGE-INTO test-mail.sh (one row, which also checks the header) |
| 05 | test-mail.sh::7 harness senders pass; test-migrate-ask.sh::11 sender bodies pass lint (hand-copied bodies) | T2, 1 s | **DELETE** migrate-ask lint half (it tests copies, not the senders); replace with the gap G-05 behaviour test |
| 06 | test-mail-repeat.sh (all 8 cases) | T2, 2 s | **MERGE-INTO test-mail.sh**. Normalisation → T1. Fix `isnz` on refused count → exact count |
| 07 | test-mail-pane.sh::done R flag / bad id / sendmail sets R (secondary) | T2, 2 s | KEEP (in cockpit-observability); move the `done` cases into test-mail.sh when that file is next touched |
| 08 | test-mail-tidy.sh::open-ask kept, A..E3, unreadable store, dry-run; ::service/timer/units.sh greps | T3 (embedded Dolt), 9 s | **DEMOTE-TO-T2** with a stub `bd` answering the ask-label list. Unit greps **→ T0 unit lint**. Stop counting setup steps as passes |
| 09 | test-mail-health.sh (all) | T2, 3 s | KEEP (canonical path for "aged unread"; see cluster D4). Use the standard summary line |
| 10 | test-mail-deliver.sh::SuccessExitStatus grep, start-branch greps (5), escalation fires/no dup/empty mailbox | T2 + source grep, 3 s | **SOURCE-GREP**: replace the 5 `world.sh start` greps with a behavioural `world.sh start` against the systemctl mock. Keep the notify cases |
| 11 | test-mail-aeon.sh::SEEN RED (a) hook, SEEN GREEN (e), once not twice, BEAD_ID empty, aeon:<id> live/absent | T3 file, 16 s (hook cases take ms) | **DEMOTE-TO-T2**: split into test-mail-aeon-hook.sh (no testdb) |
| 12 | test-mail-aeon.sh::SEEN RED (b) amend, SEEN GREEN (c), SEEN RED (d) mailbox gone | T3, remainder of 16 s | KEEP T3. Add a positive control that the mailbox existed during the run (case d) |
| 13 | test-brief-mail-options.sh (all); test-migrate-ask.sh::no ask.sh callers, ask.sh deleted | T0, 4 s + 1 s | KEEP both as **T0** in the lint stage. Parse `cmd_send` options once, not once per brief |
| 14 | test-mail-decision-ask.sh::X-Spira-Bead, 0 blocking deps, override blocks; test-archivist-cited-bead.sh (all 7) | T3, 15 s + 8 s | **DELETE test-archivist-cited-bead.sh** (subset; never runs archivist.sh). Move its "guard logged the refusal" assert into decision-ask |
| 15 | test-mail-decision-ask.sh::sendmail closes decision bead, unblocks, verdict note; test-mail-sendmail.sh::reply closes tracking bead, accept-default verdict note | T3, 15 s + 20 s (two testdbs) | **MERGE**: test-mail-decision-ask.sh MERGE-INTO test-mail-sendmail.sh → one `test-verdict-flow.sh` on one testdb |
| 16 | test-mail-sendmail.sh::accept-default (5 asserts) | T3 | KEEP in the merged file |
| 17 | test-mail-sendmail.sh::routed to sender, no sender mailbox → concierge, no In-Reply-To; test-mail-decision-ask.sh::reply routes to concierge | T3 (routing needs no db) | **DEMOTE-TO-T2**: move into test-mail.sh with `X-Spira-Bead` absent |
| 18 | test-mail-sendmail.sh::suit uphold/retire/amend each close suit bead | T3; only asserts "closed" | KEEP one T3 close. **Add** a T1 table for the reason mapping (the mail.sh:454-460 branch is untested for its output) |
| 19 | test-mail-sendmail.sh::missing bead non-zero, original stays in new/ | T3 | DEMOTE-TO-T2 (stub bd close rc≠0) |
| 20 | test-answers-premise-rejected.sh::monitor/session verdict vs rejection, reason text | T3 (testdb + bd close), 18 s | **DEMOTE-TO-T1**: feed fixture bead JSON to answers.py. Cover label-only and prefix-only separately (gap G-07) |
| 21 | test-answers-premise-rejected.sh::watchd stamp matcher located, stamp == closed_at, not pinned now | T3 + **source extraction** of a sed regex from watchd.sh | **SOURCE-GREP**: export the stamp format from one place (answers.py emits and watchd consumes a shared constant) and test the round trip at T1 |
| 22 | test-answers-silent-seed.sh::cold start, recovery SEEDED AT, 1 answer, normal pass | T3, 5 s | **MERGE-INTO test-answers.sh** (T1 fixture) |
| 23 | test-answers-premise-rejected.sh::watcher printed, woke once, empty SPIRA_WAKE; test-answers-silent-seed.sh::unfixed/fixed inline loop (tautological), real loop exits 1 | T3 with `timeout 6` loops | KEEP the real-loop cases as T2 with a stub answers.py. **DELETE** the two inline-bash "defect 1" cases |
| 24 | test-sentinel.sh (3 cases) | T3, 5 s; misnamed | **MERGE-INTO test-verdict-flow.sh** (same testdb). Rename the section "land_escalate". Set `SPIRA_LAND_ESCALATE_EVERY=0` explicitly |
| 25 | test-skew-escalate.sh (all) | T2, 2 s | KEEP. Fix the header (still says SPIRA_NOTIFY/ask.sh) |
| 26 | test-verify-asks.sh (all 9 groups) | T3, 31 s (6 reset/seed cycles) | **DEMOTE-TO-T1** for the sweep classification (stub `cockpit_beads` rows). Keep 1 real `--apply` close at T3 on a single seed |
| 27 | test-watchd-tail.sh (all 12) | T3-ish, 60 s (≈17 s fixed sleeps + 8 s polls) | KEEP at T2, but **replace fixed sleeps with polling** on `tailers`/output. Relax the ≤3 s wall-clock assert to "refused before the next event" |
| 28 | test-watch-notify.sh::first sighting … oldest stamp decides (≈20 cases) | T2, 29 s | KEEP. Inject the clock (`SPIRA_NOW`) in place of the 3× `sleep 1`. **Move the install/render section to the T0 unit lint** |
| 29 | test-watch-notify.sh::healthy … events and health independent; test-watchd-halt-health.sh::notify escalates without stamp; test-cockpit-watcher-owner.sh::notify orphan (secondary) | T2 | KEEP in watch-notify. Halt-health's control case is a duplicate |
| 30 | test-watchd-halt-health.sh::no stamp → DEGRADED, stamp → HALTED, notify with stamp | T2, 4 s | **MERGE-INTO test-watch-notify.sh** (it shares the fixture). Drop the dead notify.sh stub |
| 31 | test-watch-notify.sh::malformed manifest, empty filter, non-numeric threshold, missing manifest, refused channel, no mail.sh, takes no args | T2 | KEEP |
| 32 | test-watch-refresh.sh::steady pass cost, restart on stale file ×6, ExecStart elsewhere, argv first word, never ×4, log row, inactive, meter, dry-run | T1-shaped, 28 s | KEEP as T1 (model template). **Stop copying spira/*.sh twice**: symlink or point `SPIRA_HOME`. Move the install section to T0 |
| 33 | test-watch-refresh.sh::systemctl show fails, unreadable start time, malformed manifest, reaper ×3, entry point, unknown arg | T1 | KEEP |
| 34 | test-watchd-health-ids.sh (5) | T3 (testdb for one `bd config get`), 24 s | **DEMOTE-TO-T1** with a stub `SPIRA_BD` that prints a prefix. Add the exit-2 case. Leave the `issue_prefix` contract to a single bd-contract suite. Make SKIP exit 77, not 0 |
| 35 | test-watchd-prune.sh (5) | T2, **22 s for 3 invocations** | DEMOTE-TO-T1 (pure file ops). **Profile first**: the 22 s is startup, not work (see §7) |
| 36 | test-watcher-reopen.sh::pc1/pc2/case1/case2 | T3 server-mode Dolt, 45 s | DEMOTE-TO-T1 the interval→cause classifier. **MERGE-INTO test-incident-recur-cause.sh** the events-table and dep-blocked cases. Delete case2 (duplicates pc2) and the unconditional "dep added" ok |
| 37 | test-watch-unit-restart.sh (5); test-unit-restart-limits.sh (secondary); render/install sections of test-watch-notify.sh, test-watch-refresh.sh, test-pr-notify.sh; unit greps of test-mail-tidy.sh | T0 + T2 install runs | **DELETE test-watch-unit-restart.sh** (the `systemd/*.service` glob in test-unit-restart-limits.sh already covers it). Fold the render/install checks into one T0 unit-render lint |
| 38 | test-pr-notify.sh::positive control … push/hold not scanned | T2, 22 s (full install.sh run + ~10 invocations) | DEMOTE-TO-T1 the gh-JSON classifier. Keep one T2 branch-without-PR case. Install section → T0 |
| 39 | test-concierge.sh::roster (task pool/lane/operator exact, shipped concierge) | T1-shaped inside a T4-shaped file, 6 s | KEEP as T1 in a split test-concierge-roster.sh (also overlaps test-fayth-predicates.sh; keep one) |
| 40 | test-concierge.sh::_resume-id, session hook recording (8), _live-pid, HEADLESS ×2, dangling resume, start survives cgroup, layout pane | mixed; ~40 % skipped in CI and the skips are not counted | Split: T1/T2 cases → CI. **DELETE the duplicate HEADLESS and duplicate launcher `--resume` sections.** systemd/tmux cases → host-only T4 with skips **reported as skips** |
| 41 | test-concierge.sh::brief composed (reads the **live** statute book), missing brief, all-typo core, no-wiki brief | non-hermetic | DEMOTE-TO-T1 with a fixture statute set via the render_memories seam |
| 42 | test-render-memories.sh (13) | T3 (testdb as key-value store), 7 s | **DEMOTE-TO-T1** (12 cases over a slug→body fixture); keep 1 real-bd read |
| 43 | test-statute-projection.sh (12) | T3, 5 s; law-synth half **silently skipped** in CI | KEEP at T2. Make the skip a counted skip (or vendor a law-synth fixture) so a green run does not hide ~14 assertions |
| 44 | test-sop-lint.sh (13); test-sop.sh::lint METRIC (4) | T3, 8 s + part of 24 s | **MERGE**: test-sop-lint.sh MERGE-INTO test-sop.sh as a T1 validator table. Keep 1 fail-closed read. The "gate calls sop.sh lint" grep → T0 |
| 45 | test-sop.sh (≈110 asserts); test-ops-closing.sh (secondary, 102 s: SOP closing rule, which is now **off** for every shipped persona, sp-q27cp) | T3, 24 s | KEEP at T2 behind a shelf seam. **Stop invoking sop.sh twice per rc+output pair.** The ops.md brief-text check → T0 brief lint (**SOURCE-GREP**). test-ops-closing.sh's SOP half is flagged to its owning area as testing a mechanism no persona enables |

---

## 4. Duplicate clusters

| # | Behaviour | Files | Evidence | Keep |
|---|---|---|---|---|
| D1 | Question files a relates_to decision bead; ALLOW_BLOCKING restores the edge | test-archivist-cited-bead.sh, test-mail-decision-ask.sh | Same 0-deps-before/after + override → 1 dep assertions. archivist-cited-bead never runs archivist.sh (it only sets a From header) | test-mail-decision-ask.sh (merged into verdict-flow). Delete archivist-cited-bead (−8 s) |
| D2 | Reply closes the decision bead, verdict note on the work bead, routing to concierge | test-mail-decision-ask.sh, test-mail-sendmail.sh | Both build their own embedded-Dolt testdb (15 s + 20 s). Both assert close-on-reply and the verdict note | One `test-verdict-flow.sh` on one testdb, which also absorbs test-sentinel.sh (a third testdb, 5 s) |
| D3 | Default-required, ask-below promise, lint override | test-mail.sh, test-mail-kinds.sh, test-migrate-ask.sh | Identical refuse/accept pairs forked through mail.sh in two files. migrate-ask re-types sender bodies | test-mail.sh as the single T1 lint table |
| D4 | Alarm on aged unread concierge mail | test-mail-health.sh (mail-health.sh), test-mail-deliver.sh (watchd `mail-deliver` extern watcher) | **Product duplication**: `watchd.sh cmd_notify` runs `_wd_notify_health` (which probes `mail.sh unread-age` for mail-deliver) and then `bash mail-health.sh` in the same pass (watchd.sh:1488-1494) | Decide on one canonical alarm (recommend mail-health.sh, the mailbox-agnostic one, and make the mail-deliver health probe daemon-liveness only), then keep one test. See G-01 |
| D5 | Answers harness | test-answers-premise-rejected.sh, test-answers-silent-seed.sh | Same answers.py + watch-answers loop fixture, each with its own testdb | One test-answers.sh: T1 fixture JSON + 2 T2 loop cases |
| D6 | Notify health-half escalation of a matured unhealthy daemon | test-watch-notify.sh, test-watchd-halt-health.sh, test-cockpit-watcher-owner.sh (notify pc) | halt-health's "notify escalates without stamp" control is the same case as watch-notify's "matured unhealthy watcher escalates" | test-watch-notify.sh (+2 halt cases) |
| D7 | Restart limiter reachable / Restart=always / StartLimit in [Unit] | test-watch-unit-restart.sh, test-unit-restart-limits.sh | The latter globs `systemd/*.service`, which includes `spira-watch@.service` | test-unit-restart-limits.sh. Delete test-watch-unit-restart.sh |
| D8 | Rendered unit: CPUQuota/Nice/no `@`/config-only paths; install enables the timer, not the service | test-watch-notify.sh, test-watch-refresh.sh, test-pr-notify.sh, test-mail-tidy.sh (+ test-verdict-timer.sh, test-install-* elsewhere) | Same stub-systemctl + `install.sh` run pattern repeated per unit, each paying a full install run | One T0 `test-units-lint.sh` iterating every rendered unit |
| D9 | SOP back-door lint plant/forget | test-sop.sh (METRIC), test-sop-lint.sh | Same plant via `bd remember` → lint → forget pattern | T1 validator table in test-sop.sh |
| D10 | Within-file duplicates | test-concierge.sh (HEADLESS ×2, launcher --resume ×2); test-watcher-reopen.sh (pc2 = case2) | Mapper notes | Keep one of each |

---

## 5. Unit-extractable logic

| Logic | Where | Tested today via | Seam for a T1 test |
|---|---|---|---|
| Message lint | `mail.sh:_lint_check(from, subject, kind, default, urgent, body)` (line 125). It is already a pure function of its arguments plus the kind files | 30–50 forks of `mail.sh send` into a Maildir per file | mail.sh has **no main guard** (the `case "$1"` at line 723 runs and exits 1 when sourced). Add `[ "${BASH_SOURCE[0]}" = "$0" ] \|\| return 0` before the dispatch (watchd.sh:1615 already does this), or add `mail.sh lint` reading a message on stdin |
| Repeat-guard subject normalisation | `mail.sh:_repeat_check` (line 61) | full sends | same main guard; call `_repeat_check` with `SPIRA_RUN` in a tmp dir |
| Tidy keep/archive decision | `mail.sh:cmd_tidy` (bd list `--label $SPIRA_ASK_LABEL`, `urgent_max_s=604800`) | embedded-Dolt testdb for two beads | a `SPIRA_BD` stub printing the open-ask JSON; separate the per-message verdict function from the `mv` |
| Reply routing | `mail.sh:_reply_mailbox` (line 428) | real-bd testdb in two files | main guard; call it with a fixture original message |
| Suit verdict words → close reason | `mail.sh:_sendmail_close_bead` (lines 448-490) | only "bead is closed" | extract `_suit_reason <body>` → table test |
| Verdict / premise rendering, stamp, seeding | `spira/answers.py` (premise detection lines 268-269, 401) | bd create/close + `timeout 6` loops | answers.py already reads bead JSON; add a `--from-json <file>` input (or a stub bd on PATH) and a fixed `--now` |
| VERIFY sweep | `cockpit/verify-asks.sh` (it reads rows through `cockpit_beads`) | 6 testdb reset/seed cycles, 31 s | a stub `cockpit_beads` / `BD` emitting rows; assert the SATISFIED / still open / MISLABELLED lines and the close calls recorded by the stub |
| Statute tiering / budget / cache | `lib.sh:render_memories` | real bd as a key-value store, 12 `bash -c` sourcings of lib.sh | the cache file is already a seam: pre-write a fixture cache (or `SPIRA_MEMORIES_CMD`) and drive every case except one from it |
| SOP validator | `sop.sh` lint/write field checks | ~20 bd round-trips | a `sop.sh validate` reading an SOP body on stdin (no shelf) |
| SOP ledger / log / digest | `sop.sh applied/log/digest` | ~90 sop.sh→bd calls | the ledger is already a file (`SOP_LEDGER`); add a `SOP_SHELF_CMD` seam for the shelf read so only the bead-note cases need bd |
| health-ids prefix | `watchd.sh:cmd_health_ids` | testdb up/drop for one `bd config get issue_prefix` | `SPIRA_BD` stub, which is already honoured |
| Reopen-cause classification | `incident.sh` interval compare | server-mode Dolt, 45 s | extract `_reopen_cause <closed_epoch> <now> <interval>` |
| PR classification | `pr-notify.sh` gh JSON → GREEN/RED/skip | 10 invocations + a full install | the gh stub already exists; the cost is the install section, not the logic |
| Concierge roster, resume id, hook record | `lib.sh:spira_*_fayths`, `concierge.sh _resume-id`, `hooks/session.sh` | in a 760-line mixed file with live statute book | already callable as subcommands; move them to their own file with a fixture chamber and a fixture statute cache |

---

## 6. Gaps (implied by code or cited scars, covered by nothing)

1. **G-01 Double escalation of one mail backlog.** Every `watchd.sh notify` pass runs `_wd_notify_health` and `mail-health.sh` back to back (watchd.sh:1488-1494). When the delivery daemon is down and concierge mail is aged, both paths can mail Ryan about the same backlog. No test runs notify with a registered mailbox and a down daemon together. Add a T2 case asserting exactly one operator message, or retire one path (D4).
2. **G-02 `tidy` urgent retention.** The header's rule 3 (`X-Spira-Urgent` kept for 7 days, `urgent_max_s=604800` in `cmd_tidy`) is untested at both edges.
3. **G-03 Suit verdict effects.** uphold, retire and amend produce different close reasons (`mail.sh:454-460`, default `amended`), but test-mail-sendmail.sh only asserts `closed`. An unrecognised suit word has no test.
4. **G-04 verify-asks safety.** `VERIFY:` runs unattended as `timeout 120 bash -c "$cmd"` (verify-asks.sh:57). Nothing covers the timeout (a hanging VERIFY), a VERIFY with side effects, or the unreachable-DB branch, which prints "checked nothing" and **exits 0**. Decide whether that exit is intended (fail-open) and pin it with a test.
5. **G-05 Real senders pass lint.** test-migrate-ask.sh lints hand-copied bodies, so a regression in a real emitter (`land_escalate`, `skew.sh escalate`, `watchd _wd_ask`, `incident.sh`, `archivist.sh`) goes unseen. A T2 test should invoke each emitter with `mail.sh` pointed at a scratch Maildir and without the lint override.
6. **G-06 Premise-rejected detection branches.** answers.py accepts the label OR the reason prefix (lines 268-269). The only test sets both on the same bead (test-answers-premise-rejected.sh:60-69), so neither branch is independently covered.
7. **G-07 `rule.sh retire`, `list` and `show` behaviour.** Only `enact` is tested (test-statute-projection.sh). `list` appears only through the concierge brief, and `retire` is untested, although CLAUDE.md prescribes it for superseded statutes.
8. **G-08 `render_memories` delivery fence.** CLAUDE.md records that a hand-started session receives no statutes, and `render_memories` is called only from aeon.sh and concierge.sh. No test asserts that each fayth-composed entry point includes the `# Memories in force` section.
9. **G-09 health-ids exit 2.** The "unusable prefix" refusal (watchd.sh:1632) has no case, and the SKIP path of test-watchd-health-ids.sh exits 0, so a missing engine reads as green.
10. **G-10 `mail-fence.sh`.** It exists beside mail.sh and has its own test-mail-fence.sh, but taxonomy.json assigns that test to another area. Confirm which area owns it so its behaviour sits in one plan.
11. **G-11 Concurrent sends.** `_mail_msgid` is `date +%s.$RANDOM.$$`. Nothing tests two senders in the same second, or concurrent repeat-guard stamps (the guard's check-then-stamp is not atomic).
12. **G-12 Skips that are not reported.** test-concierge.sh (~40 % of its assertions) and test-statute-projection.sh (the law-synth half plus the MISMATCH/OK cases) skip silently and still report green. This is a reporting gap: skips must be counted in the standard record (T4/acceptance for the host-only parts).

---

## 7. Cost

**Current (primary files, ci_secs from signals.tsv):**
answers-premise-rejected 18 + answers-silent-seed 5 + archivist-cited-bead 8 + brief-mail-options 4 + concierge 6 + mail-aeon 16 + mail-decision-ask 15 + mail-deliver 3 + mail-health 3 + mail-kinds 3 + mail-repeat 2 + mail-sendmail 20 + mail-tidy 9 + mail 5 + migrate-ask 1 + pr-notify 22 + render-memories 7 + sentinel 5 + skew-escalate 2 + sop-lint 8 + sop 24 + statute-projection 5 + verify-asks 31 + watch-notify 29 + watch-refresh 28 + watch-unit-restart 2 + watchd-halt-health 4 + watchd-health-ids 24 + watchd-prune 22 + watchd-tail 60 + watcher-reopen 45 = **436 s** across 31 files.

**Projected after the verdicts** (22 suites):

| Suite after | From | Projected s | Basis |
|---|---|---|---|
| test-mail.sh (T1 lint table + T2 Maildir, repeat, routing) | mail 5 + kinds 3 + repeat 2 + migrate-ask 1 = 11 | 5 | lint rows in-process; about 15 Maildir forks remain |
| test-verdict-flow.sh (one testdb) | decision-ask 15 + sendmail 20 + sentinel 5 + archivist 8 = 48 | 20 | one testdb_up (~5 s) + ~25 bd round-trips |
| test-answers.sh | premise 18 + silent-seed 5 = 23 | 4 | T1 fixture JSON; 2 short T2 loop runs |
| test-mail-aeon (hook T2 + lifecycle T3) | 16 | 13 | the aeon.sh run and testdb stay |
| test-mail-tidy.sh (stub bd) | 9 | 2 | no testdb |
| test-mail-deliver.sh / test-mail-health.sh / test-skew-escalate.sh | 3 + 3 + 2 | 3 + 3 + 2 | unchanged |
| test-brief-mail-options.sh (T0) | 4 | 1 | parse cmd_send once |
| test-concierge (CI part) | 6 | 4 | systemd/tmux parts move to host acceptance |
| test-render-memories.sh | 7 | 5 | T1 + one real-bd read (testdb_up dominates) |
| test-sop.sh (+sop-lint) | 24 + 8 = 32 | 10 | T1 validator; shelf seam; no double invocation |
| test-statute-projection.sh | 5 | 5 | unchanged |
| test-verify-asks.sh | 31 | 6 | T1 sweep + one seed/close |
| test-watch-notify.sh (+halt-health) | 29 + 4 = 33 | 16 | clock injection (−3 s sleeps), install section out (~8–10 s), +2 cases |
| test-watch-refresh.sh | 28 | 8 | no double copy of spira/*.sh; install section out |
| test-watchd-health-ids.sh | 24 | 1 | stub SPIRA_BD |
| test-watchd-prune.sh | 22 | 3 | *after profiling* (see note) |
| test-watchd-tail.sh | 60 | 15 | ~17 s fixed sleeps and ~8 s polls → event-driven polling |
| test-watcher-reopen (T1 + merged cases) | 45 | 11 | T1 classifier ~1 s + ~10 s marginal inside test-incident-recur-cause.sh's existing server db |
| test-pr-notify.sh | 22 | 4 | install run out; classifier T1 |
| test-watch-unit-restart.sh | 2 | 0 | deleted (D7) |
| share of new T0 test-units-lint.sh | — | 5 | one render pass for all units, attributed here |

Sum: 5 + 20 + 4 + 13 + 2 + 3 + 3 + 2 + 1 + 4 + 5 + 10 + 5 + 6 + 16 + 8 + 1 + 3 + 15 + 11 + 4 + 0 + 5 = **146 s**.

**436 → 146 suite-seconds (−290 s, −67 %)**, and 31 → 22 suites (plus one shared T0 lint). Where the saving comes from: testdb elimination or consolidation (verdict-flow, answers, health-ids, verify-asks, tidy, render-memories: ≈125 s), fixed sleeps in watchd-tail (≈45 s), install/render sections repeated per unit (≈40 s), and conf.sh/startup overhead in the watchd suites (≈40 s, which depends on profiling).

Note on the estimate. test-watchd-prune.sh spends 22 s on 3 `watchd.sh` invocations and test-watchd-health-ids.sh spends 24 s where its testdb alone should take ~5 s. That suggests `watchd.sh`/`conf.sh` startup under `env -i` in the container costs several seconds per invocation. If so, it is the single biggest lever in this area: watch-notify makes ~60 invocations and watch-refresh ~30. It should be profiled before a tier is committed. If startup cannot be fixed, the prune/notify/refresh projections rise by ~15 s in total (→ ~161 s).

---

---

## 8. UC catalogue (machine-readable index for spira/plan-lint.sh)

One bullet per use case declared in section 2 above, in the format
`docs/test-plan/README.md` requires (`* \`UC-<area>-NN\` [T<n>] — <one-line>`).
The tier here is the cheapest tier that would catch a regression per that
schema; a covering suite's own `# tier:` need not match exactly. Text is
abridged from section 2's full requirement — section 2 is authoritative.

* `UC-operator-channel-01` [T2] — send delivers atomically via tmp/ into new/; read prints headers+body and moves new→cur; read with nothing unread fails; list [--unread], count and unread-age…
* `UC-operator-channel-02` [T1] — Lint refuses a message with no From or no Subject, a subject that is or starts with a bead id, a context-free bead id in the body (key:value metadata allowed),…
* `UC-operator-channel-03` [T1] — Kinds are data. An unknown kind is refused and named. template <kind> prints the skeleton. The requires: fields in a kind file drive validation:…
* `UC-operator-channel-04` [T1] — SPIRA_MAIL_LINT_CONSIDERED bypasses lint and records X-Spira-Lint-Override: <reason>.
* `UC-operator-channel-05` [T2] — Every message a real harness sender emits (lib.sh, sentinel, watchd, reflect, strand, pilgrimage, skew, incident, archivist) passes lint.
* `UC-operator-channel-06` [T1] — Operator-mailbox repeat guard: a send whose normalised subject matches one inside SPIRA_MAIL_REPEAT_WINDOW is refused, names the override and is recorded under…
* `UC-operator-channel-07` [T2] — done sets the Maildir R flag idempotently (it fails on an unknown id), and a sendmail reply marks the original R.
* `UC-operator-channel-08` [T2] — tidy keeps open-ask mail (label from SPIRA_ASK_LABEL), fresh unread mail and X-Spira-Urgent mail younger than 7 days, plus only the newest of a repeated…
* `UC-operator-channel-09` [T2] — Aged unread backlog in a registered mailbox alerts the operator once, clears when drained, and re-arms on recurrence. Fresh mail never alerts.
* `UC-operator-channel-10` [T2] — The delivery daemon treats a SIGTERM exit (143) as clean, and world.sh start revives an enabled-but-inactive mail-deliver without restarting an active one.…
* `UC-operator-channel-11` [T2] — Aeon mailbox: the PostToolUse hook injects each message into the running session exactly once (new→cur) and stays silent on an empty mailbox or empty BEAD_ID.…
* `UC-operator-channel-12` [T3] — bead.sh amend on an in-progress bead mails its live aeon and sends nothing when no aeon is alive. The aeon's mailbox is removed when the aeon exits.
* `UC-operator-channel-13` [T0] — Chamber briefs never tell an agent to pass a send option that cmd_send does not accept. No caller of the retired cockpit/ask.sh remains.
* `UC-operator-channel-14` [T3] — send --kind question|decision --bead W files a separate decision bead, names it in X-Spira-Bead, and links it to W as relates_to, never as a blocking dependency.
* `UC-operator-channel-15` [T3] — A reply to a message carrying X-Spira-Bead closes that bead, leaves the work bead open and writes the verdict and reply body into the work bead's notes. Under…
* `UC-operator-channel-16` [T3] — The mail client's accept-default key (aerc/accept-default.sh) closes the decision with the message's X-Spira-Default as the verdict, using a real spira.conf.
* `UC-operator-channel-17` [T2] — Reply routing: a reply goes to the sender's mailbox when one exists. It goes to concierge when the sender is a chamber persona, when the sender has no mailbox,…
* `UC-operator-channel-18` [T1] — Suit verdict words (uphold / retire / amend) close the suit bead, each with its own close reason (upheld/retired/amended).
* `UC-operator-channel-19` [T2] — If the bead close fails, sendmail exits non-zero naming the bead and leaves the original message unmarked.
* `UC-operator-channel-20` [T1] — answers.py renders a verdict as RYAN ANSWERED / verdict on. A premise rejection (label premise-rejected OR reason prefix premise-rejected:) renders as RYAN…
* `UC-operator-channel-21` [T1] — Every monitor headline carries the bead's own closed_at, in the format watchd.sh parses.
* `UC-operator-channel-22` [T1] — Answer-reader cursors: a cold start seeds silently and writes the cursor. Crash recovery (witness present, cursors absent) prints SEEDED AT with the count…
* `UC-operator-channel-23` [T2] — watch-answers.sh loop wakes the reader exactly once per pass that found answers, and an empty SPIRA_WAKE wakes nobody. The loop exits 1 when a pass fails (so…
* `UC-operator-channel-24` [T3] — land_escalate reaches the operator unless an OPEN ask with the same subject exists. A closed ask does not suppress it (sp-yki4).
* `UC-operator-channel-25` [T2] — Component escalations (for example skew.sh check --escalate) carry ## Question and ## Default. A missing or failing mailer is reported with its rc and output,…
* `UC-operator-channel-26` [T1] — verify-asks.sh --apply closes an open ask whose VERIFY: command exits 0 (the close reason quotes the command). It leaves failing or VERIFY-less asks open,…
* `UC-operator-channel-27` [T2] — watchd tail allows one reader per watcher. A second reader refuses at once (exit 3) and names the holder pid and --takeover. Each event is delivered exactly…
* `UC-operator-channel-28` [T2] — notify events half: an actionable line (matching SPIRA_ACTIONABLE from config) unread longer than SPIRA_NOTIFY_AGE is escalated once, with its text and…
* `UC-operator-channel-29` [T2] — notify health half: a daemon watcher that is inactive, failed or failing its probe beyond the threshold is escalated once with NRestarts, its last log line and…
* `UC-operator-channel-30` [T2] — During a world halt ($SPIRA_RUN/world.halted), status reads HALTED rather than DEGRADED, and notify escalates nothing and clears the unhealthy clocks.
* `UC-operator-channel-31` [T2] — notify exits 3 rather than passing when the manifest is malformed or missing, the filter is empty, the threshold is non-numeric, it gets an argument, or there…
* `UC-operator-channel-32` [T1] — watch-refresh restarts an active daemon watcher when its target, a sibling library, the config, conf.sh, the manifest, the dispatcher or its unit's ExecStart…
* `UC-operator-channel-33` [T1] — A refresh pass fails loudly and restarts nothing when systemctl show fails, the start time is unreadable or the manifest is malformed. The orphan reaper…
* `UC-operator-channel-34` [T1] — health-ids reads the id prefix from the database's issue_prefix, not from the goal (sp-c57o), and falls back to SPIRA_ID_PREFIX. A state file with none of our…
* `UC-operator-channel-35` [T1] — prune removes the runtime files of watcher names that are no longer in the manifest and keeps active ones. A malformed manifest removes nothing, and a missing…
* `UC-operator-channel-36` [T1] — A watcher-filed incident refiled while blocked by an open dependency stays open, records no reopen event and stays out of bd ready. A closed incident refiled…
* `UC-operator-channel-37` [T0] — Watcher and notifier units (spira-watch@, -notify, -refresh, -pr-notify, -mail-tidy) render with CPUQuota, Nice, reachable restart limits, no @ placeholders…
* `UC-operator-channel-38` [T1] — pr-notify classifies each PR in a pr-mode repo as GREEN, RED or skipped (pending, null or empty checks) and ignores push/hold repos. It reports a spira/*…
* `UC-operator-channel-39` [T1] — A fayth with FAYTH_SUMMON=operator never enters the task pool or the lanes. The shipped concierge is operator-only and the builder stays summonable.
* `UC-operator-channel-40` [T1] — Concierge resume: the resume id is used only when the recorded cwd matches the brain dir. The SessionStart hook records the session only when SPIRA_CONCIERGE=1…
* `UC-operator-channel-41` [T1] — The composed brief has no surviving {{, names an executable mail.sh and bead.sh (the harness one when SPIRA_WIKI is unset), and carries the statutes. A missing…
* `UC-operator-channel-42` [T1] — render_memories renders SPIRA_STATUTE_CORE in full and every other statute as exactly one slug line. An over-budget core statute falls back to a slug. The…
* `UC-operator-channel-43` [T2] — rule.sh enact prints "Statute is live" only when the synth hook succeeds. A failing, missing or non-executable hook is non-zero and prints "NOT regenerated".…
* `UC-operator-channel-44` [T1] — SOP validation (write and lint): SYMPTOM/CHECK/FIX present, MATCH is a valid ERE, 250-word cap, no operator-specific /home path, METRIC well-formed. Each rule…
* `UC-operator-channel-45` [T2] — The SOP ledger: applied appends one complete JSON record at the configured path, append-only in epoch order, and refuses bad arguments while writing nothing. A…
