//! The screen runs on the proto-round (SUBMITTED and CERTIFIED beads), never inside a cut. A
//! passing candidate has its pass recorded for its tip; a failing one is sent back (evidence note
//! first, then the GateRed), superseded, or — once the cap is spent — held out and reported. A
//! candidate the screen cannot judge records nothing and stays unscreened.

mod git;
mod store;

use std::collections::BTreeMap;

pub use git::GitProbe;
pub use store::FileStore;

pub const SEND_BACK_CAP: u32 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub tip: String,
}

pub enum Merge {
    Clean(String),
    Conflict(Vec<String>),
}

pub trait Probe {
    fn base(&self) -> Result<String, String>;
    fn merge(&self, tip: &str) -> Result<Merge, String>;
    fn patch_id(&self, tip: &str) -> Result<String, String>;
    fn stacked_on(&self, id: &str, tip: &str) -> Result<Vec<String>, String>;
    fn state(&self, id: &str) -> Result<String, String>;
}

pub trait Store {
    fn get(&self, id: &str, tip: &str) -> Option<Verdict>;
    fn put(&self, v: &Verdict) -> Result<(), String>;
    fn send_backs(&self, id: &str) -> u32;
    fn record_send_back(&self, id: &str) -> Result<u32, String>;
    /// True the first time `(id, tip)` is reported as capped.
    fn first_cap_report(&self, id: &str, tip: &str) -> bool;
}

pub trait Acts {
    fn note(&mut self, id: &str, text: &str) -> Result<(), String>;
    fn gate_red(&mut self, id: &str, tip: &str, reason: &str) -> Result<(), String>;
    fn supersede(&mut self, id: &str, keeper: &str) -> Result<(), String>;
    /// Records that `tip` passed, on the lifecycle store.
    fn pass(&mut self, id: &str, tip: &str) -> Result<(), String>;
    fn tell(&mut self, msg: &str);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub id: String,
    pub tip: String,
    pub base: String,
    pub conflict: Option<Vec<String>>,
    pub patch_id: String,
    pub stacked: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub reason: &'static str,
    pub text: String,
}

impl Verdict {
    pub fn findings(&self, state_of: &dyn Fn(&str) -> Option<String>) -> Vec<Finding> {
        let mut out = Vec::new();
        if let Some(files) = &self.conflict {
            let shown = files.iter().take(10).cloned().collect::<Vec<_>>().join(", ");
            out.push(Finding { reason: "no-rebase", text: format!("does not merge onto the landing ref: {shown}") });
        }
        for other in &self.stacked {
            if state_of(other).as_deref() == Some("REWORK") {
                out.push(Finding {
                    reason: "suites-failed",
                    text: format!("stacked on {other}, which is in REWORK: its commits ride in with this branch"),
                });
            }
        }
        out
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub pass: Vec<String>,
    /// Passed on a verdict actually reached, and recorded: the only candidates a cut may take.
    pub sifted: Vec<String>,
    pub sent_back: Vec<String>,
    pub superseded: Vec<String>,
    pub capped: Vec<String>,
    pub held: Vec<String>,
    pub errors: Vec<String>,
}

fn verdict(probe: &dyn Probe, store: &dyn Store, c: &Candidate, base: &str) -> Result<Verdict, String> {
    if let Some(v) = store.get(&c.id, &c.tip).filter(|v| v.base == base) {
        return Ok(v);
    }
    let mut v = Verdict { id: c.id.clone(), tip: c.tip.clone(), base: base.to_string(), conflict: None, patch_id: String::new(), stacked: Vec::new() };
    if let Merge::Conflict(files) = probe.merge(&c.tip)? {
        v.conflict = Some(files);
    }
    v.patch_id = probe.patch_id(&c.tip)?;
    v.stacked = probe.stacked_on(&c.id, &c.tip)?;
    store.put(&v)?;
    Ok(v)
}

/// A candidate the screen cannot judge passes, with the error in `errors`.
pub fn screen(probe: &dyn Probe, store: &dyn Store, acts: &mut dyn Acts, pool: &[Candidate], open_round: &[Candidate]) -> Outcome {
    let mut out = Outcome::default();
    let base = match probe.base() {
        Ok(b) => b,
        Err(e) => {
            out.errors.push(format!("sift: no base ({e}); cutting unfiltered"));
            out.pass = pool.iter().map(|c| c.id.clone()).collect();
            return out;
        }
    };
    let mut verdicts: BTreeMap<String, Verdict> = BTreeMap::new();
    for c in pool {
        match verdict(probe, store, c, &base) {
            Ok(v) => {
                verdicts.insert(c.id.clone(), v);
            }
            Err(e) => out.errors.push(format!("sift: {} screening failed ({e}); passed unscreened", c.id)),
        }
    }

    let mut by_patch: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut in_round: Vec<String> = Vec::new();
    for m in open_round {
        match probe.patch_id(&m.tip) {
            Ok(p) if !p.is_empty() => {
                by_patch.entry(p).or_default().push(m.id.clone());
                in_round.push(m.id.clone());
            }
            Ok(_) => {}
            Err(e) => out.errors.push(format!("sift: open-round member {} patch-id failed ({e})", m.id)),
        }
    }
    for (id, v) in &verdicts {
        if !v.patch_id.is_empty() {
            by_patch.entry(v.patch_id.clone()).or_default().push(id.clone());
        }
    }
    let mut duplicate_of: BTreeMap<String, String> = BTreeMap::new();
    for ids in by_patch.values().filter(|ids| ids.len() > 1) {
        let mut sorted = ids.clone();
        sorted.sort();
        let keeper = sorted.iter().find(|i| in_round.contains(i)).unwrap_or(&sorted[0]).clone();
        for id in sorted.into_iter().filter(|i| *i != keeper && verdicts.contains_key(i)) {
            duplicate_of.insert(id, keeper.clone());
        }
    }

    let states: std::cell::RefCell<BTreeMap<String, Option<String>>> = Default::default();
    let state_of = |id: &str| -> Option<String> {
        states.borrow_mut().entry(id.to_string()).or_insert_with(|| probe.state(id).ok()).clone()
    };

    for c in pool {
        let Some(v) = verdicts.get(&c.id) else {
            out.pass.push(c.id.clone());
            continue;
        };
        if let Some(keeper) = duplicate_of.get(&c.id) {
            match acts.supersede(&c.id, keeper) {
                Ok(()) => {
                    acts.tell(&format!("SIFT superseded {} (identical patch to {keeper})", c.id));
                    out.superseded.push(c.id.clone());
                }
                Err(e) => {
                    out.errors.push(format!("sift: {} supersede failed ({e})", c.id));
                    out.pass.push(c.id.clone());
                }
            }
            continue;
        }
        let findings = v.findings(&state_of);
        if findings.is_empty() {
            out.pass.push(c.id.clone());
            match acts.pass(&c.id, &c.tip) {
                Ok(()) => out.sifted.push(c.id.clone()),
                Err(e) => out.errors.push(format!("sift: {} pass not recorded ({e}); stays unscreened", c.id)),
            }
            continue;
        }
        let prior = store.send_backs(&c.id);
        if prior >= SEND_BACK_CAP {
            if store.first_cap_report(&c.id, &c.tip) {
                acts.tell(&format!("SIFT REPEAT {}: red after {prior} send-backs, left in place and excluded from cuts — {}", c.id, findings[0].text));
            }
            out.capped.push(c.id.clone());
            continue;
        }
        let note = format!(
            "Sift (pre-round screen) sent this back before it reached a round. Fix these, then resubmit:\n{}",
            findings.iter().map(|f| format!("- [{}] {}", f.reason, f.text)).collect::<Vec<_>>().join("\n")
        );
        if let Err(e) = acts.note(&c.id, &note) {
            out.errors.push(format!("sift: {} evidence note failed ({e}); not sent back, held out of this cut", c.id));
            out.held.push(c.id.clone());
            continue;
        }
        match acts.gate_red(&c.id, &c.tip, findings[0].reason) {
            Ok(()) => {
                out.sent_back.push(c.id.clone());
                if let Err(e) = store.record_send_back(&c.id) {
                    out.errors.push(format!("sift: {} send-back count not recorded ({e})", c.id));
                }
                acts.tell(&format!("SIFT sent {} to REWORK: {}", c.id, findings[0].text));
            }
            Err(e) => {
                out.errors.push(format!("sift: {} GateRed refused ({e}); held out of this cut", c.id));
                out.held.push(c.id.clone());
            }
        }
    }
    out
}

/// The pool the cut should use: the screen's passes (the whole pool when the screen could not run).
pub fn filter<T>(pool: Vec<T>, id_of: impl Fn(&T) -> &str, out: &Outcome) -> Vec<T> {
    pool.into_iter().filter(|m| out.pass.iter().any(|p| p == id_of(m))).collect()
}

pub fn parse_stacked_id(subject: &str, own: &str) -> Option<String> {
    let s = subject.strip_prefix("spira: land ").unwrap_or(subject);
    let rest = s.strip_prefix("sp-")?;
    let n = rest.find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.'))?;
    if n == 0 || !matches!(rest.as_bytes()[n], b':' | b' ') {
        return None;
    }
    let id = format!("sp-{}", &rest[..n]);
    (id != own).then_some(id)
}

#[cfg(test)]
mod tests;
