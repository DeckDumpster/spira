//! The account's two rate-limit windows: the samples file, the burn rate across its span, and
//! the `SP_LIMIT_*` lines. Samples are account-wide, so `env` answers from the newest any
//! session wrote.

use std::path::Path;

use crate::fmt;
use crate::files::write_by_rename;

const KEEP_SECS: i64 = 21600;
const MIN_SPAN_SECS: i64 = 600;
const MIN_MOVE_POINTS: f64 = 2.0;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Win {
    pub pct: f64,
    pub resets_at: i64,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Sample {
    pub at: i64,
    pub five: Option<Win>,
    pub seven: Option<Win>,
}

impl Sample {
    pub fn win(&self, idx: usize) -> Option<Win> {
        if idx == 0 {
            self.five
        } else {
            self.seven
        }
    }
    fn same_reading(&self, o: &Sample) -> bool {
        self.five == o.five && self.seven == o.seven
    }
}

fn win_text(w: Option<Win>) -> String {
    match w {
        None => "- -".into(),
        Some(w) => format!("{} {}", fmt::g(w.pct), w.resets_at),
    }
}

fn parse_line(line: &str) -> Option<Sample> {
    let f: Vec<&str> = line.split_whitespace().collect();
    if f.len() != 5 {
        return None;
    }
    let win = |a: &str, b: &str| -> Option<Option<Win>> {
        if a == "-" && b == "-" {
            return Some(None);
        }
        Some(Some(Win { pct: a.parse().ok()?, resets_at: b.parse().ok()? }))
    };
    Some(Sample { at: f[0].parse().ok()?, five: win(f[1], f[2])?, seven: win(f[3], f[4])? })
}

/// The samples oldest-first, and whether the file is unreadable. Absent is not broken: it is
/// "no sample yet". One line that does not parse refuses the whole file.
pub fn load(path: &Path) -> (Vec<Sample>, bool) {
    let text = match std::fs::read(path) {
        Ok(b) => match String::from_utf8(b) {
            Ok(t) => t,
            Err(_) => return (vec![], true),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (vec![], false),
        Err(_) => return (vec![], true),
    };
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match parse_line(line) {
            Some(s) => out.push(s),
            None => return (vec![], true),
        }
    }
    out.sort_by_key(|s| s.at);
    (out, false)
}

/// Prune to six hours, append, collapse a run of identical readings to its two endpoints, write
/// by rename. Returns the list as it now stands.
pub fn record(path: &Path, samples: &[Sample], sample: Sample) -> Vec<Sample> {
    let mut keep: Vec<Sample> = samples.iter().copied().filter(|s| sample.at - s.at <= KEEP_SECS).collect();
    let n = keep.len();
    if n >= 2 && keep[n - 1].same_reading(&sample) && keep[n - 2].same_reading(&sample) {
        keep[n - 1] = sample;
    } else {
        keep.push(sample);
    }
    keep.sort_by_key(|s| s.at);
    let body: String = keep.iter().map(|s| format!("{} {} {}\n", s.at, win_text(s.five), win_text(s.seven))).collect();
    write_by_rename(path, &body);
    keep
}

/// Percent per minute across the retained span of one window since its last reset; `None` when
/// unmeasurable. Never a pairwise slope: the percentage is rounded to a point.
pub fn rate(samples: &[Sample], idx: usize) -> Option<f64> {
    let pts: Vec<(i64, f64)> = samples.iter().filter_map(|s| s.win(idx).map(|w| (s.at, w.pct))).collect();
    if pts.len() < 2 {
        return None;
    }
    let mut start = 0;
    for i in 1..pts.len() {
        if pts[i].1 < pts[i - 1].1 - 1.0 {
            start = i;
        }
    }
    let pts = &pts[start..];
    if pts.len() < 2 {
        return None;
    }
    let span = pts[pts.len() - 1].0 - pts[0].0;
    let movement = pts[pts.len() - 1].1 - pts[0].1;
    if span < MIN_SPAN_SECS || movement < MIN_MOVE_POINTS {
        return None;
    }
    Some(movement / (span as f64 / 60.0))
}

/// The five `SP_LIMIT_*` lines. `?` is a probe that broke, `-` one that found nothing; neither is 0.
pub fn env_lines(samples: &[Sample], broken: bool, now: i64) -> Vec<String> {
    let keys = ["5H_PCT", "5H_ETA", "7D_PCT", "7D_ETA", "AGE"];
    if broken || samples.is_empty() {
        let v = if broken { "?" } else { "-" };
        return keys.iter().map(|k| format!("SP_LIMIT_{k}={v}")).collect();
    }
    let newest = samples[samples.len() - 1];
    let mut out = Vec::new();
    for (idx, short) in [(0, "5H"), (1, "7D")] {
        let Some(w) = newest.win(idx) else {
            out.push(format!("SP_LIMIT_{short}_PCT=-"));
            out.push(format!("SP_LIMIT_{short}_ETA=-"));
            continue;
        };
        out.push(format!("SP_LIMIT_{short}_PCT={}", fmt::g(w.pct)));
        let eta = match rate(samples, idx) {
            _ if w.pct >= 100.0 => "0".to_string(),
            Some(r) if r > 0.0 => format!("{}", ((100.0 - w.pct) / r * 60.0) as i64),
            _ => "-".to_string(),
        };
        out.push(format!("SP_LIMIT_{short}_ETA={eta}"));
    }
    out.push(format!("SP_LIMIT_AGE={}", (now - newest.at).max(0)));
    out
}

const WARN: f64 = 70.0;
const HIGH: f64 = 90.0;

/// The status line's segment for the windows the hook carried this instant.
pub fn segment(now_sample: &Sample, samples: &[Sample], now: i64) -> String {
    let mut out = String::new();
    for idx in [0, 1] {
        let Some(w) = now_sample.win(idx) else { continue };
        let col = if w.pct < WARN { "\x1b[32m" } else if w.pct < HIGH { "\x1b[33m" } else { "\x1b[31m" };
        let ttr = (w.resets_at - now) as f64 / 60.0;
        let mut clause = String::new();
        if w.pct < 100.0 {
            if let Some(r) = rate(samples, idx).filter(|r| *r > 0.0) {
                let eta = (100.0 - w.pct) / r;
                if ttr <= 0.0 || eta < ttr {
                    let cc = if eta < 60.0 { "\x1b[31m" } else { "\x1b[2m" };
                    clause = format!(" \x1b[2m↑{:.1}%/h\x1b[0m {cc}full in ~{}\x1b[0m", r * 60.0, fmt::dur(eta));
                }
            }
        }
        out.push_str(&format!(" \x1b[2m·\x1b[0m \x1b[2m{}\x1b[0m {col}{}%\x1b[0m{clause}", fmt::dur(ttr), fmt::g(w.pct)));
    }
    out
}
