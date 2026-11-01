use std::cell::Cell;

use lifecycle::ask::{AskEventKind, AskState};
use lifecycle::bead::HoldKind;
use runtime::ports::VirtualClock;
use state_store::{Config, Contract, MemBackend, Ports, Request};

use super::{AskClose, Store};
use crate::callers::{self, Machine};
use crate::control::state::{self, Call};

const AT: i64 = 1_767_225_600;

struct Rig {
    s: Store<MemBackend>,
    n: Cell<u64>,
}

impl Machine for &Rig {
    fn call(&mut self, args: &[String]) -> (i32, String) {
        self.lc_args(args)
    }
}

impl Rig {
    fn new() -> Self {
        let c = Contract::open(
            MemBackend::new(),
            Ports::virtual_clock(&VirtualClock::new(AT as u64 * 1_000)),
            Config {
                min_lock_ttl_ms: 1_000,
            },
        )
        .unwrap();
        Rig {
            s: Store::new(c, 1_000),
            n: Cell::new(0),
        }
    }

    fn req(&self) -> Request {
        self.n.set(self.n.get() + 1);
        Request::new(&format!("r{}", self.n.get()), "test", "t")
    }

    fn lc_args(&self, args: &[String]) -> (i32, String) {
        let req = self.req();
        state::run(
            &self.s,
            args,
            &Call {
                now: AT,
                request: &req.id,
            },
        )
    }

    fn ok(&self, argv: &[&str]) {
        let args: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let (rc, out) = self.lc_args(&args);
        assert_eq!(rc, 0, "{argv:?}: {out}");
    }

    /// A READY record held on an ask, and the ask naming it.
    fn asked(&self) {
        self.ok(&["create-bead", "sp-w", "--repo", "spira", "--ready"]);
        let mut m = self;
        let held = callers::run(
            "hold",
            &["sp-w", "ask", "which way?", "test"].map(String::from),
            &mut m,
        );
        assert_eq!(held.code, 0, "{held:?}");
        self.ok(&["create-ask", "sp-q", "--work-bead", "sp-w"]);
    }

    fn holds(&self) -> Vec<HoldKind> {
        let rec = self.s.record("sp-w").unwrap().unwrap();
        rec.value.row.holds.into_iter().collect()
    }
}

fn answer() -> AskEventKind {
    AskEventKind::Answer {
        quote: "this way".into(),
        channel: "chat".into(),
    }
}

#[test]
fn closing_an_ask_lifts_its_work_beads_hold_in_the_same_txn() {
    let r = Rig::new();
    r.asked();
    assert_eq!(r.holds(), [HoldKind::Ask]);
    let head = r.s.contract().head().unwrap();
    let closed = r.s.close_ask("sp-q", answer(), "test", "m-1", AT, &r.req());
    assert_eq!(closed.unwrap(), AskClose::Closed);
    assert_eq!(r.s.contract().head().unwrap(), head + 1, "one revision");
    assert_eq!(
        r.s.ask("sp-q").unwrap().unwrap().row.state,
        AskState::Answered
    );
    assert!(r.holds().is_empty());
}

#[test]
fn a_close_whose_work_bead_moved_after_the_plan_writes_nothing() {
    let r = Rig::new();
    r.asked();
    let req = r.req();
    let Ok(planned) =
        r.s.plan_close_ask("sp-q", answer(), "test", "m-1", AT, &req)
            .unwrap()
    else {
        panic!("the close was not planned");
    };
    r.ok(&["set-repo", "sp-w", "other"]);
    assert_eq!(r.s.commit_close(planned, &req).unwrap(), AskClose::LostRace);
    let ask = r.s.ask("sp-q").unwrap().unwrap();
    assert_eq!((ask.row.state, ask.closed_at), (AskState::Open, None));
    assert_eq!(r.holds(), [HoldKind::Ask], "the hold stays");
}

#[test]
fn a_closed_ask_is_refused_and_an_existing_ask_is_left_as_it_is() {
    let r = Rig::new();
    r.asked();
    let close = || r.s.close_ask("sp-q", answer(), "test", "m-1", AT, &r.req());
    assert_eq!(close().unwrap(), AskClose::Closed);
    assert!(matches!(
        close().unwrap(),
        AskClose::Refused(lifecycle::Refusal::Terminal { .. })
    ));
    r.ok(&["create-ask", "sp-q"]);
    let ask = r.s.ask("sp-q").unwrap().unwrap();
    assert_eq!(
        (ask.row.state, ask.row.work_bead.as_deref()),
        (AskState::Answered, Some("sp-w"))
    );
}
