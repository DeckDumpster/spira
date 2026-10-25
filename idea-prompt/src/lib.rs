//! The idle detector behind the parked-ideas prompt. Pure decision over injected ports: the
//! clock, the busy probe, the idea list and the mail door come in through [`Ports`].

pub struct Idea {
    pub id: String,
    pub title: String,
}

pub struct Config {
    pub idle_secs: u64,
    pub wake_from: u32,
    pub wake_until: u32,
    pub min_gap_secs: u64,
}

/// What the previous pass left behind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    pub last_busy: u64,
    pub last_prompt: u64,
    /// A prompt went out since Spira was last busy; only busy-then-idle again re-arms it.
    pub prompted: bool,
}

pub trait Ports {
    fn now(&self) -> u64;
    /// Local hour (0-23) in the operator's zone at `now`.
    fn local_hour(&self, now: u64) -> Result<u32, String>;
    /// Any READY, REWORK or WORKING work bead, or any open round.
    fn busy(&self) -> Result<bool, String>;
    /// Open ideas, newest first.
    fn ideas(&self) -> Result<Vec<Idea>, String>;
    fn send(&self, subject: &str, default: &str, body: &str) -> Result<(), String>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Busy,
    NotIdleLongEnough,
    AlreadyPrompted,
    TooSoon,
    OutsideWakingHours,
    NoIdeas,
    Prompted(usize),
}

pub fn parse_state(text: &str) -> State {
    let mut s = State::default();
    for line in text.lines() {
        match line.split_once('=') {
            Some(("last_busy", v)) => s.last_busy = v.trim().parse().unwrap_or(0),
            Some(("last_prompt", v)) => s.last_prompt = v.trim().parse().unwrap_or(0),
            Some(("prompted", v)) => s.prompted = v.trim() == "1",
            _ => {}
        }
    }
    s
}

pub fn render_state(s: &State) -> String {
    format!("last_busy={}\nlast_prompt={}\nprompted={}\n", s.last_busy, s.last_prompt, u8::from(s.prompted))
}

pub fn in_waking_hours(hour: u32, cfg: &Config) -> bool {
    hour >= cfg.wake_from && hour < cfg.wake_until
}

pub fn render_body(ideas: &[Idea]) -> String {
    let mut out = String::from("Spira has been idle. These ideas are parked, newest first:\n\n");
    for (i, idea) in ideas.iter().enumerate() {
        out.push_str(&format!("{}. {} — {}\n", i + 1, idea.id, idea.title));
    }
    out.push_str("\nReply with the number of the one to start and the Concierge opens a design review for it. \"not now\" (or no answer) leaves them parked until Spira has been busy and gone idle again.\n");
    out
}

/// One pass. The state is always updated in `state`; the caller persists it.
pub fn pass(ports: &dyn Ports, cfg: &Config, state: &mut State) -> Result<Outcome, String> {
    let now = ports.now();
    if ports.busy()? {
        state.last_busy = now;
        state.prompted = false;
        return Ok(Outcome::Busy);
    }
    if state.last_busy == 0 {
        state.last_busy = now;
    }
    if now.saturating_sub(state.last_busy) < cfg.idle_secs {
        return Ok(Outcome::NotIdleLongEnough);
    }
    if state.prompted {
        return Ok(Outcome::AlreadyPrompted);
    }
    if state.last_prompt != 0 && now.saturating_sub(state.last_prompt) < cfg.min_gap_secs {
        return Ok(Outcome::TooSoon);
    }
    if !in_waking_hours(ports.local_hour(now)?, cfg) {
        return Ok(Outcome::OutsideWakingHours);
    }
    let ideas = ports.ideas()?;
    if ideas.is_empty() {
        return Ok(Outcome::NoIdeas);
    }
    ports.send(
        &format!("{} parked idea(s) — start any?", ideas.len()),
        "not now",
        &render_body(&ideas),
    )?;
    state.prompted = true;
    state.last_prompt = now;
    Ok(Outcome::Prompted(ideas.len()))
}

#[cfg(test)]
mod tests;
