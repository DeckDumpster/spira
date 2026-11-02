//! `release intake`: wire systemd's `OnFailure=` alert templates into Spira's incident intake
//! by writing a drop-in beside each one (DESIGN.md "intake"). Replaces
//! `spira/install-intake.sh`. Idempotent and cheap — `spira-ops.service` runs `install` as an
//! `ExecStartPre` on every pass (the cockpit-ensure lesson: a repair loop hung off a session
//! cannot repair the case where the session died).
//!
//! **No default glob.** `SPIRA_ALERT_GLOB` names the caller's own alert units; unset, every
//! subcommand refuses nothing and does nothing, printing why, and exits `0` — a default would
//! be one box's inventory, and the wrong one would silently wire nothing while reporting
//! success. Widening the glob is a deliberate, one-at-a-time act.

use crate::systemctl::Systemctl;
use std::path::{Path, PathBuf};

/// The drop-in file this writes beside every matched template.
pub const DROPIN: &str = "50-spira-intake.conf";

/// Finds alert templates. A trait so tests never run real `find`.
pub trait Templates {
    /// Every path directly under `dir` (no recursion) whose name matches the `find -name`
    /// glob `pattern`, sorted.
    fn find(&self, dir: &Path, pattern: &str) -> Result<Vec<PathBuf>, String>;
}

pub struct RealTemplates;

impl Templates for RealTemplates {
    fn find(&self, dir: &Path, pattern: &str) -> Result<Vec<PathBuf>, String> {
        let out = spira_config::bounded::bounded("find").arg(dir).arg("-maxdepth").arg("1").arg("-name").arg(pattern).output().map_err(|e| format!("cannot run find: {e}"))?;
        if !out.status.success() {
            // find on a missing/unreadable dir: treat as "none", matching the script's own
            // `2>/dev/null` (a missing UNITDIR is refused earlier, by the `install`/`status`
            // callers themselves, not by this lookup).
            return Ok(Vec::new());
        }
        let mut paths: Vec<PathBuf> = String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect();
        paths.sort();
        Ok(paths)
    }
}

pub struct Opts {
    pub unit_dir: PathBuf,
    /// `SPIRA_ALERT_GLOB`. `None` means unset — every subcommand is then a deliberate no-op.
    pub pattern: Option<String>,
    /// The release's own `incident.sh` — `<release>/spira/incident.sh`.
    pub incident: PathBuf,
    /// `SPIRA_SYSTEMCTL_RELOAD`, default `true`.
    pub reload: bool,
}

/// Why every subcommand does nothing: printed once, on stderr, then the caller exits `0` —
/// an unset glob is not a failure, it is "nothing configured to wire" (DESIGN.md above).
pub const NO_GLOB: &str = "SPIRA_ALERT_GLOB is unset — nothing to wire.\n  Set it to a find(1) name pattern matching the alert units whose failure you want\n  filed as incident beads, e.g. SPIRA_ALERT_GLOB='alert-prod@.service'.";

fn dropin_path(template: &Path) -> PathBuf {
    let mut dir = template.as_os_str().to_owned();
    dir.push(".d");
    PathBuf::from(dir).join(DROPIN)
}

fn unit_name(template: &Path) -> String {
    template.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

/// The drop-in's own content — a fixed `ExecStart=-<incident.sh> systemd %i` after the
/// first (the alert's own) `ExecStart=`, so the Pushover alert always goes out first and a
/// failure here cannot delay or fail it (the leading `-`).
pub fn conf_body(incident: &Path) -> String {
    format!(
        "# Installed by `release intake` — do not edit; it is overwritten.\n#\n# A second ExecStart on a Type=oneshot unit runs after the first, so the Pushover alert\n# still goes out first and this cannot delay it. The leading '-' makes the failure of\n# intake a non-failure of the alert.\n[Service]\nExecStart=-{} systemd %i\n",
        incident.display()
    )
}

/// The probe unit `install`'s verify step asks `systemctl --user cat` about: a template
/// `<x>@.service` becomes `<x>@probe.service` (an always-nameable instance of the same
/// template, so `cat` can show the merged unit — including the drop-in — without that
/// instance ever having run); a non-templated name is left as-is (the script's own literal
/// bash substitution, `${u%@.service}@probe.service`, carried over unchanged).
fn probe_unit(name: &str) -> String {
    format!("{}@probe.service", name.strip_suffix("@.service").unwrap_or(name))
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct InstallReport {
    pub wired: Vec<String>,
    pub total: usize,
    pub unverified: Vec<String>,
}

/// `install`: write (or refresh) the drop-in beside every template `pattern` matches.
/// Refuses (nothing written) when `unit_dir` is not a directory, `incident.sh` is not
/// executable, or no template matches. `Ok(None)` for the deliberate no-op ([`NO_GLOB`]).
pub fn install(t: &dyn Templates, sc: &dyn Systemctl, o: &Opts) -> Result<Option<InstallReport>, String> {
    let Some(pattern) = &o.pattern else { return Ok(None) };
    if !o.unit_dir.is_dir() {
        return Err(format!("no {}", o.unit_dir.display()));
    }
    if !crate::fsutil::is_executable(&o.incident) {
        return Err(format!(
            "refusing — {} is not executable.\n  A drop-in pointing at a script that cannot run is a wire that reports\n  installed and delivers nothing.",
            o.incident.display()
        ));
    }
    let templates = t.find(&o.unit_dir, pattern)?;
    if templates.is_empty() {
        return Err(format!("no templates match {pattern} in {}", o.unit_dir.display()));
    }
    let body = conf_body(&o.incident);
    let mut wired = Vec::new();
    for tpl in &templates {
        let dp = dropin_path(tpl);
        let unchanged = std::fs::read_to_string(&dp).map(|existing| existing == body).unwrap_or(false);
        if unchanged {
            continue;
        }
        if let Some(dir) = dp.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        crate::fsutil::write_atomic(&dp, &body)?;
        wired.push(unit_name(tpl));
    }
    if !wired.is_empty() && o.reload {
        sc.daemon_reload().map_err(|e| format!("daemon-reload: {e}"))?;
    }
    let mut unverified = Vec::new();
    if o.reload {
        for tpl in &templates {
            let name = unit_name(tpl);
            let ok = sc.cat(&probe_unit(&name)).map(|text| text.contains("incident.sh systemd")).unwrap_or(false);
            if !ok {
                unverified.push(name);
            }
        }
    }
    Ok(Some(InstallReport { wired, total: templates.len(), unverified }))
}

/// `status`: `Ok(None)` for the deliberate no-op ([`NO_GLOB`]); otherwise one line per
/// template (`wired`/`UNWIRED`) plus the wired/total counts.
pub fn status(t: &dyn Templates, o: &Opts) -> Result<Option<(Vec<String>, usize, usize)>, String> {
    let Some(pattern) = &o.pattern else { return Ok(None) };
    let templates = t.find(&o.unit_dir, pattern)?;
    let mut lines = Vec::new();
    let mut wired = 0;
    for tpl in &templates {
        let name = unit_name(tpl);
        if dropin_path(tpl).is_file() {
            wired += 1;
            lines.push(format!("wired    {name}"));
        } else {
            lines.push(format!("UNWIRED  {name}"));
        }
    }
    Ok(Some((lines, wired, templates.len())))
}

/// `uninstall`: remove every drop-in this wrote. `Ok(None)` for the deliberate no-op.
pub fn uninstall(t: &dyn Templates, sc: &dyn Systemctl, o: &Opts) -> Result<Option<Vec<String>>, String> {
    let Some(pattern) = &o.pattern else { return Ok(None) };
    if !o.unit_dir.is_dir() {
        return Ok(Some(Vec::new()));
    }
    let templates = t.find(&o.unit_dir, pattern)?;
    let mut removed = Vec::new();
    for tpl in &templates {
        let dp = dropin_path(tpl);
        if dp.is_file() {
            std::fs::remove_file(&dp).map_err(|e| format!("cannot remove {}: {e}", dp.display()))?;
            if let Some(dir) = dp.parent() {
                let _ = std::fs::remove_dir(dir); // best-effort, like the script's `rmdir 2>/dev/null`
            }
            removed.push(unit_name(tpl));
        }
    }
    if !removed.is_empty() && o.reload {
        sc.daemon_reload().map_err(|e| format!("daemon-reload: {e}"))?;
    }
    Ok(Some(removed))
}

#[cfg(test)]
mod tests {
    use super::*;



    #[test]
    fn probe_unit_handles_templated_and_plain_names() {
        assert_eq!(probe_unit("alert-prod@.service"), "alert-prod@probe.service");
        assert_eq!(probe_unit("plain.service"), "plain.service@probe.service");
    }
}
