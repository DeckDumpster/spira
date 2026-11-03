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

/// The most rounds a repo can hold open at once that a reader takes as a whole set: far past
/// any round limit, so more is a store gone wrong, said so, not a list.
pub const OPEN_MAX: usize = 64;

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
    /// The items under `prefix` whose `data.state` is one of `states`, read through the store's
    /// state index; more than `limit` is an error, never a part of the set.
    fn list_in_states(&self, prefix: &str, states: &[String], limit: usize) -> Result<Vec<Item>, String>;
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

/// Every state the batch machine has not ended, as a record can hold it: serde's name for the
/// variant, which spira-lc writes, and the machine's word, so no spelling hides an open round.
fn open_spellings() -> Vec<String> {
    const OPEN: [BatchState; 8] = [
        BatchState::Staged,
        BatchState::Open,
        BatchState::CiRunning,
        BatchState::Green,
        BatchState::Landing,
        BatchState::Deploying,
        BatchState::Attributing,
        BatchState::Rebuilding,
    ];
    let mut out = Vec::new();
    for s in OPEN {
        debug_assert!(!s.is_terminal());
        if let Some(v) = serde_json::to_value(s).ok().and_then(|v| v.as_str().map(str::to_string)) {
            out.push(v);
        }
        out.push(s.as_str().to_string());
    }
    out.dedup();
    out
}

/// The machine's word (`OPEN`, `CI_RUNNING`) for a record's `state` as spira-lc writes it: its
/// `BatchRecord` carries the state as serde's name for the variant (`Open`, `CiRunning`), and a
/// record written in the machine's own word reads the same. `None`: no state the machine knows.
pub fn state_word(stored: &str) -> Option<&'static str> {
    BatchState::from_str(stored)
        .or_else(|| serde_json::from_value(Value::String(stored.to_string())).ok())
        .map(BatchState::as_str)
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
    let stored = text(d, "state").ok_or_else(|| undecodable("state"))?;
    let state = state_word(&stored)
        .ok_or_else(|| undecodable(&format!("batch state (it holds {stored:?})")))?
        .to_string();
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
    in_creation_order(repo, d.list(&format!("{PREFIX}{repo}/"))?)
}

/// The rounds among `items` (not the keys beneath them), decoded, oldest first.
fn in_creation_order(repo: &str, items: Vec<Item>) -> Result<Vec<BatchRecord>, String> {
    let mut items: Vec<Item> = items
        .into_iter()
        .filter(|i| round_id(repo, &i.key).is_some())
        .collect();
    items.sort_by_key(|i| i.create_rev);
    let mut out: Vec<BatchRecord> = items.iter().map(decode).collect::<Result<_, _>>()?;
    // Rounds imported in one txn share a creation revision; their cut time orders them.
    out.sort_by_key(|b| {
        let opened: i64 = b.kv.get("opened").and_then(|o| o.parse().ok()).unwrap_or(0);
        (b.item.create_rev.main, opened, b.id.clone())
    });
    Ok(out)
}

/// The rounds of `repo` the batch machine has not ended, in the order they were created: read
/// through the store's state index, so the cost is the open rounds, never every round the repo
/// has had, which an import from an older store brings in by the thousand.
pub fn open_batches(d: &dyn Door, repo: &str) -> Result<Vec<BatchRecord>, String> {
    let items = d.list_in_states(&format!("{PREFIX}{repo}/"), &open_spellings(), OPEN_MAX)?;
    Ok(in_creation_order(repo, items)?
        .into_iter()
        .filter(|b| !b.is_terminal())
        .collect())
}

/// A door whose every call fails with why the store's routing could not be resolved, so an
/// unresolvable config is "cannot tell", never "no round open".
pub struct Unresolved(pub String);

impl Door for Unresolved {
    fn get(&self, _: &str) -> Result<Option<Item>, String> {
        Err(self.0.clone())
    }
    fn list(&self, _: &str) -> Result<Vec<Item>, String> {
        Err(self.0.clone())
    }
    fn list_in_states(&self, _: &str, _: &[String], _: usize) -> Result<Vec<Item>, String> {
        Err(self.0.clone())
    }
    fn txn(&self, _: &Txn, _: &str) -> Result<TxnResult, String> {
        Err(self.0.clone())
    }
}

pub fn batch(d: &dyn Door, repo: &str, id: &str) -> Result<Option<BatchRecord>, String> {
    d.get(&key(repo, id))?.as_ref().map(decode).transpose()
}

/// The queue's own fields of `kv`: everything but `batch_id` and `repo`, as the record's `round`
/// map holds them.
pub fn fields(kv: &Kv) -> serde_json::Map<String, Value> {
    kv.lines
        .iter()
        .filter(|(f, _)| f != "batch_id" && f != "repo")
        .map(|(f, v)| (f.clone(), Value::String(v.clone())))
        .collect()
}

/// Writes the queue's fields of `kv` (everything but `batch_id` and `repo`) into the round's
/// record, compared on the revision it was read at, so a machine step in between is re-read and
/// kept rather than overwritten.
pub fn save(d: &dyn Door, repo: &str, kv: &Kv) -> Result<(), String> {
    let id = kv.get("batch_id").unwrap_or("");
    let k = key(repo, id);
    let own = fields(kv);
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

/// [`save`], only while the record's field `field` still reads `expect`: `Ok(false)` when another
/// writer moved that field first, so the caller's step is no longer the record's next one.
pub fn save_if(d: &dyn Door, repo: &str, kv: &Kv, field: &str, expect: &str) -> Result<bool, String> {
    let id = kv.get("batch_id").unwrap_or("");
    let k = key(repo, id);
    let own = fields(kv);
    for _ in 0..SAVE_TRIES {
        let Some(item) = d.get(&k)? else {
            return Err(format!("the control store holds no round {id} for {repo}"));
        };
        if decode(&item)?.kv.get(field) != Some(expect) {
            return Ok(false);
        }
        let mut value = item.value.clone();
        value.data["round"] = Value::Object(own.clone());
        let t = Txn::new()
            .when(Compare::ModRev(k.clone(), Some(item.mod_rev)))
            .put(&k, value);
        if d.txn(&t, "queue.round-fields")?.succeeded {
            return Ok(true);
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
        /// A txn of this kind answers "cannot tell": the queue died before it was written.
        pub lost_kind: Cell<Option<&'static str>>,
        /// The kinds of the txns written through the door, in order.
        pub kinds: std::cell::RefCell<Vec<String>>,
        /// How many records the door has answered with: what a read costs the store.
        pub items_read: Cell<usize>,
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
                lost_kind: Cell::new(None),
                kinds: Default::default(),
                items_read: Cell::new(0),
            }
        }
    }

    /// `state` (`OPEN`, the machine's word) spelled as spira-lc's `BatchRecord` writes it into
    /// the record: serde's name for the variant (`Open`). A word the machine does not know is
    /// written as given.
    pub fn stored(state: &str) -> String {
        BatchState::from_str(state)
            .and_then(|s| serde_json::to_value(s).ok())
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| state.to_string())
    }

    impl MemDoor {
        fn request(&self, kind: &str) -> Request {
            self.n.set(self.n.get() + 1);
            Request::new(&format!("test/{}", self.n.get()), "queue", kind)
        }

        /// A round as spira-lc's cut writes it, in `state`.
        pub fn put_round(&self, repo: &str, id: &str, state: &str, members: &[(&str, &str)], opened_at: i64) {
            self.cut_round(repo, id, state, members, opened_at, None);
        }

        /// [`MemDoor::put_round`], with the queue's fields `round` in the same txn when given.
        pub fn cut_round(
            &self,
            repo: &str,
            id: &str,
            state: &str,
            members: &[(&str, &str)],
            opened_at: i64,
            round: Option<&Kv>,
        ) {
            let members: Vec<Value> = members
                .iter()
                .map(|(m, tip)| serde_json::json!({ "bead_id": m, "tip": tip }))
                .collect();
            let v = serde_json::json!({
                "batch_id": id, "repo": repo, "state": stored(state), "parent": null,
                "head": format!("head-{id}"), "base": "b0", "run": null, "reason": null,
                "pass": 0, "phase": null, "version": 1,
                "members": members, "opened_at": opened_at,
            });
            let mut v = v;
            if let Some(kv) = round {
                v["round"] = Value::Object(fields(kv));
            }
            let t = Txn::new().put(&key(repo, id), Object::new(v));
            assert!(self.store.txn(&t, &self.request("seed")).unwrap().succeeded);
        }

        /// The machine moves round `id` to `state`, keeping every other field.
        pub fn set_state(&self, repo: &str, id: &str, state: &str) {
            self.step(repo, id, state, None);
        }

        /// One machine step of round `id` to `state`, with the queue's fields `round` written in
        /// the same txn when given, as spira-lc's `--round` writes them.
        pub fn step(&self, repo: &str, id: &str, state: &str, round: Option<&Kv>) {
            let k = key(repo, id);
            let Some(item) = self.store.get(&k).unwrap() else {
                return;
            };
            let mut value = item.value;
            value.data["state"] = Value::String(stored(state));
            let n = value.data["version"].as_u64().unwrap_or(0) + 1;
            value.data["version"] = n.into();
            if let Some(kv) = round {
                value.data["round"] = Value::Object(fields(kv));
            }
            let t = Txn::new().put(&k, value);
            assert!(self.store.txn(&t, &self.request("step")).unwrap().succeeded);
        }

        /// The repo, state and version of round `id`, in whichever repo it was cut.
        pub fn find(&self, id: &str) -> Option<(String, String, String)> {
            let (items, _) = self.store.list(&[PREFIX], &state_store::Selector::Everything).unwrap();
            let d = &items.iter().find(|i| i.value.data["batch_id"] == id)?.value.data;
            Some((
                d["repo"].as_str()?.to_string(),
                state_word(d["state"].as_str()?).map_or_else(|| d["state"].to_string(), str::to_string),
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
            let got = self.store.get(key).map_err(|e| e.to_string())?;
            self.items_read.set(self.items_read.get() + usize::from(got.is_some()));
            Ok(got)
        }
        fn list(&self, prefix: &str) -> Result<Vec<Item>, String> {
            self.up()?;
            let items = self
                .store
                .list(&[prefix], &state_store::Selector::Everything)
                .map(|(items, _)| items)
                .map_err(|e| e.to_string())?;
            self.items_read.set(self.items_read.get() + items.len());
            Ok(items)
        }
        fn list_in_states(&self, prefix: &str, states: &[String], limit: usize) -> Result<Vec<Item>, String> {
            self.up()?;
            let at = self.store.head().map_err(|e| e.to_string())?;
            let mut items = Vec::new();
            for state in states {
                items.extend(
                    self.store
                        .range_in_state(prefix, state, None, limit + 1, at)
                        .map_err(|e| e.to_string())?,
                );
            }
            self.items_read.set(self.items_read.get() + items.len());
            if items.len() > limit {
                return Err(format!("cannot tell: more than {limit} rounds under {prefix} are open"));
            }
            Ok(items)
        }
        fn txn(&self, t: &Txn, kind: &str) -> Result<TxnResult, String> {
            self.up()?;
            if self.lost_kind.get() == Some(kind) {
                return Err(format!("cannot tell: the {kind} write was lost"));
            }
            self.kinds.borrow_mut().push(kind.to_string());
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
        let none = Unresolved("cannot tell: SPIRA_LC_LIFECYCLE_ON_STORE does not resolve".into());
        assert!(open_batches(&none, "spira").unwrap_err().contains("does not resolve"));
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
    fn a_state_reads_in_either_spelling_and_an_unknown_one_is_undecodable_not_open() {
        assert_eq!(state_word("Abandoned"), Some("ABANDONED"));
        assert_eq!(state_word("CiRunning"), Some("CI_RUNNING"));
        assert_eq!(state_word("LANDED"), Some("LANDED"));
        assert_eq!(state_word("abandoned"), None);
        let d = MemDoor::default();
        d.put_round("spira", "spira-1", "LANDED", &[], 1);
        let b = batch(&d, "spira", "spira-1").unwrap().unwrap();
        assert!(b.is_terminal() && b.state == "LANDED", "{b:?}");
        d.put_round("spira", "spira-2", "Retired", &[], 2);
        let e = batch(&d, "spira", "spira-2").unwrap_err();
        assert!(e.contains("Retired"), "{e}");
    }

    #[test]
    fn a_progress_key_under_a_round_is_not_a_round() {
        assert_eq!(round_id("spira", "batches/spira/b1"), Some("b1"));
        assert_eq!(round_id("spira", "batches/spira/b1/progress"), None);
        assert_eq!(round_id("spira", "batches/spiral/b1"), None);
    }
}
