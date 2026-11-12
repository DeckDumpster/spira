//! A config change reaching the running scheduler. `spira-config set` names the key; the scheduler
//! reads it again and rebuilds the tasks that pin it, between turns and never while the batcher is
//! driving a round or one of those tasks has an effect in flight. Keys stay pinned in between: a
//! task sees one value for the whole of a tick.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use runtime::{Engine, EventKind, Executor};

use crate::tasks::a::batcher_local;
use crate::Settings;

pub const STREAM: &str = "scheduler.config";
pub const WANTED: &str = "config.repin.wanted";
pub const REPINNED: &str = "config.repinned";

type Fresh = Arc<dyn Fn(&[String]) -> Result<BTreeMap<String, String>, String> + Send + Sync>;
type Lookup = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

#[derive(Default)]
struct State {
    /// Re-pinned keys over the values the process started with.
    pinned: BTreeMap<String, String>,
    /// Keys changed in the file and not yet taken, with the value they will take.
    wanted: BTreeMap<String, String>,
    waits: String,
}

pub struct Repin {
    base: Settings,
    fresh: Fresh,
    lookup: Lookup,
    state: Mutex<State>,
}

impl Repin {
    /// `fresh` reads named keys from the files again; `lookup` is the pin the process started with.
    pub fn new(base: Settings, fresh: Fresh, lookup: Lookup) -> Self {
        Repin { base, fresh, lookup, state: Mutex::default() }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The value the scheduler holds for `key`; `None` for a key it does not pin per task.
    pub fn held(&self, key: &str) -> Option<String> {
        let base = self.base.task_config.get(key)?;
        Some(self.state().pinned.get(key).unwrap_or(base).clone())
    }

    /// Records that `key` changed in the file. The answer says what the scheduler holds and takes.
    pub fn request(&self, key: &str) -> Result<serde_json::Value, String> {
        let Some(held) = self.held(key) else {
            let held = (self.lookup)(key)?;
            let wanted = (self.fresh)(&[key.to_string()])?.remove(key).unwrap_or_default();
            return Ok(serde_json::json!({ "state": "restart", "held": held, "wanted": wanted }));
        };
        let wanted = (self.fresh)(&[key.to_string()])?.remove(key).unwrap_or_default();
        let mut st = self.state();
        if wanted == held {
            st.wanted.remove(key);
            return Ok(serde_json::json!({ "state": "unchanged", "held": held, "wanted": wanted }));
        }
        st.wanted.insert(key.to_string(), wanted.clone());
        Ok(serde_json::json!({ "state": "pending", "held": held, "wanted": wanted }))
    }

    /// The `repin` section of `scheduler status`: what waits, and what for.
    pub fn document(&self) -> serde_json::Value {
        let (wanted, waits) = {
            let st = self.state();
            (st.wanted.clone(), st.waits.clone())
        };
        let pending: BTreeMap<&String, serde_json::Value> = wanted.iter().map(|(k, w)| (k, serde_json::json!({ "held": self.held(k), "wanted": w }))).collect();
        serde_json::json!({ "pending": pending, "waits": waits })
    }

    /// Takes what can be taken now. Runs at the start of a turn, with the engine between ticks.
    pub fn apply(&self, engine: &mut Engine) {
        let wanted = self.state().wanted.clone();
        if wanted.is_empty() {
            return;
        }
        if engine.carry_body(batcher_local::NAME, batcher_local::STATE).is_some_and(|b| batcher_local::driving(&b)) {
            self.state().waits = "the batcher to finish the round it is driving".into();
            return;
        }
        let status = engine.status();
        let busy: BTreeSet<&str> = status.tasks.iter().filter(|t| !t.in_flight.is_empty()).map(|t| t.name.as_str()).collect();
        let mut take = BTreeMap::new();
        let mut waits = String::new();
        for (key, value) in &wanted {
            let blocked = crate::tasks::all().find(|t| busy.contains(t.name) && t.keys.iter().flat_map(|ks| ks.iter()).any(|k| k == key));
            match blocked {
                Some(t) => waits = format!("{} to finish the effect it has in flight", t.name),
                None => {
                    take.insert(key.clone(), value.clone());
                }
            }
        }
        if take.is_empty() {
            self.state().waits = waits;
            return;
        }
        let mut pinned = self.state().pinned.clone();
        pinned.extend(take.clone());
        let lookup = self.lookup.clone();
        let next = Settings::from_lookup(&|k| pinned.get(k).cloned().map_or_else(|| lookup(k), Ok)).and_then(|s| {
            let mut s = s.with_environment(&|k| std::env::var(k).ok().filter(|v| !v.is_empty()))?;
            s.reconciler.home = self.base.reconciler.home.clone();
            Ok(s)
        });
        let next = match next {
            Ok(s) => s,
            Err(e) => {
                self.state().waits = format!("re-pin refused: {e}");
                return;
            }
        };
        let mut lines = vec![];
        for t in crate::tasks::all().filter(|t| t.keys.iter().flat_map(|ks| ks.iter()).any(|k| take.contains_key(*k))) {
            engine.rebind(t.name, (t.build)(&next).handler);
        }
        for (key, value) in &take {
            let old = self.held(key).unwrap_or_default();
            lines.push(format!("re-pinned {key} {old} -> {value}"));
        }
        {
            let mut st = self.state();
            for k in take.keys() {
                st.wanted.remove(k);
            }
            st.pinned = pinned;
            st.waits = waits;
        }
        for line in lines {
            eprintln!("spira-runtime: {line}");
            let id = format!("{REPINNED}/{}/{}", engine.now_ms(), line.replace(' ', "_"));
            if let Err(e) = engine.submit(&id, STREAM, EventKind::new(REPINNED, &line)) {
                eprintln!("spira-runtime: the re-pin was not journaled: {e}");
            }
        }
    }
}
