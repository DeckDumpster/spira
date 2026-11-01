//! The GC controller: one `tick(now)` removes finalized keys, collects owner-reference orphans
//! and expires keys whose TTL has run out.

use crate::backend::Backend;
use crate::contract::Contract;
use crate::Result;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tick {
    pub finalized: Vec<String>,
    pub orphans: Vec<String>,
    pub expired: Vec<String>,
}

pub struct Controller<'a, B: Backend> {
    contract: &'a Contract<B>,
}

impl<'a, B: Backend> Controller<'a, B> {
    pub fn new(contract: &'a Contract<B>) -> Self {
        Controller { contract }
    }

    pub fn tick(&self, now_ms: u64) -> Result<Tick> {
        let _ = (self.contract, now_ms);
        todo!()
    }
}
