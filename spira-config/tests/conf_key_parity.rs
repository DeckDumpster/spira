//! sp-9nljd's regression test: every key `spira/conf.sh`'s own `SPIRA_CONF_KEYS` allowlist
//! accepts must survive `spira.conf` → [`convert`] → [`export_sh`] unchanged — not warned
//! about and dropped, which is what let ~200 of conf.sh's ~280 keys (SPIRA_AGENT,
//! SPIRA_OPERATED, SPIRA_RELEASES, ...) vanish silently through this converter.
//!
//! The key list is read from the real `spira/conf.sh` rather than copied here, so this test
//! fails the moment conf.sh accepts a key this schema does not — the same drift that caused
//! the defect, caught before it ships instead of after.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use spira_config::convert::convert;
use spira_config::export_sh;

fn conf_sh_keys() -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/conf.sh");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let start = text
        .find("SPIRA_CONF_KEYS=\"\n")
        .expect("conf.sh: SPIRA_CONF_KEYS block not found");
    let after = &text[start + "SPIRA_CONF_KEYS=\"\n".len()..];
    let end = after.find("\"\n").expect("conf.sh: SPIRA_CONF_KEYS block unterminated");
    let mut seen = std::collections::BTreeSet::new();
    after[..end]
        .split_whitespace()
        .map(str::to_string)
        .filter(|k| seen.insert(k.clone()))
        .collect()
}

/// Every key `conf.sh` accepts that this schema types as something other than a bare string
/// — the value to write into the synthetic `spira.conf`, and the value it must come back as
/// after `export --sh`'s uppercased `KEY=value` line. A key absent from this map is treated
/// as a plain string field: the same value must survive byte-for-byte.
fn typed_cases() -> BTreeMap<&'static str, (&'static str, &'static str)> {
    let mut m = BTreeMap::new();
    for k in [
        "SPIRA_NOTIFY_AGE",
        "SPIRA_VERDICT_TTL",
        "SPIRA_MAX_AEONS",
        "SPIRA_MAX_LIVE_AEONS",
        "SPIRA_BATCH_MAXPAR",
        "SPIRA_CERTIFY_PAR",
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
        "SPIRA_CONCIERGE_INBOX_DEDUP",
        "SPIRA_CONCIERGE_INBOX_STALL",
        "SPIRA_CONCIERGE_INBOX_BACKOFF",
    ] {
        m.insert(k, ("700", "700"));
    }
    for k in [
        "SPIRA_CERT_IDLE_SKIP",
        "SPIRA_QUEUE_LOCAL_GATE",
        "SPIRA_QUEUE_BATCH_IDLE_CUT",
    ] {
        m.insert(k, ("1", "true"));
    }
    m.insert("SPIRA_CERTIFY_SUITES", ("on", "on"));
    m.insert("SPIRA_CZAR_STAGE_DEADLOCK", ("shadow", "shadow"));
    m.insert("SPIRA_FAYTHS", ("alpha beta", "alpha beta"));
    m
}

/// Applies `conf.sh`'s own renaming (`spira_toml_read`): `export --sh` names a `[spira]`
/// field as-is, uppercased, and conf.sh restores the `SPIRA_` prefix unless the bare key is
/// itself a member of `SPIRA_CONF_KEYS` (a genuine `COCKPIT_*` key, already spelled that way
/// in spira.conf) — membership, not a `COCKPIT_*` wildcard, because a wildcard mis-restores
/// SPIRA_COCKPIT_TRACE_LINES to the bare COCKPIT_TRACE_LINES.
fn exported_map(sh: &str, conf_keys: &[String]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in sh.lines() {
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let prefixed = format!("SPIRA_{key}");
        let key = if conf_keys.iter().any(|k| k == &prefixed) {
            prefixed
        } else {
            key.to_string()
        };
        let val = val.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')).unwrap_or(val);
        out.insert(key, val.replace("'\\''", "'"));
    }
    out
}

#[test]
fn every_conf_sh_key_survives_convert_and_export() {
    let keys = conf_sh_keys();
    assert!(keys.len() > 200, "sanity: expected conf.sh's allowlist to run past 200 keys, got {}", keys.len());

    let typed = typed_cases();
    let mut conf_text = String::new();
    let mut expected: BTreeMap<String, String> = BTreeMap::new();
    for key in &keys {
        let (conf_val, exported_val) = typed
            .get(key.as_str())
            .copied()
            .unwrap_or((key.as_str(), key.as_str()));
        conf_text.push_str(&format!("{key} = {conf_val}\n"));
        expected.insert(key.clone(), exported_val.to_string());
    }

    let (doc, warnings) =
        convert(&conf_text, "/opt/fixture-home", "", &[]).expect("every conf.sh key must convert");
    assert!(
        warnings.0.is_empty(),
        "no accepted key should warn: {:?}",
        warnings.0
    );

    let sh = export_sh(&doc);
    let exported = exported_map(&sh, &keys);

    let mut missing = Vec::new();
    let mut wrong = Vec::new();
    for (key, want) in &expected {
        match exported.get(key) {
            None => missing.push(key.clone()),
            Some(got) if got != want => wrong.push(format!("{key}: got {got:?}, want {want:?}")),
            Some(_) => {}
        }
    }
    assert!(missing.is_empty(), "keys dropped by convert/export: {missing:?}");
    assert!(wrong.is_empty(), "keys that changed value: {wrong:?}");
}

// POSITIVE CONTROL: a key conf.sh does not accept (planted, never a real key) must still be
// refused, not silently dropped — proves the check above could have caught a real omission
// instead of passing vacuously (law-absence-needs-a-positive-control).
#[test]
fn a_planted_unmapped_key_is_refused_not_dropped() {
    let conf_text = "SPIRA_TOTALLY_MADE_UP_TEST_KEY = 1\n";
    let errors = convert(conf_text, "/opt/fixture-home", "", &[])
        .expect_err("a key neither conf.sh nor the schema accepts must refuse the whole convert");
    assert!(
        errors.iter().any(|e| e.contains("SPIRA_TOTALLY_MADE_UP_TEST_KEY")),
        "expected an error naming the planted key, got {errors:?}"
    );
}
