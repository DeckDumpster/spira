//! The page rule: whether a gap the alarms component sees sends a page, adds a note to the open
//! ask, or says nothing. A page's identity is its section, its open condition types and the
//! decision it asks; the steps tried are evidence, never identity. At most one page goes out per
//! (section, condition type) per [`REMINDER_S`]; a condition declared page-on-raise pages once per
//! raise. State is kept in `incident alarm`'s file shape: one `<ref>\t<epoch>` row each.

use sha2::{Digest, Sha256};

pub use crate::alarm::REMINDER_S;

/// Rows older than this are dropped: past every window, and past any raise worth remembering.
pub const KEEP_S: i64 = 7 * 86_400;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// An open condition no declared action addresses, from the pass record.
    NoLever,
    /// Every candidate failed inside its back-off, from the pass record.
    Exhausted,
    /// A condition the kind declares page-on-raise, raised at `since`.
    Raised { since: String },
    /// The goodput pass's record is older than the dead-man allows.
    DeadMan,
}

/// One decision a page asks, with the default taken if nobody answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub question: String,
    pub default: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub section: String,
    pub source: Source,
    /// Sorted and deduplicated.
    pub conditions: Vec<String>,
    pub decision: Decision,
    /// The escalation class the section's kind asks under; `None` goes to the Concierge.
    pub class: Option<String>,
    /// What the body shows: each condition with its reason and age, the distance.
    pub facts: Vec<String>,
    /// Each step tried and its result.
    pub steps: Vec<String>,
}

impl Candidate {
    pub fn new(section: &str, source: Source, conditions: Vec<String>, decision: Decision) -> Candidate {
        let mut conditions = conditions;
        conditions.sort();
        conditions.dedup();
        Candidate { section: section.into(), source, conditions, decision, class: None, facts: Vec::new(), steps: Vec::new() }
    }

    pub fn identity(&self) -> String {
        let raise = match &self.source {
            Source::Raised { since } => since.as_str(),
            _ => "",
        };
        hash(&[&self.section, &self.conditions.join(","), &self.decision.question, raise])
    }

    pub fn evidence(&self) -> String {
        hash(&[&self.steps.join("\n")])
    }

    /// The row key for one of its conditions: the source keeps a raise's rows and each pass-record
    /// source's window apart, so one never resets or notes over another.
    fn key(&self, condition: &str) -> String {
        let source = match self.source {
            Source::NoLever => "nolever",
            Source::Exhausted => "exhausted",
            Source::Raised { .. } => "raised",
            Source::DeadMan => "deadman",
        };
        format!("{source}:{condition}")
    }

    fn owns(&self, r: &Row) -> bool {
        r.section == self.section && self.conditions.iter().any(|c| self.key(c) == r.condition)
    }

    /// The reference an attempt to send this page is recorded under.
    pub fn reference(&self) -> String {
        format!("page:{}:{}", self.section, self.conditions.join(","))
    }

    pub fn subject(&self) -> String {
        format!("alarm: {} — {}", self.section, self.conditions.join(", "))
    }

    /// The condition, its distance and age, each step tried with its result, one decision with its
    /// default, and the runbook entry for each condition.
    pub fn body(&self) -> String {
        let mut b = format!("## Question\n{}\n\n## Default\n{}\n\n## Evidence\nsection: {}\n", self.decision.question, self.decision.default, self.section);
        for f in &self.facts {
            b.push_str(&format!("{f}\n"));
        }
        b.push_str("\n## Steps tried\n");
        if self.steps.is_empty() {
            b.push_str("none\n");
        }
        for s in &self.steps {
            b.push_str(&format!("- {s}\n"));
        }
        b.push_str("\n## Runbook\n");
        for c in &self.conditions {
            b.push_str(&format!("- {c}: {}\n", runbook(c)));
        }
        if let Some(class) = &self.class {
            b.push_str(&format!("\n## Class basis\nThe {} section's kind escalates its pages as {class}.\n", self.section));
        }
        b
    }
}

/// The runbook entry for a condition type.
pub fn runbook(condition: &str) -> String {
    format!("docs/runbooks/reconciler.md#{}", condition.to_ascii_lowercase())
}

fn hash(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0u8]);
    }
    h.finalize().iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// One row: what was paged, under which identity and evidence, as which ask, and when it was sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub section: String,
    pub condition: String,
    pub identity: String,
    pub evidence: String,
    pub ask: String,
    pub sent: i64,
}

impl Row {
    fn reference(&self) -> String {
        [&self.section, &self.condition, &self.identity, &self.evidence, &self.ask].map(|s| s.replace(['|', '\t', '\n'], " ")).join("|")
    }
}

pub fn parse(state: &str) -> Vec<Row> {
    state
        .lines()
        .filter_map(|l| {
            let (r, t) = l.rsplit_once('\t')?;
            let p: Vec<&str> = r.splitn(5, '|').collect();
            let [section, condition, identity, evidence, ask] = p.as_slice() else { return None };
            Some(Row { section: section.to_string(), condition: condition.to_string(), identity: identity.to_string(), evidence: evidence.to_string(), ask: ask.to_string(), sent: t.parse().ok()? })
        })
        .collect()
}

pub fn render(rows: &[Row]) -> String {
    rows.iter().map(|r| format!("{}\t{}\n", r.reference(), r.sent)).collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Send,
    /// Inside the window with a changed identity or changed evidence: a note on the open ask.
    Note { ask: String },
    Quiet,
}

/// What `c` does at `now` given the rows already kept.
pub fn decide(rows: &[Row], c: &Candidate, now: i64) -> Verdict {
    let identity = c.identity();
    if matches!(c.source, Source::Raised { .. }) {
        let seen = rows.iter().any(|r| c.owns(r) && r.identity == identity);
        return if seen { Verdict::Quiet } else { Verdict::Send };
    }
    let live: Vec<&Row> = rows.iter().filter(|r| c.owns(r) && now - r.sent < REMINDER_S).collect();
    if c.conditions.iter().any(|cond| !live.iter().any(|r| r.condition == c.key(cond))) {
        return Verdict::Send;
    }
    let evidence = c.evidence();
    if live.iter().all(|r| r.identity == identity && r.evidence == evidence) {
        return Verdict::Quiet;
    }
    Verdict::Note { ask: live.iter().map(|r| r.ask.clone()).find(|a| !a.is_empty()).unwrap_or_default() }
}

/// The rows after `c` was sent as `ask` at `now`: each of its conditions' rows replaced.
pub fn sent(rows: &[Row], c: &Candidate, ask: &str, now: i64) -> Vec<Row> {
    let raised = matches!(c.source, Source::Raised { .. });
    let mut out: Vec<Row> = rows
        .iter()
        .filter(|r| now - r.sent < KEEP_S)
        .filter(|r| raised || !c.owns(r))
        .cloned()
        .collect();
    for cond in &c.conditions {
        out.push(Row { section: c.section.clone(), condition: c.key(cond), identity: c.identity(), evidence: c.evidence(), ask: ask.into(), sent: now });
    }
    out
}

/// The rows after a note on the open ask: the window keeps its start, and the identity and
/// evidence move to what was noted, so the same note is not added twice.
pub fn noted(rows: &[Row], c: &Candidate, now: i64) -> Vec<Row> {
    rows.iter()
        .map(|r| {
            let mut r = r.clone();
            if c.owns(&r) && now - r.sent < REMINDER_S {
                r.identity = c.identity();
                r.evidence = c.evidence();
            }
            r
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nolever(conds: &[&str], steps: &[&str]) -> Candidate {
        let mut c = Candidate::new(
            "RoundController/repo-b",
            Source::NoLever,
            conds.iter().map(|s| s.to_string()).collect(),
            Decision { question: "q".into(), default: "d".into() },
        );
        c.steps = steps.iter().map(|s| s.to_string()).collect();
        c
    }

    #[test]
    fn identity_is_section_conditions_and_decision_never_the_steps() {
        let a = nolever(&["B", "A"], &["hold-member: refused"]);
        let b = nolever(&["A", "B", "A"], &["concurrency: failed"]);
        assert_eq!(a.conditions, ["A", "B"]);
        assert_eq!(a.identity(), b.identity());
        assert_ne!(a.evidence(), b.evidence());
        let mut c = a.clone();
        c.decision.question = "other".into();
        assert_ne!(a.identity(), c.identity());
    }

    #[test]
    fn rows_round_trip_and_a_malformed_row_is_dropped() {
        let c = nolever(&["A"], &[]);
        let rows = sent(&[], &c, "sp-ask", 1000);
        assert_eq!(parse(&render(&rows)), rows);
        assert!(parse("junk\nx|y\t5\na|b|c|d|e\tnot-a-number\n").is_empty());
    }

    #[test]
    fn a_page_is_capped_per_condition_type_and_a_changed_one_is_a_note() {
        let c = nolever(&["A"], &["s1"]);
        assert_eq!(decide(&[], &c, 1000), Verdict::Send);
        let rows = sent(&[], &c, "sp-ask", 1000);
        assert_eq!(decide(&rows, &c, 1060), Verdict::Quiet);
        let changed = nolever(&["A"], &["s2"]);
        assert_eq!(decide(&rows, &changed, 1120), Verdict::Note { ask: "sp-ask".into() });
        let rows = noted(&rows, &changed, 1120);
        assert_eq!(decide(&rows, &changed, 1180), Verdict::Quiet, "a note is added once");
        assert_eq!(rows[0].sent, 1000, "a note keeps the window's start");
        assert_eq!(decide(&rows, &nolever(&["A", "B"], &["s2"]), 1200), Verdict::Send, "a second condition type pages");
        assert_eq!(decide(&rows, &c, 1000 + REMINDER_S), Verdict::Send, "the window ends");
    }

    #[test]
    fn a_raise_pages_once_whatever_the_cap_and_a_new_raise_pages_again() {
        let raise = |since: &str| Candidate::new("RoundController/r", Source::Raised { since: since.into() }, vec!["LaneBlocked".into()], Decision { question: "q".into(), default: "d".into() });
        let rows = sent(&[], &raise("t1"), "", 1000);
        assert_eq!(decide(&rows, &raise("t1"), 1060), Verdict::Quiet);
        assert_eq!(decide(&rows, &raise("t2"), 1120), Verdict::Send, "a raise is not capped by the window");
        let rows = sent(&rows, &raise("t2"), "", 1120);
        assert_eq!(rows.len(), 2, "a raise keeps the earlier raise's row");
        assert!(sent(&rows, &raise("t3"), "", 1000 + KEEP_S).len() == 2, "rows past KEEP_S are dropped");
    }

    #[test]
    fn a_raise_and_a_pass_record_entry_on_one_section_keep_their_own_rows() {
        let raise = Candidate::new("RoundController/repo-b", Source::Raised { since: "t1".into() }, vec!["A".into()], Decision { question: "r".into(), default: "d".into() });
        let lever = nolever(&["A"], &["s1"]);
        let mut exhausted = nolever(&["A"], &["s1"]);
        exhausted.source = Source::Exhausted;
        exhausted.decision.question = "e".into();
        let mut rows = Vec::new();
        for c in [&lever, &raise, &exhausted] {
            assert_eq!(decide(&rows, c, 1000), Verdict::Send);
            rows = sent(&rows, c, "sp-ask", 1000);
        }
        for t in [1060, 1120, 1180] {
            for c in [&lever, &raise, &exhausted] {
                assert_eq!(decide(&rows, c, t), Verdict::Quiet, "{:?} at {t}", c.source);
            }
        }
    }

    #[test]
    fn the_body_carries_the_evidence_one_decision_and_the_runbook() {
        let mut c = nolever(&["StackedOnUndrivenParent"], &["hold-member: refused (no-hold-target)"]);
        c.facts.push("distance: 3.0".into());
        let b = c.body();
        for want in ["## Question\nq", "## Default\nd", "distance: 3.0", "- hold-member: refused", "docs/runbooks/reconciler.md#stackedonundrivenparent"] {
            assert!(b.contains(want), "{want} missing from {b}");
        }
        assert!(!b.contains("## Class basis"));
        c.class = Some("policy".into());
        c.steps.clear();
        let b = c.body();
        assert!(b.contains("## Class basis") && b.contains("none\n"), "{b}");
        assert_eq!(c.reference(), "page:RoundController/repo-b:StackedOnUndrivenParent");
        assert!(c.subject().contains("RoundController/repo-b"));
    }
}
