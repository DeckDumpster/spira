//! In-memory ports for the intake's unit tests: a `Bd` that keeps its beads in a vector, a
//! mailer that records what it was asked to send, and a fixed clock.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::decide::{BeadRow, BeadStatus, Scope};
use crate::ports::{Bd, Clock, Mailer};
use crate::run::FileConfig;

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
    /// Repo-map text: when set, `create` applies the filing door's repo, build-spec and lane refusals.
    pub(crate) admits: Option<String>,
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

    fn door(&self, labels: &str, body: &str) -> Result<(), String> {
        let Some(map) = &self.admits else { return Ok(()) };
        let repo = labels.split(',').find_map(|l| l.strip_prefix("repo:")).ok_or("bead: --repo <name> required")?;
        if bead::rune_partition(labels) == Some(spira_config::Lane::Plan) && !bead::has_build_spec(body) {
            return Err(format!("bead: refusing: a bead for builder must name what to build under a `{}` heading", bead::BUILD_HEADING));
        }
        match bead::lane_check(labels, repo, &bead::repos_by_name(map), false) {
            bead::LaneCheck::Admitted => Ok(()),
            bead::LaneCheck::Refused(m) => Err(m),
        }
    }
}

impl Bd for FakeBd {
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
        if let Some(e) = &self.create_error {
            return Err(e.clone());
        }
        self.door(labels, _body)?;
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
        plan_labels: Some("spira,plan"),
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

