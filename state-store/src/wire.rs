//! The JSON a remote client's contract calls and their answers travel as: the one definition the
//! client that encodes a call and the server that answers it both use. Every value and every
//! [`Error`] survives the trip unchanged.
//!
//! An answer is an exit code and one JSON object: 0 `{"ok": …}`, 3 `{"error": <Error>}` and 2
//! `{"cannot_tell": "…"}`. A backend failure is "cannot tell", never a refusal, because a caller
//! may retry the one and must not retry the other.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{Error, Fence, Item, Request, Result, Selector, Txn};

pub const OK: i32 = 0;
pub const CANNOT_TELL: i32 = 2;
pub const REFUSED: i32 = 3;

/// The calls, by the word that names each on a command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    Get,
    List,
    Txn,
    Lock,
    Renew,
    Release,
}

impl Verb {
    pub const ALL: [Verb; 6] = [
        Verb::Get,
        Verb::List,
        Verb::Txn,
        Verb::Lock,
        Verb::Renew,
        Verb::Release,
    ];

    pub fn word(self) -> &'static str {
        match self {
            Verb::Get => "get",
            Verb::List => "list",
            Verb::Txn => "txn",
            Verb::Lock => "lock",
            Verb::Renew => "renew",
            Verb::Release => "release",
        }
    }

    pub fn from_word(w: &str) -> Option<Verb> {
        Verb::ALL.into_iter().find(|v| v.word() == w)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Get {
    pub key: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct List {
    pub prefixes: Vec<String>,
    pub selector: Selector,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TxnCall {
    pub txn: Txn,
    pub request: Request,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lock {
    pub resource: String,
    pub ttl_ms: u64,
    pub request: Request,
}

/// `renew` and `release`: the holder's fence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fenced {
    pub fence: Fence,
    pub request: Request,
}

/// `list`'s answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Listed {
    pub items: Vec<Item>,
    pub list_rev: u64,
}

pub fn ok<T: Serialize>(v: &T) -> (i32, String) {
    match serde_json::to_value(v) {
        Ok(v) => (OK, json!({ "ok": v }).to_string()),
        Err(e) => cannot_tell(&format!("unencodable answer: {e}")),
    }
}

pub fn cannot_tell(reason: &str) -> (i32, String) {
    (CANNOT_TELL, json!({ "cannot_tell": reason }).to_string())
}

pub fn refused(e: &Error) -> (i32, String) {
    match e {
        Error::Backend(why) => cannot_tell(why),
        e => (REFUSED, json!({ "error": e }).to_string()),
    }
}

pub fn answer<T: Serialize>(r: &Result<T>) -> (i32, String) {
    match r {
        Ok(v) => ok(v),
        Err(e) => refused(e),
    }
}

/// The answer an exit code and its stdout carry. Anything but a well-formed `ok` or `error` is
/// `Backend`, carrying the text, so nothing unreadable passes for an answer.
pub fn decode<T: DeserializeOwned>(code: i32, stdout: &str) -> Result<T> {
    let v: Value = serde_json::from_str(stdout.trim())
        .map_err(|e| Error::Backend(format!("exit {code}, not JSON ({e}): {}", stdout.trim())))?;
    let field = |name: &str| {
        v.get(name)
            .cloned()
            .ok_or_else(|| Error::Backend(format!("exit {code} without {name:?}: {v}")))
    };
    let undecodable = |e: serde_json::Error| Error::Backend(format!("exit {code}: {e}: {v}"));
    match code {
        OK => serde_json::from_value(field("ok")?).map_err(undecodable),
        REFUSED => Err(serde_json::from_value(field("error")?).map_err(undecodable)?),
        CANNOT_TELL => Err(Error::Backend(match field("cannot_tell")? {
            Value::String(s) => s,
            other => other.to_string(),
        })),
        _ => Err(Error::Backend(format!("exit {code}: {v}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Compare, Limit, Object, Op, Rev, TxnResult};

    fn item() -> Item {
        Item {
            key: "locks/round/r/b1".into(),
            value: Object::new(json!({ "ttl_ms": 5000, "nested": { "a": [1, null, "x"] } }))
                .with_ttl(30)
                .with_finalizers(&["f"])
                .with_owners(&["batches/r/b1"]),
            mod_rev: Rev::new(7, 2),
            create_rev: Rev::new(u64::MAX, u32::MAX),
            lock_expires_at_ms: Some(1_800_000_005_000),
        }
    }

    fn request() -> Request {
        Request {
            evidence: json!({ "why": "test" }),
            ..Request::new("rq-1", "round-controller", "run.renew")
        }
    }

    fn fence() -> Fence {
        Fence {
            resource: "round/r/b1".into(),
            token: Rev::new(4, 1),
        }
    }

    fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(v: T) {
        let text = serde_json::to_string(&v).unwrap();
        assert_eq!(serde_json::from_str::<T>(&text).unwrap(), v, "{text}");
    }

    #[test]
    fn every_request_round_trips_to_an_equal_value() {
        let txn = Txn {
            compares: vec![
                Compare::ModRev("batches/r/b1".into(), Some(Rev::new(3, 0))),
                Compare::ModRev("batches/r/b2".into(), None),
                Compare::CreateRev("lanes/r".into(), Some(Rev::new(1, 0))),
                Compare::Value("slots/proxmox/0".into(), Some(Object::new(json!("v")))),
                Compare::Value("slots/proxmox/1".into(), None),
                Compare::LockCheck {
                    key: "locks/round/r/b1".into(),
                    token: Rev::new(4, 1),
                },
            ],
            ops: vec![
                Op::Put("batches/r/b1".into(), Object::new(json!({ "k": 1 }))),
                Op::Delete("batches/r/b1/progress".into()),
                Op::LockAcquire {
                    key: "locks/runner-slot/proxmox/0".into(),
                    ttl_ms: 9000,
                },
            ],
            else_ops: vec![Op::Put("faults/r/b/d".into(), Object::default())],
        };
        round_trip(Get { key: "k".into() });
        round_trip(List {
            prefixes: vec!["batches/".into(), "verdicts/".into()],
            selector: Selector::Everything,
        });
        round_trip(List {
            prefixes: vec![],
            selector: Selector::Eq("/state".into(), json!("OPEN")),
        });
        round_trip(TxnCall {
            txn,
            request: request(),
        });
        round_trip(Lock {
            resource: "round/r/b1".into(),
            ttl_ms: 30_000,
            request: request(),
        });
        round_trip(Fenced {
            fence: fence(),
            request: request(),
        });
        for v in Verb::ALL {
            assert_eq!(Verb::from_word(v.word()), Some(v));
        }
        assert_eq!(Verb::from_word("watch"), None);
    }

    fn through<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(r: Result<T>) {
        let (code, text) = answer(&r);
        assert_eq!(decode::<T>(code, &text), r, "{code} {text}");
    }

    #[test]
    fn every_answer_round_trips_to_an_equal_value() {
        through::<Option<Item>>(Ok(Some(item())));
        through::<Option<Item>>(Ok(None));
        through(Ok(Listed {
            items: vec![
                item(),
                Item {
                    lock_expires_at_ms: None,
                    ..item()
                },
            ],
            list_rev: 9,
        }));
        through(Ok(TxnResult {
            rev: 12,
            succeeded: true,
            already_applied: true,
            fences: vec![fence()],
        }));
        through(Ok(TxnResult {
            rev: 12,
            succeeded: false,
            already_applied: false,
            fences: vec![],
        }));
        through(Ok(fence()));
        through(Ok(()));
    }

    #[test]
    fn every_error_round_trips_to_an_equal_value() {
        let errors = [
            Error::Conflict {
                key: "k".into(),
                existing: Box::new(item()),
            },
            Error::NotFound { key: "k".into() },
            Error::RevMismatch {
                key: "k".into(),
                expected: Rev::new(1, 0),
                actual: Rev::new(2, 3),
            },
            Error::Limit(Limit::Ops { max: 128, got: 129 }),
            Error::Limit(Limit::Compares { max: 128, got: 200 }),
            Error::Limit(Limit::RequestBytes { max: 10, got: 11 }),
            Error::Limit(Limit::DuplicateKey { key: "k".into() }),
            Error::TtlRequired { key: "k".into() },
            Error::TtlNotAllowed { key: "k".into() },
            Error::ReservedKey { key: "k".into() },
            Error::TooOld { compacted: 5 },
            Error::FutureRev { rev: 9, head: 8 },
            Error::Locked {
                resource: "round/r/b1".into(),
                token: Rev::new(4, 1),
            },
            Error::LockLost {
                resource: "round/r/b1".into(),
            },
            Error::LockTtlBelowMinimum {
                min_ms: 1000,
                got_ms: 10,
            },
            Error::WaitTooLong {
                max_ms: 2500,
                got_ms: 9000,
            },
            Error::Stale {
                expected: 3,
                head: 4,
            },
            Error::Backend("disk full".into()),
        ];
        for e in errors {
            through::<()>(Err(e));
        }
    }

    #[test]
    fn a_backend_failure_is_cannot_tell_and_a_refusal_is_exit_3() {
        assert_eq!(refused(&Error::Backend("gone".into())).0, CANNOT_TELL);
        assert_eq!(refused(&Error::NotFound { key: "k".into() }).0, REFUSED);
    }

    #[test]
    fn an_unreadable_answer_is_backend_carrying_the_text() {
        for (code, text) in [
            (0, "not json"),
            (0, "{\"nope\":1}"),
            (3, "{\"error\":{\"no_such_variant\":{}}}"),
            (1, "{\"x\":1}"),
        ] {
            match decode::<()>(code, text) {
                Err(Error::Backend(why)) => assert!(why.contains(&format!("exit {code}")), "{why}"),
                other => panic!("{code} {text}: {other:?}"),
            }
        }
    }
}
