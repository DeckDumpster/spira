//! `conf-key-registry` — every key `spira/conf.sh` accepts survives spira-config's
//! `convert` → `export --sh` unchanged. Moved from spira-config's
//! `every_conf_sh_key_survives_convert_and_export` (sp-9nljd). Contract: DESIGN.md.

use std::process::Command;
use std::collections::BTreeMap;

use spira_config::convert::convert;
use spira_config::export_sh;

use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct ConfKeyRegistry;

const NAME: &str = "conf-key-registry";
pub const CONF_SH: &str = "spira/conf.sh";
const BLOCK_OPEN: &str = "SPIRA_CONF_KEYS=\"\n";

/// conf.sh's `SPIRA_CONF_KEYS` allowlist, first occurrence of each key, in order. `None`
/// when the block is missing or unterminated.
pub fn conf_keys(conf_sh: &str) -> Option<Vec<String>> {
    let start = conf_sh.find(BLOCK_OPEN)?;
    let after = &conf_sh[start + BLOCK_OPEN.len()..];
    let end = after.find("\"\n")?;
    let mut seen = std::collections::BTreeSet::new();
    Some(after[..end].split_whitespace().map(str::to_string).filter(|k| seen.insert(k.clone())).collect())
}

/// When conf.sh no longer carries the literal `SPIRA_CONF_KEYS` block in its own text
/// (sp-g3uwp: it is generated from `spira/conf.d/` — one file per key — rather than
/// hand-maintained inline), regenerate `conf-gen.sh`'s output against the tree under lint
/// and read the keys from there instead. This keeps checking the keys actually in force
/// rather than refusing the moment the literal block this rule used to grep is gone by
/// design; `conf.sh` and this fallback both resolve through the identical generator, so
/// they cannot disagree about which keys exist.
fn conf_keys_via_registry(root: &std::path::Path) -> Option<Vec<String>> {
    let conf_gen = root.join("spira/conf-gen.sh");
    let conf_d = root.join("spira/conf.d");
    if !conf_gen.is_file() || !conf_d.is_dir() {
        return None;
    }
    // batch-job: this runs whatever its caller names, as long as that takes
    let status = Command::new("bash").arg(&conf_gen).current_dir(root).status().ok()?;
    if !status.success() {
        return None;
    }
    let generated = std::fs::read_to_string(root.join("spira/conf.d.keys.generated.sh")).ok()?;
    conf_keys(&generated)
}

/// Keys the schema types as something other than a bare string: (value written into the
/// synthetic spira.conf, value `export --sh` must give back), read off the registry's TYPE.
/// Any other key is a string field, and the key's own name is written and must come back
/// byte-for-byte.
pub fn typed_cases() -> BTreeMap<&'static str, (&'static str, &'static str)> {
    spira_config::SPIRA_KEYS
        .iter()
        .filter_map(|k| {
            let case = match k.ty {
                "u32" => ("7", "7"),
                "u64" => ("700", "700"),
                "bool" => ("1", "true"),
                "onoff" => ("on", "on"),
                "czar_stage" => ("shadow", "shadow"),
                "list" => ("alpha beta", "alpha beta"),
                _ => return None,
            };
            Some((k.key, case))
        })
        .collect()
}

/// conf.sh's own renaming of an `export --sh` line (`spira_toml_read`): the `SPIRA_` prefix
/// is restored unless the bare key is itself a member of the allowlist.
fn exported_map(sh: &str, keys: &[String]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in sh.lines() {
        let Some((key, val)) = line.split_once('=') else { continue };
        let prefixed = format!("SPIRA_{key}");
        let key = if keys.iter().any(|k| k == &prefixed) { prefixed } else { key.to_string() };
        let val = val.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')).unwrap_or(val);
        out.insert(key, val.replace("'\\''", "'"));
    }
    out
}

/// Each problem with carrying `keys` through convert and export, one message per key.
pub fn round_trip(keys: &[String]) -> Vec<String> {
    let typed = typed_cases();
    let mut conf_text = String::new();
    let mut expected = BTreeMap::new();
    for key in keys {
        let (conf_val, exported_val) = typed.get(key.as_str()).copied().unwrap_or((key.as_str(), key.as_str()));
        conf_text.push_str(&format!("{key} = {conf_val}\n"));
        expected.insert(key.clone(), exported_val.to_string());
    }
    let (doc, warnings) = match convert(&conf_text, "/opt/fixture-home", "", &[]) {
        Ok(v) => v,
        Err(errors) => return errors.into_iter().map(|e| format!("convert refused: {e}")).collect(),
    };
    let mut out: Vec<String> = warnings.0.iter().map(|w| format!("convert warned (the key would be dropped): {w}")).collect();
    let exported = exported_map(&export_sh(&doc), keys);
    for (key, want) in &expected {
        if spira_config::is_secret_shaped(key) {
            continue;
        }
        match exported.get(key) {
            None => out.push(format!("{key}: accepted by conf.sh, dropped by spira-config convert/export")),
            Some(got) if got != want => out.push(format!("{key}: exported as {got:?}, want {want:?}")),
            Some(_) => {}
        }
    }
    out
}

impl Rule for ConfKeyRegistry {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path == CONF_SH
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let text = tree.text_of(CONF_SH).ok_or(LintError::EmptyScope)?;
        let refuse = |reason: &str| LintError::BadAllow { file: CONF_SH.into(), line: 0, reason: reason.into() };
        let keys = conf_keys(&text)
            .or_else(|| conf_keys_via_registry(&tree.root))
            .ok_or_else(|| refuse("SPIRA_CONF_KEYS block not found or unterminated, and spira/conf.d/ + conf-gen.sh could not regenerate it"))?;
        // A parser reading the wrong block reports clean over a handful of keys.
        if keys.len() < 200 {
            return Err(refuse(&format!("SPIRA_CONF_KEYS parsed to {} keys, expected over 200 — reading the wrong thing", keys.len())));
        }
        Ok(round_trip(&keys)
            .into_iter()
            .map(|message| Finding { rule: NAME, path: CONF_SH.into(), line: None, message })
            .collect())
    }

    fn hint(&self) -> &'static str {
        "A key conf.sh accepts must exist in spira-config's schema (spira-config/src), or it is \
dropped silently on the way to the typed config. Add the field there in the same change."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(ks: &[&str]) -> Vec<String> {
        ks.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_block_is_parsed_once_per_key() {
        let t = "x=1\nSPIRA_CONF_KEYS=\"\n  SPIRA_A SPIRA_B\n  SPIRA_A\n\"\nSPIRA_C=1\n";
        assert_eq!(conf_keys(t).unwrap(), keys(&["SPIRA_A", "SPIRA_B"]));
        assert!(conf_keys("SPIRA_CONF_KEYS=\"a b\"\n").is_none());
    }

    #[test]
    fn known_keys_survive_and_a_planted_unknown_key_is_named() {
        assert!(round_trip(&keys(&["SPIRA_MAX_AEONS", "SPIRA_CERTIFY_PAR"])).is_empty());
        let got = round_trip(&keys(&["SPIRA_MAX_AEONS", "SPIRA_TOTALLY_MADE_UP_TEST_KEY"]));
        assert!(!got.is_empty());
        assert!(got.iter().any(|m| m.contains("SPIRA_TOTALLY_MADE_UP_TEST_KEY")), "{got:?}");
    }

    #[test]
    fn a_secret_shaped_key_is_deliberately_not_exported() {
        assert!(round_trip(&keys(&["SPIRA_BROKER_GH_TOKEN"])).is_empty());
    }

    #[test]
    fn falls_back_to_the_registry_when_conf_sh_has_no_literal_block() {
        let t = crate::testutil::TempDir::new("ckr-registry");
        // conf.sh carries no literal SPIRA_CONF_KEYS block at all (sp-g3uwp) — just the
        // call that regenerates and sources it at use time.
        t.write(CONF_SH, "_spira_conf_gen_ensure keys || return 1\n");
        t.write(
            "spira/conf.d/SPIRA_MAX_AEONS",
            "TYPE=u32\nGROUP=test\nDOC=test\nDEFAULT<<'EOF'\n    : \"${SPIRA_MAX_AEONS:=7}\"\nEOF\n",
        );
        t.write(
            "spira/conf.d/SPIRA_CERTIFY_PAR",
            "TYPE=u32\nGROUP=test\nDOC=test\nDEFAULT<<'EOF'\n    : \"${SPIRA_CERTIFY_PAR:=7}\"\nEOF\n",
        );
        // A minimal stand-in generator — this test exercises the fallback plumbing itself
        // (that it shells out to conf-gen.sh and reads the result back), not the real
        // spira/conf-gen.sh's own correctness, which test-conf-registry-parity.sh covers.
        t.write(
            "spira/conf-gen.sh",
            "#!/usr/bin/env bash\nset -euo pipefail\nd=\"$(cd \"$(dirname \"${BASH_SOURCE[0]}\")\" && pwd)\"\n{ printf 'SPIRA_CONF_KEYS=\"\\n'; ls \"$d/conf.d\"; printf '\"\\n'; } > \"$d/conf.d.keys.generated.sh\"\n",
        );
        // conf_keys() directly, not the full Rule::check (which also floors at 200 keys —
        // a guard against a parser silently reading the wrong block, not part of what the
        // registry fallback itself is responsible for; the real registry clears it easily).
        let got = conf_keys_via_registry(t.path()).expect("fallback should resolve the registry, not refuse");
        assert_eq!(got, keys(&["SPIRA_CERTIFY_PAR", "SPIRA_MAX_AEONS"]));
    }

    #[test]
    fn a_short_block_is_a_refusal_not_a_pass() {
        let t = crate::testutil::TempDir::new("ckr");
        t.write(CONF_SH, "SPIRA_CONF_KEYS=\"\nSPIRA_MAX_AEONS\n\"\n");
        let tree = Tree::from_paths(t.path(), [CONF_SH], std::iter::empty::<&str>());
        assert!(matches!(ConfKeyRegistry.check(&tree), Err(LintError::BadAllow { .. })));
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(ConfKeyRegistry),
    ]
}
