//! The ports' production implementations.

pub mod verbs;

use runtime::ports::{Clock, VirtualClock, WallClock};
use state_store::{Error, Fence, Item, Request, Selector, Txn, TxnResult};

use crate::ports::Store;

/// The wall clock, or `SPIRA_NOW` (epoch seconds) when the simulator sets it.
pub fn clock(spira_now: Option<u64>) -> Box<dyn Clock> {
    match spira_now {
        Some(s) => Box::new(VirtualClock::new(s * 1000)),
        None => Box::new(WallClock),
    }
}

/// The store until spira-lc serves the control plane: every call is refused, naming why, so
/// nothing can mistake an unwired store for an empty one (law-only-spira-lc-talks-to-the-stores).
pub struct Unserved;

fn unserved<T>() -> state_store::Result<T> {
    Err(Error::Backend("the control-plane store has no spira-lc door yet: round-controller reads and writes nothing".into()))
}

impl Store for Unserved {
    fn get(&self, _: &str) -> state_store::Result<Option<Item>> {
        unserved()
    }
    fn list(&self, _: &[&str], _: &Selector) -> state_store::Result<(Vec<Item>, u64)> {
        unserved()
    }
    fn txn(&self, _: &Txn, _: &Request) -> state_store::Result<TxnResult> {
        unserved()
    }
    fn lock(&self, _: &str, _: u64, _: &Request) -> state_store::Result<Fence> {
        unserved()
    }
    fn renew(&self, _: &Fence, _: &Request) -> state_store::Result<()> {
        unserved()
    }
    fn release(&self, _: &Fence, _: &Request) -> state_store::Result<()> {
        unserved()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spira_now_pins_the_clock_and_its_absence_is_the_wall() {
        assert_eq!(clock(Some(1_800_000_000)).now_ms(), 1_800_000_000_000);
        assert!(clock(None).now_ms() > 1_700_000_000_000);
    }

    #[test]
    fn the_unserved_store_refuses_every_call_naming_why() {
        let req = Request::new("r", "a", "k");
        let fence = Fence {
            resource: "x".into(),
            token: state_store::Rev::new(1, 0),
        };
        let errs = [
            Unserved.get("k").unwrap_err(),
            Unserved.list(&["k"], &Selector::Everything).unwrap_err(),
            Unserved.txn(&Txn::new(), &req).unwrap_err(),
            Unserved.lock("x", 1, &req).unwrap_err(),
            Unserved.renew(&fence, &req).unwrap_err(),
            Unserved.release(&fence, &req).unwrap_err(),
        ];
        assert!(
            errs.iter()
                .all(|e| e.to_string().contains("no spira-lc door")),
            "{errs:?}"
        );
    }
}
