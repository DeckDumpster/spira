//! The service around the `runtime` crate: its pinned settings, the effect runner, the
//! systemd notifier the dispatch loop kicks, and the OTLP log sink for wide events.

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

/// Every workflow the service hosts. The heartbeat is registered here and nowhere else.
pub fn registrations(s: &Settings) -> Vec<Registration> {
    vec![Heartbeat { interval_ms: s.heartbeat_secs * 1000, ping_deadline_ms: s.ping_deadline_ms() }.registration(), s.round_duty.clone().registration(), s.inbox_keeper.clone().registration()]
}

pub fn open_engine(s: &Settings, journal: Arc<dyn Journal>, clock: Arc<dyn runtime::ports::Clock>, marker: Arc<dyn Marker>, sink: Arc<dyn WideSink>, extra: Vec<Registration>) -> runtime::Result<Engine> {
    let mut regs = registrations(s);
    regs.extend(extra);
    Engine::open(journal, clock, regs, 0)?
        .with_sink(sink)
        .with_baseline(vec![("runtime.version".into(), Value::from(env!("CARGO_PKG_VERSION"))), ("config.hash".into(), Value::from(s.hash()))])
        .with_stall_limit(s.stall_limit)
        .with_marker(marker)
}

/// Runs the effects the service's workflows ask for. The only HTTP call is the heartbeat ping; the watchers read files and the lifecycle socket.
#[derive(Default)]
pub struct Effects {
    pub heartbeat_url: String,
    pub world: world::World,
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
            watchers::INBOX_APPEND => self.world.inbox_append(&spec.input),
            other => Err(format!("no runner for effect {other}")),
        }
    }
}
