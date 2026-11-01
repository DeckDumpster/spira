//! The section kinds admission knows: each kind's owner, its typed Spec and Status, and the
//! condition types it declares. A kind not here is refused on write and absent to readers;
//! each real kind is added by its own component's work.

use serde::de::DeserializeOwned;
use serde_json::Value;

pub mod probe;

/// The andon cord's kind. Its own design registers it; until then it is refused by name.
pub const ANDON: &str = "Andon";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConditionDecl {
    pub type_: &'static str,
    /// Raising it pages whatever the per-section cap says.
    pub page_on_raise: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldShape {
    /// `{ value, window }`: the only shape a distance or an action's `moves` may name.
    Numeric,
    /// Detail for a reader; never a term.
    List,
}

/// The field at `.0` and why it does not fit the kind's type.
pub type SchemaError = (String, String);

#[derive(Debug)]
pub struct SectionKind {
    pub name: &'static str,
    /// The one actor that writes this kind's status, flow, distance and actions, and acks its
    /// intents.
    pub owner: &'static str,
    pub conditions: &'static [ConditionDecl],
    pub fields: &'static [(&'static str, FieldShape)],
    pub check_spec: fn(&Value) -> Result<(), SchemaError>,
    pub check_status: fn(&Value) -> Result<(), SchemaError>,
}

impl SectionKind {
    pub fn condition(&self, type_: &str) -> Option<&'static ConditionDecl> {
        self.conditions.iter().find(|c| c.type_ == type_)
    }

    pub fn shape(&self, field: &str) -> Option<FieldShape> {
        self.fields.iter().find(|(f, _)| *f == field).map(|(_, s)| *s)
    }
}

/// Checks `v` against the typed `T`, naming the first field that does not fit.
pub fn typed<T: DeserializeOwned>(v: &Value) -> Result<(), SchemaError> {
    serde_path_to_error::deserialize::<_, T>(v)
        .map(|_| ())
        .map_err(|e| (e.path().to_string(), e.inner().to_string()))
}

static KINDS: &[SectionKind] = &[probe::KIND];

pub fn registry() -> &'static [SectionKind] {
    KINDS
}

pub fn lookup(kind: &str) -> Option<&'static SectionKind> {
    KINDS.iter().find(|k| k.name == kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn andon_is_not_registered_and_names_are_unique() {
        assert!(lookup(ANDON).is_none());
        let mut names: Vec<_> = registry().iter().map(|k| k.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), registry().len());
    }
}
