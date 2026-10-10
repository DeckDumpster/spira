//! The screen as a state machine per bead: screened, sent_back(n), capped. Every move is an
//! event; an illegal one is refused naming the state, and nothing is written. `sift status`
//! reads the recorded events and nothing else.

use serde_json::{json, Value};

use crate::SEND_BACK_CAP;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SiftEvent {
    Screened { tip: String },
    SentBack { n: u32, tip: String, reason: String },
    Capped { tip: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiftState {
    Screened,
    SentBack(u32),
    Capped,
}

impl SiftState {
    pub fn name(self) -> String {
        match self {
            SiftState::Screened => "screened".into(),
            SiftState::SentBack(n) => format!("sent_back({n})"),
            SiftState::Capped => "capped".into(),
        }
    }
}

impl SiftEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            SiftEvent::Screened { .. } => "screened",
            SiftEvent::SentBack { .. } => "sent_back",
            SiftEvent::Capped { .. } => "capped",
        }
    }

    pub fn tip(&self) -> &str {
        match self {
            SiftEvent::Screened { tip } | SiftEvent::SentBack { tip, .. } | SiftEvent::Capped { tip } => tip,
        }
    }

    fn to(&self) -> SiftState {
        match self {
            SiftEvent::Screened { .. } => SiftState::Screened,
            SiftEvent::SentBack { n, .. } => SiftState::SentBack(*n),
            SiftEvent::Capped { .. } => SiftState::Capped,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub event: SiftEvent,
    pub at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sifted {
    pub id: String,
    pub events: Vec<Recorded>,
}

impl Sifted {
    pub fn new(id: &str) -> Sifted {
        Sifted { id: id.into(), events: Vec::new() }
    }

    pub fn state(&self) -> Option<SiftState> {
        self.events.last().map(|r| r.event.to())
    }

    /// How many times this bead has been sent back; the last `sent_back(n)` is the count.
    pub fn send_backs(&self) -> u32 {
        self.events.iter().rev().find_map(|r| if let SiftEvent::SentBack { n, .. } = r.event { Some(n) } else { None }).unwrap_or(0)
    }

    /// Whether `event` is a move from the recorded state.
    pub fn refusal(&self, event: &SiftEvent) -> Option<String> {
        let ok = match (self.state(), event) {
            (_, SiftEvent::Screened { .. }) => true,
            (Some(SiftState::Capped), SiftEvent::SentBack { .. }) => false,
            (_, SiftEvent::SentBack { n, .. }) => *n == self.send_backs() + 1 && *n <= SEND_BACK_CAP,
            (_, SiftEvent::Capped { .. }) => self.send_backs() >= SEND_BACK_CAP,
        };
        if ok {
            return None;
        }
        let from = self.state().map_or("unrecorded".to_string(), SiftState::name);
        Some(format!("{} is {from}; it cannot become {}", self.id, event.to().name()))
    }

    /// Records `event` at `at`, or refuses naming the state. Repeating the last event for the
    /// same tip is not a move.
    pub fn record(&mut self, event: SiftEvent, at: u64) -> Result<bool, String> {
        if let Some(e) = self.refusal(&event) {
            return Err(e);
        }
        let repeats = self.events.last().is_some_and(|r| matches!(event, SiftEvent::Screened { .. } | SiftEvent::Capped { .. }) && r.event == event);
        if repeats {
            return Ok(false);
        }
        self.events.push(Recorded { event, at });
        Ok(true)
    }

    fn events_json(&self) -> Value {
        Value::Array(
            self.events
                .iter()
                .map(|r| match &r.event {
                    SiftEvent::Screened { tip } => json!({"event": "screened", "tip": tip, "at": r.at}),
                    SiftEvent::SentBack { n, tip, reason } => json!({"event": "sent_back", "n": n, "tip": tip, "reason": reason, "at": r.at}),
                    SiftEvent::Capped { tip } => json!({"event": "capped", "tip": tip, "at": r.at}),
                })
                .collect(),
        )
    }

    pub fn to_json(&self) -> Value {
        json!({"id": self.id, "events": self.events_json()})
    }

    pub fn from_json(text: &str) -> Result<Sifted, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| format!("not a sift record: {e}"))?;
        let id = v.get("id").and_then(Value::as_str).ok_or("sift record has no id")?.to_string();
        let mut out = Sifted::new(&id);
        for e in v.get("events").and_then(Value::as_array).ok_or("sift record has no events")? {
            let s = |k: &str| e.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
            let event = match e.get("event").and_then(Value::as_str) {
                Some("screened") => SiftEvent::Screened { tip: s("tip") },
                Some("sent_back") => SiftEvent::SentBack { n: e.get("n").and_then(Value::as_u64).unwrap_or(0) as u32, tip: s("tip"), reason: s("reason") },
                Some("capped") => SiftEvent::Capped { tip: s("tip") },
                _ => return Err(format!("sift record of {id} has an event of an unknown kind")),
            };
            out.events.push(Recorded { event, at: e.get("at").and_then(Value::as_u64).unwrap_or(0) });
        }
        Ok(out)
    }

    /// The read: exactly the recorded state and the events behind it.
    pub fn status_json(&self) -> Value {
        let last = self.events.last();
        json!({
            "id": self.id,
            "state": self.state().map_or("unrecorded".to_string(), SiftState::name),
            "send_backs": self.send_backs(),
            "tip": last.map(|r| r.event.tip()).unwrap_or(""),
            "since": last.map_or(0, |r| r.at),
            "events": self.events_json(),
        })
    }
}
