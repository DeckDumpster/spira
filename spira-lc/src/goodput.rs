//! Goodput: landed beads that did not harm the loop, as a ratio and as a rate, over the last W
//! of unhalted time. Pure: every input is a typed loop event, and nothing here reads prose
//! (law-a-matcher-reads-code-not-prose). A stop's `reason` and a landing's `subject` are carried
//! for display and never read.

use std::collections::{BTreeMap, BTreeSet};

pub const DAY_SECS: u64 = 86_400;
pub const NO_LANDINGS: &str = "no landings in window";
pub const NO_CERTIFIED_EXITS: &str = "no bead left CERTIFIED in window";

/// Only the halt has a production source yet: the stamp. Releases and landing stops arrive with
/// the typed stop family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub enum StopKind {
    /// The world halted.
    Halt,
    /// The world released from a halt.
    Release,
    /// Landing stopped on one repo; it is charged to its bead until that bead's revert lands.
    Pause,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LoopEvent {
    Landed {
        bead: String,
        repo: String,
        at: u64,
        reverts: Option<String>,
        subject: String,
    },
    RedAttributed {
        bead: String,
        batch: String,
        innocent: Vec<String>,
        at: u64,
        wall_secs: u64,
    },
    Eject {
        bead: String,
        batch: String,
        at: u64,
        repeat_n: u32,
    },
    Stop {
        kind: StopKind,
        repo: Option<String>,
        bead: Option<String>,
        at: u64,
        reason: String,
    },
    OverCapPass {
        repo: String,
        at: u64,
        wall_secs: u64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Measure {
    Value(f64),
    Unobservable(&'static str),
    WarmingUp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    LandingStop,
    Revert,
    RedWithInnocents,
    RepeatEject,
    OverCap,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Evidence {
    pub signal: Signal,
    pub at: u64,
    /// The unhalted seconds inside the window this signal charged.
    pub secs: u64,
    pub batch: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Harm {
    pub bead: String,
    /// Clipped to the window's unhalted time.
    pub cost_secs: u64,
    pub evidence: Vec<Evidence>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Goodput {
    pub window_secs: u64,
    pub landed: Measure,
    pub ratio: Measure,
    /// Landed per day × ratio.
    pub rate: Measure,
    pub harmful: Vec<Harm>,
    pub warming_until: Option<u64>,
}

/// The last `secs` of unhalted time before `now`, as the running intervals `(from, to]` it is
/// made of, newest first.
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub secs: u64,
    pub now: u64,
    pub start: u64,
    running: Vec<(u64, u64)>,
    /// While the window reaches back before the latest release from a halt of at least
    /// `secs`: when it stops, if the loop runs from now on.
    pub warming_until: Option<u64>,
}

/// The world's halts as `(halted_at, released_at)`, oldest first; an open halt runs to `now`.
pub fn halts(events: &[LoopEvent], now: u64) -> Vec<(u64, u64, bool)> {
    let mut stops: Vec<(u64, StopKind)> = events
        .iter()
        .filter_map(|e| match e {
            LoopEvent::Stop { kind: k @ (StopKind::Halt | StopKind::Release), at, .. } if *at <= now => Some((*at, *k)),
            _ => None,
        })
        .collect();
    stops.sort_by_key(|s| s.0);
    let mut out = Vec::new();
    let mut open: Option<u64> = None;
    for (at, kind) in stops {
        match (kind, open) {
            (StopKind::Halt, None) => open = Some(at),
            (StopKind::Release, Some(h)) => {
                out.push((h, at, true));
                open = None;
            }
            _ => {}
        }
    }
    if let Some(h) = open {
        out.push((h, now, false));
    }
    out
}

impl Window {
    pub fn new(events: &[LoopEvent], now: u64, secs: u64) -> Window {
        let halted = halts(events, now);
        let mut running = Vec::new();
        let mut cursor = now;
        let mut remaining = secs;
        for &(from, to, _) in halted.iter().rev() {
            let to = to.min(cursor);
            let len = cursor.saturating_sub(to);
            if len >= remaining {
                break;
            }
            if len > 0 {
                running.push((to, cursor));
            }
            remaining -= len;
            cursor = from.min(cursor);
        }
        let start = cursor.saturating_sub(remaining);
        if cursor > start {
            running.push((start, cursor));
        }
        let mut w = Window {
            secs,
            now,
            start,
            running,
            warming_until: None,
        };
        let release = halted.iter().rev().find(|&&(from, to, released)| released && to - from >= secs).map(|h| h.1);
        if let Some(r) = release.filter(|&r| w.start < r) {
            w.warming_until = Some(now + secs.saturating_sub(w.overlap(r, now)));
        }
        w
    }

    /// Whether `t` falls in the window's running time.
    pub fn contains(&self, t: u64) -> bool {
        self.running.iter().any(|&(a, b)| t > a && t <= b)
    }

    /// The window's running seconds inside `[from, to]`.
    pub fn overlap(&self, from: u64, to: u64) -> u64 {
        self.running
            .iter()
            .map(|&(a, b)| b.min(to).saturating_sub(a.max(from)))
            .sum()
    }

    fn per_day(&self) -> f64 {
        DAY_SECS as f64 / self.secs as f64
    }
}

/// What the goodput of one window is measured over: every repo, or one.
#[derive(Clone, Copy, Debug)]
pub enum Scope<'a> {
    All,
    Repo(&'a str),
}

/// Goodput over `window`. `repo_of` places a bead whose events do not name its repo.
pub fn goodput(events: &[LoopEvent], window: &Window, scope: Scope, repo_of: &BTreeMap<String, String>) -> Goodput {
    let in_scope = |repo: Option<&str>| match scope {
        Scope::All => true,
        Scope::Repo(r) => repo == Some(r),
    };
    let bead_repo = |b: &str| repo_of.get(b).map(String::as_str);

    let landed: BTreeSet<&str> = events
        .iter()
        .filter_map(|e| match e {
            LoopEvent::Landed { bead, repo, at, .. } if window.contains(*at) && in_scope(Some(repo)) => Some(bead.as_str()),
            _ => None,
        })
        .collect();
    let mut revert_of: BTreeMap<&str, u64> = BTreeMap::new();
    let mut landed_at: BTreeMap<&str, (u64, &str)> = BTreeMap::new();
    for e in events {
        if let LoopEvent::Landed { bead, repo, at, reverts, .. } = e {
            landed_at.entry(bead).or_insert((*at, repo));
            if let Some(h) = reverts {
                let t = revert_of.entry(h).or_insert(*at);
                *t = (*t).min(*at);
            }
        }
    }
    let repeat_ejects: BTreeSet<(&str, &str)> = events
        .iter()
        .filter_map(|e| match e {
            LoopEvent::Eject { bead, batch, repeat_n, .. } if *repeat_n >= 2 => Some((bead.as_str(), batch.as_str())),
            _ => None,
        })
        .collect();

    let mut charged: BTreeMap<String, Vec<Evidence>> = BTreeMap::new();
    let mut charge = |bead: &str, ev: Evidence| charged.entry(bead.to_string()).or_default().push(ev);
    for (h, &at) in &revert_of {
        let repo = landed_at.get(h).map(|l| l.1).or_else(|| bead_repo(h));
        if window.contains(at) && in_scope(repo) {
            charge(h, Evidence { signal: Signal::Revert, at, secs: 0, batch: None });
        }
    }
    for e in events {
        match e {
            LoopEvent::Stop { kind: StopKind::Pause, repo, bead: Some(h), at, .. } => {
                let repo = repo.as_deref().or_else(|| bead_repo(h));
                let until = revert_of.get(h.as_str()).copied().filter(|r| r > at).unwrap_or(window.now);
                let secs = window.overlap(*at, until);
                if secs > 0 && in_scope(repo) {
                    charge(h, Evidence { signal: Signal::LandingStop, at: *at, secs, batch: None });
                }
            }
            LoopEvent::RedAttributed { bead, batch, innocent, at, wall_secs } => {
                let signal = if !innocent.is_empty() {
                    Signal::RedWithInnocents
                } else if repeat_ejects.contains(&(bead.as_str(), batch.as_str())) {
                    Signal::RepeatEject
                } else {
                    continue;
                };
                let secs = window.overlap(at.saturating_sub(*wall_secs), *at);
                if secs > 0 && in_scope(bead_repo(bead)) {
                    charge(bead, Evidence { signal, at: *at, secs, batch: Some(batch.clone()) });
                }
            }
            LoopEvent::OverCapPass { repo, at, wall_secs } if in_scope(Some(repo)) => {
                let pass = (at.saturating_sub(*wall_secs), *at);
                for (h, &(landed, h_repo)) in &landed_at {
                    let Some(&reverted) = revert_of.get(h) else { continue };
                    if h_repo != repo || *at <= landed || *at > reverted {
                        continue;
                    }
                    let secs = window.overlap(pass.0.max(landed), pass.1);
                    if secs > 0 {
                        charge(h, Evidence { signal: Signal::OverCap, at: *at, secs, batch: None });
                    }
                }
            }
            _ => {}
        }
    }

    let harmful: Vec<Harm> = charged
        .into_iter()
        .map(|(bead, evidence)| Harm {
            bead,
            cost_secs: evidence.iter().map(|e| e.secs).sum::<u64>().min(window.secs),
            evidence,
        })
        .collect();

    let warming = window.warming_until.is_some();
    let n = landed.len() as f64;
    let (landed_m, ratio, rate) = if warming {
        (Measure::WarmingUp, Measure::WarmingUp, Measure::WarmingUp)
    } else if landed.is_empty() {
        (Measure::Value(0.0), Measure::Unobservable(NO_LANDINGS), Measure::Value(0.0))
    } else {
        let bad = harmful.iter().filter(|h| landed.contains(h.bead.as_str())).count() as f64;
        let cost: u64 = harmful.iter().map(|h| h.cost_secs).sum();
        let r = ((n - bad) / n - cost as f64 / window.secs as f64).clamp(0.0, 1.0);
        (Measure::Value(n), Measure::Value(r), Measure::Value(n * window.per_day() * r))
    };
    Goodput {
        window_secs: window.secs,
        landed: landed_m,
        ratio,
        rate,
        harmful,
        warming_until: window.warming_until,
    }
}

/// The p95 of CERTIFIED dwell over the window, from `(left_at, secs)` for each stay that ended
/// in it; a stay still open is passed as ending now.
pub fn dwell_p95(stays: &[(u64, u64)], window: &Window) -> Measure {
    if window.warming_until.is_some() {
        return Measure::WarmingUp;
    }
    let secs: Vec<f64> = stays.iter().filter(|(at, _)| window.contains(*at)).map(|(_, s)| *s as f64).collect();
    match tsd::query::quantile(&secs, 0.95) {
        Some(v) => Measure::Value(v),
        None => Measure::Unobservable(NO_CERTIFIED_EXITS),
    }
}

#[cfg(test)]
mod tests;
