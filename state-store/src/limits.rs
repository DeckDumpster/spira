//! etcd v3's request limits, enforced locally so a distributed backend never sees a request it
//! would refuse. The constants are exported for callers that size their own txns.

use std::collections::BTreeSet;

use crate::contract::{Compare, Txn};
use crate::{Error, Limit, Result};

/// etcd's default `--max-txn-ops`, applied to each branch and to the compares.
pub const MAX_TXN_OPS: usize = 128;
pub const MAX_TXN_COMPARES: usize = 128;
/// etcd's default `--max-request-bytes` (1.5 MiB).
pub const MAX_REQUEST_BYTES: usize = 1_572_864;

/// Ops on the larger branch of a round's cut of `members`: each member's lifecycle and delivery
/// put, its gate when the step moves it, and its history entry; the round, its id index and its
/// history entry; then the events and request keys the contract adds. A landing moves every
/// member's gate, so the bound counts one for each.
pub const fn cut_ops(members: usize) -> usize {
    4 * members + 5
}

/// Compares a cut makes: each member's record, delivery and gate, the round's key and its id
/// index absent, and each blocker gate it re-checks.
pub const fn cut_compares(members: usize, gates: usize) -> usize {
    3 * members + 2 + gates
}

/// Whether a cut of `members` comparing `gates` blocker gates fits both limits.
pub const fn cut_fits(members: usize, gates: usize) -> bool {
    cut_ops(members) <= MAX_TXN_OPS && cut_compares(members, gates) <= MAX_TXN_COMPARES
}

/// The most members one round may hold: every cascade on a round (cut, promote, settle,
/// abandon) writes at most a cut's ops, so a round no larger than this is always writable.
pub const MAX_CUT_MEMBERS: usize = {
    let mut n = 0;
    while cut_fits(n + 1, 0) {
        n += 1;
    }
    n
};

/// The blocker gates a cut at the member cap may still compare.
pub const MAX_CUT_GATES: usize = MAX_TXN_COMPARES - cut_compares(MAX_CUT_MEMBERS, 0);

/// The keys and values a txn's compares carry.
pub fn compare_bytes(compares: &[Compare]) -> usize {
    compares
        .iter()
        .map(|c| match c {
            Compare::ModRev(k, _)
            | Compare::CreateRev(k, _)
            | Compare::LockCheck { key: k, .. } => k.len(),
            Compare::Value(k, v) => k.len() + v.as_ref().map_or(0, |o| o.encode().len()),
        })
        .sum()
}

/// Refuses a request of `got` serialized bytes over etcd's limit.
pub fn check_bytes(got: usize) -> Result<()> {
    if got > MAX_REQUEST_BYTES {
        return Err(Error::Limit(Limit::RequestBytes {
            max: MAX_REQUEST_BYTES,
            got,
        }));
    }
    Ok(())
}

/// Refuses the op, compare and duplicate-key breaches; the size is checked on the batch the
/// contract builds (`check_bytes`), which includes its bookkeeping. `bookkeeping` is the keys the contract adds to a branch (the
/// request key, and the events key when the txn is audited): they count toward the op limit.
pub fn check(t: &Txn, bookkeeping: usize) -> Result<()> {
    let ops = t.ops.len().max(t.else_ops.len()) + bookkeeping;
    if ops > MAX_TXN_OPS {
        return Err(Error::Limit(Limit::Ops {
            max: MAX_TXN_OPS,
            got: ops,
        }));
    }
    if t.compares.len() > MAX_TXN_COMPARES {
        return Err(Error::Limit(Limit::Compares {
            max: MAX_TXN_COMPARES,
            got: t.compares.len(),
        }));
    }
    for branch in [&t.ops, &t.else_ops] {
        let mut seen = BTreeSet::new();
        for o in branch.iter() {
            if !seen.insert(o.key()) {
                return Err(Error::Limit(Limit::DuplicateKey {
                    key: o.key().to_string(),
                }));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_member_cap_is_the_largest_cut_within_the_op_limit_and_leaves_room_for_gates() {
        assert!(cut_fits(MAX_CUT_MEMBERS, MAX_CUT_GATES));
        assert!(!cut_fits(MAX_CUT_MEMBERS + 1, 0));
        assert!(!cut_fits(MAX_CUT_MEMBERS, MAX_CUT_GATES + 1));
        assert!(cut_ops(MAX_CUT_MEMBERS) <= MAX_TXN_OPS);
        assert!(cut_ops(MAX_CUT_MEMBERS + 1) > MAX_TXN_OPS);
        assert_eq!(MAX_CUT_MEMBERS, 30);
    }
}
