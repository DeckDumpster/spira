//! Schedules read from systemd timer and service unit text. Virtual time is milliseconds.

use std::sync::Arc;

use crate::doubles::Doubles;
use crate::Rng;
pub use runtime::calendar::Calendar;
use runtime::units::{ScriptedUnits, EFFECTS};

/// `doubles` answering every unit effect (`unit.start`, `unit.stop`, ...) from `units`.
pub fn with_units(mut doubles: Doubles, units: Arc<ScriptedUnits>) -> Doubles {
    for name in EFFECTS {
        let u = units.clone();
        doubles = doubles.double(name, move |input| runtime::units::effect(&*u, name, input));
    }
    doubles
}

pub const SECOND: u64 = 1000;
const DAY_S: u64 = 86_400;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Schedule {
    pub first: Option<u64>,
    pub interval: Option<u64>,
    pub calendar: Vec<Calendar>,
    pub randomized: u64,
    pub accuracy: u64,
}

fn section<'a>(text: &'a str, name: &str) -> Vec<(&'a str, &'a str)> {
    let mut inside = false;
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(h) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            inside = h == name;
        } else if inside {
            if let Some((k, v)) = line.split_once('=') {
                out.push((k.trim(), v.trim()));
            }
        }
    }
    out
}

/// A systemd time span: bare number is seconds; tokens add.
pub fn parse_span(v: &str) -> Result<u64, String> {
    let mut total = 0u64;
    let mut any = false;
    for tok in v.split_whitespace() {
        let split = tok.find(|c: char| !c.is_ascii_digit()).unwrap_or(tok.len());
        let (num, unit) = tok.split_at(split);
        let n: u64 = num.parse().map_err(|_| format!("bad time span {v:?}"))?;
        let mult = match unit {
            "" | "s" | "sec" | "second" | "seconds" => SECOND,
            "ms" | "msec" => 1,
            "m" | "min" | "minute" | "minutes" => 60 * SECOND,
            "h" | "hr" | "hour" | "hours" => 3600 * SECOND,
            "d" | "day" | "days" => DAY_S * SECOND,
            _ => return Err(format!("unsupported time span unit in {v:?}")),
        };
        total = total.saturating_add(n.saturating_mul(mult));
        any = true;
    }
    if any { Ok(total) } else { Err(format!("empty time span {v:?}")) }
}

/// Reads the `[Timer]` section. A timer that would never fire is an error.
pub fn parse_timer(text: &str) -> Result<Schedule, String> {
    let mut s = Schedule::default();
    let mut on_unit = false;
    for (k, v) in section(text, "Timer") {
        match k {
            "OnBootSec" | "OnStartupSec" | "OnActiveSec" => {
                let t = parse_span(v)?;
                s.first = Some(s.first.map_or(t, |f| f.min(t)));
            }
            "OnUnitActiveSec" => {
                s.interval = Some(parse_span(v)?);
                on_unit = true;
            }
            "OnUnitInactiveSec" => return Err("OnUnitInactiveSec is not modelled".into()),
            "OnCalendar" => s.calendar.push(Calendar::parse(v)?),
            "RandomizedDelaySec" => s.randomized = parse_span(v)?,
            "AccuracySec" => s.accuracy = parse_span(v)?,
            _ => {}
        }
    }
    if on_unit && s.first.is_none() {
        return Err("OnUnitActiveSec without a boot or active anchor never fires".into());
    }
    if s.first.is_none() && s.calendar.is_empty() {
        return Err("timer has no OnBootSec, OnActiveSec or OnCalendar".into());
    }
    if s.interval == Some(0) {
        return Err("OnUnitActiveSec=0".into());
    }
    Ok(s)
}

/// The service a timer starts: its `Unit=`, else the same stem.
pub fn timer_target(timer_name: &str, text: &str) -> String {
    section(text, "Timer")
        .into_iter()
        .rev()
        .find(|(k, _)| *k == "Unit")
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| format!("{}.service", timer_name.trim_end_matches(".timer")))
}

/// An actor is one pass; anything but `Type=oneshot` is not.
pub fn require_oneshot(service: &str, text: &str) -> Result<(), String> {
    let ty = section(text, "Service")
        .into_iter()
        .rev()
        .find(|(k, _)| *k == "Type")
        .map(|(_, v)| v);
    match ty {
        Some("oneshot") => Ok(()),
        other => Err(format!("{service} is Type={}, not oneshot", other.unwrap_or("simple (unset)"))),
    }
}

impl Schedule {
    /// Virtual times (ms from sim start, which is `epoch_unix_s`) at which the timer
    /// elapses within `horizon`, each delayed by seeded randomized-delay and accuracy jitter.
    pub fn fire_times(&self, epoch_unix_s: u64, horizon: u64, rng: &mut Rng) -> Vec<u64> {
        let mut bases = Vec::new();
        if let Some(first) = self.first {
            let mut t = first;
            while t <= horizon {
                bases.push(t);
                match self.interval {
                    Some(i) => t += i,
                    None => break,
                }
            }
        }
        let start = epoch_unix_s * SECOND;
        for cal in &self.calendar {
            let mut t = cal.at_or_after(start);
            while t - start <= horizon {
                bases.push(t - start);
                t = cal.next_after(t);
            }
        }
        bases.sort_unstable();
        bases
            .into_iter()
            .map(|b| b + rng.range(0, self.randomized) + rng.range(0, self.accuracy))
            .collect()
    }
}

#[cfg(test)]
mod unit_double_tests {
    use super::*;
    use runtime::host::EffectRunner;
    use runtime::ports::VirtualClock;
    use runtime::units::{Exit, UnitFacts, UnitHandle, UnitState, UnitCompletions};
    use runtime::EffectSpec;

    #[test]
    fn unit_effects_reach_the_scripted_units_by_name() {
        let clock = VirtualClock::new(0);
        let units = Arc::new(ScriptedUnits::new(Arc::new(clock.clone())));
        units.script("sweep", 5_000, Exit::Code(0));
        let d = with_units(Doubles::default(), units.clone());
        let spec = r#"{"task":"sweep","key":"k1","argv":["sweep"],"env":[],"cwd":null}"#;
        let h = d.run("e1", &EffectSpec::by_input("unit.start", spec, 1_000)).unwrap();
        assert_eq!(serde_json::from_str::<UnitHandle>(&h).unwrap(), UnitHandle::of("sweep", "k1").unwrap());
        let f = d.run("e2", &EffectSpec::by_input("unit.facts", h.clone(), 1_000)).unwrap();
        assert_eq!(serde_json::from_str::<UnitFacts>(&f).unwrap().state, UnitState::Active);
        clock.advance(5_000);
        assert_eq!(units.ran().unwrap().len(), 1);
        assert_eq!(d.asked(), vec!["unit.start", "unit.facts"]);
    }
}
