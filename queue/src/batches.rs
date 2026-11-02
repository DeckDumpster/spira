//! The rounds of a repo as the control store holds them, under `batches/<repo>/<id>`, read and
//! written only through spira-lc's `rc` door (law-only-spira-lc-talks-to-the-stores). A round's
//! state is the batch machine's; the fields only the queue keeps ride in the record's `round`
//! map, which every machine step carries unchanged.

use lifecycle::batch::BatchState;
use serde_json::Value;
use state_store::{Compare, Item, Txn, TxnResult};

use crate::model::Member;
use crate::records::Kv;

pub const PREFIX: &str = "batches/";

/// How many times a field write is retried when a machine step moved the record under it.
const SAVE_TRIES: usize = 3;

/// The queue's fields, in the order `round open` writes them; any other follows by name.
const FIELD_ORDER: [&str; 7] = [
    "head", "base", "parent", "members", "worktree", "actor", "opened",
];

/// The rc calls the queue makes: `get`, `list` and `txn`.
pub trait Door {
    fn get(&self, key: &str) -> Result<Option<Item>, String>;
    fn list(&self, prefix: &str) -> Result<Vec<Item>, String>;
    /// `kind` names the write in the request the call commits under.
    fn txn(&self, t: &Txn, kind: &str) -> Result<TxnResult, String>;
}

/// One round: the machine's state and the queue's view of the record, as the round files held
/// it (`batch_id`, `repo`, then the queue's own fields).
#[derive(Debug, Clone, PartialEq)]
pub struct BatchRecord {
    pub id: String,
    pub state: String,
    pub kv: Kv,
    item: Item,
}

impl BatchRecord {
    pub fn is_terminal(&self) -> bool {
        BatchState::from_str(&self.state).is_some_and(BatchState::is_terminal)
    }
}

pub fn key(repo: &str, id: &str) -> String {
    format!("{PREFIX}{repo}/{id}")
}

/// The round id of a round's own key in `repo`; `None` for a key beneath one (its progress).
fn round_id<'a>(repo: &str, key: &'a str) -> Option<&'a str> {
    let id = key.strip_prefix(PREFIX)?.strip_prefix(repo)?.strip_prefix('/')?;
    (!id.is_empty() && !id.contains('/')).then_some(id)
}

fn text(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

pub fn decode(item: &Item) -> Result<BatchRecord, String> {
    let d = &item.value.data;
    let undecodable = |what: &str| format!("{}: undecodable round: no {what}", item.key);
    let id = text(d, "batch_id").ok_or_else(|| undecodable("batch_id"))?;
    let state = text(d, "state").ok_or_else(|| undecodable("state"))?;
    let mut kv = Kv::default();
    kv.push("batch_id", &id);
    kv.push("repo", &text(d, "repo").unwrap_or_default());
    match d.get("round").and_then(Value::as_object).filter(|m| !m.is_empty()) {
        Some(own) => {
            let mut fields: Vec<(&String, &Value)> = own.iter().collect();
            let rank = |k: &str| FIELD_ORDER.iter().position(|f| *f == k).unwrap_or(FIELD_ORDER.len());
            fields.sort_by(|a, b| (rank(a.0), a.0).cmp(&(rank(b.0), b.0)));
            for (k, v) in fields {
                kv.push(k, v.as_str().ok_or_else(|| undecodable(&format!("text for {k}")))?);
            }
        }
        None => machine_fields(d, &mut kv),
    }
    Ok(BatchRecord {
        id,
        state,
        kv,
        item: item.clone(),
    })
}

/// A round the queue never wrote (one cut or imported by another writer), from the machine's
/// own fields.
fn machine_fields(d: &Value, kv: &mut Kv) {
    for k in ["head", "base", "parent"] {
        if let Some(v) = text(d, k) {
            kv.push(k, &v);
        }
    }
    let members: Vec<Member> = d
        .get("members")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            Some(Member {
                id: text(m, "bead_id")?,
                tip: text(m, "tip")?,
            })
        })
        .collect();
    kv.push("members", &crate::model::render_members(&members));
    if let Some(at) = d.get("opened_at").and_then(Value::as_i64) {
        kv.push("opened", &at.to_string());
    }
}

/// Every round of `repo`, in the order they were created.
pub fn batches(d: &dyn Door, repo: &str) -> Result<Vec<BatchRecord>, String> {
    let mut items: Vec<Item> = d
        .list(&format!("{PREFIX}{repo}/"))?
        .into_iter()
        .filter(|i| round_id(repo, &i.key).is_some())
        .collect();
    items.sort_by_key(|i| i.create_rev);
    items.iter().map(decode).collect()
}

/// The rounds of `repo` the batch machine has not ended, in the order they were created.
pub fn open_batches(d: &dyn Door, repo: &str) -> Result<Vec<BatchRecord>, String> {
    Ok(batches(d, repo)?
        .into_iter()
        .filter(|b| !b.is_terminal())
        .collect())
}

pub fn batch(d: &dyn Door, repo: &str, id: &str) -> Result<Option<BatchRecord>, String> {
    d.get(&key(repo, id))?.as_ref().map(decode).transpose()
}

/// Writes the queue's fields of `kv` (everything but `batch_id` and `repo`) into the round's
/// record, compared on the revision it was read at, so a machine step in between is re-read and
/// kept rather than overwritten.
pub fn save(d: &dyn Door, repo: &str, kv: &Kv) -> Result<(), String> {
    let id = kv.get("batch_id").unwrap_or("");
    let k = key(repo, id);
    let own: serde_json::Map<String, Value> = kv
        .lines
        .iter()
        .filter(|(f, _)| f != "batch_id" && f != "repo")
        .map(|(f, v)| (f.clone(), Value::String(v.clone())))
        .collect();
    for _ in 0..SAVE_TRIES {
        let Some(item) = d.get(&k)? else {
            return Err(format!("the control store holds no round {id} for {repo}"));
        };
        let mut value = item.value.clone();
        value.data["round"] = Value::Object(own.clone());
        let t = Txn::new()
            .when(Compare::ModRev(k.clone(), Some(item.mod_rev)))
            .put(&k, value);
        if d.txn(&t, "queue.round-fields")?.succeeded {
            return Ok(());
        }
    }
    Err(format!(
        "round {id}'s record moved under each of {SAVE_TRIES} writes of its fields"
    ))
}

#[cfg(test)]
pub mod mem {
    //! A [`Door`] straight onto an in-memory contract, for the queue's unit tests.

    use std::cell::Cell;
    use std::sync::Arc;

    use state_store::{Config, Contract, MemBackend, MonotonicClock, Object, Ports, Request};

    use super::*;

    pub struct MemDoor {
        pub store: Contract<MemBackend>,
        n: Cell<u64>,
        /// Every call answers "cannot tell", as an unreachable lc-serve does.
        pub down: Cell<bool>,
    }

    impl Default for MemDoor {
        fn default() -> Self {
            let ports = Ports {
                mono: Arc::new(MonotonicClock::new()),
                wall: Arc::new(MonotonicClock::new()),
            };
            MemDoor {
                store: Contract::open(MemBackend::new(), ports, Config { min_lock_ttl_ms: 1 })
                    .expect("an in-memory store opens"),
                n: Cell::new(0),
                down: Cell::new(false),
            }
        }
    }

    impl MemDoor {
        fn request(&self, kind: &str) -> Request {
            self.n.set(self.n.get() + 1);
            Request::new(&format!("test/{}", self.n.get()), "queue", kind)
        }

        /// A round as spira-lc's cut writes it, in `state`.
        pub fn put_round(&self, repo: &str, id: &str, state: &str, members: &[(&str, &str)], opened_at: i64) {
            let members: Vec<Value> = members
                .iter()
                .map(|(m, tip)| serde_json::json!({ "bead_id": m, "tip": tip }))
                .collect();
            let v = serde_json::json!({
                "batch_id": id, "repo": repo, "state": state, "parent": null,
                "head": format!("head-{id}"), "base": "b0", "run": null, "reason": null,
                "pass": 0, "phase": null, "version": 1,
                "members": members, "opened_at": opened_at,
            });
            let t = Txn::new().put(&key(repo, id), Object::new(v));
            assert!(self.store.txn(&t, &self.request("seed")).unwrap().succeeded);
        }

        /// The machine moves round `id` to `state`, keeping every other field.
        pub fn set_state(&self, repo: &str, id: &str, state: &str) {
            let k = key(repo, id);
            let Some(item) = self.store.get(&k).unwrap() else {
                return;
            };
            let mut value = item.value;
            value.data["state"] = Value::String(state.into());
            let n = value.data["version"].as_u64().unwrap_or(0) + 1;
            value.data["version"] = n.into();
            let t = Txn::new().put(&k, value);
            assert!(self.store.txn(&t, &self.request("step")).unwrap().succeeded);
        }

        /// The repo, state and version of round `id`, in whichever repo it was cut.
        pub fn find(&self, id: &str) -> Option<(String, String, String)> {
            let (items, _) = self.store.list(&[PREFIX], &state_store::Selector::Everything).unwrap();
            let d = &items.iter().find(|i| i.value.data["batch_id"] == id)?.value.data;
            Some((
                d["repo"].as_str()?.to_string(),
                d["state"].as_str()?.to_string(),
                d["version"].to_string(),
            ))
        }
    }

    impl MemDoor {
        fn up(&self) -> Result<(), String> {
            match self.down.get() {
                true => Err("cannot tell: lc-serve did not answer".into()),
                false => Ok(()),
            }
        }
    }

    impl Door for MemDoor {
        fn get(&self, key: &str) -> Result<Option<Item>, String> {
            self.up()?;
            self.store.get(key).map_err(|e| e.to_string())
        }
        fn list(&self, prefix: &str) -> Result<Vec<Item>, String> {
            self.up()?;
            self.store
                .list(&[prefix], &state_store::Selector::Everything)
                .map(|(items, _)| items)
                .map_err(|e| e.to_string())
        }
        fn txn(&self, t: &Txn, kind: &str) -> Result<TxnResult, String> {
            self.up()?;
            self.store.txn(t, &self.request(kind)).map_err(|e| e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mem::MemDoor;
    use super::*;

    #[test]
    fn open_batches_are_the_unended_rounds_of_the_repo_in_creation_order() {
        let d = MemDoor::default();
        d.put_round("spira", "spira-3", "OPEN", &[("sp-c", "tc")], 30);
        d.put_round("spira", "spira-1", "ABANDONED", &[("sp-a", "ta")], 10);
        d.put_round("spira", "spira-2", "GREEN", &[("sp-b", "tb")], 20);
        d.put_round("other", "other-1", "OPEN", &[("sp-o", "to")], 5);
        for terminal in ["LANDED", "SETTLED", "DISCARDED"] {
            d.put_round("spira", &format!("spira-{terminal}"), terminal, &[], 1);
        }
        let open: Vec<String> = open_batches(&d, "spira").unwrap().into_iter().map(|b| b.id).collect();
        assert_eq!(open, ["spira-3", "spira-2"]);
        assert_eq!(batches(&d, "spira").unwrap().len(), 6);
    }

    #[test]
    fn a_round_the_queue_never_wrote_reads_as_the_machines_fields() {
        let d = MemDoor::default();
        d.put_round("spira", "spira-1", "OPEN", &[("sp-a", "ta"), ("sp-b", "tb")], 10);
        let b = batch(&d, "spira", "spira-1").unwrap().unwrap();
        assert_eq!(
            b.kv.render(),
            "batch_id=spira-1\nrepo=spira\nhead=head-spira-1\nbase=b0\nmembers=sp-a:ta sp-b:tb\nopened=10\n"
        );
        assert!(batch(&d, "spira", "spira-9").unwrap().is_none());
    }

    #[test]
    fn saved_fields_read_back_as_written_and_a_machine_step_after_keeps_them() {
        let d = MemDoor::default();
        d.put_round("spira", "spira-1", "OPEN", &[("sp-a", "ta")], 10);
        let mut kv = batch(&d, "spira", "spira-1").unwrap().unwrap().kv;
        kv.push("worktree", "/wt");
        kv.push("red", "test-a.sh");
        kv.remove("base");
        save(&d, "spira", &kv).unwrap();
        d.set_state("spira", "spira-1", "CI_RUNNING");
        let b = batch(&d, "spira", "spira-1").unwrap().unwrap();
        assert_eq!(b.state, "CI_RUNNING");
        assert_eq!(
            b.kv.render(),
            "batch_id=spira-1\nrepo=spira\nhead=head-spira-1\nmembers=sp-a:ta\nworktree=/wt\nopened=10\nred=test-a.sh\n"
        );
    }

    #[test]
    fn a_save_of_a_round_the_store_does_not_hold_is_refused_naming_it() {
        let d = MemDoor::default();
        let mut kv = Kv::default();
        kv.push("batch_id", "spira-9");
        let e = save(&d, "spira", &kv).unwrap_err();
        assert!(e.contains("spira-9") && e.contains("spira"), "{e}");
    }

    #[test]
    fn a_progress_key_under_a_round_is_not_a_round() {
        assert_eq!(round_id("spira", "batches/spira/b1"), Some("b1"));
        assert_eq!(round_id("spira", "batches/spira/b1/progress"), None);
        assert_eq!(round_id("spira", "batches/spiral/b1"), None);
    }
}
