//! The ask machine: an escalation to the operator is its own lifecycle, never a row on the
//! bead machine, so nothing that lists or claims work can ever see one. An ask is OPEN until
//! exactly one of three exits closes it, and every exit carries who closed it and the words
//! that did, so each close is auditable.

use crate::{Outcome, Refusal, Version};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AskState {
    Open,
    /// The operator's own answer, in his words, through a channel (pane, mail, chat).
    Answered,
    /// Dismissed: the default the ask carried was executed.
    DefaultTaken,
    /// Moot: the question stopped mattering.
    Withdrawn,
}

impl AskState {
    pub fn is_terminal(self) -> bool {
        self != AskState::Open
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AskState::Open => "OPEN",
            AskState::Answered => "ANSWERED",
            AskState::DefaultTaken => "DEFAULT_TAKEN",
            AskState::Withdrawn => "WITHDRAWN",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "OPEN" => AskState::Open,
            "ANSWERED" => AskState::Answered,
            "DEFAULT_TAKEN" => AskState::DefaultTaken,
            "WITHDRAWN" => AskState::Withdrawn,
            _ => return None,
        })
    }
}

/// The three exits. `quote` is the answer's words (for `Answered`), the default executed
/// (for `DefaultTaken`) or why it is moot (for `Withdrawn`); it may not be empty.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AskEventKind {
    Answer { quote: String, channel: String },
    TakeDefault { quote: String },
    Withdraw { quote: String },
}

impl AskEventKind {
    fn quote(&self) -> &str {
        match self {
            AskEventKind::Answer { quote, .. } | AskEventKind::TakeDefault { quote } | AskEventKind::Withdraw { quote } => quote,
        }
    }

    fn target(&self) -> AskState {
        match self {
            AskEventKind::Answer { .. } => AskState::Answered,
            AskEventKind::TakeDefault { .. } => AskState::DefaultTaken,
            AskEventKind::Withdraw { .. } => AskState::Withdrawn,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            AskEventKind::Answer { .. } => "Answer",
            AskEventKind::TakeDefault { .. } => "TakeDefault",
            AskEventKind::Withdraw { .. } => "Withdraw",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AskRow {
    pub ask_id: String,
    pub state: AskState,
    /// The work bead this ask holds, if any; its `ask` hold lifts when the ask closes.
    pub work_bead: Option<String>,
    /// Who closed it and with what words, once it is not OPEN.
    pub closed_by: Option<String>,
    pub quote: Option<String>,
    pub channel: Option<String>,
    pub version: Version,
}

impl AskRow {
    pub fn open(ask_id: impl Into<String>, work_bead: Option<String>) -> Self {
        AskRow { ask_id: ask_id.into(), state: AskState::Open, work_bead, closed_by: None, quote: None, channel: None, version: 0 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskEvent {
    pub expect: AskState,
    pub version: Version,
    pub kind: AskEventKind,
    pub actor: String,
}

pub fn apply(row: &AskRow, ev: &AskEvent) -> Outcome<AskRow> {
    if ev.expect != row.state {
        return Outcome::refuse(row.clone(), Refusal::ExpectMismatch { expected: ev.expect.as_str().into(), actual: row.state.as_str().into() });
    }
    if ev.version != row.version {
        return Outcome::refuse(row.clone(), Refusal::StaleVersion { given: ev.version, current: row.version });
    }
    if row.state.is_terminal() {
        return Outcome::refuse(row.clone(), Refusal::Terminal { state: row.state.as_str().into() });
    }
    if ev.actor.trim().is_empty() || ev.kind.quote().trim().is_empty() {
        return Outcome::refuse(
            row.clone(),
            Refusal::IllegalTransition { state: row.state.as_str().into(), event: format!("{} without an actor and a quoted answer", ev.kind.name()) },
        );
    }
    let mut next = row.clone();
    next.state = ev.kind.target();
    next.closed_by = Some(ev.actor.clone());
    next.quote = Some(ev.kind.quote().to_string());
    next.channel = match &ev.kind {
        AskEventKind::Answer { channel, .. } => Some(channel.clone()),
        _ => None,
    };
    next.version = row.version + 1;
    Outcome::applied(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: AskEventKind) -> AskEvent {
        AskEvent { expect: AskState::Open, version: 0, kind, actor: "concierge".into() }
    }
    fn answer() -> AskEventKind {
        AskEventKind::Answer { quote: "yes, ship it".into(), channel: "pane".into() }
    }

    #[test]
    fn each_exit_records_who_and_the_quoted_words() {
        let row = AskRow::open("sp-a", Some("sp-w".into()));
        let o = apply(&row, &ev(answer()));
        assert!(o.applied);
        assert_eq!((o.row.state, o.row.version), (AskState::Answered, 1));
        assert_eq!((o.row.closed_by.as_deref(), o.row.quote.as_deref(), o.row.channel.as_deref()), (Some("concierge"), Some("yes, ship it"), Some("pane")));
        let d = apply(&row, &ev(AskEventKind::TakeDefault { quote: "ran the default".into() }));
        assert_eq!(d.row.state, AskState::DefaultTaken);
        assert_eq!(d.row.channel, None);
        let w = apply(&row, &ev(AskEventKind::Withdraw { quote: "moot".into() }));
        assert_eq!(w.row.state, AskState::Withdrawn);
    }

    #[test]
    fn a_closed_ask_refuses_every_further_exit_and_is_unchanged() {
        let closed = apply(&AskRow::open("sp-a", None), &ev(answer())).row;
        for kind in [answer(), AskEventKind::Withdraw { quote: "moot".into() }] {
            let o = apply(&closed, &AskEvent { expect: AskState::Answered, version: 1, kind, actor: "x".into() });
            assert!(!o.applied);
            assert_eq!(o.refusal, Some(Refusal::Terminal { state: "ANSWERED".into() }));
            assert_eq!(o.row, closed);
        }
    }

    #[test]
    fn an_exit_without_words_or_an_actor_is_refused() {
        let row = AskRow::open("sp-a", None);
        assert!(!apply(&row, &ev(AskEventKind::Withdraw { quote: "  ".into() })).applied);
        assert!(!apply(&row, &AskEvent { actor: "".into(), ..ev(answer()) }).applied);
    }

    #[test]
    fn a_stale_version_or_expectation_is_refused() {
        let row = AskRow::open("sp-a", None);
        assert!(matches!(apply(&row, &AskEvent { version: 4, ..ev(answer()) }).refusal, Some(Refusal::StaleVersion { .. })));
        assert!(matches!(apply(&row, &AskEvent { expect: AskState::Answered, ..ev(answer()) }).refusal, Some(Refusal::ExpectMismatch { .. })));
    }

    #[test]
    fn states_round_trip_their_names() {
        for s in [AskState::Open, AskState::Answered, AskState::DefaultTaken, AskState::Withdrawn] {
            assert_eq!(AskState::from_str(s.as_str()), Some(s));
        }
    }
}
