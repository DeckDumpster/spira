//! The producer contract: a fragment names the kinds it owns, its own schema version, and its
//! provenance (producer, version, time). The operator's own settings are one producer among
//! them, carried as a fragment like any other — see `examples/default.toml`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::resource::RawResource;

/// A producer stamps its own clock: `time` is whatever the producer says it is, never
/// overwritten by the composer or the store (law-producers-stamp-their-own-clock).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub producer: String,
    pub version: String,
    pub time: String,
}

/// One producer's contribution: the resources it owns, plus who it is. A document handed to
/// `spira apply` is a fragment like any other, with the operator as its producer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fragment {
    pub producer: Provenance,
    #[serde(default)]
    pub resources: Vec<RawResource>,
}

/// Parses `text` as a fragment, reporting the first error at its TOML path — the same
/// diagnostic shape `spira-config::validate` uses.
pub fn parse_fragment(text: &str) -> Result<Fragment, String> {
    let de = toml::Deserializer::new(text);
    serde_path_to_error::deserialize(de).map_err(|e| {
        let path = e.path().to_string();
        if path.is_empty() {
            e.inner().to_string()
        } else {
            format!("{path}: {}", e.inner())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_minimal_fragment() {
        let text = r#"
            [producer]
            producer = "operator"
            version = "1"
            time = "2026-09-25T00:00:00Z"

            [[resources]]
            apiVersion = "spira/v1"
            kind = "Fleet"
            [resources.metadata]
            name = "fleet"
            [resources.spec]
            ceiling = 8
            lane_cap = 2
        "#;
        let frag = parse_fragment(text).expect("valid fragment");
        assert_eq!(frag.producer.producer, "operator");
        assert_eq!(frag.resources.len(), 1);
    }

    #[test]
    fn unknown_top_level_field_names_its_path() {
        let text = r#"
            [producer]
            producer = "operator"
            version = "1"
            time = "2026-09-25T00:00:00Z"
            bogus = true
        "#;
        let err = parse_fragment(text).unwrap_err();
        assert!(err.starts_with("producer.bogus"), "{err}");
    }
}
