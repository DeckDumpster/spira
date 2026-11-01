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

/// The keys and values a txn's compares carry.
pub fn compare_bytes(compares: &[Compare]) -> usize {
    compares
        .iter()
        .map(|c| match c {
            Compare::ModRev(k, _) | Compare::CreateRev(k, _) => k.len(),
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
