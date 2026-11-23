//! What gates a repository's branch, read from file text alone: the landing ref's `gate.steps`,
//! the tiers its `.spira/testenv.toml` declares, its configured column, or nothing.

use crate::parse_foreign;

pub const BUILD_TIER: &str = "build";
pub const QUEUE_LOCAL: &str = "queue.local";
/// The gate's NO_VERDICT reason for a repository that declares tiers its gate runs none of.
pub const CERTIFIES_NOTHING: &str = "certifies-nothing";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateSource {
    Steps,
    Testenv { tiers: Vec<String>, build_tier: bool },
    Column(String),
    Nothing { declares_tiers: bool },
}

fn doc(text: &str) -> Result<toml::Value, String> {
    parse_foreign(text)
}

/// The tiers a declaration names, `build` aside.
pub fn declared_tiers(text: &str) -> Result<Vec<String>, String> {
    let d = doc(text)?;
    Ok(d.get("tier").and_then(|t| t.as_table()).map(|t| t.keys().filter(|k| *k != BUILD_TIER).cloned().collect()).unwrap_or_default())
}

/// The tiers a bare testenv run runs: the declaration's non-empty `default`, else every declared tier but `build`.
pub fn default_tiers(text: &str) -> Result<Vec<String>, String> {
    let d = doc(text)?;
    let declared: Vec<String> = d.get("default").and_then(|d| d.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    if declared.is_empty() {
        declared_tiers(text)
    } else {
        Ok(declared)
    }
}

pub fn has_build_tier(text: &str) -> Result<bool, String> {
    Ok(doc(text)?.get("tier").and_then(|t| t.get(BUILD_TIER)).is_some())
}

/// The declaration's `ci_entry`, trimmed; None when absent or blank.
pub fn ci_entry(text: &str) -> Result<Option<String>, String> {
    Ok(doc(text)?.get("ci_entry").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(String::from))
}

/// Order: `gate.steps`; then, with no harness Containerfile, `queue.local` and a non-empty
/// `ci_entry`, testenv; then a non-empty column; else nothing. A declaration that does not
/// read is an `Err` whenever it would have been consulted.
pub fn resolve(has_steps: bool, declaration: Option<&str>, has_containerfile: bool, land: &str, column: &str) -> Result<GateSource, String> {
    if has_steps {
        return Ok(GateSource::Steps);
    }
    let mut declares_tiers = false;
    if let Some(text) = declaration.filter(|_| !has_containerfile) {
        let (tiers, build_tier, entry) = (default_tiers(text)?, has_build_tier(text)?, ci_entry(text)?);
        if land == QUEUE_LOCAL && entry.is_some() {
            return Ok(GateSource::Testenv { tiers, build_tier });
        }
        declares_tiers = !declared_tiers(text)?.is_empty();
    }
    if !column.trim().is_empty() {
        return Ok(GateSource::Column(column.to_string()));
    }
    Ok(GateSource::Nothing { declares_tiers })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DECL: &str = "ci_entry = \"bash gate.sh\"\ndefault = [\"gate\"]\n[tier.gate]\ncmd = \"x\"\n[tier.other]\ncmd = \"y\"\n[tier.build]\ncmd = \"z\"\n";

    #[test_id(t6tu482)]
    #[test]
    fn a_gate_steps_file_decides_before_anything_else() {
        assert_eq!(resolve(true, Some(DECL), false, QUEUE_LOCAL, "bash a").unwrap(), GateSource::Steps);
    }

    #[test_id(t67c1ez)]
    #[test]
    fn a_queue_local_declaration_with_a_ci_entry_gates_by_its_default_tiers() {
        assert_eq!(
            resolve(false, Some(DECL), false, QUEUE_LOCAL, "bash gate.sh").unwrap(),
            GateSource::Testenv { tiers: vec!["gate".into()], build_tier: true }
        );
        let bare = "ci_entry = \"x\"\n[tier.a]\ncmd = \"x\"\n";
        assert_eq!(resolve(false, Some(bare), false, QUEUE_LOCAL, "").unwrap(), GateSource::Testenv { tiers: vec!["a".into()], build_tier: false });
    }

    #[test_id(thhy3fc)]
    #[test]
    fn a_declaration_does_not_gate_a_forge_repository_or_one_without_a_ci_entry() {
        assert_eq!(resolve(false, Some(DECL), false, "queue", "col").unwrap(), GateSource::Column("col".into()));
        let no_entry = "[tier.a]\ncmd = \"x\"\n";
        assert_eq!(resolve(false, Some(no_entry), false, QUEUE_LOCAL, "").unwrap(), GateSource::Nothing { declares_tiers: true });
    }

    #[test_id(t7dvgi6)]
    #[test]
    fn a_harness_containerfile_makes_the_declaration_not_the_gate() {
        assert_eq!(resolve(false, Some(DECL), true, QUEUE_LOCAL, "").unwrap(), GateSource::Nothing { declares_tiers: false });
    }

    #[test_id(tyv4o2r)]
    #[test]
    fn a_column_gates_when_no_testenv_declaration_does_and_nothing_gates_otherwise() {
        assert_eq!(resolve(false, None, false, QUEUE_LOCAL, "bash gate.sh").unwrap(), GateSource::Column("bash gate.sh".into()));
        assert_eq!(resolve(false, None, false, QUEUE_LOCAL, "  ").unwrap(), GateSource::Nothing { declares_tiers: false });
        let only_build = "[tier.build]\ncmd = \"z\"\n";
        assert_eq!(resolve(false, Some(only_build), false, QUEUE_LOCAL, "").unwrap(), GateSource::Nothing { declares_tiers: false });
    }

    #[test_id(tdbzbze)]
    #[test]
    fn a_declaration_that_does_not_parse_is_an_error_never_nothing() {
        assert!(resolve(false, Some("ci_entry = ["), false, QUEUE_LOCAL, "").is_err());
        assert!(resolve(false, Some("ci_entry = ["), false, QUEUE_LOCAL, "bash a").is_err());
    }
}
