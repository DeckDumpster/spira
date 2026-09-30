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

use std::env;
use std::path::{Path, PathBuf};

/// `$SPIRA_RUN`, defaulting the way conf.sh and every Rust caller in this workspace already
/// do (landing-pass, czar-pass, reconciler): `/tmp/spira` when unset. This binary does not
/// source conf.sh itself — every caller that starts it (a systemd unit, the sentinel, an
/// operator shell) has already sourced it, so the value conf.sh resolved is already in the
/// environment (conf.sh exports `SPIRA_RUN`; see its export list).
pub fn spira_run() -> PathBuf {
    env::var_os("SPIRA_RUN").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp/spira"))
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
pub fn halt_stamp() -> PathBuf {
    spira_run().join("world.halted")
}

/// The drain stamp path: `$SPIRA_RUN/world.draining`.
pub fn drain_stamp() -> PathBuf {
    spira_run().join("world.draining")
}

/// `$SPIRA_INSTANCE`-qualified suffix — `""` when unset, else `-<instance>` — the same
/// `_inst_sfx` every per-instance timer/unit name in world.sh builds.
pub fn instance_suffix() -> String {
    match env::var("SPIRA_INSTANCE") {
        Ok(s) if !s.is_empty() => format!("-{s}"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spira_run_defaults_to_tmp_spira() {
        let saved = env::var("SPIRA_RUN").ok();
        env::remove_var("SPIRA_RUN");
        assert_eq!(spira_run(), PathBuf::from("/tmp/spira"));
        if let Some(v) = saved {
            env::set_var("SPIRA_RUN", v);
        }
    }

    #[test]
    fn instance_suffix_empty_when_unset() {
        let saved = env::var("SPIRA_INSTANCE").ok();
        env::remove_var("SPIRA_INSTANCE");
        assert_eq!(instance_suffix(), "");
        if let Some(v) = saved {
            env::set_var("SPIRA_INSTANCE", v);
        } else {
            env::remove_var("SPIRA_INSTANCE");
        }
    }
}
