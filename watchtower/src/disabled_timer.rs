//! `--disabled-timer-check` — escalates an essential timer (`world.sh`'s `TIMER_PRIORITY`)
//! that is disabled with no recorded `ctrl.sh` suspension, while the world is not stopped
//! (sp-1ar8t). `world.sh start` already refuses a bare "RUNNING" print when this is true,
//! but that only runs at the moment someone runs `start`; this is the periodic leg, on the
//! sentinel's own cadence.

use crate::incident::{self, Finding};
use crate::log::log;
use crate::seams;
use std::process::Command;

pub struct Cfg {
    pub systemctl: String,
    pub instance_suffix: String,
}

fn systemctl_ok(cfg: &Cfg, args: &[&str]) -> bool {
    let mut full = vec!["--user"];
    full.extend_from_slice(args);
    Command::new(&cfg.systemctl)
        .args(&full)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn systemctl_stdout(cfg: &Cfg, args: &[&str]) -> String {
    let mut full = vec!["--user"];
    full.extend_from_slice(args);
    Command::new(&cfg.systemctl)
        .args(&full)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// `${base}${sfx}.timer`, unless neither `is-enabled` nor `is-active` recognises that name —
/// then the un-suffixed `${base}.timer`. Matches the bash's fallback exactly (tries the
/// instance-suffixed name first, falls back only when systemd knows nothing about it at all).
pub fn resolve_timer_name(cfg: &Cfg, base: &str) -> String {
    let suffixed = format!("{base}{}.timer", cfg.instance_suffix);
    let enabled_ok = systemctl_ok(cfg, &["is-enabled", &suffixed]);
    let active_ok = systemctl_ok(cfg, &["is-active", &suffixed]);
    if !enabled_ok && !active_ok {
        format!("{base}.timer")
    } else {
        suffixed
    }
}

/// The pure decision: escalate iff systemd reports the timer's enablement state as exactly
/// `disabled` AND `ctrl.sh` has no recorded suspension for its base name.
pub fn should_escalate(enabled_state: &str, suspended: bool) -> bool {
    enabled_state == "disabled" && !suspended
}

pub fn run(spira_home: &str, db: &str, home_repo: &str, incident_sh: &str, cfg: &Cfg) {
    if !incident::is_usable(incident_sh) {
        log(&format!(
            "watchtower: disabled-timer-check skipped — {} not readable",
            incident_sh
        ));
        return;
    }
    let timers = match seams::timer_priority_and_suspended(spira_home) {
        Some(t) => t,
        None => {
            log("watchtower: disabled-timer-check skipped — could not read TIMER_PRIORITY/ctrl suspensions (world.sh/ctrl.sh)");
            return;
        }
    };
    for base in &timers.priority {
        let timer = resolve_timer_name(cfg, base);
        let state = systemctl_stdout(cfg, &["is-enabled", &timer]);
        let suspended = timers.suspended.contains(base);
        if !should_escalate(&state, suspended) {
            continue;
        }
        let body = format!(
            "{timer} is disabled with no recorded ctrl suspension.\n\nA disabled essential timer with no ctrl.sh suspension reason is an accident, not a decision. Re-enable it, or record why it is stopped:\n  ctrl.sh suspend {base} --reason \"...\" --owner <bead>\n"
        );
        let f = Finding::new(
            db,
            home_repo,
            &format!("DISABLED TIMER: {timer} disabled with no recorded suspension"),
            &body,
        )
        .priority(1)
        .reference(format!("incident:disabled-timer-{base}"))
        .cause("disabled-timer");
        incident::file(incident_sh, &f);
        log(&format!(
            "watchtower: disabled-timer-check filed escalation for {timer}"
        ));
    }
    log("watchtower: disabled-timer-check complete");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escalates_only_when_disabled_and_unsuspended() {
        assert!(should_escalate("disabled", false));
        assert!(!should_escalate("disabled", true));
        assert!(!should_escalate("enabled", false));
        assert!(!should_escalate("static", false));
        assert!(!should_escalate("", false));
    }
}
