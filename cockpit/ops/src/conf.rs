//! conf — apply the harness's one configuration surface to this process's own
//! environment, the same way `layout.sh`/`health.sh`/`rebuild.sh` self-sourced `conf.sh`
//! as their very first action, so every later `std::env::var` call anywhere in these
//! binaries (`layout::env_nonempty`, `health_main::env_nonempty`, `rebuild`'s own
//! `std::env::var("SPIRA_VIEW")`) sees a resolved value, not an unset one.
//!
//! UNTIL WAVE 4.8 ("retire conf re-import seams in Rust") this shelled a bash subprocess
//! per invocation (`set -a; . conf.sh; env -0`) — the right call at the time (`conf.sh`'s
//! own Rust port was scheduled last, after everything that merely calls it), but
//! `spira_config::resolve()` has since landed (sp-eekjm/sp-ubcgo) and is exactly the
//! in-process answer this module used to re-derive by shelling out a second time.
//!
//! Retired outright rather than ported: the bash-sourcing `Conf`/`Conf::load` had zero
//! callers beyond `self_source` itself (grep confirms), so there is no "arbitrary
//! conf.sh" caller left to preserve.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// Keys `resolve()` computes but this process never sets into its own environment —
/// `Command::new()` inherits env into every child this crate spawns, and these are the
/// per-copy-fact / host-policy keys `cockpit-collect`'s `bootstrap_config` names
/// (wave4-decomposition.md row (b)).
const NEVER_EXPORTED: &[&str] = &["SPIRA_HOME", "SPIRA_REPO", "SPIRA_REPO_DERIVED", "SPIRA_REPO_MAP", "SPIRA_FAYTHS", "SPIRA_MAX_AEONS"];

/// `spira_config::resolve()`'s in-process answer, applied to this process's own
/// environment — the one self-sourcing step every one of `health`/`layout`/`rebuild`
/// needs at startup, since each one is invoked directly (by a tmux pane command or by
/// systemd) with only `SPIRA_RELEASE`+`PATH` guaranteed set, exactly as their bash
/// originals were. Inserts a key only when it is not already set (conf.sh's own
/// `${VAR:=default}` rule) and never one of [`NEVER_EXPORTED`].
///
/// Silent on any failure (missing `SPIRA_RELEASE`, an unreadable registry, a containment
/// refusal): a caller that already has its config in the environment — a developer
/// running a binary by hand from an already-`. conf.sh`'d shell — must not be blocked by
/// this, matching `layout.sh`'s own `{ set +u; . conf.sh 2>/dev/null; set -u; } || true`.
pub fn self_source() {
    let Some(release) = std::env::var("SPIRA_RELEASE").ok().filter(|s| !s.is_empty()) else { return };
    let home = PathBuf::from(release).join("spira");
    let env_map: BTreeMap<String, String> = std::env::vars().collect();
    let repo = spira_config::resolve::derive_home_repo(&home, &env_map);
    let resolved = spira_config::resolve::resolve_or_say("cockpit-ops", &home, &repo, &env_map);
    for (k, v) in resolved.values {
        if NEVER_EXPORTED.contains(&k.as_str()) {
            continue;
        }
        if std::env::var_os(&k).is_none() {
            std::env::set_var(k, v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ENV VARS ARE PROCESS-GLOBAL (spira-config's own locate.rs/lib.rs tests guard the
    // same hazard): the one test below that resolves config takes this lock. Per Ryan
    // 2026-10-05 (one source of config), an unresolvable `SPIRA_TOML` is now a flat refusal
    // rather than a fall-through to conf.d's own generated defaults, so this test declares
    // its value through a real `fixture_toml` document instead of pointing `SPIRA_TOML` at a
    // nonexistent file and hand-writing a one-off `conf.d/SPIRA_COCKPIT`.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn self_source_applies_a_registry_default_without_overriding_an_explicit_value_and_never_leaks_the_forbidden_set() {
        let _g = ENV_LOCK.lock().unwrap();
        let saved_release = std::env::var_os("SPIRA_RELEASE");
        let saved_toml = std::env::var_os("SPIRA_TOML");
        let saved_right_pct = std::env::var_os("COCKPIT_RIGHT_PCT");
        let saved_cockpit = std::env::var_os("SPIRA_COCKPIT");
        let saved_max_aeons = std::env::var_os("SPIRA_MAX_AEONS");
        std::env::remove_var("SPIRA_COCKPIT");
        std::env::remove_var("SPIRA_MAX_AEONS");

        let dir = testkit::TempDir::new("cockpit-ops-conf");
        // `self_source` derives `home` as `$SPIRA_RELEASE/spira` — point SPIRA_RELEASE at
        // this tree's release root so `home` lands on the REAL spira/ dir (the registry
        // `resolve_for_process` validates registered keys against), not a fabricated one.
        let release_root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        std::env::set_var("SPIRA_RELEASE", release_root);
        let toml = spira_config::process::fixture_toml(dir.path(), &[("SPIRA_COCKPIT", "/resolved/cockpit")]);
        std::env::set_var("SPIRA_TOML", &toml);
        // conf.sh's own convention is ${VAR:-default}; a caller override must survive.
        std::env::set_var("COCKPIT_RIGHT_PCT", "50");

        self_source();

        let got_cockpit = std::env::var("SPIRA_COCKPIT").ok();
        let got_right_pct = std::env::var("COCKPIT_RIGHT_PCT").ok();
        let got_max_aeons = std::env::var_os("SPIRA_MAX_AEONS");

        match saved_release {
            Some(v) => std::env::set_var("SPIRA_RELEASE", v),
            None => std::env::remove_var("SPIRA_RELEASE"),
        }
        match saved_toml {
            Some(v) => std::env::set_var("SPIRA_TOML", v),
            None => std::env::remove_var("SPIRA_TOML"),
        }
        match saved_right_pct {
            Some(v) => std::env::set_var("COCKPIT_RIGHT_PCT", v),
            None => std::env::remove_var("COCKPIT_RIGHT_PCT"),
        }
        match saved_cockpit {
            Some(v) => std::env::set_var("SPIRA_COCKPIT", v),
            None => std::env::remove_var("SPIRA_COCKPIT"),
        }
        match saved_max_aeons {
            Some(v) => std::env::set_var("SPIRA_MAX_AEONS", v),
            None => std::env::remove_var("SPIRA_MAX_AEONS"),
        }
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(got_cockpit, Some("/resolved/cockpit".to_string()), "a registry default must reach the real environment");
        assert_eq!(got_right_pct, Some("50".to_string()), "an explicit env override must survive self_source");
        assert_eq!(got_max_aeons, None, "SPIRA_MAX_AEONS must never leak into this process's own environment");
    }

    #[test]
    fn self_source_is_silent_with_no_spira_release() {
        let _g = ENV_LOCK.lock().unwrap();
        let saved = std::env::var_os("SPIRA_RELEASE");
        std::env::remove_var("SPIRA_RELEASE");
        self_source(); // must not panic
        if let Some(v) = saved {
            std::env::set_var("SPIRA_RELEASE", v);
        }
    }
}
