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
    /// Prerequisite bead id -> the tip this member's own work was built on (design
    /// stacked-dependents-2026-09-28 §1). Empty for an unstacked member — the pool's default,
    /// and byte-identical to today's behaviour wherever a caller never populates it.
    pub stack: BTreeMap<Id, String>,
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

/// Topological merge order (design §3, "Order"): a member merges only after every
/// prerequisite named in its `stack` that is also present in `members` — Kahn's algorithm,
/// picking among the members with no unmet prerequisite by `order_key` so a pool with no
/// stacking at all sorts exactly as before. A cycle (which claim-time depth checking should
/// have already refused) cannot stall this: once nothing is ready, the lowest `order_key`
/// member breaks it rather than the round silently dropping members.
pub fn topo_order(members: &[Member]) -> Vec<Member> {
    let ids: std::collections::BTreeSet<&Id> = members.iter().map(|m| &m.id).collect();
    let mut remaining: Vec<Member> = members.to_vec();
    let mut placed: std::collections::BTreeSet<Id> = std::collections::BTreeSet::new();
    let mut out = Vec::with_capacity(members.len());
    while !remaining.is_empty() {
        let ready: Vec<usize> = remaining
            .iter()
            .enumerate()
            .filter(|(_, m)| m.stack.keys().all(|p| !ids.contains(p) || placed.contains(p)))
            .map(|(i, _)| i)
            .collect();
        let idx = if ready.is_empty() {
            (0..remaining.len()).min_by_key(|&i| order_key(&remaining[i])).unwrap()
        } else {
            *ready.iter().min_by_key(|&&i| order_key(&remaining[i])).unwrap()
        };
        let picked = remaining.remove(idx);
        placed.insert(picked.id.clone());
        out.push(picked);
    }
    out
}

/// True when some other pool member's `stack` names `id` as a prerequisite — the fact that
/// tells "empty because a dependent already carried this tip into the round" apart from
/// "empty because it genuinely has no commits of its own", which `MergeResult::Empty` alone
/// cannot say (design §3, "Closure": a contained member is still a member, never dropped).
pub fn stacked_into(pool: &[Member], id: &Id) -> bool {
    pool.iter().any(|m| m.stack.contains_key(id))
}

/// member_added's stale-stack refusal (design §3): a member whose `stack` names a
/// prerequisite tip that is not, at that exact tip, another member of this same pool is
/// refused — sequenced behind that prerequisite via `combine`'s existing `Dependency`
/// set-aside, the tip invariant extended to round assembly. A prerequisite present in the
/// pool at the recorded tip is closure working as intended: it merges into the same round.
pub fn stack_sequencing(pool: &[Member]) -> BTreeMap<Id, Id> {
    let by_id: BTreeMap<&Id, &Member> = pool.iter().map(|m| (&m.id, m)).collect();
    let mut sequenced = BTreeMap::new();
    for m in pool {
        for (prereq, tip) in &m.stack {
            let current = by_id.get(prereq).map(|p| &p.tip == tip).unwrap_or(false);
            if !current {
                sequenced.insert(m.id.clone(), prereq.clone());
                break;
            }
        }
    }
    sequenced
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

/// N ≈ certify rate × round duration, clamped to [1, 30]. The pool that fills during one
/// round is the next round. Floor of 1, not a larger bootstrap number: no history yet means
/// `h` is `PoolHistory::default()`, and law-batcher-earns-the-round-by-parity's own rule is
/// to cut on the first certified member until there is real history to adapt from.
pub fn adaptive_n(h: PoolHistory) -> u32 {
    let raw = (h.certify_rate_per_min * h.round_duration_mins).round();
    if raw.is_nan() {
        return 1;
    }
    raw.clamp(1.0, 30.0) as u32
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
    /// A batch PR is open: the next round is prepared on its head, never opened until it lands.
    Prepare(u32),
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
    /// open-pr). While one is open any non-empty pool prepares the next round on that PR's
    /// head; it opens only once the PR has landed, and the open PR is never touched.
    pub batch_open: bool,
}

pub fn should_cut(t: &TriggerInputs) -> Option<TriggerReason> {
    if t.main_red {
        return Some(TriggerReason::MainRed);
    }
    if let Some(express) = t.pool.iter().find(|m| m.express) {
        return Some(TriggerReason::Express(express.id.clone()));
    }
    if t.pool.is_empty() {
        return None;
    }
    if t.batch_open {
        return Some(TriggerReason::Prepare(t.pool.len() as u32));
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
    /// The member's tip was already an ancestor of the round head before any merge was
    /// attempted — reset to an old base, or no commits of its own. `git merge` here would
    /// succeed as a no-op ("Already up to date"), which is indistinguishable from `Ok` by
    /// exit status alone, so the IO seam must classify this before ever shelling out.
    Empty,
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
    /// The member's tip was already in the round before it was ever merged — nothing of
    /// its own reaches the tree, so it must never be marked BATCHED or LANDED for it.
    Empty,
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
            Some(MergeResult::Empty) if stacked_into(input.pool, &m.id) => merged.push(m.clone()),
            Some(MergeResult::Empty) => set_aside.push(SetAside { id: m.id.clone(), reason: SetAsideReason::Empty }),
            _ => {
                let deleted = input.deleted_suites.get(&m.id).cloned().unwrap_or_default();
                set_aside.push(SetAside { id: m.id.clone(), reason: SetAsideReason::Conflict { deleted_suites: deleted } });
            }
        }
    }
    let merged = topo_order(&merged);
    Combined { merged, set_aside }
}

// ---------------------------------------------------------------------------------------
// F. Stale members: compare against when the base ref moved, not the base commit's own time
// ---------------------------------------------------------------------------------------

/// A batch merge commit is created before the batch lands, so the base head's own commit
/// time never looks newer than a member certified moments before. Compare instead against
/// the fetch that observed the ref move, so a retry fires the moment the base actually
/// changed underneath the member.
/// Consecutive base-conflict set-asides at which a certified member is withdrawn.
pub const CONFLICT_EJECT_ROUNDS: u32 = 2;

/// The consecutive-round count after another base-conflict set-aside of `tip`, given the
/// recorded `(tip, count)` — a different tip is a different member and starts over.
pub fn conflict_streak(prev: Option<(&str, u32)>, tip: &str) -> u32 {
    match prev {
        Some((t, n)) if t == tip => n + 1,
        _ => 1,
    }
}

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
        // A digit, or the full five-letter suffix current ids are minted with, keeps prose
        // like "sp-" alone or "sp-ish" from being mistaken for a bead id.
        let base = candidate[3..].split('.').next().unwrap_or("");
        if base.chars().any(|c| c.is_ascii_digit()) || (base.len() == 5 && base.bytes().all(|b| b.is_ascii_lowercase())) {
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
        TriggerReason::Prepare(n) => format!("{n} certified while a batch is open"),
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
        SetAsideReason::Empty => format!("EMPTY {}: tip is already in the round — not a member", set_aside.id),
    };
    ev("evicted", vec![set_aside.id.clone()], text)
}

pub fn abandoned_event(id: &Id, why: &str) -> Event {
    ev("abandoned", vec![id.clone()], format!("{id} abandoned: {why}"))
}

// ---------------------------------------------------------------------------------------
// Local red-suite attribution (sp-hqoap): a round that goes red on its own local corpus is
// never sent to CI. Attribution names, per red suite, either the member(s) whose tip
// reproduces it or BASE — the base itself, unrelated to any round member. What is pure here
// is just naming the two outcomes an ejection can carry; the attribution itself (targeted,
// without-member reruns of each red suite as it lands) is `attrib`'s own (sp-hvtgs,
// concurrent with the round) — attribute.sh, its bash predecessor, is retired (sp-uwhx0).
// ---------------------------------------------------------------------------------------

/// One member ejected from the round before it ever reached CI, and the suite(s) it was
/// found responsible for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ejection {
    pub id: Id,
    pub suites: Vec<Id>,
}

pub fn ejected_event(e: &Ejection) -> Event {
    ev("ejected", vec![e.id.clone()], format!("{} ejected: red on {} (local attribution, pre-PR)", e.id, e.suites.join(",")))
}

/// A suite red against the base itself blocks the whole round — no member is at fault, so
/// none is ejected — and is filed as an incident instead (base_fail_body).
pub fn base_fail_event(repo: &str, suites: &[Id]) -> Event {
    ev("basefail", vec![], format!("{repo}: base itself red on {} — held for Ops, no member ejected", suites.join(",")))
}

/// The body of the incident filed for a base-red or an unattributable local red: same shape
/// as judgement_body (repo, where, suites, evidence), but with no member list — a base fail
/// has no round member to name, and an unattributed red names none by definition.
pub fn base_fail_body(repo: &str, round_branch: &str, suites: &[Id], evidence: &str) -> String {
    format!(
        "Repo: {repo}\nRound branch: {round_branch}\nFailing suite(s): {}\nEvidence: {evidence}\n",
        suites.join(", ")
    )
}

// ---------------------------------------------------------------------------------------
// Judgement: a red the corpus or CI produced that this crate cannot resolve mechanically —
// re-run already happened (classify), delete-if-it-flips already didn't apply. What is left
// is Scope C: pin it to a member, fix it, sequence it behind a dependency, or revert main.
// That authority belongs to the summoned persona (sp-47kq1), not this crate; this core only
// says when a summon is warranted and what the summoned aeon needs to start.
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedSource {
    /// The full local corpus, run before a PR ever opens.
    Local,
    /// CI on an already-opened, batcher-owned PR (the producer is verdict.sh, sp-lomk3).
    Ci,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Judgement {
    pub source: RedSource,
    pub suites: Vec<Id>,
}

/// Every suite still DoubleRed after `classify`'s own re-run is a judgement call: re-run
/// already happened, so a flip would have shown up as `Flip`, not this.
pub fn judgement_for(verdicts: &[SuiteVerdict]) -> Option<Judgement> {
    let suites: Vec<Id> = verdicts.iter().filter(|v| v.classification == Classification::DoubleRed).map(|v| v.name.clone()).collect();
    if suites.is_empty() {
        None
    } else {
        Some(Judgement { source: RedSource::Local, suites })
    }
}

/// A CI-only red: the local corpus was green (or this member skipped it), CI came back red
/// on the batch PR anyway. `red_suites` is read off CI by the caller (verdict.sh, sp-lomk3);
/// this core only says what judgement looks like once it arrives.
pub fn judgement_for_ci(red_suites: &[Id]) -> Option<Judgement> {
    if red_suites.is_empty() {
        None
    } else {
        Some(Judgement { source: RedSource::Ci, suites: red_suites.to_vec() })
    }
}

/// Identity of a round for hold purposes: the same members at the same tips, whatever order
/// the pool listed them in.
pub fn round_key(members: &[(Id, String)]) -> String {
    let mut v: Vec<String> = members.iter().map(|(id, tip)| format!("{id}:{tip}")).collect();
    v.sort();
    v.join(" ")
}

/// The round-level commit for a mechanical integration fix: names every member so the landing
/// record carries each one's id.
pub fn integration_fix_message(fixes: &[&str], members: &[Id]) -> String {
    format!(
        "round: {} (integration fix for {})\n\nThe merged tree was red where no member was alone; fixed in the round, not in any member's branch.\nMembers: {}\n",
        fixes.join(", "),
        members.join(" "),
        members.join(" ")
    )
}

pub fn judgement_event(j: &Judgement, repo: &str) -> Event {
    let where_ = match j.source {
        RedSource::Local => "local corpus",
        RedSource::Ci => "CI",
    };
    ev("judgement", vec![], format!("{repo}: {where_} double-red ({}) — summoned batcher for judgement", j.suites.join(",")))
}

/// The body of the bead filed for the summoned aeon: everything a person doing this by hand
/// on 2026-09-24 had in front of them — repo, where the red was seen, which suites, which
/// members were in the round (a double-red's culprit is one of these), and where the
/// evidence lives (a local results dir, or a CI run link once sp-lomk3 supplies one).
// ---------------------------------------------------------------------------------------
// Terminal gate (sp-828tp, epic sp-hq9x8): the one check both queue.forge's PR-open and
// queue.local's land-local finish run before a round is allowed to leave this box. Nothing
// here shells out — `named_ids`/`bins_present` are the IO seam's own read of git and the
// --with-bins cache. Green-on-this-exact-head is stabilize_round's own control flow (only
// reaching this call once reds.is_empty() at that same head), not re-checked here: a
// `verdict_head` param once compared against `head` was fabricated from `head` itself at its
// only call site, so the comparison could never refuse — removed rather than left promising a
// check that was never wired (sp-j21fv).
// ---------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A round member has no merge commit reachable from `head` carrying its own certified
    /// tip as a parent — named_ids is the IO seam's own git-log-verified set, so this also
    /// catches a tip silently swapped for a different patch after the corpus went green.
    MemberNotNamed { id: Id },
    /// No --with-bins corpus artifacts for this head's own tree.
    BinsMissing,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::MemberNotNamed { id } => write!(f, "{id} is not named by any commit reachable from head"),
            Refusal::BinsMissing => write!(f, "no --with-bins corpus artifacts for this head"),
        }
    }
}

/// Refuses unless every one of `members` is in `named_ids` AND `bins_present`. Checked in that
/// order so the message a refusal prints is always the first thing actually wrong.
pub fn terminal_ready(members: &[Member], named_ids: &[Id], bins_present: bool) -> Result<(), Refusal> {
    for m in members {
        if !named_ids.iter().any(|id| id == &m.id) {
            return Err(Refusal::MemberNotNamed { id: m.id.clone() });
        }
    }
    if !bins_present {
        return Err(Refusal::BinsMissing);
    }
    Ok(())
}

pub fn judgement_body(repo: &str, j: &Judgement, members: &[Id], evidence: &str) -> String {
    let members_line = if members.is_empty() { "(none)".to_string() } else { members.join(", ") };
    let where_ = match j.source {
        RedSource::Local => "local corpus (pre-PR)",
        RedSource::Ci => "CI (batcher-owned batch PR)",
    };
    format!(
        "Repo: {repo}\nSource: {where_}\nFailing suite(s): {}\nRound member(s): {members_line}\nEvidence: {evidence}\n",
        j.suites.join(", ")
    )
}

#[cfg(test)]
#[path = "core/tests.rs"]
mod tests;
