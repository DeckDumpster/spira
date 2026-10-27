//! The MENDING machine: one row per (batch, pass, suite) failure, from the moment a mender is
//! picked for it to its outcome. The deadline is fixed at pickup from evidence the caller
//! supplies and no event moves it; past it the only transition is `Expire`, which the machine
//! applies as a return carrying whatever diagnosis exists.

use crate::{Outcome, Refusal, Version};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MendingState {
    Waiting,
    Mending,
    Mended,
    Environment,
    Flaky,
    Returned,
    Unsure,
}

impl MendingState {
    pub fn is_terminal(self) -> bool {
        !matches!(self, MendingState::Waiting | MendingState::Mending)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            MendingState::Waiting => "WAITING",
            MendingState::Mending => "MENDING",
            MendingState::Mended => "MENDED",
            MendingState::Environment => "ENVIRONMENT",
            MendingState::Flaky => "FLAKY",
            MendingState::Returned => "RETURNED",
            MendingState::Unsure => "UNSURE",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "WAITING" => MendingState::Waiting,
            "MENDING" => MendingState::Mending,
            "MENDED" => MendingState::Mended,
            "ENVIRONMENT" => MendingState::Environment,
            "FLAKY" => MendingState::Flaky,
            "RETURNED" => MendingState::Returned,
            "UNSURE" => MendingState::Unsure,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MendingRow {
    pub batch_id: String,
    pub pass: u32,
    pub suite: String,
    pub state: MendingState,
    pub picked_at: Option<i64>,
    pub deadline: Option<i64>,
    pub triage_ended_at: Option<i64>,
    pub diagnosis: Option<String>,
    pub version: Version,
}

impl MendingRow {
    /// Opened by the batch's `Failed` event in the same transaction; no prior row to swap.
    pub fn failed(batch_id: impl Into<String>, pass: u32, suite: impl Into<String>) -> Self {
        MendingRow {
            batch_id: batch_id.into(),
            pass,
            suite: suite.into(),
            state: MendingState::Waiting,
            picked_at: None,
            deadline: None,
            triage_ended_at: None,
            diagnosis: None,
            version: 0,
        }
    }

    pub fn key(&self) -> String {
        key(&self.batch_id, self.pass, &self.suite)
    }

    /// Only a MENDING row carries a deadline that can be overdue; WAITING has none.
    pub fn overdue(&self, now: i64) -> bool {
        self.state == MendingState::Mending && self.deadline.is_some_and(|d| now >= d)
    }
}

pub fn key(batch_id: &str, pass: u32, suite: &str) -> String {
    format!("{batch_id}#{pass}#{suite}")
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MendingEventKind {
    /// WAITING -> MENDING: a mender took the failure. The deadline is `at + deadline_s`.
    Pickup { at: i64, deadline_s: u64 },
    /// Stamps when triage ended, once, while MENDING.
    TriageEnded { at: i64 },
    /// Replaces the diagnosis written so far. Moves no state and no deadline.
    Diagnosis { text: String },
    Mended { at: i64 },
    Environment { at: i64 },
    Flaky { at: i64 },
    Unsure { at: i64 },
    /// The mender's own return, carrying its diagnosis.
    Return { at: i64, diagnosis: String },
    /// MENDING -> RETURNED once `at` has reached the deadline, with the diagnosis so far.
    Expire { at: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MendingEvent {
    pub expect: MendingState,
    pub version: Version,
    pub kind: MendingEventKind,
    pub actor: String,
}

/// Actors that may record an outcome for a failure no mender picked.
pub fn may_settle_unpicked(actor: &str) -> bool {
    matches!(actor, "concierge" | "operator")
}

fn illegal(row: &MendingRow, kind: &MendingEventKind) -> Outcome<MendingRow> {
    Outcome::refuse(row.clone(), Refusal::IllegalTransition { state: row.state.as_str().to_string(), event: format!("{kind:?}") })
}

pub fn apply(row: &MendingRow, ev: &MendingEvent) -> Outcome<MendingRow> {
    if ev.version != row.version {
        return Outcome::refuse(row.clone(), Refusal::StaleVersion { given: ev.version, current: row.version });
    }
    if ev.expect != row.state {
        return Outcome::refuse(
            row.clone(),
            Refusal::ExpectMismatch { expected: ev.expect.as_str().to_string(), actual: row.state.as_str().to_string() },
        );
    }
    if row.state.is_terminal() {
        return Outcome::refuse(row.clone(), Refusal::Terminal { state: row.state.as_str().to_string() });
    }
    let kind = &ev.kind;
    let mut new = row.clone();
    new.version += 1;
    let past_deadline = |at: i64| row.deadline.filter(|&d| at >= d);
    let settle = |mut new: MendingRow, state: MendingState, at: i64| match past_deadline(at) {
        Some(deadline) => Outcome::refuse(row.clone(), Refusal::DeadlinePassed { deadline, at }),
        None => {
            new.state = state;
            Outcome::applied(new)
        }
    };
    let triager = may_settle_unpicked(&ev.actor);
    match (row.state, kind) {
        (MendingState::Waiting, MendingEventKind::TriageEnded { at }) if triager && row.triage_ended_at.is_none() => {
            new.triage_ended_at = Some(*at);
            Outcome::applied(new)
        }
        (MendingState::Waiting, MendingEventKind::Diagnosis { text }) if triager => {
            new.diagnosis = Some(text.clone());
            Outcome::applied(new)
        }
        (MendingState::Waiting, MendingEventKind::Mended { at }) if triager => settle(new, MendingState::Mended, *at),
        (MendingState::Waiting, MendingEventKind::Environment { at }) if triager => settle(new, MendingState::Environment, *at),
        (MendingState::Waiting, MendingEventKind::Flaky { at }) if triager => settle(new, MendingState::Flaky, *at),
        (MendingState::Waiting, MendingEventKind::Unsure { at }) if triager => settle(new, MendingState::Unsure, *at),
        (MendingState::Waiting, MendingEventKind::Return { at, diagnosis }) if triager => {
            new.diagnosis = Some(diagnosis.clone());
            settle(new, MendingState::Returned, *at)
        }
        (MendingState::Waiting, MendingEventKind::Pickup { at, deadline_s }) => {
            new.state = MendingState::Mending;
            new.picked_at = Some(*at);
            new.deadline = Some(at.saturating_add(i64::try_from(*deadline_s).unwrap_or(i64::MAX)));
            Outcome::applied(new)
        }
        (MendingState::Mending, MendingEventKind::TriageEnded { at }) if row.triage_ended_at.is_none() => match past_deadline(*at) {
            Some(deadline) => Outcome::refuse(row.clone(), Refusal::DeadlinePassed { deadline, at: *at }),
            None => {
                new.triage_ended_at = Some(*at);
                Outcome::applied(new)
            }
        },
        (MendingState::Mending, MendingEventKind::Diagnosis { text }) => {
            new.diagnosis = Some(text.clone());
            Outcome::applied(new)
        }
        (MendingState::Mending, MendingEventKind::Mended { at }) => settle(new, MendingState::Mended, *at),
        (MendingState::Mending, MendingEventKind::Environment { at }) => settle(new, MendingState::Environment, *at),
        (MendingState::Mending, MendingEventKind::Flaky { at }) => settle(new, MendingState::Flaky, *at),
        (MendingState::Mending, MendingEventKind::Unsure { at }) => settle(new, MendingState::Unsure, *at),
        (MendingState::Mending, MendingEventKind::Return { at, diagnosis }) => {
            new.diagnosis = Some(diagnosis.clone());
            settle(new, MendingState::Returned, *at)
        }
        (MendingState::Mending, MendingEventKind::Expire { at }) if row.overdue(*at) => {
            new.state = MendingState::Returned;
            Outcome::applied(new)
        }
        _ => illegal(row, kind),
    }
}

/// Mends one bead may carry in total; a further failure returns it instead.
pub const MEND_CAP_PER_BEAD: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileChange {
    pub path: String,
    pub added: u32,
    pub deleted: u32,
}

/// What a `mended` claims and the git facts around it, assembled by the caller from the
/// members' branches; `assess` is the only judge of them.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct MendFacts {
    /// Every file the mend changed, across all the mended members.
    pub changes: Vec<FileChange>,
    /// Every file in any member's own diff against the base.
    pub member_files: Vec<String>,
    /// `crate:<dir>` / `suite:<path>` the touched files require green.
    pub required_checks: Vec<String>,
    /// The checks the mender recorded green.
    pub checked: Vec<String>,
    pub ask: Option<String>,
}

fn is_fence(path: &str, fences: &[String]) -> bool {
    fences.iter().any(|f| match f.strip_suffix('*') {
        Some(prefix) => path.starts_with(prefix),
        None => path.starts_with(f.as_str()) && (f.ends_with('/') || path == f),
    })
}

fn is_test_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    path.starts_with("tests/") || path.contains("/tests/") || name.starts_with("test-") || name.ends_with("_test.rs")
}

/// The reasons a mend must carry an ask: gate or fence code, a test's expectation changed
/// (an existing test file with lines removed), or a file outside every member's diff.
pub fn escalations(facts: &MendFacts, fences: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for c in &facts.changes {
        if is_fence(&c.path, fences) {
            out.push(format!("{} is gate or fence code", c.path));
        } else if is_test_path(&c.path) && c.deleted > 0 {
            out.push(format!("{} changes a test's expectation ({} lines removed)", c.path, c.deleted));
        }
        if !facts.member_files.iter().any(|f| *f == c.path) {
            out.push(format!("{} is outside every member's diff", c.path));
        }
    }
    out
}

/// Checks required but not recorded green; an ask never waives these.
pub fn missing_checks(facts: &MendFacts) -> Vec<String> {
    facts.required_checks.iter().filter(|r| !facts.checked.contains(r)).map(|r| format!("no green check recorded for {r}")).collect()
}

pub fn assess(facts: &MendFacts, fences: &[String]) -> Result<(), Refusal> {
    let mut reasons = missing_checks(facts);
    if facts.ask.as_deref().is_none_or(str::is_empty) {
        reasons.extend(escalations(facts, fences).into_iter().map(|r| format!("{r}: attach the ask that reports it")));
    }
    if reasons.is_empty() { Ok(()) } else { Err(Refusal::MendRefused { reasons }) }
}

/// Which checks a set of touched files requires. `crate_of` names the crate directory a file
/// belongs to; `suites_of` the suites that reference a non-Rust file.
pub fn required_checks(paths: &[String], crate_of: impl Fn(&str) -> Option<String>, suites_of: impl Fn(&str) -> Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in paths {
        if let Some(c) = crate_of(p) {
            out.push(format!("crate:{c}"));
        }
        let rust = p.ends_with(".rs") || p.ends_with("Cargo.toml") || p.ends_with("Cargo.lock");
        if !rust {
            out.extend(suites_of(p).into_iter().map(|s| format!("suite:{s}")));
        }
        let name = p.rsplit('/').next().unwrap_or(p);
        if name.starts_with("test-") && name.ends_with(".sh") {
            out.push(format!("suite:{p}"));
        }
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picked(at: i64) -> MendingRow {
        let r = MendingRow::failed("b1", 1, "test-x.sh");
        step(&r, MendingEventKind::Pickup { at, deadline_s: 300 })
    }

    fn step(r: &MendingRow, kind: MendingEventKind) -> MendingRow {
        let out = apply(r, &MendingEvent { expect: r.state, version: r.version, kind: kind.clone(), actor: "t".into() });
        assert!(out.applied, "{kind:?} refused at {:?}: {:?}", r.state, out.refusal);
        out.row
    }

    fn refusal(r: &MendingRow, kind: MendingEventKind) -> Refusal {
        let out = apply(r, &MendingEvent { expect: r.state, version: r.version, kind, actor: "t".into() });
        assert!(!out.applied);
        assert_eq!(out.row, *r);
        out.refusal.unwrap()
    }

    #[test]
    fn pickup_stamps_a_deadline_from_the_pickup_not_from_the_failure() {
        let r = picked(1000);
        assert_eq!((r.state, r.picked_at, r.deadline), (MendingState::Mending, Some(1000), Some(1300)));
        assert_eq!(MendingRow::failed("b1", 1, "s").deadline, None, "a waiting failure has no clock");
    }

    #[test]
    fn nothing_the_mender_sends_moves_the_deadline() {
        let r = picked(1000);
        let r = step(&r, MendingEventKind::TriageEnded { at: 1050 });
        let r = step(&r, MendingEventKind::Diagnosis { text: "x".into() });
        assert_eq!((r.deadline, r.triage_ended_at), (Some(1300), Some(1050)));
        assert!(matches!(refusal(&r, MendingEventKind::TriageEnded { at: 1060 }), Refusal::IllegalTransition { .. }), "triage ends once");
        assert!(matches!(refusal(&r, MendingEventKind::Pickup { at: 1200, deadline_s: 300 }), Refusal::IllegalTransition { .. }));
    }

    #[test]
    fn every_outcome_settles_a_row_before_its_deadline() {
        let outcomes = [
            (MendingEventKind::Mended { at: 1299 }, MendingState::Mended),
            (MendingEventKind::Environment { at: 1299 }, MendingState::Environment),
            (MendingEventKind::Flaky { at: 1299 }, MendingState::Flaky),
            (MendingEventKind::Unsure { at: 1299 }, MendingState::Unsure),
            (MendingEventKind::Return { at: 1299, diagnosis: "d".into() }, MendingState::Returned),
        ];
        for (kind, state) in outcomes {
            assert_eq!(step(&picked(1000), kind).state, state);
        }
        let r = step(&picked(1000), MendingEventKind::Return { at: 1299, diagnosis: "d".into() });
        assert_eq!(r.diagnosis.as_deref(), Some("d"));
    }

    #[test]
    fn an_outcome_at_or_past_the_deadline_is_refused() {
        for kind in [
            MendingEventKind::Mended { at: 1300 },
            MendingEventKind::Environment { at: 1300 },
            MendingEventKind::Flaky { at: 1300 },
            MendingEventKind::Unsure { at: 1300 },
            MendingEventKind::Return { at: 1300, diagnosis: "late".into() },
            MendingEventKind::TriageEnded { at: 1300 },
        ] {
            assert_eq!(refusal(&picked(1000), kind), Refusal::DeadlinePassed { deadline: 1300, at: 1300 });
        }
    }

    #[test]
    fn expire_returns_with_the_diagnosis_written_so_far_and_only_when_overdue() {
        let r = step(&picked(1000), MendingEventKind::Diagnosis { text: "half a diagnosis".into() });
        assert!(matches!(refusal(&r, MendingEventKind::Expire { at: 1299 }), Refusal::IllegalTransition { .. }));
        assert!(!r.overdue(1299) && r.overdue(1300));
        let r = step(&r, MendingEventKind::Expire { at: 1300 });
        assert_eq!((r.state, r.diagnosis.as_deref()), (MendingState::Returned, Some("half a diagnosis")));
        assert!(matches!(refusal(&r, MendingEventKind::Expire { at: 9999 }), Refusal::Terminal { .. }));
    }

    #[test]
    fn a_waiting_row_never_expires_and_has_no_outcome() {
        let r = MendingRow::failed("b1", 1, "s");
        assert!(!r.overdue(i64::MAX));
        for kind in [MendingEventKind::Expire { at: i64::MAX }, MendingEventKind::Mended { at: 1 }, MendingEventKind::Diagnosis { text: "x".into() }] {
            assert!(matches!(refusal(&r, kind), Refusal::IllegalTransition { .. }));
        }
    }

    #[test]
    fn the_concierge_and_operator_settle_an_unpicked_failure_and_no_one_else_does() {
        let r = MendingRow::failed("b1", 1, "s");
        for actor in ["concierge", "operator"] {
            for (kind, state) in [
                (MendingEventKind::Environment { at: 5 }, MendingState::Environment),
                (MendingEventKind::Flaky { at: 5 }, MendingState::Flaky),
                (MendingEventKind::Unsure { at: 5 }, MendingState::Unsure),
                (MendingEventKind::Mended { at: 5 }, MendingState::Mended),
                (MendingEventKind::Return { at: 5, diagnosis: "d".into() }, MendingState::Returned),
            ] {
                let out = apply(&r, &MendingEvent { expect: r.state, version: r.version, kind, actor: actor.into() });
                assert!(out.applied && out.row.state == state, "{actor} {state:?}: {:?}", out.refusal);
            }
        }
        for actor in ["dwarf-x", "unknown", "Concierge"] {
            let kind = MendingEventKind::Environment { at: 5 };
            let out = apply(&r, &MendingEvent { expect: r.state, version: r.version, kind, actor: actor.into() });
            assert!(matches!(out.refusal, Some(Refusal::IllegalTransition { .. })), "{actor}");
        }
    }

    #[test]
    fn stale_version_and_expect_mismatch_never_mutate() {
        let r = picked(1000);
        let out = apply(&r, &MendingEvent { expect: r.state, version: 0, kind: MendingEventKind::Expire { at: 5000 }, actor: "t".into() });
        assert!(matches!(out.refusal, Some(Refusal::StaleVersion { .. })) && out.row == r);
        let out = apply(&r, &MendingEvent { expect: MendingState::Waiting, version: r.version, kind: MendingEventKind::Expire { at: 5000 }, actor: "t".into() });
        assert!(matches!(out.refusal, Some(Refusal::ExpectMismatch { .. })) && out.row == r);
    }

    fn ch(path: &str, added: u32, deleted: u32) -> FileChange {
        FileChange { path: path.into(), added, deleted }
    }

    fn facts(changes: Vec<FileChange>) -> MendFacts {
        MendFacts { member_files: changes.iter().map(|c| c.path.clone()).collect(), changes, ..Default::default() }
    }

    fn fences() -> Vec<String> {
        vec!["gate/".into(), "spira/gate-suites".into(), "spira/inventory*".into()]
    }

    #[test]
    fn a_fence_edit_without_an_ask_is_refused_and_with_one_taken() {
        let mut f = facts(vec![ch("gate/src/lib.rs", 3, 1)]);
        let Err(Refusal::MendRefused { reasons }) = assess(&f, &fences()) else { panic!("a fence edit passed without an ask") };
        assert!(reasons[0].contains("gate or fence code"), "{reasons:?}");
        f.ask = Some("ask-1".into());
        assert_eq!(assess(&f, &fences()), Ok(()));
    }

    #[test]
    fn a_trailing_star_fences_a_prefix_and_a_bare_entry_only_itself() {
        assert!(is_fence("spira/inventory-deny", &fences()) && is_fence("spira/gate-suites", &fences()));
        assert!(!is_fence("spira/gate-suites-extra", &fences()) && !is_fence("gates/x", &fences()));
    }

    #[test]
    fn a_plain_in_diff_source_edit_needs_no_ask() {
        assert_eq!(assess(&facts(vec![ch("queue/src/ops/round.rs", 5, 5)]), &fences()), Ok(()));
    }

    #[test]
    fn a_changed_expectation_and_an_outside_edit_each_need_an_ask() {
        assert_eq!(escalations(&facts(vec![ch("queue/src/tests/round.rs", 2, 2)]), &fences()).len(), 1);
        assert!(escalations(&facts(vec![ch("spira/test-x.sh", 9, 0)]), &fences()).is_empty(), "adding to a test is not changing its expectation");
        let mut f = facts(vec![ch("lib/other.rs", 1, 0)]);
        f.member_files = vec!["lib/mine.rs".into()];
        assert_eq!(escalations(&f, &fences()), vec!["lib/other.rs is outside every member's diff".to_string()]);
    }

    #[test]
    fn a_tip_without_its_green_checks_is_refused_even_with_an_ask() {
        let mut f = facts(vec![ch("queue/src/lib.rs", 1, 0)]);
        f.required_checks = vec!["crate:queue".into(), "suite:spira/test-a.sh".into()];
        f.checked = vec!["crate:queue".into()];
        f.ask = Some("ask-1".into());
        let Err(Refusal::MendRefused { reasons }) = assess(&f, &fences()) else { panic!("missing check accepted") };
        assert_eq!(reasons, vec!["no green check recorded for suite:spira/test-a.sh".to_string()]);
        f.checked.push("suite:spira/test-a.sh".into());
        assert_eq!(assess(&f, &fences()), Ok(()));
    }

    #[test]
    fn required_checks_name_crates_and_the_suites_that_reference_non_rust_files() {
        let paths = vec!["queue/src/a.rs".to_string(), "spira/lib.sh".to_string(), "spira/test-b.sh".to_string()];
        let got = required_checks(
            &paths,
            |p| p.strip_suffix("/src/a.rs").map(String::from),
            |p| if p == "spira/lib.sh" { vec!["spira/test-a.sh".into()] } else { vec![] },
        );
        assert_eq!(got, vec!["crate:queue", "suite:spira/test-a.sh", "suite:spira/test-b.sh"]);
    }
}
