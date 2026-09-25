//! The pure half of the batcher: round membership, set-asides, green/flip/double-red
//! classification, adaptive triggers and stacking, and the PR/queue record — from the
//! certified set with tips, merge-conflict results, two suite-run result sets, the clock and
//! pool history. Nothing here reads a file, runs git or testenv-batch, calls the forge, or
//! looks at a wall clock, so every decision the batcher can make is a fixture a test can
//! replay. The IO seam (git, testenv-batch, the forge, the bead store) is a separate crate.

use std::collections::BTreeMap;

pub type Id = String;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub id: Id,
    pub tip: String,
    pub title: String,
    pub priority: Option<u8>,
    pub express: bool,
    pub certified_at: u64,
}

/// Lowest number is most urgent; an unknown priority sorts last so it never manufactures an
/// inversion. Express always outranks rank (REQUIREMENTS: express ranks first).
///
/// Public so the IO seam can sort the pool into the same order *before* attempting git
/// merges — combine() only sorts what already merged, but merge order decides who wins a
/// batch-accumulation conflict, and that has to be the same express/priority/arrival order.
pub fn order_key(m: &Member) -> (u8, u8, u64) {
    (if m.express { 0 } else { 1 }, m.priority.unwrap_or(u8::MAX), m.certified_at)
}

/// A title as a reader should see it: a leading `sp-xxxx:` names some OTHER bead and makes
/// `<id> — <title>` read as two beads, so it is dropped; never cut mid-word otherwise.
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

// ---------------------------------------------------------------------------------------
// A. Adaptive trigger
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PoolHistory {
    pub certify_rate_per_min: f64,
    pub round_duration_mins: f64,
}

/// N ≈ certify rate × round duration, clamped to [4, 30]. The pool that fills during one
/// round is the next round.
pub fn adaptive_n(h: PoolHistory) -> u32 {
    let raw = (h.certify_rate_per_min * h.round_duration_mins).round();
    if raw.is_nan() {
        return 4;
    }
    raw.clamp(4.0, 30.0) as u32
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TriggerReason {
    /// The certified pool reached N.
    PoolFull(u32),
    /// Nothing new has arrived for Q minutes.
    Idle { waited_mins: u64 },
    /// An express or main-red fix cuts a round of its own, at once.
    Express(Id),
    MainRed,
}

pub struct TriggerInputs<'a> {
    pub pool: &'a [Member],
    pub now: u64,
    /// Time the most recently certified member in the pool arrived; None when the pool is
    /// empty (nothing to be idle about).
    pub last_arrival: Option<u64>,
    pub n: u32,
    pub q_minutes: u64,
    /// A main-red fix is waiting: cut at once regardless of pool size.
    pub main_red: bool,
    /// A batch PR is already open: the only back pressure (law-queue-back-pressure-is-an-
    /// open-pr). While one is open, only an express or main-red cut may fire (pipelined on
    /// top of that PR's head); ordinary pool/idle triggers wait for it to close.
    pub batch_open: bool,
}

pub fn should_cut(t: &TriggerInputs) -> Option<TriggerReason> {
    if t.main_red {
        return Some(TriggerReason::MainRed);
    }
    if let Some(express) = t.pool.iter().find(|m| m.express) {
        return Some(TriggerReason::Express(express.id.clone()));
    }
    if t.batch_open {
        return None;
    }
    if t.pool.is_empty() {
        return None;
    }
    if t.pool.len() as u32 >= t.n {
        return Some(TriggerReason::PoolFull(t.pool.len() as u32));
    }
    if let Some(last) = t.last_arrival {
        let waited = t.now.saturating_sub(last) / 60;
        if waited >= t.q_minutes {
            return Some(TriggerReason::Idle { waited_mins: waited });
        }
    }
    None
}

// ---------------------------------------------------------------------------------------
// B. Combined local validation: merge + set-asides + classification
// ---------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeResult {
    Ok,
    Conflict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetAsideReason {
    /// Handed back to the landing pass to rebase; never reopened by the batcher itself.
    Conflict { deleted_suites: Vec<String> },
    /// A member's own new/changed suite is red against main-plus-that-member-alone and
    /// names the bead it waits on (E): sequenced behind that bead, certification withdrawn.
    TestAheadOfCode { waits_on: Id },
    /// A member sequenced behind a dependency by prior judgement (C), not yet cleared.
    Dependency { waits_on: Id },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetAside {
    pub id: Id,
    pub reason: SetAsideReason,
}

pub struct CombineInput<'a> {
    pub pool: &'a [Member],
    pub merges: &'a BTreeMap<Id, MergeResult>,
    /// Suites deleted since each member was certified (F), reported only for members that
    /// conflict, since that is the list their builder needs to rebase.
    pub deleted_suites: &'a BTreeMap<Id, Vec<String>>,
    /// Members already sequenced behind a dependency by an earlier round's judgement or E
    /// check; still withheld until that bead lands.
    pub sequenced: &'a BTreeMap<Id, Id>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Combined {
    /// Round membership, ordered express-first then by rank (REQUIREMENTS: express ranks
    /// first).
    pub merged: Vec<Member>,
    pub set_aside: Vec<SetAside>,
}

pub fn combine(input: &CombineInput) -> Combined {
    let mut merged = Vec::new();
    let mut set_aside = Vec::new();
    for m in input.pool {
        if let Some(dep) = input.sequenced.get(&m.id) {
            set_aside.push(SetAside { id: m.id.clone(), reason: SetAsideReason::Dependency { waits_on: dep.clone() } });
            continue;
        }
        match input.merges.get(&m.id) {
            Some(MergeResult::Ok) => merged.push(m.clone()),
            _ => {
                let deleted = input.deleted_suites.get(&m.id).cloned().unwrap_or_default();
                set_aside.push(SetAside { id: m.id.clone(), reason: SetAsideReason::Conflict { deleted_suites: deleted } });
            }
        }
    }
    merged.sort_by_key(order_key);
    Combined { merged, set_aside }
}

// ---------------------------------------------------------------------------------------
// F. Stale members: compare against when the base ref moved, not the base commit's own time
// ---------------------------------------------------------------------------------------

/// A batch merge commit is created before the batch lands, so the base head's own commit
/// time never looks newer than a member certified moments before. Compare instead against
/// the fetch that observed the ref move, so a retry fires the moment the base actually
/// changed underneath the member.
pub fn stale_retry_due(member_certified_at: u64, base_ref_moved_at: u64) -> bool {
    base_ref_moved_at > member_certified_at
}

// ---------------------------------------------------------------------------------------
// Suite classification: green / flip / double-red
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuiteOutcome {
    Green,
    Red,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuiteRun {
    pub name: String,
    pub outcome: SuiteOutcome,
    pub failing_assertions: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Classification {
    Green,
    /// Red on the first run, green on the re-run: not attributed to any member's code.
    Flip,
    /// Red on both runs: a real culprit, handed to judgement (C).
    DoubleRed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuiteVerdict {
    pub name: String,
    pub classification: Classification,
    pub failing_assertions: Vec<String>,
}

/// Run the full corpus, then re-run only the reds (B). A suite absent from the re-run set
/// (nothing to re-run because it was never red) cannot flip; a suite present but not found
/// red there classifies as Flip only when it clearly reports Green — an unreadable or
/// missing re-run leaves it DoubleRed, so a lookup failure never manufactures a green.
pub fn classify(first: &[SuiteRun], rerun: &[SuiteRun]) -> Vec<SuiteVerdict> {
    let rerun_by_name: BTreeMap<&str, &SuiteRun> = rerun.iter().map(|r| (r.name.as_str(), r)).collect();
    first
        .iter()
        .map(|f| {
            let classification = match f.outcome {
                SuiteOutcome::Green => Classification::Green,
                SuiteOutcome::Red => match rerun_by_name.get(f.name.as_str()) {
                    Some(r) if r.outcome == SuiteOutcome::Green => Classification::Flip,
                    _ => Classification::DoubleRed,
                },
            };
            SuiteVerdict { name: f.name.clone(), classification, failing_assertions: f.failing_assertions.clone() }
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// E. A test ahead of its code (the commonest double-red shape; deterministic)
// ---------------------------------------------------------------------------------------

/// An assertion that names the bead its behaviour waits on, e.g. "...waits on sp-ui46l" or
/// "...blocked on sp-29g55". Scans for the pattern rather than requiring exact phrasing,
/// since the wording is the test author's, not the batcher's.
pub fn waits_on(assertion_text: &str) -> Option<Id> {
    let bytes = assertion_text.as_bytes();
    let mut i = 0;
    while let Some(off) = assertion_text[i..].find("sp-") {
        let start = i + off;
        let mut end = start + 3;
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'.') {
            end += 1;
        }
        let candidate = &assertion_text[start..end];
        // Require at least one digit after "sp-" so prose like "sp-" alone, or a word that
        // merely starts with it, is never mistaken for a bead id.
        if candidate.len() > 3 && candidate[3..].chars().any(|c| c.is_ascii_digit()) {
            return Some(candidate.trim_end_matches('.').to_string());
        }
        i = end.max(start + 1);
        if i >= bytes.len() {
            break;
        }
    }
    None
}

/// At combine time, a member's own NEW or CHANGED suites are run against main-plus-that-
/// member alone; a suite red there that names a bead it waits on sequences the member
/// behind that bead (dependency + withdrawn certification) before the full corpus ever
/// runs (E).
pub fn test_ahead_of_code(member_id: &Id, own_suites: &[SuiteRun]) -> Option<SetAside> {
    own_suites
        .iter()
        .filter(|s| s.outcome == SuiteOutcome::Red)
        .find_map(|s| s.failing_assertions.iter().find_map(|a| waits_on(a)))
        .map(|dep| SetAside { id: member_id.clone(), reason: SetAsideReason::TestAheadOfCode { waits_on: dep } })
}

// ---------------------------------------------------------------------------------------
// Bisect state: bound to member tips, expired when any tip changes or any batch lands
// ---------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BisectState {
    pub members: Vec<Id>,
    pub tips: BTreeMap<Id, String>,
}

pub fn bisect_expired(state: &BisectState, current_tips: &BTreeMap<Id, String>, batch_landed_since: bool) -> bool {
    if batch_landed_since {
        return true;
    }
    state.members.iter().any(|id| current_tips.get(id) != state.tips.get(id))
}

/// Narrow the bisect group to the half that reproduced the failure. A group of one is not
/// narrowed further — it is pinned, and judgement (C) takes it from there.
pub fn bisect_next(state: &BisectState, bad_half: &[Id]) -> Option<BisectState> {
    if bad_half.len() <= 1 {
        return None;
    }
    let mid = bad_half.len() / 2;
    let members: Vec<Id> = bad_half[..mid].to_vec();
    let tips = state.tips.iter().filter(|(k, _)| members.contains(k)).map(|(k, v)| (k.clone(), v.clone())).collect();
    Some(BisectState { members, tips })
}

/// The initial split of a double-red's members into two halves to test independently.
pub fn bisect_split(members: &[Id], tips: &BTreeMap<Id, String>) -> (BisectState, BisectState) {
    let mid = members.len().div_ceil(2);
    let (a, b) = members.split_at(mid);
    let group = |ids: &[Id]| BisectState {
        members: ids.to_vec(),
        tips: ids.iter().filter_map(|id| tips.get(id).map(|t| (id.clone(), t.clone()))).collect(),
    };
    (group(a), group(b))
}

// ---------------------------------------------------------------------------------------
// D. The PR / queue record
// ---------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrRecord {
    pub members: Vec<Id>,
    pub body: String,
}

/// One PR per validated round, carrying the queue's open-batch record. `members` is
/// already ordered express-first then by rank (`combine`'s output order); the body lists
/// full titles, never cut mid-word, with any leading foreign `sp-x:` prefix dropped.
pub fn pr_record(members: &[Member]) -> PrRecord {
    let body = members
        .iter()
        .map(|m| {
            let title = clean_title(&m.title);
            let x = if m.express { " express" } else { "" };
            let p = m.priority.map(|p| format!("P{p}")).unwrap_or_else(|| "P?".into());
            if title.is_empty() {
                format!("- {} ({}{x})", m.id, p)
            } else {
                format!("- {} ({}{x}) {title}", m.id, p)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    PrRecord { members: members.iter().map(|m| m.id.clone()).collect(), body }
}

// ---------------------------------------------------------------------------------------
// Structured events: one per transition, so nothing is reconstructed from log prose
// ---------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub kind: &'static str,
    pub ids: Vec<Id>,
    pub text: String,
}

fn ev(kind: &'static str, ids: Vec<Id>, text: String) -> Event {
    Event { kind, ids, text }
}

pub fn cut_event(reason: &TriggerReason, combined: &Combined) -> Event {
    let ids: Vec<Id> = combined.merged.iter().map(|m| m.id.clone()).collect();
    let why = match reason {
        TriggerReason::PoolFull(n) => format!("pool reached {n}"),
        TriggerReason::Idle { waited_mins } => format!("idle {waited_mins}m with nothing new"),
        TriggerReason::Express(id) => format!("express {id}"),
        TriggerReason::MainRed => "main red".to_string(),
    };
    ev("cut", ids.clone(), format!("round cut ({why}): {} member(s)", ids.len()))
}

pub fn skipped_event(reason: &str) -> Event {
    ev("skipped", vec![], format!("no round cut: {reason}"))
}

pub fn opened_event(pr: &PrRecord) -> Event {
    ev("opened", pr.members.clone(), format!("PR opened with {} member(s)", pr.members.len()))
}

pub fn evicted_event(set_aside: &SetAside) -> Event {
    let text = match &set_aside.reason {
        SetAsideReason::Conflict { deleted_suites } if deleted_suites.is_empty() => {
            format!("{} set aside: conflicts with base, handed to the landing pass to rebase", set_aside.id)
        }
        SetAsideReason::Conflict { deleted_suites } => format!(
            "{} set aside: conflicts with base, handed to the landing pass to rebase (suites deleted since cut: {})",
            set_aside.id,
            deleted_suites.join(", ")
        ),
        SetAsideReason::TestAheadOfCode { waits_on } => {
            format!("{} set aside: test ahead of its code, sequenced behind {waits_on}", set_aside.id)
        }
        SetAsideReason::Dependency { waits_on } => format!("{} set aside: sequenced behind {waits_on}", set_aside.id),
    };
    ev("evicted", vec![set_aside.id.clone()], text)
}

pub fn abandoned_event(id: &Id, why: &str) -> Event {
    ev("abandoned", vec![id.clone()], format!("{id} abandoned: {why}"))
}

#[cfg(test)]
#[path = "core/tests.rs"]
mod tests;
