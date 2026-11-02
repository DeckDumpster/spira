#![allow(clippy::type_complexity)]
//! Test support shared by dwarf's unit tests (the whole-run tests that lived here went with
//! sp-ao9dn2: each drove real git or other programs).

use std::collections::BTreeMap;

/// Every key spira-config's own "every key declared" test fixture
/// (`spira/testlib/complete.toml`, the same document
/// `spira_config::process::fixture_toml` writes from) declares, renamed `SPIRA_*`/
/// `COCKPIT_*` — the same rename `spira_config::resolve`'s own (private) `toml_key_map`
/// applies when it builds a toml-backed `Resolved`, replicated here (not called) because
/// it is not `pub`. No file I/O, no `$SPIRA_TOML`/`$SPIRA_HOME` env, and nowhere near
/// `spira_config::process::cfg`'s own cached resolution — this builds `Conf`'s backing map
/// directly, the same shape `merge_resolved_config` would produce in production.
///
/// `pub(crate)`: `escape::tests::conf_at` builds its own `Conf` the same way and needs the
/// same base — a hand-built `Conf` whose registered keys are simply absent now under-
/// resolves them to 0/"" (`Conf::i`/`Conf::s` carry no Rust-side default any more, per Ryan
/// 2026-10-05: one source of config), which is a silent behaviour change for whichever one
/// of this file's many fixtures does not also move onto this same base.
pub(crate) fn complete_vars() -> BTreeMap<String, String> {
    let doc = spira_config::validate(&spira_config::process::complete_fixture_text()).expect("the complete fixture validates");
    spira_config::spira_string_map(&doc)
        .into_iter()
        .map(|(k, v)| if k.starts_with("COCKPIT_") { (k, v) } else { (format!("SPIRA_{k}"), v) })
        .chain(std::iter::once((
            "SPIRA_TOML".to_string(),
            concat!(env!("CARGO_MANIFEST_DIR"), "/../spira/testlib/complete.toml").to_string(),
        )))
        .collect()
}
