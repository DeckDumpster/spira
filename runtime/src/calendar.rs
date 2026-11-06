//! `OnCalendar` specs (S8): the subset the task catalogue uses, and the next time one elapses.
//! Times are UTC milliseconds since the epoch.

use std::str::FromStr;

const DAY_S: u64 = 86_400;
const WEEKDAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Calendar {
    /// Monday is 0; `None` is every day.
    dows: Option<Vec<u64>>,
    hours: Vec<u64>,
    minutes: Vec<u64>,
    seconds: Vec<u64>,
}

/// `*`, or a comma list of `N` and `N/STEP` (N, then every STEP after it).
fn field(f: &str, max: u64, what: &str) -> Result<Vec<u64>, String> {
    let bad = || format!("unsupported {what} field {f:?}");
    if f == "*" {
        return Ok((0..max).collect());
    }
    let mut out = Vec::new();
    for item in f.split(',') {
        let (start, step) = match item.split_once('/') {
            Some((s, st)) => (s, Some(st.parse::<u64>().map_err(|_| bad())?)),
            None => (item, None),
        };
        let start: u64 = start.parse().map_err(|_| bad())?;
        if start >= max || step == Some(0) {
            return Err(bad());
        }
        match step {
            Some(st) => out.extend((start..max).step_by(st as usize)),
            None => out.push(start),
        }
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

fn weekday_of(day: u64) -> u64 {
    (day + 3) % 7
}

impl Calendar {
    pub fn parse(spec: &str) -> Result<Calendar, String> {
        match spec.trim() {
            "daily" => return Calendar::parse("*-*-* 00:00:00"),
            "weekly" => return Calendar::parse("Mon *-*-* 00:00:00"),
            _ => {}
        }
        let unsupported = || format!("unsupported OnCalendar={spec:?}");
        let mut parts: Vec<&str> = spec.split_whitespace().collect();
        let mut dows = None;
        if parts.len() == 3 {
            let d = parts[0]
                .split(',')
                .map(|n| WEEKDAYS.iter().position(|x| n.eq_ignore_ascii_case(x)).map(|i| i as u64).ok_or_else(|| format!("unsupported weekday {n:?} in OnCalendar={spec:?}")))
                .collect::<Result<Vec<u64>, String>>()?;
            dows = Some(d);
            parts.remove(0);
        }
        if parts.len() != 2 || parts[0] != "*-*-*" {
            return Err(unsupported());
        }
        let t: Vec<&str> = parts[1].split(':').collect();
        if !(2..=3).contains(&t.len()) {
            return Err(unsupported());
        }
        Ok(Calendar {
            dows,
            hours: field(t[0], 24, "hour")?,
            minutes: field(t[1], 60, "minute")?,
            seconds: field(t.get(2).copied().unwrap_or("0"), 60, "second")?,
        })
    }

    /// The first elapse at or after `ms`.
    pub fn at_or_after(&self, ms: u64) -> u64 {
        let s0 = ms.div_ceil(1000);
        let mut day = s0 / DAY_S;
        loop {
            if self.dows.as_ref().is_none_or(|d| d.contains(&weekday_of(day))) {
                for h in &self.hours {
                    for m in &self.minutes {
                        for s in &self.seconds {
                            let abs = day * DAY_S + h * 3600 + m * 60 + s;
                            if abs >= s0 {
                                return abs * 1000;
                            }
                        }
                    }
                }
            }
            day += 1;
        }
    }

    /// The first elapse strictly after `now_ms`.
    pub fn next_after(&self, now_ms: u64) -> u64 {
        self.at_or_after(now_ms.saturating_add(1))
    }
}

impl FromStr for Calendar {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Calendar::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UTC `y-m-d h:m:s` as epoch milliseconds.
    fn at(y: i64, mo: i64, d: i64, h: u64, mi: u64, s: u64) -> u64 {
        let (y, mo) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * mo + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = (era * 146_097 + doe - 719_468) as u64;
        (days * DAY_S + h * 3600 + mi * 60 + s) * 1000
    }

    #[test]
    fn the_date_helper_agrees_with_the_epoch() {
        assert_eq!(at(1970, 1, 1, 0, 0, 0), 0);
        assert_eq!(at(2026, 11, 3, 0, 0, 0), 1_793_664_000_000);
        assert_eq!(weekday_of(at(2026, 11, 3, 0, 0, 0) / 1000 / DAY_S), 1, "2026-11-03 is a Tuesday");
    }

    #[test]
    fn every_catalogue_spec_parses_and_elapses_next_where_systemd_would() {
        let now = at(2026, 11, 3, 10, 7, 0);
        let table = [
            ("*-*-* *:*:00", at(2026, 11, 3, 10, 8, 0)),
            ("*-*-* *:07:00", at(2026, 11, 3, 11, 7, 0)),
            ("*-*-* *:07,37:00", at(2026, 11, 3, 10, 37, 0)),
            ("*-*-* *:20:00", at(2026, 11, 3, 10, 20, 0)),
            ("*-*-* *:35:00", at(2026, 11, 3, 10, 35, 0)),
            ("*-*-* *:0/5:00", at(2026, 11, 3, 10, 10, 0)),
            ("*-*-* 04:20:00", at(2026, 11, 4, 4, 20, 0)),
            ("*-*-* 06:30:00", at(2026, 11, 4, 6, 30, 0)),
            ("Mon *-*-* 06:15:00", at(2026, 11, 9, 6, 15, 0)),
            ("weekly", at(2026, 11, 9, 0, 0, 0)),
            ("daily", at(2026, 11, 4, 0, 0, 0)),
        ];
        for (spec, want) in table {
            let c = Calendar::parse(spec).unwrap_or_else(|e| panic!("{spec}: {e}"));
            assert_eq!(c.next_after(now), want, "{spec}");
        }
    }

    #[test]
    fn next_after_is_strict_and_steps_through_milliseconds() {
        let c = Calendar::parse("*-*-* *:07:00").unwrap();
        assert_eq!(c.next_after(at(2026, 11, 3, 10, 6, 59) + 999), at(2026, 11, 3, 10, 7, 0));
        assert_eq!(c.next_after(at(2026, 11, 3, 10, 7, 0)), at(2026, 11, 3, 11, 7, 0));
        assert_eq!(c.at_or_after(at(2026, 11, 3, 10, 7, 0)), at(2026, 11, 3, 10, 7, 0));
        let every_five = Calendar::parse("*-*-* *:0/5:00").unwrap();
        assert_eq!(every_five.next_after(at(2026, 11, 3, 23, 55, 0)), at(2026, 11, 4, 0, 0, 0));
        let weekly = Calendar::parse("weekly").unwrap();
        assert_eq!(weekly.next_after(at(2026, 11, 9, 0, 0, 0)), at(2026, 11, 16, 0, 0, 0));
    }

    #[test]
    fn a_step_field_lists_every_step_from_its_start() {
        assert_eq!(Calendar::parse("*-*-* *:0/5:00").unwrap().minutes, (0..60).step_by(5).collect::<Vec<_>>());
        assert_eq!(Calendar::parse("*-*-* *:7/30:00").unwrap().minutes, vec![7, 37]);
        assert_eq!(Calendar::parse("*-*-* *:07,37:00").unwrap(), Calendar::parse("*-*-* *:7/30:00").unwrap());
    }

    #[test]
    fn specs_outside_the_subset_are_refused() {
        for spec in ["", "monthly", "*-*-* 25:00:00", "*-*-* *:60:00", "*-*-* *:0/0:00", "Fun *-*-* 01:00:00", "2026-*-* 01:00:00", "*-*-* 01", "*-*-* 1:2:3:4", "*-*-* *:a:00"] {
            assert!(Calendar::parse(spec).is_err(), "{spec:?} parsed");
        }
    }
}
