//! The round lease guard: `round certify`, `eject`, `land` and `abandon` touch a round only when
//! no other holder leases it. The lease is `locks/round/<repo>/<batch>` on the control store, read
//! through spira-lc; `--lease-token` is how its holder says it is the one calling.

use super::{World, FAIL};
use crate::ports::RoundLease;

/// Whether a verb on `batch` may go on. The guard reads before the verb acts: it cannot fence the
/// verb's later writes, so a lease taken between the two is the next caller's to meet.
pub fn lease_guard(
    w: &World,
    label: &str,
    repo: &str,
    batch: &str,
    token: Option<&str>,
) -> Result<(), i32> {
    // Unread, a lease would be passed as absent: a verb that cannot tell refuses.
    let held = match w.lc.round_lease(repo, batch) {
        Ok(h) => h,
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot tell whether round {batch} is leased: {e} — refused, nothing changed; run it again once spira-lc answers `lock show round/{repo}/{batch}`"));
            return Err(FAIL);
        }
    };
    match (held, token) {
        (None, None) => Ok(()),
        (Some(l), Some(t)) if same_token(&l.token, t) => Ok(()),
        (None, Some(t)) => {
            w.err(format!("queue.sh {label}: round {batch} holds no lease, so --lease-token {t} no longer holds it — refused, nothing changed; the lease lapsed: take it again, or run without --lease-token"));
            Err(FAIL)
        }
        (Some(l), _) => {
            w.err(refusal(label, repo, batch, &l));
            Err(FAIL)
        }
    }
}

fn refusal(label: &str, repo: &str, batch: &str, l: &RoundLease) -> String {
    let holder = l.holder.as_deref().unwrap_or("a holder no record names");
    format!(
        "queue.sh {label}: round {batch} is leased by {holder} under token {} with {}s left — refused, nothing changed; only its holder touches it, with --lease-token {}; it is free once the lease lapses, or once `round-controller lease clear --repo {repo} --batch {batch}` proves its holder dead",
        l.token, l.ttl_left_secs, l.token
    )
}

/// A token as holders pass it (`<main>-<sub>`) or as the store prints it (`<main>.<sub>`).
fn same_token(held: &str, given: &str) -> bool {
    let norm = |t: &str| t.replace('.', "-");
    norm(held) == norm(given)
}
