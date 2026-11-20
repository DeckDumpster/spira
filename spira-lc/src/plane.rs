//! Where a semantic verb reads a bead's lifecycle row from and writes its events to: the control
//! store, through its own `show`, `list` and `event` verbs. A store that is not configured or
//! cannot be opened is "cannot tell", never a fall-back to anything else.

use lifecycle::bead::{BeadEventKind, LifecycleRow};
use state_store::Backend;

use crate::control::state;
use crate::control::store::Store;
use crate::db::DbError;
use crate::CANNOT_TELL;

/// The control store the bead-state verbs answer from.
pub(crate) struct Plane<'a, B: Backend>(pub(crate) &'a Store<B>);

impl<B: Backend> Plane<'_, B> {
    /// The bead's lifecycle row.
    pub(crate) fn bead(&self, id: &str) -> Result<Option<LifecycleRow>, DbError> {
        self.0.record(id).map(|r| r.map(|r| r.value.row)).map_err(|e| DbError::CannotTell(format!("the control store: {e}")))
    }

    /// The repository the bead's lifecycle row names.
    pub(crate) fn repo(&self, id: &str) -> Result<Option<String>, DbError> {
        self.0.record(id).map(|r| r.and_then(|r| r.value.repo)).map_err(|e| DbError::CannotTell(format!("the control store: {e}")))
    }

    /// A primitive read (`show`, `list`) as `spira-lc` itself answers it.
    pub(crate) fn read(&self, args: &[String]) -> (i32, String) {
        let request = crate::control::served::request_id();
        state::run(self.0, args, &state::Call { now: crate::db::now_epoch(), request: &request, inputs: &*crate::control::claimable::LIVE })
    }

    /// One bead event from wherever the row is now, under its CAS.
    pub(crate) fn apply(&self, id: &str, actor: &str, kind: &BeadEventKind) -> (i32, String) {
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
        self.read(&argv)
    }
}

/// Run `f` on the configured store; one that is unset or cannot be opened is cannot tell.
fn with<T>(f: impl FnOnce(&Plane<'_, crate::control::served::Served>) -> T, cannot: impl FnOnce(String) -> T) -> T {
    match crate::control::served::configured() {
        Some(Ok(store)) => f(&Plane(&store)),
        Some(Err(e)) => cannot(format!("the control store: {e}")),
        None => cannot("SPIRA_LC_STORE_PATH is unset: lifecycle is served by the control store alone".into()),
    }
}

/// The bead's lifecycle row.
pub(crate) fn bead(id: &str) -> Result<Option<LifecycleRow>, DbError> {
    with(|p| p.bead(id), |e| Err(DbError::CannotTell(e)))
}

/// The repository the bead's row names.
pub(crate) fn repo(id: &str) -> Result<Option<String>, DbError> {
    with(|p| p.repo(id), |e| Err(DbError::CannotTell(e)))
}

/// `show <id>` or `list …`.
pub(crate) fn read(args: &[String]) -> (i32, String) {
    with(|p| p.read(args), |e| (CANNOT_TELL, format!("cannot tell: {e}")))
}

/// One bead event.
pub(crate) fn apply(id: &str, actor: &str, kind: BeadEventKind) -> (i32, String) {
    with(|p| p.apply(id, actor, &kind), |e| (CANNOT_TELL, format!("cannot tell: {e}")))
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
    /// and moved there.
    #[test]
    fn a_bead_only_on_the_control_store_is_read_and_moved_there() {
        let s = store();
        assert!(!s.create("sp-a453hs", Some("harness"), None, 100, &Request::new("c1", "test", "t")).unwrap());
        let p: Plane<'_, MemBackend> = Plane(&s);
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
    }

    #[test]
    fn no_record_on_the_store_is_no_row_and_an_event_on_it_is_not_applied() {
        let s = store();
        let p: Plane<'_, MemBackend> = Plane(&s);
        assert!(p.bead("sp-none").unwrap().is_none());
        assert_eq!(p.apply("sp-none", "a", &BeadEventKind::Release).0, CANNOT_TELL);
    }

    #[test]
    fn with_no_control_store_configured_every_verb_is_cannot_tell() {
        let dir = testkit::TempDir::new("spira-lc-plane-no-store");
        let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_LC_STORE_PATH", "")]);
        let _env = testkit::env(&[("SPIRA_TOML", Some(toml.to_str().unwrap()))]);
        assert!(matches!(bead("sp-a"), Err(DbError::CannotTell(m)) if m.contains("SPIRA_LC_STORE_PATH")));
        assert_eq!(apply("sp-a", "a", BeadEventKind::Release).0, CANNOT_TELL);
        assert_eq!(read(&["show".to_string(), "sp-a".to_string()]).0, CANNOT_TELL);
    }

    /// The semantic verbs read and write the bead through this module, never the dolt row
    /// directly: on fc5a88428 `work submit` read `rows::fetch_bead` and answered "no bead row"
    /// for a bead that lived only on the control store.
    #[test]
    fn the_semantic_verbs_never_read_the_dolt_row_directly() {
        let code = |src: &'static str| src.split("#[cfg(test)]").next().unwrap_or(src);
        for (name, src) in [("work.rs", include_str!("work.rs"))] {
            for direct in [concat!("rows::", "fetch_bead("), concat!("rows::", "bead_repo("), concat!("crate::", "cmd_show("), concat!("crate::", "cmd_list(")] {
                assert!(!code(src).contains(direct), "{name} reads {direct} — the dolt row — instead of plane::");
            }
        }
    }
}
