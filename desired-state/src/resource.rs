//! The typed resource kinds: an apiVersion, a kind, a metadata.name and a spec, in the style
//! of Kubernetes manifests. Every spec carries `deny_unknown_fields`, so a typo is a hard
//! error rather than a setting nobody reads (law-fail-closed-at-the-source).

use std::collections::BTreeMap;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The schema version every kind here is published at. A breaking change to any one kind's
/// spec ships as a new version plus an explicit migration, not a silent reinterpretation of
/// this constant.
pub const API_VERSION: &str = "spira/v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub name: String,
}

/// A resource as it appears on the wire, before its `spec` has been matched against its
/// `kind`'s schema. `spec` stays a raw TOML value until [`parse_resource`] validates it —
/// that is the seam an unknown `kind` or an unsupported `apiVersion` is refused at.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawResource {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
    pub metadata: Metadata,
    pub spec: toml::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FleetSpec {
    pub ceiling: u32,
    pub lane_cap: u32,
    #[serde(default)]
    pub partition_minimum: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnitSpec {
    pub name: String,
    pub enabled: bool,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnitsSpec {
    #[serde(default)]
    pub units: Vec<UnitSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StoreSpec {
    pub reachable: bool,
    pub schema_version: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CockpitSpec {
    #[serde(default)]
    pub dashboards: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReleaseSpec {
    pub release: String,
    #[serde(default)]
    pub allow_override: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueueSpec {
    pub cadence_seconds: u32,
    pub batch_width: u32,
    pub base_gate_green: bool,
    pub max_stale_batches: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowSpec {
    #[serde(default)]
    pub velocity_floor: BTreeMap<String, u32>,
    #[serde(default)]
    pub dwell_limit_seconds: BTreeMap<String, u32>,
}

/// The union of every kind's spec, published as one JSON Schema (`schema/resources.schema.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum KindSpec {
    Fleet(FleetSpec),
    Units(UnitsSpec),
    Store(StoreSpec),
    Cockpit(CockpitSpec),
    Release(ReleaseSpec),
    Queue(QueueSpec),
    Flow(FlowSpec),
}

impl KindSpec {
    pub fn kind_name(&self) -> &'static str {
        match self {
            KindSpec::Fleet(_) => "Fleet",
            KindSpec::Units(_) => "Units",
            KindSpec::Store(_) => "Store",
            KindSpec::Cockpit(_) => "Cockpit",
            KindSpec::Release(_) => "Release",
            KindSpec::Queue(_) => "Queue",
            KindSpec::Flow(_) => "Flow",
        }
    }
}

/// A [`RawResource`] whose `kind`, `apiVersion` and `spec` have all been checked.
#[derive(Debug, Clone, PartialEq)]
pub struct Resource {
    pub name: String,
    pub spec: KindSpec,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResourceError {
    UnknownKind { kind: String, name: String },
    UnsupportedApiVersion { kind: String, name: String, got: String },
    InvalidSpec { kind: String, name: String, message: String },
}

impl fmt::Display for ResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResourceError::UnknownKind { kind, name } => {
                write!(f, "{name}: unknown kind {kind:?}")
            }
            ResourceError::UnsupportedApiVersion { kind, name, got } => {
                write!(
                    f,
                    "{kind}/{name}: unsupported apiVersion {got:?} (want {API_VERSION:?})"
                )
            }
            ResourceError::InvalidSpec { kind, name, message } => {
                write!(f, "{kind}/{name}: {message}")
            }
        }
    }
}

fn deserialize_spec<T>(kind: &str, name: &str, value: &toml::Value) -> Result<T, ResourceError>
where
    T: serde::de::DeserializeOwned,
{
    value.clone().try_into().map_err(|e| ResourceError::InvalidSpec {
        kind: kind.to_string(),
        name: name.to_string(),
        message: e.to_string(),
    })
}

/// Validates one [`RawResource`] against its `kind`'s schema: an unrecognised `kind`, an
/// unsupported `apiVersion`, or a spec with an unknown or missing field are all refused here
/// rather than merged silently.
pub fn parse_resource(raw: &RawResource) -> Result<Resource, ResourceError> {
    if raw.api_version != API_VERSION {
        return Err(ResourceError::UnsupportedApiVersion {
            kind: raw.kind.clone(),
            name: raw.metadata.name.clone(),
            got: raw.api_version.clone(),
        });
    }
    let name = raw.metadata.name.clone();
    let spec = match raw.kind.as_str() {
        "Fleet" => KindSpec::Fleet(deserialize_spec(&raw.kind, &name, &raw.spec)?),
        "Units" => KindSpec::Units(deserialize_spec(&raw.kind, &name, &raw.spec)?),
        "Store" => KindSpec::Store(deserialize_spec(&raw.kind, &name, &raw.spec)?),
        "Cockpit" => KindSpec::Cockpit(deserialize_spec(&raw.kind, &name, &raw.spec)?),
        "Release" => KindSpec::Release(deserialize_spec(&raw.kind, &name, &raw.spec)?),
        "Queue" => KindSpec::Queue(deserialize_spec(&raw.kind, &name, &raw.spec)?),
        "Flow" => KindSpec::Flow(deserialize_spec(&raw.kind, &name, &raw.spec)?),
        other => {
            return Err(ResourceError::UnknownKind {
                kind: other.to_string(),
                name,
            })
        }
    };
    Ok(Resource { name, spec })
}

/// The JSON Schema every kind's spec is published against.
pub fn json_schema() -> schemars::schema::RootSchema {
    schemars::schema_for!(KindSpec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(kind: &str, name: &str, spec: toml::Value) -> RawResource {
        RawResource {
            api_version: API_VERSION.to_string(),
            kind: kind.to_string(),
            metadata: Metadata { name: name.to_string() },
            spec,
        }
    }

    #[test]
    fn unknown_kind_is_refused() {
        let r = raw("Warp", "x", toml::Value::Table(Default::default()));
        assert!(matches!(
            parse_resource(&r),
            Err(ResourceError::UnknownKind { .. })
        ));
    }

    #[test]
    fn unsupported_api_version_is_refused() {
        let mut r = raw("Fleet", "x", toml::Value::Table(Default::default()));
        r.api_version = "spira/v2".to_string();
        assert!(matches!(
            parse_resource(&r),
            Err(ResourceError::UnsupportedApiVersion { .. })
        ));
    }

    #[test]
    fn unknown_field_in_spec_is_refused() {
        let mut spec = toml::map::Map::new();
        spec.insert("ceiling".into(), toml::Value::Integer(8));
        spec.insert("lane_cap".into(), toml::Value::Integer(2));
        spec.insert("bogus".into(), toml::Value::Boolean(true));
        let r = raw("Fleet", "x", toml::Value::Table(spec));
        assert!(matches!(
            parse_resource(&r),
            Err(ResourceError::InvalidSpec { .. })
        ));
    }

    #[test]
    fn missing_required_field_in_spec_is_refused() {
        let mut spec = toml::map::Map::new();
        spec.insert("ceiling".into(), toml::Value::Integer(8));
        let r = raw("Fleet", "x", toml::Value::Table(spec));
        assert!(matches!(
            parse_resource(&r),
            Err(ResourceError::InvalidSpec { .. })
        ));
    }

    #[test]
    fn valid_fleet_spec_parses() {
        let mut spec = toml::map::Map::new();
        spec.insert("ceiling".into(), toml::Value::Integer(8));
        spec.insert("lane_cap".into(), toml::Value::Integer(2));
        let r = raw("Fleet", "fleet", toml::Value::Table(spec));
        let parsed = parse_resource(&r).expect("valid");
        assert_eq!(parsed.name, "fleet");
        match parsed.spec {
            KindSpec::Fleet(f) => {
                assert_eq!(f.ceiling, 8);
                assert_eq!(f.lane_cap, 2);
            }
            other => panic!("wrong kind: {other:?}"),
        }
    }
}
