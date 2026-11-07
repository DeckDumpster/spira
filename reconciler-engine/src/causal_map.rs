//! The causal map: one static weight per section kind, in goodput per day lost per unit of
//! distance, from a reviewed seed file. Nothing here learns: a misranked gap stays until the
//! file is edited.

use std::collections::BTreeMap;

use serde::Deserialize;
use spira_desired_state::sections::{self, probe};

const SEED: &str = include_str!("../../reconciler/causal-map.toml");

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Weight {
    pub kind: String,
    pub weight: f64,
    pub unit: String,
    pub source: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    weight: Vec<Weight>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CausalMap {
    weights: BTreeMap<String, Weight>,
}

impl CausalMap {
    /// The map in `text`, refused unless every kind in `required` has exactly one line, every
    /// line names a registered kind, and each weight is a finite, non-negative number with its
    /// evidence.
    pub fn load(text: &str, required: &[&str]) -> Result<CausalMap, String> {
        let file: File = toml::from_str(text).map_err(|e| format!("the causal map does not parse: {e}"))?;
        let mut weights = BTreeMap::new();
        for w in file.weight {
            if sections::lookup(&w.kind).is_none() {
                return Err(format!("the causal map weighs {:?}, which is no section kind", w.kind));
            }
            if !w.weight.is_finite() || w.weight < 0.0 {
                return Err(format!("the causal map's weight for {} is {}, not a finite number from 0", w.kind, w.weight));
            }
            if w.source.trim().is_empty() || w.unit.trim().is_empty() {
                return Err(format!("the causal map's weight for {} names no unit or no source", w.kind));
            }
            if let Some(dup) = weights.insert(w.kind.clone(), w) {
                return Err(format!("the causal map weighs {} twice", dup.kind));
            }
        }
        if let Some(kind) = required.iter().find(|k| !weights.contains_key(**k)) {
            return Err(format!("the causal map has no weight for the section kind {kind}"));
        }
        Ok(CausalMap { weights })
    }

    /// The reviewed seed file, shipped with this build: every kind a component publishes.
    pub fn seed() -> Result<CausalMap, String> {
        let published: Vec<&str> = sections::registry().iter().map(|k| k.name).filter(|n| *n != probe::NAME).collect();
        CausalMap::load(SEED, &published)
    }

    pub fn weight(&self, kind: &str) -> Option<f64> {
        self.weights.get(kind).map(|w| w.weight)
    }
}

#[cfg(test)]
mod tests;
