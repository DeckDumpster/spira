//! In-memory ports for the intake's unit tests: a `Bd` that keeps its beads in a vector, a
//! mailer that records what it was asked to send, and a fixed clock.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};

use crate::decide::{BeadRow, BeadStatus, Scope};
use crate::ports::{Bd, Clock, Mailer, Rows};
use spira_store::incident::{self, Payload, Recorded, Row};
use crate::run::FileConfig;

/// The incident rows in memory, moved by the same rules the store verb applies.
#[derive(Default)]
pub(crate) struct FakeRows {
    pub(crate) held: RefCell<BTreeMap<String, Row>>,
    /// The store's clock: what a raise is stamped with and a claim's lapse is judged on.
    pub(crate) now: Cell<i64>,
    /// The control store does not answer.
    pub(crate) down: Cell<bool>,
}

impl FakeRows {
    fn up(&self) -> Result<(), String> {
        if self.down.get() { Err("store not running".into()) } else { Ok(()) }
    }

    /// The one row held, for a test that expects exactly one.
    pub(crate) fn only(&self) -> Row {
        let held = self.held.borrow();
        assert_eq!(held.len(), 1, "{held:?}");
        held.values().next().cloned().unwrap_or_default()
    }

    fn row(&self, key: &str) -> Result<Row, String> {
        self.held.borrow().get(key).cloned().ok_or_else(|| "no such incident".into())
    }
}

impl Rows for &FakeRows {
    fn raise(&self, payload: &Payload) -> Result<String, String> {
        self.up()?;
        let key = incident::key(&payload.reference);
        let mut held = self.held.borrow_mut();
        let next = incident::raise(held.remove(&key), payload.clone(), self.now.get());
        held.insert(key.clone(), next);
        Ok(key)
    }
    fn claim(&self, key: &str, by: &str, for_secs: i64) -> Result<Row, String> {
        self.up()?;
        let next = incident::claim(&self.row(key)?, by, for_secs, self.now.get())?;
        self.held.borrow_mut().insert(key.into(), next.clone());
        Ok(next)
    }
    fn record(&self, key: &str, outcome: &Recorded) -> Result<(), String> {
        self.up()?;
        let next = incident::record(&self.row(key)?, outcome);
        let mut held = self.held.borrow_mut();
        match next {
            Some(row) => held.insert(key.into(), row),
            None => held.remove(key),
        };
        Ok(())
    }
    fn retry(&self, only: Option<&str>) -> Result<usize, String> {
        self.up()?;
        let mut held = self.held.borrow_mut();
        let due: Vec<(String, Row)> = held.iter().filter(|(k, _)| only.is_none_or(|o| o == k.as_str())).filter_map(|(k, r)| incident::retry(r).map(|n| (k.clone(), n))).collect();
        let n = due.len();
        held.extend(due);
        Ok(n)
    }
    fn list(&self) -> Result<Vec<(String, Row)>, String> {
        self.up()?;
        let mut rows: Vec<(String, Row)> = self.held.borrow().iter().map(|(k, r)| (k.clone(), r.clone())).collect();
        rows.sort_by(|a, b| (a.1.first_at, &a.0).cmp(&(b.1.first_at, &b.0)));
        Ok(rows)
    }
}

/// One `list` call: `(scope, label, closed_after, external_ref)`.
pub(crate) type ListCall = (Scope, Option<String>, Option<String>, Option<String>);

#[derive(Default)]
pub(crate) struct FakeBd {
    pub(crate) rows: RefCell<Vec<BeadRow>>,
    pub(crate) lists: RefCell<Vec<ListCall>>,
    pub(crate) reachable: bool,
    /// The lifecycle machine does not answer: every dedup pass is blind.
    pub(crate) lc_down: bool,
    /// The store's deadline: a handed-on list narrowed by neither label nor reference is too big.
    pub(crate) unfiltered_handed_on_times_out: bool,
    /// `spira-lc fact` refuses every write.
    pub(crate) fact_fails: bool,
    /// `bd create` refuses with this text.
    pub(crate) create_error: Option<String>,
    /// Every `bd create` asked for, refused ones included.
    pub(crate) create_calls: RefCell<usize>,
    pub(crate) incidents: FakeRows,
    pub(crate) created: RefCell<Vec<(String, String)>>,
    pub(crate) bodies: RefCell<HashMap<String, String>>,
    pub(crate) reopened: RefCell<Vec<String>>,
    pub(crate) related: RefCell<Vec<(String, String)>>,
    pub(crate) successors: RefCell<HashMap<String, String>>,
    pub(crate) duplicated: RefCell<Vec<(String, String)>>,
    pub(crate) labels: RefCell<HashMap<String, Vec<String>>>,
    pub(crate) notes: RefCell<HashMap<String, Vec<String>>>,
    pub(crate) create_labels: RefCell<HashMap<String, String>>,
    pub(crate) next_id: RefCell<u32>,
    /// `bd show` calls made: the label-keyed dedup must make none.
    pub(crate) shows: RefCell<usize>,
    pub(crate) facts: RefCell<Vec<(String, String, String)>>,
}

impl FakeBd {
    pub(crate) fn new() -> FakeBd {
        FakeBd { reachable: true, next_id: RefCell::new(1), ..Default::default() }
    }
}

impl Bd for FakeBd {
    fn incident_rows(&self) -> Box<dyn Rows + '_> {
        Box::new(&self.incidents)
    }
    // A row's `status` stands for its lifecycle row's state (decide::status_of_lc).
    fn list(&self, _db: &str, scope: Scope, label: Option<&str>, closed_after: Option<&str>, external_ref: Option<&str>) -> Result<Vec<BeadRow>, String> {
        let owned = |s: Option<&str>| s.map(str::to_string);
        self.lists.borrow_mut().push((scope, owned(label), owned(closed_after), owned(external_ref)));
        if self.lc_down {
            return Err("spira-lc: down".into());
        }
        if self.unfiltered_handed_on_times_out && scope == Scope::HandedOn && label.is_none() && external_ref.is_none() {
            return Err("cannot tell: deadline".into());
        }
        let rows = self.rows.borrow();
        Ok(rows
            .iter()
            .filter(|r| scope.holds(r.status) && label.map(|l| r.labels.iter().any(|x| x == l)).unwrap_or(true))
                .filter(|r| closed_after.is_none_or(|d| r.closed_at.as_deref().is_some_and(|c| c >= d)))
            .filter(|r| external_ref.is_none_or(|x| r.external_ref.as_deref() == Some(x)))
            .cloned()
            .collect())
    }
    fn create(&self, _db: &str, title: &str, _kind: &str, _priority: &str, labels: &str, external_ref: &str, _body: &str, _actor: Option<&str>) -> Result<String, String> {
        *self.create_calls.borrow_mut() += 1;
        if let Some(e) = &self.create_error {
            return Err(e.clone());
        }
        let mut n = self.next_id.borrow_mut();
        let id = format!("sp-fake{n}");
        *n += 1;
        self.created.borrow_mut().push((title.to_string(), external_ref.to_string()));
        self.bodies.borrow_mut().insert(id.clone(), _body.to_string());
        self.create_labels.borrow_mut().insert(id.clone(), labels.to_string());
        self.rows.borrow_mut().push(BeadRow {
            id: id.clone(),
            status: BeadStatus::Open,
            external_ref: Some(external_ref.to_string()),
            labels: labels.split(',').map(str::to_string).collect(),
            closed_at: None,
        });
        Ok(id)
    }
    fn label_add(&self, _db: &str, id: &str, label: &str) -> bool {
        self.labels.borrow_mut().entry(id.to_string()).or_default().push(label.to_string());
        if let Some(row) = self.rows.borrow_mut().iter_mut().find(|r| r.id == id) {
            row.labels.push(label.to_string());
        }
        true
    }
    fn label_remove(&self, _db: &str, id: &str, label: &str) -> bool {
        if let Some(row) = self.rows.borrow_mut().iter_mut().find(|r| r.id == id) {
            row.labels.retain(|l| l != label);
        }
        true
    }
    fn label_list(&self, _db: &str, id: &str) -> Vec<String> {
        self.rows.borrow().iter().find(|r| r.id == id).map(|r| r.labels.clone()).unwrap_or_default()
    }
    fn note(&self, _db: &str, id: &str, text: &str) -> bool {
        self.notes.borrow_mut().entry(id.to_string()).or_default().push(text.to_string());
        true
    }
    fn reopen(&self, _db: &str, id: &str, _cause: &str) -> bool {
        self.reopened.borrow_mut().push(id.to_string());
        if let Some(row) = self.rows.borrow_mut().iter_mut().find(|r| r.id == id) {
            row.status = BeadStatus::Open;
            row.closed_at = None;
        }
        true
    }
    fn relate(&self, _db: &str, a: &str, b: &str) -> bool {
        self.related.borrow_mut().push((a.to_string(), b.to_string()));
        true
    }
    fn duplicate(&self, _db: &str, id: &str, survivor: &str) -> bool {
        self.duplicated.borrow_mut().push((id.to_string(), survivor.to_string()));
        true
    }
    fn live_successor(&self, _db: &str, id: &str) -> Option<String> {
        self.successors.borrow().get(id).cloned()
    }
    fn show_closed_at(&self, _db: &str, id: &str) -> Option<String> {
            *self.shows.borrow_mut() += 1;
        self.rows.borrow().iter().find(|r| r.id == id).and_then(|r| r.closed_at.clone())
    }
    fn show_created_at(&self, _db: &str, _id: &str) -> Option<String> {
            *self.shows.borrow_mut() += 1;
        None
    }
    fn reachable(&self, _db: &str) -> bool {
        self.reachable
    }
    fn fact(&self, id: &str, kind: &str, cause: &str) -> bool {
        if self.fact_fails {
            return false;
        }
        self.facts.borrow_mut().push((id.into(), kind.into(), cause.into()));
        true
    }
    fn fact_count(&self, id: &str, kind: &str) -> Option<usize> {
        Some(self.facts.borrow().iter().filter(|(i, k, _)| i == id && k == kind).count())
    }
}

#[derive(Default)]
pub(crate) struct FakeMailer {
    pub(crate) sent: RefCell<Vec<String>>,
    pub(crate) defaults: RefCell<Vec<String>>,
    pub(crate) bodies: RefCell<Vec<String>>,
}
impl Mailer for FakeMailer {
    fn send_operator_question(&self, subject: &str, default: &str, body: &str) -> bool {
        self.sent.borrow_mut().push(subject.to_string());
        self.defaults.borrow_mut().push(default.to_string());
        self.bodies.borrow_mut().push(body.to_string());
        true
    }
}

pub(crate) struct FixedClock(pub(crate) i64);
impl Clock for FixedClock {
    fn now(&self) -> i64 {
        self.0
    }
}

pub(crate) fn cfg<'a>(known: &'a [String]) -> FileConfig<'a> {
    FileConfig {
        db: "db",
        kind: "bug",
        priority: "2",
        actor: None,
        cause: "systemd-fail",
        fenrir_at: 5,
        fenrir_exempt: false,
        watcher_interval_s: 1800,
        dedup_lookback_days: 7,
        repo_declared: Some("spira"),
        delivers_pref: None,
        sop_ledger: "run/sop/applied.jsonl",
        spira_run: "run",
        home_repo: "spira",
        known_repos: known,
        ask_label: "needs-ryan", // literal-ok: test fixture value, not a config default read at runtime.
        express: false,
        provenance: "foo.service on host: ?",
        retired_keys: &[],
    }
}

