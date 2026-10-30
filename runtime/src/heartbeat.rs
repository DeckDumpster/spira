//! The heartbeat workflow: a memory-only timer that asks for one ping effect each time it
//! fires, but only when the engine has seen events past the ones the heartbeat itself wrote.

use crate::event::*;

pub const STREAM: &str = "runtime.heartbeat";
pub const HEARTBEAT: EventKindTag = "heartbeat";
pub const PING: &str = "heartbeat.ping";
pub const NAME: &str = "heartbeat";

pub struct Heartbeat {
    pub interval_ms: u64,
    pub ping_deadline_ms: u64,
}

impl Heartbeat {
    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    /// The first beat, handed to `Engine::start` each time the process starts.
    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("heartbeat/boot", STREAM, EventKind::new(HEARTBEAT, "0"))
    }
}

impl Handler for Heartbeat {
    fn interests(&self) -> &[EventKindTag] {
        &[HEARTBEAT]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        let own = view.pushed(&ev.stream) as u64;
        let foreign = (view.total() as u64).saturating_sub(own);
        let seen: u64 = ev.kind.body.parse().unwrap_or(0);
        let advanced = foreign > seen;
        ctx.event().set("heartbeat.foreign_events", foreign);
        ctx.event().set("heartbeat.advanced", advanced);
        let mut cmds = vec![];
        if advanced {
            cmds.push(Command::Probe(EffectSpec { name: PING.into(), input: foreign.to_string(), deadline_ms: self.ping_deadline_ms }));
        }
        cmds.push(Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(HEARTBEAT, &foreign.max(seen).to_string()) });
        cmds
    }
}
