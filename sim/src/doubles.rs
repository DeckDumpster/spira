//! Effect doubles: the runner a simulated host answers effects with. A name with no double is
//! refused with the production runner's own text, so the handler sees what it would see live.

use std::collections::BTreeMap;
use std::sync::Mutex;

use runtime::host::EffectRunner;
use runtime::EffectSpec;

pub type Double = Box<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

#[derive(Default)]
pub struct Doubles {
    by_name: BTreeMap<String, Double>,
    asked: Mutex<Vec<String>>,
}

impl Doubles {
    pub fn double(mut self, name: &str, f: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static) -> Self {
        self.by_name.insert(name.into(), Box::new(f));
        self
    }

    /// Every effect name asked for, in the order it was run.
    pub fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl EffectRunner for Doubles {
    fn run(&self, _key: &str, spec: &EffectSpec) -> Result<String, String> {
        self.asked.lock().unwrap_or_else(|e| e.into_inner()).push(spec.name.clone());
        match self.by_name.get(&spec.name) {
            Some(f) => f(&spec.input),
            None => Err(format!("no runner for effect {}", spec.name)),
        }
    }
}
