//! The service around the `runtime` crate: its pinned settings, the effect runner, the
//! systemd notifier the dispatch loop kicks, and the OTLP log sink for wide events.

pub mod forge;
pub mod notify;
pub mod settings;
pub mod sink;
pub mod wake;
pub mod watchers;
pub mod world;

use std::sync::Arc;
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
    vec![Heartbeat { interval_ms: s.heartbeat_secs * 1000, ping_deadline_ms: s.ping_deadline_ms() }.registration(), s.round_duty.clone().registration(), s.inbox_keeper.clone().registration(), s.pr_notify.clone().registration(), s.asks.clone().registration(), s.design_asks.clone().registration()]
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

/// Runs the effects the service's workflows ask for. HTTP goes to the heartbeat URL and the forge; the watchers read files and the lifecycle socket, and the operator's mail is delivered in-process.
#[derive(Default)]
pub struct Effects {
    pub heartbeat_url: String,
    pub world: world::World,
    pub forge: forge::Forge,
    pub mail: Option<Arc<mail::env::Env>>,
}

/// The mail environment operator notes are sent under: no bd (a note needs no tracking bead, and an empty db keeps the mailer from launching one), and the repeat guard overridden because consecutive merges normalise to one subject.
pub fn operator_mail(mut env: mail::env::Env) -> mail::env::Env {
    env.db.clear();
    env.repeat_considered = Some("pr-notify deduplicates by merge sha; subjects differ only in PR number and sha".into());
    env
}

impl Effects {
    fn operator_note(&self, input: &str) -> Result<String, String> {
        let env = self.mail.as_deref().ok_or("no mail environment configured")?;
        let note: watchers::pr_notify::Note = serde_json::from_str(input).map_err(|e| e.to_string())?;
        let args = mail::cmds::SendArgs { mailbox: "operator", from: Some("PR Notify <pr-notify@spira>"), subject: &note.subject, kind: "note", default: "", class: "", bead: "", urgent: false, digest: true, dry_run: false, requested_by: "" };
        let bd = mail::bead::BdCli { bin: String::new(), db: String::new(), conn_retries: 1 };
        mail::cmds::send(&bd, env, &args, format!("## Note\n{}\n", note.body)).map(|o| o.delivered_path.display().to_string()).inspect_err(|_| eprintln!("pr-notify: operator mail failed for: {}", note.subject))
    }
}

impl EffectRunner for Effects {
    fn run(&self, _key: &str, spec: &EffectSpec) -> Result<String, String> {
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
            watchers::INBOX_APPEND => self.world.inbox_append(&spec.input),
            other => Err(format!("no runner for effect {other}")),
        }
    }
}
