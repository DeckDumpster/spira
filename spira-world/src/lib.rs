//! spira-world — shared primitives for `world.sh`, `aeons.sh` and `slay.sh` (DESIGN.md).
//!
//! Three tools, one crate, because `world.sh stop` slays aeons the same way `slay.sh` does,
//! `world.sh start` reads suspensions the same way `ctrl.sh` (now `spira-ctrl`) does, and
//! `aeons.sh status` counts live aeons the same way `world.sh status` does — in bash these
//! were three separate re-derivations (the exact drift lib.sh's own comments warn about);
//! in Rust they are one function each, called from three binaries.

pub mod fleet;
pub mod proc;
pub mod round;
pub mod seam;
pub mod sysctl;

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};

/// This process's own `SPIRA_HOME` — [`locate_home`] against [`env::current_exe`], for a
/// caller (`spira_run`/`instance`) that has no executable path of its own to hand in. An
/// unlocatable `current_exe` (never observed outside a test harness) falls back to an empty
/// path, which [`spira_config::resolve::resolve_run_dir`] still resolves correctly: the
/// derived default depends on `$HOME`/XDG, not on `home` itself (see that function's doc).
fn resolve_home() -> PathBuf {
    let exe = env::current_exe().unwrap_or_else(|_| PathBuf::from("spira-world"));
    locate_home(&exe).unwrap_or_default()
}

/// `spira.run`, resolved in-process through `spira_config` (`SPIRA_RUN` in the environment
/// still wins outright — every systemd unit sets it explicitly, so a production unit's
/// behaviour is unchanged — else the resolved config document, else the derived XDG default).
/// law-a-binary-resolves-the-config-it-reads (sp-ivfu3): this used to default to the
/// literal `/tmp/spira` whenever `SPIRA_RUN` was unset, which is exactly what a bare
/// operator shell (no systemd unit to set it) got — `world status` read and wrote the
/// wrong run directory while reporting on the real one. REFUSES, named, rather than
/// guessing, when `spira_config` itself cannot resolve.
pub fn spira_run() -> Result<PathBuf, String> {
    let env_map: BTreeMap<String, String> = env::vars().collect();
    spira_config::resolve::resolve_run_dir(&env_map, &resolve_home())
}

/// `spira.instance`, resolved the same way [`spira_run`] resolves `spira.run` — never a
/// bare `$SPIRA_INSTANCE` read with an empty-string default (sp-ivfu3's widening: that
/// literal default is what made a bare shell's `world status` name the unqualified
/// `spira-sentinel.timer` instead of the installed `spira-sentinel-prod.timer`).
pub fn instance() -> Result<String, String> {
    let env_map: BTreeMap<String, String> = env::vars().collect();
    spira_config::resolve::resolve_instance(&env_map, &resolve_home())
}

/// `$SPIRA_HOME` — the checkout this instance runs from. Resolved the way `sentinel::locate_home`
/// does (DESIGN.md §2.7): the environment if set, else the first ancestor of this executable
/// that holds `lib.sh`, across both the release layout (`bin/<exe>` next to `spira/lib.sh`)
/// and a cargo target layout. Needed only by the seams that call into lib.sh (slay's work
/// destruction, aeons' lane report) — `world.sh` and `ctrl.sh` proper never need it.
pub fn locate_home(exe: &Path) -> Option<PathBuf> {
    if let Ok(h) = env::var("SPIRA_HOME") {
        if !h.is_empty() {
            return Some(PathBuf::from(h));
        }
    }
    let dir = exe.parent()?;
    [dir.join("../spira"), dir.join("../../spira"), dir.join("../../../spira")]
        .into_iter()
        .find(|c| c.join("lib.sh").is_file())
        .map(|c| c.canonicalize().unwrap_or(c))
}

/// `$SPIRA_PROD` if set, else `$SPIRA_HOME` — the same "both homes" fallback `live_aeons`
/// and `live_workers` use in world.sh, because in split-checkout mode every aeon executes
/// `$SPIRA_PROD/aeon.sh` while `$SPIRA_HOME` is the development checkout.
pub fn spira_prod_or_home(home: &Path) -> PathBuf {
    env::var_os("SPIRA_PROD").map(PathBuf::from).unwrap_or_else(|| home.to_path_buf())
}

/// The world-halt stamp path: `$SPIRA_RUN/world.halted`.
pub fn halt_stamp() -> Result<PathBuf, String> {
    spira_run().map(|r| r.join("world.halted"))
}

/// The argv paths `live_workers` matches against — every one of them, not just gate.sh:
/// `$SPIRA_HOME/gate.sh`, `${SPIRA_PROD:-$SPIRA_HOME}/gate.sh`, `landing-pass` beside
/// whichever of those two is in force (`dirname(prod)/bin/landing-pass`, matching
/// `landing.sh`'s retirement into that binary), and whatever `landing-pass` resolves to
/// on the launcher's own PATH right now. world.sh's own comment: "both homes, for the
/// same reason live_aeons matches both" — this is that set, gathered once so `status` and
/// any other caller can't each grow their own partial copy.
pub fn live_worker_paths(home: &Path) -> Vec<String> {
    let prod = spira_prod_or_home(home);
    let mut v = vec![
        home.join("gate.sh").to_string_lossy().into_owned(),
        prod.join("gate.sh").to_string_lossy().into_owned(),
    ];
    if let Some(dir) = prod.parent() {
        v.push(dir.join("bin").join("landing-pass").to_string_lossy().into_owned());
    }
    if let Ok(p) = env::var("PATH") {
        if let Some(found) = p.split(':').map(Path::new).map(|d| d.join("landing-pass")).find(|c| c.is_file()) {
            v.push(found.to_string_lossy().into_owned());
        }
    }
    v
}

/// The drain stamp path: `$SPIRA_RUN/world.draining`.
pub fn drain_stamp() -> Result<PathBuf, String> {
    spira_run().map(|r| r.join("world.draining"))
}

/// `$SPIRA_INSTANCE`-qualified suffix for a UNIT NAME — `-<instance>`, always, including
/// `-prod` (NOT [`spira_config::resolve`]'s own `inst_sfx`, which special-cases `"prod"`
/// to empty for a DATA DIRECTORY's name — a different convention for a different thing;
/// see `spira_config::unit`'s own doc on the scar (sp-smbq0) of the two being confused).
/// The real installed units on this harness are always instance-qualified
/// (`spira-sentinel-prod.timer`), which is exactly what `sysctl::subject_of`/
/// `is_essential_timer` already assume when stripping or matching a suffix.
pub fn instance_suffix() -> Result<String, String> {
    instance().map(|i| format!("-{i}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes this crate's own env-mutating tests against each other — `cargo test`
    /// runs them on separate threads by default, and `std::env::set_var` is process-global
    /// (the same hazard `spira_config`'s own `ENV_LOCK` exists for, `pub(crate)` there so
    /// this crate needs its own).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// sp-ivfu3: the old contract was "defaults to the literal /tmp/spira when SPIRA_RUN is
    /// unset" — exactly the bug (a bare shell silently read/wrote the wrong run directory).
    /// The new contract: resolved in-process via spira_config, which still produces a real
    /// path with NO env/toml override present (the derived XDG default), never /tmp/spira.
    #[test]
    fn spira_run_resolves_via_config_never_the_tmp_spira_literal() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let names = ["SPIRA_RUN", "SPIRA_HOME", "SPIRA_TOML", "SPIRA_CONF", "SPIRA_REPO", "XDG_CONFIG_HOME", "HOME"];
        let saved: Vec<(&str, Option<String>)> = names.iter().map(|n| (*n, env::var(n).ok())).collect();
        for n in names {
            env::remove_var(n);
        }
        let home = testkit::TempDir::new("spira-world-run-default");
        env::set_var("HOME", home.to_str().unwrap());
        env::set_var("XDG_CONFIG_HOME", home.join("no-such-xdg").to_str().unwrap());

        let got = spira_run();

        for (n, v) in saved {
            match v {
                Some(v) => env::set_var(n, v),
                None => env::remove_var(n),
            }
        }
        let got = got.unwrap();
        assert_ne!(got, PathBuf::from("/tmp/spira"));
        assert_eq!(got, home.join(".local/share/spira/run"));
    }

    #[test]
    fn instance_suffix_defaults_to_prod_suffixed() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let names = ["SPIRA_INSTANCE", "SPIRA_HOME", "SPIRA_TOML", "SPIRA_CONF", "SPIRA_REPO", "XDG_CONFIG_HOME", "HOME"];
        let saved: Vec<(&str, Option<String>)> = names.iter().map(|n| (*n, env::var(n).ok())).collect();
        for n in names {
            env::remove_var(n);
        }
        let home = testkit::TempDir::new("spira-world-instance-default");
        env::set_var("HOME", home.to_str().unwrap());
        env::set_var("XDG_CONFIG_HOME", home.join("no-such-xdg").to_str().unwrap());

        let got = instance_suffix();

        for (n, v) in saved {
            match v {
                Some(v) => env::set_var(n, v),
                None => env::remove_var(n),
            }
        }
        assert_eq!(got.unwrap(), "-prod", "the real installed units are always instance-qualified");
    }
}
