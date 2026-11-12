//! Effect doubles: the runner a simulated host answers effects with. A name with no double is
//! refused with the production runner's own text, so the handler sees what it would see live.

use std::collections::BTreeMap;
use std::sync::Mutex;

use runtime::host::EffectRunner;
use runtime::EffectSpec;

pub type Double = Box<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

#[derive(Default)]
pub struct Doubles {
    by_name: BTreeMap<String, Double>,
    asked: Mutex<Vec<String>>,
}

impl Doubles {
    pub fn double(mut self, name: &str, f: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static) -> Self {
        self.by_name.insert(name.into(), Box::new(f));
        self
    }

    /// Every effect name this runner has a double for, sorted.
    pub fn names(&self) -> Vec<String> {
        self.by_name.keys().cloned().collect()
    }

    /// Every effect name asked for, in the order it was run.
    pub fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl EffectRunner for Doubles {
    fn run(&self, _key: &str, spec: &EffectSpec) -> Result<String, String> {
        self.asked.lock().unwrap_or_else(|e| e.into_inner()).push(spec.name.clone());
        match self.by_name.get(&spec.name) {
            Some(f) => f(&spec.input),
            None => Err(format!("no runner for effect {}", spec.name)),
        }
    }
}

/// Every task row of the service (spira-runtime's `tasks::all()` and `tasks::pending()`) with
/// the effects its row declares, by name, in any order: the check compares the rows as a set, so
/// porting a row (moving it from `PENDING` to `TASKS`) needs no edit here. Named here, not read from the table, so a task that
/// gains an effect fails T4 until the simulator can answer it; spira-runtime's
/// `the_sim_names_a_double_for_every_effect_of_every_row_ported_or_pending` prints this list
/// when the two disagree.
pub const TASK_EFFECTS: &[(&str, &[&str])] = &[
    ("heartbeat", &["heartbeat.ping"]),
    ("round-duty", &["round-duty.observe", "inbox.append"]),
    ("inbox-keeper", &["inbox-keeper.observe", "concierge.wake"]),
    ("pr-notify", &["pr-notify.observe", "operator.note", "inbox.append"]),
    ("asks", &["asks.observe", "inbox.append"]),
    ("design-asks", &["design-asks.observe", "inbox.append"]),
    ("brain-dirty", &["brain-dirty.observe", "inbox.append"]),
    ("supersede-duty", &["supersede-duty.observe", "supersede-duty.act", "inbox.append"]),
    ("bead-sweep", &["bead-sweep.observe", "bead-sweep.repair", "inbox.append"]),
    ("publish-backlog", &["publish-backlog.observe", "inbox.append"]),
    ("overrides", &["overrides.apply", "inbox.append"]),
    ("readiness-dependents", &["readiness-dependents.look", "readiness-dependents.run"]),
    ("view", &["view.observe", "view.flip", "view.forget"]),
    ("reconciler", &["reconciler.observe", "reconciler.record", "reconciler.remedy", "reconciler.mail"]),
    ("reconciler-flow", &["reconciler-flow.observe", "reconciler-flow.status", "reconciler-flow.mail"]),
    ("sentinel-call", &["sentinel-call.observe", "sentinel-call.unit_start", "sentinel-call.preempt", "sentinel-call.preempt_log", "sentinel-call.metric"]),
    ("sentinel-pass", &["sentinel-pass.observe", "sentinel-pass.lc_event", "sentinel-pass.lc_hold", "sentinel-pass.lc_unhold", "sentinel-pass.lc_fact", "sentinel-pass.lc_close", "sentinel-pass.content_label", "sentinel-pass.content_note", "sentinel-pass.content_close", "sentinel-pass.recompute_blocked", "sentinel-pass.stop_holder", "sentinel-pass.landing_start", "sentinel-pass.watchtower", "sentinel-pass.strand", "sentinel-pass.pilgrimage_close", "sentinel-pass.ask", "sentinel-pass.incident_event", "sentinel-pass.reflect", "sentinel-pass.phase_metric"]),
    ("czar", &["czar.observe", "czar.file_incident", "czar.file_base_red", "czar.rerun_workflow", "czar.restart_landing", "czar.status", "inbox.append"]),
    ("sift", &["sift.observe", "sift.fetch_base", "sift.note", "sift.send_back", "sift.record_pass", "sift.supersede", "inbox.append"]),
    ("batcher.local", &["unit.facts", "batcher.local.observe", "batcher.local.fetch", "batcher.local.assemble", "batcher.local.round_open", "batcher.local.corpus", "batcher.local.stage", "batcher.local.stage_test", "batcher.local.eject", "batcher.local.withdraw", "batcher.local.rebase_stale", "batcher.local.certify", "batcher.local.lc_certify", "batcher.local.land", "batcher.local.abandon", "batcher.local.promote", "batcher.local.redrive", "batcher.local.file", "batcher.local.pause", "batcher.local.keep_certify_output", "inbox.append"]),
    ("batcher.forge", &["unit.facts", "batcher.forge.observe", "batcher.forge.arm", "batcher.forge.fetch", "batcher.forge.assemble", "batcher.forge.corpus", "batcher.forge.withdraw", "batcher.forge.shots", "batcher.forge.batch_open", "batcher.forge.file", "batcher.forge.verdict_write", "inbox.append"]),
    ("queue.step", &["unit.facts", "queue.step.observe", "queue.step.push", "queue.step.land", "queue.step.close_bead", "queue.step.gh_closeout", "queue.step.pr_close", "queue.step.cancel_runs", "queue.step.rerun", "queue.step.judgement", "queue.step.observe_flake", "queue.step.eject", "queue.step.reconcile", "queue.step.sweep", "queue.step.file_bug", "queue.step.notify", "queue.step.record"]),
    ("publish", &["publish.observe", "publish.push_branch", "publish.push_forge", "publish.pr_create", "publish.pr_close", "publish.cancel_runs", "publish.rerun", "publish.record", "publish.notify", "publish.settle"]),
    ("gate.worker", &["gate.worker.observe", "gate.worker.tip", "gate.worker.claim", "gate.worker.discard", "gate.worker.retain_output", "gate.worker.complete", "gate.worker.recover", "unit.start"]),
    ("gate.check", &["gate.check.observe", "gate.check.discover", "gate.check.resolve", "gate.check.ci_failed"]),
    ("landing", &["landing.observe", "landing.fetch", "landing.deliver_pr_merged", "landing.deliver_pr_closed", "landing.deliver_by_content", "landing.reopen", "landing.event", "landing.note", "landing.bump_requeue", "landing.ask", "landing.rebase", "landing.confine", "landing.force_push", "landing.pr_create", "landing.pr_automerge"]),
    ("sweep.target", &["sweep.target.observe", "sweep.target.remove_target", "sweep.target.remove_foreign_run"]),
    ("submission-provision", &["submission-provision.observe", "submission-provision.cert_renew", "submission-provision.config_set", "submission-provision.password_write", "submission-provision.firewall_open", "submission-provision.unit_restart", "inbox.append"]),
    ("reconcile.section", &["reconcile.section.publish"]),
    ("reconcile.goodput", &["reconcile.goodput.observe", "reconcile.goodput.lock", "reconcile.goodput.standby", "reconcile.goodput.judge", "reconcile.goodput.put", "reconcile.goodput.record", "reconcile.goodput.release"]),
    ("reconcile.content", &["reconcile.content.observe", "reconcile.content.skew_check", "reconcile.content.close"]),
    ("reconcile.heimdall", &["reconcile.heimdall.observe", "reconcile.heimdall.raise", "reconcile.heimdall.clear", "reconcile.heimdall.refresh", "reconcile.heimdall.fallback", "reconcile.heimdall.heartbeat"]),
    ("reconcile.continuity", &["reconcile.continuity.observe", "inbox.append"]),
    ("incident.settle", &["incident.settle.observe", "incident.settle.close", "incident.settle.note", "incident.settle.label", "incident.settle.log"]),
    ("incident.alarms", &["incident.alarms.observe", "incident.alarms.page", "incident.alarms.note", "incident.alarms.floor_ask", "incident.alarms.retry", "incident.alarms.attempt", "incident.alarms.knob_init", "incident.alarms.publish", "incident.alarms.heartbeat"]),
    ("mail.reconcile", &["mail.reconcile.observe", "mail.reconcile.note", "mail.reconcile.close", "mail.reconcile.lift", "mail.reconcile.answer", "mail.reconcile.inbox", "mail.reconcile.mark"]),
    ("asks.verify", &["asks.verify.observe", "asks.verify.check", "asks.verify.close", "asks.verify.withdraw", "asks.verify.note"]),
    ("hel.sweep", &["hel.sweep.observe", "hel.sweep.pr_tip", "hel.sweep.send", "hel.sweep.content_on_base", "hel.sweep.close_on_land", "hel.sweep.archive", "hel.sweep.free_worktree", "hel.sweep.orphan_worktree", "hel.sweep.prune"]),
    ("hel.reap_terminal", &["hel.reap_terminal.observe", "hel.reap_terminal.destroy_worktree", "hel.reap_terminal.remove_scratch", "hel.reap_terminal.prune"]),
    ("hel.reap_stale", &["hel.reap_stale.observe", "hel.reap_stale.destroy_worktree", "hel.reap_stale.prune"]),
    ("sweep.reclaim", &["sweep.reclaim.observe", "sweep.reclaim.remove_target", "sweep.reclaim.podman_prune", "sweep.reclaim.trim_cache"]),
    ("asks.moot", &["asks.moot.observe", "asks.moot.check", "asks.moot.resolve", "asks.moot.withdraw"]),
    ("mail.tidy", &["mail.tidy.observe", "mail.tidy.archive"]),
    ("sweep.tsd", &["sweep.tsd.observe", "sweep.tsd.prune"]),
    ("roundvm.drain", &["roundvm.drain.observe", "roundvm.drain.release"]),
    ("roundvm.ami_prune", &["roundvm.ami_prune.observe", "roundvm.ami_prune.deregister", "roundvm.ami_prune.delete_snapshot"]),
    ("sweep.archive", &["sweep.archive.observe", "sweep.archive.store", "sweep.archive.index"]),
    ("sweep.nix_gc", &["sweep.nix_gc.observe", "sweep.nix_gc.collect", "inbox.append"]),
    ("sweep.export_beads", &["sweep.export_beads.observe", "sweep.export_beads.write", "sweep.export_beads.commit", "sweep.export_beads.publish", "sweep.export_beads.touch"]),
    ("sweep.escape_census", &["sweep.escape_census.observe", "sweep.escape_census.report"]),
    ("watch.perf", &["watch.perf.observe", "inbox.append", "watch.perf.log", "watch.perf.slow_sample", "watch.perf.metrics"]),
    ("watch.skew", &["watch.skew.observe", "watch.skew.escalate", "watch.skew.log", "watch.skew.refresh"]),
    ("watch.refusals", &["watch.refusals.observe", "watch.refusals.file"]),
    ("watch.runtime", &["watch.runtime.observe", "watch.runtime.file_bump", "watch.runtime.metrics", "watch.runtime.log"]),
    ("watch.tower", &["watch.tower.observe", "watch.tower.prompt", "watch.tower.alarm", "watch.tower.drain_check", "watch.tower.log"]),
    ("watch.mend", &["watch.mend.observe", "watch.mend.log_append", "watch.mend.lift_stop"]),
    ("watch.sop_lint", &["watch.sop_lint.observe", "watch.sop_lint.file_incident"]),
    ("cert.full", &["cert.full.observe", "cert.full.checkout", "unit.start", "unit.stop", "cert.full.file_bead", "cert.full.record"]),
    ("cert.sample", &["cert.sample.observe", "cert.sample.branch", "unit.start", "cert.sample.file_bead", "cert.sample.record"]),
    ("inbox.notify", &["inbox.notify.observe", "inbox.notify.ask", "inbox.notify.sop_match", "unit.start", "inbox.notify.sop_applied", "inbox.notify.concierge_note", "inbox.notify.mail_health"]),
    ("incident.probe", &["incident.probe.observe", "incident.probe.send"]),
    ("watch.ci_spill", &["watch.ci_spill.observe", "inbox.append"]),
    ("watch.dwarf_usage", &["watch.dwarf_usage.sample"]),
    ("watch.backup_age", &["watch.backup_age.observe", "watch.backup_age.mail"]),
    ("watch.pass_rate", &["watch.pass_rate.observe", "watch.pass_rate.mail"]),
    ("watch.throughput", &["watch.throughput.observe", "watch.throughput.file", "watch.throughput.clear", "inbox.append"]),
    ("watch.transitions", &["watch.transitions.observe", "inbox.append"]),
    ("watch.ami_stale", &["watch.ami_stale.observe", "inbox.append"]),
    ("watch.testenv_cap", &["watch.testenv_cap.observe", "watch.testenv_cap.kill", "inbox.append", "watch.testenv_cap.metric"]),
    ("watch.warm_full", &["watch.warm_full.observe", "watch.warm_full.reap", "inbox.append"]),
    ("intake.gh", &["intake.gh.observe", "intake.gh.file_work", "intake.gh.file_untrusted", "intake.gh.note_seen", "intake.gh.close_promoted", "intake.gh.digest", "intake.gh.unverifiable", "inbox.append"]),
    ("trigger.groom", &["trigger.groom.observe", "trigger.groom.file"]),
    ("trigger.warden", &["trigger.warden.observe", "trigger.warden.file"]),
    ("trigger.mimir", &["trigger.mimir.observe", "trigger.mimir.file"]),
    ("persona.ops", &["persona.ops.observe", "persona.ops.intake_install", "persona.ops.incident_drain", "persona.ops.brief", "unit.start", "unit.stop", "persona.ops.ledger", "persona.ops.cleanup"]),
    ("persona.archivist", &["persona.archivist.observe", "persona.archivist.brief", "unit.start", "unit.stop", "persona.archivist.commit", "persona.archivist.settle", "persona.archivist.digest_send", "persona.archivist.mark", "persona.archivist.forget"]),
    ("asks.ideas", &["asks.ideas.observe", "asks.ideas.ask"]),
    ("intake.filings", &["intake.filings.observe", "intake.filings.file", "intake.filings.append", "intake.filings.ack", "inbox.append"]),
    ("intake.kit_bump", &["intake.kit_bump.observe", "intake.kit_bump.file", "intake.kit_bump.amend"]),
    ("cockpit-tier-5s", &["cockpit-tier-5s.observe", "cockpit-tier-5s.publish"]),
    ("cockpit-tier-60s", &["cockpit-tier-60s.observe", "cockpit-tier-60s.publish", "cockpit-tier-60s.slots_sample"]),
    ("cockpit-tier-600s", &["cockpit-tier-600s.observe", "cockpit-tier-600s.publish"]),
    ("mail.wake", &["mail.wake.observe", "mail.wake.wake"]),
];

/// The effects several tasks share (spira-runtime's `tasks::SHARED`): the inbox line and the
/// unit manager's verbs. `sim::units::with_units` replaces the latter with a scripted manager.
pub const SHARED_EFFECTS: &[&str] = &["inbox.append", "unit.start", "unit.stop", "unit.restart", "unit.facts"];

/// Every effect name `TASK_EFFECTS` and `SHARED_EFFECTS` hold, each once, sorted.
pub fn service_effects() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = TASK_EFFECTS.iter().flat_map(|(_, es)| es.iter().copied()).chain(SHARED_EFFECTS.iter().copied()).collect();
    out.sort();
    out.dedup();
    out
}

/// A double answering an empty object.
fn quiet(d: Doubles, name: &str) -> Doubles {
    d.double(name, |_| Ok("{}".to_string()))
}

impl Doubles {
    /// The quiet world for the tasks named `tasks`: every effect each declares, and the shared
    /// ones, answer `{}`. A name `TASK_EFFECTS` does not hold is refused, naming it.
    pub fn for_tasks(tasks: &[&str]) -> Result<Doubles, String> {
        let mut d = SHARED_EFFECTS.iter().fold(Doubles::default(), |d, n| quiet(d, n));
        for t in tasks {
            let (_, effects) = TASK_EFFECTS.iter().find(|(n, _)| n == t).ok_or_else(|| format!("the simulator has no doubles for task {t}"))?;
            d = effects.iter().fold(d, |d, n| quiet(d, n));
        }
        Ok(d)
    }
}

/// The service's task table in a quiet world: every effect of every row, ported or pending,
/// succeeds with an empty object. An act does nothing; an observe whose snapshot needs fields
/// reads as no answer, so the task stays idle. A scenario that needs a world replaces the
/// doubles it cares about.
pub fn service() -> Doubles {
    service_effects().into_iter().fold(Doubles::default(), quiet)
}
