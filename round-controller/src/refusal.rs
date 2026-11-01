//! Why a step withheld its act, typed, and how it is told: one outcome line per cause. A step's
//! standing refusal is kept at [`standing_key`], so a repeat of the same refusal on the next
//! pass writes nothing and logs nothing, and a different one replaces it.

use serde::{Deserialize, Serialize};
use state_store::{Object, Request, Txn};

use crate::outcome::Outcome;
use crate::ports::{Ports, VerbRefusal};

/// How long a standing refusal is kept without being restated. It is regenerable telemetry: a
/// refusal that outlives it is logged once more, and nothing else reads it.
pub const STANDING_TTL_SECS: u64 = 86_400; // batch-job: a day's passes share one refusal line

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The runner slot the cut was handed is not live under its token.
    SlotNotHeld {
        key: String,
    },
    /// The repo has no lane record: no landing ref, image digest or harness to cut on.
    LaneUnknown {
        key: String,
    },
    /// A Base, Image or Install fault stands on the cut's (base, image digest).
    LaneFault {
        key: String,
    },
    /// The identity holds a red verdict.
    RedIdentity {
        key: String,
    },
    /// The identity's transient faults back it off, or have run out.
    BackingOff {
        key: String,
    },
    /// An explicit cut named a judged-red identity or a member proven red at its tip.
    CutOfJudgedRed {
        key: String,
    },
    /// An explicit cut found the repo's stack at its depth.
    StackFull {
        key: String,
    },
    /// An explicit cut's members do not make an identity: none, a duplicate, or too many.
    BadMembers {
        key: String,
    },
    Unreadable {
        key: String,
        text: String,
    },
    /// No runner slot was free for the round the schedule step would cut; `text` names every
    /// slot's holder and the key that opens each closed pool.
    NoSlot {
        key: String,
        text: String,
    },
    /// The round was cut but its lease could not be taken, so no runner was launched for it.
    LeaseNotTaken {
        key: String,
        text: String,
    },
    /// A queue verb refused.
    Verb {
        batch: Option<String>,
        refusal: VerbRefusal,
    },
    /// spira-lc refused a section or intent write: the field it names and the rule it broke.
    Admission {
        key: String,
        field: String,
        rule: String,
    },
}

impl Refusal {
    pub fn word(&self) -> &'static str {
        match self {
            Refusal::SlotNotHeld { .. } => "slot-not-held",
            Refusal::LaneUnknown { .. } => "lane-unknown",
            Refusal::LaneFault { .. } => "lane-fault",
            Refusal::RedIdentity { .. } => "red-identity",
            Refusal::BackingOff { .. } => "backing-off",
            Refusal::CutOfJudgedRed { .. } => "cut-of-judged-red",
            Refusal::StackFull { .. } => "stack-full",
            Refusal::BadMembers { .. } => "bad-members",
            Refusal::Unreadable { .. } => "unreadable",
            Refusal::NoSlot { .. } => "no-slot",
            Refusal::LeaseNotTaken { .. } => "lease-not-taken",
            Refusal::Verb { .. } => "verb-refused",
            Refusal::Admission { .. } => "admission-refused",
        }
    }

    /// The key that names the cause: the same refusal always has the same key.
    pub fn key(&self) -> String {
        match self {
            Refusal::SlotNotHeld { key }
            | Refusal::LaneUnknown { key }
            | Refusal::LaneFault { key }
            | Refusal::RedIdentity { key }
            | Refusal::BackingOff { key }
            | Refusal::CutOfJudgedRed { key }
            | Refusal::StackFull { key }
            | Refusal::BadMembers { key }
            | Refusal::Unreadable { key, .. }
            | Refusal::NoSlot { key, .. }
            | Refusal::LeaseNotTaken { key, .. } => key.clone(),
            Refusal::Admission { key, field, .. } => format!("{key}#{field}"),
            Refusal::Verb { batch, refusal } => format!(
                "verb/{}/{}/{}",
                refusal.verb.word(),
                batch.as_deref().unwrap_or("-"),
                refusal
                    .code
                    .map_or_else(|| "not-run".to_string(), |c| c.to_string())
            ),
        }
    }

    pub fn unreadable(key: &str, e: impl std::fmt::Display) -> Refusal {
        Refusal::Unreadable {
            key: key.into(),
            text: e.to_string(),
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Unreadable { key, text } => write!(f, "unreadable {key}: {text}"),
            Refusal::NoSlot { text, .. } => write!(f, "{text}"),
            Refusal::LeaseNotTaken { key, text } => write!(f, "lease-not-taken {key}: {text}"),
            Refusal::Verb { refusal, .. } => write!(
                f,
                "queue round {} refused: {}",
                refusal.verb.word(),
                refusal.text
            ),
            Refusal::Admission { key, field, rule } => {
                write!(f, "spira-lc refused {key}: {field}: {rule}")
            }
            r => write!(f, "{} {}", r.word(), r.key()),
        }
    }
}

/// Where `step`'s standing refusal for `repo` is kept.
pub fn standing_key(repo: &str, step: &str) -> String {
    format!("telemetry/round-controller/{repo}/{step}")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Standing {
    word: String,
    key: String,
    since: u64,
}

/// Tells `step`'s refusal for `repo` once: logged and kept only when it is not already the
/// standing one; `None` clears whatever stood. A store that cannot keep it costs a repeated
/// line, never a lost one.
pub fn note(p: &Ports, repo: &str, step: &str, r: Option<&Refusal>) {
    let at = p.clock.now_ms();
    let skey = standing_key(repo, step);
    let standing: Option<Standing> = p
        .store
        .get(&skey)
        .ok()
        .flatten()
        .and_then(|i| serde_json::from_value(i.value.data).ok());
    let Some(r) = r else {
        if standing.is_some() {
            write(p, &skey, Txn::new().delete(&skey), "clear");
        }
        return;
    };
    if standing
        .as_ref()
        .is_some_and(|s| s.word == r.word() && s.key == r.key())
    {
        return;
    }
    let data = serde_json::to_value(Standing {
        word: r.word().into(),
        key: r.key(),
        since: at,
    })
    .unwrap_or_default();
    write(
        p,
        &skey,
        Txn::new().put(&skey, Object::new(data).with_ttl(STANDING_TTL_SECS)),
        "set",
    );
    p.log.write(&match r {
        Refusal::Verb { batch, refusal } => Outcome::of::<()>(
            at,
            repo,
            batch.as_deref(),
            refusal.verb,
            &Err(refusal.clone()),
        ),
        Refusal::Admission { rule, .. } => Outcome::refused_write(at, repo, &r.key(), rule),
        r => Outcome::declined(at, repo, r.word(), &r.key()),
    });
}

fn write(p: &Ports, skey: &str, t: Txn, what: &str) {
    let req = Request::new(
        &format!("refusal-{what}-{skey}-{}", p.next_u64()),
        "round-controller",
        "refusal",
    );
    let _ = p.store.txn(&t, &req);
}
