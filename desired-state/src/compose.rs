//! The composer: merges fragments into one composite, refusing a conflict (two producers
//! claiming the same resource) or an unknown kind rather than picking a winner
//! (law-fail-closed-at-the-source). Pure — no IO — so it replay-tests against synthetic
//! fragments; [`crate::store`] is the IO seam that turns a [`Composite`] into a durable,
//! versioned artifact.

use std::collections::BTreeMap;
use std::fmt;

use sha2::{Digest, Sha256};

use crate::producer::Fragment;
use crate::resource::{check_resource, RawResource, ResourceError};

#[derive(Debug, Clone)]
pub struct ComposedResource {
    pub raw: RawResource,
    pub producer: String,
}

/// The merged desired state: one resource per (kind, name), each still attributed to the
/// producer that claimed it. Ordered by key so serialization and hashing are deterministic.
#[derive(Debug, Clone, Default)]
pub struct Composite {
    pub resources: BTreeMap<(String, String), ComposedResource>,
}

impl Composite {
    /// A hash of the *content* — kind, name and spec — deliberately excluding provenance, so
    /// re-running the same producers at a later time does not manufacture a new version.
    pub fn content_hash(&self) -> String {
        let mut hasher = Sha256::new();
        for ((kind, name), resource) in &self.resources {
            hasher.update(kind.as_bytes());
            hasher.update([0u8]);
            hasher.update(name.as_bytes());
            hasher.update([0u8]);
            let spec = toml::to_string(&resource.raw.spec).unwrap_or_default();
            hasher.update(spec.as_bytes());
            hasher.update([0u8]);
        }
        format!("{:x}", hasher.finalize())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ComposeError {
    UnknownKind { producer: String, kind: String, name: String },
    UnsupportedApiVersion { producer: String, kind: String, name: String, got: String },
    InvalidSpec { producer: String, kind: String, name: String, message: String },
    Conflict { kind: String, name: String, producers: Vec<String> },
}

impl fmt::Display for ComposeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ComposeError::UnknownKind { producer, kind, name } => {
                write!(f, "{producer}: {name}: unknown kind {kind:?}")
            }
            ComposeError::UnsupportedApiVersion { producer, kind, name, got } => {
                write!(f, "{producer}: {kind}/{name}: unsupported apiVersion {got:?}")
            }
            ComposeError::InvalidSpec { producer, kind, name, message } => {
                write!(f, "{producer}: {kind}/{name}: {message}")
            }
            ComposeError::Conflict { kind, name, producers } => {
                write!(
                    f,
                    "{kind}/{name}: claimed by more than one producer: {}",
                    producers.join(", ")
                )
            }
        }
    }
}

fn from_resource_error(producer: String, e: ResourceError) -> ComposeError {
    match e {
        ResourceError::UnknownKind { kind, name } => ComposeError::UnknownKind { producer, kind, name },
        ResourceError::UnsupportedApiVersion { kind, name, got } => {
            ComposeError::UnsupportedApiVersion { producer, kind, name, got }
        }
        ResourceError::InvalidSpec { kind, name, message } => {
            ComposeError::InvalidSpec { producer, kind, name, message }
        }
    }
}

/// Merges `fragments` into a [`Composite`]. Refuses — the whole compose, not just the
/// offending resource — on any unknown kind, unsupported apiVersion, invalid spec, or a
/// (kind, name) claimed by more than one producer.
pub fn compose(fragments: &[Fragment]) -> Result<Composite, Vec<ComposeError>> {
    let mut claims: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let mut resources: BTreeMap<(String, String), ComposedResource> = BTreeMap::new();
    let mut errors: Vec<ComposeError> = Vec::new();

    for fragment in fragments {
        for raw in &fragment.resources {
            let key = (raw.kind.clone(), raw.metadata.name.clone());
            claims
                .entry(key.clone())
                .or_default()
                .push(fragment.producer.producer.clone());
            match check_resource(raw) {
                Ok(()) => {
                    resources.insert(
                        key,
                        ComposedResource {
                            raw: raw.clone(),
                            producer: fragment.producer.producer.clone(),
                        },
                    );
                }
                Err(e) => errors.push(from_resource_error(fragment.producer.producer.clone(), e)),
            }
        }
    }

    for ((kind, name), producers) in &claims {
        if producers.len() > 1 {
            errors.push(ComposeError::Conflict {
                kind: kind.clone(),
                name: name.clone(),
                producers: producers.clone(),
            });
        }
    }

    if !errors.is_empty() {
        errors.sort();
        return Err(errors);
    }
    Ok(Composite { resources })
}

/// Each section kind's spec in the composite, as the body `spira-lc section apply-spec`
/// takes. A composite with no section kind has none, and publishing it writes nothing.
pub fn section_specs(composite: &Composite) -> Vec<serde_json::Value> {
    composite
        .resources
        .values()
        .filter(|r| crate::sections::lookup(&r.raw.kind).is_some())
        .filter_map(|r| serde_json::to_value(&r.raw).ok())
        .collect()
}

/// Hands each section spec to `apply`, stopping at the first that fails.
pub fn publish_specs(
    composite: &Composite,
    apply: &mut dyn FnMut(&serde_json::Value) -> Result<(), String>,
) -> Result<usize, String> {
    let specs = section_specs(composite);
    for s in &specs {
        apply(s)?;
    }
    Ok(specs.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::producer::Provenance;
    use crate::resource::Metadata;

    fn fragment(producer: &str, resources: Vec<RawResource>) -> Fragment {
        Fragment {
            producer: Provenance {
                producer: producer.to_string(),
                version: "1".to_string(),
                time: "2026-09-25T00:00:00Z".to_string(),
            },
            resources,
        }
    }

    fn fleet(name: &str, ceiling: i64) -> RawResource {
        let mut spec = toml::map::Map::new();
        spec.insert("ceiling".into(), toml::Value::Integer(ceiling));
        spec.insert("lane_cap".into(), toml::Value::Integer(2));
        RawResource {
            api_version: crate::resource::API_VERSION.to_string(),
            kind: "Fleet".to_string(),
            metadata: Metadata { name: name.to_string() },
            spec: toml::Value::Table(spec),
        }
    }

    #[test]
    fn merges_disjoint_fragments() {
        let a = fragment("operator", vec![fleet("fleet", 8)]);
        let b = fragment("release-owner", vec![]);
        let composite = compose(&[a, b]).expect("no conflict");
        assert_eq!(composite.resources.len(), 1);
    }

    #[test]
    fn two_producers_claiming_the_same_resource_is_refused() {
        let a = fragment("operator", vec![fleet("fleet", 8)]);
        let b = fragment("intruder", vec![fleet("fleet", 3)]);
        let errors = compose(&[a, b]).expect_err("conflict");
        assert!(matches!(errors[0], ComposeError::Conflict { .. }), "{errors:?}");
    }

    #[test]
    fn unknown_kind_is_refused_whole() {
        let mut bogus = fleet("x", 8);
        bogus.kind = "Warp".to_string();
        let a = fragment("operator", vec![bogus]);
        let errors = compose(&[a]).expect_err("refused");
        assert!(matches!(errors[0], ComposeError::UnknownKind { .. }), "{errors:?}");
    }

    #[test]
    fn content_hash_is_stable_across_producer_and_time_changes() {
        let a = fragment("operator", vec![fleet("fleet", 8)]);
        let composite_a = compose(&[a]).unwrap();

        let mut b = fragment("operator-v2", vec![fleet("fleet", 8)]);
        b.producer.time = "2099-01-01T00:00:00Z".to_string();
        let composite_b = compose(std::slice::from_ref(&b)).unwrap();

        assert_eq!(composite_a.content_hash(), composite_b.content_hash());
    }

    #[test]
    fn content_hash_changes_when_a_spec_changes() {
        let a = compose(&[fragment("operator", vec![fleet("fleet", 8)])]).unwrap();
        let b = compose(&[fragment("operator", vec![fleet("fleet", 9)])]).unwrap();
        assert_ne!(a.content_hash(), b.content_hash());
    }

    fn probe(spec: &str) -> RawResource {
        RawResource {
            api_version: crate::resource::API_VERSION.to_string(),
            kind: crate::sections::probe::NAME.to_string(),
            metadata: Metadata { name: "p".to_string() },
            spec: toml::from_str(spec).unwrap(),
        }
    }

    #[test]
    fn a_section_kinds_spec_composes_and_is_published_and_no_other_kind_is() {
        let ok = probe(r#"depth_max = { value = 4.0, statute = "law-x" }"#);
        let composite = compose(&[fragment("operator", vec![ok, fleet("fleet", 8)])]).unwrap();
        let mut sent = Vec::new();
        let n = publish_specs(&composite, &mut |v| {
            sent.push(v.clone());
            Ok(())
        })
        .unwrap();
        assert_eq!(n, 1);
        assert_eq!(sent[0]["kind"], "Probe");
        assert_eq!(sent[0]["spec"]["depth_max"]["value"], 4.0);

        let plain = compose(&[fragment("operator", vec![fleet("fleet", 8)])]).unwrap();
        let n = publish_specs(&plain, &mut |_| Err("called".into())).unwrap();
        assert_eq!(n, 0, "a composite with no section kind publishes nothing");
    }

    #[test]
    fn a_section_spec_outside_its_kinds_type_is_refused_whole() {
        let bad = probe(r#"depth_max = { value = 4.0, statute = "law-x", extra = 1 }"#);
        let errors = compose(&[fragment("operator", vec![bad])]).expect_err("refused");
        assert!(matches!(errors[0], ComposeError::InvalidSpec { .. }), "{errors:?}");
        let mut v1 = probe("");
        v1.api_version = "spira/v0".into();
        let errors = compose(&[fragment("operator", vec![v1])]).expect_err("refused");
        assert!(matches!(errors[0], ComposeError::UnsupportedApiVersion { .. }), "{errors:?}");
    }
}
