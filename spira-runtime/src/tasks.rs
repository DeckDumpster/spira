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

pub struct Task {
    pub name: &'static str,
    /// `Work` is held while the scheduler is halted; `Signal` (waking a reader, paging, mailing
    /// the operator) runs through a halt. A row says which, explicitly; a `Signal` task may not
    /// write the lifecycle store, start a unit or touch git (`tests::no_signal_task_does_work`).
    pub plane: runtime::Plane,
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
}

/// The service's own tasks: the heartbeat and the watchers under `watchers/`.
pub static CORE: &[Task] = &[
    Task { name: heartbeat::NAME, plane: runtime::Plane::Work, boot: Heartbeat::boot, cadence_ms: |s| s.heartbeat_secs * 1000, effects: &[heartbeat::PING], runners: &[(heartbeat::PING, |e, _, s| e.ping(s))], keys: &[&["SPIRA_RUNTIME_HEARTBEAT_URL", "SPIRA_RUNTIME_HEARTBEAT_SECS"]], build: |s| Heartbeat { interval_ms: s.heartbeat_secs * 1000, ping_deadline_ms: s.ping_deadline_ms() }.registration() },
    Task { name: round_duty::NAME, plane: runtime::Plane::Work, boot: round_duty::RoundDuty::boot, cadence_ms: |s| s.round_duty.interval_ms, effects: &[round_duty::OBSERVE, INBOX_APPEND], runners: &[(round_duty::OBSERVE, |e, _, s| e.world.round_snapshot(&s.input))], keys: &[&["SPIRA_RUNTIME_ROUND_DUTY_SECS", "SPIRA_ROUND_MIN", "SPIRA_ROUND_MAX_OPEN", "SPIRA_ROUND_DUE_REMIND", "SPIRA_ROUND_IDLE_CUT"]], build: |s| s.round_duty.clone().registration() },
    Task { name: inbox_keeper::NAME, plane: runtime::Plane::Work, boot: inbox_keeper::InboxKeeper::boot, cadence_ms: |s| s.inbox_keeper.interval_ms, effects: &[inbox_keeper::OBSERVE, CONCIERGE_WAKE], runners: &[(inbox_keeper::OBSERVE, |e, _, _| e.world.keeper_snapshot()), (CONCIERGE_WAKE, |e, _, s| e.world.home_path().and_then(|home| wake::concierge_wake(&home.join("concierge.sh"), &s.input)))], keys: &[&["SPIRA_RUNTIME_INBOX_KEEPER_SECS", "SPIRA_CONCIERGE_INBOX_STALL", "SPIRA_CONCIERGE_INBOX_BACKOFF"]], build: |s| s.inbox_keeper.clone().registration() },
    Task { name: pr_notify::NAME, plane: runtime::Plane::Work, boot: pr_notify::PrNotify::boot, cadence_ms: |s| s.pr_notify.interval_ms, effects: &[pr_notify::OBSERVE, pr_notify::OPERATOR_NOTE, INBOX_APPEND], runners: &[(pr_notify::OBSERVE, |e, _, s| e.world.pr_snapshot(&e.forge, &s.input)), (pr_notify::OPERATOR_NOTE, |e, _, s| e.operator_note(&s.input))], keys: &[&["SPIRA_RUNTIME_PR_NOTIFY_SECS", "SPIRA_PR_DEPLOY_GIVEUP"]], build: |s| s.pr_notify.clone().registration() },
    Task { name: asks::NAME, plane: runtime::Plane::Work, boot: asks::Asks::boot, cadence_ms: |s| s.asks.interval_ms, effects: &[asks::OBSERVE, INBOX_APPEND], runners: &[(asks::OBSERVE, |e, _, s| e.world.asks_snapshot(&s.input))], keys: &[&["SPIRA_RUNTIME_ASKS_SECS"]], build: |s| s.asks.clone().registration() },
    Task { name: design_asks::NAME, plane: runtime::Plane::Work, boot: design_asks::DesignAsks::boot, cadence_ms: |s| s.design_asks.interval_ms, effects: &[design_asks::OBSERVE, INBOX_APPEND], runners: &[(design_asks::OBSERVE, |e, _, _| e.world.design_asks_snapshot())], keys: &[&["SPIRA_RUNTIME_DESIGN_ASKS_SECS", "SPIRA_DESIGN_ASKS_REPEAT"]], build: |s| s.design_asks.clone().registration() },
    Task { name: brain_dirty::NAME, plane: runtime::Plane::Work, boot: brain_dirty::BrainDirty::boot, cadence_ms: |s| s.brain_dirty.interval_ms, effects: &[brain_dirty::OBSERVE, INBOX_APPEND], runners: &[(brain_dirty::OBSERVE, |e, _, _| e.world.brain_dirty_snapshot())], keys: &[&["SPIRA_RUNTIME_BRAIN_DIRTY_SECS", "SPIRA_BRAIN_DIRTY_AGE"]], build: |s| s.brain_dirty.clone().registration() },
    Task { name: supersede_duty::NAME, plane: runtime::Plane::Work, boot: supersede_duty::SupersedeDuty::boot, cadence_ms: |s| s.supersede_duty.interval_ms, effects: &[supersede_duty::OBSERVE, supersede_duty::ACT, INBOX_APPEND], runners: &[(supersede_duty::OBSERVE, |e, _, s| e.world.supersede_snapshot(&s.input)), (supersede_duty::ACT, |e, _, s| e.world.supersede_act(&s.input))], keys: &[&["SPIRA_RUNTIME_SUPERSEDE_DUTY_SECS"]], build: |s| s.supersede_duty.clone().registration() },
    Task { name: bead_sweep::NAME, plane: runtime::Plane::Work, boot: bead_sweep::BeadSweep::boot, cadence_ms: |s| s.bead_sweep.interval_ms, effects: &[bead_sweep::OBSERVE, bead_sweep::REPAIR, INBOX_APPEND], runners: &[(bead_sweep::OBSERVE, |e, _, _| e.world.sweep_snapshot()), (bead_sweep::REPAIR, |e, _, s| e.world.sweep_repair(&s.input))], keys: &[&["SPIRA_RUNTIME_BEAD_SWEEP_SECS", "SPIRA_BEAD_SWEEP_HOLD_MIN", "SPIRA_BEAD_SWEEP_REWORK_MIN", "SPIRA_BEAD_SWEEP_CERT_MIN", "SPIRA_BEAD_SWEEP_WAIT_MIN", "SPIRA_BEAD_SWEEP_BATCH_MIN", "SPIRA_SCOPED_STOP_REVIEW_SECS"]], build: |s| s.bead_sweep.clone().registration() },
    Task { name: publish_backlog::NAME, plane: runtime::Plane::Work, boot: publish_backlog::PublishBacklog::boot, cadence_ms: |s| s.publish_backlog.interval_ms, effects: &[publish_backlog::OBSERVE, INBOX_APPEND], runners: &[(publish_backlog::OBSERVE, |e, _, _| e.world.publish_backlog_snapshot())], keys: &[&["SPIRA_RUNTIME_PUBLISH_BACKLOG_SECS", "SPIRA_LOCAL_BACKLOG_COUNT", "SPIRA_LOCAL_BACKLOG_AGE"]], build: |s| s.publish_backlog.clone().registration() },
    Task { name: repo_events::NAME, plane: runtime::Plane::Work, boot: repo_events::RepoEvents::boot, cadence_ms: |s| s.repo_events.interval_ms, effects: &[repo_events::OBSERVE, INBOX_APPEND], runners: &[(repo_events::OBSERVE, |e, _, s| e.world.repo_events_snapshot(&e.forge, &s.input))], keys: &[&["SPIRA_RUNTIME_REPO_EVENTS_SECS"]], build: |s| s.repo_events.clone().registration() },
    Task { name: overrides::NAME, plane: runtime::Plane::Work, boot: overrides::Overrides::boot, cadence_ms: |s| s.overrides.interval_ms, effects: &[overrides::APPLY, INBOX_APPEND], runners: &[(overrides::APPLY, |e, _, _| e.world.overrides_apply())], keys: &[&["SPIRA_RUNTIME_OVERRIDES_SECS"]], build: |s| s.overrides.clone().registration() },
    Task { name: view::NAME, plane: runtime::Plane::Work, boot: view::View::boot, cadence_ms: |s| s.view.interval_ms, effects: &[view::OBSERVE, view::FLIP, view::FORGET], runners: &[(view::OBSERVE, |e, _, s| e.world.view_snapshot(&s.input)), (view::FLIP, |e, _, s| e.world.view_flip(&s.input)), (view::FORGET, |e, _, s| e.world.view_forget(&s.input))], keys: &[&["SPIRA_RUNTIME_VIEW_SECS", "SPIRA_VIEW"]], build: |s| s.view.clone().registration() },
    Task { name: reconciler::NAME, plane: runtime::Plane::Work, boot: reconciler::Reconciler::boot, cadence_ms: |s| s.reconciler.interval_ms, effects: &[reconciler::OBSERVE, reconciler::RECORD, reconciler::REMEDY, reconciler::MAIL], runners: &[(reconciler::OBSERVE, |e, _, s| e.reconcile_observe(&s.input)), (reconciler::RECORD, |e, _, s| e.reconcile_record(&s.input)), (reconciler::REMEDY, |e, k, s| e.reconcile_remedy(k, &s.input)), (reconciler::MAIL, |e, k, s| e.reconcile_mail(k, &s.input))], keys: &[&["SPIRA_RUNTIME_RECONCILER_SECS", "SPIRA_RUNES", "SPIRA_STRANDED_RUNNER_MIN_AGE", "SPIRA_LC_PASSWORD_FILE"], ::reconciler::Config::KEYS], build: |s| s.reconciler.clone().registration() },
    Task { name: reconciler_flow::NAME, plane: runtime::Plane::Work, boot: reconciler_flow::ReconcilerFlow::boot, cadence_ms: |s| s.reconciler_flow.interval_ms, effects: &[reconciler_flow::OBSERVE, reconciler_flow::STATUS, reconciler_flow::MAIL], runners: &[(reconciler_flow::OBSERVE, |e, _, s| e.world.flow_snapshot(&s.input)), (reconciler_flow::STATUS, |e, _, s| e.world.flow_status(&s.input)), (reconciler_flow::MAIL, |e, _, s| e.concierge_note(&s.input))], keys: &[&["SPIRA_RUNTIME_RECONCILER_FLOW_SECS", "SPIRA_FLOW_WINDOW_HOURS", "SPIRA_FLOW_BASELINE_HOURS", "SPIRA_FLOW_GRACE_SECS", "SPIRA_FLOW_UNOBSERVABLE_GRACE_SECS", "SPIRA_FLOW_REWORK_WINDOW_HOURS", "SPIRA_FLOW_SENTINEL_PERIOD_SECS"]], build: |s| s.reconciler_flow.clone().registration() },
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
pub const CONTRACT_PAGES: &[&str] = &["asks-ideas", "asks-moot", "asks-verify", "batcher-forge", "batcher-local", "cert-full", "cert-sample", "cockpit-tier-5s", "cockpit-tier-600s", "cockpit-tier-60s", "czar", "gate-check", "gate-worker", "hel-reap-stale", "hel-reap-terminal", "hel-sweep", "inbox-notify", "incident-alarms", "incident-probe", "incident-settle", "intake-filings", "intake-gh", "intake-kit-bump", "landing", "mail-reconcile", "mail-tidy", "persona-archivist", "persona-ops", "publish", "queue-step", "reconcile-content", "reconcile-continuity", "reconcile-goodput", "reconcile-heimdall", "reconcile-section", "roundvm-ami-prune", "roundvm-drain", "sentinel-call", "sentinel-pass", "sift", "source-mail-arrived", "source-submission", "source-unit-ran", "submission-provision", "sweep-archive", "sweep-escape-census", "sweep-export-beads", "sweep-nix-gc", "sweep-reclaim", "sweep-target", "sweep-tsd", "trigger-groom", "trigger-mimir", "trigger-warden", "watch-ami-stale", "watch-backup-age", "watch-ci-spill", "watch-mend", "watch-pass-rate", "watch-perf-happy", "watch-perf", "watch-refusals", "watch-runtime", "watch-skew", "watch-sop-lint", "watch-spill-key", "watch-statute-audit", "watch-testenv-cap", "watch-tower", "watch-warm-full"];

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

/// Every effect a `Signal` task asks for, and every one of [`SIGNAL_EFFECTS`], with what its
/// runner touches. Reviewed by hand against the runner: none writes the lifecycle store,
/// starts a unit or touches git. A `Signal` row whose effect is missing here fails
/// `tests::no_signal_task_does_work`.
pub const SIGNAL_SAFE: &[(&str, &str)] = &[
    (INBOX_APPEND, "appends a line to the Concierge's inbox file (World::inbox_append)"),
    (g::mail_wake::OBSERVE, "reads each mailbox's unread age (mail unread-age, ls)"),
    (g::mail_wake::WAKE, "runs the mailbox reader's own wake command through the process port"),
    (e::incident_probe::OBSERVE, "reads the newest probe attempt under the run dir"),
    (e::incident_probe::SEND, "mails the probe through the mail store (bdq); refuses any other mailbox"),
];

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
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/watchers");
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
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tasks");
        // Files only: each family also has a `tasks/<x>/` directory of task modules beside `<x>.rs`.
        let mut out: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "rs")).filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned())).collect();
        out.sort();
        out
    }

    #[test]
    fn every_family_file_is_in_the_families_list() {
        let src = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tasks.rs")).unwrap();
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

    #[test]
    fn the_undeclarable_table_in_the_docs_is_the_rows() {
        let src = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tasks.rs")).unwrap();
        let doc = src.split("pub static FAMILIES:").nth(1).and_then(|r| r.split("pub static PENDING_FAMILIES:").next()).unwrap_or("");
        let want = undeclarable_table();
        assert!(doc.contains(&want), "the table above PENDING_FAMILIES is stale; it is:\n{want}");
    }

    #[test]
    fn no_two_tasks_across_families_share_a_name_or_a_stream() {
        static PLANTED: &[Task] = &[Task { name: heartbeat::NAME, plane: runtime::Plane::Work, boot: Heartbeat::boot, cadence_ms: |_| 1, effects: &[], runners: &[], keys: &[], build: |s| (CORE[0].build)(s) }];
        let caught = clashes(&[CORE, PLANTED]);
        assert!(caught.iter().any(|c| c.starts_with("name ")) && caught.iter().any(|c| c.starts_with("stream ")), "a row planted in a second family must be caught: {caught:?}");
        assert_eq!(clashes(FAMILIES), Vec::<String>::new());
    }

    #[test]
    fn the_registrations_and_the_boot_list_are_the_table() {
        let s = crate::settings::fixture();
        let regs: Vec<String> = crate::registrations(&s).into_iter().map(|r| r.name).collect();
        assert_eq!(regs, all().map(|t| t.name.to_string()).collect::<Vec<_>>());
        assert_eq!(boots().iter().map(|b| b.1).collect::<Vec<_>>(), all().map(|t| t.stream()).collect::<Vec<_>>());
    }

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

    #[test]
    fn the_unit_effects_are_shared_and_run_on_the_configured_unit_manager() {
        use runtime::host::EffectRunner;
        use std::sync::Arc;
        for e in units::EFFECTS {
            assert_eq!(SHARED.iter().filter(|(n, _)| *n == e).count(), 1, "{e} is one shared runner");
            assert!(pending().all(|t| t.runners.iter().all(|(n, _)| *n != e)), "{e} has a runner on a row as well");
        }
        let start = |input: &str| runtime::EffectSpec { name: units::UNIT_START.into(), input: input.into(), deadline_ms: 1 };
        let spec = serde_json::to_string(&units::UnitSpec { task: "cert.full".into(), key: "k1".into(), argv: vec!["true".into()], env: vec![], cwd: None }).unwrap();
        assert_eq!(crate::Effects::default().run("k", &start(&spec)), Err("no unit manager configured".into()), "no backend is refused, not guessed");
        let scripted = Arc::new(units::ScriptedUnits::new(Arc::new(runtime::ports::VirtualClock::new(0))));
        scripted.script("cert.full", 1_000, units::Exit::Code(0));
        let fx = crate::Effects { units: Some(scripted.clone()), ..crate::Effects::default() };
        let handle: units::UnitHandle = serde_json::from_str(&fx.run("k", &start(&spec)).unwrap()).unwrap();
        assert_eq!(handle, units::UnitHandle::of("cert.full", "k1").unwrap());
        let facts = runtime::EffectSpec { name: units::UNIT_FACTS.into(), input: serde_json::to_string(&handle).unwrap(), deadline_ms: 1 };
        let facts: units::UnitFacts = serde_json::from_str(&fx.run("k", &facts).unwrap()).unwrap();
        assert_eq!(facts.state, units::UnitState::Active);
        let stop = runtime::EffectSpec { name: units::UNIT_STOP.into(), input: serde_json::to_string(&handle).unwrap(), deadline_ms: 1 };
        assert_eq!(fx.run("k", &stop), Ok(String::new()));
        assert!(scripted.calls().iter().any(|c| c.starts_with("start ")) && scripted.calls().iter().any(|c| c.starts_with("stop ")), "{:?}", scripted.calls());
    }

    #[test]
    fn an_unknown_effect_name_is_refused_by_the_router() {
        use runtime::host::EffectRunner;
        let spec = runtime::EffectSpec { name: runtime::taskrig::PLANTED_UNKNOWN.into(), input: String::new(), deadline_ms: 1 };
        assert_eq!(crate::Effects::default().run("k", &spec), Err(format!("no runner for effect {}", runtime::taskrig::PLANTED_UNKNOWN)));
        let ping = runtime::EffectSpec { name: heartbeat::PING.into(), input: String::new(), deadline_ms: 1 };
        assert_eq!(crate::Effects::default().run("k", &ping), Ok("no heartbeat url configured".into()), "a declared name reaches its runner");
    }

    /// A halt lets `Signal` tasks run, so none may do the world's work: every effect a `Signal`
    /// row (ported or pending) or `SIGNAL_EFFECTS` names is one reviewed in `SIGNAL_SAFE`, and
    /// none is a unit verb.
    #[test]
    fn no_signal_task_does_work() {
        let safe: Vec<&str> = SIGNAL_SAFE.iter().map(|(n, _)| *n).collect();
        for (name, _) in SIGNAL_SAFE {
            assert!(!units::EFFECTS.contains(name), "{name} is a unit verb; it cannot be safe through a halt");
        }
        for e in SIGNAL_EFFECTS {
            assert!(safe.contains(e), "{e} runs through a halt for every task but is not reviewed in SIGNAL_SAFE");
        }
        let signal: Vec<&Task> = all().chain(pending()).filter(|t| t.plane == runtime::Plane::Signal).collect();
        assert!(!signal.is_empty(), "the signal tasks are on the signal plane");
        for t in signal {
            for e in t.effects {
                assert!(safe.contains(e), "{} is Signal but asks for {e}, which SIGNAL_SAFE does not clear (a lifecycle write, a unit or git keeps a task on Work)", t.name);
            }
        }
    }

    #[test]
    fn every_row_boots_arms_its_cadence_and_asks_only_for_declared_effects() {
        let s = crate::settings::fixture();
        for t in all() {
            let mut rig = TaskRig::new(1, (t.build)(&s)).unwrap();
            let (_, stream, kind) = (t.boot)();
            rig.tick(stream, kind).unwrap();
            let next = rig.engine().next_timer_at();
            assert_eq!(next, Some((t.cadence_ms)(&s)), "{}: the boot tick must arm the next one a cadence later", t.name);
            for q in rig.requested() {
                assert!(t.effects.contains(&q.name.as_str()), "{} asked for {} which its row does not declare", t.name, q.name);
            }
        }
    }
}
