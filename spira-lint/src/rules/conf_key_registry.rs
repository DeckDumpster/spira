//! `conf-key-registry` — every key `spira/conf.sh` accepts survives spira-config's
//! `convert` → `export --sh` unchanged. Moved from spira-config's
//! `every_conf_sh_key_survives_convert_and_export` (sp-9nljd). Contract: DESIGN.md.

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

/// Keys the schema types as something other than a bare string: (value written into the
/// synthetic spira.conf, value `export --sh` must give back). Any other key is a string
/// field, and the key's own name is written and must come back byte-for-byte.
pub fn typed_cases() -> BTreeMap<&'static str, (&'static str, &'static str)> {
    let mut m = BTreeMap::new();
    for k in [
        "SPIRA_NOTIFY_AGE",
        "SPIRA_VERDICT_TTL",
        "SPIRA_MAX_AEONS",
        "SPIRA_MAX_LIVE_AEONS",
        "SPIRA_STACK_MAX_DEPTH",
        "SPIRA_BATCH_MAXPAR",
        "SPIRA_CERTIFY_PAR",
        "SPIRA_COMPILE_PAR",
        "SPIRA_TEST_PAR",
        "SPIRA_QUEUE_BATCH_MAX",
        "SPIRA_QUEUE_THROTTLE_RELEASE_AT",
        "COCKPIT_BOTTOM_PCT",
        "COCKPIT_RIGHT_PCT",
        "SPIRA_BATCH_MEM_PER_SUITE_MIB",
        "SPIRA_LANES_MAX_LIVE",
        "SPIRA_THRASH_MINUTES",
        "SPIRA_HOOK_LINES",
    ] {
        m.insert(k, ("7", "7"));
    }
    for k in [
        "SPIRA_QUEUE_BATCH_WAIT",
        "SPIRA_QUEUE_CI_MAXSEC",
        "SPIRA_SUITES_BUDGET",
        "SPIRA_LOOM_BUDGET_MS",
        "SPIRA_MAIL_SETTLE",
        "SPIRA_QUEUE_TRANSITION_POLLSEC",
        "SPIRA_QUEUE_TRANSITION_MAXSEC",
        "SPIRA_LOCAL_BACKLOG_COUNT",
        "SPIRA_LOCAL_BACKLOG_AGE",
        "SPIRA_SUMMON_LOCK_WAIT",
        "SPIRA_SUMMON_JITTER",
        "SPIRA_CONCIERGE_INBOX_DEDUP",
        "SPIRA_CONCIERGE_INBOX_STALL",
        "SPIRA_CONCIERGE_INBOX_BACKOFF",
    ] {
        m.insert(k, ("700", "700"));
    }
    for k in ["SPIRA_CERT_IDLE_SKIP", "SPIRA_QUEUE_LOCAL_GATE", "SPIRA_QUEUE_BATCH_IDLE_CUT", "SPIRA_LIFECYCLE_ENFORCE", "SPIRA_MAIL_MUTE"] {
        m.insert(k, ("1", "true"));
    }
    m.insert("SPIRA_CERTIFY_SUITES", ("on", "on"));
    m.insert("SPIRA_CZAR_STAGE_DEADLOCK", ("shadow", "shadow"));
    m.insert("SPIRA_FAYTHS", ("alpha beta", "alpha beta"));
    m
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
        let keys = conf_keys(&text).ok_or_else(|| refuse("SPIRA_CONF_KEYS block not found or unterminated"))?;
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
    fn a_short_block_is_a_refusal_not_a_pass() {
        let t = crate::testutil::TempDir::new("ckr");
        t.write(CONF_SH, "SPIRA_CONF_KEYS=\"\nSPIRA_MAX_AEONS\n\"\n");
        let tree = Tree::from_paths(t.path(), [CONF_SH], std::iter::empty::<&str>());
        assert!(matches!(ConfKeyRegistry.check(&tree), Err(LintError::BadAllow { .. })));
    }
}
