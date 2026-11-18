//! Facts: events that move no bead's state but that the attempt count and the poison decision
//! fold. One key per fact, `facts/<bead>/<request id>`; a read orders each bead's by the revision
//! each committed at, after the facts carried over from the dolt database, which keep their order.

use serde_json::{json, Value};
use state_store::{Backend, Object, Request, Result, Selector, Txn};

use super::keys;

const CARRY_CHUNK: usize = 100;

/// The key component of facts carried over from the dolt database.
pub const CARRIED: &str = "carried-from-lifecycle";
use super::store::Store;

#[derive(Clone, Debug, PartialEq)]
pub struct Fact {
    pub bead: String,
    pub kind: String,
    pub cause: String,
    pub actor: String,
    pub at: i64,
}

impl Fact {
    fn stored(&self) -> Value {
        json!({ "kind": self.kind, "cause": self.cause, "actor": self.actor, "at": self.at })
    }

    fn read(bead: &str, v: &Value) -> Fact {
        let text = |k: &str| v[k].as_str().unwrap_or("").to_string();
        Fact { bead: bead.to_string(), kind: text("kind"), cause: text("cause"), actor: text("actor"), at: v["at"].as_i64().unwrap_or(0) }
    }
}

impl<B: Backend> Store<B> {
    pub fn append_facts(&self, facts: &[Fact], req: &Request) -> Result<()> {
        let mut t = Txn::new();
        for (n, f) in facts.iter().enumerate() {
            t = t.put(&keys::fact(&f.bead, &format!("{}/{n:04}", req.id)), Object::new(f.stored()));
        }
        self.contract().txn(&t, req).map(|_| ())
    }

    /// Facts carried over from another store, each under `<carry>/<n>` so a second carry of the
    /// same rows writes nothing: a chunk whose first key exists was carried whole. Returns how
    /// many facts this call wrote.
    pub fn carry_facts(&self, carry: &str, facts: &[Fact], actor: &str) -> Result<usize> {
        let mut written = 0;
        for (c, chunk) in facts.chunks(CARRY_CHUNK).enumerate() {
            let key = |n: usize, f: &Fact| keys::fact(&f.bead, &format!("{carry}/{:010}", c * CARRY_CHUNK + n));
            let mut t = Txn::new().when(state_store::Compare::ModRev(key(0, &chunk[0]), None));
            for (n, f) in chunk.iter().enumerate() {
                t = t.put(&key(n, f), Object::new(f.stored()));
            }
            let req = Request::new(&format!("{carry}/{c:010}"), actor, "carry-facts");
            let done = self.contract().txn(&t, &req)?;
            if done.succeeded && !done.already_applied {
                written += chunk.len();
            }
        }
        Ok(written)
    }

    /// The facts of `ids` (every bead's when empty), each bead's in the order they committed.
    pub fn facts(&self, ids: &[String]) -> Result<Vec<Fact>> {
        let prefixes: Vec<String> = if ids.is_empty() { vec![keys::FACTS.to_string()] } else { ids.iter().map(|i| keys::facts_of(i)).collect() };
        let (mut items, _) = self.contract().list(&prefixes.iter().map(String::as_str).collect::<Vec<_>>(), &Selector::Everything)?;
        let carried = |k: &str| k.contains(&format!("/{CARRIED}/"));
        items.sort_by_key(|i| (bead_of(&i.key).to_string(), !carried(&i.key), if carried(&i.key) { Default::default() } else { i.mod_rev }, i.key.clone()));
        Ok(items.iter().map(|i| Fact::read(bead_of(&i.key), &i.value.data)).collect())
    }
}

fn bead_of(key: &str) -> &str {
    key.strip_prefix(keys::FACTS).and_then(|k| k.split('/').next()).unwrap_or("")
}
