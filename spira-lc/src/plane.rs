//! Which plane a semantic verb reads a bead's lifecycle row from and writes its events to.
//!
//! The primitive verbs (`show`, `list`, `event`) reach the control store through
//! `control::served::dispatch` once `SPIRA_LC_LIFECYCLE_ON_STORE` is on. The semantic verbs —
//! `work <id> submit|done|blocked|…`, the ask close's hold lift, the classifier and the mend
//! retirement — are handed the dolt connection instead, and read the row with
//! `rows::fetch_bead` and wrote with `run_bead_event` against it: with lifecycle on the store,
//! a bead born there had "no bead row", and one that predates the cutover had its event written
//! to the retired dolt row while the store's record stayed where it was (sp-cxfgj5).
//!
//! Every such read and write goes through here. With lifecycle on the store it is the store's
//! record and the store's own `event` verb; the dolt row only while the store does not serve
//! lifecycle (pre-cutover or direct mode). A store that is declared but cannot be opened is
//! "cannot tell", never a fall-back to dolt.

use std::sync::Arc;

use lifecycle::bead::{BeadEventKind, BeadRow};
use state_store::Backend;

use crate::control::state;
use crate::control::store::Store;
use crate::db::{Conn, DbError};
use crate::CANNOT_TELL;

/// Where the bead-state verbs answer from.
pub(crate) enum Plane<'a, B: Backend> {
    /// The control store, lifecycle on it.
    Store(&'a Store<B>),
    /// The dolt lifecycle tables.
    Dolt(&'a Conn),
}

impl<B: Backend> Plane<'_, B> {
    /// The bead's lifecycle row.
    pub(crate) fn bead(&self, id: &str) -> Result<Option<BeadRow>, DbError> {
        match self {
            Plane::Store(s) => s.record(id).map(|r| r.map(|r| r.value.row)).map_err(|e| DbError::CannotTell(format!("the control store: {e}"))),
            Plane::Dolt(conn) => crate::rows::fetch_bead(conn, id),
        }
    }

    /// The repository the bead's lifecycle row names.
    pub(crate) fn repo(&self, id: &str) -> Result<Option<String>, DbError> {
        match self {
            Plane::Store(s) => s.record(id).map(|r| r.and_then(|r| r.value.repo)).map_err(|e| DbError::CannotTell(format!("the control store: {e}"))),
            Plane::Dolt(conn) => crate::rows::bead_repo(conn, id),
        }
    }

    /// A primitive read (`show`, `list`) as `spira-lc` itself answers it on this plane.
    pub(crate) fn read(&self, args: &[String]) -> (i32, String) {
        match self {
            Plane::Store(s) => {
                let request = crate::control::served::request_id();
                state::run(s, args, &state::Call { now: crate::db::now_epoch(), request: &request })
            }
            Plane::Dolt(conn) => match args.first().map(String::as_str) {
                Some("show") => crate::cmd_show(&args[1..], conn),
                Some("list") => crate::cmd_list(&args[1..], conn),
                other => (CANNOT_TELL, format!("plane read: {other:?} is not a read verb")),
            },
        }
    }

    /// One bead event from wherever the row is now, under its CAS.
    pub(crate) fn apply(&self, id: &str, actor: &str, kind: &BeadEventKind) -> (i32, String) {
        match self {
            Plane::Store(s) => {
                let row = match self.bead(id) {
                    Ok(Some(r)) => r,
                    Ok(None) => return (CANNOT_TELL, format!("work: no bead row for {id}")),
                    Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
                };
                let kind_json = serde_json::to_string(kind).unwrap_or_default();
                let argv: Vec<String> = ["event", "bead", id, "--expect", row.state.as_str(), "--version", &row.version.to_string(), "--actor", actor, "--kind", &kind_json]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                let request = crate::control::served::request_id();
                state::run(s, &argv, &state::Call { now: crate::db::now_epoch(), request: &request })
            }
            Plane::Dolt(conn) => crate::dolt_apply_bead_event(conn, id, actor, kind.clone()),
        }
    }
}

/// The plane the configuration declares: the control store while lifecycle is on it.
fn configured() -> Option<Result<Arc<Store<crate::control::served::Served>>, String>> {
    crate::control::served::lifecycle_store()
}

/// Run `f` on the declared plane; a declared store that cannot be opened is cannot tell.
fn with<T>(conn: &Conn, f: impl FnOnce(&Plane<'_, crate::control::served::Served>) -> T, cannot: impl FnOnce(String) -> T) -> T {
    match configured() {
        Some(Ok(store)) => f(&Plane::Store(&store)),
        Some(Err(e)) => cannot(format!("the control store: {e}")),
        None => f(&Plane::Dolt(conn)),
    }
}

/// The bead's lifecycle row, from the declared plane.
pub(crate) fn bead(conn: &Conn, id: &str) -> Result<Option<BeadRow>, DbError> {
    with(conn, |p| p.bead(id), |e| Err(DbError::CannotTell(e)))
}

/// The repository the bead's row names, from the declared plane.
pub(crate) fn repo(conn: &Conn, id: &str) -> Result<Option<String>, DbError> {
    with(conn, |p| p.repo(id), |e| Err(DbError::CannotTell(e)))
}

/// `show <id>` or `list …`, on the declared plane.
pub(crate) fn read(conn: &Conn, args: &[String]) -> (i32, String) {
    with(conn, |p| p.read(args), |e| (CANNOT_TELL, format!("cannot tell: {e}")))
}

/// One bead event, on the declared plane.
pub(crate) fn apply(conn: &Conn, id: &str, actor: &str, kind: BeadEventKind) -> (i32, String) {
    with(conn, |p| p.apply(id, actor, &kind), |e| (CANNOT_TELL, format!("cannot tell: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lifecycle::bead::{BeadState, Stack};
    use runtime::ports::VirtualClock;
    use state_store::{Config, Contract, MemBackend, Ports, Request};

    fn store() -> Store<MemBackend> {
        let c = Contract::open(MemBackend::new(), Ports::virtual_clock(&VirtualClock::new(0)), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        Store::new(c, 1_000)
    }

    /// The scar: a bead that exists only on the control store (born after the cutover) is found
    /// and moved there; the dolt connection — unreachable here — is never asked.
    #[test]
    fn a_bead_only_on_the_control_store_is_read_and_moved_there_never_on_dolt() {
        let s = store();
        assert!(!s.create("sp-a453hs", Some("harness"), None, 100, &Request::new("c1", "test", "t")).unwrap());
        let p: Plane<'_, MemBackend> = Plane::Store(&s);
        let row = p.bead("sp-a453hs").unwrap().expect("the store's record is the row");
        assert_eq!(row.state, BeadState::Ready);
        assert_eq!(p.repo("sp-a453hs").unwrap().as_deref(), Some("harness"));
        let claim = BeadEventKind::Claim { holder: "dwarf-1".into(), lease_until: crate::db::now_epoch() + 3_600, stack: Stack::new(), stack_depth: 0, stack_max_depth: 4, persona: None, directed: None };
        assert_eq!(p.apply("sp-a453hs", "dwarf-1", &claim).0, 0, "claim on the store");
        let (rc, out) = p.apply("sp-a453hs", "dwarf-1", &BeadEventKind::Submit { tip: "0123456789abcdef0123456789abcdef01234567".into() });
        assert_eq!(rc, 0, "{out}");
        assert_eq!(s.record("sp-a453hs").unwrap().unwrap().value.row.state, BeadState::Submitted, "the event landed on the store");
        let (rc, shown) = p.read(&["show".to_string(), "sp-a453hs".to_string()]);
        assert_eq!(rc, 0, "{shown}");
        let shown: serde_json::Value = serde_json::from_str(&shown).unwrap();
        assert_eq!(shown["bead"]["state"], "SUBMITTED", "`work show` reads the plane the submit wrote: {shown}");
        let (rc, listed) = p.read(&["list".to_string(), "--live".to_string()]);
        assert_eq!(rc, 0, "{listed}");
        assert!(listed.contains("SUBMITTED") && listed.contains("sp-a453hs"), "`work list` reads it too: {listed}");
        let dolt = Conn::unreachable();
        assert!(Plane::<MemBackend>::Dolt(&dolt).bead("sp-a453hs").is_err(), "the dolt plane is a different store");
    }

    #[test]
    fn no_record_on_the_store_is_no_row_and_an_event_on_it_is_not_applied() {
        let s = store();
        let p: Plane<'_, MemBackend> = Plane::Store(&s);
        assert!(p.bead("sp-none").unwrap().is_none());
        assert_eq!(p.apply("sp-none", "a", &BeadEventKind::Release).0, CANNOT_TELL);
    }

    /// The semantic verbs read and write the bead through this module, never the dolt row
    /// directly: on fc5a88428 `work submit` read `rows::fetch_bead` and answered "no bead row"
    /// for a bead that lived only on the control store.
    #[test]
    fn the_semantic_verbs_never_read_the_dolt_row_directly() {
        let code = |src: &'static str| src.split("#[cfg(test)]").next().unwrap_or(src);
        for (name, src) in [("work.rs", include_str!("work.rs")), ("classify_cmd.rs", include_str!("classify_cmd.rs"))] {
            for direct in [concat!("rows::", "fetch_bead("), concat!("rows::", "bead_repo("), concat!("crate::", "cmd_show("), concat!("crate::", "cmd_list(")] {
                assert!(!code(src).contains(direct), "{name} reads {direct} — the dolt row — instead of plane::");
            }
        }
    }
}
