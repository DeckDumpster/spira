//! The table of tasks the service hosts, split into one slice per family so each family's
//! rows live in their own file under `tasks/`. `all()` walks every family in `FAMILIES` order;
//! `registrations()` and the boot list in `main.rs` are both read from it, so a task registered
//! is a task booted.
//!
//! Every family file holds two slices. `TASKS` is the rows that are ported, and only those: a
//! row there is booted, read by `Settings::from_lookup` and named in `SPIRA_TASK_MODES`.
//! `PENDING` is the rows written from their contracts whose `decide` and runners are not ported
//! yet; `pending()` walks them. Porting a task is moving its row from `PENDING` to `TASKS`. Every
//! key a pending row declares is already registered config, and every effect it declares
//! already has a sim double, so the move changes nothing else.
//!
//! Each pending row's name is the name of its contract page under the brain's
//! `wiki/projects/spira/designs/tasks/`, by [`contract_page`]: `.` and `_` become `-`
//! (`hel.reap_terminal` is `hel-reap-terminal.md`, `sentinel-call` is `sentinel-call.md`).

use runtime::event::{EventKind, Registration};
use runtime::heartbeat::{self, Heartbeat};
use runtime::units;

use crate::watchers::*;
use crate::{wake, Runner, Settings};

pub mod a;
pub mod b;
pub mod c;
pub mod d;
pub mod e;
pub mod f;
pub mod g;

/// What a task reads from the store, so the engine can wake it on a change rather than poll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reads {
    /// No store read.
    None,
    /// The store keys under these prefixes are every input it decides from: woken by them, it
    /// stops polling, and runs only at a due time it reports or when the in-process sweep finds the store moved.
    Only(&'static [&'static str]),
    /// These prefixes, and inputs no store change reports (files, git, the forge, units, a clock): woken
    /// early by the prefixes, at most once per cadence, and its own cadence stands.
    Polled(&'static [&'static str]),
    /// It reads the store in a way no key prefix states; the reason is in the words.
    Undeclarable(&'static str),
}

impl Reads {
    pub fn prefixes(&self) -> &'static [&'static str] {
        match self {
            Reads::Only(p) | Reads::Polled(p) => p,
            Reads::None | Reads::Undeclarable(_) => &[],
        }
    }
}

#[derive(Clone, Copy)]
pub struct Task {
    pub name: &'static str,
    pub reads: Reads,
    /// `Signal` is shorthand for every effect of the task on the signal plane; `Work` leaves
    /// each effect on the plane `signal` gives it. A row says which, explicitly.
    pub plane: runtime::Plane,
    /// The task's effects on the signal plane: those whose only act is to page, send mail or
    /// append to the inbox, run through a halt; every other effect is held. No signal effect
    /// may write the lifecycle store, start a unit or touch git ([`signal_violations`]).
    pub signal: &'static [&'static str],
    /// The first tick, handed to `Engine::start` each time the process starts: (id, stream, kind).
    pub boot: fn() -> (&'static str, &'static str, EventKind),
    pub cadence_ms: fn(&Settings) -> u64,
    /// Every effect or probe name the handler can ask for.
    pub effects: &'static [&'static str],
    /// The runners of the effects this task alone emits; an effect several tasks emit is in `SHARED`.
    pub runners: &'static [(&'static str, Runner)],
    /// Every config key building the task reads, as slices so a wrapped crate's own list joins whole.
    pub keys: &'static [&'static [&'static str]],
    pub build: fn(&Settings) -> Registration,
}

impl Task {
    pub fn stream(&self) -> &'static str {
        (self.boot)().1
    }

    /// The plane a halt reads for this task's effect `name`.
    pub fn plane_of(&self, name: &str) -> runtime::Plane {
        if self.plane == runtime::Plane::Signal || self.signal.contains(&name) || SIGNAL_EFFECTS.contains(&name) {
            runtime::Plane::Signal
        } else {
            runtime::Plane::Work
        }
    }
}

/// The service's own tasks: the heartbeat and the watchers under `watchers/`.
pub static CORE: &[Task] = &[
    Task { name: heartbeat::NAME, reads: Reads::None, plane: runtime::Plane::Work, signal: &[], boot: Heartbeat::boot, cadence_ms: |s| s.heartbeat_secs * 1000, effects: &[heartbeat::PING], runners: &[(heartbeat::PING, |e, _, s| e.ping(s))], keys: &[&["SPIRA_RUNTIME_HEARTBEAT_URL", "SPIRA_RUNTIME_HEARTBEAT_SECS"]], build: |s| Heartbeat { interval_ms: s.heartbeat_secs * 1000, ping_deadline_ms: s.ping_deadline_ms() }.registration() },
    Task { name: round_duty::NAME, reads: Reads::Polled(&["lifecycle/", "readiness/", "batches/"]), plane: runtime::Plane::Work, signal: &[], boot: round_duty::RoundDuty::boot, cadence_ms: |s| s.round_duty.interval_ms, effects: &[round_duty::OBSERVE, INBOX_APPEND], runners: &[(round_duty::OBSERVE, |e, _, s| e.world.round_snapshot(&s.input))], keys: &[&["SPIRA_RUNTIME_ROUND_DUTY_SECS", "SPIRA_ROUND_MIN", "SPIRA_ROUND_MAX_OPEN", "SPIRA_ROUND_DUE_REMIND", "SPIRA_ROUND_IDLE_CUT", "SPIRA_ROUND_LEASE_TTL_SECS", "SPIRA_HALT_PAGE_AFTER", "SPIRA_ACTIVATION_STORE_BOUND"]], build: |s| s.round_duty.clone().registration() },
    Task { name: inbox_keeper::NAME, reads: Reads::None, plane: runtime::Plane::Work, signal: &[CONCIERGE_WAKE], boot: inbox_keeper::InboxKeeper::boot, cadence_ms: |s| s.inbox_keeper.interval_ms, effects: &[inbox_keeper::OBSERVE, CONCIERGE_WAKE], runners: &[(inbox_keeper::OBSERVE, |e, _, s| e.world.keeper_snapshot(&s.input)), (CONCIERGE_WAKE, |_, _, s| wake::release_wake_for_process(&s.input, std::time::Duration::from_millis(inbox_keeper::WAKE_KILL_MS)))], keys: &[&["SPIRA_RUNTIME_INBOX_KEEPER_SECS", "SPIRA_CONCIERGE_INBOX_STALL", "SPIRA_CONCIERGE_INBOX_BACKOFF"]], build: |s| s.inbox_keeper.clone().registration() },
    Task { name: pr_notify::NAME, reads: Reads::Polled(&["batches/"]), plane: runtime::Plane::Work, signal: &[pr_notify::OPERATOR_NOTE], boot: pr_notify::PrNotify::boot, cadence_ms: |s| s.pr_notify.interval_ms, effects: &[pr_notify::OBSERVE, pr_notify::OPERATOR_NOTE, INBOX_APPEND], runners: &[(pr_notify::OBSERVE, |e, _, s| e.world.pr_snapshot(&e.forge, &s.input)), (pr_notify::OPERATOR_NOTE, |e, _, s| e.operator_note(&s.input))], keys: &[&["SPIRA_RUNTIME_PR_NOTIFY_SECS", "SPIRA_PR_DEPLOY_GIVEUP"]], build: |s| s.pr_notify.clone().registration() },
    Task { name: cr_cadence::NAME, reads: Reads::None, plane: runtime::Plane::Work, signal: &[], boot: cr_cadence::CrCadence::boot, cadence_ms: |s| s.cr_cadence.interval_ms, effects: &[cr_cadence::OBSERVE, cr_cadence::OPEN, INBOX_APPEND], runners: &[(cr_cadence::OBSERVE, |e, _, s| e.world.cr_cadence_snapshot(&s.input)), (cr_cadence::OPEN, |e, _, _| e.world.cr_open())], keys: &[&["SPIRA_RUNTIME_CR_CADENCE_SECS", "SPIRA_CR_TZ", "SPIRA_CR_OPEN_HOUR", "SPIRA_CR_STALE_DAYS"]], build: |s| s.cr_cadence.clone().registration() },
    Task { name: margin_comments::NAME, reads: Reads::None, plane: runtime::Plane::Work, signal: &[], boot: margin_comments::MarginComments::boot, cadence_ms: |s| s.sweep_secs * 1000, effects: &[margin_comments::DELIVER, margin_comments::PUBLISH, INBOX_APPEND], runners: &[(margin_comments::DELIVER, |e, _, s| e.world.margin_deliver(&s.input)), (margin_comments::PUBLISH, |e, _, s| e.world.margin_publish(&s.input))], keys: &[&["SPIRA_RUNTIME_SWEEP_SECS"]], build: |s| margin_comments::MarginComments { sweep_ms: s.sweep_secs * 1000 }.registration() },
    Task { name: asks::NAME, reads: Reads::Polled(&["planning/", "lifecycle/", "history/"]), plane: runtime::Plane::Work, signal: &[], boot: asks::Asks::boot, cadence_ms: |s| s.asks.interval_ms, effects: &[asks::OBSERVE, INBOX_APPEND], runners: &[(asks::OBSERVE, |e, _, s| e.world.asks_snapshot(&s.input))], keys: &[&["SPIRA_RUNTIME_ASKS_SECS"]], build: |s| s.asks.clone().registration() },
    Task { name: design_asks::NAME, reads: Reads::Polled(&["planning/", "lifecycle/"]), plane: runtime::Plane::Work, signal: &[], boot: design_asks::DesignAsks::boot, cadence_ms: |s| s.design_asks.interval_ms, effects: &[design_asks::OBSERVE, INBOX_APPEND], runners: &[(design_asks::OBSERVE, |e, _, _| e.world.design_asks_snapshot())], keys: &[&["SPIRA_RUNTIME_DESIGN_ASKS_SECS", "SPIRA_DESIGN_ASKS_REPEAT"]], build: |s| s.design_asks.clone().registration() },
    Task { name: brain_dirty::NAME, reads: Reads::None, plane: runtime::Plane::Work, signal: &[], boot: brain_dirty::BrainDirty::boot, cadence_ms: |s| s.brain_dirty.interval_ms, effects: &[brain_dirty::OBSERVE, INBOX_APPEND], runners: &[(brain_dirty::OBSERVE, |e, _, _| e.world.brain_dirty_snapshot())], keys: &[&["SPIRA_RUNTIME_BRAIN_DIRTY_SECS", "SPIRA_BRAIN_DIRTY_AGE"]], build: |s| s.brain_dirty.clone().registration() },
    Task { name: writer_grants::NAME, reads: Reads::Undeclarable("the verdict is lc-serve's in-memory writer grants, read with no store call"), plane: runtime::Plane::Work, signal: &[], boot: writer_grants::WriterGrants::boot, cadence_ms: |s| s.writer_grants.interval_ms, effects: &[writer_grants::OBSERVE, INBOX_APPEND], runners: &[(writer_grants::OBSERVE, |e, _, _| e.world.writer_grants_snapshot())], keys: &[&["SPIRA_RUNTIME_WRITER_GRANTS_SECS"]], build: |s| s.writer_grants.clone().registration() },
    Task { name: wake_audit::NAME, reads: Reads::None, plane: runtime::Plane::Work, signal: &[], boot: wake_audit::WakeAudit::boot, cadence_ms: |s| s.sweep_secs * 1000, effects: &[INBOX_APPEND], runners: &[], keys: &[&["SPIRA_RUNTIME_SWEEP_SECS"]], build: |s| wake_audit::WakeAudit { interval_ms: s.sweep_secs * 1000 }.registration() },
    Task { name: supersede_duty::NAME, reads: Reads::Only(&["lifecycle/", "readiness/"]), plane: runtime::Plane::Work, signal: &[], boot: supersede_duty::SupersedeDuty::boot, cadence_ms: |s| s.supersede_duty.interval_ms, effects: &[supersede_duty::OBSERVE, supersede_duty::ACT, INBOX_APPEND], runners: &[(supersede_duty::OBSERVE, |e, _, s| e.world.supersede_snapshot(&s.input)), (supersede_duty::ACT, |e, _, s| e.world.supersede_act(&s.input))], keys: &[&["SPIRA_RUNTIME_SUPERSEDE_DUTY_SECS"]], build: |s| s.supersede_duty.clone().registration() },
    Task { name: claim_count::NAME, reads: Reads::Only(&["lifecycle/", "readiness/"]), plane: runtime::Plane::Work, signal: &[], boot: claim_count::ClaimCount::boot, cadence_ms: |s| s.claim_count.interval_ms, effects: &[claim_count::OBSERVE, INBOX_APPEND], runners: &[(claim_count::OBSERVE, |e, _, _| e.world.claim_count_snapshot())], keys: &[&["SPIRA_RUNTIME_CLAIM_COUNT_SECS", "SPIRA_CLAIM_PAGE_EVERY"]], build: |s| s.claim_count.clone().registration() },
    Task { name: bead_sweep::NAME, reads: Reads::Polled(&["lifecycle/", "readiness/", "planning/"]), plane: runtime::Plane::Work, signal: &[], boot: bead_sweep::BeadSweep::boot, cadence_ms: |s| s.bead_sweep.interval_ms, effects: &[bead_sweep::OBSERVE, bead_sweep::REPAIR, INBOX_APPEND], runners: &[(bead_sweep::OBSERVE, |e, _, _| e.world.sweep_snapshot()), (bead_sweep::REPAIR, |e, _, s| e.world.sweep_repair(&s.input))], keys: &[&["SPIRA_RUNTIME_BEAD_SWEEP_SECS", "SPIRA_BEAD_SWEEP_HOLD_MIN", "SPIRA_BEAD_SWEEP_REWORK_MIN", "SPIRA_BEAD_SWEEP_CERT_MIN", "SPIRA_BEAD_SWEEP_WAIT_MIN", "SPIRA_BEAD_SWEEP_BATCH_MIN", "SPIRA_SCOPED_STOP_REVIEW_SECS"]], build: |s| s.bead_sweep.clone().registration() },
    Task { name: publish_backlog::NAME, reads: Reads::None, plane: runtime::Plane::Work, signal: &[], boot: publish_backlog::PublishBacklog::boot, cadence_ms: |s| s.publish_backlog.interval_ms, effects: &[publish_backlog::OBSERVE, INBOX_APPEND], runners: &[(publish_backlog::OBSERVE, |e, _, _| e.world.publish_backlog_snapshot())], keys: &[&["SPIRA_RUNTIME_PUBLISH_BACKLOG_SECS", "SPIRA_RUNTIME_PUBLISH_BACKLOG_RECHECK_SECS", "SPIRA_LOCAL_BACKLOG_COUNT", "SPIRA_LOCAL_BACKLOG_AGE"]], build: |s| s.publish_backlog.clone().registration() },
    Task { name: overrides::NAME, reads: Reads::None, plane: runtime::Plane::Work, signal: &[], boot: overrides::Overrides::boot, cadence_ms: |s| s.overrides.interval_ms, effects: &[overrides::APPLY, INBOX_APPEND], runners: &[(overrides::APPLY, |e, _, _| e.world.overrides_apply())], keys: &[&["SPIRA_RUNTIME_OVERRIDES_SECS"]], build: |s| s.overrides.clone().registration() },
    Task { name: readiness_dependents::NAME, reads: Reads::Only(&["lifecycle/"]), plane: runtime::Plane::Work, signal: &[], boot: readiness_dependents::ReadinessDependents::boot, cadence_ms: |s| s.readiness_dependents.interval_ms, effects: &[readiness_dependents::LOOK, readiness_dependents::RUN], runners: &[(readiness_dependents::LOOK, |e, _, s| e.world.readiness_look(&s.input)), (readiness_dependents::RUN, |e, _, s| e.world.readiness_dependents(&s.input))], keys: &[&["SPIRA_RUNTIME_READINESS_DEPENDENTS_SECS"]], build: |s| s.readiness_dependents.clone().registration() },
    Task { name: view::NAME, reads: Reads::None, plane: runtime::Plane::Work, signal: &[], boot: view::View::boot, cadence_ms: |s| s.view.interval_ms, effects: &[view::OBSERVE, view::FLIP, view::FORGET], runners: &[(view::OBSERVE, |e, _, s| e.world.view_snapshot(&s.input)), (view::FLIP, |e, _, s| e.world.view_flip(&s.input)), (view::FORGET, |e, _, s| e.world.view_forget(&s.input))], keys: &[&["SPIRA_RUNTIME_VIEW_SECS", "SPIRA_VIEW"]], build: |s| s.view.clone().registration() },
    Task { name: reconciler::NAME, reads: Reads::Polled(&["lifecycle/", "readiness/", "batches/", "planning/", "gates/"]), plane: runtime::Plane::Work, signal: &[], boot: reconciler::Reconciler::boot, cadence_ms: |s| s.reconciler.interval_ms, effects: &[reconciler::OBSERVE, reconciler::RECORD, reconciler::REMEDY, reconciler::MAIL], runners: &[(reconciler::OBSERVE, |e, _, s| e.reconcile_observe(&s.input)), (reconciler::RECORD, |e, _, s| e.reconcile_record(&s.input)), (reconciler::REMEDY, |e, k, s| e.reconcile_remedy(k, &s.input)), (reconciler::MAIL, |e, k, s| e.reconcile_mail(k, &s.input))], keys: &[&["SPIRA_RUNTIME_RECONCILER_SECS", "SPIRA_RUNES", "SPIRA_STRANDED_RUNNER_MIN_AGE", "SPIRA_LC_PASSWORD_FILE"], ::reconciler::Config::KEYS], build: |s| s.reconciler.clone().registration() },
    Task { name: reconciler_flow::NAME, reads: Reads::Polled(&["lifecycle/", "planning/"]), plane: runtime::Plane::Work, signal: &[], boot: reconciler_flow::ReconcilerFlow::boot, cadence_ms: |s| s.reconciler_flow.interval_ms, effects: &[reconciler_flow::OBSERVE, reconciler_flow::STATUS, reconciler_flow::MAIL], runners: &[(reconciler_flow::OBSERVE, |e, _, s| e.world.flow_snapshot(&s.input)), (reconciler_flow::STATUS, |e, _, s| e.world.flow_status(&s.input)), (reconciler_flow::MAIL, |e, _, s| e.concierge_note(&s.input))], keys: &[&["SPIRA_RUNTIME_RECONCILER_FLOW_SECS", "SPIRA_FLOW_WINDOW_HOURS", "SPIRA_FLOW_BASELINE_HOURS", "SPIRA_FLOW_GRACE_SECS", "SPIRA_FLOW_UNOBSERVABLE_GRACE_SECS", "SPIRA_FLOW_REWORK_WINDOW_HOURS", "SPIRA_FLOW_SENTINEL_PERIOD_SECS", "SPIRA_RECONCILER_SHADOW_KINDS"]], build: |s| s.reconciler_flow.clone().registration() },
];

/// Every family's slice, in boot order. A new family file under `tasks/` joins here.
pub static FAMILIES: &[&[Task]] = &[CORE, a::TASKS, b::TASKS, c::TASKS, d::TASKS, e::TASKS, f::TASKS, g::TASKS];

/// Every family's rows not yet ported, in the same family order. A new family file joins here too.
///
/// Pending rows that declare a key in [`Settings::UNDECLARABLE`], which `from_lookup` refuses
/// once the row is in `TASKS`. The port renames or drops each key before its row moves. The
/// block below is `tests::undeclarable_table()` over the current rows, and
/// `the_undeclarable_table_in_the_docs_is_the_rows` fails when it is stale:
///
/// No pending row declares one today.
///
/// Dropping the last row's use of a key also drops it from `UNDECLARABLE`, or
/// `every_key_a_pending_row_declares_is_registered_and_the_fixture_resolves_it` calls it stale.
pub static PENDING_FAMILIES: &[&[Task]] = &[a::PENDING, b::PENDING, c::PENDING, d::PENDING, e::PENDING, f::PENDING, g::PENDING];

/// The prefixes every `Only` and `Polled` row declares, each once: what the store's projection must keep current.
pub fn watched_prefixes() -> Vec<&'static str> {
    let mut all: Vec<&'static str> = self::all().flat_map(|t| t.reads.prefixes().iter().copied()).collect();
    all.sort_unstable();
    all.dedup();
    all
}

/// Every task in every family, in `FAMILIES` order.
pub fn all() -> impl Iterator<Item = &'static Task> {
    FAMILIES.iter().flat_map(|f| f.iter())
}

/// Every row written but not ported, in `PENDING_FAMILIES` order. None of them is booted.
pub fn pending() -> impl Iterator<Item = &'static Task> {
    PENDING_FAMILIES.iter().flat_map(|f| f.iter())
}

/// The page for task `name` among [`CONTRACT_PAGES`]: `.` and `_` become `-`, so
/// `intake.kit_bump` is `intake-kit-bump` (the file `intake-kit-bump.md`). Task names are
/// otherwise already page names.
pub fn contract_page(name: &str) -> String {
    name.replace(['.', '_'], "-")
}

/// Every task contract page, by basename without `.md`: the listing of the brain's
/// `wiki/projects/spira/designs/tasks/` at brain commit f0573d98, less its hub
/// (`task-contracts`) and `retired`. Copied here because tests run in the test sandbox, which
/// hides the home the brain lives in. A contract added there is added here in the commit that
/// stubs its task. The three `source-*` pages are the sources in `tasks::g`, not rows.
pub const CONTRACT_PAGES: &[&str] = &["asks-ideas", "asks-moot", "asks-verify", "batcher-forge", "batcher-local", "cockpit-tier-5s", "cockpit-tier-600s", "cockpit-tier-60s", "czar", "gate-check", "gate-worker", "hel-reap-stale", "hel-reap-terminal", "hel-sweep", "inbox-notify", "incident-alarms", "incident-probe", "incident-settle", "intake-filings", "intake-gh", "intake-kit-bump", "landing", "lc-ship", "mail-reconcile", "mail-tidy", "persona-archivist", "persona-ops", "publish", "queue-step", "reconcile-content", "reconcile-goodput", "reconcile-heimdall", "reconcile-section", "roundvm-ami-prune", "roundvm-drain", "sentinel-call", "sentinel-pass", "sift", "source-mail-arrived", "source-submission", "source-unit-ran", "submission-provision", "sweep-archive", "sweep-escape-census", "sweep-export-beads", "sweep-reclaim", "sweep-target", "sweep-tsd", "trigger-groom", "trigger-mimir", "trigger-warden", "watch-ami-stale", "watch-backup-age", "watch-ci-spill", "watch-mend", "watch-off-box", "watch-pass-rate", "watch-perf", "watch-refusals", "watch-runtime", "watch-skew", "watch-sop-lint", "watch-testenv-cap", "watch-tower"];

/// Each task name and each stream claimed by more than one row across `families`.
pub fn clashes(families: &[&[Task]]) -> Vec<String> {
    let mut names = std::collections::BTreeMap::<&str, usize>::new();
    let mut streams = std::collections::BTreeMap::<&str, usize>::new();
    for t in families.iter().flat_map(|f| f.iter()) {
        *names.entry(t.name).or_default() += 1;
        *streams.entry(t.stream()).or_default() += 1;
    }
    let twice = |kind: &str, m: std::collections::BTreeMap<&str, usize>| m.into_iter().filter(|(_, n)| *n > 1).map(|(k, n)| format!("{kind} {k} x{n}")).collect::<Vec<_>>();
    let mut out = twice("name", names);
    out.extend(twice("stream", streams));
    out
}

/// Effects more than one task emits: the inbox line, and the unit manager's four verbs
/// (`runtime::units::EFFECTS`), each run on `Effects::units`.
pub static SHARED: &[(&str, Runner)] = &[
    (INBOX_APPEND, |e, _, s| e.world.inbox_append(&s.input)),
    (units::UNIT_START, |e, _, s| e.unit(s)),
    (units::UNIT_STOP, |e, _, s| e.unit(s)),
    (units::UNIT_RESTART, |e, _, s| e.unit(s)),
    (units::UNIT_FACTS, |e, _, s| e.unit(s)),
];

/// Effects that run through a halt whichever task asks for them: the inbox line is how a
/// stopped world still reaches the operator.
pub const SIGNAL_EFFECTS: &[&str] = &[INBOX_APPEND];

/// Every effect on the signal plane, with what its runner touches. Reviewed by hand against
/// the runner: each pages, sends mail, wakes a reader or appends to the inbox (or, for a
/// `Signal` task, reads), and none writes the lifecycle store, starts a unit or touches git.
/// A signal effect missing here is one [`signal_violations`] reports.
pub const SIGNAL_SAFE: &[(&str, &str)] = &[
    (INBOX_APPEND, "appends a line to the Concierge's inbox file (World::inbox_append)"),
    (g::mail_wake::OBSERVE, "reads each mailbox's unread age (mail unread-age, ls)"),
    (g::mail_wake::WAKE, "runs the mailbox reader's own wake command through the process port"),
    (e::incident_probe::OBSERVE, "reads the newest probe attempt under the run dir"),
    (e::incident_probe::SEND, "mails the probe through the mail store (bdq); refuses any other mailbox"),
    (b::incident_alarms::PAGE, "pages: mail send of an operator ask, the maildir fallback when it fails"),
    (b::incident_alarms::FLOOR_ASK, "pages the floor ask through the same delivery"),
    (b::incident_alarms::NOTE, "adds a note to the ask an earlier page opened (mail)"),
    (b::incident_alarms::RETRY, "re-sends one undelivered page through the same delivery"),
    (e::inbox_notify::ASK, "mails the operator a question (watchd send)"),
    (e::inbox_notify::CONCIERGE_NOTE, "mails the Concierge a note (watchd send)"),
    (pr_notify::OPERATOR_NOTE, "mails the operator a note (mail send)"),
    (CONCIERGE_WAKE, "runs the Concierge's wake verb, which wakes its reader"),
    (e::watch_backup_age::MAIL, "mails an alert (alert::send)"),
    (e::watch_pass_rate::MAIL, "mails an alert (alert::send)"),
    (f::asks_ideas::ASK, "mails the operator the idea prompt (idea_prompt send)"),
];

/// Every signal-plane effect of `rows` that is a unit verb or is not reviewed in
/// [`SIGNAL_SAFE`]: a halt would let it do the world's work.
pub fn signal_violations<'a>(rows: impl Iterator<Item = &'a Task>) -> Vec<String> {
    let safe: Vec<&str> = SIGNAL_SAFE.iter().map(|(n, _)| *n).collect();
    let mut out = vec![];
    for t in rows {
        for e in t.effects.iter().chain(t.signal).filter(|e| t.plane_of(e) == runtime::Plane::Signal) {
            if units::EFFECTS.contains(e) {
                out.push(format!("{}: {e} is a unit verb on the signal plane", t.name));
            } else if !safe.contains(e) {
                out.push(format!("{}: {e} is on the signal plane but SIGNAL_SAFE does not clear it", t.name));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Every runner by effect name. A name owned twice keeps the first; `owners` shows the clash.
pub fn routes() -> std::collections::BTreeMap<&'static str, Runner> {
    let mut out = std::collections::BTreeMap::new();
    for (name, r) in all().flat_map(|t| t.runners.iter()).chain(SHARED) {
        out.entry(*name).or_insert(*r);
    }
    out
}

/// Each effect name with how many places supply its runner.
pub fn owners() -> std::collections::BTreeMap<&'static str, usize> {
    let mut out = std::collections::BTreeMap::new();
    for (name, _) in all().flat_map(|t| t.runners.iter()).chain(SHARED) {
        *out.entry(*name).or_default() += 1;
    }
    out
}

/// The boot ticks of every task, in `all()` order.
pub fn boots() -> Vec<(&'static str, &'static str, EventKind)> {
    all().map(|t| (t.boot)()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::taskrig::TaskRig;

    /// Every `pub const NAME` declared by a module under `watchers/`, read from the source so the
    /// list does not come from the table it checks.
    fn watcher_names() -> Vec<String> {
        let dir = testkit::manifest_dir().join("src/watchers");
        let mut out = vec![];
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let src = std::fs::read_to_string(e.path()).unwrap();
            for line in src.lines() {
                if let Some(rest) = line.strip_prefix("pub const NAME: &str = \"") {
                    out.push(rest.trim_end_matches("\";").to_string());
                }
            }
        }
        out.sort();
        out
    }

    #[test_id(txbybk9)]
    #[test]
    fn the_cert_sweeps_are_retired() {
        for gone in ["cert.full", "cert.sample"] {
            assert!(!all().chain(pending()).any(|t| t.name == gone), "{gone} is still a task");
            let table = crate::modes::parse(&format!("{gone}=observe")).unwrap();
            let err = crate::modes::apply(vec![], &table).err().unwrap();
            assert!(err.contains(gone), "{err}");
        }
    }

    #[test_id(tv7fpqa)]
    #[test]
    fn every_watcher_module_and_the_heartbeat_have_a_row() {
        let names = watcher_names();
        assert!(names.len() >= 14 && names.contains(&"asks".to_string()), "the source scan must find the watchers: {names:?}");
        let rows: Vec<&str> = all().map(|t| t.name).collect();
        let missing: Vec<&String> = names.iter().filter(|n| !rows.contains(&n.as_str())).collect();
        assert!(missing.is_empty(), "watchers with no task row: {missing:?}");
        assert!(rows.contains(&heartbeat::NAME));
        let mut sorted = rows.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), rows.len(), "a task name appears twice");
    }

    /// Every file under `tasks/`, by stem, that `tasks.rs` does not declare as a module and list
    /// in both `FAMILIES` (its `TASKS`) and `PENDING_FAMILIES` (its `PENDING`). `src` is the text
    /// of `tasks.rs`.
    fn unreachable_families(src: &str, stems: &[String]) -> Vec<String> {
        let list = |name: &str| src.split(&format!("pub static {name}: ")).nth(1).and_then(|r| r.split(';').next()).unwrap_or("").to_string();
        let (families, pending) = (list("FAMILIES"), list("PENDING_FAMILIES"));
        stems.iter().filter(|s| !src.contains(&format!("pub mod {s};")) || !families.contains(&format!("{s}::TASKS")) || !pending.contains(&format!("{s}::PENDING"))).cloned().collect()
    }

    fn family_stems() -> Vec<String> {
        let dir = testkit::manifest_dir().join("src/tasks");
        // Files only: each family also has a `tasks/<x>/` directory of task modules beside `<x>.rs`.
        let mut out: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "rs")).filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned())).collect();
        out.sort();
        out
    }

    #[test_id(tg6izw7)]
    #[test]
    fn every_family_file_is_in_the_families_list() {
        let src = std::fs::read_to_string(testkit::manifest_dir().join("src/tasks.rs")).unwrap();
        let stems = family_stems();
        assert_eq!(stems, ["a", "b", "c", "d", "e", "f", "g"], "the scan must find the family files");
        let planted = [stems.clone(), vec!["planted".to_string()]].concat();
        assert_eq!(unreachable_families(&src, &planted), ["planted"], "a family file left out of FAMILIES must be caught");
        assert_eq!(unreachable_families(&src, &stems), Vec::<String>::new());
        assert_eq!(FAMILIES.len(), 1 + stems.len(), "CORE plus one slice per family file");
        assert_eq!(PENDING_FAMILIES.len(), stems.len(), "one pending slice per family file");
        assert_eq!(all().count(), FAMILIES.iter().map(|f| f.len()).sum::<usize>());
        assert_eq!(pending().count(), PENDING_FAMILIES.iter().map(|f| f.len()).sum::<usize>());
    }

    #[test_id(tm29r2l)]
    #[test]
    fn a_task_name_maps_to_its_contract_page() {
        assert_eq!(contract_page("hel.reap_terminal"), "hel-reap-terminal");
        assert_eq!(contract_page("intake.kit_bump"), "intake-kit-bump");
        assert_eq!(contract_page("sentinel-call"), "sentinel-call");
        assert_eq!(contract_page("cockpit-tier-5s"), "cockpit-tier-5s");
    }

    /// What breaks "pending ∪ ported = the contract pages, disjoint": a pending row with no page,
    /// a page (less the `source-*` pages) with no row, a row both pending and ported. `ported`
    /// is every booted row, so the service's own rows, which have no page, are in it harmlessly.
    fn contract_cover(pending: &[&str], ported: &[&str]) -> Vec<String> {
        let mut out = vec![];
        out.extend(pending.iter().filter(|n| !CONTRACT_PAGES.contains(&contract_page(n).as_str())).map(|n| format!("pending {n} has no contract page")));
        let rows: Vec<String> = pending.iter().chain(ported).map(|n| contract_page(n)).collect();
        out.extend(CONTRACT_PAGES.iter().filter(|p| !p.starts_with("source-") && !rows.contains(&p.to_string())).map(|p| format!("page {p} has no row")));
        out.extend(pending.iter().filter(|n| ported.contains(n)).map(|n| format!("{n} is both pending and ported")));
        out
    }

    #[test_id(t4zyg9i)]
    #[test]
    fn every_pending_row_has_a_contract_page_and_no_row_is_both_pending_and_ported() {
        let rows: Vec<&str> = CONTRACT_PAGES.iter().copied().filter(|p| !p.starts_with("source-")).collect();
        assert_eq!(contract_cover(&[], &rows), Vec::<String>::new(), "every page ported and none pending is the end state, not a fault");
        assert_eq!(contract_cover(&rows, &[]), Vec::<String>::new(), "every page pending is the start state");
        assert_eq!(contract_cover(&["planted.no_such_task"], &rows), ["pending planted.no_such_task has no contract page"], "a name with no page must be caught");
        assert_eq!(contract_cover(&rows[1..], &[]), [format!("page {} has no row", rows[0])], "a page with no row must be caught");
        assert_eq!(contract_cover(&rows, &rows[..1]), [format!("{} is both pending and ported", rows[0])], "a row in both must be caught");
        let names: Vec<&str> = pending().map(|t| t.name).collect();
        let ported: Vec<&str> = all().map(|t| t.name).collect();
        assert_eq!(contract_cover(&names, &ported), Vec::<String>::new());
        let mut every: Vec<&[Task]> = FAMILIES.to_vec();
        every.extend(PENDING_FAMILIES);
        assert_eq!(clashes(&every), Vec::<String>::new(), "a pending row shares a name or a stream");
    }

    /// The doc block above `PENDING_FAMILIES`, built from the rows: each key of
    /// `Settings::UNDECLARABLE` with the pending rows whose `keys` declare it, by family letter.
    fn undeclarable_table() -> String {
        let rows: Vec<String> = Settings::UNDECLARABLE
            .iter()
            .map(|k| {
                let by: Vec<String> = PENDING_FAMILIES.iter().zip('a'..).flat_map(|(f, letter)| f.iter().filter(|t| t.keys.iter().any(|ks| ks.contains(k))).map(move |t| format!("`{}` ({letter})", t.name))).collect();
                format!("/// | `{k}` | {} |\n", by.join(", "))
            })
            .collect();
        if rows.is_empty() {
            return "/// No pending row declares one today.\n".into();
        }
        format!("/// | Key | Declared in `KEYS` by |\n/// |---|---|\n{}", rows.concat())
    }

    #[test_id(tit65c2)]
    #[test]
    fn the_undeclarable_table_in_the_docs_is_the_rows() {
        let src = std::fs::read_to_string(testkit::manifest_dir().join("src/tasks.rs")).unwrap();
        let doc = src.split("pub static FAMILIES:").nth(1).and_then(|r| r.split("pub static PENDING_FAMILIES:").next()).unwrap_or("");
        let want = undeclarable_table();
        assert!(doc.contains(&want), "the table above PENDING_FAMILIES is stale; it is:\n{want}");
    }

    #[test_id(tromiq7)]
    #[test]
    fn no_two_tasks_across_families_share_a_name_or_a_stream() {
        static PLANTED: &[Task] = &[Task { name: heartbeat::NAME, reads: Reads::None, plane: runtime::Plane::Work, signal: &[], boot: Heartbeat::boot, cadence_ms: |_| 1, effects: &[], runners: &[], keys: &[], build: |s| (CORE[0].build)(s) }];
        let caught = clashes(&[CORE, PLANTED]);
        assert!(caught.iter().any(|c| c.starts_with("name ")) && caught.iter().any(|c| c.starts_with("stream ")), "a row planted in a second family must be caught: {caught:?}");
        assert_eq!(clashes(FAMILIES), Vec::<String>::new());
    }

    #[test_id(tgh8oe2)]
    #[test]
    fn the_registrations_and_the_boot_list_are_the_table() {
        let s = crate::settings::fixture();
        let regs: Vec<String> = crate::registrations(&s).into_iter().map(|r| r.name).collect();
        assert_eq!(regs, all().map(|t| t.name.to_string()).collect::<Vec<_>>());
        assert_eq!(boots().iter().map(|b| b.1).collect::<Vec<_>>(), all().map(|t| t.stream()).collect::<Vec<_>>());
    }

    #[test_id(tx9i6kc)]
    #[test]
    fn every_declared_effect_has_one_runner_and_every_runner_is_declared() {
        let routes = routes();
        let declared: Vec<&str> = all().flat_map(|t| t.effects.iter().copied()).collect();
        let missing: Vec<String> = all().flat_map(|t| t.effects.iter().filter(|n| !routes.contains_key(*n)).map(move |n| format!("{}: {n}", t.name))).collect();
        assert!(missing.is_empty(), "declared effects with no runner: {missing:?}");
        let twice: Vec<_> = owners().into_iter().filter(|(_, n)| *n > 1).collect();
        assert!(twice.is_empty(), "effects with more than one runner: {twice:?}");
        let orphan: Vec<&&str> = routes.keys().filter(|n| !declared.contains(n) && !units::EFFECTS.contains(n)).collect();
        assert!(orphan.is_empty(), "runners no task declares: {orphan:?}");
    }

    #[test_id(t79oem1)]
    #[test]
    fn every_pending_row_declares_each_effect_once_with_one_runner_for_each() {
        let shared: Vec<&str> = SHARED.iter().map(|(n, _)| *n).collect();
        for t in pending() {
            let mut names = t.effects.to_vec();
            names.sort();
            names.dedup();
            assert_eq!(names.len(), t.effects.len(), "{}: an effect is declared twice", t.name);
            for e in t.effects.iter().filter(|e| !shared.contains(e)) {
                assert_eq!(t.runners.iter().filter(|(n, _)| n == e).count(), 1, "{}: {e} needs exactly one runner on its row", t.name);
            }
            let stray: Vec<&str> = t.runners.iter().map(|(n, _)| *n).filter(|n| !t.effects.contains(n) || shared.contains(n)).collect();
            assert!(stray.is_empty(), "{}: runners for undeclared or shared effects: {stray:?}", t.name);
        }
    }

    /// `rows` as the Rust literal `sim::doubles::TASK_EFFECTS` holds, so a mismatch prints the fix.
    fn task_effects_literal(rows: &[(&str, Vec<&str>)]) -> String {
        rows.iter().map(|(n, es)| format!("    ({n:?}, &[{}]),\n", es.iter().map(|e| format!("{e:?}")).collect::<Vec<_>>().join(", "))).collect()
    }

    /// Whether `have` names the same rows with the same effects as `want`.
    fn same_task_effects(have: &[(&str, Vec<&str>)], want: &[(&str, Vec<&str>)]) -> bool {
        fn set(rows: &[(&str, Vec<&str>)]) -> std::collections::BTreeSet<(String, Vec<String>)> {
            rows.iter().map(|(n, es)| (n.to_string(), es.iter().map(|e| e.to_string()).collect())).collect()
        }
        have.len() == want.len() && set(have) == set(want)
    }

    #[test_id(t18pz6m)]
    #[test]
    fn the_sim_names_a_double_for_every_effect_of_every_row_ported_or_pending() {
        let want: Vec<(&str, Vec<&str>)> = all().chain(pending()).map(|t| (t.name, t.effects.to_vec())).collect();
        let have: Vec<(&str, Vec<&str>)> = sim::doubles::TASK_EFFECTS.iter().map(|(n, es)| (*n, es.to_vec())).collect();
        let mut moved = want.clone();
        moved.rotate_left(1);
        assert!(same_task_effects(&moved, &want), "a row moved from PENDING to TASKS changes the table's order, not its rows");
        let mut lost = want.clone();
        lost.pop();
        assert!(!same_task_effects(&lost, &want), "a row missing from TASK_EFFECTS must be caught");
        assert!(same_task_effects(&have, &want), "sim::doubles::TASK_EFFECTS must name every row's effects, in any order:\n{}", task_effects_literal(&want));
        assert!(sim::doubles::Doubles::for_tasks(&["planted.no_such_task"]).is_err(), "a task the sim does not know is refused");
        let names: Vec<&str> = all().chain(pending()).map(|t| t.name).collect();
        let doubles = sim::doubles::Doubles::for_tasks(&names).unwrap().names();
        let missing: Vec<&str> = all().chain(pending()).flat_map(|t| t.effects.iter().copied()).filter(|e| !doubles.iter().any(|d| d == e)).collect();
        assert!(missing.is_empty(), "declared effects with no sim double: {missing:?}");
        let shared: Vec<&str> = SHARED.iter().map(|(n, _)| *n).collect();
        assert!(shared.iter().all(|e| sim::doubles::service().names().iter().any(|d| d == e)), "every shared effect has a double");
    }

    #[test_id(t12urlu)]
    #[test]
    fn the_unit_effects_are_shared_and_run_on_the_configured_unit_manager() {
        use runtime::host::EffectRunner;
        use std::sync::Arc;
        for e in units::EFFECTS {
            assert_eq!(SHARED.iter().filter(|(n, _)| *n == e).count(), 1, "{e} is one shared runner");
            assert!(pending().all(|t| t.runners.iter().all(|(n, _)| *n != e)), "{e} has a runner on a row as well");
        }
        let start = |input: &str| runtime::EffectSpec::keyed(units::UNIT_START, units::UNIT_START, input, 1);
        let spec = serde_json::to_string(&units::UnitSpec { task: "watch.skew".into(), key: "k1".into(), argv: vec!["true".into()], env: vec![], cwd: None }).unwrap();
        assert_eq!(crate::Effects::default().run("k", &start(&spec)), Err("no unit manager configured".into()), "no backend is refused, not guessed");
        let scripted = Arc::new(units::ScriptedUnits::new(Arc::new(runtime::ports::VirtualClock::new(0))));
        scripted.script("watch.skew", 1_000, units::Exit::Code(0));
        let fx = crate::Effects { units: Some(scripted.clone()), ..crate::Effects::default() };
        let handle: units::UnitHandle = serde_json::from_str(&fx.run("k", &start(&spec)).unwrap()).unwrap();
        assert_eq!(handle, units::UnitHandle::of("watch.skew", "k1").unwrap());
        let facts = runtime::EffectSpec::keyed(units::UNIT_FACTS, units::UNIT_FACTS, serde_json::to_string(&handle).unwrap(), 1);
        let facts: units::UnitFacts = serde_json::from_str(&fx.run("k", &facts).unwrap()).unwrap();
        assert_eq!(facts.state, units::UnitState::Active);
        let stop = runtime::EffectSpec::keyed(units::UNIT_STOP, units::UNIT_STOP, serde_json::to_string(&handle).unwrap(), 1);
        assert_eq!(fx.run("k", &stop), Ok(String::new()));
        assert!(scripted.calls().iter().any(|c| c.starts_with("start ")) && scripted.calls().iter().any(|c| c.starts_with("stop ")), "{:?}", scripted.calls());
    }

    #[test_id(t9qkqh0)]
    #[test]
    fn an_unknown_effect_name_is_refused_by_the_router() {
        use runtime::host::EffectRunner;
        let spec = runtime::EffectSpec::keyed(runtime::taskrig::PLANTED_UNKNOWN, runtime::taskrig::PLANTED_UNKNOWN, String::new(), 1);
        assert_eq!(crate::Effects::default().run("k", &spec), Err(format!("no runner for effect {}", runtime::taskrig::PLANTED_UNKNOWN)));
        let ping = runtime::EffectSpec::keyed(heartbeat::PING, heartbeat::PING, String::new(), 1);
        assert_eq!(crate::Effects::default().run("k", &ping), Ok("no heartbeat url configured".into()), "a declared name reaches its runner");
    }

    /// A halt lets signal-plane effects run, so none may do the world's work: every one, of
    /// every row ported or pending, is cleared in `SIGNAL_SAFE` and is no unit verb.
    #[test_id(tvjnjad)]
    #[test]
    fn no_signal_task_does_work() {
        for (name, _) in SIGNAL_SAFE {
            assert!(!units::EFFECTS.contains(name), "{name} is a unit verb; it cannot be safe through a halt");
        }
        assert_eq!(signal_violations(all().chain(pending())), Vec::<String>::new());
        let signal: usize = all().map(|t| t.effects.iter().filter(|e| t.plane_of(e) == runtime::Plane::Signal).count()).sum();
        assert!(signal > 0, "the signal effects are on the signal plane");
        for t in all() {
            for e in t.signal {
                assert!(t.effects.contains(e), "{}: signal effect {e} is not one of the row's effects", t.name);
            }
        }
    }

    /// The rule is per effect: a row on `Work` with a unit start or an unreviewed effect on the
    /// signal plane is reported, and a `Signal` row is shorthand for every effect signal.
    #[test_id(ttpjygh)]
    #[test]
    fn a_signal_effect_that_starts_a_unit_or_is_unreviewed_is_reported() {
        let base = all().find(|t| t.name == e::inbox_notify::NAME).expect("inbox.notify is a row");
        let starts = Task { signal: &[units::UNIT_START, e::inbox_notify::ASK], ..*base };
        assert_eq!(signal_violations([&starts].into_iter()), vec![format!("{}: {} is a unit verb on the signal plane", base.name, units::UNIT_START)]);
        let whole = Task { plane: runtime::Plane::Signal, signal: &[], ..*base };
        let found = signal_violations([&whole].into_iter());
        assert!(found.iter().any(|v| v.contains(units::UNIT_START)) && found.iter().any(|v| v.contains(e::inbox_notify::SOP_MATCH)), "a Signal row puts every effect on the signal plane: {found:?}");
        assert_eq!(base.plane_of(e::inbox_notify::ASK), runtime::Plane::Signal);
        assert_eq!(base.plane_of(units::UNIT_START), runtime::Plane::Work, "inbox.notify's unit starts stay on Work");
    }

    #[test_id(tvi4d89)]
    #[test]
    fn every_row_boots_arms_its_cadence_and_asks_only_for_declared_effects() {
        let s = crate::settings::fixture();
        let mut jittered = Vec::new();
        for t in all() {
            let mut rig = TaskRig::new(1, (t.build)(&s)).unwrap();
            let (_, stream, kind) = (t.boot)();
            rig.tick(stream, kind).unwrap();
            let next = rig.engine().next_timer_at();
            let first = (t.cadence_ms)(&s);
            let jitter_ms = t.keys.iter().flat_map(|ks| ks.iter()).filter(|k| k.ends_with("_JITTER_SECS")).filter_map(|k| s.task_config.get(*k)).filter_map(|v| v.trim().parse::<u64>().ok()).max().unwrap_or(0) * 1000;
            let armed = next.unwrap_or_else(|| panic!("{}: the boot tick armed no next tick", t.name));
            assert!((first..first + jitter_ms.max(1)).contains(&armed), "{}: the boot tick must arm the next one in [{first}, {first} + {jitter_ms} ms), got {armed}", t.name);
            if armed != first {
                jittered.push(t.name);
            }
            for q in rig.requested() {
                assert!(t.effects.contains(&q.name.as_str()), "{} asked for {} which its row does not declare", t.name, q.name);
            }
        }
        for name in ["hel.sweep", "hel.reap_terminal", "roundvm.ami_prune"] {
            assert!(jittered.contains(&name), "{name}: a task declaring a jitter key must fire off its slot at a non-zero jitter; jittered: {jittered:?}");
        }
    }

    /// A task whose effect times out asks the world before it asks again: a retry on timeout
    /// that never observed repeats an act whose first run may have landed.
    #[test_id(tf5e4lv)]
    #[test]
    fn no_task_retries_an_act_on_timeout_without_observing_first() {
        use runtime::taskrig::retries_without_observing;
        use runtime::{Command, Ctx, EffectSpec, Event, EventKind, EventKindTag, Handler, Projection, Registration, EFFECT_TIMED_OUT};
        struct Retrier(std::sync::atomic::AtomicUsize);
        impl Handler for Retrier {
            fn interests(&self) -> &[EventKindTag] {
                &["planted.go", EFFECT_TIMED_OUT]
            }
            fn handle(&self, _: &Ctx, _: &Projection, _: &Event) -> Vec<Command> {
                let asked = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if asked >= 2 {
                    return vec![];
                }
                vec![Command::Effect(EffectSpec::keyed("planted.act", "planted.act", "x", 1_000))]
            }
        }
        let mut planted = TaskRig::new(1, Registration::new("planted", Retrier(Default::default()))).unwrap().timing_out();
        planted.tick("planted", EventKind::new("planted.go", "")).unwrap();
        planted.advance(5_000).unwrap();
        assert!(!retries_without_observing(planted.requested()).is_empty(), "the check must see a retry that never looked: {:?}", planted.requested());

        let s = crate::settings::fixture();
        let mut acts = 0;
        for t in all() {
            let mut rig = TaskRig::new(1, (t.build)(&s)).unwrap().timing_out();
            let (_, stream, kind) = (t.boot)();
            rig.tick(stream, kind).unwrap();
            rig.advance(3 * (t.cadence_ms)(&s)).unwrap();
            assert_eq!(retries_without_observing(rig.requested()), Vec::<String>::new(), "{} retried an act after it timed out, without observing first", t.name);
            acts += rig.requested().iter().filter(|r| r.journaled).count();
        }
        assert!(acts > 0, "no row asked for an act under timeout, so the check above proves nothing");
    }

    #[test_id(tfrxphk)]
    #[test]
    fn every_declared_prefix_names_a_whole_store_directory_the_projection_may_hold() {
        let mut declared = 0;
        for t in all() {
            for p in t.reads.prefixes() {
                declared += 1;
                assert!(p.ends_with('/') && p.len() > 1, "{}: {p:?} is not a directory prefix", t.name);
                assert!(!p.starts_with("runtime/"), "{}: {p:?} is the scheduler's own key space", t.name);
                assert_ne!(*p, "telemetry/", "{}: the whole of telemetry/ changes every second", t.name);
                assert_ne!(*p, state_store::EVENTS_PREFIX, "{}: {p:?} moves on every audited write, the scheduler's own journal turns included; declare the directory the row decides from", t.name);
            }
            if let Reads::Undeclarable(why) = t.reads {
                assert!(!why.trim().is_empty(), "{} gives no reason", t.name);
            }
            if let Reads::Only(p) | Reads::Polled(p) = t.reads {
                assert!(!p.is_empty(), "{} declares reads and names none", t.name);
            }
        }
        assert!(declared > 0);
    }

    #[test_id(t8ickjl)]
    #[test]
    fn the_watched_prefixes_are_the_union_of_every_declared_one() {
        let watched = watched_prefixes();
        let mut want: Vec<&str> = all().flat_map(|t| t.reads.prefixes().iter().copied()).collect();
        want.sort_unstable();
        want.dedup();
        assert_eq!(watched, want);
        assert!(watched.contains(&"work-requests/") && watched.contains(&"lifecycle/"));
        assert!(all().any(|t| matches!(t.reads, Reads::Undeclarable(_))), "the table has no row that cannot declare");
    }

    #[test_id(tgw04ey)]
    #[test]
    fn a_row_that_reads_only_the_store_stops_polling_and_one_that_reads_more_keeps_its_cadence() {
        let s = crate::settings::fixture();
        let regs = crate::registrations(&s);
        let wake = |name: &str| regs.iter().find(|r| r.name == name).unwrap().wake.clone();
        let cadence = |name: &str| (all().find(|t| t.name == name).unwrap().cadence_ms)(&s);
        let only = wake(claim_count::NAME).expect("claim-count declares its reads");
        assert_eq!((only.polls, only.min_gap_ms), (false, cadence(claim_count::NAME)));
        let polled = wake(asks::NAME).expect("asks declares its reads");
        assert_eq!((polled.polls, polled.min_gap_ms), (true, cadence(asks::NAME)));
        assert_eq!(wake(writer_grants::NAME), None);
        assert_eq!(wake(heartbeat::NAME), None);
    }

    #[test_id(trbl2p7)]
    #[test]
    fn the_rows_that_decide_from_a_bead_transition_wake_on_its_lifecycle_key_and_not_on_the_audit_or_the_journal() {
        let s = crate::settings::fixture();
        let regs = crate::registrations(&s);
        let (audit, journal) = (state_store::events_key(7), "runtime/state/t/k");
        for name in [readiness_dependents::NAME, reconciler_flow::NAME, b::reconcile_content::NAME] {
            let wake = regs.iter().find(|r| r.name == name).unwrap().wake.clone().unwrap_or_else(|| panic!("{name} declares its reads"));
            let names = |key: &str| wake.reads.iter().any(|p| key.starts_with(p));
            assert!(names("lifecycle/sp-x"), "{name} must wake on a bead transition");
            assert!(!names(&audit) && !names(journal), "{name} wakes on a write that is no bead transition");
        }
    }

    /// Counts what reaches the journal: turns committed and poison reads.
    struct Counting {
        inner: std::sync::Arc<runtime::MemJournal>,
        commits: std::sync::atomic::AtomicU64,
        poison_reads: std::sync::atomic::AtomicU64,
    }

    impl runtime::Journal for Counting {
        fn record(&self, ev: &runtime::Event) {
            self.inner.record(ev)
        }
        fn boot(&self) -> runtime::Result<runtime::Boot> {
            self.inner.boot()
        }
        fn commit(&self, turn: &runtime::Turn) -> runtime::Result<()> {
            self.commits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.commit(turn)
        }
        fn poison(&self, p: &runtime::Poison) -> runtime::Result<()> {
            self.inner.poison(p)
        }
        fn poisoned(&self, stream: &runtime::StreamKey) -> runtime::Result<Option<runtime::Poison>> {
            self.poison_reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.poisoned(stream)
        }
    }

    fn answer_every_effect(e: &mut runtime::Engine, asked: &mut u64) {
        use runtime::Executor;
        for p in e.pending_effects() {
            *asked += 1;
            let out = if p.spec.name == readiness_dependents::LOOK { runtime::Outcome::Succeeded(r#"{"through":1,"keys":[]}"#.into()) } else { runtime::Outcome::Failed("scripted".into()) };
            e.complete(&p.key, out).unwrap();
        }
        e.run_until_idle().unwrap();
    }

    #[test_id(t4l3har)]
    #[test]
    fn an_idle_hour_of_the_four_store_only_rows_runs_nothing_commits_nothing_and_reads_no_poison() {
        use runtime::ports::Clock;
        use runtime::Executor;
        let s = crate::settings::fixture();
        let only = [d::broker_exec::NAME, readiness_dependents::NAME, claim_count::NAME, supersede_duty::NAME];
        let regs: Vec<Registration> = crate::registrations(&s).into_iter().filter(|r| only.contains(&r.name.as_str())).collect();
        assert_eq!(regs.len(), 4);
        let clock = runtime::ports::VirtualClock::new(0);
        let changes = std::sync::Arc::new(runtime::changes::ScriptedChanges::default());
        let journal = std::sync::Arc::new(Counting { inner: runtime::MemJournal::new(), commits: Default::default(), poison_reads: Default::default() });
        let mut e = runtime::Engine::open(journal.clone(), std::sync::Arc::new(clock.clone()), regs, 1).unwrap().with_sweep(60_000, changes.clone());
        changes.set(1);
        for name in only {
            let (_, stream, kind) = (all().find(|t| t.name == name).unwrap().boot)();
            e.start(stream, kind);
        }
        let mut asked = 0;
        e.run_until_idle().unwrap();
        answer_every_effect(&mut e, &mut asked);
        assert!(asked > 0, "the boot ticks asked for nothing, so the quiet below proves nothing");
        let (commits, reads) = (journal.commits.load(std::sync::atomic::Ordering::SeqCst), journal.poison_reads.load(std::sync::atomic::Ordering::SeqCst));
        asked = 0;
        for _ in 0..61 {
            clock.advance(60_000);
            e.run_until_idle().unwrap();
            answer_every_effect(&mut e, &mut asked);
        }
        assert_eq!(asked, 0, "an effect ran in an idle hour");
        assert_eq!((journal.commits.load(std::sync::atomic::Ordering::SeqCst), journal.poison_reads.load(std::sync::atomic::Ordering::SeqCst)), (commits, reads));
        let next = e.next_timer_at();
        assert_eq!(next, Some(clock.now_ms() + 60_000));
        for t in e.status().tasks {
            assert_eq!(t.next_due_at, next, "{}", t.name);
        }
    }

    fn readiness_rig() -> (runtime::Engine, runtime::ports::VirtualClock, std::sync::Arc<runtime::changes::ScriptedChanges>) {
        use runtime::Executor;
        let s = crate::settings::fixture();
        let regs: Vec<Registration> = crate::registrations(&s).into_iter().filter(|r| r.name == readiness_dependents::NAME).collect();
        let clock = runtime::ports::VirtualClock::new(0);
        let changes = std::sync::Arc::new(runtime::changes::ScriptedChanges::default());
        changes.set(1);
        let mut e = runtime::Engine::open(runtime::MemJournal::new(), std::sync::Arc::new(clock.clone()), regs, 1).unwrap().with_sweep(60_000, changes.clone());
        let (_, stream, kind) = readiness_dependents::ReadinessDependents::boot();
        e.start(stream, kind);
        e.run_until_idle().unwrap();
        (e, clock, changes)
    }

    fn asked(e: &runtime::Engine, name: &str) -> usize {
        use runtime::Executor;
        e.pending_effects().iter().filter(|p| p.spec.name == name).count()
    }

    fn answer(e: &mut runtime::Engine, name: &str, out: runtime::Outcome) {
        use runtime::Executor;
        let key = e.pending_effects().into_iter().find(|p| p.spec.name == name).unwrap_or_else(|| panic!("{name} not asked")).key;
        e.complete(&key, out).unwrap();
        e.run_until_idle().unwrap();
    }

    #[test_id(tr4d8ua)]
    #[test]
    fn a_failed_readiness_run_is_looked_for_again_after_its_interval_and_before_any_sweep() {
        use runtime::Executor;
        for out in [runtime::Outcome::Failed("store down".into()), runtime::Outcome::TimedOut] {
            let (mut e, clock, _) = readiness_rig();
            answer(&mut e, readiness_dependents::LOOK, runtime::Outcome::Succeeded(r#"{"through":42,"keys":["sp-a"]}"#.into()));
            answer(&mut e, readiness_dependents::RUN, out.clone());
            assert_eq!(asked(&e, readiness_dependents::LOOK), 0);
            let interval = crate::settings::fixture().readiness_dependents.interval_ms;
            assert!(interval < 60_000);
            clock.advance(interval);
            e.run_until_idle().unwrap();
            assert_eq!(asked(&e, readiness_dependents::LOOK), 1, "{out:?}: asked again at its interval, not at the sweep");
        }
    }

    #[test_id(tj9x5hc)]
    #[test]
    fn a_failed_readiness_look_is_looked_for_again_after_its_interval_and_before_any_sweep() {
        use runtime::Executor;
        for out in [runtime::Outcome::Failed("store down".into()), runtime::Outcome::TimedOut] {
            let (mut e, clock, _) = readiness_rig();
            answer(&mut e, readiness_dependents::LOOK, out.clone());
            clock.advance(crate::settings::fixture().readiness_dependents.interval_ms);
            e.run_until_idle().unwrap();
            assert_eq!(asked(&e, readiness_dependents::LOOK), 1, "{out:?}");
        }
    }

    #[test_id(tk1y6bo)]
    #[test]
    fn a_lifecycle_change_wakes_readiness_dependents_with_no_sweep_tick() {
        use runtime::event::{StoreChanged, STORE_STREAM};
        use runtime::Executor;
        let (mut e, clock, _) = readiness_rig();
        answer(&mut e, readiness_dependents::LOOK, runtime::Outcome::Succeeded(r#"{"through":41,"keys":[]}"#.into()));
        assert_eq!(asked(&e, readiness_dependents::LOOK), 0);
        clock.advance(10_000);
        let c = StoreChanged { rev: 2, keys: vec!["lifecycle/sp-a".into()], reload: false };
        e.submit("c1", STORE_STREAM, c.kind()).unwrap();
        e.run_until_idle().unwrap();
        assert_eq!(asked(&e, readiness_dependents::LOOK), 1);
    }

    #[test_id(tw5s0nc)]
    #[test]
    fn a_lifecycle_change_and_the_sweep_falling_due_together_look_once() {
        use runtime::event::{StoreChanged, STORE_STREAM};
        use runtime::Executor;
        let (mut e, clock, changes) = readiness_rig();
        answer(&mut e, readiness_dependents::LOOK, runtime::Outcome::Succeeded(r#"{"through":1,"keys":[]}"#.into()));
        clock.advance(56_000);
        changes.set(2);
        let c = StoreChanged { rev: 2, keys: vec!["lifecycle/sp-a".into()], reload: false };
        e.submit("c1", STORE_STREAM, c.kind()).unwrap();
        e.run_until_idle().unwrap();
        answer(&mut e, readiness_dependents::LOOK, runtime::Outcome::Succeeded(r#"{"through":2,"keys":["sp-a"]}"#.into()));
        answer(&mut e, readiness_dependents::RUN, runtime::Outcome::Succeeded(String::new()));
        clock.advance(1_000);
        changes.set(3);
        let c = StoreChanged { rev: 3, keys: vec!["lifecycle/sp-a".into()], reload: false };
        e.submit("c2", STORE_STREAM, c.kind()).unwrap();
        e.run_until_idle().unwrap();
        clock.advance(4_000);
        e.run_until_idle().unwrap();
        assert_eq!(asked(&e, readiness_dependents::LOOK), 1, "the wake and the sweep are one run");
        answer(&mut e, readiness_dependents::LOOK, runtime::Outcome::Succeeded(r#"{"through":3,"keys":[]}"#.into()));
        for rev in [4, 5] {
            changes.set(rev);
            let c = StoreChanged { rev, keys: vec!["readiness/sp-a".into()], reload: false };
            e.submit(&format!("r{rev}"), STORE_STREAM, c.kind()).unwrap();
            e.run_until_idle().unwrap();
        }
        let t = &e.status().tasks[0];
        assert_eq!(t.sweep_found, 0);
        assert_eq!((t.runs_by_wake.get("signal").copied(), t.runs_by_wake.get("sweep").copied()), (Some(2), Some(1)));
    }
}
