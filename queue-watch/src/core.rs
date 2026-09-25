//! The pure half: a repo's previous state plus one snapshot of the world gives the next state
//! and the events between them. Nothing here reads a file, runs a command or looks at a
//! clock, so every transition the queue can make is a fixture a test can replay.
//!
//! EVENTS COME FROM STATE, NEVER FROM LOG LINES. The queue's own log misses whole
//! transitions (an express eviction closes a PR and writes nothing) and its prose changes
//! shape, so a watcher that greps it goes quiet exactly when something unusual happens.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub id: String,
    pub tip: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    pub pr: String,
    pub head: String,
    pub members: Vec<Member>,
}

impl Batch {
    fn ids(&self) -> BTreeSet<&str> {
        self.members.iter().map(|m| m.id.as_str()).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrState {
    Open,
    Merged,
    Closed,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ci {
    Pending,
    Green,
    Red,
    Fault(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bead {
    pub priority: Option<u8>,
    pub title: String,
    pub express: bool,
}

/// What became of a batch PR that is no longer the open one. Gathered by the IO layer only
/// when the core's previous state says a batch just went away, so a quiet queue costs no
/// forge calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub pr: String,
    pub state: PrState,
    /// Per member: is its tip an ancestor of the fetched base? None when not checked.
    pub on_base: Vec<(String, Option<bool>)>,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub now: u64,
    pub batch: Option<Batch>,
    pub ci: Option<Ci>,
    /// The bisect group the next cut is forced to, with the record's mtime.
    pub bisect: Option<(Vec<String>, u64)>,
    pub certified: Vec<String>,
    pub beads: BTreeMap<String, Bead>,
    pub outcome: Option<Outcome>,
    /// Anything that could not be read. Non-empty means this snapshot is not evidence of a
    /// quiet queue, and the core says so instead of reporting nothing.
    pub errors: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub idle_stall_secs: u64,
    pub head_stall_secs: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { idle_stall_secs: 600, head_stall_secs: 2700 }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RepoState {
    started: bool,
    batch: Option<Batch>,
    ci: Option<Ci>,
    head_since: u64,
    head_warned: bool,
    idle_since: Option<u64>,
    idle_warned: bool,
    blind: Option<String>,
}

impl RepoState {
    pub fn batch(&self) -> Option<&Batch> {
        self.batch.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub kind: &'static str,
    pub pr: Option<String>,
    pub ids: Vec<String>,
    pub text: String,
}

fn ev(kind: &'static str, pr: Option<&str>, ids: Vec<String>, text: String) -> Event {
    Event { kind, pr: pr.map(str::to_string), ids, text }
}

/// A title as a reader should see it: a leading `sp-xxxx:` names some OTHER bead and makes
/// `<id> — <title>` read as two beads, so it is dropped.
pub fn clean_title(t: &str) -> String {
    let t = t.trim();
    if let Some(rest) = t.strip_prefix("sp-") {
        if let Some(colon) = rest.find(':') {
            let id = &rest[..colon];
            if !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '.') {
                return rest[colon + 1..].trim_start().to_string();
            }
        }
    }
    t.to_string()
}

fn prio_str(b: Option<&Bead>) -> String {
    match b.and_then(|b| b.priority) {
        Some(p) => format!("P{p}"),
        None => "P?".into(),
    }
}

fn describe(id: &str, beads: &BTreeMap<String, Bead>) -> String {
    let b = beads.get(id);
    let title = b.map(|b| clean_title(&b.title)).unwrap_or_default();
    let x = if b.map(|b| b.express).unwrap_or(false) { " express" } else { "" };
    if title.is_empty() {
        format!("{id} ({}{x})", prio_str(b))
    } else {
        format!("{id} ({}{x}) {title}", prio_str(b))
    }
}

fn mins(secs: u64) -> u64 {
    secs / 60
}

/// Lowest number is most urgent; an unknown priority sorts last so it never manufactures an
/// inversion.
fn rank(b: Option<&Bead>) -> u8 {
    b.and_then(|b| b.priority).unwrap_or(u8::MAX)
}

pub fn step(prev: &RepoState, snap: &Snapshot, lim: Limits) -> (RepoState, Vec<Event>) {
    let mut st = prev.clone();
    let mut out = Vec::new();

    if !snap.errors.is_empty() {
        let why = snap.errors.join("; ");
        if st.blind.as_deref() != Some(why.as_str()) {
            out.push(ev("blind", None, vec![], format!("cannot see the queue — {why}")));
            st.blind = Some(why);
        }
        return (st, out);
    }
    if let Some(was) = st.blind.take() {
        out.push(ev("recovered", None, vec![], format!("queue readable again (was: {was})")));
    }

    if !st.started {
        st.started = true;
        match &snap.batch {
            Some(b) => {
                let ids: Vec<String> = b.members.iter().map(|m| m.id.clone()).collect();
                let list: Vec<String> = ids.iter().map(|i| describe(i, &snap.beads)).collect();
                out.push(ev(
                    "watching",
                    Some(&b.pr),
                    ids,
                    format!("PR {} open with {} member(s): {}; {} certified waiting", b.pr, b.members.len(), list.join(", "), snap.certified.len()),
                ));
                st.head_since = snap.now;
            }
            None => out.push(ev(
                "watching",
                None,
                vec![],
                format!("no batch open; {} certified waiting", snap.certified.len()),
            )),
        }
        st.batch = snap.batch.clone();
        st.ci = snap.ci.clone();
        idle_bookkeeping(&mut st, snap, lim, &mut out);
        return (st, out);
    }

    let prev_batch = st.batch.clone();
    let same_pr = matches!((&prev_batch, &snap.batch), (Some(a), Some(b)) if a.pr == b.pr);

    // A batch went away, or was replaced by another PR: say what became of it.
    if let Some(pb) = &prev_batch {
        if !same_pr {
            out.push(outcome_event(pb, snap));
        }
    }

    match (&prev_batch, &snap.batch) {
        (_, Some(cb)) if !same_pr => {
            out.extend(opened_events(cb, snap));
            st.head_since = snap.now;
            st.head_warned = false;
            st.ci = None;
        }
        (Some(pb), Some(cb)) => {
            let now_ids = cb.ids();
            let red = match &st.ci {
                Some(Ci::Red) => " after a red run",
                _ => "",
            };
            for m in &pb.members {
                if !now_ids.contains(m.id.as_str()) {
                    out.push(ev(
                        "ejected",
                        Some(&cb.pr),
                        vec![m.id.clone()],
                        format!("{} ejected from PR {}{red}", describe(&m.id, &snap.beads), cb.pr),
                    ));
                }
            }
            if pb.head != cb.head {
                out.push(ev(
                    "repushed",
                    Some(&cb.pr),
                    cb.members.iter().map(|m| m.id.clone()).collect(),
                    format!("PR {} re-pushed with {} member(s)", cb.pr, cb.members.len()),
                ));
                st.head_since = snap.now;
                st.head_warned = false;
                st.ci = None;
            }
        }
        _ => {}
    }
    st.batch = snap.batch.clone();

    if let (Some(cb), Some(ci)) = (&snap.batch, &snap.ci) {
        if st.ci.as_ref() != Some(ci) {
            let text = match ci {
                Ci::Pending => format!("PR {} CI running", cb.pr),
                Ci::Green => format!("PR {} CI green — lands on the next verdict pass", cb.pr),
                Ci::Red => format!("PR {} CI red — attribution follows", cb.pr),
                Ci::Fault(k) => format!("PR {} CI did not test the branch ({k})", cb.pr),
            };
            out.push(ev("ci", Some(&cb.pr), vec![], text));
            st.ci = Some(ci.clone());
        }
    }

    if let Some(cb) = &snap.batch {
        let age = snap.now.saturating_sub(st.head_since);
        if !st.head_warned && age >= lim.head_stall_secs {
            out.push(ev(
                "stall",
                Some(&cb.pr),
                vec![],
                format!("PR {} unchanged for {}m (CI {})", cb.pr, mins(age), ci_word(st.ci.as_ref())),
            ));
            st.head_warned = true;
        }
    }
    idle_bookkeeping(&mut st, snap, lim, &mut out);
    (st, out)
}

fn ci_word(ci: Option<&Ci>) -> &'static str {
    match ci {
        Some(Ci::Pending) => "running",
        Some(Ci::Green) => "green",
        Some(Ci::Red) => "red",
        Some(Ci::Fault(_)) => "faulted",
        None => "unread",
    }
}

fn idle_bookkeeping(st: &mut RepoState, snap: &Snapshot, lim: Limits, out: &mut Vec<Event>) {
    if snap.batch.is_some() || snap.certified.is_empty() {
        st.idle_since = None;
        st.idle_warned = false;
        return;
    }
    let since = *st.idle_since.get_or_insert(snap.now);
    let age = snap.now.saturating_sub(since);
    if !st.idle_warned && age >= lim.idle_stall_secs {
        out.push(ev(
            "stall",
            None,
            snap.certified.clone(),
            format!("no batch open for {}m with {} certified waiting", mins(age), snap.certified.len()),
        ));
        st.idle_warned = true;
    }
}

fn outcome_event(pb: &Batch, snap: &Snapshot) -> Event {
    let ids: Vec<String> = pb.members.iter().map(|m| m.id.clone()).collect();
    let oc = snap.outcome.as_ref().filter(|o| o.pr == pb.pr);
    match oc.map(|o| o.state) {
        Some(PrState::Merged) => {
            let o = oc.unwrap();
            let missing: Vec<&str> = o
                .on_base
                .iter()
                .filter(|(_, v)| *v == Some(false))
                .map(|(i, _)| i.as_str())
                .collect();
            let unchecked = o.on_base.iter().filter(|(_, v)| v.is_none()).count();
            if !missing.is_empty() {
                ev(
                    "landed-unverified",
                    Some(&pb.pr),
                    ids,
                    format!("PR {} merged but {} NOT on the base branch: {}", pb.pr, missing.len(), missing.join(" ")),
                )
            } else {
                let how = if unchecked == 0 { "verified on base" } else { "base not checked" };
                let list: Vec<String> = pb.members.iter().map(|m| describe(&m.id, &snap.beads)).collect();
                ev("landed", Some(&pb.pr), ids, format!("PR {} landed ({how}): {}", pb.pr, list.join(", ")))
            }
        }
        Some(PrState::Closed) => {
            let back: BTreeSet<&str> = snap.certified.iter().map(String::as_str).collect();
            let returned = pb.members.iter().filter(|m| back.contains(m.id.as_str())).count();
            let why = if returned == pb.members.len() && returned > 0 {
                "all members back in CERTIFIED — evicted or abandoned".to_string()
            } else {
                format!("{returned} of {} member(s) back in CERTIFIED", pb.members.len())
            };
            ev("closed-unlanded", Some(&pb.pr), ids, format!("PR {} closed without landing: {why}", pb.pr))
        }
        Some(PrState::Open) => ev(
            "closed-unlanded",
            Some(&pb.pr),
            ids,
            format!("the queue dropped PR {} but it is still open on the forge", pb.pr),
        ),
        _ => ev(
            "closed-unlanded",
            Some(&pb.pr),
            ids,
            format!("PR {} is no longer the open batch and its forge state could not be read", pb.pr),
        ),
    }
}

fn opened_events(cb: &Batch, snap: &Snapshot) -> Vec<Event> {
    let mut out = Vec::new();
    let ids: Vec<String> = cb.members.iter().map(|m| m.id.clone()).collect();
    let list: Vec<String> = ids.iter().map(|i| describe(i, &snap.beads)).collect();
    out.push(ev(
        "opened",
        Some(&cb.pr),
        ids.clone(),
        format!("PR {} opened with {} member(s): {}", cb.pr, cb.members.len(), list.join(", ")),
    ));

    let member_set = cb.ids();
    if let Some((group, mtime)) = &snap.bisect {
        let g: BTreeSet<&str> = group.iter().map(String::as_str).collect();
        if !g.is_empty() && g == member_set {
            out.push(ev(
                "forced-cut",
                Some(&cb.pr),
                ids.clone(),
                format!(
                    "PR {}'s membership was forced by a bisect group recorded {}m ago, not chosen by priority",
                    cb.pr,
                    mins(snap.now.saturating_sub(*mtime))
                ),
            ));
        }
    }

    let worst = cb.members.iter().map(|m| rank(snap.beads.get(&m.id))).filter(|r| *r != u8::MAX).max();
    if let Some(worst) = worst {
        let mut left: Vec<(&String, u8)> = snap
            .certified
            .iter()
            .filter(|c| !member_set.contains(c.as_str()))
            .map(|c| (c, rank(snap.beads.get(c))))
            .filter(|(_, r)| *r < worst)
            .collect();
        left.sort_by_key(|(c, r)| (*r, (*c).clone()));
        if !left.is_empty() {
            let n = left.len();
            let shown: Vec<String> = left.iter().take(6).map(|(c, _)| describe(c, &snap.beads)).collect();
            let more = if n > 6 { format!(" and {} more", n - 6) } else { String::new() };
            out.push(ev(
                "priority-inversion",
                Some(&cb.pr),
                left.iter().map(|(c, _)| (*c).clone()).collect(),
                format!(
                    "PR {} took P{worst} work while {n} more urgent certified bead(s) wait: {}{more}",
                    cb.pr,
                    shown.join(", ")
                ),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests;
