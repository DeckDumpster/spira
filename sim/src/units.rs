//! Schedules read from systemd timer and service unit text. Virtual time is milliseconds.

use crate::Rng;

pub const SECOND: u64 = 1000;
const DAY_S: u64 = 86_400;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Calendar {
    dows: Option<Vec<u64>>,
    hours: Vec<u64>,
    minutes: Vec<u64>,
    seconds: Vec<u64>,
}

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

fn field(f: &str, max: u64, what: &str) -> Result<Vec<u64>, String> {
    if f == "*" {
        return Ok((0..max).collect());
    }
    f.split(',')
        .map(|x| match x.parse::<u64>() {
            Ok(n) if n < max => Ok(n),
            _ => Err(format!("unsupported {what} field {f:?}")),
        })
        .collect()
}

fn parse_calendar(v: &str) -> Result<Calendar, String> {
    let mut parts: Vec<&str> = v.split_whitespace().collect();
    let mut dows = None;
    if parts.len() == 3 {
        let names = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
        let d: Result<Vec<u64>, String> = parts[0]
            .split(',')
            .map(|n| {
                names
                    .iter()
                    .position(|x| n.to_ascii_lowercase() == *x)
                    .map(|i| i as u64)
                    .ok_or_else(|| format!("unsupported weekday {n:?} in OnCalendar={v:?}"))
            })
            .collect();
        dows = Some(d?);
        parts.remove(0);
    }
    if parts.len() != 2 || parts[0] != "*-*-*" {
        return Err(format!("unsupported OnCalendar={v:?}"));
    }
    let t: Vec<&str> = parts[1].split(':').collect();
    if !(2..=3).contains(&t.len()) {
        return Err(format!("unsupported OnCalendar={v:?}"));
    }
    Ok(Calendar {
        dows,
        hours: field(t[0], 24, "hour")?,
        minutes: field(t[1], 60, "minute")?,
        seconds: field(t.get(2).copied().unwrap_or("0"), 60, "second")?,
    })
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
            "OnCalendar" => s.calendar.push(parse_calendar(v)?),
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

fn weekday(day: u64) -> u64 {
    (day + 3) % 7
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
        for cal in &self.calendar {
            let mut day = epoch_unix_s / DAY_S;
            while (day * DAY_S).saturating_sub(epoch_unix_s) * SECOND <= horizon {
                if cal.dows.as_ref().is_none_or(|d| d.contains(&weekday(day))) {
                    for h in &cal.hours {
                        for m in &cal.minutes {
                            for s in &cal.seconds {
                                let abs = day * DAY_S + h * 3600 + m * 60 + s;
                                if abs >= epoch_unix_s && (abs - epoch_unix_s) * SECOND <= horizon {
                                    bases.push((abs - epoch_unix_s) * SECOND);
                                }
                            }
                        }
                    }
                }
                day += 1;
            }
        }
        bases.sort_unstable();
        bases
            .into_iter()
            .map(|b| b + rng.range(0, self.randomized) + rng.range(0, self.accuracy))
            .collect()
    }
}
