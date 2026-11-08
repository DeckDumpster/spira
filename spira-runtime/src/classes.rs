//! The effector class each effect runs under, so each class's cap in `PoolCaps` binds on the
//! effects it names and a slow class cannot take the permits another class needs.
//!
//! A task with a contract names each effect's class in the Class column of its Act table (the
//! `//! ## Act` section of its file under `tasks/`); [`CLASSES`] holds that column, and
//! `every_act_table_row_agrees_with_the_class_table` fails when the two drift. The core
//! watchers and the shared effects have no table, so their rows here are the route: a read of
//! the lifecycle store or mail through it is `store`, a forge call `forge`, a unit verb `unit`,
//! everything else on this host `host`. An effect with no row is refused at registration by
//! `every_registered_effect_names_a_class`; nothing falls through to Host by default.

use runtime::pools::EffectClass::{self, Container, Forge, Git, Host, Store, Unit};

/// Every registered effect by name, with its class.
pub const CLASSES: &[(&str, EffectClass)] = &[
    ("asks.ideas.ask", Store),
    ("asks.ideas.observe", Store),
    ("asks.moot.check", Host),
    ("asks.moot.observe", Store),
    ("asks.moot.resolve", Store),
    ("asks.moot.withdraw", Store),
    ("asks.observe", Store),
    ("asks.verify.check", Host),
    ("asks.verify.close", Store),
    ("asks.verify.note", Host),
    ("asks.verify.observe", Store),
    ("asks.verify.withdraw", Store),
    ("batcher.forge.arm", Forge),
    ("batcher.forge.assemble", Git),
    ("batcher.forge.batch_open", Forge),
    ("batcher.forge.corpus", Unit),
    ("batcher.forge.fetch", Git),
    ("batcher.forge.file", Store),
    ("batcher.forge.observe", Store),
    ("batcher.forge.shots", Container),
    ("batcher.forge.verdict_write", Store),
    ("batcher.forge.withdraw", Store),
    ("batcher.local.abandon", Store),
    ("batcher.local.assemble", Git),
    ("batcher.local.certify", Unit),
    ("batcher.local.corpus", Unit),
    ("batcher.local.eject", Store),
    ("batcher.local.fetch", Git),
    ("batcher.local.file", Store),
    ("batcher.local.land", Host),
    ("batcher.local.lc_certify", Store),
    ("batcher.local.observe", Store),
    ("batcher.local.pause", Store),
    ("batcher.local.promote", Git),
    ("batcher.local.rebase_stale", Unit),
    ("batcher.local.redrive", Unit),
    ("batcher.local.round_open", Store),
    ("batcher.local.stage_test", Git),
    ("batcher.local.stage", Unit),
    ("batcher.local.withdraw", Store),
    ("bead-sweep.observe", Store),
    ("bead-sweep.repair", Store),
    ("brain-dirty.observe", Git),
    ("cert.full.checkout", Git),
    ("cert.full.file_bead", Store),
    ("cert.full.observe", Git),
    ("cert.full.record", Host),
    ("cert.sample.branch", Git),
    ("cert.sample.file_bead", Store),
    ("cert.sample.observe", Git),
    ("cert.sample.record", Host),
    ("cockpit-tier-5s.observe", Host),
    ("cockpit-tier-5s.publish", Host),
    ("cockpit-tier-600s.observe", Host),
    ("cockpit-tier-600s.publish", Host),
    ("cockpit-tier-60s.observe", Host),
    ("cockpit-tier-60s.publish", Host),
    ("cockpit-tier-60s.slots_sample", Store),
    ("concierge.wake", Host),
    ("czar.file_base_red", Store),
    ("czar.file_incident", Store),
    ("czar.observe", Store),
    ("czar.rerun_workflow", Forge),
    ("czar.restart_landing", Unit),
    ("czar.status", Host),
    ("design-asks.observe", Host),
    ("gate.check.ci_failed", Store),
    ("gate.check.discover", Store),
    ("gate.check.file_bead", Store),
    ("gate.check.note", Store),
    ("gate.check.observe", Store),
    ("gate.check.raise_priority", Store),
    ("gate.check.resolve", Store),
    ("gate.check.tsd_ingest", Host),
    ("gate.worker.claim", Store),
    ("gate.worker.complete", Store),
    ("gate.worker.discard", Store),
    ("gate.worker.observe", Store),
    ("gate.worker.recover", Store),
    ("gate.worker.retain_output", Host),
    ("gate.worker.tip", Git),
    ("heartbeat.ping", Host),
    ("hel.reap_stale.destroy_worktree", Git),
    ("hel.reap_stale.observe", Store),
    ("hel.reap_stale.prune", Git),
    ("hel.reap_terminal.destroy_worktree", Git),
    ("hel.reap_terminal.observe", Store),
    ("hel.reap_terminal.prune", Git),
    ("hel.reap_terminal.remove_scratch", Host),
    ("hel.sweep.archive", Git),
    ("hel.sweep.close_on_land", Store),
    ("hel.sweep.content_on_base", Store),
    ("hel.sweep.free_worktree", Git),
    ("hel.sweep.observe", Store),
    ("hel.sweep.orphan_worktree", Git),
    ("hel.sweep.pr_tip", Forge),
    ("hel.sweep.prune", Git),
    ("hel.sweep.send", Git),
    ("inbox.append", Host),
    ("inbox-keeper.observe", Host),
    ("inbox.notify.ask", Store),
    ("inbox.notify.concierge_note", Store),
    ("inbox.notify.mail_health", Store),
    ("inbox.notify.observe", Host),
    ("inbox.notify.sop_applied", Store),
    ("inbox.notify.sop_match", Store),
    ("incident.alarms.attempt", Host),
    ("incident.alarms.floor_ask", Store),
    ("incident.alarms.heartbeat", Host),
    ("incident.alarms.knob_init", Store),
    ("incident.alarms.note", Store),
    ("incident.alarms.observe", Store),
    ("incident.alarms.page", Store),
    ("incident.alarms.publish", Store),
    ("incident.alarms.retry", Store),
    ("incident.probe.observe", Host),
    ("incident.probe.send", Store),
    ("incident.settle.close", Store),
    ("incident.settle.label", Store),
    ("incident.settle.log", Host),
    ("incident.settle.note", Store),
    ("incident.settle.observe", Store),
    ("intake.filings.ack", Host),
    ("intake.filings.append", Store),
    ("intake.filings.file", Store),
    ("intake.filings.observe", Host),
    ("intake.gh.close_promoted", Store),
    ("intake.gh.digest", Store),
    ("intake.gh.file_untrusted", Store),
    ("intake.gh.file_work", Store),
    ("intake.gh.note_seen", Store),
    ("intake.gh.observe", Forge),
    ("intake.gh.unverifiable", Store),
    ("intake.kit_bump.amend", Store),
    ("intake.kit_bump.file", Store),
    ("intake.kit_bump.observe", Host),
    ("landing.ask", Store),
    ("landing.bump_requeue", Store),
    ("landing.confine", Git),
    ("landing.deliver_by_content", Store),
    ("landing.deliver_pr_closed", Store),
    ("landing.deliver_pr_merged", Store),
    ("landing.event", Store),
    ("landing.fetch", Git),
    ("landing.force_push", Git),
    ("landing.note", Store),
    ("landing.observe", Store),
    ("landing.pr_automerge", Forge),
    ("landing.pr_create", Forge),
    ("landing.rebase", Git),
    ("landing.reopen", Store),
    ("mail.reconcile.answer", Store),
    ("mail.reconcile.close", Store),
    ("mail.reconcile.inbox", Host),
    ("mail.reconcile.lift", Store),
    ("mail.reconcile.mark", Host),
    ("mail.reconcile.note", Store),
    ("mail.reconcile.observe", Store),
    ("mail.tidy.archive", Host),
    ("mail.tidy.observe", Store),
    ("mail.wake.observe", Host),
    ("mail.wake.wake", Host),
    ("operator.note", Store),
    ("overrides.apply", Host),
    ("readiness-dependents.run", Store),
    ("persona.archivist.brief", Host),
    ("persona.archivist.commit", Git),
    ("persona.archivist.digest_send", Host),
    ("persona.archivist.forget", Host),
    ("persona.archivist.mark", Host),
    ("persona.archivist.observe", Host),
    ("persona.archivist.settle", Host),
    ("persona.ops.brief", Host),
    ("persona.ops.cleanup", Host),
    ("persona.ops.incident_drain", Store),
    ("persona.ops.intake_install", Host),
    ("persona.ops.ledger", Host),
    ("persona.ops.observe", Host),
    ("pr-notify.observe", Forge),
    ("publish-backlog.observe", Git),
    ("publish.cancel_runs", Forge),
    ("publish.notify", Store),
    ("publish.observe", Store),
    ("publish.pr_close", Forge),
    ("publish.pr_create", Forge),
    ("publish.push_branch", Git),
    ("publish.push_forge", Git),
    ("publish.record", Host),
    ("publish.rerun", Forge),
    ("publish.settle", Git),
    ("queue.step.cancel_runs", Forge),
    ("queue.step.close_bead", Store),
    ("queue.step.eject", Store),
    ("queue.step.file_bug", Store),
    ("queue.step.gh_closeout", Forge),
    ("queue.step.judgement", Unit),
    ("queue.step.land", Store),
    ("queue.step.notify", Store),
    ("queue.step.observe_flake", Container),
    ("queue.step.observe", Store),
    ("queue.step.pr_close", Forge),
    ("queue.step.push", Git),
    ("queue.step.reconcile", Store),
    ("queue.step.record", Host),
    ("queue.step.rerun", Forge),
    ("queue.step.sweep", Store),
    ("reconcile.content.close", Store),
    ("reconcile.content.observe", Store),
    ("reconcile.content.skew_check", Store),
    ("reconcile.continuity.observe", Store),
    ("reconcile.goodput.judge", Store),
    ("reconcile.goodput.lock", Store),
    ("reconcile.goodput.observe", Store),
    ("reconcile.goodput.put", Store),
    ("reconcile.goodput.record", Store),
    ("reconcile.goodput.release", Store),
    ("reconcile.goodput.standby", Store),
    ("reconcile.heimdall.clear", Store),
    ("reconcile.heimdall.fallback", Host),
    ("reconcile.heimdall.heartbeat", Host),
    ("reconcile.heimdall.observe", Store),
    ("reconcile.heimdall.raise", Store),
    ("reconcile.heimdall.refresh", Store),
    ("reconciler-flow.mail", Store),
    ("reconciler-flow.observe", Store),
    ("reconciler-flow.status", Store),
    ("reconciler.mail", Store),
    ("reconciler.observe", Host),
    ("reconciler.record", Host),
    ("reconciler.remedy", Host),
    ("reconcile.section.publish", Store),
    ("repo-events.observe", Forge),
    ("round-duty.observe", Host),
    ("roundvm.ami_prune.delete_snapshot", Host),
    ("roundvm.ami_prune.deregister", Host),
    ("roundvm.ami_prune.observe", Host),
    ("roundvm.drain.observe", Host),
    ("roundvm.drain.release", Host),
    ("sentinel-call.metric", Host),
    ("sentinel-call.observe", Store),
    ("sentinel-call.preempt_log", Host),
    ("sentinel-call.preempt", Unit),
    ("sentinel-call.unit_start", Unit),
    ("sentinel-pass.ask", Store),
    ("sentinel-pass.content_close", Store),
    ("sentinel-pass.content_label", Store),
    ("sentinel-pass.content_note", Store),
    ("sentinel-pass.incident_event", Store),
    ("sentinel-pass.landing_start", Unit),
    ("sentinel-pass.lc_close", Store),
    ("sentinel-pass.lc_event", Store),
    ("sentinel-pass.lc_fact", Store),
    ("sentinel-pass.lc_hold", Store),
    ("sentinel-pass.lc_unhold", Store),
    ("sentinel-pass.observe", Store),
    ("sentinel-pass.phase_metric", Host),
    ("sentinel-pass.pilgrimage_close", Store),
    ("sentinel-pass.recompute_blocked", Store),
    ("sentinel-pass.reflect", Unit),
    ("sentinel-pass.stop_holder", Unit),
    ("sentinel-pass.strand", Store),
    ("sentinel-pass.watchtower", Host),
    ("sift.fetch_base", Git),
    ("sift.note", Store),
    ("sift.observe", Git),
    ("sift.record_pass", Store),
    ("sift.send_back", Store),
    ("sift.supersede", Store),
    ("submission-provision.cert_renew", Host),
    ("submission-provision.config_set", Host),
    ("submission-provision.firewall_open", Host),
    ("submission-provision.observe", Store),
    ("submission-provision.password_write", Host),
    ("submission-provision.unit_restart", Host),
    ("supersede-duty.act", Store),
    ("supersede-duty.observe", Store),
    ("sweep.archive.index", Host),
    ("sweep.archive.observe", Host),
    ("sweep.archive.store", Host),
    ("sweep.escape_census.observe", Store),
    ("sweep.escape_census.report", Host),
    ("sweep.export_beads.commit", Git),
    ("sweep.export_beads.observe", Store),
    ("sweep.export_beads.publish", Git),
    ("sweep.export_beads.touch", Host),
    ("sweep.export_beads.write", Host),
    ("sweep.nix_gc.collect", Unit),
    ("sweep.nix_gc.observe", Host),
    ("sweep.reclaim.observe", Store),
    ("sweep.reclaim.podman_prune", Container),
    ("sweep.reclaim.remove_target", Host),
    ("sweep.reclaim.trim_cache", Host),
    ("sweep.target.observe", Store),
    ("sweep.target.remove_foreign_run", Host),
    ("sweep.target.remove_target", Host),
    ("sweep.tsd.observe", Host),
    ("sweep.tsd.prune", Host),
    ("trigger.groom.file", Store),
    ("trigger.groom.observe", Store),
    ("trigger.mimir.file", Store),
    ("trigger.mimir.observe", Store),
    ("trigger.warden.file", Store),
    ("trigger.warden.observe", Store),
    ("unit.facts", Unit),
    ("unit.restart", Unit),
    ("unit.start", Unit),
    ("unit.stop", Unit),
    ("view.flip", Host),
    ("view.forget", Host),
    ("view.observe", Host),
    ("watch.ami_stale.observe", Host),
    ("watch.backup_age.mail", Store),
    ("watch.backup_age.observe", Host),
    ("watch.ci_spill.observe", Git),
    ("watch.dwarf_usage.sample", Host),
    ("watch.mend.lift_stop", Host),
    ("watch.mend.log_append", Host),
    ("watch.mend.observe", Store),
    ("watch.pass_rate.mail", Store),
    ("watch.pass_rate.observe", Host),
    ("watch.perf_happy.checkout", Git),
    ("watch.perf_happy.history", Host),
    ("watch.perf_happy.log", Host),
    ("watch.perf_happy.observe", Store),
    ("watch.perf_happy.teardown", Container),
    ("watch.perf.log", Host),
    ("watch.perf.metrics", Host),
    ("watch.perf.observe", Store),
    ("watch.perf.slow_sample", Host),
    ("watch.refusals.file", Store),
    ("watch.refusals.observe", Store),
    ("watch.runtime.file_bump", Store),
    ("watch.runtime.log", Host),
    ("watch.runtime.metrics", Host),
    ("watch.runtime.observe", Host),
    ("watch.skew.escalate", Store),
    ("watch.skew.log", Host),
    ("watch.skew.observe", Git),
    ("watch.skew.refresh", Git),
    ("watch.sop_lint.file_incident", Store),
    ("watch.sop_lint.observe", Store),
    ("watch.spill_key.observe", Host),
    ("watch.statute_audit.file_bead", Store),
    ("watch.statute_audit.observe", Store),
    ("watch.testenv_cap.kill", Host),
    ("watch.testenv_cap.metric", Host),
    ("watch.testenv_cap.observe", Host),
    ("watch.throughput.observe", Host),
    ("watch.throughput.stamp", Host),
    ("watch.tower.alarm", Store),
    ("watch.tower.drain_check", Store),
    ("watch.tower.log", Host),
    ("watch.tower.observe", Host),
    ("watch.tower.prompt", Host),
    ("watch.transitions.observe", Host),
    ("watch.warm_full.observe", Host),
    ("watch.warm_full.reap", Host),
];

/// The class `name` runs under, `None` for a name no task registers.
pub fn class_of(name: &str) -> Option<EffectClass> {
    CLASSES.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::Path;

    /// Every effect name a booted task can ask for or route, and the shared ones.
    fn registered() -> Vec<&'static str> {
        let mut out: Vec<&str> = crate::tasks::all().flat_map(|t| t.effects.iter().copied().chain(t.runners.iter().map(|(n, _)| *n))).chain(crate::tasks::SHARED.iter().map(|(n, _)| *n)).collect();
        out.sort();
        out.dedup();
        out
    }

    fn files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                files(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    /// Each Act-table row's effect (the first backticked name in its first cell) and the first
    /// word of its Class cell, from every file under `src/tasks`, read from the source.
    fn act_tables() -> BTreeMap<String, (String, String)> {
        let mut paths = vec![];
        files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tasks"), &mut paths);
        let mut out = BTreeMap::new();
        for p in paths {
            let src = std::fs::read_to_string(&p).unwrap();
            let mut on = false;
            for line in src.lines() {
                if line.to_ascii_lowercase().starts_with("//! | effect | class |") {
                    on = true;
                    continue;
                }
                if !on {
                    continue;
                }
                let Some(row) = line.strip_prefix("//! |") else {
                    on = false;
                    continue;
                };
                if row.starts_with("---") {
                    continue;
                }
                let cells: Vec<&str> = row.split(" | ").collect();
                let (Some(effect), Some(class)) = (cells.first(), cells.get(1)) else { continue };
                let Some(name) = effect.split('`').nth(1) else { continue };
                let word: String = class.trim().chars().take_while(|c| c.is_ascii_alphabetic()).collect();
                out.insert(name.to_string(), (word, p.file_name().unwrap().to_string_lossy().into_owned()));
            }
        }
        out
    }

    fn name(c: EffectClass) -> &'static str {
        match c {
            Store => "Store",
            Unit => "Unit",
            Git => "Git",
            Forge => "Forge",
            Host => "Host",
            Container => "Container",
        }
    }

    #[test]
    fn every_registered_effect_names_a_class() {
        let tables = act_tables();
        let missing: Vec<String> = registered()
            .into_iter()
            .filter(|n| class_of(n).is_none())
            .map(|n| {
                let guess = tables.get(n).and_then(|(w, _)| EffectClass::parse(w)).map_or("/* no Act table row */ Host", name);
                format!("    ({n:?}, {guess}),")
            })
            .collect();
        assert!(missing.is_empty(), "effects with no class row; add them to CLASSES:\n{}", missing.join("\n"));
    }

    #[test]
    fn every_act_table_row_agrees_with_the_class_table() {
        let tables = act_tables();
        assert!(tables.len() >= 100, "the source scan must find the Act tables: {}", tables.len());
        let mut wrong = vec![];
        for (n, c) in CLASSES {
            if let Some((word, file)) = tables.get(*n) {
                if EffectClass::parse(word) != Some(*c) {
                    wrong.push(format!("{n}: CLASSES says {}, {file}'s Act table says {word:?}", c.name()));
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn the_class_table_names_only_registered_effects_once() {
        let reg = registered();
        let mut seen = std::collections::BTreeSet::new();
        for (n, _) in CLASSES {
            assert!(seen.insert(*n), "{n} appears twice");
            assert!(reg.contains(n), "{n} is in CLASSES but no booted task registers it");
        }
    }

    /// The contract's own example: a dwarf start is a unit verb, its metric a host write.
    #[test]
    fn sentinel_call_runs_unit_start_as_unit_and_metric_as_host() {
        use crate::tasks::a::sentinel_call::{METRIC, UNIT_START};
        assert_eq!(class_of(UNIT_START), Some(Unit));
        assert_eq!(class_of(METRIC), Some(Host));
        assert_eq!(class_of(runtime::units::UNIT_START), Some(Unit));
    }
}
