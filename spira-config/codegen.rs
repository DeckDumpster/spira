//! Generates `[spira]`'s struct, the `KEY => field` mapping and the key history from the
//! config key registry: `spira/conf.d/<KEY>` (keys conf.sh accepts) and `spira/conf.toml.d/<KEY>`
//! (keys only spira.toml carries). Std-only: `build.rs` and the registry tests both `#[path]`-include it.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

pub struct Key {
    pub key: String,
    pub field: String,
    pub ty: String,
    pub toml_only: bool,
    pub max: Option<f64>,
    pub schema_doc: Option<String>,
}

pub fn field_of(key: &str) -> String {
    key.strip_prefix("SPIRA_").unwrap_or(key).to_ascii_lowercase()
}

fn rust_type(ty: &str) -> Option<&'static str> {
    Some(match ty {
        "string" => "Option<String>",
        "u32" => "Option<u32>",
        "u64" => "Option<u64>",
        "bool" => "Option<bool>",
        "onoff" => "Option<crate::OnOff>",
        "czar_stage" => "Option<crate::CzarStage>",
        "list" => "Vec<String>",
        _ => return None,
    })
}

fn parse_file(key: &str, text: &str, toml_only: bool) -> Result<Key, String> {
    let (mut ty, mut max, mut schema_doc) = (None, None, None);
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("TYPE=") {
            ty = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("MAX=") {
            max = Some(v.trim().parse::<f64>().map_err(|e| format!("{key}: MAX={v:?}: {e}"))?);
        } else if let Some(v) = line.strip_prefix("SCHEMA=") {
            schema_doc = Some(v.trim().to_string());
        } else if line.starts_with("DEFAULT<<") {
            break;
        }
    }
    let ty = ty.ok_or_else(|| format!("{key}: no TYPE= line"))?;
    if rust_type(&ty).is_none() {
        return Err(format!(
            "{key}: TYPE={ty:?} is not one of string u32 u64 bool list onoff czar_stage"
        ));
    }
    Ok(Key { key: key.to_string(), field: field_of(key), ty, toml_only, max, schema_doc })
}

fn read_dir_keys(dir: &Path, toml_only: bool, out: &mut Vec<Key>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if !name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
            continue;
        }
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        out.push(parse_file(name, &text, toml_only)?);
    }
    Ok(())
}

pub fn load(conf_d: &Path, conf_toml_d: &Path) -> Result<Vec<Key>, String> {
    let mut keys = Vec::new();
    read_dir_keys(conf_d, false, &mut keys)?;
    if conf_toml_d.is_dir() {
        read_dir_keys(conf_toml_d, true, &mut keys)?;
    }
    keys.sort_by(|a, b| a.key.cmp(&b.key));
    let mut seen = BTreeSet::new();
    for k in &keys {
        if !seen.insert(k.field.clone()) {
            return Err(format!("{}: field {} is declared twice in the registry", k.key, k.field));
        }
    }
    if keys.is_empty() {
        return Err(format!("{} holds no keys", conf_d.display()));
    }
    Ok(keys)
}

pub fn section_rs(keys: &[Key]) -> String {
    let mut o = String::new();
    o.push_str("/// `[spira]` — host-wide keys, one field per registry key. Every field but `id_prefix` is\n");
    o.push_str("/// optional (see `require_id_prefix`); conf.sh derives each default.\n");
    o.push_str("#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]\n");
    o.push_str("#[serde(deny_unknown_fields)]\npub struct SpiraSection {\n");
    for k in keys {
        if let Some(d) = &k.schema_doc {
            o.push_str(&format!("    #[doc = {d:?}]\n"));
        }
        if k.ty == "list" {
            o.push_str("    #[serde(default)]\n");
        }
        o.push_str(&format!("    pub {}: {},\n", k.field, rust_type(&k.ty).unwrap()));
    }
    o.push_str("}\n\npub const SPIRA_KEYS: &[SpiraKey] = &[\n");
    for k in keys {
        o.push_str(&format!(
            "    SpiraKey {{ key: {:?}, field: {:?}, ty: {:?}, toml_only: {} }},\n",
            k.key, k.field, k.ty, k.toml_only
        ));
    }
    o.push_str("];\n\npub const SPIRA_FIELD_MAX: &[(&str, f64)] = &[\n");
    for k in keys {
        if let Some(m) = k.max {
            o.push_str(&format!("    ({:?}, {:?}),\n", k.field, m));
        }
    }
    o.push_str("];\n");
    o
}

pub fn convert_rs(keys: &[Key]) -> String {
    let mut o = String::from(
        "pub(crate) fn apply_spira_key(\n    s: &mut SpiraSection,\n    key: &str,\n    val: &String,\n    warnings: &mut ConvertWarnings,\n) -> bool {\n    match key {\n",
    );
    for k in keys {
        let (key, f) = (&k.key, &k.field);
        let arm = match k.ty.as_str() {
            "string" => format!("s.{f} = Some(val.clone())"),
            "u32" => format!("s.{f} = parse_u32(warnings, {f:?}, val)"),
            "u64" => format!("s.{f} = parse_u64(warnings, {f:?}, val)"),
            "bool" => format!("s.{f} = parse_bool01(warnings, {f:?}, val)"),
            "list" => format!("s.{f} = val.split_whitespace().map(str::to_string).collect()"),
            "onoff" => format!(
                "s.{f} = match val.as_str() {{\n            \"on\" => Some(OnOff::On),\n            \"off\" => Some(OnOff::Off),\n            other => {{\n                warnings.push(format!(\"spira.{f}: not on/off: {{other:?}}\"));\n                None\n            }}\n        }}"
            ),
            "czar_stage" => format!(
                "s.{f} = match val.as_str() {{\n            \"shadow\" => Some(crate::CzarStage::Shadow),\n            \"act\" => Some(crate::CzarStage::Act),\n            other => {{\n                warnings.push(format!(\"spira.{f}: not shadow/act: {{other:?}}\"));\n                None\n            }}\n        }}"
            ),
            _ => unreachable!("load() refuses unknown TYPE"),
        };
        o.push_str(&format!("        {key:?} => {arm},\n"));
    }
    o.push_str("        _ => return false,\n    }\n    true\n}\n");
    o
}

pub fn history_merge(existing: &str, keys: &[Key]) -> String {
    let mut lines: Vec<String> =
        existing.lines().filter(|l| !l.is_empty()).map(str::to_string).collect();
    let have: BTreeSet<String> = lines.iter().cloned().collect();
    let mut new: Vec<&str> =
        keys.iter().map(|k| k.field.as_str()).filter(|f| !have.contains(*f)).collect();
    new.sort_unstable();
    lines.extend(new.into_iter().map(str::to_string));
    let mut out = lines.join("\n");
    out.push('\n');
    out
}
