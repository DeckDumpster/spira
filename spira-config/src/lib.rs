//! spira-config — the typed schema behind `spira.toml`.
//!
//! One document, three kinds of table: `[spira]` (host-wide keys), `[repo.<name>]` (a
//! checkout the harness may work in) and `[persona.<name>]` (an aeon's fayth, cut down to
//! the fields the harness itself reads rather than a persona's prose). Every struct here
//! carries `deny_unknown_fields`, so a typo is a hard error instead of a setting nobody is
//! reading — the same failure mode `spira.conf`'s own allowlist exists to catch, now
//! enforced by the type system instead of a hand-maintained string.
//!
//! `[spira]` covers the keys a real install actually sets, not every key `conf.sh` allows —
//! conf.sh's own allowlist runs past 200, most of them derived defaults nothing overrides.
//! Widening this table is scoped to the cutover bead, where each consumer's own reads say
//! which of the rest still need a home.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub mod convert;

/// The root of `spira.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpiraToml {
    pub spira: Option<SpiraSection>,
    #[serde(default)]
    pub repo: BTreeMap<String, RepoSection>,
    #[serde(default)]
    pub persona: BTreeMap<String, PersonaSection>,
}

/// on/off, spelled as an enum rather than a bool so a TOML reader sees the word `spira.conf`
/// already used (`SPIRA_CERTIFY_SUITES = off`) instead of learning a second spelling for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OnOff {
    On,
    Off,
}

/// A czar stage: shadow investigates and writes CZAR-WOULD notes; act permits mutation.
/// One representative key (`czar_stage_deadlock`) ships here — the other six
/// `SPIRA_CZAR_STAGE_*` classes are the same enum and widen with the rest of `[spira]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CzarStage {
    Shadow,
    Act,
}

/// `[spira]` — host-wide keys. Every field is optional: `conf.sh` derives a default for
/// each of these from where the harness is installed, and a clean clone sets none of them.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpiraSection {
    pub home_repo: Option<String>,
    pub db: Option<String>,
    pub run: Option<String>,
    pub goal: Option<String>,
    pub path: Option<String>,
    pub workspaces: Option<String>,
    pub prod: Option<String>,
    pub dolt_data: Option<String>,
    pub wiki: Option<String>,
    pub wiki_hook: Option<String>,
    pub view: Option<String>,
    pub tz: Option<String>,
    pub operator: Option<String>,
    pub operator_actor: Option<String>,
    pub ask_label: Option<String>,
    pub ci_label: Option<String>,
    pub scope_label: Option<String>,
    pub actionable: Option<String>,
    pub notify_age: Option<u64>,
    pub verdict_ttl: Option<u64>,
    pub max_aeons: Option<u32>,
    pub max_live_aeons: Option<u32>,
    #[serde(default)]
    pub fayths: Vec<String>,
    pub batch_maxpar: Option<u32>,
    pub certify_par: Option<u32>,
    pub certify_suites: Option<OnOff>,
    pub cert_idle_skip: Option<bool>,
    pub queue_batch_max: Option<u32>,
    pub queue_batch_wait: Option<u64>,
    pub queue_ci_maxsec: Option<u64>,
    pub queue_local_gate: Option<bool>,
    pub queue_throttle_release_at: Option<u32>,
    pub suites_budget: Option<u64>,
    pub loom_addr: Option<String>,
    pub loom_budget_ms: Option<u64>,
    pub mail_readers: Option<String>,
    pub gh_intake_repo: Option<String>,
    pub cockpit_bottom_pct: Option<u32>,
    pub cockpit_right_pct: Option<u32>,
    pub czar_stage_deadlock: Option<CzarStage>,
}

/// How a landed branch reaches its base — see `repo-map.example`'s own `land` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LandMode {
    Push,
    Pr,
    Hold,
    Queue,
}

/// A persona lane a repository admits — the explicit form of `repo-map.example`'s `lanes`
/// column. A row's shorthand mode (`consume`/`develop`/`self`) is expanded to this array by
/// the converter; the schema itself only ever sees the expanded list.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Lane {
    Plan,
    Incident,
    Groom,
    MaechenSweep,
    Spike,
    CzarTrigger,
}

/// `[repo.<name>]` — a checkout the harness may work in. `path` and `mode` are the two
/// facts nothing can derive: where the checkout is, and what a green gate does with a
/// branch. Everything else has a sensible empty reading (no format, no explicit base to
/// resolve, no extra lanes).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepoSection {
    pub path: String,
    pub mode: LandMode,
    pub base: Option<String>,
    pub format: Option<String>,
    pub gate: Option<String>,
    #[serde(default)]
    pub lanes: Vec<Lane>,
    /// Overrides the host's default forge script for this one repository. Absent means
    /// "use `[spira]`'s own", which is every repository today.
    pub forge: Option<String>,
}

/// `append` keeps Claude Code's own coding guidance underneath the persona layer;
/// `replace` is for a persona narrow enough that the default guidance would mislead it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SystemPromptMode {
    Append,
    Replace,
}

/// The claim TTL and heartbeat cadence a persona's lease runs on.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub minutes: Option<u32>,
    pub heartbeat_seconds: Option<u32>,
}

/// `[persona.<name>]` — a fayth, cut down to what the harness itself reads to summon and
/// dispatch it. `model` is the one field with no sensible default: an aeon summoned under
/// no model is not a cheaper aeon, it is a dead one, so it is required.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PersonaSection {
    pub model: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    pub lane: Option<String>,
    #[serde(default)]
    pub lease: Option<Lease>,
    pub system_prompt: Option<SystemPromptMode>,
}

/// The JSON Schema for [`SpiraToml`], as shipped in `schema/spira.schema.json`. Generated
/// from the same types `validate` deserializes into, so the shipped schema and the actual
/// hard errors can never name different fields.
pub fn json_schema() -> schemars::schema::RootSchema {
    schemars::schema_for!(SpiraToml)
}

/// Parses `text` as `spira.toml` and reports the first error at the TOML path it occurred
/// on (`spira.max_aeons`, `repo.service.mode`, ...) rather than a bare line/column, so a
/// hard error names the thing to fix instead of the place the parser gave up.
pub fn validate(text: &str) -> Result<SpiraToml, String> {
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

/// Reads one dotted path out of an already-validated document — `spira.max_aeons`,
/// `repo.service.mode`, `persona.builder.lease.minutes` — for `spira-config get`.
pub fn get_path(doc: &SpiraToml, path: &str) -> Option<String> {
    let value = serde_json::to_value(doc).ok()?;
    let mut cur = &value;
    for seg in path.split('.') {
        cur = cur.get(seg)?;
    }
    match cur {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// Renders `[spira]` as quoted `KEY=value` lines — `spira-config export --sh` — for the
/// bash callers this schema has not replaced yet. Only scalar and list fields have a bash
/// shape; tables (`repo`, `persona`) are not exported.
pub fn export_sh(doc: &SpiraToml) -> String {
    let Some(spira) = &doc.spira else {
        return String::new();
    };
    let value = serde_json::to_value(spira).unwrap_or(serde_json::Value::Null);
    let serde_json::Value::Object(map) = value else {
        return String::new();
    };
    let mut out = String::new();
    for (key, val) in map {
        let shell_val = match val {
            serde_json::Value::Null => continue,
            serde_json::Value::String(s) => s,
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Array(items) => {
                if items.is_empty() {
                    continue;
                }
                items
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| v.to_string())
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            }
            serde_json::Value::Object(_) => continue,
        };
        out.push_str(&key.to_uppercase());
        out.push('=');
        out.push_str(&shell_quote(&shell_val));
        out.push('\n');
    }
    out
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_minimal_document() {
        let doc = validate("[spira]\nmax_aeons = 4\n").expect("valid");
        assert_eq!(doc.spira.unwrap().max_aeons, Some(4));
    }

    #[test]
    fn unknown_key_names_its_path() {
        let err = validate("[spira]\nbogus = 1\n").unwrap_err();
        assert!(err.starts_with("spira.bogus"), "{err}");
    }

    #[test]
    fn wrong_type_names_its_path() {
        let err = validate("[spira]\nmax_aeons = \"four\"\n").unwrap_err();
        assert!(err.starts_with("spira.max_aeons"), "{err}");
    }

    #[test]
    fn bad_enum_names_its_path() {
        let err = validate("[spira]\nczar_stage_deadlock = \"sometimes\"\n").unwrap_err();
        assert!(err.starts_with("spira.czar_stage_deadlock"), "{err}");
    }

    #[test]
    fn missing_required_repo_field() {
        let err = validate("[repo.home]\nmode = \"push\"\n").unwrap_err();
        assert!(err.starts_with("repo.home"), "{err}");
    }

    #[test]
    fn missing_required_persona_field() {
        let err = validate("[persona.builder]\ntools = [\"Bash\"]\n").unwrap_err();
        assert!(err.starts_with("persona.builder"), "{err}");
    }

    #[test]
    fn the_inline_comment_scar_is_refused_unquoted() {
        // sp-upkae's own scar: an inline `#` comment after a bare, unquoted value used to
        // be silently absorbed into that value by conf.sh's KEY=value reader. TOML has no
        // such ambiguity to inherit: a bare word is not a legal value at all, so the same
        // line is a hard parse error here rather than a value nobody refused.
        let err = validate("[spira]\ndb = /home/x # a trailing comment\n").unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn the_inline_comment_scar_is_harmless_quoted() {
        let doc = validate("[spira]\ndb = \"/home/x\" # a trailing comment\n").expect("valid");
        assert_eq!(doc.spira.unwrap().db, Some("/home/x".to_string()));
    }

    #[test]
    fn get_path_reads_nested_tables() {
        let doc =
            validate("[repo.home]\npath = \"/srv/checkouts/home\"\nmode = \"push\"\n").unwrap();
        assert_eq!(get_path(&doc, "repo.home.mode"), Some("push".to_string()));
        assert_eq!(get_path(&doc, "repo.home.base"), None);
    }

    #[test]
    fn export_sh_quotes_and_uppercases() {
        let doc = validate("[spira]\nhome_repo = \"a b\"\nmax_aeons = 4\n").unwrap();
        let out = export_sh(&doc);
        assert!(out.contains("HOME_REPO='a b'\n"), "{out}");
        assert!(out.contains("MAX_AEONS='4'\n"), "{out}");
    }
}
