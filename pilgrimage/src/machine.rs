//! A pilgrimage as a state machine: every move is an event, an illegal one is refused naming
//! the state, and nothing is written. The state is the last recorded event's target.

use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Waiting,
    Walking,
    DefectsFiled,
    Clean,
    ConciergeWalk,
    Done,
}

pub const STATES: [State; 6] = [State::Waiting, State::Walking, State::DefectsFiled, State::Clean, State::ConciergeWalk, State::Done];

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Waiting => "waiting",
            State::Walking => "walking",
            State::DefectsFiled => "defects-filed",
            State::Clean => "clean",
            State::ConciergeWalk => "concierge-walk",
            State::Done => "done",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Opened,
    /// Walk `n` began; `landed_at` is when the last dependency landed.
    WalkStarted { n: u32, landed_at: u64 },
    DefectsFiled { count: u32 },
    /// The filed defects are the pilgrimage's dependencies now; it waits for them to land.
    Waiting,
    Clean,
    ConciergeWalkAsked { ask: String },
    Done,
}

impl Event {
    pub fn kind(&self) -> &'static str {
        match self {
            Event::Opened => "opened",
            Event::WalkStarted { .. } => "walk_started",
            Event::DefectsFiled { .. } => "defects_filed",
            Event::Waiting => "waiting",
            Event::Clean => "clean",
            Event::ConciergeWalkAsked { .. } => "concierge_walk_asked",
            Event::Done => "done",
        }
    }

    pub fn to(&self) -> State {
        match self {
            Event::Opened | Event::Waiting => State::Waiting,
            Event::WalkStarted { .. } => State::Walking,
            Event::DefectsFiled { .. } => State::DefectsFiled,
            Event::Clean => State::Clean,
            Event::ConciergeWalkAsked { .. } => State::ConciergeWalk,
            Event::Done => State::Done,
        }
    }

    #[allow(clippy::wrong_self_convention)]
    fn from_state(&self) -> Option<State> {
        match self {
            Event::Opened => None,
            Event::WalkStarted { .. } => Some(State::Waiting),
            Event::DefectsFiled { .. } | Event::Clean => Some(State::Walking),
            Event::Waiting => Some(State::DefectsFiled),
            Event::ConciergeWalkAsked { .. } => Some(State::Clean),
            Event::Done => Some(State::ConciergeWalk),
        }
    }

    fn to_json(&self, at: u64) -> Value {
        let mut v = json!({"event": self.kind(), "at": at});
        match self {
            Event::WalkStarted { n, landed_at } => {
                v["n"] = json!(n);
                v["landed_at"] = json!(landed_at);
            }
            Event::DefectsFiled { count } => v["count"] = json!(count),
            Event::ConciergeWalkAsked { ask } => v["ask"] = json!(ask),
            _ => {}
        }
        v
    }

    fn from_json(v: &Value) -> Option<(Event, u64)> {
        let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        let event = match v.get("event")?.as_str()? {
            "opened" => Event::Opened,
            "walk_started" => Event::WalkStarted { n: n("n") as u32, landed_at: n("landed_at") },
            "defects_filed" => Event::DefectsFiled { count: n("count") as u32 },
            "waiting" => Event::Waiting,
            "clean" => Event::Clean,
            "concierge_walk_asked" => Event::ConciergeWalkAsked { ask: v.get("ask").and_then(Value::as_str).unwrap_or_default().to_string() },
            "done" => Event::Done,
            _ => return None,
        };
        Some((event, n("at")))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub event: Event,
    pub at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pilgrimage {
    pub id: String,
    pub events: Vec<Recorded>,
}

impl Pilgrimage {
    pub fn new(id: &str) -> Pilgrimage {
        Pilgrimage { id: id.into(), events: Vec::new() }
    }

    pub fn state(&self) -> Option<State> {
        self.events.last().map(|r| r.event.to())
    }

    pub fn walks(&self) -> u32 {
        self.events.iter().filter(|r| matches!(r.event, Event::WalkStarted { .. })).count() as u32
    }

    pub fn defects(&self) -> u32 {
        self.events.iter().map(|r| if let Event::DefectsFiled { count } = r.event { count } else { 0 }).sum()
    }

    pub fn landed_at(&self) -> Option<u64> {
        self.events.iter().find_map(|r| if let Event::WalkStarted { landed_at, .. } = r.event { Some(landed_at) } else { None })
    }

    pub fn clean_at(&self) -> Option<u64> {
        self.events.iter().find(|r| r.event == Event::Clean).map(|r| r.at)
    }

    pub fn ask(&self) -> Option<&str> {
        self.events.iter().find_map(|r| if let Event::ConciergeWalkAsked { ask } = &r.event { Some(ask.as_str()) } else { None })
    }

    pub fn refusal(&self, event: &Event) -> Option<String> {
        let state = self.state();
        let legal = match (event.from_state(), state) {
            (None, None) => true,
            (Some(want), Some(have)) => want == have,
            _ => false,
        };
        let numbered = match event {
            Event::WalkStarted { n, .. } => *n == self.walks() + 1,
            Event::DefectsFiled { count } => *count > 0,
            _ => true,
        };
        if legal && numbered {
            return None;
        }
        let from = state.map_or("unrecorded", State::name);
        let why = if legal { " (a walk is numbered after the last, and defects are at least one)" } else { "" };
        Some(format!("{} is {from}; it cannot take {}{why}", self.id, event.kind()))
    }

    pub fn record(&mut self, event: Event, at: u64) -> Result<(), String> {
        if let Some(e) = self.refusal(&event) {
            return Err(e);
        }
        self.events.push(Recorded { event, at });
        Ok(())
    }

    pub fn to_json(&self) -> Value {
        json!({"id": self.id, "events": self.events.iter().map(|r| r.event.to_json(r.at)).collect::<Vec<_>>()})
    }

    pub fn from_json(text: &str) -> Result<Pilgrimage, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| format!("not a pilgrimage record: {e}"))?;
        let id = v.get("id").and_then(Value::as_str).ok_or("pilgrimage record has no id")?;
        let mut out = Pilgrimage::new(id);
        for e in v.get("events").and_then(Value::as_array).ok_or("pilgrimage record has no events")? {
            let (event, at) = Event::from_json(e).ok_or_else(|| format!("pilgrimage record of {id} has an event of an unknown kind"))?;
            out.events.push(Recorded { event, at });
        }
        Ok(out)
    }

    pub fn status_json(&self) -> Value {
        json!({
            "id": self.id,
            "state": self.state().map_or("unrecorded", State::name),
            "walks": self.walks(),
            "defects": self.defects(),
            "since": self.events.last().map_or(0, |r| r.at),
            "ask": self.ask().unwrap_or(""),
            "events": self.to_json()["events"],
        })
    }
}
