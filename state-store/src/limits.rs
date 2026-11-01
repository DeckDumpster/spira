//! etcd v3's request limits, enforced locally so a distributed backend never sees a request it
//! would refuse. The constants are exported for callers that size their own txns.

use std::collections::BTreeSet;

use crate::contract::{Compare, Op, Request, Txn};
use crate::{Error, Limit, Result};

/// etcd's default `--max-txn-ops`, applied to each branch and to the compares.
pub const MAX_TXN_OPS: usize = 128;
pub const MAX_TXN_COMPARES: usize = 128;
/// etcd's default `--max-request-bytes` (1.5 MiB).
pub const MAX_REQUEST_BYTES: usize = 1_572_864;

/// The serialized size of a request as the caller sends it: every key and encoded value in the
/// compares and both branches, plus the request id and its audit record.
pub fn request_bytes(t: &Txn, req: &Request) -> usize {
    let cmp: usize = t
        .compares
        .iter()
        .map(|c| match c {
            Compare::ModRev(k, _) | Compare::CreateRev(k, _) => k.len(),
            Compare::Value(k, v) => k.len() + v.as_ref().map_or(0, |o| o.encode().len()),
        })
        .sum();
    let ops = |ops: &[Op]| -> usize {
        ops.iter()
            .map(|o| match o {
                Op::Put(k, v) => k.len() + v.encode().len(),
                Op::Delete(k) => k.len(),
            })
            .sum()
    };
    cmp + ops(&t.ops) + ops(&t.else_ops) + req.encoded_len()
}

/// Refuses whatever etcd would. `bookkeeping` is the keys the contract adds to a branch (the
/// request key, and the events key when the txn is audited): they count toward the op limit.
pub fn check(t: &Txn, req: &Request, bookkeeping: usize) -> Result<()> {
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
    let bytes = request_bytes(t, req);
    if bytes > MAX_REQUEST_BYTES {
        return Err(Error::Limit(Limit::RequestBytes {
            max: MAX_REQUEST_BYTES,
            got: bytes,
        }));
    }
    Ok(())
}
