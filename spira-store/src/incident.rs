//! An unfiled incident as its control-store row, and the rules that move it. The store verb
//! (`spira-lc incidents`) and the intake's in-memory fake both apply these, so they cannot
//! disagree about what a raise, a claim or a record does.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The most bytes of a payload a row keeps, the cut marker included.
pub const BODY_CAP: usize = 16 * 1024;
const TITLE_CAP: usize = 1_000;
const REASON_CAP: usize = 2_000;
const REF_MAX: usize = 160;

/// The first 8 hex chars of the reference's sha256, as the `ref:` label carries it.
pub fn ref_hash(reference: &str) -> String {
    let digest = Sha256::digest(reference.as_bytes());
    digest.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// external_ref is a fixed-width column, so an overlong key keeps a readable prefix plus the
/// hash of the whole.
pub fn bound_ref(reference: &str) -> String {
    if reference.chars().count() <= REF_MAX {
        return reference.to_string();
    }
    let head: String = reference.chars().take(REF_MAX - 9).collect();
    format!("{head}:{}", ref_hash(reference))
}

/// A natural key as one store key component: `%` and `/` are escaped, so it never splits.
pub fn component(raw: &str) -> String {
    raw.replace('%', "%25").replace('/', "%2F")
}

/// The key of `reference`'s row, under the store's incidents prefix.
pub fn key(reference: &str) -> String {
    component(&bound_ref(reference))
}

/// What a raise carries: everything a later filing, in another process, needs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Payload {
    pub reference: String,
    pub title: String,
    pub body: String,
    pub unit: String,
    pub path: String,
    pub cause: String,
    pub repo: Option<String>,
    pub kind: String,
    pub priority: String,
    pub express: bool,
    pub actor: Option<String>,
    pub labels: String,
    pub fenrir_exempt: bool,
    /// How far back this filing's dedup looks, when further than the configured window.
    pub lookback_days: Option<i64>,
}

/// `text` cut to `max` bytes on a character boundary.
fn cut(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

impl Payload {
    /// The payload as a row holds it: reference bounded, title and body capped. Idempotent.
    pub fn bounded(mut self) -> Payload {
        self.reference = bound_ref(&self.reference);
        self.title = cut(&self.title, TITLE_CAP).to_string();
        if self.body.len() > BODY_CAP {
            let marker = format!("\n[body capped at {BODY_CAP} bytes: {} were raised]\n", self.body.len());
            self.body = format!("{}{marker}", cut(&self.body, BODY_CAP - marker.len()));
        }
        self
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    #[default]
    Pending,
    Filing,
    Refused,
    Unread,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Filing => "filing",
            State::Refused => "refused",
            State::Unread => "unread",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub by: String,
    pub until: i64,
    /// The row's event count when it was claimed: a higher one at `record` is news still unfiled.
    pub events: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Row {
    #[serde(flatten)]
    pub payload: Payload,
    pub events: u64,
    pub first_at: i64,
    pub last_at: i64,
    pub state: State,
    pub reason: Option<String>,
    /// The repository's admitted lanes a refusal was judged against.
    pub lanes: Option<String>,
    pub retry_at: Option<i64>,
    pub claim: Option<Claim>,
    pub bead: Option<String>,
    /// Consecutive filings that could not reach the content store: the retry delay doubles on it.
    pub tries: u32,
}

impl Row {
    /// Why the row is not being filed now, as a refusal or an error names it.
    pub fn standing(&self) -> String {
        match &self.reason {
            Some(why) => format!("state {}: {why}", self.state.as_str()),
            None => format!("state {}", self.state.as_str()),
        }
    }
}

/// The row after a raise: a new one is pending; a held one takes the newest payload and one
/// more event and keeps its state, so a raise never adds a filing attempt.
pub fn raise(current: Option<Row>, payload: Payload, now: i64) -> Row {
    let payload = payload.bounded();
    match current {
        None => Row { payload, events: 1, first_at: now, last_at: now, ..Row::default() },
        Some(row) => Row { payload, events: row.events + 1, last_at: now, ..row },
    }
}

/// The row claimed for `by` until `for_secs` from `now`: only a pending row, or a filing one
/// whose claim has lapsed.
pub fn claim(row: &Row, by: &str, for_secs: i64, now: i64) -> Result<Row, String> {
    let free = match row.state {
        State::Pending => true,
        State::Filing => row.claim.as_ref().is_none_or(|c| c.until <= now),
        State::Refused | State::Unread => false,
    };
    if !free {
        return Err(row.standing());
    }
    let claim = Claim { by: by.to_string(), until: now + for_secs, events: row.events };
    Ok(Row { state: State::Filing, claim: Some(claim), ..row.clone() })
}

/// How a claimed filing ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// `bead` is `None` when the event rightly files nothing.
    Filed { bead: Option<String> },
    Refused { reason: String, lanes: Option<String> },
    Unread { reason: String, retry_at: i64 },
}

/// The row after `outcome`; `None` when it is filed and nothing newer waits, so the row goes.
pub fn record(row: &Row, outcome: &Recorded) -> Option<Row> {
    let held = Row { claim: None, ..row.clone() };
    match outcome {
        Recorded::Filed { bead } => {
            if row.claim.as_ref().is_some_and(|c| c.events == row.events) {
                return None;
            }
            let bead = bead.clone().or_else(|| row.bead.clone());
            Some(Row { state: State::Pending, bead, reason: None, lanes: None, retry_at: None, tries: 0, ..held })
        }
        Recorded::Refused { reason, lanes } => {
            Some(Row { state: State::Refused, reason: Some(cut(reason, REASON_CAP).to_string()), lanes: lanes.clone(), retry_at: None, tries: 0, ..held })
        }
        Recorded::Unread { reason, retry_at } => {
            Some(Row { state: State::Unread, reason: Some(cut(reason, REASON_CAP).to_string()), lanes: None, retry_at: Some(*retry_at), tries: row.tries.saturating_add(1), ..held })
        }
    }
}

/// A refused or unread row made pending again, or `None` for any other. Its try count stays, so
/// a retry that fails again waits longer.
pub fn retry(row: &Row) -> Option<Row> {
    matches!(row.state, State::Refused | State::Unread).then(|| Row { state: State::Pending, retry_at: None, ..row.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(body: &str) -> Payload {
        Payload { reference: "incident:a".into(), title: "t".into(), body: body.into(), ..Payload::default() }
    }

    #[test_id(tek3weq)]
    #[test]
    fn a_body_past_the_cap_is_cut_on_a_character_boundary_and_says_so_once() {
        let long = "é".repeat(BODY_CAP);
        let held = payload(&long).bounded();
        assert!(held.body.len() <= BODY_CAP, "{}", held.body.len());
        assert!(held.body.contains(&format!("body capped at {BODY_CAP} bytes: {} were raised", long.len())));
        assert_eq!(held.clone().bounded(), held, "the store bounds what the client already bounded");
        assert_eq!(payload("short").bounded().body, "short");
    }

    #[test_id(tmujrun)]
    #[test]
    fn a_raise_keeps_a_held_rows_state_and_counts_it() {
        let first = raise(None, payload("one"), 10);
        let refused = record(&claim(&first, "a", 60, 10).unwrap(), &Recorded::Refused { reason: "walled".into(), lanes: Some("plan".into()) }).unwrap();
        let again = raise(Some(refused), payload("two"), 20);
        assert_eq!((again.state, again.events, again.first_at, again.last_at, again.payload.body.as_str()), (State::Refused, 2, 10, 20, "two"));
        assert_eq!(claim(&again, "b", 60, 20), Err("state refused: walled".into()));
    }

    #[test_id(tqvr4kg)]
    #[test]
    fn a_row_is_claimed_once_until_its_claim_lapses() {
        let row = claim(&raise(None, payload("p"), 10), "a", 60, 10).unwrap();
        assert!(claim(&row, "b", 60, 69).is_err());
        assert_eq!(claim(&row, "b", 60, 70).unwrap().claim.unwrap().by, "b");
    }

    #[test_id(tbtzwtv)]
    #[test]
    fn a_retry_keeps_the_try_count_the_delay_doubles_on() {
        let row = claim(&raise(None, payload("p"), 10), "a", 60, 10).unwrap();
        let unread = record(&row, &Recorded::Unread { reason: "down".into(), retry_at: 70 }).unwrap();
        assert_eq!(unread.tries, 1);
        assert_eq!(retry(&unread).map(|r| (r.state, r.tries, r.retry_at)), Some((State::Pending, 1, None)));
        assert_eq!(retry(&row), None);
    }

    #[test_id(tt2nafr)]
    #[test]
    fn an_overlong_reference_keys_one_bounded_component() {
        let long = format!("basefail:r/s:{}", "x".repeat(900));
        let k = key(&long);
        assert!(!k.contains('/') && k.starts_with("basefail:r%2Fs:"), "{k}");
        assert_eq!(k, key(&bound_ref(&long)));
    }
}
