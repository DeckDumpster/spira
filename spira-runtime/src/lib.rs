//! The service around the `runtime` crate: its pinned settings, the effect runner, the
//! systemd notifier the dispatch loop kicks, and the OTLP log sink for wide events.

pub mod forge;
pub mod hunk;
pub mod notify;
pub mod reconcile_host;
pub mod settings;
pub mod sink;
pub mod wake;
pub mod watchers;
pub mod wiki_git;
pub mod world;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use runtime::heartbeat::{Heartbeat, PING};
use runtime::host::EffectRunner;
use runtime::{Engine, EffectSpec, Journal, Marker, Registration, Value, WideSink};

pub use settings::Settings;

/// No registration here consumes events other writers append, so the journal is read for them
/// this often rather than on every wake.
const FOREIGN_POLL_MS: u64 = 30_000;

/// Every workflow the service hosts. The heartbeat is registered here and nowhere else.
pub fn registrations(s: &Settings) -> Vec<Registration> {
    vec![Heartbeat { interval_ms: s.heartbeat_secs * 1000, ping_deadline_ms: s.ping_deadline_ms() }.registration(), s.round_duty.clone().registration(), s.inbox_keeper.clone().registration(), s.pr_notify.clone().registration(), s.asks.clone().registration(), s.design_asks.clone().registration(), s.brain_dirty.clone().registration(), s.supersede_duty.clone().registration(), s.bead_sweep.clone().registration(), s.publish_backlog.clone().registration(), s.repo_events.clone().registration(), s.overrides.clone().registration(), s.view.clone().registration(), s.reconciler.clone().registration(), s.reconciler_flow.clone().registration()]
}

pub fn open_engine(s: &Settings, journal: Arc<dyn Journal>, clock: Arc<dyn runtime::ports::Clock>, marker: Arc<dyn Marker>, sink: Arc<dyn WideSink>, extra: Vec<Registration>) -> runtime::Result<Engine> {
    let mut regs = registrations(s);
    regs.extend(extra);
    Engine::open(journal, clock, regs, 0)?
        .with_sink(sink)
        .with_baseline(vec![("runtime.version".into(), Value::from(env!("CARGO_PKG_VERSION"))), ("config.hash".into(), Value::from(s.hash()))])
        .with_foreign_poll(FOREIGN_POLL_MS)
        .with_stall_limit(s.stall_limit)
        .with_marker(marker)
}

/// Runs the effects the service's workflows ask for. HTTP goes to the heartbeat URL and the forge; the watchers read files, the lifecycle socket and (brain-dirty) a bounded git on the wiki checkout, and the operator's mail is delivered in-process.
#[derive(Default)]
pub struct Effects {
    pub heartbeat_url: String,
    pub world: world::World,
    pub forge: forge::Forge,
    pub mail: Option<Arc<mail::env::Env>>,
    pub reconciler: Option<watchers::reconciler::Reconciler>,
    pub landrefs: Arc<reconcile_host::Landrefs>,
    /// What the reconciler has asked git, by the shas that decided the answer.
    pub memo: Arc<std::sync::Mutex<std::collections::BTreeMap<String, String>>>,
    /// The programs the reconciler launches and the bus it reads, when not the installed ones.
    pub seams: Option<reconciler::Seams>,
    pub bus: Option<String>,
    pub enabled: OnceLock<String>,
    pub reconciling: AtomicBool,
    pub concierge_mail: Option<Arc<mail::env::Env>>,
}

/// The mail environment operator notes are sent under: no bd (a note needs no tracking bead, and an empty db keeps the mailer from launching one), and the repeat guard overridden because consecutive merges normalise to one subject.
pub fn operator_mail(mut env: mail::env::Env) -> mail::env::Env {
    env.db.clear();
    env.repeat_considered = Some("pr-notify deduplicates by merge sha; subjects differ only in PR number and sha".into());
    env
}

/// The mail environment the Concierge's alerts are sent under: no bd, as for a note, and the repeat guard left on.
pub fn concierge_mail(mut env: mail::env::Env) -> mail::env::Env {
    env.db.clear();
    env
}

impl Effects {
    fn concierge_note(&self, input: &str) -> Result<String, String> {
        let env = self.concierge_mail.as_deref().ok_or("no concierge mail environment configured")?;
        let note: watchers::reconciler_flow::Mail = serde_json::from_str(input).map_err(|e| e.to_string())?;
        let args = mail::cmds::SendArgs { mailbox: "concierge", from: Some(reconciler_engine::mail::FROM), subject: &note.subject, kind: "note", default: "", class: "", bead: "", urgent: false, digest: false, dry_run: false, requested_by: "" };
        let bd = mail::bead::BdCli { bin: String::new(), db: String::new(), conn_retries: 1 };
        mail::cmds::send(&bd, env, &args, note.body).map(|o| o.delivered_path.display().to_string()).inspect_err(|_| eprintln!("reconciler-flow: concierge alert failed for: {}", note.subject))
    }

    fn operator_note(&self, input: &str) -> Result<String, String> {
        let env = self.mail.as_deref().ok_or("no mail environment configured")?;
        let note: watchers::pr_notify::Note = serde_json::from_str(input).map_err(|e| e.to_string())?;
        let args = mail::cmds::SendArgs { mailbox: "operator", from: Some("PR Notify <pr-notify@spira>"), subject: &note.subject, kind: "note", default: "", class: "", bead: "", urgent: false, digest: true, dry_run: false, requested_by: "" };
        let bd = mail::bead::BdCli { bdq: String::new(), conn_retries: 1 };
        mail::cmds::send(&bd, env, &args, format!("## Note\n{}\n", note.body)).map(|o| o.delivered_path.display().to_string()).inspect_err(|_| eprintln!("pr-notify: operator mail failed for: {}", note.subject))
    }
}

/// Marks a pass running and frees the mark on drop, so two passes never overlap.
struct Passing<'a>(&'a AtomicBool);

impl Drop for Passing<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Entries older than this are dropped from the effect ledger.
const LEDGER_KEEP_SECS: u64 = 86_400;

impl Effects {
    /// One structural pass: the reads, in this process, as a serialised `Observation`.
    fn reconcile_observe(&self, input: &str) -> Result<String, String> {
        let r = self.reconciler.as_ref().ok_or("no reconciler configured")?;
        let input: watchers::reconciler::ObserveIn = serde_json::from_str(input).map_err(|e| e.to_string())?;
        if self.reconciling.swap(true, Ordering::SeqCst) {
            return Err("a reconciler pass is still running".into());
        }
        let _passing = Passing(&self.reconciling);
        let at = input.at / 1000;
        let run = self.world.run_dir.clone();
        let instance = r.values.get("SPIRA_INSTANCE").cloned().unwrap_or_default();
        let enabled = self.enabled.get_or_init(|| install::bootstrap::manifest_from_env(&instance).map(|m| m.enable(&instance).join("\n")).unwrap_or_default()).clone();
        let min_age = r.values.get("SPIRA_STRANDED_RUNNER_MIN_AGE").and_then(|v| v.trim().parse().ok()).unwrap_or(0);
        let mut host = reconcile_host::RuntimeHost::new(self.world.clone(), self.forge.clone(), self.mail.clone(), r.home.clone().into(), r.values.get("SPIRA_RUNES").cloned().unwrap_or_default(), min_age, at, enabled, self.landrefs.clone());
        host.bus = self.bus.clone();
        let mut cfg = reconciler::Config::from_lookup(&|k| r.values.get(k).cloned().ok_or_else(|| format!("{k} unknown")), self.seams.clone().unwrap_or_else(reconciler::Seams::production), run, r.home.clone(), at, reconciler_engine::clock::iso_of(at))?.with_desired_state().with_host(Arc::new(host));
        cfg = cfg.with_memo(self.memo.clone());
        cfg.skip_units = vec![spira_config::unit::unit_name("spira-runtime", "service", &instance)];
        let observation = reconciler::observe(&cfg, &input.reminded);
        serde_json::to_string(&observation).map_err(|e| e.to_string())
    }

    /// Runs `act` once per effect key: a restart re-issues an effect that has no outcome, and a
    /// remedy or a mail must not go twice for it.
    fn once(&self, key: &str, act: impl FnOnce() -> Result<String, String>) -> Result<String, String> {
        let dir = self.world.run_dir.join("reconciler-effects.d");
        let safe: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
        if dir.join(&safe).exists() {
            return Ok(format!("effect {key} already ran"));
        }
        let out = act()?;
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(&safe), "");
        let old = std::time::SystemTime::now() - Duration::from_secs(LEDGER_KEEP_SECS);
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            if e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t < old) {
                let _ = std::fs::remove_file(e.path());
            }
        }
        Ok(out)
    }

    fn log_line(&self, line: &str) {
        use std::io::Write;
        let stamp = reconciler_engine::clock::iso_of(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()));
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.world.run_dir.join("reconciler.log")) {
            let _ = writeln!(f, "{stamp} spira: {line}");
        }
    }

    fn reconcile_record(&self, input: &str) -> Result<String, String> {
        let r = self.reconciler.as_ref().ok_or("no reconciler configured")?;
        let rec: watchers::reconciler::Record = serde_json::from_str(input).map_err(|e| e.to_string())?;
        let endpoint = r.values.get("SPIRA_METRICS_ENDPOINT").cloned().filter(|e| !e.trim().is_empty());
        let store = tsd::store::Pusher::new(endpoint);
        let now_iso = reconciler_engine::clock::iso_of(rec.at / 1000);
        let release = reconciler::release_of(&r.home);
        for row in &rec.status {
            reconciler_engine::io::push_status(&store, &now_iso, row);
        }
        for e in &rec.events {
            reconciler_engine::effect::append_event(&store, &reconciler_engine::effect::RemedyEvent { ts: &now_iso, event: &e.event, kind: reconciler_engine::effect::kind_of(&e.key), key: &e.key, action: &e.action, metric: "gap", before: e.before, after: e.after, release: &release });
        }
        rec.log.iter().for_each(|l| self.log_line(l));
        Ok(format!("{} status, {} events", rec.status.len(), rec.events.len()))
    }

    fn reconcile_remedy(&self, key: &str, input: &str) -> Result<String, String> {
        let job: watchers::reconciler::RemedyJob = serde_json::from_str(input).map_err(|e| e.to_string())?;
        self.once(key, || {
            for (program, args) in &job.steps {
                reconciler::run_cmd_ok(program, &args.iter().map(String::as_str).collect::<Vec<_>>());
            }
            Ok(format!("{}: {} step(s)", job.key, job.steps.len()))
        })
    }

    fn reconcile_mail(&self, key: &str, input: &str) -> Result<String, String> {
        let job: watchers::reconciler::MailJob = serde_json::from_str(input).map_err(|e| e.to_string())?;
        self.once(key, || {
            reconcile_host::send_mail(self.mail.as_deref(), &job.mail).inspect_err(|e| self.log_line(&format!("reconciler: {} failed: {e}", job.what)))?;
            if job.remind.is_some() {
                self.log_line(&format!("reconciler: {}", job.what));
            }
            Ok(job.what.clone())
        })
    }
}

impl EffectRunner for Effects {
    fn run(&self, key: &str, spec: &EffectSpec) -> Result<String, String> {
        match spec.name.as_str() {
            PING if self.heartbeat_url.is_empty() => Ok("no heartbeat url configured".into()),
            PING => {
                let r = ureq::AgentBuilder::new().timeout(Duration::from_millis(spec.deadline_ms)).build().get(&self.heartbeat_url).call().map_err(|e| e.to_string())?;
                Ok(format!("status {}", r.status()))
            }
            watchers::round_duty::OBSERVE => self.world.round_snapshot(&spec.input),
            watchers::inbox_keeper::OBSERVE => self.world.keeper_snapshot(),
            watchers::CONCIERGE_WAKE => self.world.home_path().and_then(|home| wake::concierge_wake(&home.join("concierge.sh"), &spec.input)),
            watchers::pr_notify::OBSERVE => self.world.pr_snapshot(&self.forge, &spec.input),
            watchers::pr_notify::OPERATOR_NOTE => self.operator_note(&spec.input),
            watchers::asks::OBSERVE => self.world.asks_snapshot(&spec.input),
            watchers::design_asks::OBSERVE => self.world.design_asks_snapshot(),
            watchers::brain_dirty::OBSERVE => self.world.brain_dirty_snapshot(),
            watchers::supersede_duty::OBSERVE => self.world.supersede_snapshot(&spec.input),
            watchers::supersede_duty::ACT => self.world.supersede_act(&spec.input),
            watchers::bead_sweep::OBSERVE => self.world.sweep_snapshot(),
            watchers::bead_sweep::REPAIR => self.world.sweep_repair(&spec.input),
            watchers::publish_backlog::OBSERVE => self.world.publish_backlog_snapshot(),
            watchers::repo_events::OBSERVE => self.world.repo_events_snapshot(&self.forge, &spec.input),
            watchers::overrides::APPLY => self.world.overrides_apply(),
            watchers::reconciler_flow::OBSERVE => self.world.flow_snapshot(&spec.input),
            watchers::reconciler_flow::STATUS => self.world.flow_status(&spec.input),
            watchers::reconciler_flow::MAIL => self.concierge_note(&spec.input),
            watchers::view::OBSERVE => self.world.view_snapshot(&spec.input),
            watchers::view::FLIP => self.world.view_flip(&spec.input),
            watchers::view::FORGET => self.world.view_forget(&spec.input),
            watchers::reconciler::OBSERVE => self.reconcile_observe(&spec.input),
            watchers::reconciler::RECORD => self.reconcile_record(&spec.input),
            watchers::reconciler::REMEDY => self.reconcile_remedy(key, &spec.input),
            watchers::reconciler::MAIL => self.reconcile_mail(key, &spec.input),
            watchers::INBOX_APPEND => self.world.inbox_append(&spec.input),
            other => Err(format!("no runner for effect {other}")),
        }
    }
}
