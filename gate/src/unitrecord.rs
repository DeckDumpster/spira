//! What a unit-tests run cost and decided: `testenv --record` writes it, `work checks` reads it
//! back. A field the run did not reach is absent, never zero.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UnitRecord {
    /// `direct`, `transitive` or `tests`.
    #[serde(default)]
    pub selection: Option<String>,
    #[serde(default)]
    pub crates: Vec<String>,
    /// `sandbox` or `container`.
    #[serde(default)]
    pub runner: Option<String>,
    /// `owned` (the session's build directory) or `branch` (the runner's per-branch one).
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub compiled: Option<u64>,
    #[serde(default)]
    pub fresh: Option<u64>,
    #[serde(default)]
    pub build_s: Option<f64>,
    #[serde(default)]
    pub test_s: Option<f64>,
    #[serde(default)]
    pub load1_start: Option<f64>,
    #[serde(default)]
    pub load_wait_s: Option<u64>,
    #[serde(default)]
    pub skipped: Option<u64>,
    #[serde(default)]
    pub failing: Vec<String>,
    #[serde(default)]
    pub rc: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_cut_early_round_trips_with_only_the_fields_it_reached() {
        let r = UnitRecord { selection: Some("transitive".into()), crates: vec!["a".into()], rc: Some(2), ..Default::default() };
        let back: UnitRecord = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
        assert_eq!(serde_json::from_str::<UnitRecord>("{}").unwrap(), UnitRecord::default());
    }
}
