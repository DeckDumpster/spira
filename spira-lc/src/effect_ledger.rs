//! Which open effects have run: `runtime/done/<request_id>`. In the library so an in-process
//! caller reaches the store logic `spira-lc journal done|has-done` serves.

use serde_json::json;
use state_store::{Backend, Compare, Contract, Object, Request, Result, Txn};

use crate::keys;

pub const OPEN_EFFECT: &str = "open-effect";
pub const DONE: &str = "done";

/// Marks `id` run, only while its effect is still open: the txn that resolves it deletes the
/// mark, so one written afterwards would never be deleted. Answers whether it wrote.
pub fn done<B: Backend>(contract: &Contract<B>, id: &str, request: &Request) -> Result<bool> {
    let open = keys::runtime(OPEN_EFFECT, &[id]);
    let Some(found) = contract.get(&open)? else { return Ok(false) };
    let t = Txn::new().when(Compare::ModRev(open, Some(found.mod_rev))).put(&keys::runtime(DONE, &[id]), Object::new(json!({})));
    Ok(contract.txn(&t, request)?.succeeded)
}

pub fn has_done<B: Backend>(contract: &Contract<B>, id: &str) -> Result<bool> {
    Ok(contract.get(&keys::runtime(DONE, &[id]))?.is_some())
}
