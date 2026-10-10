//! The three refusals `units-install` and the root `install` phase 4 both run before
//! touching a unit: path collisions with a sibling instance's config, landref currency, and
//! live aeons under this installation. Shared here so the two callers cannot drift.

use crate::bootstrap::nonempty_env;
use crate::guards;
use crate::systemctl::Systemctl;
use crate::values::HostValues;
use std::path::Path;
use std::process::Command;

pub fn check_collisions(instance: &str, host: &HostValues) -> Result<(), Vec<String>> {
    let Some(conf_file) = nonempty_env("SPIRA_CONF_FILE") else { return Ok(()) };
    let dir = match Path::new(&conf_file).parent() {
        Some(d) => d,
        None => return Ok(()),
    };
    let home = nonempty_env("HOME").unwrap_or_default();
    let mut others = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("conf") || p == Path::new(&conf_file) {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&p) {
                others.push(guards::parse_conf_keys(&p.to_string_lossy(), &text, &home));
            }
        }
    }
    let this: std::collections::BTreeMap<&str, &str> = [("SPIRA_RUN", host.run.as_str()), ("SPIRA_DB", host.db.as_str()), ("SPIRA_PROD", host.prod.as_str()), ("SPIRA_DOLT_DATA", host.dolt_data.as_str()), ("SPIRA_TESTDB_PORT", host.testdb_port.as_str())]
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .collect();
    guards::check_path_collisions(instance, &this, &others)
}

/// Landref currency (`_check_landref_current`, systemd/install.sh): the checkout must be on
/// its landref branch and not behind it, unless it is a release install (no `.git`) or a
/// detached-HEAD tag checkout.
pub fn check_landref(host: &HostValues) -> Result<(), String> {
    let repo = &host.repo;
    if !Path::new(repo).join(".git").exists() {
        return Ok(());
    }
    let git = |args: &[&str]| -> (bool, String) {
        let out = spira_config::bounded::bounded("git").arg("-C").arg(repo).args(args).output();
        match out {
            Ok(o) => (o.status.success(), String::from_utf8_lossy(&o.stdout).trim().to_string()),
            Err(_) => (false, String::new()),
        }
    };
    let (_, cur) = git(&["rev-parse", "--abbrev-ref", "HEAD"]);
    if cur == "HEAD" {
        let (ok, tag) = git(&["describe", "--exact-match", "--tags", "HEAD"]);
        if ok && !tag.is_empty() {
            println!("install: release install from tag {tag} — landref currency not applicable");
            return Ok(());
        }
    }
    let (ok, base) = git(&["symbolic-ref", "-q", "--short", "refs/remotes/origin/HEAD"]);
    let base = if ok && !base.is_empty() {
        base
    } else {
        // batch-job: git history or network operation, as long as the repository is large
        let _ = Command::new("git").arg("-C").arg(repo).args(["remote", "set-head", "origin", "-a"]).status();
        let (ok2, b2) = git(&["symbolic-ref", "-q", "--short", "refs/remotes/origin/HEAD"]);
        if ok2 {
            b2
        } else {
            return Err(format!("cannot resolve landref for {repo}"));
        }
    };
    let base_short = base.rsplit('/').next().unwrap_or(&base);
    if cur != base_short {
        return Err(format!("refusing — checkout is on branch {cur}, not the landref ({base_short}). switch to {base_short} first, or set SPIRA_INSTALL_FORCE=1 to override."));
    }
    let (_, behind) = git(&["rev-list", "--count", &format!("HEAD..{base}")]);
    let behind: u64 = behind.parse().unwrap_or(0);
    if behind > 0 {
        return Err(format!("refusing — checkout is {behind} commit(s) behind {base}. git -C {repo} pull --rebase, or set SPIRA_INSTALL_FORCE=1 to override."));
    }
    Ok(())
}

/// `spira_live_aeons` (lib.sh), reimplemented directly against [`Systemctl`]. Active units
/// only — `list_matching`'s file-known-or-ever-loaded union would also catch a dead or
/// never-started aeon unit left over from a prior run, which is not one this install would
/// disrupt.
pub fn live_aeons(systemctl: &dyn Systemctl, instance: &str) -> Vec<String> {
    systemctl.list_active_matching(&format!("spira-aeon-*-{instance}.service"))
}

/// Run all three, in the same order `units-install` does, `Err` naming every refusal line.
pub fn preflight(instance: &str, host: &HostValues, systemctl: &dyn Systemctl) -> Result<(), Vec<String>> {
    let mut lines = Vec::new();
    if let Err(l) = check_collisions(instance, host) {
        lines.extend(l.into_iter().map(|m| format!("refusing — {m}")));
    }
    if let Err(e) = check_landref(host) {
        lines.push(e);
    }
    let live = live_aeons(systemctl, instance);
    if !live.is_empty() {
        lines.push(format!("refusing — live aeons for instance {instance} would be disrupted: {}", live.join(", ")));
    }
    if lines.is_empty() {
        Ok(())
    } else {
        Err(lines)
    }
}
